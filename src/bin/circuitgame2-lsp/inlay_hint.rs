//! 展開した回路の NOR ゲート数と定常化上界を、宣言名の横に表示する。

use std::ops::Range;

use circuitgame2_cli::CLOCKED_LIMITS;
use circuitgame2_core::{
    editor::{LineIndex, PositionEncoding, TextPosition},
    project::{check_use_aliases, index_use_targets, normalize_path, validate_circuit_basis},
    syntax::circuit::{Head, named_build_entry},
};
use lsp_types::{InlayHint, InlayHintLabel, InlayHintTooltip, Uri};

use super::{
    diagnostic::wire_range,
    sources::{DiskSyntaxCache, OpenDocument, SourceSet},
};

/// 照会時に確認したヒントと、解析対象だった文書の識別子。
pub(super) struct HintSnapshot {
    /// 照会範囲内で得られたヒント。
    pub(super) hints: Vec<InlayHint>,
    /// 文書の変更によって再照会が必要になる依存先。
    pub(super) dependencies: Vec<Uri>,
}

/// 要求範囲内で展開できた関数とモジュールのヒントを返す。
pub(super) fn collect<'a>(
    current: OpenDocument<'a>,
    start: TextPosition,
    end: TextPosition,
    encoding: PositionEncoding,
    open: impl Iterator<Item = OpenDocument<'a>>,
    disk: &DiskSyntaxCache,
) -> Option<HintSnapshot> {
    let text = current.text?;
    let lines = LineIndex::new(text);
    let start = lines.position_to_byte(start, encoding)?;
    let end = lines.position_to_byte(end, encoding)?;
    if start >= end {
        return None;
    }
    let (id, sources) = SourceSet::load(current, open).ok()?;
    let hints = analyze(text, &lines, start..end, encoding, &id, &sources, disk);
    sources
        .still_current([(current.uri, id.as_str())])
        .then(|| HintSnapshot {
            hints,
            dependencies: sources.uris().cloned().collect(),
        })
}

fn analyze(
    text: &str,
    lines: &LineIndex,
    range: Range<usize>,
    encoding: PositionEncoding,
    id: &str,
    sources: &SourceSet<'_>,
    disk: &DiskSyntaxCache,
) -> Vec<InlayHint> {
    let Ok(path) = normalize_path(id) else {
        return Vec::new();
    };
    let Ok(project) = sources.parse_strict_with_disk_cache(Some(disk)) else {
        return Vec::new();
    };
    let Some(file) = project
        .sources()
        .iter()
        .position(|source| source.id.same_file(&path))
    else {
        return Vec::new();
    };
    if validate_circuit_basis(&project).is_err() {
        return Vec::new();
    }
    let Ok(targets) = index_use_targets(&project) else {
        return Vec::new();
    };
    let Ok(aliases) = check_use_aliases(&targets) else {
        return Vec::new();
    };
    let mut hints = Vec::new();
    let limits = circuitgame2_core::project::ClockedModuleVmLimits {
        state_bits: sources.state_bits(),
        ..CLOCKED_LIMITS
    };
    let circuit = &project.circuits()[file];
    for (name, form) in circuit.names.iter().enumerate() {
        let offset = form.name.span.end;
        if !range.contains(&offset) {
            continue;
        }
        let (nor_gates, settling_ticks) = match form.head {
            Head::Func => {
                let entry = named_build_entry(text, circuit, name);
                let Ok(Some(function)) = aliases.expand_reachable_function(
                    (file, entry),
                    CLOCKED_LIMITS.width,
                    CLOCKED_LIMITS.wiring_bits,
                    CLOCKED_LIMITS.gates,
                ) else {
                    continue;
                };
                (function.netlist().gates.len(), None)
            }
            Head::Module => {
                let Ok(Some(hint)) = aliases.module_expansion_hint(file, name, limits) else {
                    continue;
                };
                (hint.nor_gates, hint.settling_ticks)
            }
            _ => continue,
        };
        let Some(position) = wire_range(lines, text.as_bytes(), &(offset..offset), encoding)
            .map(|range| range.start)
        else {
            continue;
        };
        let count_help = "階層を展開した回路の NOR ゲート数。配線と状態素子は含まない。";
        let (label, tooltip) = match settling_ticks {
            Some(ticks) => (
                format!("NOR {nor_gates} · 定常化 ≤ {ticks} tick"),
                format!(
                    "{count_help}\n定常化上界は、固定入力の下で任意の有効な初期状態から内部ゲート値が揃うまでの tick 数。"
                ),
            ),
            None => (format!("NOR {nor_gates}"), count_help.into()),
        };
        hints.push(InlayHint {
            position,
            label: InlayHintLabel::String(label),
            kind: None,
            text_edits: None,
            tooltip: Some(InlayHintTooltip::String(tooltip)),
            padding_left: Some(true),
            padding_right: None,
            data: None,
        });
    }
    hints
}
