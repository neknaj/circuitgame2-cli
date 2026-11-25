//! 展開した回路が持つ NOR ゲート数を、宣言ごとに報告する。

use std::io::Write;

use circuitgame2_core::{
    project::{CircuitProject, ModuleBodyPosition, UseAliases},
    syntax::circuit::{Head, named_build_entry},
};

use super::{BIT_LIMIT, GATE_LIMIT, WIDTH_LIMIT, clocked::LIMITS, diagnostic, output_error};

/// 指定した宣言、または全関数と全モジュールを展開し、完全修飾名と個数を書く。
///
/// 全件の場合は正規化したソース順と宣言順に並べる。配線と状態素子は数えない。
///
/// # Errors
///
/// 指定した宣言が見つからない場合、展開できない場合、または出力に失敗した場合にエラーを返す。
pub(super) fn write(
    project: &CircuitProject<'_>,
    aliases: &UseAliases<'_, '_, '_>,
    requested: Option<&str>,
    source_diagnostic: impl Fn(String) -> String,
    out: &mut impl Write,
) -> Result<(), String> {
    let functions = if requested.is_none() {
        Some(
            aliases
                .expand_functions(WIDTH_LIMIT, BIT_LIMIT, GATE_LIMIT)
                .map_err(|error| source_diagnostic(diagnostic::expansion_error(project, error)))?,
        )
    } else {
        None
    };
    let mut rows = Vec::new();
    for (file, (source, circuit)) in project.sources().iter().zip(project.circuits()).enumerate() {
        for (name_index, form) in circuit.names.iter().enumerate() {
            if !matches!(form.head, Head::Func | Head::Module) {
                continue;
            }
            let name = match &form.namespace {
                Some(parent) => format!("::{}::{}", parent.word, form.name.word),
                None => format!("::{}", form.name.word),
            };
            if requested.is_some_and(|requested| requested != name) {
                continue;
            }
            let unavailable = || format!("{name} の展開結果が見つからない。");
            let count = if form.head == Head::Func {
                let entry = named_build_entry(source.text(), circuit, name_index);
                if let Some(functions) = &functions {
                    functions
                        .get(file, entry)
                        .ok_or_else(unavailable)?
                        .netlist()
                        .gates
                        .len()
                } else {
                    aliases
                        .expand_reachable_function(
                            (file, entry),
                            WIDTH_LIMIT,
                            BIT_LIMIT,
                            GATE_LIMIT,
                        )
                        .map_err(|error| {
                            source_diagnostic(diagnostic::expansion_error(project, error))
                        })?
                        .ok_or_else(unavailable)?
                        .netlist()
                        .gates
                        .len()
                }
            } else {
                let entry = named_build_entry(source.text(), circuit, name_index);
                let owner = ModuleBodyPosition { file, entry };
                aliases
                    .lower_clocked_vm(owner, LIMITS)
                    .map_err(|error| {
                        source_diagnostic(diagnostic::clocked::error(project, owner, &name, error))
                    })?
                    .ok_or_else(unavailable)?
                    .vm
                    .nor_gate_count()
            };
            rows.push((name, count));
        }
    }
    if let Some(name) = requested
        && rows.is_empty()
    {
        return Err(format!(
            "{name} の関数またはモジュールが見つからない。:: から始まる完全修飾名で指定する。"
        ));
    }
    for (name, count) in rows {
        writeln!(out, "{name}\t{count}").map_err(output_error)?;
    }
    Ok(())
}
