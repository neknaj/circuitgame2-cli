//! core の診断と関連箇所を端末向けに表示する。

pub(super) mod clocked;

use std::{
    io::{self, Write},
    ops::Range,
};

use circuitgame2_core::{
    project::{
        AbsoluteTarget, AliasConflict, AliasDisposition, BitFuncError, CircuitBasisError,
        CircuitProject, CircuitProjectError, DefinitionAnalysisError, DefinitionBatchError,
        DefinitionReferenceReason, ExpandFunctionsError, ExportError, FunctionBuildDiagnostic,
        FunctionExpansionError, LocatedName, ResolvedUse, UseAliasReport, UseTargetError,
    },
    syntax::circuit::CircuitFormError,
};

fn position(path: &str, span: &Range<usize>) -> String {
    format!("{path} [{}..{}]", span.start, span.end)
}

pub(super) fn located(site: &LocatedName<'_, '_>) -> String {
    position(&site.source.id.components().join("/"), &site.span)
}

fn directive(site: &ResolvedUse<'_, '_>) -> String {
    position(
        &site.site.source.id.components().join("/"),
        &site.site.alias.span,
    )
}

pub(super) fn project_error(error: CircuitProjectError<'_>) -> String {
    match error {
        CircuitProjectError::Syntax { id, error, .. } => {
            let path = id.components().join("/");
            match error {
                CircuitFormError::Syntax(error) => {
                    format!(
                        "{} 構文エラー ({:?})。",
                        position(&path, &error.span),
                        error.kind
                    )
                }
                CircuitFormError::Header { kind, span } => {
                    format!(
                        "{} 回路フォームの構文エラー ({kind:?})。",
                        position(&path, &span)
                    )
                }
            }
        }
        CircuitProjectError::Sources(error) => {
            format!("ソースを収集できない ({error:?})。")
        }
        CircuitProjectError::UnsupportedLanguage { id, .. } => {
            format!(
                "{} は NCG2 の回路ソースを指定する。",
                id.components().join("/")
            )
        }
    }
}

pub(super) fn alias_error<'p, 'a, D: std::borrow::Borrow<ResolvedUse<'p, 'a>>>(
    error: AliasConflict<D, AbsoluteTarget<'p, 'a, 'a>>,
) -> String
where
    'a: 'p,
{
    match error {
        AliasConflict::BetweenUses { later, first } => format!(
            "{} 同じ別名が異なる対象を指す。\n  最初の指定 {}",
            directive(later.borrow()),
            directive(first.borrow()),
        ),
        AliasConflict::Direct {
            directive: alias,
            existing,
        } => {
            let prior = match existing {
                AbsoluteTarget::Declaration { source, form } => {
                    position(&source.id.components().join("/"), &form.name.span)
                }
                AbsoluteTarget::Namespace { introduced_at, .. } => located(&introduced_at),
            };
            format!(
                "{} 別名が既存の名前と衝突する。\n  既存の名前 {prior}",
                directive(alias.borrow())
            )
        }
    }
}

pub(super) fn alias_warnings(
    report: &UseAliasReport<'_, '_>,
    contextualize: impl Fn(String) -> String,
) {
    let directives = report.targets.directives();
    let mut stderr = io::stderr().lock();
    for (alias, disposition) in directives.iter().zip(&report.dispositions) {
        if let AliasDisposition::Duplicate { first } = disposition
            && let Some(first) = directives.get(*first)
        {
            let message = format!(
                "警告 {} 同じ別名と参照先が重複している。\n  最初の指定 {}",
                directive(alias),
                directive(first),
            );
            let _ = writeln!(stderr, "{}", contextualize(message));
        }
    }
}

fn reference_error(error: DefinitionBatchError<'_, '_>) -> String {
    let at = position(
        &error.site.source.id.components().join("/"),
        &error.site.name.span,
    );
    reference_reason(&at, error.site.name.word, error.reason)
}

pub(super) fn reference_reason(
    at: &str,
    name: &str,
    reason: DefinitionReferenceReason<'_, '_>,
) -> String {
    match reason {
        DefinitionReferenceReason::Missing => format!("{at} {name} が見つからない。"),
        DefinitionReferenceReason::MissingPrefix => {
            format!("{at} {name} の先頭の名前を解決できない。")
        }
        DefinitionReferenceReason::DeclarationPrefix { source, form } => format!(
            "{at} {name} の先頭は名前空間を指す必要がある。\n  参照先 {}",
            position(&source.id.components().join("/"), &form.name.span),
        ),
        DefinitionReferenceReason::Private(declaration) => {
            format!(
                "{at} {name} は非公開である。\n  参照先 {}",
                located(&declaration)
            )
        }
        DefinitionReferenceReason::Namespace(namespace) => format!(
            "{at} {name} は名前空間を指している。\n  参照先 {}",
            located(&namespace),
        ),
        DefinitionReferenceReason::WrongDeclaration { source, form } => format!(
            "{at} {name} が指す {:?} 宣言はここでは使えない。\n  参照先 {}",
            form.head,
            position(&source.id.components().join("/"), &form.name.span),
        ),
    }
}

