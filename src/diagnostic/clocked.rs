//! 順序回路のコンパイル診断を、元のソース範囲に結び付けて表示する。

use circuitgame2_core::{
    netlist::{
        ClockedInstanceError, ClockedNorError, ClockedStorageError, ClockedVmError, GateInput,
        StateAllocationError,
    },
    project::{
        BitSelectionError, CallInputReason, CircuitLiteralWidthError, CircuitProject,
        ClockedArgumentWiringError, ClockedDefinitionLayoutError, ClockedDriverBitsError,
        ClockedEndpointError, ClockedModuleError, ClockedModuleOutputsError, ClockedModuleVmError,
        ClockedStorageLayoutError, ConcatResultWidthError, ConcatWidthError, LocatedName,
        ModuleAliasTypesError, ModuleBodyPosition, ModuleCallInputsError, ModuleCallPortsError,
        ModuleCallsError, ModuleConcatsError, ModuleDriverError, ModuleOutputTypesError,
        ModuleRootTypesError, ModuleSelectionsError, ModuleWiringError, OutputDriver,
        PrimitiveModuleError, ResolvedPortType, SelectionArgumentError, SignalType,
        StructureLayoutError, StructureNumber, StructureNumberErrorKind, StructurePlacementError,
        StructureResolutionError, StructureTypeError, VramOutputError,
    },
};

use super::{expansion_error, located, position, reference_reason};

/// 設定の読み出し信号に対応する違反と、回路側の関連範囲を表示する。
pub(crate) fn vram_output_error(error: VramOutputError<'_, '_>) -> String {
    match error {
        VramOutputError::Module(error) => calls("VRAM の対象モジュール", error),
        VramOutputError::MissingDefinition => {
            "read-output が指定する信号の定義が見つからない。".into()
        }
        VramOutputError::NotDirectMemory(site) => format!(
            "read-output にはルート本体で RAM を直接呼んだ定義の出力名を指定する。\n  回路側の定義 {}",
            located(&site),
        ),
    }
}

