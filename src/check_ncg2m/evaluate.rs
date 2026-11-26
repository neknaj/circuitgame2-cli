//! 選んだ NCG2M の補助関数を評価し、スカラー値を標準出力へ書く。

use std::{
    io::Write,
    num::{NonZeroU64, NonZeroUsize},
    process::ExitCode,
};

use circuitgame2_core::{
    ncg2m::{
        call_evaluation::scalar::{
            ScalarBindingError, ScalarEntryError, ScalarExecution, ScalarExecutionError,
            ScalarExecutionSlice, ScalarFoldExecution, ScalarFoldSlice, ScalarValue,
            prepare_scalar_entry, prepare_scalar_fold,
        },
        data_types::{DataType, DataTypeAtom},
        integers::{IntegerType, IntegerValue, read_integer_literal},
    },
    project::{
        AbsoluteResolutionError, DefnPosition, MixedAbsoluteTarget, ProjectDeclarationKind,
        SourceSnapshot,
    },
};

use super::{diagnostic, errors, parse_sources, resolve_aliases, resolve_targets, site};

const SLICE: u64 = 4096;

#[cfg(any(unix, windows))]
fn interruption() -> Result<impl Fn() -> bool, String> {
    use std::sync::atomic::{AtomicBool, Ordering};

    static REQUESTED: AtomicBool = AtomicBool::new(false);
    ctrlc::set_handler(|| REQUESTED.store(true, Ordering::Relaxed))
        .map_err(|error| format!("中断ハンドラーを設定できない ({error})。"))?;
    Ok(|| REQUESTED.load(Ordering::Relaxed))
}

#[cfg(not(any(unix, windows)))]
fn interruption() -> Result<impl Fn() -> bool, String> {
    Ok(|| false)
}

enum Evaluation<'plan, 'p, 'a> {
    Scalar(ScalarExecution<'plan, 'p, 'a>),
    Fold(ScalarFoldExecution<'p, 'a>),
}

enum EvaluationStep<'plan, 'p, 'a> {
    Suspended(Evaluation<'plan, 'p, 'a>),
    Complete(ScalarValue),
}

