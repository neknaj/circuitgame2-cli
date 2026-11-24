//! 順序回路のコンパイル診断を、元のソース範囲に結び付けて表示する。

use circuitgame2_core::{
    netlist::{ClockedStorageError, ClockedVmError, StateAllocationError},
    project::{
        CallInputReason, CircuitProject, ClockedArgumentWiringError, ClockedDefinitionLayoutError,
        ClockedEndpointError, ClockedModuleOutputsError, ClockedModuleVmError,
        ClockedStorageLayoutError, ModuleAliasTypesError, ModuleBodyPosition,
        ModuleCallInputsError, ModuleCallPortsError, ModuleCallsError, ModuleDriverError,
        ModuleOutputTypesError, ModuleRootTypesError, ModuleSelectionsError, ModuleWiringError,
        ResolvedPortType, SelectionArgumentError,
    },
};

use super::{located, position};

/// 変換段階と違反のある位置を表示する。Core が返す関連範囲も添える。
pub(crate) fn error(
    project: &CircuitProject<'_>,
    owner: ModuleBodyPosition,
    name: &str,
    diagnostic: ClockedModuleVmError<'_, '_>,
) -> String {
    let site = project
        .circuits()
        .get(owner.file)
        .and_then(|circuit| {
            let entry = circuit.syntax.entries.get(owner.entry)?;
            let named = circuit
                .names
                .iter()
                .find(|named| named.form == entry.form)?;
            let source = project.sources().get(owner.file)?;
            Some(position(
                &source.id.components().join("/"),
                &named.name.span,
            ))
        })
        .unwrap_or_else(|| name.to_owned());

    match diagnostic {
        ClockedModuleVmError::Endpoints(error) => match error {
            ClockedEndpointError::Calls(error) => calls(&site, error),
            ClockedEndpointError::Ports(error) => ports(&site, error),
            ClockedEndpointError::Storage(error) => match error {
                ClockedStorageLayoutError::ExternalCapacity => {
                    format!("{site} 外部入力のビット列が配線容量を超える。")
                }
                ClockedStorageLayoutError::Calls(error) => calls(&site, error),
                ClockedStorageLayoutError::Structure { call, .. } => {
                    format!("{} RAM のデータ型を配置できない。", located(&call))
                }
                ClockedStorageLayoutError::Capacity {
                    call,
                    annotation,
                    used,
                    width,
                } => format!(
                    "{} 状態素子の出力が入力配線の容量を超える。現在 {used} ビット、追加 {width} ビット。\n  出力の型 {}",
                    located(&call),
                    located(&annotation),
                ),
            },
            ClockedEndpointError::Allocation => format!("{site} 配線の配置表を確保できない。"),
            ClockedEndpointError::Layout => format!("{site} 定義のビット配置が一致しない。"),
            ClockedEndpointError::Unsupported(at) => format!(
                "{} 直接の NOR、ラッチ、RAM の呼び出しを定義の右辺に指定する。",
                located(&at),
            ),
            ClockedEndpointError::GateCapacity(at) => {
                format!("{} NOR ゲート数が上限を超える。", located(&at))
            }
            ClockedEndpointError::Width {
                definition,
                produced,
                allocated,
            } => format!(
                "{} 定義に必要な幅は {allocated} ビットだが、呼び出しは {produced} ビットを出力する。",
                located(&definition),
            ),
        },
        ClockedModuleVmError::Arguments(error) => match error {
            ClockedArgumentWiringError::Layout(error) => ports(&site, error),
            ClockedArgumentWiringError::Allocation => {
                format!("{site} 引数の配線表を確保できない。")
            }
            ClockedArgumentWiringError::Unsupported(at) => {
                format!(
                    "{} 引数には信号名かビットリテラルを指定する。",
                    located(&at)
                )
            }
            ClockedArgumentWiringError::Wiring { argument, reason } => {
                format!(
                    "{} 引数のビット配線を確定できない ({reason:?})。",
                    located(&argument)
                )
            }
        },
        ClockedModuleVmError::Allocation => format!("{site} ネットリストの配線表を確保できない。"),
        ClockedModuleVmError::CallWiring(at) => {
            format!("{} 呼び出しの入出力を配線できない。", located(&at))
        }
        ClockedModuleVmError::OutputWiring {
            port,
            bit,
            site: at,
        } => format!(
            "{} 出力ポート {port} のビット {bit} の駆動元を確定できない。",
            at.as_ref().map_or(site, located),
        ),
        ClockedModuleVmError::InstanceCount => {
            format!("{site} ゲートと状態素子の配置が一致しない。")
        }
        ClockedModuleVmError::Netlist(error) => {
            format!("{site} ネットリストに無効な参照がある ({error:?})。")
        }
        ClockedModuleVmError::Vm(error, at) => {
            let site = at.as_ref().map_or(site, located);
            match error {
                ClockedVmError::InputLayout => format!("{site} VM の入力の配置が一致しない。"),
                ClockedVmError::Storage { instance, error } => {
                    let reason = match error {
                        ClockedStorageError::State(StateAllocationError::Capacity) => {
                            "記憶領域が容量上限を超える。".to_owned()
                        }
                        ClockedStorageError::State(StateAllocationError::Allocation) => {
                            "記憶領域を確保できない。".to_owned()
                        }
                        ClockedStorageError::State(StateAllocationError::ZeroWidth) => {
                            "データ幅に 1 ビット以上を指定する。".to_owned()
                        }
                        _ => format!("入力に無効な配線がある ({error:?})。"),
                    };
                    format!("{site} 状態素子 {instance} の{reason}")
                }
            }
        }
    }
}

