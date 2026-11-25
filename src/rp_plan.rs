//! 回路ポートとボード端子を照合し、RP ファームウェアへ渡す配線表を出力する。

use std::{fs, io::Write, ops::Range, path::Path};

use circuitgame2_cli::config::{self, ProjectConfig};
use circuitgame2_core::time::exact_microseconds;
use circuitgame2_core::{
    backend::rp::{
        AssignmentError, Board, GpioBinding, GpioCircuitImage, GpioImageError, GpioOutputPin,
        PeripheralInputSource, PinSite, RpInputSource, Signal, UartCircuitImage, UartImageError,
        UartInputSignal, UartPortNames, UartSetup,
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
mod spi;

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
    spi: Option<Spanned<spi::Spi>>,
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

/// Core に渡す周辺機能の信号順と、各設定値の診断位置を対応させる。
struct PeripheralLocations<'a> {
    name: &'static str,
    instance: Range<usize>,
    pins: &'a [(Signal, &'static str, Range<usize>)],
    to: Vec<&'a Spanned<String>>,
    from: Vec<&'a Spanned<String>>,
}

impl PeripheralLocations<'_> {
    fn names(&self) -> (Vec<&str>, Vec<&str>) {
        fn names<'a>(ports: &[&'a Spanned<String>]) -> Vec<&'a str> {
            ports.iter().map(|port| port.get_ref().as_str()).collect()
        }
        (names(&self.to), names(&self.from))
    }

    fn pin(&self, signal: Signal) -> Option<(&'static str, Range<usize>)> {
        self.pins
            .iter()
            .find_map(|(kind, name, span)| (*kind == signal).then(|| (*name, span.clone())))
    }

    fn request(&self, index: usize) -> Option<Range<usize>> {
        let port = if index < self.to.len() {
            self.to.get(index)
        } else {
            self.from.get(index - self.to.len())
        };
        port.map(|name| name.span())
    }

    fn assignment_error(&self, error: AssignmentError, board: &Path) -> String {
        match error {
            AssignmentError::UnknownInstance { instance, .. } => config::at(
                board,
                Some(self.instance.clone()),
                &format!(
                    "{} コントローラー番号は 0 または 1。指定値は {instance}。",
                    self.name,
                ),
            ),
            AssignmentError::UnsupportedSignal {
                site,
                pin,
                instance,
            } => {
                let (signal, span) = self
                    .pin(site.signal)
                    .unwrap_or(("信号", self.instance.clone()));
                config::at(
                    board,
                    Some(span),
                    &format!(
                        "GP{pin} は {}{instance} の {signal} に割り当てられない。",
                        self.name
                    ),
                )
            }
            _ => config::at(
                board,
                None,
                &format!(
                    "{} のピンまたはコントローラーの割り当てに違反がある。",
                    self.name
                ),
            ),
        }
    }
}

fn data_error(
    board: &Path,
    read: &Spanned<Vec<Spanned<String>>>,
    write: &Spanned<Vec<Spanned<String>>>,
) -> String {
    let read_bits = read.get_ref().len();
    let write_bits = write.get_ref().len();
    let valid = |bits| (8..=255 * 8).contains(&bits) && bits % 8 == 0;
    if !valid(read_bits) || !valid(write_bits) {
        let (field, span, bits) = if !valid(read_bits) {
            ("read-data", read.span(), read_bits)
        } else {
            ("write-data", write.span(), write_bits)
        };
        return config::at(
            board,
            Some(span),
            &format!("`{field}` のポート名は {bits} 個。1〜255 バイト分を8個ずつ指定する。"),
        );
    }
    format!(
        "{}\n{}",
        config::at(
            board,
            Some(write.span()),
            &format!(
                "`write-data` の容量は {} バイト。`read-data` と同じ容量を指定する。",
                write_bits / 8
            )
        ),
        config::at(
            board,
            Some(read.span()),
            &format!("`read-data` の容量は {} バイト。", read_bits / 8)
        ),
    )
}

fn image_error(
    error: GpioImageError<'_, '_, '_>,
    board: &Path,
    wiring: &Wiring,
    source: &Source<'_, '_>,
    peripheral: Option<&PeripheralLocations<'_>>,
) -> String {
    let request_span = |index| match peripheral {
        Some(locations) if index >= wiring.gpio.len() => {
            locations.request(index - wiring.gpio.len())
        }
        _ => span(wiring, index),
    };
    let pin_span = |site: PinSite| match peripheral {
        Some(locations) if site.assignment == wiring.gpio.len() => {
            locations.pin(site.signal).map(|(_, span)| span)
        }
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
        GpioImageError::Pin(error) => match peripheral {
            Some(locations) => locations.assignment_error(error, board),
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
                let reason = if peripheral.is_some() {
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
            &if let Some(locations) = peripheral {
                format!(
                    "回路入力 {} に GPIO または {} の接続が必要。",
                    port + 1,
                    locations.name
                )
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
    let mut peripherals = [
        wiring.uart.as_ref().map(|entry| ("UART", entry.span())),
        wiring.i2c.as_ref().map(|entry| ("I²C", entry.span())),
        wiring.spi.as_ref().map(|entry| ("SPI", entry.span())),
    ];
    peripherals.sort_by_key(|entry| entry.as_ref().map_or(usize::MAX, |(_, span)| span.start));
    if let [Some((first, first_span)), Some((later, later_span)), ..] = peripherals {
        let pair = match (first, later) {
            ("I²C", "UART") => "UART と I²C".to_owned(),
            _ => format!("{first} と {later}"),
        };
        return Err(format!(
            "{}\n{}",
            config::at(
                board,
                Some(later_span),
                &format!("{pair} を同じ配線表に指定できない。")
            ),
            config::at(board, Some(first_span), "先に指定した周辺機能。"),
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
    if let Some(settings) = &wiring.spi {
        return spi::export(
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

fn write_peripheral_layout(
    out: &mut impl Write,
    controller: &str,
    inputs: &[PeripheralInputSource],
    gpio_outputs: &[GpioOutputPin],
    peripheral_outputs: &[usize],
    layout: &PackedNorLayout32,
) -> std::io::Result<()> {
    write!(out, "{controller}-outputs")?;
    for port in peripheral_outputs {
        write!(out, " {port}")?;
    }
    writeln!(out)?;
    for source in inputs {
        match source {
            PeripheralInputSource::Gpio(pin) => writeln!(out, "input {pin}")?,
            PeripheralInputSource::Controller(index) => {
                writeln!(out, "input-{controller} {index}")?
            }
        }
    }
    write_layout(out, layout, gpio_outputs)
}
