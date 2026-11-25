//! Core が確定した構文と名前解決の診断を LSP に写す。

use std::ops::Range;

use circuitgame2_core::{
    editor::{LineIndex, PositionEncoding, TextPosition},
    project::{
        DefinitionBatchError, DefinitionReferenceReason, LocatedName, check_use_aliases,
        index_use_targets, validate_circuit_basis,
    },
    syntax::{
        ErrorKind, Repair,
        circuit::{
            CachedCircuitRecovery, CircuitFormError, CircuitRecovery, Clause, Head, HeaderError,
            OriginalCircuitList, RecoveredFormOutcome,
        },
    },
};
use lsp_types::{
    Diagnostic, DiagnosticRelatedInformation, DiagnosticSeverity, Location, NumberOrString,
    Position, Uri,
};
use serde_json::json;

use super::sources::SourceSet;

mod local;
mod meta;
pub(super) use local::LocalRecovery;
pub(super) use meta::collect_meta;

#[derive(Clone)]
struct Report {
    span: Range<usize>,
    code: &'static str,
    message: String,
    related: Option<(Range<usize>, &'static str)>,
}

/// 元のバイト範囲も添え、主範囲の位置順で診断を返す。
pub(super) fn collect(
    uri: &Uri,
    analysis: &CachedCircuitRecovery,
    encoding: PositionEncoding,
) -> Vec<Diagnostic> {
    let source = analysis.text();
    let reports = match analysis.recovery() {
        CircuitRecovery::Complete(_) => return Vec::new(),
        CircuitRecovery::Inspected { checked, .. } => checked
            .iter()
            .filter_map(|item| match &item.outcome {
                RecoveredFormOutcome::Valid(_) => None,
                RecoveredFormOutcome::Rejected { kind, span, .. } => {
                    Some(header(*kind, span.clone()))
                }
            })
            .collect(),
        CircuitRecovery::WithSyntaxRepairs {
            recovered,
            original,
        } => {
            let mut reports: Vec<_> = recovered.repairs.iter().map(repair).collect();
            reports.extend(original.iter().flatten().flat_map(original_reports));
            reports
        }
    };
    render(uri, source, reports, encoding, "NCG2")
}

fn original_reports(list: &OriginalCircuitList) -> impl Iterator<Item = Report> + '_ {
    list.source_diagnostics()
        .iter()
        .flatten()
        .map(|error| match error {
            CircuitFormError::Syntax(error) => syntax(error.kind, error.span.clone()),
            CircuitFormError::Header { kind, span } => header(*kind, span.clone()),
        })
}

fn render(
    uri: &Uri,
    source: &str,
    mut reports: Vec<Report>,
    encoding: PositionEncoding,
    source_label: &'static str,
) -> Vec<Diagnostic> {
    reports.sort_by_key(|report| (report.span.start, report.span.end));
    let lines = LineIndex::new(source);
    reports
        .into_iter()
        .filter_map(|report| {
            let range = wire_range(&lines, source.as_bytes(), &report.span, encoding)?;
            let related_information = report.related.as_ref().and_then(|(related_span, label)| {
                let range = wire_range(&lines, source.as_bytes(), related_span, encoding)?;
                Some(vec![DiagnosticRelatedInformation {
                    location: Location::new(uri.clone(), range),
                    message: (*label).to_owned(),
                }])
            });
            Some(Diagnostic {
                range,
                severity: Some(DiagnosticSeverity::ERROR),
                source: Some(source_label.to_owned()),
                code: Some(NumberOrString::String(report.code.to_owned())),
                message: report.message,
                related_information,
                data: Some(json!({
                    "byteStart": report.span.start,
                    "byteEnd": report.span.end,
                    "related": report.related.as_ref().map(|(span, _)| [span.start, span.end]),
                })),
                ..Diagnostic::default()
            })
        })
        .collect()
}

/// 同じソース集合の構文、宣言、別名を確定できたときに参照診断を返す。
pub(super) fn collect_references(
    project: &circuitgame2_core::project::CircuitProject<'_>,
    sources: &SourceSet<'_>,
    encoding: PositionEncoding,
) -> Vec<(Uri, Diagnostic)> {
    if validate_circuit_basis(project).is_err() {
        return Vec::new();
    }
    let Ok(targets) = index_use_targets(project) else {
        return Vec::new();
    };
    let Ok(scope) = check_use_aliases(&targets) else {
        return Vec::new();
    };
    let resolved = scope.resolve_definitions_with_diagnostics();
    resolved
        .diagnostics
        .into_iter()
        .filter_map(|item| reference_diagnostic(item, sources, encoding))
        .collect()
}