fn calls(site: &str, error: ModuleCallsError<'_, '_>) -> String {
    match error {
        ModuleCallsError::Module(ModuleOutputTypesError::Mismatch {
            expected, actual, ..
        }) => format!(
            "{} 宣言型は {} だが、駆動元の型は {}。\n  駆動元の型 {}",
            located(&expected.origin),
            port_type(expected.ty),
            port_type(actual.ty),
            located(&actual.origin),
        ),
        ModuleCallsError::Module(ModuleOutputTypesError::Expressions(error)) => match error {
            ModuleSelectionsError::Argument(error)
            | ModuleSelectionsError::Types(ModuleAliasTypesError::Sources(
                ModuleRootTypesError::SelectionArgument(error),
            )) => selection(error),
            ModuleSelectionsError::Width {
                expression,
                width,
                limit,
            } => format!(
                "{} ビット選択の結果幅 {width} が上限 {limit} を超える。",
                located(&expression),
            ),
            ModuleSelectionsError::InputType { input, actual } => format!(
                "{} ビット選択にはビット列が必要だが、型は {}。\n  入力の型 {}",
                located(&input),
                port_type(actual.ty),
                located(&actual.origin),
            ),
            ModuleSelectionsError::Types(ModuleAliasTypesError::Sources(
                ModuleRootTypesError::Wiring(ModuleWiringError::Undefined(at)),
            )) => {
                format!("{} 参照した信号が見つからない。", located(&at))
            }
            ModuleSelectionsError::Types(ModuleAliasTypesError::Sources(
                ModuleRootTypesError::Wiring(ModuleWiringError::Drivers(
                    ModuleDriverError::NeedsExpansion(at),
                )),
            )) => {
                format!("{} マクロ展開が必要なため実行できない。", located(&at))
            }
            ModuleSelectionsError::Types(ModuleAliasTypesError::Sources(
                ModuleRootTypesError::Call(error),
            )) => call_ports(site, error),
            _ => format!("{site} モジュールの型または結線に違反がある。"),
        },
        ModuleCallsError::Call {
            reason:
                ModuleCallInputsError::Argument {
                    argument,
                    expected,
                    required_at,
                    reason,
                    ..
                },
            ..
        } => {
            let mut message = format!(
                "{} 引数の要求型は {}。",
                located(&argument),
                port_type(expected)
            );
            if let Some(required_at) = required_at {
                message.push_str(&format!("\n  入力の型 {}", located(&required_at)));
            }
            match reason {
                CallInputReason::LiteralWidth(error) => {
                    message.push_str(&format!("\n  定数の幅に違反がある ({error:?})。"));
                }
                CallInputReason::StructRequired => message.push_str("\n  構造体の値を指定する。"),
                CallInputReason::SignalType(actual) => message.push_str(&format!(
                    "\n  実際の型は {}。{}",
                    port_type(actual.ty),
                    located(&actual.origin),
                )),
            }
            message
        }
        ModuleCallsError::Call {
            reason: ModuleCallInputsError::Ports(error),
            ..
        } => call_ports(site, error),
        ModuleCallsError::Call {
            reason:
                ModuleCallInputsError::TypeCount {
                    expected_count,
                    supplied,
                },
            ..
        } => {
            format!("{site} 引数の型情報は {expected_count} 件必要だが、{supplied} 件ある。")
        }
    }
}

