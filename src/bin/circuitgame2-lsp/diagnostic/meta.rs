//! NCG2M の厳格索引が報告する最初の違反を LSP 診断に変換する。

use circuitgame2_core::{
    editor::PositionEncoding,
    ncg2m::{
        defn::{DefnHeaderError, DefnPart},
        file::{MacroPart, MetaFileError, index_meta_file},
    },
};
use lsp_types::{Diagnostic, Uri};

use super::{Report, header, render, syntax};

/// 宣言と名前解決の指示を検査し、元の文書の主範囲と関連範囲を返す。
pub(in super::super) fn collect_meta(
    uri: &Uri,
    source: &str,
    encoding: PositionEncoding,
) -> Vec<Diagnostic> {
    let Err(error) = index_meta_file(source) else {
        return Vec::new();
    };
    render(uri, source, vec![meta(error)], encoding, "NCG2M")
}

fn meta(error: MetaFileError) -> Report {
    match error {
        MetaFileError::Syntax(error) => syntax(error.kind, error.span),
        MetaFileError::ExpectedItem(span) => Report {
            span,
            code: "ExpectedMetaItem",
            message: "この位置に NCG2M の宣言または指示を書く。".into(),
            related: None,
        },
        MetaFileError::Directive { span, kind } => header(kind, span),
        MetaFileError::Defn(error) => defn_header(error),
        MetaFileError::Defmacro { span, part } => {
            let message = match part {
                MacroPart::Declaration => "`(defmacro name (parameter ...) body)` の形式が必要。",
                MacroPart::Name => "マクロ名に無修飾名を指定する。",
                MacroPart::Parameters => "マクロの仮引数をリストで指定する。",
                MacroPart::ParameterName => "マクロの仮引数名に無修飾名を指定する。",
            };
            Report {
                span,
                code: "InvalidDefmacroHeader",
                message: message.into(),
                related: None,
            }
        }
    }
}

fn defn_header(error: DefnHeaderError) -> Report {
    let (span, code, message, related) = match error {
        DefnHeaderError::Syntax(error) => return syntax(error.kind, error.span),
        DefnHeaderError::ExpectedOneDeclaration(span) => (
            span,
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
            (span, "InvalidDefnHeader", message, None)
        }
        DefnHeaderError::DuplicateParameter { duplicate, first } => (
            duplicate,
            "DuplicateDefnParameter",
            "同じ仮引数名が複数ある。",
            Some((first, "最初の仮引数")),
        ),
        DefnHeaderError::ParameterCount {
            arguments,
            parameters,
        } => (
            parameters,
            "DefnParameterCount",
            "仮引数の個数が署名の引数型の個数と一致しない。",
            Some((arguments, "署名の引数型")),
        ),
    };
    Report {
        span,
        code,
        message: message.into(),
        related,
    }
}
