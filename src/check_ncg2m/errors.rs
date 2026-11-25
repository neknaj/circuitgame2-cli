//! 混在索引と補助関数の診断を、元のソース範囲を保持して表示する。

use std::ops::Range;

use circuitgame2_core::{
    ncg2m::{
        data_types::{BuiltinDataType, DataType, DataTypeAtom},
        defn::DefnHeaderError,
        expression_types::{
            DeferredExpression, DefnBodyCallError, ExpressionCheckFailure, ExpressionTypeError,
            FoldSyntaxError, LetSyntaxError,
        },
        file::MetaFileError,
        integers::{IntegerLiteralError, IntegerOperation, IntegerType},
    },
    project::{
        CallInputError, CallInputReason, CircuitProjectError, DefnCallError, DefnCallGraphError,
        DefnNameError, FuncArityError, FuncCallOutputError, FuncCallTarget, FuncLiteralError,
        FuncSignalTypeError, FuncWiringError, LocatedName, MixedFuncArityError, MixedFuncCallError,
        MixedFuncCalleeError, MixedFuncDriverError, MixedFuncTypeError, MixedFuncWiringError,
        MixedTypedSignatureError, ParsedProjectSources, ProjectFileSyntaxError, ProjectSource,
        ProjectSourceParseError, RoutineSignatureError, TypedSignatureError,
    },
};

use super::{Report, diagnostic, failure, site};
use crate::check_body;

pub(super) fn parse(
    error: ProjectSourceParseError<'_>,
    report: &mut Option<&mut Report<'_>>,
) -> String {
    let error = match error {
        ProjectSourceParseError::Collection(error) => CircuitProjectError::Sources(error),
        ProjectSourceParseError::File {
            id,
            revision,
            error,
            ..
        } => match error {
            ProjectFileSyntaxError::Circuit(error) => CircuitProjectError::Syntax {
                id,
                revision,
                error,
            },
            ProjectFileSyntaxError::Functional(error) => {
                let path = id.components().join("/");
                if let Some(report) = report {
                    let (span, related) = match &error {
                        MetaFileError::Syntax(error) => (error.span.clone(), None),
                        MetaFileError::ExpectedItem(span)
                        | MetaFileError::Directive { span, .. }
                        | MetaFileError::Defmacro { span, .. } => (span.clone(), None),
                        MetaFileError::Defn(error) => header_ranges(error),
                    };
                    report.file_error("ncg2m_syntax", &path, span, related);
                }
                return diagnostic::meta_file_error(&path, error);
            }
        },
    };
    if let Some(report) = report {
        report.project_error(&error);
    }
    diagnostic::project_error(error)
}

/// 呼び出し元と宣言元の範囲を保って混在関数の型診断を表示する。
pub(super) fn mixed_function_types(
    function: &LocatedName<'_, '_>,
    error: MixedFuncTypeError<'_, '_>,
    report: &mut Option<&mut Report<'_>>,
) -> String {
    match error {
        MixedFuncTypeError::Arity(error) => mixed_function(function, error, report),
        MixedFuncTypeError::Literal(FuncLiteralError::Width {
            literal,
            required_at,
            reason,
        }) => failure(
            report,
            "literal_width",
            literal,
            &diagnostic::clocked::literal_width_error(reason),
            required_at.map(|required| ("必要な幅の指定", required)),
        ),
        MixedFuncTypeError::Literal(error) => {
            check_body::function_error(function, FuncSignalTypeError::Literal(error), report)
        }
        MixedFuncTypeError::Signature { call, error } => {
            mixed_signature(error, Some(site(call.source, call.call.span)), report)
        }
        MixedFuncTypeError::Output(FuncCallOutputError::OutputType {
            target,
            call,
            expected,
            actual,
        }) => {
            let call = (!same_location(&call, &actual.origin)).then_some(("呼び出し式", call));
            failure(
                report,
                "output_type_mismatch",
                target,
                &format!(
                    "出力の宣言型は {} だが、呼び出しが返す型は {}。",
                    diagnostic::clocked::port_type(expected.ty),
                    diagnostic::clocked::port_type(actual.ty),
                ),
                [
                    ("出力の型", expected.origin),
                    ("呼び出しが返す型", actual.origin),
                ]
                .into_iter()
                .chain(call),
            )
        }
        MixedFuncTypeError::Output(error) => {
            check_body::function_error(function, FuncSignalTypeError::CallOutput(error), report)
        }
        MixedFuncTypeError::Signal(error) => {
            check_body::function_error(function, FuncSignalTypeError::Signal(error), report)
        }
        MixedFuncTypeError::Input(CallInputError::Argument {
            argument,
            expected,
            required_at,
            reason,
            ..
        }) => {
            let required = required_at.map(|required| ("入力の型", required));
            let (reason, actual) = match reason {
                CallInputReason::LiteralWidth(error) => {
                    (diagnostic::clocked::literal_width_error(error), None)
                }
                CallInputReason::StructRequired => ("構造体の値を指定する。".into(), None),
                CallInputReason::SignalType(actual) => (
                    format!(
                        "実引数の型は {}。",
                        diagnostic::clocked::port_type(actual.ty)
                    ),
                    (!same_location(&argument, &actual.origin))
                        .then_some(("実引数の型の根拠", actual.origin)),
                ),
            };
            failure(
                report,
                "call_argument",
                argument,
                &format!(
                    "入力には {} が必要。{reason}",
                    diagnostic::clocked::port_type(expected),
                ),
                required.into_iter().chain(actual),
            )
        }
        MixedFuncTypeError::Input(CallInputError::Caller(error)) => {
            check_body::function_error(function, FuncSignalTypeError::Signal(error), report)
        }
        MixedFuncTypeError::Input(_) => failure(
            report,
            "function_check",
            site(function.source, function.span.clone()),
            "関数の呼び出しの入力に違反がある。",
            [],
        ),
    }
}

