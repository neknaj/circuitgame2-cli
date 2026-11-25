//! 実行設定の外部デバイスを回路の署名に照合する。

use std::{collections::BTreeMap, ops::Range, path::Path};

use circuitgame2_core::{
    project::{
        DeviceBindingsError, DevicePortBindingError, DevicePortRequest, DevicePortType,
        LocatedName, ModuleBodyPosition, PortDirection, ResolvedPortType, RoutineSignatureError,
        TypedSignatureError, UseAliases,
    },
    time::TickPeriod,
};

use super::{clocked, config, diagnostic};

pub(super) fn check(
    path: &Path,
    settings: &config::ProjectConfig,
    aliases: &UseAliases<'_, '_, '_>,
    root: ModuleBodyPosition,
) -> Result<(), String> {
    let mut names = BTreeMap::new();
    let mut requests = Vec::new();
    let mut spans = Vec::new();

    for entry in &settings.device {
        let device = entry.get_ref();
        if let Some(first) = names.insert(device.name.get_ref(), device.name.span()) {
            let repeated = config::at(path, Some(device.name.span()), "デバイス名が重複している。");
            return Err(with_related(
                repeated,
                config::at(path, Some(first), "最初の指定。"),
            ));
        }
        if device.kind.get_ref() != "uart-terminal" {
            return Err(config::at(
                path,
                Some(device.kind.span()),
                "未登録のデバイス種別。",
            ));
        }
        check_uart_options(path, settings, device, entry.span())?;

        let bindings = device.bindings.get_ref();
        // TOML 上の順序を保ち、Core が返す添字を元の指定位置に対応させる。
        let mut ordered: Vec<_> = bindings.iter().collect();
        ordered.sort_unstable_by_key(|(_, value)| value.span().start);
        for (port, circuit_port) in ordered {
            let direction = match port.as_str() {
                "tx" => PortDirection::Input,
                "rx" => PortDirection::Output,
                _ => {
                    return Err(config::at(
                        path,
                        Some(circuit_port.span()),
                        &format!("UART に {port:?} ポートはない。"),
                    ));
                }
            };
            requests.push(DevicePortRequest {
                direction,
                circuit_port: circuit_port.get_ref(),
                expected: DevicePortType::Bits(1),
            });
            spans.push(circuit_port.span());
        }
        for required in ["tx", "rx"] {
            if !bindings.contains_key(required) {
                return Err(config::at(
                    path,
                    Some(device.bindings.span()),
                    &format!("UART の {required} の接続先を指定する。"),
                ));
            }
        }
    }

    aliases
        .resolve_device_port_bindings(root, clocked::LIMITS.width, &requests)
        .map(|_| ())
        .map_err(|error| match error {
            DeviceBindingsError::Binding { request, error } => {
                binding_diagnostic(path, spans[request].clone(), error)
            }
            DeviceBindingsError::DuplicateDriver { first, later } => with_related(
                config::at(
                    path,
                    Some(spans[later].clone()),
                    "同じ回路入力を複数の接続が駆動している。",
                ),
                config::at(path, Some(spans[first].clone()), "先行する接続。"),
            ),
        })
}

