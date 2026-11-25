//! Core の構文索引を、ツールから読める UTF-8 JSON に変換する。

use std::{io::Write, ops::Range};

use circuitgame2_core::{
    project::CircuitProject,
    syntax::{
        AtomKind, Form,
        circuit::{ExpressionReferenceKind, Head},
    },
};
use serde::Serialize;

#[derive(Serialize)]
struct Dump<'a> {
    version: u32,
    sources: Vec<Source<'a>>,
}

#[derive(Serialize)]
struct Source<'a> {
    path: String,
    forms: Vec<Node<'a>>,
    symbols: Vec<Symbol<'a>>,
    references: Vec<Reference>,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Node<'a> {
    Atom {
        atom_kind: &'static str,
        range: Range<usize>,
        text: &'a str,
    },
    List {
        range: Range<usize>,
        subtree_end: usize,
    },
}

#[derive(Serialize)]
struct Symbol<'a> {
    kind: &'static str,
    name: &'a str,
    range: Range<usize>,
    form: usize,
    namespace: Option<usize>,
}

#[derive(Serialize)]
struct Reference {
    kind: &'static str,
    form: usize,
    range: Range<usize>,
}

fn atom_name(kind: AtomKind) -> &'static str {
    match kind {
        AtomKind::Name => "name",
        AtomKind::RelativePath => "relative_path",
        AtomKind::AbsolutePath => "absolute_path",
        AtomKind::Natural => "natural",
        AtomKind::NegativeInteger => "negative_integer",
        AtomKind::Binary => "binary",
        AtomKind::Hexadecimal => "hexadecimal",
        AtomKind::String => "string",
    }
}

fn symbol_kind(head: Head) -> &'static str {
    match head {
        Head::Using => "using",
        Head::Namespace => "namespace",
        Head::Use => "use",
        Head::Export => "export",
        Head::Func => "func",
        Head::Module => "module",
        Head::ModulePart => "module_part",
        Head::Struct => "struct",
        Head::Latch => "latch",
        Head::Memory => "memory",
        Head::Test => "test",
    }
}

/// 確認済みのソースごとに、構文木、宣言名、共有宣言を使う位置を出力する。
///
/// 範囲は UTF-8 の半開バイト範囲。構文木の添字はソースごとに 0 から始まる。
///
/// # Errors
///
/// 構文索引の範囲を元ソースに適用できない場合や、出力に失敗した場合にエラーを返す。
pub(super) fn write(project: &CircuitProject<'_>, out: &mut impl Write) -> Result<(), String> {
    let mut sources = Vec::new();
    for (source, index) in project.sources().iter().zip(project.circuits()) {
        let forms = index
            .syntax
            .forms
            .iter()
            .map(|form| match form {
                Form::Atom { kind, span } => {
                    let text = source.text().get(span.clone()).ok_or_else(|| {
                        "構文索引の範囲が元の UTF-8 本文と一致しない。".to_owned()
                    })?;
                    Ok(Node::Atom {
                        atom_kind: atom_name(*kind),
                        range: span.clone(),
                        text,
                    })
                }
                Form::List { span, end } => Ok(Node::List {
                    range: span.clone(),
                    subtree_end: *end,
                }),
            })
            .collect::<Result<_, String>>()?;
        let symbols = index
            .names
            .iter()
            .map(|entry| Symbol {
                kind: symbol_kind(entry.head),
                name: entry.name.word,
                range: entry.name.span.clone(),
                form: entry.form,
                namespace: entry.block,
            })
            .collect();
        let references = index
            .references
            .iter()
            .map(|reference| Reference {
                kind: match reference.kind {
                    ExpressionReferenceKind::Callee => "callee",
                    ExpressionReferenceKind::CastType => "cast_type",
                },
                form: reference.form,
                range: reference.span.clone(),
            })
            .collect();
        sources.push(Source {
            path: source.id.components().join("/"),
            forms,
            symbols,
            references,
        });
    }
    serde_json::to_writer(
        &mut *out,
        &Dump {
            version: 1,
            sources,
        },
    )
    .map_err(|error| format!("構文索引を JSON で出力できない ({error})。"))?;
    writeln!(out).map_err(super::output_error)
}
