//! NCG2M を含むソース集合に対し、Core が扱う静的検査を実行する。

use std::{num::NonZeroUsize, ops::Range};

use circuitgame2_core::{
    ncg2m::file::MetaItemKind,
    project::{
        AliasConflict, AliasDisposition, CheckedBitFunc, DefinitionAnalysisError, FuncWiringResult,
        LocatedName, LoweredBitFunc, MixedAbsoluteTarget, MixedUseAliasConflict, MixedUseAliases,
        MixedUseTargets, ParsedProjectSources, ProjectDeclaration, ProjectDeclarationKind,
        ProjectSource, ProjectSourceIndex, ResolvedDefmacro, SourceSnapshot, UseSite,
        check_mixed_use_aliases, index_mixed_use_targets, parse_project_sources,
        validate_mixed_circuit_basis,
    },
    syntax::circuit::Head,
};

use super::{
    BIT_LIMIT, GATE_LIMIT, WIDTH_LIMIT, check_report::Report, declaration_key, diagnostic,
};

mod errors;
mod evaluate;
mod expansion;

// 一件の展開に割り当てる評価器の遷移数。使い切った場合は成功を報告しない。
const EXPANSION_FUEL: NonZeroUsize = NonZeroUsize::new(100_000).unwrap();

pub(super) use evaluate::{evaluate, parse_argument};

