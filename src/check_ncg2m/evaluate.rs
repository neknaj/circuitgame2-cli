//! 選んだ NCG2M の補助関数を評価し、スカラー値を標準出力へ書く。

use std::{
    io::Write,
    num::{NonZeroU64, NonZeroUsize},
    process::ExitCode,
};

use circuitgame2_core::{
    ncg2m::{
        call_evaluation::scalar::{
            ScalarBindingError, ScalarEntryError, ScalarExecutionError, ScalarExecutionSlice,
            ScalarValue, prepare_scalar_entry,
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
/// fuel を指定した場合は、その回数まで評価する。その範囲で一時停止した場合、
/// 値を出力せず終了コード 2 を返す。指定を省いた場合は、終端まで実行枠を与える。
/// Unix と Windows では Ctrl+C を実行枠の区切りで確認し、終了コード 130 を返す。
pub(in super::super) fn evaluate(
    snapshots: &[SourceSnapshot<'_>],
    name: &str,
    arguments: Vec<ScalarValue>,
    fuel: Option<NonZeroU64>,
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
    let entry = prepare_scalar_entry(&plan, position).map_err(|error| {
        let reason = match error {
            ScalarEntryError::UnknownDefinition => "補助関数が呼び出し計画にない。",
            ScalarEntryError::UnsupportedFragment => {
                "本体または呼び出し先にスカラー評価の対象外の式や型がある。"
            }
        };
        contextualize(format!(
            "{} {reason}",
            diagnostic::located(&declaration.name)
        ))
    })?;
    let mut state = entry
        .start(arguments)
        .map_err(|error| contextualize(binding_error(error)))?;
    let interrupted = interruption()?;
    let mut remaining = fuel;
    loop {
        // fuel の総回数を守りながら、枠の区切りで中断要求を確認する。
        let count = remaining.map_or(SLICE, |available| available.get().min(SLICE));
        let slice = NonZeroUsize::new(count as usize).expect("1..=4096 steps fit usize");
        let step = state.run_slice(slice);
        if interrupted() {
            eprintln!("補助関数の評価を中断した。");
            return Ok(ExitCode::from(130));
        }
        match step {
            Ok(ScalarExecutionSlice::Complete(value)) => {
                match value {
                    ScalarValue::Bool(value) => writeln!(stdout, "{value}"),
                    ScalarValue::Integer(IntegerValue::Nat(value)) => writeln!(stdout, "{value}"),
                    ScalarValue::Integer(IntegerValue::Int(value)) => writeln!(stdout, "{value}"),
                }
                .map_err(super::super::output_error)?;
                return Ok(ExitCode::SUCCESS);
            }
            Ok(ScalarExecutionSlice::Suspended(next)) => {
                if let Some(available) = remaining {
                    remaining = NonZeroU64::new(available.get() - count);
                    if remaining.is_none() {
                        eprintln!("指定した fuel の範囲で評価が完了しなかった。");
                        return Ok(ExitCode::from(2));
                    }
                }
                state = next;
            }
            Err(error) => return Err(contextualize(execution_error(error))),
        }
    }
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
        ScalarExecutionError::Deferred { source, span } => format!(
            "{} スカラー評価の対象外の式や型がある。",
            diagnostic::located(&site(source, span))
        ),
    }
}
