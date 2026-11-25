//! 回路ポートとボード端子を照合し、RP ファームウェアへ渡す配線表を出力する。

use std::{fs, io::Write, ops::Range, path::Path};

use circuitgame2_cli::config::{self, ProjectConfig};
use circuitgame2_core::time::exact_microseconds;
use circuitgame2_core::{
    backend::rp::{AssignmentError, Board, GpioBinding, GpioCircuitImage, GpioImageError},
    project::{
        CircuitProject, ClockedModuleVmError, DeviceBindingsError, DevicePortBindingError,
        ModuleBodyPosition, UseAliases,
    },
};
use serde::Deserialize;
use toml::Spanned;

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
#[serde(deny_unknown_fields)]
struct Wiring {
    board: BoardKind,
    #[serde(default)]
    gpio: Vec<Spanned<Gpio>>,
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
    wiring.gpio.get(at).map(Spanned::span)
}

fn image_error(
    error: GpioImageError<'_, '_, '_>,
    board: &Path,
    wiring: &Wiring,
    source: &Source<'_, '_>,
) -> String {
    let at = |index, reason: &str| config::at(board, span(wiring, index), reason);
    let related = |index| at(index, "先に指定した接続。");
    match error {
        GpioImageError::Pin(AssignmentError::DuplicatePin { first, site, pin }) => format!(
            "{}\n{}",
            at(
                site.assignment,
                &format!("GP{pin} を複数の接続で使用している。")
            ),
            related(first.assignment)
        ),
        GpioImageError::Pin(AssignmentError::UnexposedPin { site, pin }) => at(
            site.assignment,
            &format!("GP{pin} はボードの外部端子に出ていない。"),
        ),
        GpioImageError::Pin(_) => {
            config::at(board, None, "GPIO 接続でボードの制約に違反している。")
        }
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
            DevicePortBindingError::TypeMismatch { actual, .. } => format!(
                "{}\n{}",
                at(request, "GPIO は単一ビットの回路ポートに接続する。"),
                config::source_diagnostic(
                    source.settings_path,
                    &format!(
                        "回路側のポート {}",
                        super::diagnostic::located(&actual.name)
                    )
                ),
            ),
            _ => at(
                request,
                "接続先のモジュールまたはポート宣言を検査できない。",
            ),
        },
        GpioImageError::MissingInput { port } => config::at(
            board,
            None,
            &format!("回路入力 {} に GPIO の接続が必要。", port + 1),
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

/// チェック済みのルートから GPIO 配線表を生成して標準出力へ書く。
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
    let bindings: Vec<_> = wiring
        .gpio
        .iter()
        .map(|row| row.get_ref().binding())
        .collect();
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
        .map_err(|error| image_error(error, board, &wiring, &source))?;
    write_plan(stdout, wiring.board, &image).map_err(super::output_error)
}

fn write_plan(
    stdout: &mut impl Write,
    board: BoardKind,
    image: &GpioCircuitImage,
) -> std::io::Result<()> {
    let layout = image.layout();
    writeln!(stdout, "ncg2-rp-gpio-v1")?;
    writeln!(stdout, "board {}", board.wire_name())?;
    writeln!(stdout, "period-us {}", image.tick_micros())?;
    for pin in image.input_pins() {
        writeln!(stdout, "input {pin}")?;
    }
    for gate in layout.gates() {
        writeln!(stdout, "gate {} {}", gate.left, gate.right)?;
    }
    for port in layout.outputs() {
        writeln!(stdout, "port {port}")?;
    }
    for output in image.output_pins() {
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
