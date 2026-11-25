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
    read_settings_located(path).map_err(|error| at(path, error.span, &error.reason))
}

/// 設定を読み込めない理由と、対象箇所が分かる場合のファイル内の範囲。
pub struct SettingsDiagnostic {
    /// 診断を機械処理するための識別子。
    pub code: &'static str,
    /// 設定ファイルの半開バイト範囲。
    pub span: Option<Range<usize>>,
    /// 設定ファイル名と範囲を除いたメッセージ。
    pub reason: String,
}

/// 設定と解析に使った内容を読み、診断の範囲を別に返す。
///
/// # Errors
///
/// ファイルの読み込み、UTF-8 と TOML の解析、空のソース一覧の順に失敗を報告する。
pub fn read_settings_located(path: &Path) -> Result<(ProjectConfig, Vec<u8>), SettingsDiagnostic> {
    let bytes = fs::read(path).map_err(|error| SettingsDiagnostic {
        code: "settings_read",
        span: None,
        reason: format!("設定を読み込めない ({error})。"),
    })?;
    let config = parse(&bytes).map_err(|error| match error {
        ConfigError::InvalidUtf8(span) => SettingsDiagnostic {
            code: "settings_utf8",
            span: Some(span),
            reason: "設定に不正な UTF-8 が含まれる。".into(),
        },
        ConfigError::InvalidToml { span, reason } => SettingsDiagnostic {
            code: "settings_toml",
            span,
            reason: format!("設定を解釈できない。{reason}"),
        },
        ConfigError::EmptySources(span) => SettingsDiagnostic {
            code: "settings_empty_sources",
            span: Some(span),
            reason: "sources にソースを1件以上指定する。".into(),
        },
    })?;
    Ok((config, bytes))
}
