//! 展開済みの回路を対象に、固定入力での定常化を報告する。

use std::{io::Write, num::NonZeroU64, process::ExitCode};

use circuitgame2_core::project::{CircuitProject, ModuleBodyPosition, UseAliases};

use super::{clocked::LIMITS, diagnostic, output_error};

/// 固定入力で経過 tick 数が上界に達してから繰り返す間隔。
pub(super) struct Condition {
    pub(super) within: u64,
    pub(super) period: NonZeroU64,
}

/// 回路を展開して上界を求め、指定した条件に対する結果を返す。
pub(super) fn analyze(
    project: &CircuitProject<'_>,
    aliases: &UseAliases<'_, '_, '_>,
    position: ModuleBodyPosition,
    name: &str,
    condition: Option<Condition>,
    source_diagnostic: impl Fn(String) -> String,
    stdout: &mut impl Write,
) -> Result<ExitCode, String> {
    let compiled = aliases
        .lower_clocked_vm(position, LIMITS)
        .map_err(|error| {
            source_diagnostic(diagnostic::clocked::error(project, position, name, error))
        })?
        .ok_or_else(|| format!("{name} のモジュールを解析できない。"))?;
    let bound = compiled.vm.settling_bound();
    report(stdout, condition, bound)
}

fn report(
    stdout: &mut impl Write,
    condition: Option<Condition>,
    bound: Option<usize>,
) -> Result<ExitCode, String> {
    let unproved = ExitCode::from(2);
    let (status, result) = match (condition, bound) {
        (None, Some(bound)) => (
            ExitCode::SUCCESS,
            writeln!(stdout, "推定上界 {bound} tick。固定入力で定常化する。"),
        ),
        (None, None) => (
            unproved,
            writeln!(stdout, "未推定。ゲート専用の非循環回路を対象とする。"),
        ),
        (Some(Condition { within, period }), Some(bound))
            if bound as u128 <= u128::from(within) =>
        {
            (
                ExitCode::SUCCESS,
                writeln!(
                    stdout,
                    "検証済み。固定入力で {within} tick 後から {period} tick 間隔で同じ状態になる。推定上界は {bound} tick。"
                ),
            )
        }
        (Some(Condition { within, .. }), Some(bound)) => (
            unproved,
            writeln!(
                stdout,
                "未証明。推定上界 {bound} tick が指定の {within} tick を超える。"
            ),
        ),
        (Some(_), None) => (
            unproved,
            writeln!(stdout, "未証明。ゲート専用の非循環回路を対象とする。"),
        ),
    };
    result.map_err(output_error)?;
    Ok(status)
}