/// 変換段階と違反のある位置を表示する。Core が返す関連範囲も添える。
pub(crate) fn error(
    project: &CircuitProject<'_>,
    owner: ModuleBodyPosition,
    name: &str,
    diagnostic: ClockedModuleVmError<'_, '_>,
) -> String {
    let site = module_site(project, owner).unwrap_or_else(|| name.to_owned());

    match diagnostic {
        ClockedModuleVmError::Hierarchy(error) => hierarchy_error(project, &site, error),
        ClockedModuleVmError::InModule {
            owner: module,
            reason,
        } => {
            let mut message = error(project, module, name, *reason);
            if module != owner
                && let Some(module_site) = module_site(project, module)
            {
                message.push_str(&format!("\n  展開中のモジュール {module_site}"));
            }
            message
        }
        ClockedModuleVmError::ChildStateCapacity {
            call,
            used,
            width,
            capacity,
        } => format!(
            "{} 子モジュールの状態出力を加えると、親の入力配線の容量を超える。使用済み {used} ビット、追加 {width} ビット、上限 {capacity} ビット。",
            located(&call),
        ),
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
                "{} 定義の右辺には信号名、ビット選択、信号名やビットリテラルの連接、または基底 NOR・ラッチ・RAM・func・子 module の直接呼び出しを指定する。",
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

fn module_site(project: &CircuitProject<'_>, owner: ModuleBodyPosition) -> Option<String> {
    let circuit = project.circuits().get(owner.file)?;
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
}

fn hierarchy_error(
    project: &CircuitProject<'_>,
    site: &str,
    error: ClockedModuleError<'_, '_>,
) -> String {
    match error {
        ClockedModuleError::Parent(error) => calls(site, error),
        ClockedModuleError::Child { call, reason } => child_error(site, &call, reason),
        ClockedModuleError::Function { call, expansion } => format!(
            "{}\n  関数の呼び出し元 {}",
            expansion_error(project, expansion),
            located(&call),
        ),
        ClockedModuleError::StructureLayout(error) => structure_layout_error(site, error),
        ClockedModuleError::PortLayout {
            direction,
            site: port,
            layout,
        } => ports(
            site,
            ClockedModuleOutputsError::Port {
                direction,
                site: port,
                layout,
            },
        ),
        ClockedModuleError::Unsupported(at) => {
            format!("{} この式や型は現在の階層変換で扱えない。", located(&at))
        }
        ClockedModuleError::Capacity(at) => {
            format!("{} NOR ゲート数が上限を超える。", located(&at))
        }
        ClockedModuleError::AliasCycle(references) => {
            alias_cycle(site, references.iter().map(located))
        }
        ClockedModuleError::Recursion(references) => diagnostic_path(
            site,
            "モジュールの呼び出しが循環している。",
            "循環内の呼び出し",
            references.iter().map(located),
        ),
        ClockedModuleError::Instance(at, error) => {
            let detail = match error {
                ClockedInstanceError::InputCount => {
                    "実引数のビット数が子モジュールの入力幅と一致しない。".to_owned()
                }
                ClockedInstanceError::GateCapacity => "NOR ゲート数が上限を超える。".to_owned(),
                ClockedInstanceError::Argument(index) => {
                    format!("実引数のビット添字 {index} が親の配線範囲外を指している。")
                }
            };
            format!("{} {detail}", located(&at))
        }
        ClockedModuleError::Netlist(error) => match error {
            ClockedNorError::GateInput { gate, side } => {
                let side = match side {
                    GateInput::Left => "左",
                    GateInput::Right => "右",
                };
                format!("{site} ゲート {gate} の{side}入力が配線の範囲外を指している。")
            }
            ClockedNorError::Output { output } => {
                format!("{site} 出力のビット添字 {output} が配線の範囲外を指している。")
            }
        },
    }
}

fn child_error(
    site: &str,
    call: &LocatedName<'_, '_>,
    error: PrimitiveModuleError<'_, '_>,
) -> String {
    let call = located(call);
    let message = match error {
        PrimitiveModuleError::Module(error) => calls(site, error),
        PrimitiveModuleError::Port(at) => {
            format!(
                "{} このポートの型は子モジュールの変換で扱えない。",
                located(&at)
            )
        }
        PrimitiveModuleError::Definition(at) => {
            format!("{} この右辺は子モジュールの変換で扱えない。", located(&at))
        }
        PrimitiveModuleError::Argument(at) => {
            format!("{} この引数は子モジュールの変換で扱えない。", located(&at))
        }
    };
    format!("{message}\n  子モジュールの呼び出し元 {call}")
}

fn structure_layout_error(site: &str, error: StructureTypeError<'_, '_>) -> String {
    match error {
        StructureTypeError::Layout(StructureLayoutError::Resolution(error)) => match error.error {
            StructureResolutionError::Number(error) => {
                let number = match error.number {
                    StructureNumber::TotalWidth => "構造体の幅",
                    StructureNumber::FieldWidth => "フィールドの幅",
                    StructureNumber::FieldOffset => "フィールドのオフセット",
                };
                let detail = match error.kind {
                    StructureNumberErrorKind::NumericRange => {
                        format!("{number}が 64 ビット自然数の範囲を超える。")
                    }
                    StructureNumberErrorKind::ZeroWidth => {
                        format!("{number}には 1 ビット以上を指定する。")
                    }
                    StructureNumberErrorKind::WidthLimit { width, limit } => {
                        format!("{number} {width} ビットが上限 {limit} ビットを超える。")
                    }
                };
                let site = position(&error.source.id.components().join("/"), &error.span);
                format!("{site} {detail}")
            }
            StructureResolutionError::TypeReference { reference, reason } => {
                let name = reference
                    .source
                    .text
                    .get(reference.span.clone())
                    .unwrap_or("型名");
                reference_reason(&located(&reference), name, reason)
            }
        },
        StructureTypeError::Layout(StructureLayoutError::Placement { error, .. }) => match error {
            StructurePlacementError::DuplicateName { later, prior } => format!(
                "{} フィールド名が重複している。\n  先の宣言 {}",
                located(&later),
                located(&prior),
            ),
            StructurePlacementError::Outside {
                offset,
                start,
                field_width,
                structure_width,
            } => format!(
                "{} 開始ビット {start}、幅 {field_width} ビットのフィールドが構造体の幅 {structure_width} ビットに収まらない。",
                located(&offset),
            ),
            StructurePlacementError::Overlap {
                later,
                prior,
                later_range,
                prior_range,
            } => format!(
                "{} フィールドのビット範囲 [{}..{}] が先のフィールド [{}..{}] と重なる。\n  先の宣言 {}",
                located(&later),
                later_range.start,
                later_range.end,
                prior_range.start,
                prior_range.end,
                located(&prior),
            ),
        },
        StructureTypeError::Cycle { references } => diagnostic_path(
            site,
            "構造体のフィールド型が循環している。",
            "循環内の型参照",
            references
                .iter()
                .map(|reference| located(&reference.location)),
        ),
    }
}

pub(crate) fn calls(site: &str, error: ModuleCallsError<'_, '_>) -> String {
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
        ModuleSelectionsError::InputType { input, actual } => selection_input_type(&input, &actual),
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
        ModuleConcatsError::Selection(error) => selection(error),
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
pub(crate) fn literal_width_error(error: CircuitLiteralWidthError) -> String {
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
fn alias_cycle(fallback: &str, locations: impl Iterator<Item = String>) -> String {
    diagnostic_path(
        fallback,
        "配線参照だけで循環している。",
        "循環内の参照",
        locations,
    )
}

fn diagnostic_path(
    fallback: &str,
    heading: &str,
    label: &str,
    mut locations: impl Iterator<Item = String>,
) -> String {
    let first = locations.next().unwrap_or_else(|| fallback.to_owned());
    let mut message = format!("{first} {heading}");
    for location in locations {
        message.push_str(&format!("\n  {label} {location}"));
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

pub(crate) fn selection_input_type(
    input: &LocatedName<'_, '_>,
    actual: &SignalType<'_, '_>,
) -> String {
    format!(
        "{} ビット選択にはビット列が必要だが、型は {}。\n  入力の型 {}",
        located(input),
        port_type(actual.ty),
        located(&actual.origin),
    )
}

pub(crate) fn selection(error: SelectionArgumentError<'_, '_>) -> String {
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

pub(crate) fn port_type(ty: ResolvedPortType<'_, '_>) -> String {
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
