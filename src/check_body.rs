//! 実行前の検査で、各回路本体の結線と型を確認する。

use circuitgame2_cli::CLOCKED_LIMITS;
use circuitgame2_core::{
    project::{
        CircuitProject, FuncArityError, FuncAtomTypeError, FuncDriverError, FuncLiteralError,
        FuncSignalReason, FuncSignalTypeError, FuncWiringError, LocatedName, ModuleBodyPosition,
        ModuleCallInputsError, ModuleCallsError, ModuleOutputTypesError, UseAliases,
    },
    syntax::circuit::Head,
};

use super::{check_report::Report, diagnostic};

/// ソースとフォームの順に本体を調べ、最初に見つけた違反を返す。
pub(super) fn check<'p, 'a>(
    project: &'p CircuitProject<'a>,
    aliases: &UseAliases<'_, 'p, 'a>,
    report: &mut Option<&mut Report<'_>>,
) -> Result<(), String> {
    for (file, (source, circuit)) in project.sources().iter().zip(project.circuits()).enumerate() {
        for (entry, item) in circuit.syntax.entries.iter().enumerate() {
            let name = match item.head {
                Head::Func | Head::Module => circuit
                    .names
                    .binary_search_by_key(&item.form, |name| name.form)
                    .ok()
                    .map(|index| &circuit.names[index].name),
                _ => continue,
            };
            let Some(name) = name else {
                return Err("回路本体のソース上の名前が見つからない。".into());
            };
            let site = LocatedName {
                source,
                span: name.span.clone(),
            };
            let width = CLOCKED_LIMITS.width;
            if item.head == Head::Func {
                aliases
                    .check_func_signal_types(file, entry, width)
                    .map_err(|error| function_error(&site, error, report))?;
            } else {
                aliases
                    .check_module_calls(ModuleBodyPosition { file, entry }, width)
                    .map_err(|error| {
                        if let Some(report) = report {
                            match &error {
                                ModuleCallsError::Module(ModuleOutputTypesError::Mismatch {
                                    expected,
                                    actual,
                                    ..
                                }) => report.body_error(
                                    "output_type_mismatch",
                                    &expected.origin,
                                    [&actual.origin],
                                ),
                                ModuleCallsError::Call {
                                    reason:
                                        ModuleCallInputsError::Argument {
                                            argument,
                                            required_at,
                                            ..
                                        },
                                    ..
                                } => {
                                    report.body_error("call_argument", argument, required_at.iter())
                                }
                                _ => report.body_error("module_check", &site, []),
                            }
                        }
                        diagnostic::clocked::calls(&diagnostic::located(&site), error)
                    })?;
            }
        }
    }
    Ok(())
}

fn function_error(
    function: &LocatedName<'_, '_>,
    error: FuncSignalTypeError<'_, '_>,
    report: &mut Option<&mut Report<'_>>,
) -> String {
    match error {
        FuncSignalTypeError::Signal(FuncAtomTypeError::OutputType {
            reference,
            expected,
            actual,
        }) => {
            if let Some(report) = report {
                report.body_error(
                    "output_type_mismatch",
                    &reference,
                    [&expected.origin, &actual.origin],
                );
            }
            format!(
                "{} 出力の宣言型は {} だが、駆動元の型は {}。\n  出力の型 {}\n  駆動元の型 {}",
                diagnostic::located(&reference),
                diagnostic::clocked::port_type(expected.ty),
                diagnostic::clocked::port_type(actual.ty),
                diagnostic::located(&expected.origin),
                diagnostic::located(&actual.origin),
            )
        }
        FuncSignalTypeError::Literal(FuncLiteralError::Arity(FuncArityError::Wiring(
            FuncWiringError::Signal { site, reason },
        ))) => {
            let (code, detail, related) = match &reason {
                FuncSignalReason::Undefined => (
                    "undefined_signal",
                    "入力にも定義にもこの信号が見つからない。",
                    None,
                ),
                FuncSignalReason::SelfReference(first) => (
                    "self_reference",
                    "自分自身を定義する式からこの信号を参照している。",
                    Some(first),
                ),
                FuncSignalReason::ForwardReference(first) => (
                    "forward_reference",
                    "この信号は参照より後で定義されている。",
                    Some(first),
                ),
            };
            if let Some(report) = report {
                report.body_error(code, &site, related);
            }
            let mut message = format!("{} {detail}", diagnostic::located(&site));
            if let Some(definition) = related {
                message.push_str(&format!("\n  定義 {}", diagnostic::located(definition)));
            }
            message
        }
        FuncSignalTypeError::Literal(FuncLiteralError::Arity(FuncArityError::Wiring(
            FuncWiringError::Driver(error),
        ))) => {
            let (code, reason, primary, other) = match &error {
                FuncDriverError::DrivesInput { target, input } => (
                    "drives_input",
                    "入力に値を割り当てようとしている。",
                    target,
                    Some(input),
                ),
                FuncDriverError::MultipleDrivers { later, first } => (
                    "multiple_drivers",
                    "同じ信号に複数の駆動元がある。",
                    later,
                    Some(first),
                ),
                FuncDriverError::MissingOutput(output) => (
                    "missing_output_driver",
                    "出力の駆動元が見つからない。",
                    output,
                    None,
                ),
                FuncDriverError::Signature(_) => (
                    "function_signature",
                    "関数の入出力宣言に違反がある。",
                    function,
                    None,
                ),
            };
            if let Some(report) = report {
                report.body_error(code, primary, other);
            }
            let mut message = format!("{} {reason}", diagnostic::located(primary));
            if let Some(first) = other {
                message.push_str(&format!("\n  最初の指定 {}", diagnostic::located(first)));
            }
            message
        }
        FuncSignalTypeError::Literal(FuncLiteralError::Arity(FuncArityError::ArgumentCount {
            call,
            expected,
            actual,
            ..
        })) => {
            if let Some(report) = report {
                report.body_error("argument_count", &call, []);
            }
            format!(
                "{} 引数は {expected} 個必要だが、{actual} 個ある。",
                diagnostic::located(&call)
            )
        }
        FuncSignalTypeError::Literal(FuncLiteralError::Arity(FuncArityError::OutputCount {
            expression,
            left,
            expected,
            actual,
            ..
        })) => {
            if let Some(report) = report {
                report.body_error("output_count", &expression, left.iter());
            }
            format!(
                "{} 出力は {expected} 個必要だが、式が返すのは {actual} 個。",
                diagnostic::located(&expression)
            )
        }
        FuncSignalTypeError::Literal(FuncLiteralError::Width {
            literal,
            required_at,
            reason,
        }) => {
            if let Some(report) = report {
                report.body_error("literal_width", &literal, required_at.iter());
            }
            format!(
                "{} {}",
                diagnostic::located(&literal),
                diagnostic::clocked::literal_width_error(reason),
            )
        }
        _ => {
            if let Some(report) = report {
                report.body_error("function_check", function, []);
            }
            format!(
                "{} 関数の結線または型に違反がある。",
                diagnostic::located(function)
            )
        }
    }
}