fn selection(error: SelectionArgumentError<'_, '_>) -> String {
    match error {
        SelectionArgumentError::NaturalOverflow { site, argument } => format!(
            "{} ビット選択の数値引数が 64 ビットの自然数の範囲を超える ({argument:?})。",
            located(&site),
        ),
        SelectionArgumentError::ZeroCount { site } => {
            format!("{} 選択するビット数は 1 以上を指定する。", located(&site))
        }
        SelectionArgumentError::ZeroStride { site } => {
            format!("{} 選択の刻みは 1 以上を指定する。", located(&site))
        }
        SelectionArgumentError::Outside {
            site,
            width_origin,
            start,
            last,
            input_width,
        } => format!(
            "{} 選択した添字 {start}..{last} が入力幅 {input_width} ビットの範囲を外れる。\n  入力の幅 {}",
            located(&site),
            located(&width_origin),
        ),
    }
}

fn call_ports(site: &str, error: ModuleCallPortsError<'_, '_>) -> String {
    let (mut message, declaration) = match error {
        ModuleCallPortsError::InputCount {
            call,
            declaration,
            expected,
            actual,
        } => (
            format!(
                "{} 実引数は {expected} 個必要だが、{actual} 個指定された。",
                located(&call)
            ),
            declaration,
        ),
        ModuleCallPortsError::OutputCount {
            call,
            left,
            declaration,
            expected,
            actual,
        } => {
            let mut message = format!(
                "{} 定義の左辺は {expected} 出力を要求するが、呼び出しは {actual} 出力を返す。",
                located(&call),
            );
            if let Some(left) = left {
                message.push_str(&format!("\n  定義の左辺 {}", located(&left)));
            }
            (message, declaration)
        }
        ModuleCallPortsError::Signature(_) => {
            return format!("{site} 呼び出し先の署名を検査できない。");
        }
    };
    if let Some(declaration) = declaration {
        message.push_str(&format!("\n  呼び出し先の宣言 {}", located(&declaration)));
    }
    message
}

fn port_type(ty: ResolvedPortType<'_, '_>) -> String {
    match ty {
        ResolvedPortType::Bits(width) => format!("{width} ビットのビット列"),
        ResolvedPortType::Struct { form, .. } => match &form.namespace {
            Some(parent) => format!("::{}::{}", parent.word, form.name.word),
            None => format!("::{}", form.name.word),
        },
    }
}

fn ports(site: &str, error: ClockedModuleOutputsError<'_, '_>) -> String {
    match error {
        ClockedModuleOutputsError::Calls(error)
        | ClockedModuleOutputsError::Definitions(ClockedDefinitionLayoutError::Calls(error)) => {
            calls(site, error)
        }
        ClockedModuleOutputsError::Structure(_) => format!("{site} ポートの構造体を配置できない。"),
        ClockedModuleOutputsError::Port {
            direction,
            site,
            layout,
        } => format!(
            "{} ポートのビット範囲を割り当てられない ({direction:?}, {layout:?})。",
            located(&site),
        ),
        ClockedModuleOutputsError::Definitions(ClockedDefinitionLayoutError::Capacity {
            definition,
            layout,
        }) => {
            format!(
                "{} 定義のビット範囲を割り当てられない ({layout:?})。",
                located(&definition)
            )
        }
        ClockedModuleOutputsError::Definitions(
            ClockedDefinitionLayoutError::Unresolved { definition, .. }
            | ClockedDefinitionLayoutError::Structure { definition, .. },
        ) => {
            format!("{} 定義の型を配置できない。", located(&definition))
        }
        ClockedModuleOutputsError::Allocation => format!("{site} 出力の配線表を確保できない。"),
        ClockedModuleOutputsError::Wiring { site, reason } => {
            format!(
                "{} 出力の駆動元を配線できない ({reason:?})。",
                located(&site)
            )
        }
    }
}