fn same_location(left: &LocatedName<'_, '_>, right: &LocatedName<'_, '_>) -> bool {
    std::ptr::eq(left.source, right.source) && left.span == right.span
}

/// 混在プロジェクトの関数で Core が選んだ違反を、関連するソース位置とともに表示する。
fn mixed_function(
    function: &LocatedName<'_, '_>,
    error: MixedFuncArityError<'_, '_>,
    report: &mut Option<&mut Report<'_>>,
) -> String {
    let shared = match error {
        MixedFuncArityError::Reference(MixedFuncCallError { site: call, reason }) => {
            return circuit_reference(
                site(call.source, call.name.span),
                reason,
                "呼び出し先の回路関数が見つからない。",
                "呼び出し先に回路関数を指定する。",
                None,
                report,
            );
        }
        MixedFuncArityError::Wiring(MixedFuncWiringError::Signal { site, reason }) => {
            FuncArityError::Wiring(FuncWiringError::Signal { site, reason })
        }
        MixedFuncArityError::Wiring(MixedFuncWiringError::Driver(
            MixedFuncDriverError::Driver(error),
        )) => FuncArityError::Wiring(FuncWiringError::Driver(error)),
        MixedFuncArityError::Wiring(MixedFuncWiringError::Driver(
            MixedFuncDriverError::Signature(error),
        )) => return mixed_signature(error, None, report),
        MixedFuncArityError::Count(FuncArityError::OutputCount {
            expression,
            left,
            callee,
            expected,
            actual,
        }) => {
            let declaration = callee.into_iter().filter_map(|target| match target {
                FuncCallTarget::Nor => None,
                FuncCallTarget::Function { declaration, .. } => {
                    Some(("呼び出し先の宣言", declaration))
                }
            });
            return failure(
                report,
                "output_count",
                expression,
                &format!("出力は {expected} 個必要だが、式が返すのは {actual} 個。"),
                left.into_iter()
                    .map(|left| ("定義の左辺", left))
                    .chain(declaration),
            );
        }
        MixedFuncArityError::Count(FuncArityError::ArgumentCount {
            call,
            target,
            expected,
            actual,
        }) => {
            let declaration = match &target {
                FuncCallTarget::Nor => None,
                FuncCallTarget::Function { declaration, .. } => Some(declaration),
            };
            return check_body::argument_count(&call, expected, actual, declaration, report);
        }
        MixedFuncArityError::Count(error) => error,
    };
    check_body::function_error(
        function,
        FuncSignalTypeError::Literal(FuncLiteralError::Arity(shared)),
        report,
    )
}

