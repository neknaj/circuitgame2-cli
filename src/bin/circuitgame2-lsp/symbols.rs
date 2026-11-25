//! Core の宣言索引を、エディタのアウトラインに写す。

use std::{collections::HashMap, ops::Range};

use circuitgame2_core::{
    editor::{LineIndex, PositionEncoding},
    ncg2m::file::{MetaItem, MetaItemKind},
    syntax::{
        Form,
        circuit::{
            Head, RecoveredName, index_named_build_forms, recover_circuit_names,
            recover_circuit_names_with_cache,
        },
    },
};
use lsp_types::{
    DocumentSymbol, DocumentSymbolResponse, Location, SymbolInformation, SymbolKind, Uri,
};

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

/// 厳格解析で得た名前空間、補助関数、マクロを元の文書順で返す。
///
/// 階層表示は名前空間ブロックの出現ごとに分ける。選択範囲には宣言名を使う。
#[allow(deprecated)]
pub(super) fn collect_functional(
    uri: &Uri,
    text: &str,
    items: &[MetaItem],
    encoding: PositionEncoding,
    hierarchical: bool,
) -> DocumentSymbolResponse {
    let lines = LineIndex::new(text);
    let range = |span: &Range<usize>| wire_range(&lines, text.as_bytes(), span, encoding);
    let mut flat = Vec::new();
    let mut root = Vec::<DocumentSymbol>::new();
    let mut namespaces = HashMap::<usize, usize>::new();
    for item in items {
        let (name_span, kind) = match &item.kind {
            MetaItemKind::Namespace { path } => (path, SymbolKind::NAMESPACE),
            MetaItemKind::Defn(header) => (&header.name_span, SymbolKind::FUNCTION),
            MetaItemKind::Defmacro { name, .. } => (name, SymbolKind::FUNCTION),
            MetaItemKind::Use { .. } | MetaItemKind::Export { .. } => continue,
        };
        let (Some(name), Some(selection)) = (text.get(name_span.clone()), range(name_span)) else {
            continue;
        };
        if !hierarchical {
            flat.push(SymbolInformation {
                name: name.into(),
                kind,
                tags: None,
                deprecated: None,
                location: Location::new(uri.clone(), selection),
                container_name: item
                    .namespace
                    .as_ref()
                    .and_then(|span| text.get(span.clone()))
                    .map(str::to_owned),
            });
            continue;
        }
        let Some(full) = range(&item.span) else {
            continue;
        };
        let symbol = DocumentSymbol {
            name: name.into(),
            detail: matches!(item.kind, MetaItemKind::Defmacro { .. })
                .then(|| "defmacro".to_owned()),
            kind,
            tags: None,
            deprecated: None,
            range: full,
            selection_range: selection,
            children: None,
        };
        let parent = item
            .namespace
            .as_ref()
            .and_then(|span| namespaces.get(&span.start));
        if let Some(&parent) = parent {
            root[parent]
                .children
                .get_or_insert_with(Vec::new)
                .push(symbol);
        } else {
            if kind == SymbolKind::NAMESPACE {
                namespaces.insert(name_span.start, root.len());
            }
            root.push(symbol);
        }
    }
    if hierarchical {
        DocumentSymbolResponse::Nested(root)
    } else {
        DocumentSymbolResponse::Flat(flat)
    }
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
