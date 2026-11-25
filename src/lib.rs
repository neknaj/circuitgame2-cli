//! CLI と LSP が共用する設定と解析の上限。

pub mod config;

/// 実行コマンドとエディタが定常化を解析するときに使う回路の資源上限。
pub const CLOCKED_LIMITS: circuitgame2_core::project::ClockedModuleVmLimits =
    circuitgame2_core::project::ClockedModuleVmLimits {
        width: 65_536,
        wiring_bits: 1_048_576,
        gates: 1_048_576,
        state_bits: 1_048_576,
    };
