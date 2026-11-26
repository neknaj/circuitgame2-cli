//! 順序回路の VM を初期化し、reset と各 tick の公開出力を表示する。

use std::{io::Write, path::Path};

use circuitgame2_core::{
    netlist::PortBitLayout,
    project::{
        CircuitProject, ClockedModuleVmLimits, ModuleBodyPosition, UseAliases, VramDeclaration,
        VramSetupError,
    },
};

use super::{config, diagnostic, format_bits, input_bits, output_error, snapshot, validate_input};

pub(super) use circuitgame2_cli::CLOCKED_LIMITS as LIMITS;

/// 実行するモジュール、入力列、出力に加える VRAM の指定。
pub(super) struct TraceOptions<'a> {
    pub(super) name: &'a str,
    pub(super) frames: InputFrames<'a>,
    pub(super) vram: &'a [&'a str],
    pub(super) snapshot: snapshot::Options<'a>,
    pub(super) settings: Option<(&'a Path, &'a config::ProjectConfig)>,
    pub(super) limits: ClockedModuleVmLimits,
}

/// tick ごとに与える入力。空入力を繰り返す場合は回数だけを保持する。
pub(super) enum InputFrames<'a> {
    Explicit(Vec<&'a str>),
    EmptyTicks(u64),
}

impl<'a> InputFrames<'a> {
    pub(super) fn for_inputs(
        &self,
        inputs: &PortBitLayout,
        name: &str,
    ) -> Result<(&[&'a str], u64), String> {
        match self {
            Self::Explicit(frames) => Ok((frames.as_slice(), 0)),
            Self::EmptyTicks(ticks) if inputs.ranges().is_empty() => Ok((&[][..], *ticks)),
            Self::EmptyTicks(_) => {
                let width = inputs.total_bits();
                Err(format!(
                    "--ticks は入力ポートがない回路に使える。{name} には {width} ビットの入力が必要。"
                ))
            }
        }
    }
}

/// reset と各 tick の出力を、tick 番号とビット列の組で一行ずつ書く。
///
/// 全入力の幅と表記、観測する VRAM の公開名を確認してから出力を始める。
pub(super) fn trace(
    project: &CircuitProject<'_>,
    aliases: &UseAliases<'_, '_, '_>,
    position: ModuleBodyPosition,
    options: TraceOptions<'_>,
    stdout: &mut impl Write,
) -> Result<(), String> {
    let TraceOptions {
        name,
        frames,
        vram,
        snapshot,
        settings,
        limits,
    } = options;
    let declared = settings.map_or(&[][..], |(_, config)| config.vram.as_slice());
    let declarations: Vec<_> = declared
        .iter()
        .map(|entry| VramDeclaration {
            name: &entry.name,
            read_output: entry.read_output.get_ref(),
        })
        .collect();
    let mut compiled = aliases
        .lower_clocked_vram(position, limits, &declarations)
        .map_err(|error| match error {
            VramSetupError::Circuit(error) => {
                let message = diagnostic::clocked::error(project, position, name, error);
                match settings {
                    Some((path, _)) => config::source_diagnostic(path, &message),
                    None => message,
                }
            }
            VramSetupError::Output { declaration, error } => {
                let message = diagnostic::clocked::vram_output_error(error);
                match settings {
                    Some((path, _)) => config::at(
                        path,
                        declared
                            .get(declaration)
                            .map(|vram| vram.read_output.span()),
                        &message,
                    ),
                    None => message,
                }
            }
            VramSetupError::Allocation => {
                "VRAM の公開設定を保持する領域を確保できない。".to_owned()
            }
        })?
        .ok_or_else(|| format!("{name} のモジュールを実行できない。"))?;

    #[cfg(any(unix, windows))]
    if snapshot.requested() {
        return snapshot::trace(
            project,
            compiled,
            TraceOptions {
                name,
                frames,
                vram,
                snapshot,
                settings,
                limits,
            },
            stdout,
        );
    }
    #[cfg(not(any(unix, windows)))]
    let _ = snapshot;

    let (explicit, empty_ticks) = frames.for_inputs(compiled.inputs(), name)?;
    let width = compiled.inputs().total_bits();
    for (index, frame) in explicit.iter().enumerate() {
        validate_input(frame, width)
            .map_err(|reason| format!("tick {} の入力。{reason}", index + 1))?;
    }

    for &name in vram {
        if compiled.read_vram(name).is_none() {
            return Err(format!("公開 VRAM {name:?} が設定に見つからない。"));
        }
    }
    let output = compiled.reset();
    write_frame(stdout, vram, 0, output, |_, name| {
        compiled.read_vram(name).map(|view| view.bits)
    })?;
    let frames = explicit.iter().copied().chain((0..empty_ticks).map(|_| ""));
    for (tick, frame) in (1..=u64::MAX).zip(frames) {
        let output = compiled
            .step(&input_bits(frame))
            .map_err(|error| format!("{name} の tick {tick} を実行できない ({error:?})。"))?;
        write_frame(stdout, vram, tick, output, |_, name| {
            compiled.read_vram(name).map(|view| view.bits)
        })?;
    }
    Ok(())
}

/// 回路出力の後ろに、指定順で VRAM の記憶部を並べる。
pub(super) fn write_frame<'a>(
    stdout: &mut impl Write,
    vram: &[&str],
    tick: u64,
    output: Vec<bool>,
    memory: impl Fn(usize, &str) -> Option<&'a [bool]>,
) -> Result<(), String> {
    let mut line = format!("{tick} {}", format_bits(output));
    for (index, &name) in vram.iter().enumerate() {
        let bits = memory(index, name)
            .ok_or_else(|| format!("tick {tick} で公開 VRAM {name:?} を読み取れない。"))?;
        line.push(' ');
        line.push_str(&format_bits(bits.iter().copied()));
    }
    writeln!(stdout, "{line}").map_err(output_error)
}
