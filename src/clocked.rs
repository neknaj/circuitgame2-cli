//! 順序回路の VM を初期化し、reset と各 tick の公開出力を表示する。

use std::io::Write;

use circuitgame2_core::project::{
    CircuitProject, ClockedModuleVmLimits, ModuleBodyPosition, UseAliases,
};

use super::{
    BIT_LIMIT, GATE_LIMIT, WIDTH_LIMIT, diagnostic, format_bits, input_bits, output_error,
    validate_input,
};

const LIMITS: ClockedModuleVmLimits = ClockedModuleVmLimits {
    width: WIDTH_LIMIT,
    wiring_bits: BIT_LIMIT,
    gates: GATE_LIMIT,
    state_bits: 1_048_576,
};

/// reset と各 tick の出力を、tick 番号とビット列の組で一行ずつ書く。
///
/// 全入力の幅と表記を確認してから出力を始める。
pub(super) fn trace(
    project: &CircuitProject<'_>,
    aliases: &UseAliases<'_, '_, '_>,
    position: ModuleBodyPosition,
    name: &str,
    frames: &[&str],
    source_diagnostic: impl Fn(String) -> String,
    stdout: &mut impl Write,
) -> Result<(), String> {
    let mut compiled = aliases
        .lower_clocked_vm(position, LIMITS)
        .map_err(|error| {
            source_diagnostic(diagnostic::clocked::error(project, position, name, error))
        })?
        .ok_or_else(|| format!("{name} のモジュールを実行できない。"))?;

    let width = compiled.inputs.total_bits();
    for (index, frame) in frames.iter().enumerate() {
        validate_input(frame, width)
            .map_err(|reason| format!("tick {} の入力。{reason}", index + 1))?;
    }

    writeln!(stdout, "0 {}", format_bits(compiled.vm.reset())).map_err(output_error)?;
    for (index, frame) in frames.iter().enumerate() {
        let tick = index + 1;
        let output = compiled
            .vm
            .step(&input_bits(frame))
            .map_err(|error| format!("{name} の tick {tick} を実行できない ({error:?})。"))?;
        writeln!(stdout, "{tick} {}", format_bits(output)).map_err(output_error)?;
    }
    Ok(())
}
