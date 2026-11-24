//! TOML の実行設定を読み、設定ファイル内の値の位置を保持する。

use std::{collections::BTreeMap, fs, num::NonZeroU64, ops::Range, path::Path};

use serde::Deserialize;
use toml::{Spanned, Table};

/// ソース集合とモジュールの開始点を指定する実行設定。
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(super) struct ProjectConfig {
    pub(super) sources: Spanned<Vec<Spanned<String>>>,
    pub(super) entry: Option<Spanned<String>>,
    #[serde(rename = "tick-ns")]
    _tick_ns: Option<TickPeriod>,
    #[serde(default)]
    device: Vec<Spanned<Device>>,
    #[serde(default)]
    pub(super) vram: Vec<Vram>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TickPeriod {
    #[serde(rename = "numerator")]
    _numerator: NonZeroU64,
    #[serde(rename = "denominator")]
    _denominator: NonZeroU64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Device {
    #[serde(rename = "name")]
    _name: String,
    #[serde(rename = "kind")]
    _kind: String,
    #[serde(rename = "bindings")]
    _bindings: BTreeMap<String, String>,
    #[serde(rename = "options")]
    _options: Option<Table>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(super) struct Vram {
    pub(super) name: String,
    pub(super) read_output: Spanned<String>,
}

/// 設定ファイル名と、原文の UTF-8 バイト範囲を診断に付ける。
pub(super) fn at(path: &Path, span: Option<Range<usize>>, reason: &str) -> String {
    let path = path.display();
    match span {
        Some(span) => format!("{path}[{}..{}] {reason}", span.start, span.end),
        None => format!("{path} {reason}"),
    }
}

/// ソースの仮想パスを表示するときは、その基準となる設定ファイルを示す。
pub(super) fn source_diagnostic(path: &Path, message: &str) -> String {
    format!(
        "{} があるディレクトリを基準とする回路ソースの診断。\n{message}",
        path.display()
    )
}

/// 設定の形を検査し、CLI で使えるソース集合を返す。
pub(super) fn read(path: &Path) -> Result<ProjectConfig, String> {
    let bytes = fs::read(path)
        .map_err(|error| at(path, None, &format!("設定を読み込めない ({error})。")))?;
    let source = std::str::from_utf8(&bytes).map_err(|error| {
        let start = error.valid_up_to();
        let end = error.error_len().map_or(bytes.len(), |width| start + width);
        at(path, Some(start..end), "設定に不正な UTF-8 が含まれる。")
    })?;
    let config: ProjectConfig = toml::from_str(source).map_err(|error| {
        at(
            path,
            error.span(),
            &format!("設定を解釈できない。{}", error.message()),
        )
    })?;
    if config.sources.get_ref().is_empty() {
        return Err(at(
            path,
            Some(config.sources.span()),
            "sources にソースを1件以上指定する。",
        ));
    }
    if let Some(device) = config.device.first() {
        return Err(at(
            path,
            Some(device.span()),
            "この CLI は外部デバイスの接続に未対応。",
        ));
    }
    Ok(config)
}
