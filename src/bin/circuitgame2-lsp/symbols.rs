//! Core の回路フォーム索引を、エディタのアウトラインに写す。

use std::{collections::HashMap, ops::Range};

use circuitgame2_core::{
    editor::{LineIndex, PositionEncoding},
    syntax::{
        Form,
        circuit::{
            Head, RecoveredName, index_named_build_forms, recover_circuit_names,
            recover_circuit_names_with_cache,
        },
    },
};
use lsp_types::{DocumentSymbol, DocumentSymbolResponse, Location, SymbolInformation, SymbolKind};

use super::{diagnostic::wire_range, sources::OpenDocument};

fn symbol_kind(head: Head) -> Option<SymbolKind> {
    Some(match head {
        Head::Namespace => SymbolKind::NAMESPACE,
        Head::Func => SymbolKind::FUNCTION,
        Head::Module | Head::ModulePart => SymbolKind::MODULE,
        Head::Struct => SymbolKind::STRUCT,
        Head::Latch | Head::Memory => SymbolKind::OBJECT,
        Head::Test => SymbolKind::METHOD,
        Head::Using | Head::Use | Head::Export => return None,
    })
}

/// 宣言とモジュール断片を元ソース順に返す。
///
/// 文法を満たす文書ではフォーム全体を範囲に使う。回復中は元の入力で確認できた名前を返す。
#[allow(deprecated)]
pub(super) fn collect(
    document: OpenDocument<'_>,
    encoding: PositionEncoding,
    hierarchical: bool,
) -> Option<DocumentSymbolResponse> {
    let text = document.text?;
    let lines = LineIndex::new(text);
    let range = |span: &Range<usize>| wire_range(&lines, text.as_bytes(), span, encoding);
    if hierarchical {
        if let Ok(index) = index_named_build_forms(text) {
            let mut root = Vec::<DocumentSymbol>::new();
            let mut namespaces = HashMap::new();
            for item in &index.names {
                let Some(kind) = symbol_kind(item.head) else {
                    continue;
                };
                let Some(Form::List { span, .. }) = index.syntax.forms.get(item.form) else {
                    continue;
                };
                let (Some(full), Some(selection)) = (range(span), range(&item.name.span)) else {
                    continue;
                };
                let symbol = DocumentSymbol {
                    name: item.name.word.into(),
                    detail: None,
                    kind,
                    tags: None,
                    deprecated: None,
                    range: full,
                    selection_range: selection,
                    children: None,
                };
                if let Some(parent) = item.block.and_then(|block| namespaces.get(&block)).copied() {
                    let namespace: &mut DocumentSymbol = &mut root[parent];
                    namespace.children.get_or_insert_with(Vec::new).push(symbol);
                } else {
                    if item.head == Head::Namespace {
                        namespaces.insert(item.form, root.len());
                    }
                    root.push(symbol);
                }
            }
            return Some(DocumentSymbolResponse::Nested(root));
        }
    }

    let symbols = recovered_sites(document)
        .into_iter()
        .filter_map(|name| {
            let kind = symbol_kind(name.head)?;
            let selection = range(&name.name.span)?;
            Some(SymbolInformation {
                name: name.name.word.into(),
                kind,
                tags: None,
                deprecated: None,
                location: Location::new(document.uri.clone(), selection),
                container_name: name.namespace.map(|parent| parent.word.into()),
            })
        })
        .collect();
    Some(DocumentSymbolResponse::Flat(symbols))
}

/// 回復後に元の入力から確認できた名前を、文書順で返す。
pub(super) fn recovered_sites(document: OpenDocument<'_>) -> Vec<RecoveredName<'_>> {
    let Some(text) = document.text else {
        return Vec::new();
    };
    let names = match document.analysis {
        Some(cache) => recover_circuit_names_with_cache(text, cache),
        None => recover_circuit_names(text),
    };
    let names = &names;
    (0..names.position_group_count())
        .flat_map(|group| {
            (0..names.position_count(group)).filter_map(move |item| names.position(group, item))
        })
        .filter_map(|site| names.name_at(site))
        .collect()
}
