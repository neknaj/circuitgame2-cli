//! NCG2M を含むソース集合に対し、Core が扱う静的検査を実行する。

use std::ops::Range;

use circuitgame2_core::{
    ncg2m::file::MetaItemKind,
    project::{
        AliasConflict, AliasDisposition, DefinitionAnalysisError, FuncArityError, FuncCallTarget,
        LocatedName, MixedAbsoluteTarget, MixedUseAliasConflict, MixedUseAliases,
        ParsedProjectSources, ProjectSource, ProjectSourceIndex, SourceSnapshot, UseSite,
        check_mixed_use_aliases, index_mixed_use_targets, parse_project_sources,
        validate_mixed_circuit_basis,
    },
    syntax::circuit::Head,
};

use super::{check_body, check_report::Report, diagnostic};

mod errors;

/// 共通の宣言と別名を解決し、すべての補助関数の型と呼び出し関係を検査する。
pub(super) fn check(
    snapshots: &[SourceSnapshot<'_>],
    report: &mut Option<&mut Report<'_>>,
    contextualize: impl Fn(String) -> String,
) -> Result<usize, String> {
    let project = parse_project_sources(snapshots)
        .map_err(|error| contextualize(errors::parse(error, report)))?;
    if let Some(report) = report {
        report.files = Some(project.sources().len());
    }
    validate_mixed_circuit_basis(&project).map_err(|error| {
        let error = DefinitionAnalysisError::Basis(error);
        if let Some(report) = report {
            report.analysis_error(&error);
        }
        contextualize(diagnostic::analysis_error(error))
    })?;
    let targets = index_mixed_use_targets(&project).map_err(|error| {
        let error = DefinitionAnalysisError::UseTarget(error);
        if let Some(report) = report {
            report.analysis_error(&error);
        }
        contextualize(diagnostic::analysis_error(error))
    })?;
    let aliases = check_mixed_use_aliases(&targets)
        .map_err(|error| contextualize(alias_conflict(error, report)))?;
    let directives = targets.directives();
    for (directive, disposition) in directives.iter().zip(aliases.dispositions()) {
        if let AliasDisposition::Duplicate { first } = disposition {
            let first = &directives[*first].site;
            if let Some(report) = report {
                report.duplicate_use(&directive.site, first);
            } else {
                eprintln!(
                    "{}",
                    contextualize(format!(
                        "{} 同じ別名と参照先が重複している。\n  最初の指定 {}",
                        diagnostic::located(&alias_name(&directive.site)),
                        diagnostic::located(&alias_name(first)),
                    ))
                );
            }
        }
    }
    aliases
        .plan_defn_calls()
        .map_err(|error| contextualize(errors::calls(&project, error, report)))?;

    check_circuit_calls(&project, &aliases, report).map_err(&contextualize)?;

    // 結線、型、マクロ本体の検査が終わるまで、プロジェクト全体の成功は報告しない。
    for (source, index) in project.sources().iter().zip(project.indexes()) {
        match index {
            ProjectSourceIndex::Functional(items) => {
                if let Some(body) = items.iter().find_map(|item| match &item.kind {
                    MetaItemKind::Defmacro { body, .. } => Some(body.clone()),
                    _ => None,
                }) {
                    return Err(contextualize(failure(
                        report,
                        "unsupported_macro_body",
                        site(source, body),
                        "マクロ本体の検査には対応していない。",
                        [],
                    )));
                }
            }
            ProjectSourceIndex::Circuit(circuit) => {
                let span = circuit
                    .names
                    .iter()
                    .find(|form| matches!(form.head, Head::Func | Head::Module))
                    .map_or(0..0, |form| form.name.span.clone());
                return Err(contextualize(failure(
                    report,
                    "unsupported_mixed_circuit_check",
                    site(source, span),
                    "NCG2M を含むプロジェクトでは、回路本体の検査には対応していない。",
                    [],
                )));
            }
        }
    }
    Ok(project.sources().len())
}

/// 関数の定義をソース順に調べ、呼び出し名または入力個数の最初の違反を返す。
fn check_circuit_calls(
    project: &ParsedProjectSources<'_>,
    aliases: &MixedUseAliases<'_, '_, '_>,
    report: &mut Option<&mut Report<'_>>,
) -> Result<(), String> {
    for (file, index) in project.indexes().iter().enumerate() {
        let ProjectSourceIndex::Circuit(circuit) = index else {
            continue;
        };
        for (entry, (item, ranges)) in circuit
            .syntax
            .entries
            .iter()
            .zip(&circuit.entry_ranges)
            .enumerate()
        {
            if item.head != Head::Func {
                continue;
            }
            for (definition, row) in circuit.definitions[ranges.definitions.clone()]
                .iter()
                .enumerate()
            {
                for reference in 0..row.references.len() {
                    let Some(result) =
                        aliases.resolve_mixed_func_call_arity(file, entry, definition, reference)
                    else {
                        continue;
                    };
                    let call = result.map_err(|error| errors::circuit_call(error, report))?;
                    let location = site(call.source, call.call.span.clone());
                    match call.check_input_count() {
                        Ok(_) => {}
                        Err(FuncArityError::ArgumentCount {
                            call,
                            target,
                            expected,
                            actual,
                        }) => {
                            let declaration = match &target {
                                FuncCallTarget::Nor => None,
                                FuncCallTarget::Function { declaration, .. } => Some(declaration),
                            };
                            return Err(check_body::argument_count(
                                &call,
                                expected,
                                actual,
                                declaration,
                                report,
                            ));
                        }
                        Err(_) => {
                            return Err(failure(
                                report,
                                "circuit_call_check",
                                location,
                                "回路関数の呼び出しを検査できない。",
                                [],
                            ));
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn alias_name<'p, 'a>(directive: &UseSite<'p, 'a>) -> LocatedName<'p, 'a> {
    site(directive.source, directive.alias.span.clone())
}

fn alias_conflict(
    error: MixedUseAliasConflict<'_, '_, '_>,
    report: &mut Option<&mut Report<'_>>,
) -> String {
    match error {
        AliasConflict::BetweenUses { later, first } => failure(
            report,
            "alias_conflict",
            alias_name(&later.site),
            "同じ別名が異なる対象を指す。",
            [("最初の指定", alias_name(&first.site))],
        ),
        AliasConflict::Direct {
            directive,
            existing,
        } => {
            let original = match existing {
                MixedAbsoluteTarget::Declaration(declaration) => declaration.name,
                MixedAbsoluteTarget::Namespace { introduced_at, .. } => introduced_at,
            };
            failure(
                report,
                "alias_conflict",
                alias_name(&directive.site),
                "別名が既存の名前と衝突する。",
                [("既存の名前", original)],
            )
        }
    }
}

fn site<'p, 'a>(source: &'p ProjectSource<'a>, span: Range<usize>) -> LocatedName<'p, 'a> {
    LocatedName { source, span }
}

fn failure<'p, 'a>(
    report: &mut Option<&mut Report<'_>>,
    code: &'static str,
    location: LocatedName<'p, 'a>,
    reason: &str,
    related: impl IntoIterator<Item = (&'static str, LocatedName<'p, 'a>)>,
) -> String {
    use std::fmt::Write as _;

    let related: Vec<_> = related.into_iter().collect();
    if let Some(report) = report {
        report.body_error(code, &location, related.iter().map(|(_, site)| site));
    }
    let mut message = format!("{} {reason}", diagnostic::located(&location));
    for (label, other) in related {
        let _ = write!(message, "\n  {label} {}", diagnostic::located(&other));
    }
    message
}