fn reference_diagnostic(
    item: DefinitionBatchError<'_, '_>,
    sources: &SourceSet<'_>,
    encoding: PositionEncoding,
) -> Option<(Uri, Diagnostic)> {
    let name = item.site.name.word;
    let span = item.site.name.span;
    let primary = sources.location(
        LocatedName {
            source: item.site.source,
            span: span.clone(),
        },
        encoding,
    )?;
    let (code, message, related) = match item.reason {
        DefinitionReferenceReason::Missing => (
            "MissingReference",
            format!("「{name}」の宣言が見つからない。"),
            None,
        ),
        DefinitionReferenceReason::MissingPrefix => (
            "MissingReferencePrefix",
            format!("「{name}」の先頭の名前を、このスコープで解決できない。"),
            None,
        ),
        DefinitionReferenceReason::DeclarationPrefix { source, form } => (
            "ExpectedNamespacePrefix",
            format!("「{name}」の先頭には名前空間が必要。"),
            Some((
                LocatedName {
                    source,
                    span: form.name.span.clone(),
                },
                "先頭の名前が指す宣言",
            )),
        ),
        DefinitionReferenceReason::Private(location) => (
            "PrivateReference",
            format!("「{name}」は参照元の名前空間に公開されていない。"),
            Some((location, "非公開の宣言")),
        ),
        DefinitionReferenceReason::Namespace(location) => (
            "ExpectedDeclaration",
            format!("「{name}」の位置には宣言名が必要。"),
            Some((location, "参照先の名前空間")),
        ),
        DefinitionReferenceReason::WrongDeclaration { source, form } => (
            "WrongDeclarationKind",
            format!(
                "「{name}」は、この位置では使えない {} の宣言を指している。",
                head_name(form.head)
            ),
            Some((
                LocatedName {
                    source,
                    span: form.name.span.clone(),
                },
                "参照先の宣言",
            )),
        ),
    };
    let related_information = related.and_then(|(position, label)| {
        let location = sources.location(position, encoding)?;
        Some(vec![DiagnosticRelatedInformation {
            location,
            message: label.to_owned(),
        }])
    });
    Some((
        primary.uri,
        Diagnostic {
            range: primary.range,
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("NCG2".to_owned()),
            code: Some(NumberOrString::String(code.to_owned())),
            message,
            related_information,
            data: Some(json!({ "byteStart": span.start, "byteEnd": span.end })),
            ..Diagnostic::default()
        },
    ))
}

pub(super) fn wire_range(
    lines: &LineIndex<'_>,
    bytes: &[u8],
    span: &Range<usize>,
    encoding: PositionEncoding,
) -> Option<lsp_types::Range> {
    // CRLF の内部は LSP の位置にできない。表示範囲だけを改行の境界まで広げる。
    let inside_crlf = |at: usize| {
        at.checked_sub(1).and_then(|i| bytes.get(i)) == Some(&b'\r')
            && bytes.get(at) == Some(&b'\n')
    };
    let start = if inside_crlf(span.start) {
        span.start - 1
    } else {
        span.start
    };
    let end = if span.is_empty() {
        start
    } else if inside_crlf(span.end) {
        span.end + 1
    } else {
        span.end
    };
    let to_wire = |position: TextPosition| {
        Some(Position::new(
            i32::try_from(position.line).ok()? as u32,
            i32::try_from(position.column).ok()? as u32,
        ))
    };
    Some(lsp_types::Range::new(
        to_wire(lines.byte_to_position(start, encoding)?)?,
        to_wire(lines.byte_to_position(end, encoding)?)?,
    ))
}

fn repair(value: &Repair) -> Report {
    match value {
        Repair::Skipped { error, .. } => syntax(error.kind, error.span.clone()),
        Repair::InsertedSeparator { offset } => {
            syntax(ErrorKind::MissingSeparator, *offset..*offset)
        }
        Repair::InsertedClose {
            offset, opening, ..
        } => Report {
            span: *offset..*offset,
            code: "UnclosedList",
            message: "閉じ括弧 `)` が必要。".into(),
            related: Some((opening.clone(), "対応する開き括弧")),
        },
    }
}

