//! 順序回路のコンパイル診断を、元のソース範囲に結び付けて表示する。

use circuitgame2_core::{
    netlist::{ClockedStorageError, ClockedVmError, StateAllocationError},
    project::{
        BitSelectionError, CallInputReason, CircuitLiteralWidthError, CircuitProject,
        ClockedArgumentWiringError, ClockedDefinitionLayoutError, ClockedDriverBitsError,
        ClockedEndpointError, ClockedModuleOutputsError, ClockedModuleVmError,
        ClockedStorageLayoutError, ConcatResultWidthError, ConcatWidthError, ModuleAliasTypesError,
        ModuleBodyPosition, ModuleCallInputsError, ModuleCallPortsError, ModuleCallsError,
        ModuleConcatsError, ModuleDriverError, ModuleOutputTypesError, ModuleRootTypesError,
        ModuleSelectionsError, ModuleWiringError, OutputDriver, ResolvedPortType,
        SelectionArgumentError,
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
                "{} 定義の右辺には信号名、ビット選択、信号名やビットリテラルの連接、または基底 NOR・ラッチ・RAM の直接呼び出しを指定する。",
                located(&at),
            ),
            ClockedEndpointError::AliasBits { reference, reason } => {
                format!(
                    "{} 右辺の配線を確定できない。{}",
                    located(&reference),
                    driver_bits_error(reason)
                )
            }
            ClockedEndpointError::Driver(at) => {
                format!("{} 定義の駆動元が配線の範囲外にある。", located(&at))
            }
            ClockedEndpointError::AliasCycle(references) => {
                alias_cycle(&site, references.iter().map(located))
            }
            ClockedEndpointError::GateCapacity(at) => {
                format!("{} NOR ゲート数が上限を超える。", located(&at))
            }
            ClockedEndpointError::Width {
                definition,
                produced,
                allocated,
            } => format!(
                "{} 定義に必要な幅は {allocated} ビットだが、右辺の幅は {produced} ビットである。",
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
                    "{} 引数には信号名、ビットリテラル、または信号を選ぶ bit・slice を指定する。",
                    located(&at)
                )
            }
            ClockedArgumentWiringError::Selection(error) => module_expression(&site, error),
            ClockedArgumentWiringError::SelectionPort {
                argument,
                expected,
                required_at,
                width,
            } => {
                let mut message = format!(
                    "{} ビット選択の幅は {width} ビットだが、入力の要求型は {}。",
                    located(&argument),
                    port_type(expected),
                );
                if let Some(required_at) = required_at {
                    message.push_str(&format!("\n  入力の型 {}", located(&required_at)));
                }
                message
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
        ModuleCallsError::Module(ModuleOutputTypesError::Expressions(error)) => {
            module_expression(site, error)
        }
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
                    message.push_str(&format!("\n  {}", literal_width_error(error)));
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

fn module_expression(site: &str, error: ModuleSelectionsError<'_, '_>) -> String {
    match error {
        ModuleSelectionsError::Concat(error) => concat_error(site, error),
        ModuleSelectionsError::Types(ModuleAliasTypesError::Cycle(steps)) => {
            alias_cycle(site, steps.iter().map(|step| located(&step.reference)))
        }
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
    }
}

fn concat_error(site: &str, error: ModuleConcatsError<'_, '_>) -> String {
    match error {
        ModuleConcatsError::Types(error) => {
            module_expression(site, ModuleSelectionsError::Types(error))
        }
        ModuleConcatsError::Outputs {
            expression,
            left,
            targets,
        } => format!(
            "{} 連接の出力は一つだが、左辺に {targets} 個の名前がある。\n  定義の左辺 {}",
            located(&expression),
            located(&left),
        ),
        ModuleConcatsError::InputType { argument, actual } => format!(
            "{} 連接にはビット列が必要だが、引数の型は {}。\n  型が決まった箇所 {}",
            located(&argument),
            port_type(actual.ty),
            located(&actual.origin),
        ),
        ModuleConcatsError::Literal { literal, error } => {
            format!("{} {}", located(&literal), literal_width_error(error),)
        }
        ModuleConcatsError::Width { concat, reason } => {
            let detail = match reason {
                ConcatResultWidthError::Arguments(ConcatWidthError::Empty) => {
                    "連接には引数を一つ以上指定する。".to_owned()
                }
                ConcatResultWidthError::Arguments(ConcatWidthError::ZeroWidth { argument }) => {
                    format!(
                        "引数 {argument} の幅は 1 ビット以上にする。引数の添字は 0 から数える。"
                    )
                }
                ConcatResultWidthError::Arguments(ConcatWidthError::LimitExceeded {
                    argument,
                    width,
                    limit,
                }) => {
                    format!(
                        "引数 {argument} までの合計幅 {width} ビットが上限 {limit} ビットを超える。引数の添字は 0 から数える。"
                    )
                }
                ConcatResultWidthError::Mismatch { expected, actual } => {
                    format!(
                        "連接の幅は {actual} ビットだが、要求される幅は {expected} ビットである。"
                    )
                }
                ConcatResultWidthError::ConfiguredLimit { limit, actual } => {
                    format!("連接の幅 {actual} ビットが上限 {limit} ビットを超える。")
                }
            };
            format!("{} {detail}", located(&concat))
        }
    }
}

/// 連接と実引数で共通のリテラル幅診断を表示する。
fn literal_width_error(error: CircuitLiteralWidthError) -> String {
    match error {
        CircuitLiteralWidthError::InvalidLiteral => {
            "ビットリテラルには 0、1、二進表記、十六進表記を使う。".into()
        }
        CircuitLiteralWidthError::ZeroWidth => "ビットリテラルの幅は 1 ビット以上にする。".into(),
        CircuitLiteralWidthError::FixedBitWidth { expected } => {
            format!("0 と 1 の幅は 1 ビットである。ここで要求される幅は {expected} ビット。")
        }
        CircuitLiteralWidthError::DoesNotFit { expected, required } => {
            format!(
                "定数を表すのに {required} ビット必要で、要求幅 {expected} ビットに収まらない。"
            )
        }
        CircuitLiteralWidthError::LimitExceeded { width, limit } => {
            format!("ビットリテラルの幅 {width} ビットが上限 {limit} ビットを超える。")
        }
    }
}

/// Core が返した閉路順に、最初の参照位置と残りの関連位置を表示する。
fn alias_cycle(fallback: &str, mut locations: impl Iterator<Item = String>) -> String {
    let first = locations.next().unwrap_or_else(|| fallback.to_owned());
    let mut message = format!("{first} 配線参照だけで循環している。");
    for location in locations {
        message.push_str(&format!("\n  循環内の参照 {location}"));
    }
    message
}

fn driver_bits_error(error: ClockedDriverBitsError) -> String {
    match error {
        ClockedDriverBitsError::Driver(OutputDriver::Input(index)) => {
            format!("入力ポート {index} の配線が見つからない。")
        }
        ClockedDriverBitsError::Driver(OutputDriver::Definition(index)) => {
            format!("定義 {index} の配線が見つからない。")
        }
        ClockedDriverBitsError::Selection(BitSelectionError::ZeroCount) => {
            "選択するビット数は 1 以上を指定する。".into()
        }
        ClockedDriverBitsError::Selection(BitSelectionError::ZeroStride) => {
            "選択の刻みは 1 以上を指定する。".into()
        }
        ClockedDriverBitsError::Selection(BitSelectionError::Outside {
            start,
            last,
            input_width,
        }) => {
            format!(
                "末尾添字 {last} が入力幅 {input_width} ビットの範囲を外れる。開始添字は {start}。"
            )
        }
        ClockedDriverBitsError::Last { expected, actual } => {
            format!("末尾添字の計算結果は {expected} だが、保持された値は {actual} である。")
        }
        ClockedDriverBitsError::Capacity { count, limit } => {
            format!("必要な配線数 {count} が上限 {limit} を超える。")
        }
        ClockedDriverBitsError::Allocation => "配線に必要なメモリを確保できない。".into(),
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
            "{} 末尾添字 {last} が入力幅 {input_width} ビットの範囲を外れる。開始添字は {start}。\n  入力の幅 {}",
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
