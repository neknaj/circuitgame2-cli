//! 回路ポートとボード端子を照合し、RP ファームウェアへ渡す配線表を出力する。

use std::{fs, io::Write, ops::Range, path::Path};

use circuitgame2_cli::config::{self, ProjectConfig};
use circuitgame2_core::time::exact_microseconds;
use circuitgame2_core::{
    backend::rp::{
        AssignmentError, Board, GpioBinding, GpioCircuitImage, GpioImageError, GpioOutputPin,
        PinSite, RpInputSource, UartCircuitImage, UartImageError, UartInputSignal, UartPortNames,
        UartSetup,
    },
    netlist::PackedNorLayout32,
    project::{
        CircuitProject, ClockedModuleVmError, DeviceBindingsError, DevicePortBindingError,
        ModuleBodyPosition, UseAliases,
    },
};
use serde::Deserialize;
use toml::Spanned;

mod i2c;

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum BoardKind {
    Pico,
    Pico2,
}

impl BoardKind {
    fn native(self) -> Board {
        match self {
            Self::Pico => Board::Pico,
            Self::Pico2 => Board::Pico2,
        }
    }

    fn wire_name(self) -> &'static str {
        match self {
            Self::Pico => "pico",
            Self::Pico2 => "pico2",
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "direction", rename_all = "lowercase", deny_unknown_fields)]
enum Gpio {
    Input {
        port: String,
        pin: u8,
    },
    Output {
        port: String,
        pin: u8,
        #[serde(default)]
        initial_high: bool,
    },
}

