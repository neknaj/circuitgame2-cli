//! 展開の診断を呼び出し位置と原因の元範囲から表示する。

use circuitgame2_core::{
    ncg2m::{
        call_evaluation::{
            macro_binding::MacroArgumentError, macro_expansion::MacroExpansionErrorKind,
        },
        expression_types::{DeferredExpression, ExpressionCheckFailure, MacroBodyError},
        template::TemplateError,
    },
    netlist::PortBitLayoutError,
    project::{
        BitFuncLowerError, CheckedBitFunc, DefinitionAnalysisError, DefmacroNameError,
        ExpandedFuncWiringError, GeneratedWiringError, LocatedName, MacroCallError,
        MacroPreparationError, MacroPreparationFailure, ProjectSource,
    },
    syntax::circuit::GeneratedDefinitionError,
};

use super::{Report, diagnostic, errors, failure, site, unsupported};

/// Core が展開後の本体で確認した違反を、元の文書ごとに表示する。
pub(super) fn wiring<'p, 'a>(
    error: ExpandedFuncWiringError<'p, 'a>,
    first: LocatedName<'p, 'a>,
    report: &mut Option<&mut Report<'_>>,
) -> String {
    let (error, expand) = match error {
        ExpandedFuncWiringError::Generated { call, cause } => (*cause, Some(call)),
        error => (error, None),
    };
    let (code, at, reason, related) = match error {
        ExpandedFuncWiringError::Base(error) => return base(error, first, expand, report),
        ExpandedFuncWiringError::Basis(error) => {
            let error = DefinitionAnalysisError::Basis(error);
            if let Some(report) = report {
                report.analysis_error(&error);
            }
            return diagnostic::analysis_error(error);
        }
        ExpandedFuncWiringError::UnsupportedOrdinary(location) => {
            return unsupported(
                report,
                location,
                "この検査では右辺に信号、1ビット定数、基底 NOR を指定できる。",
            );
        }
        ExpandedFuncWiringError::NorScope(location) => (
            "missing_reference",
            location,
            "基底 NOR の名前を解決するスコープが見つからない。".into(),
            None,
        ),
        ExpandedFuncWiringError::NorResolvesToFunction {
            callee,
            declaration,
        } => (
            "unsupported_mixed_circuit_check",
            callee,
            "この検査が扱う呼び出しは基底 NOR。".into(),
            Some(("回路関数の宣言", declaration.name)),
        ),
        ExpandedFuncWiringError::NorName { callee, error } => {
            let (code, reason, related) = errors::circuit_reference_details(
                error,
                "呼び出し先が見つからない。",
                "呼び出し先に基底 NOR を指定する。",
            );
            (code, callee, reason.into(), related)
        }
        ExpandedFuncWiringError::UnavailableDefinition { reference, target } => (
            "forward_reference",
            reference,
            "この信号を駆動する定義が参照より前にない。".into(),
            Some(("駆動元の定義", target)),
        ),
        ExpandedFuncWiringError::OutputWidthFromDefinition {
            target,
            output,
            expected,
            actual,
        } => (
            "output_type_mismatch",
            target,
            format!("出力の宣言幅は {expected} ビットだが、駆動元の幅は {actual} ビット。"),
            Some(("出力の宣言幅", output)),
        ),
        ExpandedFuncWiringError::NorInputWidth { reference, width } => (
            "call_argument",
            reference,
            format!("基底 NOR の入力には1ビットが必要。ここで参照した信号は {width} ビット。"),
            None,
        ),
        ExpandedFuncWiringError::Generated { call, cause } => {
            return wiring(
                ExpandedFuncWiringError::Generated { call, cause },
                first,
                report,
            );
        }
    };
    report_cause(report, code, at, &reason, related, expand)
}

