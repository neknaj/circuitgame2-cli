//! TOML の実行設定を読み、設定ファイル内の値の位置を保持する。

use std::{fs, ops::Range, path::Path};

use circuitgame2_project_config::{ConfigError, parse};
pub use circuitgame2_project_config::{Device, FILE_NAME, ProjectConfig, TickPeriod, Vram};

/// 設定ファイル名と、原文の UTF-8 バイト範囲を診断に付ける。
pub fn at(path: &Path, span: Option<Range<usize>>, reason: &str) -> String {
    let path = path.display();
    match span {
        Some(span) => format!("{path}[{}..{}] {reason}", span.start, span.end),
        None => format!("{path} {reason}"),
    }
}

/// ソースの仮想パスを表示するときは、その基準となる設定ファイルを示す。
pub fn source_diagnostic(path: &Path, message: &str) -> String {
    format!(
        "{} があるディレクトリを基準とする回路ソースの診断。\n{message}",
        path.display()
    )
}

/// 設定の形を検査し、CLI で使えるソース集合を返す。
///
/// # Errors
///
/// 設定の読み取りや形式の検査で失敗した場合、または外部デバイスの接続がある場合に失敗する。
pub fn read(path: &Path) -> Result<ProjectConfig, String> {
    let config = read_settings(path)?;
    if let Some(device) = config.device.first() {
        return Err(at(
            path,
            Some(device.span()),
            "この CLI は外部デバイスの接続に未対応。",
        ));
    }
    Ok(config)
}

/// 設定を UTF-8 で読み、項目の形とソース一覧を検査する。
///
/// # Errors
///
/// ファイルが読めない場合、設定の形式に違反がある場合、またはソース一覧が空の場合に失敗する。
pub fn read_settings(path: &Path) -> Result<ProjectConfig, String> {
    read_settings_snapshot(path).map(|(settings, _)| settings)
}

/// 設定の検査結果と、解析に使った元のファイル内容を返す。
///
/// # Errors
///
/// 読み取りや検査に失敗した場合は診断を返す。設定中の違反には位置も付ける。
pub fn read_settings_snapshot(path: &Path) -> Result<(ProjectConfig, Vec<u8>), String> {
    let bytes = fs::read(path)
        .map_err(|error| at(path, None, &format!("設定を読み込めない ({error})。")))?;
    let config = parse(&bytes).map_err(|error| match error {
        ConfigError::InvalidUtf8(span) => at(path, Some(span), "設定に不正な UTF-8 が含まれる。"),
        ConfigError::InvalidToml { span, reason } => {
            at(path, span, &format!("設定を解釈できない。{reason}"))
        }
        ConfigError::EmptySources(span) => {
            at(path, Some(span), "sources にソースを1件以上指定する。")
        }
    })?;
    Ok((config, bytes))
}
