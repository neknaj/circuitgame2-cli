//! CLI と LSP が共用する設定と解析の上限。

pub mod config;

/// 実行コマンドとエディタが定常化を解析するときに使う回路の資源上限。
pub const CLOCKED_LIMITS: circuitgame2_core::project::ClockedModuleVmLimits =
    circuitgame2_core::project::ClockedModuleVmLimits {
        width: 65_536,
        wiring_bits: 1_048_576,
        gates: 1_048_576,
        state_bits: circuitgame2_project_config::DEFAULT_STATE_BITS,
    };

/// プロジェクトの指定を反映した順序回路の資源上限を返す。
pub fn clocked_limits(
    settings: Option<&config::ProjectConfig>,
) -> circuitgame2_core::project::ClockedModuleVmLimits {
    circuitgame2_core::project::ClockedModuleVmLimits {
        state_bits: settings.map_or(
            CLOCKED_LIMITS.state_bits,
            config::ProjectConfig::simulation_state_bits,
        ),
        ..CLOCKED_LIMITS
    }
}