fn report_cause<'p, 'a>(
    report: &mut Option<&mut Report<'_>>,
    code: &'static str,
    cause: LocatedName<'p, 'a>,
    reason: &str,
    related: impl IntoIterator<Item = (&'static str, LocatedName<'p, 'a>)>,
    expand: Option<LocatedName<'p, 'a>>,
) -> String {
    match expand {
        Some(call) => failure(
            report,
            code,
            call,
            reason,
            [("原因", cause)].into_iter().chain(related),
        ),
        None => failure(report, code, cause, reason, related),
    }
}

fn base<'p, 'a>(
    error: GeneratedWiringError<'p, 'a>,
    first: LocatedName<'p, 'a>,
    expand: Option<LocatedName<'p, 'a>>,
    report: &mut Option<&mut Report<'_>>,
) -> String {
    let (code, location, reason, related) = match error {
        GeneratedWiringError::Signature(error) => {
            return errors::mixed_signature(error, expand, report);
        }
        GeneratedWiringError::Preparation(error) => return preparation(error, report),
        GeneratedWiringError::UnsupportedBody => (
            "unsupported_mixed_circuit_check",
            first,
            "この検査が扱う関数には出力が必要。".into(),
            None,
        ),
        GeneratedWiringError::Fuel(call) => (
            "macro_expansion_fuel",
            call,
            "マクロ展開に割り当てた実行枠を使い切った。".into(),
            None,
        ),
        GeneratedWiringError::Expansion {
            call,
            definition,
            related,
            kind,
        } => {
            let reason = match kind {
                MacroExpansionErrorKind::Expression { error, .. } => match error {
                    MacroArgumentError::Deferred(_) => "この式の評価には対応していない。",
                    MacroArgumentError::Unbound(_) => "参照に対応する仮引数がない。",
                    MacroArgumentError::NoExpression => "評価する式が見つからない。",
                },
                MacroExpansionErrorKind::Template(_) => {
                    "評価した挿入値からテンプレートを組み立てられない。"
                }
            };
            (
                "macro_expansion",
                call,
                reason.into(),
                Some(("マクロ本体内の原因", site(definition, related))),
            )
        }
        GeneratedWiringError::InvalidDefinition {
            call,
            element,
            related,
            kind,
        } => {
            let reason = match kind {
                GeneratedDefinitionError::Empty => "生成された構文が空。",
                GeneratedDefinitionError::ExtraRoot(_) => "一要素に複数のフォームがある。",
                GeneratedDefinitionError::ExpectedDefinition(_) => "生成結果には def が必要。",
                GeneratedDefinitionError::Invalid { .. } => {
                    "生成した def の左辺または右辺が構文に合わない。"
                }
                GeneratedDefinitionError::ForbiddenHead { .. } => {
                    "生成した def に展開結果で禁止されているフォームがある。"
                }
            };
            (
                "macro_generated_definition",
                call,
                format!("展開結果の{}番目の要素。{reason}", element + 1),
                related.map(|at| ("生成元の構文", at)),
            )
        }
        GeneratedWiringError::InvalidProjection {
            call,
            element,
            related,
            ..
        } => (
            "macro_generated_syntax",
            call,
            format!(
                "展開結果の{}番目の要素から回路構文を作れない。",
                element + 1
            ),
            related.map(|at| ("生成元の構文", at)),
        ),
        GeneratedWiringError::UnsupportedDefinition { call, element } => (
            "unsupported_mixed_circuit_check",
            call,
            format!(
                "展開結果の{}番目の定義は、この検査が扱う回路構文の範囲外。",
                element + 1
            ),
            None,
        ),
        GeneratedWiringError::PortType(at) => (
            "unsupported_mixed_circuit_check",
            at,
            "この検査が扱うポートの型はビット列。".into(),
            None,
        ),
        GeneratedWiringError::Unavailable(at) => (
            "unbound_signal",
            at,
            "信号の駆動元が見つからない。".into(),
            None,
        ),
        GeneratedWiringError::MultipleDrivers(at) => (
            "multiple_drivers",
            at,
            "同じ信号を複数の箇所で駆動している。".into(),
            None,
        ),
        GeneratedWiringError::MissingOutput(at) => (
            "missing_output",
            at,
            "出力の駆動元が見つからない。".into(),
            None,
        ),
        GeneratedWiringError::OutputWidth {
            site,
            expected,
            actual,
        } => (
            "output_type_mismatch",
            site,
            format!("出力の宣言幅は {expected} ビットだが、駆動元の幅は {actual} ビット。"),
            None,
        ),
        GeneratedWiringError::Capacity => (
            "capacity",
            first,
            "局所信号の索引を確保できない。".into(),
            None,
        ),
    };
    report_cause(report, code, location, &reason, related, expand)
}

fn preparation(
    error: MacroPreparationError<'_, '_>,
    report: &mut Option<&mut Report<'_>>,
) -> String {
    let primary = site(error.source, error.site.span);
    let callee = site(error.source, error.site.callee);
    match error.kind {
        MacroPreparationFailure::Call(MacroCallError::Name(DefmacroNameError::Reference(
            reason,
        ))) => {
            let (code, message, related) = errors::circuit_reference_details(
                reason,
                "呼び出し先のマクロが見つからない。",
                "呼び出し先にマクロを指定する。",
            );
            report_cause(report, code, callee, message, related, Some(primary))
        }
        MacroPreparationFailure::Call(MacroCallError::Name(reason)) => {
            let message = match reason {
                DefmacroNameError::Caller => "この位置にマクロ展開は書けない。",
                DefmacroNameError::InvalidName => "マクロ名を解釈できない。",
                DefmacroNameError::Reference(_) => unreachable!(),
            };
            report_cause(report, "macro_call", callee, message, [], Some(primary))
        }
        MacroPreparationFailure::Call(MacroCallError::Arity { definition, actual }) => failure(
            report,
            "macro_argument_count",
            primary,
            &format!(
                "マクロには実引数が {} 個必要だが、{actual} 個を指定している。",
                definition.parameters.len()
            ),
            [("マクロの宣言", definition.declaration.name)],
        ),
        MacroPreparationFailure::Body { definition, error } => body(
            definition.declaration.name.source,
            error,
            Some(primary),
            report,
        ),
    }
}

/// マクロの型検査で得たのが違反か未対応の式かを区別し、原因の位置を表示する。
pub(super) fn body<'p, 'a>(
    source: &'p ProjectSource<'a>,
    error: MacroBodyError<'_>,
    expand: Option<LocatedName<'p, 'a>>,
    report: &mut Option<&mut Report<'_>>,
) -> String {
    let (code, range, reason, related) = match error {
        MacroBodyError::Template(error) => {
            let (range, reason) = match error {
                TemplateError::SyntaxArity(range) => {
                    (range, "syntax にはテンプレートを一つ指定する。")
                }
                TemplateError::SpliceAtRoot(range) => {
                    (range, "splice はテンプレート内のリストの要素に指定する。")
                }
                TemplateError::InsertionArity(range) => {
                    (range, "unsyntax と splice には式を一つ指定する。")
                }
            };
            ("macro_template", range, reason.into(), None)
        }
        MacroBodyError::Expression {
            error: ExpressionCheckFailure::Type(error),
            ..
        } => {
            let (range, reason, related) = errors::expression_type(error);
            ("ncg2m_expression", range, reason, related)
        }
        MacroBodyError::Deferred(error)
        | MacroBodyError::Expression {
            error: ExpressionCheckFailure::Deferred(error),
            ..
        } => {
            let range = match error {
                DeferredExpression::Form(range)
                | DeferredExpression::Name(range)
                | DeferredExpression::BooleanBinding(range)
                | DeferredExpression::StageType(range) => range,
            };
            (
                "unsupported_macro_body",
                range,
                "マクロ本体のこの式の検査には対応していない。".into(),
                None,
            )
        }
    };
    report_cause(
        report,
        code,
        site(source, range),
        &reason,
        related.map(|(label, range)| (label, site(source, range))),
        expand,
    )
}

/// 幅の違反を発生させたポート、または定義を含む関数の位置を表示する。
pub(super) fn layout_error(
    function: &CheckedBitFunc<'_, '_>,
    declaration: &LocatedName<'_, '_>,
    error: BitFuncLowerError,
) -> String {
    let (label, ports, error) = match error {
        BitFuncLowerError::Input(error) => {
            ("入力ポート", function.signature().inputs.as_slice(), error)
        }
        BitFuncLowerError::Definition(error) => ("定義", &[][..], error),
        BitFuncLowerError::Output(error) => {
            ("出力ポート", function.signature().outputs.as_slice(), error)
        }
    };
    let (index, reason) = match error {
        PortBitLayoutError::ZeroWidth { port } => (port, "幅には正の整数が必要。".into()),
        PortBitLayoutError::CapacityExceeded {
            port,
            width,
            used,
            capacity,
        } => (
            port,
            format!("{used} ビットに {width} ビットを加えると、上限 {capacity} ビットを超える。"),
        ),
    };
    let at = ports
        .get(index)
        .map_or(declaration, |port| &port.annotation);
    format!(
        "{} {}番目の{label}。{reason}",
        diagnostic::located(at),
        index + 1
    )
}