/// 共通の宣言と別名を解決し、すべての補助関数の型と呼び出し関係を検査する。
pub(super) fn check(
    snapshots: &[SourceSnapshot<'_>],
    report: &mut Option<&mut Report<'_>>,
    contextualize: impl Fn(String) -> String,
) -> Result<usize, String> {
    let project = parse_sources(snapshots, report, &contextualize)?;
    let targets = resolve_targets(&project, report, &contextualize)?;
    let aliases = resolve_aliases(&targets, report, &contextualize)?;
    check_bodies(&project, &aliases, None, report).map_err(&contextualize)?;
    Ok(project.sources().len())
}

/// 指定された関数を含む混在プロジェクトを検査し、入力と出力の配置を持つ NOR 回路に変換する。
///
/// 通常の検査で残った未対応の回路フォームやマクロ本体があれば診断を返す。
/// 名前はルートを表す `::` から始め、型幅、配線と NOR の個数には通常の `eval` と同じ上限を使う。
pub(super) fn lower_selected_func(
    snapshots: &[SourceSnapshot<'_>],
    name: &str,
    contextualize: &impl Fn(String) -> String,
) -> Result<LoweredBitFunc, String> {
    let mut report = None;
    let project = parse_sources(snapshots, &mut report, contextualize)?;
    let targets = resolve_targets(&project, &mut report, contextualize)?;
    let aliases = resolve_aliases(&targets, &mut report, contextualize)?;
    let key = declaration_key(name, "関数名")?;
    let declaration = targets
        .exports()
        .declarations()
        .declaration(key)
        .ok_or_else(|| contextualize(format!("{name} の宣言が見つからない。")))?;
    if declaration.kind != ProjectDeclarationKind::Circuit(Head::Func) {
        return Err(contextualize(format!(
            "{} {name} は func を指す必要がある。",
            diagnostic::located(&declaration.name),
        )));
    }
    let file = declaration.file;
    let entry = selected_function_entry(&project, &declaration)
        .ok_or_else(|| contextualize(format!("{name} の宣言位置が見つからない。")))?;
    let selected = check_bodies(&project, &aliases, Some((file, entry)), &mut report)
        .map_err(contextualize)?
        .ok_or_else(|| contextualize(format!("{name} の検査結果が見つからない。")))?;
    let declaration_location = diagnostic::located(&declaration.name);
    let lowered = selected.lower_bit_func(BIT_LIMIT).map_err(|error| {
        contextualize(expansion::layout_error(&selected, &declaration.name, error))
    })?;
    let gates = lowered.netlist().gates.len();
    if gates > GATE_LIMIT {
        return Err(contextualize(format!(
            "{declaration_location} NOR ゲートが {gates} 個あり、上限 {GATE_LIMIT} 個を超えている。",
        )));
    }
    Ok(lowered)
}

fn selected_function_entry(
    project: &ParsedProjectSources<'_>,
    declaration: &ProjectDeclaration<'_, '_>,
) -> Option<usize> {
    let ProjectSourceIndex::Circuit(circuit) = project.indexes().get(declaration.file)? else {
        return None;
    };
    let named = circuit.names.get(declaration.item)?;
    circuit
        .syntax
        .entries
        .iter()
        .position(|entry| entry.form == named.form)
}

fn check_bodies<'p, 'a>(
    project: &'p ParsedProjectSources<'a>,
    aliases: &MixedUseAliases<'_, 'p, 'a>,
    selection: Option<(usize, usize)>,
    report: &mut Option<&mut Report<'_>>,
) -> Result<Option<CheckedBitFunc<'p, 'a>>, String> {
    aliases
        .plan_defn_calls()
        .map_err(|error| errors::calls(project, error, report))?;

    let pending = check_circuit_functions(project, aliases, report)?;
    let mut selected = None;
    let uses_expansion = !pending.expansions.is_empty();
    if uses_expansion {
        for (file, entry, call) in pending.expansions {
            let function = check_expanded_function(aliases, file, entry, || call, report)?;
            if selection == Some((file, entry)) {
                selected = Some(function);
            }
        }
        if let Some(argument) = pending.argument {
            return Err(unsupported(
                report,
                argument,
                "この実引数の型は、混在プロジェクトの検査では確定できない。",
            ));
        }
        for (file, entry) in pending.plain {
            let function = check_expanded_function(
                aliases,
                file,
                entry,
                || function_location(project, file, entry),
                report,
            )?;
            if selection == Some((file, entry)) {
                selected = Some(function);
            }
        }
    }

    // 展開を検査できても、未検査のマクロ本体や回路フォームが残る場合は成功にしない。
    for (file, (source, index)) in project.sources().iter().zip(project.indexes()).enumerate() {
        match index {
            ProjectSourceIndex::Functional(items) => {
                for (item, indexed) in items.iter().enumerate() {
                    let MetaItemKind::Defmacro {
                        name,
                        parameters,
                        body,
                    } = &indexed.kind
                    else {
                        continue;
                    };
                    if uses_expansion {
                        let definition = ResolvedDefmacro {
                            declaration: ProjectDeclaration {
                                file,
                                item,
                                kind: ProjectDeclarationKind::Defmacro,
                                name: site(source, name.clone()),
                            },
                            parameters,
                            body,
                        };
                        match project.check_defmacro_body(&definition) {
                            Some(Ok(_)) => continue,
                            Some(Err(error)) => {
                                return Err(expansion::body(source, error, None, report));
                            }
                            None => {}
                        }
                    }
                    return Err(failure(
                        report,
                        "unsupported_macro_body",
                        site(source, body.clone()),
                        "マクロ本体の検査には対応していない。",
                        [],
                    ));
                }
            }
            ProjectSourceIndex::Circuit(circuit) => {
                if let Some(argument) = pending.argument {
                    return Err(unsupported(
                        report,
                        argument,
                        "この実引数の型は、混在プロジェクトの検査では確定できない。",
                    ));
                }
                if uses_expansion {
                    if let Some(other) = pending.other {
                        return Err(unsupported(
                            report,
                            other,
                            "この回路フォームの検査は混在プロジェクトでは未対応。",
                        ));
                    }
                    continue;
                }
                let span = circuit
                    .names
                    .iter()
                    .find(|form| matches!(form.head, Head::Func | Head::Module))
                    .map_or(0..0, |form| form.name.span.clone());
                return Err(unsupported(
                    report,
                    site(source, span),
                    "混在プロジェクトでは、回路本体の検査と展開に未対応の処理がある。",
                ));
            }
        }
    }
    Ok(selected)
}

fn parse_sources<'a>(
    snapshots: &[SourceSnapshot<'a>],
    report: &mut Option<&mut Report<'_>>,
    contextualize: &impl Fn(String) -> String,
) -> Result<ParsedProjectSources<'a>, String> {
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
    Ok(project)
}

fn resolve_targets<'p, 'a>(
    project: &'p ParsedProjectSources<'a>,
    report: &mut Option<&mut Report<'_>>,
    contextualize: &impl Fn(String) -> String,
) -> Result<MixedUseTargets<'p, 'a>, String> {
    index_mixed_use_targets(project).map_err(|error| {
        let error = DefinitionAnalysisError::UseTarget(error);
        if let Some(report) = report {
            report.analysis_error(&error);
        }
        contextualize(diagnostic::analysis_error(error))
    })
}

fn resolve_aliases<'t, 'p, 'a>(
    targets: &'t MixedUseTargets<'p, 'a>,
    report: &mut Option<&mut Report<'_>>,
    contextualize: &impl Fn(String) -> String,
) -> Result<MixedUseAliases<'t, 'p, 'a>, String> {
    let aliases = check_mixed_use_aliases(targets)
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
    Ok(aliases)
}