pub(super) fn analysis_error(
    error: DefinitionAnalysisError<'_, '_>,
    contextualize: impl Fn(String) -> String,
    print_warnings: bool,
) -> String {
    match error {
        DefinitionAnalysisError::Basis(CircuitBasisError::Missing) => {
            "(using nor) を一つ指定する。".into()
        }
        DefinitionAnalysisError::Basis(CircuitBasisError::Duplicate { later, first }) => format!(
            "{} (using nor) が重複している。\n  最初の指定 {}",
            located(&later),
            located(&first),
        ),
        DefinitionAnalysisError::Alias(conflict) => alias_error(conflict),
        DefinitionAnalysisError::Definition { error, aliases } => {
            if print_warnings {
                alias_warnings(&aliases, contextualize);
            }
            reference_error(error)
        }
        DefinitionAnalysisError::UseTarget(error) => match error {
            UseTargetError::Export(ExportError::Conflict(conflict)) => format!(
                "{} 宣言名が衝突する ({:?})。\n  先の宣言 {}",
                located(&conflict.second),
                conflict.kind,
                located(&conflict.first),
            ),
            UseTargetError::Export(ExportError::MissingDeclaration(name)) => {
                format!("{} 公開する宣言が見つからない。", located(&name))
            }
            UseTargetError::Missing(site) => format!(
                "{} 別名の参照先 {} が見つからない。",
                position(&site.source.id.components().join("/"), &site.reference.span),
                site.reference.word,
            ),
            UseTargetError::Private { site, declaration } => format!(
                "{} 別名の参照先 {} は非公開である。\n  参照先 {}",
                position(&site.source.id.components().join("/"), &site.reference.span),
                site.reference.word,
                located(&declaration),
            ),
        },
    }
}

pub(super) fn expansion_error(
    project: &CircuitProject<'_>,
    error: ExpandFunctionsError<'_, '_>,
) -> String {
    match error {
        ExpandFunctionsError::Plan(FunctionExpansionError::Reference(error)) => {
            reference_error(error)
        }
        ExpandFunctionsError::Plan(FunctionExpansionError::Cycle(sites)) => {
            let mut message = "func の呼び出しが循環している。".to_owned();
            for site in sites {
                message.push_str(&format!("\n  {}", located(&site.location)));
            }
            message
        }
        ExpandFunctionsError::Function {
            position: (file, entry),
            diagnostic,
        } => {
            let site = project
                .circuits()
                .get(file)
                .and_then(|circuit| {
                    let entry = circuit.syntax.entries.get(entry)?;
                    circuit.names.iter().find(|form| form.form == entry.form)
                })
                .and_then(|name| {
                    let source = project.sources().get(file)?;
                    Some(position(&source.id.components().join("/"), &name.name.span))
                })
                .unwrap_or_else(|| format!("ファイル {file} の宣言 {entry}"));
            match diagnostic {
                FunctionBuildDiagnostic::Prepare(BitFuncError::NeedsExpansion(at)) => {
                    format!("{} マクロ展開が必要なため評価できない。", located(&at))
                }
                FunctionBuildDiagnostic::Prepare(BitFuncError::NorInputWidth {
                    argument,
                    actual,
                })
                | FunctionBuildDiagnostic::Definition(BitFuncError::NorInputWidth {
                    argument,
                    actual,
                }) => {
                    format!(
                        "{} NOR の入力には 1 ビット必要だが、幅は {actual} ビットである。",
                        located(&argument)
                    )
                }
                FunctionBuildDiagnostic::Prepare(BitFuncError::OutputWidth {
                    annotation,
                    expected,
                    actual,
                })
                | FunctionBuildDiagnostic::Output(BitFuncError::OutputWidth {
                    annotation,
                    expected,
                    actual,
                }) => {
                    format!(
                        "{} 出力の宣言幅は {expected} ビットだが、駆動元は {actual} ビットである。",
                        located(&annotation)
                    )
                }
                FunctionBuildDiagnostic::Ports(BitFuncError::Unsupported(kind, at))
                | FunctionBuildDiagnostic::Definition(BitFuncError::Unsupported(kind, at))
                | FunctionBuildDiagnostic::Prepare(BitFuncError::Unsupported(kind, at)) => {
                    format!(
                        "{} 現在の評価器ではこの構文を扱えない ({kind:?})。",
                        located(&at)
                    )
                }
                FunctionBuildDiagnostic::Prepare(_) => {
                    format!("{site} 関数の型または結線に違反がある。")
                }
                FunctionBuildDiagnostic::Ports(_) => {
                    format!("{site} 関数の入出力ポートを配置できない。")
                }
                FunctionBuildDiagnostic::Definition(_) => {
                    format!("{site} 関数の定義を配線に変換できない。")
                }
                FunctionBuildDiagnostic::Output(_) => {
                    format!("{site} 関数の宣言出力と実際の出力の幅が異なる。")
                }
                FunctionBuildDiagnostic::Layout(error) => {
                    format!("{site} ビット範囲を割り当てられない ({error:?})。")
                }
                FunctionBuildDiagnostic::Argument(at) => {
                    format!("{} 実引数の配線を確定できない。", located(&at))
                }
                FunctionBuildDiagnostic::LocalGateCapacity(at)
                | FunctionBuildDiagnostic::CallGateCapacity(at) => {
                    format!("{} 展開後のゲート数が上限を超える。", located(&at))
                }
                FunctionBuildDiagnostic::CallOutputCapacity {
                    site,
                    used,
                    width,
                    capacity,
                } => format!(
                    "{} 子の出力を保持する配線が容量を超える。現在 {used} ビット、追加 {width} ビット、上限 {capacity} ビット。",
                    located(&site),
                ),
            }
        }
    }
}