impl Gpio {
    fn binding(&self) -> GpioBinding<'_> {
        match self {
            Self::Input { port, pin } => GpioBinding::Input { port, pin: *pin },
            Self::Output {
                port,
                pin,
                initial_high,
            } => GpioBinding::Output {
                port,
                pin: *pin,
                initial_high: *initial_high,
            },
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct Uart {
    instance: u8,
    tx_pin: u8,
    rx_pin: u8,
    baud: u32,
    rx_data: [String; 8],
    rx_valid: String,
    rx_framing: String,
    rx_overrun: String,
    rx_ready: String,
    tx_data: [String; 8],
    tx_valid: String,
    tx_ready: String,
    tx_done: String,
    tx_ok: String,
    tx_done_ready: String,
}

impl Uart {
    fn setup(&self) -> UartSetup {
        UartSetup {
            instance: self.instance,
            tx: self.tx_pin,
            rx: self.rx_pin,
            baud: self.baud,
        }
    }

    fn names(&self) -> UartPortNames<'_> {
        UartPortNames {
            rx_data: self.rx_data.each_ref().map(String::as_str),
            rx_valid: &self.rx_valid,
            rx_framing: &self.rx_framing,
            rx_overrun: &self.rx_overrun,
            rx_ready: &self.rx_ready,
            tx_data: self.tx_data.each_ref().map(String::as_str),
            tx_valid: &self.tx_valid,
            tx_ready: &self.tx_ready,
            tx_done: &self.tx_done,
            tx_ok: &self.tx_ok,
            tx_done_ready: &self.tx_done_ready,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wiring {
    board: BoardKind,
    #[serde(default)]
    gpio: Vec<Spanned<Gpio>>,
    uart: Option<Spanned<Uart>>,
    i2c: Option<Spanned<i2c::I2c>>,
}

fn read_board(path: &Path) -> Result<Wiring, String> {
    let bytes = fs::read(path)
        .map_err(|error| config::at(path, None, &format!("ボード設定を読み込めない。{error}")))?;
    let text = std::str::from_utf8(&bytes).map_err(|error| {
        let from = error.valid_up_to();
        let to = error.error_len().map_or(bytes.len(), |len| from + len);
        config::at(
            path,
            Some(from..to),
            "ボード設定に不正な UTF-8 が含まれる。",
        )
    })?;
    toml::from_str(text).map_err(|error: toml::de::Error| {
        config::at(
            path,
            error.span(),
            &format!("ボード設定を解釈できない。{}", error.message()),
        )
    })
}

fn period_micros(project: &Path, settings: &ProjectConfig) -> Result<u64, String> {
    let period = settings
        .tick_ns
        .as_ref()
        .ok_or_else(|| config::at(project, None, "RP で実行するには tick-ns を指定する。"))?;
    let value = period.get_ref();
    exact_microseconds(value.numerator.get(), value.denominator.get()).ok_or_else(|| {
        config::at(
            project,
            Some(period.span()),
            "RP の tick 周期には正の整数マイクロ秒で表せる値が必要。",
        )
    })
}

fn span(wiring: &Wiring, at: usize) -> Option<Range<usize>> {
    wiring
        .gpio
        .get(at)
        .map(Spanned::span)
        .or_else(|| wiring.uart.as_ref().map(Spanned::span))
}

fn image_error(
    error: GpioImageError<'_, '_, '_>,
    board: &Path,
    wiring: &Wiring,
    source: &Source<'_, '_>,
    i2c: Option<&i2c::Locations<'_>>,
) -> String {
    let request_span = |index| match i2c {
        Some(locations) if index >= wiring.gpio.len() => {
            locations.request(index - wiring.gpio.len())
        }
        _ => span(wiring, index),
    };
    let pin_span = |site: PinSite| match i2c {
        Some(locations) if site.assignment == wiring.gpio.len() => locations.pin(site.signal),
        _ => span(wiring, site.assignment),
    };
    let at = |index, reason: &str| config::at(board, request_span(index), reason);
    let related = |index| at(index, "先に指定した接続。");
    match error {
        GpioImageError::Pin(AssignmentError::DuplicatePin { first, site, pin }) => format!(
            "{}\n{}",
            config::at(
                board,
                pin_span(site),
                &format!("GP{pin} を複数の接続で使用している。")
            ),
            config::at(board, pin_span(first), "先に指定した接続。")
        ),
        GpioImageError::Pin(AssignmentError::UnexposedPin { site, pin }) => config::at(
            board,
            pin_span(site),
            &format!("GP{pin} はボードの外部端子に出ていない。"),
        ),
        GpioImageError::Pin(error) => match i2c {
            Some(locations) => i2c::assignment_error(error, board, locations),
            None => config::at(
                board,
                wiring.uart.as_ref().map(Spanned::span),
                "ボードのピンまたは周辺機能の割り当てに違反している。",
            ),
        },
        GpioImageError::Binding(DeviceBindingsError::DuplicateDriver { first, later }) => format!(
            "{}\n{}",
            at(later, "同じ回路入力を複数の接続が駆動している。"),
            related(first)
        ),
        GpioImageError::Binding(DeviceBindingsError::Binding { request, error }) => match error {
            DevicePortBindingError::UnknownName => at(request, "指定の回路ポートが見つからない。"),
            DevicePortBindingError::OppositeDirection(port) => format!(
                "{}\n{}",
                at(request, "指定した回路ポートの方向が逆になっている。"),
                config::source_diagnostic(
                    source.settings_path,
                    &format!("回路側のポート {}", super::diagnostic::located(&port.name))
                ),
            ),
            DevicePortBindingError::TypeMismatch { actual, .. } => {
                let reason = if i2c.is_some() {
                    format!(
                        "RP の接続先には単一ビットが必要だが、指定したポートの型は {}。",
                        super::diagnostic::clocked::port_type(actual.ty)
                    )
                } else {
                    "RP の接続先には単一ビットの回路ポートを指定する。".into()
                };
                format!(
                    "{}\n{}",
                    at(request, &reason),
                    config::source_diagnostic(
                        source.settings_path,
                        &format!(
                            "回路側のポート {}",
                            super::diagnostic::located(&actual.name)
                        )
                    ),
                )
            }
            _ => at(
                request,
                "接続先のモジュールまたはポート宣言を検査できない。",
            ),
        },
        GpioImageError::MissingInput { port } => config::at(
            board,
            None,
            &if i2c.is_some() {
                format!("回路入力 {} に GPIO または I²C の接続が必要。", port + 1)
            } else if wiring.uart.is_some() {
                format!("回路入力 {} に GPIO または UART の接続が必要。", port + 1)
            } else {
                format!("回路入力 {} に GPIO の接続が必要。", port + 1)
            },
        ),
        GpioImageError::Circuit(error) => config::source_diagnostic(
            source.settings_path,
            &super::diagnostic::clocked::error(
                source.project,
                source.owner,
                source.name,
                ClockedModuleVmError::Hierarchy(error),
            ),
        ),
        GpioImageError::LayoutSize => "回路の配線数が RP の32ビット添字に収まらない。".into(),
        GpioImageError::NoModule => "RP で実行する開始モジュールが見つからない。".into(),
        GpioImageError::ZeroTickPeriod => "RP の tick 周期は0より大きい必要がある。".into(),
    }
}

/// 設定から選んだ開始モジュールと、診断の位置を引くためのプロジェクト。
pub(super) struct Source<'p, 'a> {
    pub(super) settings_path: &'p Path,
    pub(super) settings: &'p ProjectConfig,
    pub(super) project: &'p CircuitProject<'a>,
    pub(super) owner: ModuleBodyPosition,
    pub(super) name: &'p str,
}

/// 検査済みのルートから RP の配線表を生成して標準出力へ書く。
pub(super) fn export(
    source: Source<'_, '_>,
    board: &Path,
    aliases: &UseAliases<'_, '_, '_>,
    stdout: &mut impl Write,
) -> Result<(), String> {
    if let Some(device) = source.settings.device.first() {
        return Err(config::at(
            source.settings_path,
            Some(device.span()),
            "RP の GPIO 実行は project の device 指定に対応していない。",
        ));
    }
    if let Some(vram) = source.settings.vram.first() {
        return Err(config::at(
            source.settings_path,
            Some(vram.read_output.span()),
            "RP の GPIO 実行は project の VRAM 指定に対応していない。",
        ));
    }
    let period = period_micros(source.settings_path, source.settings)?;
    let wiring = read_board(board)?;
    if let (Some(uart), Some(i2c)) = (&wiring.uart, &wiring.i2c) {
        let (first, later) = if uart.span().start < i2c.span().start {
            (uart.span(), i2c.span())
        } else {
            (i2c.span(), uart.span())
        };
        return Err(format!(
            "{}\n{}",
            config::at(
                board,
                Some(later),
                "UART と I²C を同じ配線表に指定できない。"
            ),
            config::at(board, Some(first), "先に指定した周辺機能。"),
        ));
    }
    let bindings: Vec<_> = wiring
        .gpio
        .iter()
        .map(|row| row.get_ref().binding())
        .collect();
    if let Some(settings) = &wiring.i2c {
        return i2c::export(
            stdout, &source, board, &wiring, settings, aliases, &bindings, period,
        );
    }
    if let Some(uart) = &wiring.uart {
        let image = aliases
            .build_rp_uart_image(
                source.owner,
                wiring.board.native(),
                &bindings,
                uart.get_ref().setup(),
                uart.get_ref().names(),
                period,
                super::WIDTH_LIMIT,
                super::BIT_LIMIT,
                super::GATE_LIMIT,
            )
            .map_err(|error| match error {
                UartImageError::ZeroBaud => config::at(
                    board,
                    Some(uart.span()),
                    "UART のボーレートは0より大きい必要がある。",
                ),
                UartImageError::Circuit(cause) => image_error(cause, board, &wiring, &source, None),
            })?;
        return write_uart_plan(stdout, wiring.board, &image).map_err(super::output_error);
    }
    let image = aliases
        .build_rp_gpio_image(
            source.owner,
            wiring.board.native(),
            &bindings,
            period,
            super::WIDTH_LIMIT,
            super::BIT_LIMIT,
            super::GATE_LIMIT,
        )
        .map_err(|error| image_error(error, board, &wiring, &source, None))?;
    write_plan(stdout, wiring.board, &image).map_err(super::output_error)
}

fn write_plan(
    stdout: &mut impl Write,
    board: BoardKind,
    image: &GpioCircuitImage,
) -> std::io::Result<()> {
    writeln!(stdout, "ncg2-rp-gpio-v1")?;
    writeln!(stdout, "board {}", board.wire_name())?;
    writeln!(stdout, "period-us {}", image.tick_micros())?;
    for pin in image.input_pins() {
        writeln!(stdout, "input {pin}")?;
    }
    write_layout(stdout, image.layout(), image.output_pins())
}

fn write_uart_plan(
    out: &mut impl Write,
    board: BoardKind,
    image: &UartCircuitImage,
) -> std::io::Result<()> {
    writeln!(out, "ncg2-rp-uart-v1")?;
    writeln!(out, "board {}", board.wire_name())?;
    writeln!(out, "period-us {}", image.tick_micros())?;
    let uart = image.uart();
    writeln!(
        out,
        "uart {} {} {} {}",
        uart.instance, uart.tx, uart.rx, uart.baud
    )?;
    let ports = image.uart_outputs();
    write!(out, "uart-outputs {}", ports.rx_ready)?;
    for bit in ports.tx_data {
        write!(out, " {bit}")?;
    }
    writeln!(out, " {} {}", ports.tx_valid, ports.tx_done_ready)?;
    for source in image.inputs() {
        match *source {
            RpInputSource::Gpio(pin) => writeln!(out, "input {pin}")?,
            RpInputSource::Uart(UartInputSignal::RxBit(bit)) => {
                writeln!(out, "input-uart rx-data {bit}")?
            }
            RpInputSource::Uart(signal) => {
                let name = match signal {
                    UartInputSignal::RxValid => "rx-valid",
                    UartInputSignal::RxFraming => "rx-framing",
                    UartInputSignal::RxOverrun => "rx-overrun",
                    UartInputSignal::TxReady => "tx-ready",
                    UartInputSignal::TxDone => "tx-done",
                    UartInputSignal::TxOk => "tx-ok",
                    UartInputSignal::RxBit(_) => unreachable!(),
                };
                writeln!(out, "input-uart {name}")?;
            }
        }
    }
    write_layout(out, image.layout(), image.gpio_outputs())
}

fn write_layout(
    stdout: &mut impl Write,
    layout: &PackedNorLayout32,
    outputs: &[GpioOutputPin],
) -> std::io::Result<()> {
    for gate in layout.gates() {
        writeln!(stdout, "gate {} {}", gate.left, gate.right)?;
    }
    for port in layout.outputs() {
        writeln!(stdout, "port {port}")?;
    }
    for output in outputs {
        writeln!(
            stdout,
            "output {} {} {}",
            output.port,
            output.pin,
            u8::from(output.initial_high)
        )?;
    }
    Ok(())
}
