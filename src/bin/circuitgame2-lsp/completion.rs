//! 確認済みの断片名に対するモジュール候補を LSP に写す。

use std::collections::BTreeSet;

use circuitgame2_core::{
    editor::{LineIndex, PositionEncoding, TextPosition},
    project::{
        ObservedCircuitBindingKind, collect_sources, inspect_circuit_project, normalize_path,
    },
};
use lsp_types::{CompletionItem, CompletionItemKind, CompletionList, CompletionTextEdit, TextEdit};

use super::sources::{OpenDocument, SourceSet};

/// 回復で確認できた断片の対象名に、同じ名前空間のモジュール候補を返す。
///
/// 置換範囲は解析に使った文書版の元の名前を指す。候補は部分的な結果として返す。
pub(super) fn collect<'a>(
    current: OpenDocument<'a>,
    position: TextPosition,
    encoding: PositionEncoding,
    open: impl Iterator<Item = OpenDocument<'a>>,
) -> Option<CompletionList> {
    let text = current.text?;
    let offset = LineIndex::new(text).position_to_byte(position, encoding)?;
    let (id, sources) = SourceSet::load(current, open).ok()?;
    let path = normalize_path(&id).ok()?;
    let snapshots = sources.snapshots();
    let source_list = collect_sources(&snapshots).ok()?;
    let file = source_list
        .iter()
        .position(|source| source.id.same_file(&path))?;
    let project = inspect_circuit_project(&source_list);
    let result = project.module_part_candidates(file, offset)?;
    let target = sources.location(result.replace, encoding)?;
    if &target.uri != current.uri || target.range.start.line != target.range.end.line {
        return None;
    }
    let mut seen = BTreeSet::new();
    let items = result
        .declarations
        .files
        .into_iter()
        .flatten()
        .filter_map(|(_, binding)| {
            let ObservedCircuitBindingKind::Declaration { key, .. } = binding.kind else {
                return None;
            };
            if !seen.insert(key.name) {
                return None;
            }
            Some(CompletionItem {
                label: key.name.into(),
                kind: Some(CompletionItemKind::MODULE),
                detail: Some("暫定候補。回復で確認したモジュール宣言。".into()),
                sort_text: Some(key.name.into()),
                text_edit: Some(CompletionTextEdit::Edit(TextEdit::new(
                    target.range,
                    key.name.into(),
                ))),
                ..CompletionItem::default()
            })
        })
        .collect();
    sources
        .still_current([(current.uri, id.as_str())])
        .then_some(CompletionList {
            is_incomplete: true,
            items,
        })
}