fn check_uart_options(
    path: &Path,
    settings: &config::ProjectConfig,
    device: &config::Device,
    device_span: Range<usize>,
) -> Result<(), String> {
    let Some(tick) = &settings.tick_ns else {
        return Err(config::at(
            path,
            Some(device_span.clone()),
            "UART 端末には tick-ns の指定が必要。",
        ));
    };
    let ratio = tick.get_ref();
    let period = TickPeriod::new(ratio.numerator.get(), ratio.denominator.get())
        .ok_or_else(|| config::at(path, Some(tick.span()), "tick-ns の分子と分母が範囲外。"))?;

    let options = device.options.as_ref();
    if let Some((name, value)) = options
        .into_iter()
        .flat_map(|option| option.get_ref())
        .filter(|(name, _)| name.as_str() != "baud")
        .min_by_key(|(_, value)| value.span().start)
    {
        return Err(config::at(
            path,
            Some(value.span()),
            &format!("UART に {name:?} オプションはない。"),
        ));
    }
    let baud = options
        .and_then(|options| options.get_ref().get("baud"))
        .ok_or_else(|| {
            let span = options.map_or(device_span, |options| options.span());
            config::at(path, Some(span), "UART の options.baud を指定する。")
        })?;
    let rate = baud
        .get_ref()
        .as_integer()
        .and_then(|value| u64::try_from(value).ok())
        .filter(|&value| value != 0)
        .ok_or_else(|| {
            config::at(
                path,
                Some(baud.span()),
                "UART の baud には正の整数を指定する。",
            )
        })?;

    // 隣り合うビット境界を区別するには、tick 周期が1ビット時間以下であることが必要。
    if u128::from(ratio.numerator.get()) * u128::from(rate)
        > 1_000_000_000u128 * u128::from(ratio.denominator.get())
    {
        return Err(with_related(
            config::at(
                path,
                Some(tick.span()),
                "tick 周期が長すぎて UART のビット境界を区別できない。",
            ),
            config::at(path, Some(baud.span()), "ビット時間を指定した baud。"),
        ));
    }

    // 8N1 の開始、データ、停止を合わせた10ビットの境界が表現できることを確認する。
    period.tick_at_or_after(10_000_000_000, rate).map_err(|_| {
        config::at(
            path,
            Some(baud.span()),
            "UART のフレーム境界が tick の範囲に収まらない。",
        )
    })?;
    Ok(())
}

fn binding_diagnostic(
    path: &Path,
    span: Range<usize>,
    error: DevicePortBindingError<'_, '_, '_>,
) -> String {
    let at_binding = |reason: &str| config::at(path, Some(span.clone()), reason);
    match error {
        DevicePortBindingError::NoModule => at_binding("ルートモジュールを特定できない。"),
        DevicePortBindingError::UnknownName => at_binding("接続先の回路ポートが存在しない。"),
        DevicePortBindingError::Signature(error) => with_related(
            at_binding("ルートのポート署名に違反がある。"),
            signature_error(error),
        ),
        DevicePortBindingError::OppositeDirection(port) => with_related(
            at_binding("接続先の回路ポートの方向が合わない。"),
            diagnostic::located(&port.name),
        ),
        DevicePortBindingError::TypeMismatch { actual, .. } => {
            let ty = match actual.ty {
                ResolvedPortType::Bits(width) => format!("{width} ビット"),
                ResolvedPortType::Struct { form, .. } => format!("構造体 {}", form.name.word),
            };
            with_related(
                at_binding(&format!(
                    "UART は1ビットのポートを必要とする。接続先の型は {ty}。"
                )),
                diagnostic::located(&actual.name),
            )
        }
    }
}

fn signature_error(error: TypedSignatureError<'_, '_>) -> String {
    match error {
        TypedSignatureError::Signature(RoutineSignatureError::Width(error)) => {
            let reason = diagnostic::port_width_reason(error.kind);
            let location = LocatedName {
                source: error.source,
                span: error.span,
            };
            format!("{} {reason}", diagnostic::located(&location))
        }
        TypedSignatureError::Signature(RoutineSignatureError::Duplicate {
            first, later, ..
        }) => with_related(
            format!(
                "{} このポート名が重複している。",
                diagnostic::located(&later)
            ),
            diagnostic::located(&first),
        ),
        TypedSignatureError::Signature(RoutineSignatureError::SharedWidthMismatch {
            expected,
            actual,
            output,
            input,
        }) => with_related(
            format!(
                "{} 出力は {expected} ビットを必要とするが、入力は {actual} ビット。",
                diagnostic::located(&output)
            ),
            diagnostic::located(&input),
        ),
        TypedSignatureError::TypeReference {
            reference, reason, ..
        } => {
            let name = reference
                .source
                .text()
                .get(reference.span.clone())
                .unwrap_or("指定した型");
            diagnostic::reference_reason(&diagnostic::located(&reference), name, reason)
        }
        TypedSignatureError::SharedTypeMismatch { input, output } => with_related(
            format!(
                "{} 同じ配線を使う入力と出力の型が合わない。",
                diagnostic::located(&output.annotation)
            ),
            diagnostic::located(&input.annotation),
        ),
    }
}

fn with_related(message: String, related: String) -> String {
    format!("{message}\n関連箇所 {related}")
}
