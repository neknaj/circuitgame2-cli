//! 確認済みの断片名に対するモジュール候補を LSP に写す。

use std::collections::BTreeSet;

use circuitgame2_core::{
    editor::{LineIndex, PositionEncoding, TextPosition},
    project::{
        ObservedCircuitBindingKind, collect_sources, inspect_circuit_project_with_cache,
        normalize_path,
    },
};
use lsp_types::{CompletionItem, CompletionItemKind, CompletionList, CompletionTextEdit, TextEdit};

use super::sources::{DiskSyntaxCache, OpenDocument, SourceSet};

/// 回復で確認できた断片の対象名に、同じ名前空間のモジュール候補を返す。
///
/// 置換範囲は解析に使った文書版の元の名前を指す。候補は部分的な結果として返す。
pub(super) fn collect<'a>(
    current: OpenDocument<'a>,
    position: TextPosition,
    encoding: PositionEncoding,
    open: impl Iterator<Item = OpenDocument<'a>>,
    disk: &DiskSyntaxCache,
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
    let project =
        inspect_circuit_project_with_cache(&source_list, &sources.syntax_candidates(Some(disk)));
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

#[cfg(test)]
mod tests {
    use std::fs;

    use url::Url;

    use super::*;

    #[test]
    fn completion_uses_the_current_closed_source_after_a_same_size_edit() {
        let root = tempfile::tempdir().unwrap();
        let main = root.path().join("main.ncg2");
        let library = root.path().join("library.ncg2");
        fs::write(
            root.path().join("circuitgame2.toml"),
            "sources = [\"library.ncg2\", \"main.ncg2\"]\n",
        )
        .unwrap();
        let original = "(using nor) (module gate (in x) (out y))";
        fs::write(&library, original).unwrap();
        fs::write(&main, "").unwrap();
        let uri = Url::from_file_path(&main)
            .unwrap()
            .as_str()
            .parse()
            .unwrap();
        let current = OpenDocument {
            uri: &uri,
            text: Some("(module-part ga (def y x))"),
            version: 1,
            analysis: None,
        };
        let (_, source_set) = SourceSet::load(current, [current].into_iter()).unwrap();
        let mut disk = DiskSyntaxCache::default();
        let (_, staged) = source_set
            .parse_strict_recording_disk_indexes(&disk)
            .unwrap();
        disk.extend(staged);
        assert!(source_set.syntax_candidates(Some(&disk))[0].is_some());

        let complete = || {
            collect(
                current,
                TextPosition {
                    line: 0,
                    column: 15,
                },
                PositionEncoding::Utf16,
                [current].into_iter(),
                &disk,
            )
            .unwrap()
            .items
        };
        assert_eq!(complete()[0].label, "gate");
        fs::write(&library, original.replace("gate", "gait")).unwrap();
        let items = complete();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "gait");
    }
}