#[derive(Default)]
struct PendingCircuitChecks<'p, 'a> {
    expansions: Vec<(usize, usize, LocatedName<'p, 'a>)>,
    plain: Vec<(usize, usize)>,
    other: Option<LocatedName<'p, 'a>>,
    argument: Option<LocatedName<'p, 'a>>,
}

/// 関数をソース順に調べ、結線と入出力の個数、型に関する最初の違反を返す。
fn check_circuit_functions<'p, 'a>(
    project: &'p ParsedProjectSources<'a>,
    aliases: &MixedUseAliases<'_, 'p, 'a>,
    report: &mut Option<&mut Report<'_>>,
) -> Result<PendingCircuitChecks<'p, 'a>, String> {
    let mut pending = PendingCircuitChecks::default();
    for (file, index) in project.indexes().iter().enumerate() {
        let ProjectSourceIndex::Circuit(circuit) = index else {
            continue;
        };
        for (entry, item) in circuit.syntax.entries.iter().enumerate() {
            let source = &project.sources()[file];
            if item.head != Head::Func {
                if pending.other.is_none()
                    && !matches!(
                        item.head,
                        Head::Using | Head::Namespace | Head::Use | Head::Export
                    )
                {
                    pending.other = Some(site(
                        source,
                        match &circuit.syntax.forms[item.form] {
                            circuitgame2_core::syntax::Form::List { span, .. }
                            | circuitgame2_core::syntax::Form::Atom { span, .. } => span.clone(),
                        },
                    ));
                }
                continue;
            }
            let checked = aliases
                .check_mixed_func_input_types(file, entry, WIDTH_LIMIT)
                .map_err(|error| {
                    errors::mixed_function_types(
                        &function_location(project, file, entry),
                        error,
                        report,
                    )
                })?;
            if let FuncWiringResult::NeedsExpansion(expansion) = checked.outputs.literals.wiring {
                pending.expansions.push((file, entry, expansion));
                continue;
            }
            pending.plain.push((file, entry));
            if pending.argument.is_none() {
                pending.argument = checked.deferred_argument;
            }
        }
    }
    Ok(pending)
}

fn function_location<'p, 'a>(
    project: &'p ParsedProjectSources<'a>,
    file: usize,
    entry: usize,
) -> LocatedName<'p, 'a> {
    let ProjectSourceIndex::Circuit(circuit) = &project.indexes()[file] else {
        unreachable!("回路関数の位置には回路の索引が必要");
    };
    let item = &circuit.syntax.entries[entry];
    let span = circuit
        .names
        .iter()
        .find(|name| name.form == item.form)
        .map_or(0..0, |name| name.name.span.clone());
    site(&project.sources()[file], span)
}

// この経路は本体全体を検査する。従来の型検査では、左辺の型が未確定のまま残ることがある。
fn check_expanded_function<'p, 'a>(
    aliases: &MixedUseAliases<'_, 'p, 'a>,
    file: usize,
    entry: usize,
    location: impl FnOnce() -> LocatedName<'p, 'a>,
    report: &mut Option<&mut Report<'_>>,
) -> Result<CheckedBitFunc<'p, 'a>, String> {
    match aliases.check_expanded_bit_func(file, entry, WIDTH_LIMIT, EXPANSION_FUEL) {
        Ok(Some(checked)) => Ok(checked),
        Ok(None) => Err(unsupported(
            report,
            location(),
            "この関数の本体を混在プロジェクトで検査できない。",
        )),
        Err(error) => Err(expansion::wiring(error, location(), report)),
    }
}

fn unsupported(
    report: &mut Option<&mut Report<'_>>,
    location: LocatedName<'_, '_>,
    reason: &str,
) -> String {
    failure(
        report,
        "unsupported_mixed_circuit_check",
        location,
        reason,
        [],
    )
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
