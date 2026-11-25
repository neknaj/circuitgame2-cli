//! TOML の実行設定を読み、設定ファイル内の値の位置を保持する。

use std::{collections::BTreeMap, fs, num::NonZeroU64, ops::Range, path::Path};

use serde::Deserialize;
use toml::{Spanned, Value};

/// ソース集合とモジュールの開始点を指定する実行設定。
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct ProjectConfig {
    /// 設定に書かれた順のソースパスと原文範囲。
    pub sources: Spanned<Vec<Spanned<String>>>,
    /// 実行時の開始モジュールと原文範囲。
    pub entry: Option<Spanned<String>>,
    /// 設定した tick 周期と原文範囲。
    pub tick_ns: Option<Spanned<TickPeriod>>,
    #[serde(default)]
    /// 実行設定に並んだ外部デバイスとその位置。
    pub device: Vec<Spanned<Device>>,
    #[serde(default)]
    /// 設定順に並んだ VRAM の公開指定。
    pub vram: Vec<Vram>,
}

/// TOML に記した正の有理数の tick 周期。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TickPeriod {
    /// ナノ秒数の分子。
    pub numerator: NonZeroU64,
    /// ナノ秒数の分母。
    pub denominator: NonZeroU64,
}

/// 外部デバイスの種類、配線、種別ごとのオプション。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Device {
    /// 設定内のインスタンス名と指定箇所。
    pub name: Spanned<String>,
    /// 登録済みデバイス種別と指定箇所。
    pub kind: Spanned<String>,
    /// デバイスポートから回路ポートへの接続先とその原文範囲。
    pub bindings: Spanned<BTreeMap<String, Spanned<String>>>,
    /// 種別ごとのオプションと各値の原文範囲。
    pub options: Option<Spanned<BTreeMap<String, Spanned<Value>>>>,
}

/// 公開名と、RAM の読み出しに対応するルート回路の信号名。
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Vram {
    /// 表示や観測に使う公開名。
    pub name: String,
    /// ルート本体の出力名と、その指定箇所。
    pub read_output: Spanned<String>,
}

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
    Ok((config, bytes))
}
