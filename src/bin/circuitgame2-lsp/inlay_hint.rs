//! 定常化の推論結果を、要求範囲にあるモジュール名の横へ表示する。

use std::ops::Range;

use circuitgame2_cli::CLOCKED_LIMITS;
use circuitgame2_core::{
    editor::{LineIndex, PositionEncoding, TextPosition},
    project::{
        check_use_aliases, index_use_targets, normalize_path, parse_circuit_project,
        validate_circuit_basis,
    },
};
use lsp_types::{InlayHint, InlayHintLabel, InlayHintTooltip, Uri};

use super::{
    diagnostic::wire_range,
    sources::{OpenDocument, SourceSet},
};

/// 照会時に確認したヒントと、解析対象だった文書の識別子。
pub(super) struct HintSnapshot {
    /// 照会範囲内で得られたヒント。
    pub(super) hints: Vec<InlayHint>,
    /// 文書の変更によって再照会が必要になる依存先。
    pub(super) dependencies: Vec<Uri>,
}

/// 解析対象の本文で上界を求められたモジュールにヒントを返す。
pub(super) fn collect<'a>(
    current: OpenDocument<'a>,
    start: TextPosition,
    end: TextPosition,
    encoding: PositionEncoding,
    open: impl Iterator<Item = OpenDocument<'a>>,
) -> Option<HintSnapshot> {
    let text = current.text?;
    let lines = LineIndex::new(text);
    let start = lines.position_to_byte(start, encoding)?;
    let end = lines.position_to_byte(end, encoding)?;
    if start >= end {
        return None;
    }
    let (id, sources) = SourceSet::load(current, open).ok()?;
    let hints = analyze(text, &lines, start..end, encoding, &id, &sources);
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
) -> Vec<InlayHint> {
    let Ok(path) = normalize_path(id) else {
        return Vec::new();
    };
    let snapshots = sources.snapshots();
    let Ok(project) = parse_circuit_project(&snapshots) else {
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
    for (name, form) in project.circuits()[file].names.iter().enumerate() {
        let offset = form.name.span.end;
        if !range.contains(&offset) {
            continue;
        }
        let Ok(Some(hint)) = aliases.module_settling_hint(file, name, CLOCKED_LIMITS) else {
            continue;
        };
        let offset = hint.location.span.end;
        let Some(position) = wire_range(lines, text.as_bytes(), &(offset..offset), encoding)
            .map(|range| range.start)
        else {
            continue;
        };
        hints.push(InlayHint {
            position,
            label: InlayHintLabel::String(format!("定常化 ≤ {} tick", hint.ticks)),
            kind: None,
            text_edits: None,
            tooltip: Some(InlayHintTooltip::String(
                "固定入力の下で、任意の有効な初期状態からの内部ゲート値を対象とする。上界は展開後の回路で最も深いゲートの深さ。"
                    .into(),
            )),
            padding_left: Some(true),
            padding_right: None,
            data: None,
        });
    }
    hints
}