fn mixed_signature<'p, 'a>(
    error: MixedTypedSignatureError<'p, 'a>,
    caller: Option<LocatedName<'p, 'a>>,
    report: &mut Option<&mut Report<'_>>,
) -> String {
    let caller = caller.map(|call| ("呼び出し式", call));
    let signature = match error {
        MixedTypedSignatureError::Signature(error)
        | MixedTypedSignatureError::Shared(TypedSignatureError::Signature(error)) => error,
        MixedTypedSignatureError::TypeReference {
            reference, reason, ..
        } => {
            return circuit_reference(
                reference,
                reason,
                "ポートの型名が指す構造体が見つからない。",
                "ポートの型名に構造体を指定する。",
                caller,
                report,
            );
        }
        MixedTypedSignatureError::Shared(TypedSignatureError::SharedTypeMismatch {
            input,
            output,
        }) => {
            return failure(
                report,
                "shared_port_type",
                output.annotation,
                &format!(
                    "同じ配線を使う入力の型は {}、出力が要求する型は {}。",
                    diagnostic::clocked::port_type(input.ty),
                    diagnostic::clocked::port_type(output.ty)
                ),
                [("入力の型", input.annotation)].into_iter().chain(caller),
            );
        }
        MixedTypedSignatureError::Shared(TypedSignatureError::TypeReference {
            reference, ..
        }) => {
            return failure(
                report,
                "function_signature",
                reference,
                "ポートの型名を解決できない。",
                caller,
            );
        }
    };
    match signature {
        RoutineSignatureError::Width(error) => {
            let reason = diagnostic::port_width_reason(error.kind);
            failure(
                report,
                "port_width",
                site(error.source, error.span),
                &reason,
                caller,
            )
        }
        RoutineSignatureError::Duplicate { first, later, .. } => failure(
            report,
            "duplicate_port",
            later,
            "このポート名が重複している。",
            [("最初の指定", first)].into_iter().chain(caller),
        ),
        RoutineSignatureError::SharedWidthMismatch {
            expected,
            actual,
            input,
            output,
        } => failure(
            report,
            "shared_port_width",
            output,
            &format!("出力は {expected} ビットを必要とするが、入力は {actual} ビット。"),
            [("入力の幅", input)].into_iter().chain(caller),
        ),
    }
}

