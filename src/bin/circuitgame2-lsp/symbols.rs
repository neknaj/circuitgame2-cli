//! Core の宣言索引を、エディタのアウトラインに写す。

use std::{collections::HashMap, ops::Range};

use circuitgame2_core::{
    editor::{LineIndex, PositionEncoding},
    ncg2m::file::{
        MetaItem, MetaItemKind,
        recovery::{OriginalMetaList, ProvisionalMetaDeclaration},
    },
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

/// 文書全体の索引、または元の括弧から確定したリストの索引を保持する。
pub(super) enum FunctionalOutline {
    Complete(Vec<MetaItem>),
    Fragments {
        lists: Vec<OriginalMetaList>,
        provisional: Vec<ProvisionalMetaDeclaration>,
    },
}

impl FunctionalOutline {
    /// 名前空間、補助関数、マクロを現在の文書順に返す。
    ///
    /// 回復した場合は厳格検査が通ったリストを使う。そのリストの各位置には
    /// リスト先頭の位置を加え、文書上の位置に変換する。
    pub(super) fn symbols(
        &self,
        uri: &Uri,
        text: &str,
        encoding: PositionEncoding,
        hierarchical: bool,
    ) -> Option<DocumentSymbolResponse> {
        let (complete, fragments, provisional): (Option<&[MetaItem]>, &[OriginalMetaList], &[_]) =
            match self {
                Self::Complete(items) => (Some(items), &[], &[]),
                Self::Fragments { lists, provisional } => (None, lists, provisional),
            };
        // 所属未確定の表示を添えられるクライアントにだけ暫定宣言を返す。
        let provisional = if hierarchical { provisional } else { &[] };
        let groups = complete
            .into_iter()
            .map(|items| (0, items, false))
            .chain(fragments.iter().filter_map(|list| {
                Some((list.span.start, list.index.as_ref().ok()?.as_slice(), false))
            }))
            .chain(provisional.iter().map(|declaration| {
                (
                    declaration.span.start,
                    std::slice::from_ref(&declaration.item),
                    true,
                )
            }));
        collect_functional_groups(uri, text, groups, encoding, hierarchical)
    }
}

#[allow(deprecated)]
fn collect_functional_groups<'a>(
    uri: &Uri,
    text: &str,
    groups: impl Iterator<Item = (usize, &'a [MetaItem], bool)>,
    encoding: PositionEncoding,
    hierarchical: bool,
) -> Option<DocumentSymbolResponse> {
    let mut groups = groups.peekable();
    groups.peek()?;
    let lines = LineIndex::new(text);
    let mut flat = Vec::new();
    let mut root = Vec::<DocumentSymbol>::new();
    let mut namespaces = HashMap::<usize, usize>::new();
    for (base, items, provisional) in groups {
        let rebase = |span: &Range<usize>| (base + span.start)..(base + span.end);
        let range =
            |span: &Range<usize>| wire_range(&lines, text.as_bytes(), &rebase(span), encoding);
        for item in items {
            let (name_span, kind) = match &item.kind {
                MetaItemKind::Namespace { path } => (path, SymbolKind::NAMESPACE),
                MetaItemKind::Defn(header) => (&header.name_span, SymbolKind::FUNCTION),
                MetaItemKind::Defmacro { name, .. } => (name, SymbolKind::FUNCTION),
                MetaItemKind::Use { .. } | MetaItemKind::Export { .. } => continue,
            };
            let (Some(name), Some(selection)) = (text.get(rebase(name_span)), range(name_span))
            else {
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
                        .and_then(|span| text.get(rebase(span)))
                        .map(str::to_owned),
                });
                continue;
            }
            let Some(full) = range(&item.span) else {
                continue;
            };
            let symbol = DocumentSymbol {
                name: name.into(),
                detail: if provisional {
                    Some("所属未確定".to_owned())
                } else {
                    matches!(item.kind, MetaItemKind::Defmacro { .. })
                        .then(|| "defmacro".to_owned())
                },
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
                .and_then(|span| namespaces.get(&(base + span.start)));
            if let Some(&parent) = parent {
                root[parent]
                    .children
                    .get_or_insert_with(Vec::new)
                    .push(symbol);
            } else {
                if kind == SymbolKind::NAMESPACE {
                    namespaces.insert(base + name_span.start, root.len());
                }
                root.push(symbol);
            }
        }
    }
    Some(if hierarchical {
        DocumentSymbolResponse::Nested(root)
    } else {
        DocumentSymbolResponse::Flat(flat)
    })
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