impl<'plan, 'p, 'a> Evaluation<'plan, 'p, 'a> {
    fn run_slice(self, fuel: NonZeroUsize)
        -> (Result<EvaluationStep<'plan, 'p, 'a>, ScalarExecutionError<'p, 'a>>, usize)
    {
        match self {
            Self::Scalar(state) => {
                let (result, used) = state.run_slice_with_steps(fuel);
                (result.map(|step| match step {
                    ScalarExecutionSlice::Suspended(next) => EvaluationStep::Suspended(Self::Scalar(next)),
                    ScalarExecutionSlice::Complete(value) => EvaluationStep::Complete(value),
                }), used)
            }
            Self::Fold(state) => {
                let (result, used) = state.run_slice_with_steps(fuel);
                (result.map(|step| match step {
                    ScalarFoldSlice::Suspended(next) => EvaluationStep::Suspended(Self::Fold(next)),
                    ScalarFoldSlice::Complete(value) => EvaluationStep::Complete(value),
                }), used)
            }
        }
    }
}

/// `--arg` 一つを評価器の値に変換する。整数の範囲検査は Core に委ねる。
pub(in super::super) fn parse_argument(value: &str, number: usize) -> Result<ScalarValue, String> {
    match value {
        "true" => return Ok(ScalarValue::Bool(true)),
        "false" => return Ok(ScalarValue::Bool(false)),
        _ => {}
    }
    let typed = value
        .strip_prefix("Nat:")
        .map(|word| (IntegerType::Nat, word))
        .or_else(|| {
            value
                .strip_prefix("Int:")
                .map(|word| (IntegerType::Int, word))
        });
    let Some((ty, word)) = typed else {
        return Err(format!(
            "{number} 番目の --arg に true、false、Nat:<整数>、Int:<整数> のいずれかを指定する。"
        ));
    };
    read_integer_literal(word, Some(ty))
        .map(ScalarValue::Integer)
        .map_err(|error| {
            format!(
                "{number} 番目の --arg は不正。{}",
                errors::integer_literal_message(error)
            )
        })
}

/// ルート名前空間から宣言を解決し、型と引数を検査して値を一行で返す。
///
/// fuel を指定した場合は、その回数まで評価する。その範囲で一時停止した場合は
/// 標準出力に値を書かず、呼び出し元にコード 2 の `ExitCode` を返す。
/// WASI Preview 2 のホストには失敗として伝わる。fuel を省いた場合は、終端まで実行枠を与える。
/// Unix と Windows では Ctrl+C を実行枠の区切りで確認し、終了コード 130 を返す。
/// `--steps` を指定すると、成功、失敗、中断までの実行済み遷移数を標準エラー出力に書く。
/// ソースや引数の検査に失敗し、評価を始めなかった場合は回数を表示しない。
pub(in super::super) fn evaluate(
    snapshots: &[SourceSnapshot<'_>],
    name: &str,
    arguments: Vec<ScalarValue>,
    fuel: Option<NonZeroU64>,
    show_steps: bool,
    stdout: &mut impl Write,
    contextualize: impl Fn(String) -> String,
) -> Result<ExitCode, String> {
    let mut report = None;
    let project = parse_sources(snapshots, &mut report, &contextualize)?;
    let targets = resolve_targets(&project, &mut report, &contextualize)?;
    let aliases = resolve_aliases(&targets, &mut report, &contextualize)?;
    let target = targets
        .exports()
        .resolve_absolute(name, None)
        .map_err(|error| {
            contextualize(match error {
                AbsoluteResolutionError::InvalidPath => {
                    format!("`{name}` は完全修飾名になっていない。")
                }
                AbsoluteResolutionError::Missing => format!("`{name}` の宣言が見つからない。"),
                AbsoluteResolutionError::Private(declaration) => {
                    format!("{} `{name}` は非公開。", diagnostic::located(&declaration))
                }
            })
        })?;
    let declaration = match target {
        MixedAbsoluteTarget::Declaration(declaration)
            if declaration.kind == ProjectDeclarationKind::Defn =>
        {
            declaration
        }
        other => {
            let location = match other {
                MixedAbsoluteTarget::Declaration(declaration) => declaration.name,
                MixedAbsoluteTarget::Namespace { introduced_at, .. } => introduced_at,
            };
            return Err(contextualize(format!(
                "{} `{name}` は NCG2M の補助関数を指していない。",
                diagnostic::located(&location)
            )));
        }
    };
    let plan = aliases
        .plan_defn_calls()
        .map_err(|error| contextualize(errors::calls(&project, error, &mut report)))?;
    let position = DefnPosition {
        file: declaration.file,
        item: declaration.item,
    };
    let state = match prepare_scalar_entry(&plan, position) {
        Ok(entry) => entry.start(arguments).map(Evaluation::Scalar),
        Err(error) => match error {
            ScalarEntryError::UnsupportedFragment => {
                let Some(entry) = prepare_scalar_fold(&plan, position) else {
                    return Err(contextualize(format!(
                        "{} 本体または呼び出し先にスカラー評価の対象外の式や型がある。",
                        diagnostic::located(&declaration.name)
                    )));
                };
                entry.start(arguments).map(Evaluation::Fold)
            }
            ScalarEntryError::UnknownDefinition => {
                return Err(contextualize(format!(
                    "{} 補助関数が呼び出し計画にない。",
                    diagnostic::located(&declaration.name)
                )));
            }
        },
    };
    let mut state = state.map_err(|error| contextualize(binding_error(error)))?;
    let interrupted = interruption()?;
    let mut remaining = fuel;
    let mut steps = show_steps.then_some(0_u128);
    let result = loop {
        // fuel の総回数を守りながら、枠の区切りで中断要求を確認する。
        let count = remaining.map_or(SLICE, |available| available.get().min(SLICE));
        let slice = NonZeroUsize::new(count as usize).expect("1..=4096 steps fit usize");
        let (step, used) = state.run_slice(slice);
        if let Some(total) = &mut steps {
            *total = total
                .checked_add(used as u128)
                .ok_or("評価ステップ数が表示できる範囲を超えた。")?;
        }
        if interrupted() {
            eprintln!("補助関数の評価を中断した。");
            break Ok(ExitCode::from(130));
        }
        match step {
            Ok(EvaluationStep::Complete(value)) => {
                let written = match value {
                    ScalarValue::Bool(value) => writeln!(stdout, "{value}"),
                    ScalarValue::Integer(IntegerValue::Nat(value)) => writeln!(stdout, "{value}"),
                    ScalarValue::Integer(IntegerValue::Int(value)) => writeln!(stdout, "{value}"),
                };
                break written
                    .map(|()| ExitCode::SUCCESS)
                    .map_err(super::super::output_error);
            }
            Ok(EvaluationStep::Suspended(next)) => {
                if let Some(available) = remaining {
                    remaining = NonZeroU64::new(available.get() - count);
                    if remaining.is_none() {
                        eprintln!("指定した fuel の範囲で評価が完了しなかった。");
                        break Ok(ExitCode::from(2));
                    }
                }
                state = next;
            }
            Err(error) => break Err(contextualize(execution_error(error))),
        }
    };
    if let Some(steps) = steps {
        eprintln!("steps={steps}");
    }
    result
}

fn binding_error(error: ScalarBindingError<'_, '_>) -> String {
    match error {
        ScalarBindingError::Rank => "補助関数が呼び出し計画にない。".into(),
        ScalarBindingError::Arity {
            source,
            span,
            expected_count,
            found_count,
        } => format!(
            "{} 引数は {expected_count} 個必要だが、{found_count} 個指定された。",
            diagnostic::located(&site(source, span))
        ),
        ScalarBindingError::Type {
            source,
            span,
            parameter,
            expected_type,
            found_type,
        } => format!(
            "{} {} 番目の引数は {} が必要だが、{} が指定された。",
            diagnostic::located(&site(source, span)),
            parameter + 1,
            errors::data_type(expected_type),
            errors::data_type(DataType {
                atom: DataTypeAtom::Builtin(found_type),
                list_depth: 0
            }),
        ),
    }
}

fn execution_error(error: ScalarExecutionError<'_, '_>) -> String {
    match error {
        ScalarExecutionError::Literal(error) => format!(
            "{} {}",
            diagnostic::located(&site(error.source, error.span)),
            errors::integer_literal_message(error.error)
        ),
        ScalarExecutionError::Arithmetic {
            source,
            operation,
            error,
        } => format!(
            "{} `{}` の計算結果は {} の範囲外。",
            diagnostic::located(&site(source, error.span)),
            errors::integer_operation_name(operation),
            errors::integer_type(error.ty)
        ),
        ScalarExecutionError::Deferred { source, span } => format!(
            "{} スカラー評価の対象外の式や型がある。",
            diagnostic::located(&site(source, span))
        ),
    }
}