fn circuit_reference<'p, 'a>(
    reference: LocatedName<'p, 'a>,
    reason: MixedFuncCalleeError<'p, 'a>,
    missing: &str,
    expected: &str,
    caller: Option<(&'static str, LocatedName<'p, 'a>)>,
    report: &mut Option<&mut Report<'_>>,
) -> String {
    let (code, message, related) = match reason {
        MixedFuncCalleeError::Missing => ("missing_reference", missing, None),
        MixedFuncCalleeError::MissingPrefix => (
            "missing_reference_prefix",
            "パスの先頭に対応する名前空間または別名が見つからない。",
            None,
        ),
        MixedFuncCalleeError::DeclarationPrefix(declaration) => (
            "declaration_prefix",
            "パスの先頭が宣言を指しているため、その内側をたどれない。",
            Some(("パスの先頭が指す宣言", declaration.name)),
        ),
        MixedFuncCalleeError::Private(location) => (
            "private_reference",
            "別の名前空間にある非公開の宣言は参照できない。",
            Some(("非公開の宣言", location)),
        ),
        MixedFuncCalleeError::Namespace(location) => (
            "namespace_reference",
            expected,
            Some(("名前空間の宣言", location)),
        ),
        MixedFuncCalleeError::WrongDeclaration(declaration) => (
            "wrong_declaration_kind",
            expected,
            Some(("名前が指す宣言", declaration.name)),
        ),
    };
    failure(
        report,
        code,
        reference,
        message,
        related.into_iter().chain(caller),
    )
}

pub(super) fn calls(
    project: &ParsedProjectSources<'_>,
    error: DefnCallGraphError<'_, '_>,
    report: &mut Option<&mut Report<'_>>,
) -> String {
    match error {
        DefnCallGraphError::Body { from, error } => match error {
            DefnBodyCallError::Expression(error) => {
                expression(&project.sources()[from.file], error, report)
            }
            DefnBodyCallError::Call(DefnCallError::Name { site: call, error }) => {
                let (code, reason, related) = match error {
                    DefnNameError::Caller => {
                        ("defn_caller", "呼び出し元が補助関数を指していない。", None)
                    }
                    DefnNameError::InvalidName => {
                        ("defn_call_name", "呼び出し先の名前が規則に合わない。", None)
                    }
                    DefnNameError::Missing => (
                        "missing_reference",
                        "呼び出し先の補助関数が見つからない。",
                        None,
                    ),
                    DefnNameError::MissingHead => (
                        "missing_reference_prefix",
                        "パスの先頭に対応する名前空間または別名が見つからない。",
                        None,
                    ),
                    DefnNameError::DeclarationHead(declaration) => (
                        "declaration_prefix",
                        "独立宣言の内側を名前空間として参照できない。",
                        Some(declaration.name),
                    ),
                    DefnNameError::Private(location) => (
                        "private_reference",
                        "別の名前空間にある非公開の補助関数は呼び出せない。",
                        Some(location),
                    ),
                    DefnNameError::WrongDeclaration(declaration) => (
                        "wrong_declaration_kind",
                        "呼び出し先に補助関数を指定する。",
                        Some(declaration.name),
                    ),
                    DefnNameError::Namespace(location) => (
                        "namespace_reference",
                        "呼び出し先に補助関数を指定する。",
                        Some(location),
                    ),
                };
                failure(
                    report,
                    code,
                    site(call.source, call.head),
                    reason,
                    related.map(|site| ("参照先", site)),
                )
            }
            DefnBodyCallError::Call(DefnCallError::Arity {
                site: call,
                declaration,
                expected,
                found,
            }) => failure(
                report,
                "argument_count",
                site(call.source, call.span),
                &format!("引数は {expected} 個必要だが、{found} 個ある。"),
                [("補助関数の宣言", declaration.name)],
            ),
            DefnBodyCallError::Call(DefnCallError::ArithmeticArity {
                site: call,
                operation,
                found,
            }) => failure(
                report,
                "argument_count",
                site(call.source, call.span),
                &format!(
                    "組み込みの `{}` には引数が 2 個必要だが、{found} 個ある。",
                    integer_operation_name(operation)
                ),
                [],
            ),
        },
        DefnCallGraphError::UnvisitedCall(edge) => failure(
            report,
            "unchecked_defn_call",
            site(edge.call.source, edge.call.span),
            "補助関数の呼び出しの型検査を完了できない。",
            [],
        ),
        DefnCallGraphError::UnvisitedArithmetic { site: call, .. } => failure(
            report,
            "unchecked_arithmetic",
            site(call.source, call.span),
            "整数演算の型検査を完了できない。",
            [],
        ),
        DefnCallGraphError::Cycle(edges) => {
            let mut sites = edges
                .into_iter()
                .map(|edge| site(edge.call.source, edge.call.span));
            let reason = "補助関数の呼び出しが循環している。";
            match sites.next() {
                Some(first) => failure(
                    report,
                    "defn_cycle",
                    first,
                    reason,
                    sites.map(|site| ("循環内の呼び出し", site)),
                ),
                None => reason.into(),
            }
        }
    }
}

fn header_ranges(error: &DefnHeaderError) -> (Range<usize>, Option<Range<usize>>) {
    match error {
        DefnHeaderError::Syntax(error) => (error.span.clone(), None),
        DefnHeaderError::ExpectedOneDeclaration(span) | DefnHeaderError::Expected { span, .. } => {
            (span.clone(), None)
        }
        DefnHeaderError::DuplicateParameter { duplicate, first } => {
            (duplicate.clone(), Some(first.clone()))
        }
        DefnHeaderError::ParameterCount {
            arguments,
            parameters,
        } => (parameters.clone(), Some(arguments.clone())),
    }
}

fn expression(
    source: &ProjectSource<'_>,
    error: ExpressionCheckFailure<'_>,
    report: &mut Option<&mut Report<'_>>,
) -> String {
    let (code, span, reason, related) = match error {
        ExpressionCheckFailure::Type(error) => {
            let (span, reason, related) = expression_type(error);
            ("ncg2m_expression", span, reason, related)
        }
        ExpressionCheckFailure::Deferred(error) => {
            let (span, reason) = match error {
                DeferredExpression::Form(span) => (span, "この式の検査には対応していない。"),
                DeferredExpression::Name(span) => {
                    (span, "この名前の検査には実行段階の確定が必要。")
                }
                DeferredExpression::BooleanBinding(span) => {
                    (span, "真偽値と同じ名前を使う束縛の検査には対応していない。")
                }
                DeferredExpression::StageType(span) => {
                    (span, "この型名を解決するには実行段階の確定が必要。")
                }
            };
            ("ncg2m_deferred_expression", span, reason.into(), None)
        }
    };
    failure(
        report,
        code,
        site(source, span),
        &reason,
        related.map(|(label, span)| (label, site(source, span))),
    )
}

type ExpressionDiagnostic = (Range<usize>, String, Option<(&'static str, Range<usize>)>);

fn expression_type(error: ExpressionTypeError<'_>) -> ExpressionDiagnostic {
    let (span, message) = match error {
        ExpressionTypeError::Arguments {
            span,
            expected_count,
        } => (span, format!("式には引数が {expected_count} 個必要。")),
        ExpressionTypeError::FunctionLiteral(span) => {
            (span, "関数リテラルは補助関数の宣言に書く。".into())
        }
        ExpressionTypeError::ListInScalarContext(span) => {
            (span, "この位置ではリスト以外の値が必要。".into())
        }
        ExpressionTypeError::ListNeedsAnnotation(span) => {
            (span, "空リストの要素型を型注釈で指定する。".into())
        }
        ExpressionTypeError::Let(error) => {
            return match error {
                LetSyntaxError::Arguments(span) => (
                    span,
                    "`let` に束縛列と本体式を一つずつ指定する。".into(),
                    None,
                ),
                LetSyntaxError::BindingList(span) => {
                    (span, "`let` の束縛列をリストで指定する。".into(), None)
                }
                LetSyntaxError::Binding(span) => (
                    span,
                    "束縛は名前と値の式を一つずつ持つリストで指定する。".into(),
                    None,
                ),
                LetSyntaxError::Name(span) => (span, "束縛名には無修飾名を指定する。".into(), None),
                LetSyntaxError::Duplicate { duplicate, first } => (
                    duplicate,
                    "同じ束縛名が複数ある。".into(),
                    Some(("最初の束縛", first)),
                ),
            };
        }
        ExpressionTypeError::Fold(error) => {
            return match error {
                FoldSyntaxError::Binders(span) => {
                    (span, "束縛名を二つ持つリストを指定する。".into(), None)
                }
                FoldSyntaxError::Name(span) => {
                    (span, "反復式の束縛名には無修飾名を指定する。".into(), None)
                }
                FoldSyntaxError::Duplicate { duplicate, first } => (
                    duplicate,
                    "同じ束縛名が複数ある。".into(),
                    Some(("先に書かれた束縛", first)),
                ),
            };
        }
        ExpressionTypeError::FoldListInput(span) => {
            (span, "`fold-list` の入力はリスト型で指定する。".into())
        }
        ExpressionTypeError::Annotation(error) => {
            let (span, related) = header_ranges(&error);
            return (
                span,
                "型注釈の形式が規則に合わない。".into(),
                related.map(|span| ("関連する指定", span)),
            );
        }
        ExpressionTypeError::ListDepthMismatch {
            span,
            expected_depth,
            actual_depth,
        } => (
            span,
            format!("リスト型の深さは {expected_depth} が必要だが、式の型は {actual_depth}。"),
        ),
        ExpressionTypeError::Mismatch {
            span,
            expected,
            actual,
        } => (
            span,
            format!(
                "式の型は {} が必要だが、{} になっている。",
                data_type(expected),
                data_type(actual)
            ),
        ),
        ExpressionTypeError::Integer { span, error } => (span, integer_literal_message(error)),
        ExpressionTypeError::ArithmeticResult(span) => (
            span,
            "整数演算の結果は Nat または Int でなければならない。".into(),
        ),
    };
    (span, message, None)
}

pub(super) fn integer_literal_message(error: IntegerLiteralError) -> String {
    match error {
        IntegerLiteralError::InvalidDigits => "十進整数を ASCII の数字で指定する。".into(),
        IntegerLiteralError::NegativeLiteralForNat => {
            "`Nat` の位置には負号付きの整数を書けない。".into()
        }
        IntegerLiteralError::OutOfRange(ty) => {
            format!("整数リテラルが `{}` の範囲を外れる。", integer_type(ty))
        }
    }
}

pub(super) fn integer_type(ty: IntegerType) -> &'static str {
    match ty {
        IntegerType::Nat => "Nat",
        IntegerType::Int => "Int",
    }
}

pub(super) fn integer_operation_name(operation: IntegerOperation) -> &'static str {
    match operation {
        IntegerOperation::Add => "add",
        IntegerOperation::Sub => "sub",
        IntegerOperation::Mul => "mul",
    }
}

pub(super) fn data_type(ty: DataType<'_>) -> String {
    let atom = match ty.atom {
        DataTypeAtom::Named(name) => name,
        DataTypeAtom::Builtin(BuiltinDataType::Bool) => "Bool",
        DataTypeAtom::Builtin(BuiltinDataType::Symbol) => "Symbol",
        DataTypeAtom::Builtin(BuiltinDataType::Syntax) => "Syntax",
        DataTypeAtom::Builtin(BuiltinDataType::Integer(integer)) => integer_type(integer),
    };
    format!(
        "{}{}{}",
        "(List ".repeat(ty.list_depth),
        atom,
        ")".repeat(ty.list_depth)
    )
}
