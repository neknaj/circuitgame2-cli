//! NCG2M の構文回復と元入力の違反を LSP 診断に変換する。

use circuitgame2_core::{
    editor::PositionEncoding,
    ncg2m::{
        defn::{DefnHeaderError, DefnPart},
        file::{MacroPart, MetaFileError, recovery::OriginalMetaList},
    },
    syntax::Repair,
};
use lsp_types::{Diagnostic, Uri};

use super::{Report, header, render, repair, syntax};

/// 共通 S 式の修復と、元の括弧で閉じた断片の構文違反を文書位置順に返す。
///
/// S 式の修復がない場合は、断片に含まれない違反も落とさないよう全文の最初の違反を含める。
pub(in super::super) fn render_meta_recovery(
    uri: &Uri,
    source: &str,
    encoding: PositionEncoding,
    first: &MetaFileError,
    repairs: &[Repair],
    lists: &[OriginalMetaList],
) -> Vec<Diagnostic> {
    let first = repairs.is_empty().then(|| meta(first));
    let mut reports: Vec<_> = repairs.iter().map(repair).collect();
    for list in lists {
        let Err(error) = &list.index else {
            continue;
        };
        let mut report = meta(error);
        let base = list.span.start;
        report.span = base + report.span.start..base + report.span.end;
        if let Some((span, _)) = &mut report.related {
            *span = base + span.start..base + span.end;
        }
        if first.as_ref().is_some_and(|first| {
            (first.code, &first.span, &first.related)
                == (report.code, &report.span, &report.related)
        }) {
            continue;
        }
        reports.push(report);
    }
    reports.extend(first);
    render(uri, source, reports, encoding, "NCG2M")
}

fn meta(error: &MetaFileError) -> Report {
    match error {
        MetaFileError::Syntax(error) => syntax(error.kind, error.span.clone()),
        MetaFileError::ExpectedItem(span) => Report {
            span: span.clone(),
            code: "ExpectedMetaItem",
            message: "この位置に NCG2M の宣言または指示を書く。".into(),
            related: None,
        },
        MetaFileError::Directive { span, kind } => header(*kind, span.clone()),
        MetaFileError::Defn(error) => defn_header(error),
        MetaFileError::Defmacro { span, part } => {
            let message = match part {
                MacroPart::Declaration => "`(defmacro name (parameter ...) body)` の形式が必要。",
                MacroPart::Name => "マクロ名に無修飾名を指定する。",
                MacroPart::Parameters => "マクロの仮引数をリストで指定する。",
                MacroPart::ParameterName => "マクロの仮引数名に無修飾名を指定する。",
            };
            Report {
                span: span.clone(),
                code: "InvalidDefmacroHeader",
                message: message.into(),
                related: None,
            }
        }
    }
}

fn defn_header(error: &DefnHeaderError) -> Report {
    let (span, code, message, related) = match error {
        DefnHeaderError::Syntax(error) => return syntax(error.kind, error.span.clone()),
        DefnHeaderError::ExpectedOneDeclaration(span) => (
            span.clone(),
            "ExpectedOneDefn",
            "補助関数の宣言を一つ指定する。",
            None,
        ),
        DefnHeaderError::Expected { span, part } => {
            let message = match part {
                DefnPart::Declaration => "`(defn name ...)` の形式が必要。",
                DefnPart::DeclarationName => "補助関数の名前に無修飾名を指定する。",
                DefnPart::Annotation => "`(as-type signature function)` の形式が必要。",
                DefnPart::Signature => "署名には `(Fn (ArgType ...) ResultType)` の形式を使う。",
                DefnPart::ArgumentTypes => "署名の引数型をリストで指定する。",
                DefnPart::Function => "`(fn (parameter ...) expr)` の形式が必要。",
                DefnPart::ParameterNames => "仮引数名をリストで指定する。",
                DefnPart::ParameterName => "仮引数名に無修飾名を指定する。",
                DefnPart::DataType => "データ型名または `(List T)` の形式が必要。",
            };
            (span.clone(), "InvalidDefnHeader", message, None)
        }
        DefnHeaderError::DuplicateParameter { duplicate, first } => (
            duplicate.clone(),
            "DuplicateDefnParameter",
            "同じ仮引数名が複数ある。",
            Some((first.clone(), "最初の仮引数")),
        ),
        DefnHeaderError::ParameterCount {
            arguments,
            parameters,
        } => (
            parameters.clone(),
            "DefnParameterCount",
            "仮引数の個数が署名の引数型の個数と一致しない。",
            Some((arguments.clone(), "署名の引数型")),
        ),
    };
    Report {
        span,
        code,
        message: message.into(),
        related,
    }
}