fn syntax(kind: ErrorKind, span: Range<usize>) -> Report {
    let (code, message, related) = match kind {
        ErrorKind::InvalidAtom => ("InvalidAtom", "アトムの表記を確認する。", None),
        ErrorKind::MissingSeparator => (
            "MissingSeparator",
            "アトムの間に空白などの区切りが必要。",
            None,
        ),
        ErrorKind::InvalidEscape { opening } => (
            "InvalidEscape",
            "文字列で使えないエスケープ。",
            Some((opening..opening.saturating_add(1), "文字列の開き引用符")),
        ),
        ErrorKind::UnterminatedString => ("UnterminatedString", "文字列の閉じ引用符が必要。", None),
        ErrorKind::NewlineInString { opening } => (
            "NewlineInString",
            "改行の前に文字列を閉じる。",
            Some((opening..opening.saturating_add(1), "文字列の開き引用符")),
        ),
        ErrorKind::UnexpectedClose => (
            "UnexpectedClose",
            "閉じ括弧に対応する開き括弧がない。",
            None,
        ),
        ErrorKind::UnclosedList { eof } => (
            "UnclosedList",
            "閉じ括弧 `)` が必要。",
            Some((eof..eof, "閉じ括弧を補う位置")),
        ),
    };
    Report {
        span,
        code,
        message: message.to_owned(),
        related,
    }
}

fn header(kind: HeaderError, span: Range<usize>) -> Report {
    let (code, message) = match kind {
        HeaderError::ExpectedList => ("ExpectedList", "回路フォームを括弧で囲む。"),
        HeaderError::ExpectedHead => ("ExpectedHead", "リストの先頭に回路フォームの種別が必要。"),
        HeaderError::UnknownHead => ("UnknownHead", "回路フォームの先頭語を確認する。"),
        HeaderError::MisplacedHead(head) => {
            return Report {
                span,
                code: "MisplacedHead",
                message: format!("この位置に `{}` を置くことはできない。", head_name(head)),
                related: None,
            };
        }
        HeaderError::ExpectedNamespacePath => (
            "ExpectedNamespacePath",
            "名前空間を名前か相対パスで指定する。",
        ),
        HeaderError::ExpectedNor => ("ExpectedNor", "基底を `nor` で指定する。"),
        HeaderError::ExpectedAbsolutePath => ("ExpectedAbsolutePath", "完全修飾名が必要。"),
        HeaderError::ExpectedName => ("ExpectedName", "修飾子を含まない名前が必要。"),
        HeaderError::ExpectedReference => (
            "ExpectedReference",
            "名前、相対パス、完全修飾名のいずれかが必要。",
        ),
        HeaderError::ExpectedNatural => ("ExpectedNatural", "自然数が必要。"),
        HeaderError::ExpectedPort => ("ExpectedPort", "ポート名または型付きポートが必要。"),
        HeaderError::ExpectedType => ("ExpectedType", "型への参照か、ビット幅を指定する型が必要。"),
        HeaderError::ExpectedBodyItem => (
            "ExpectedBodyItem",
            "回路本体の項目には `def` または `expand` を使う。",
        ),
        HeaderError::ExpectedTarget => (
            "ExpectedTarget",
            "`def` の出力先を名前か名前のリストで指定する。",
        ),
        HeaderError::ExpectedExpression => ("ExpectedExpression", "回路式が必要。"),
        HeaderError::ExpectedTestItem => {
            ("ExpectedTestItem", "テスト本体に `case` または操作が必要。")
        }
        HeaderError::MixedTestItems {
            first_head: (start, end),
        } => {
            return Report {
                span,
                code: "MixedTestItems",
                message: "一つのテストに `case` と操作を混在させることはできない。".into(),
                related: Some((start..end, "テスト本体の最初の項目")),
            };
        }
        HeaderError::ExpectedTestValue => (
            "ExpectedTestValue",
            "テスト用のリテラルか、型付きリテラルが必要。",
        ),
        HeaderError::ExpectedTestValueOrAny => (
            "ExpectedTestValueOrAny",
            "期待値にはテスト用の値か `any` を使う。",
        ),
        HeaderError::ExpectedClause(clause) => {
            return Report {
                span,
                code: "ExpectedClause",
                message: format!("`{}` の節が必要。", clause_name(clause)),
                related: None,
            };
        }
        HeaderError::UnexpectedArgument => ("UnexpectedArgument", "この位置に引数を追加できない。"),
    };
    Report {
        span,
        code,
        message: message.to_owned(),
        related: None,
    }
}

fn head_name(head: Head) -> &'static str {
    match head {
        Head::Using => "using",
        Head::Namespace => "namespace",
        Head::Use => "use",
        Head::Export => "export",
        Head::Func => "func",
        Head::Module => "module",
        Head::ModulePart => "module-part",
        Head::Struct => "struct",
        Head::Latch => "latch",
        Head::Memory => "memory",
        Head::Test => "test",
    }
}

fn clause_name(clause: Clause) -> &'static str {
    match clause {
        Clause::In => "in",
        Clause::Out => "out",
        Clause::Expect => "expect",
        Clause::Bits => "bits",
        Clause::Width => "width",
        Clause::Address => "address",
        Clause::Data => "data",
        Clause::At => "at",
    }
}
