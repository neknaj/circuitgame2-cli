//! 開いた本文とディスクを照合し、プロジェクトのソースを文書版付きで集める。

use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet, btree_map::Entry},
    fs, io,
    path::{Component, Path, PathBuf},
};

use circuitgame2_cli::config;
use circuitgame2_core::{
    editor::{LineIndex, PositionEncoding},
    project::{LocatedName, ProjectSource, SourceSnapshot, normalize_path},
};
use lsp_types::{Location, Uri};
use url::Url;

use super::diagnostic::wire_range;

/// エディタが保持している版。同期に失敗した版は本文を持たない。
#[derive(Clone, Copy)]
pub(super) struct OpenDocument<'a> {
    pub(super) uri: &'a Uri,
    pub(super) text: Option<&'a str>,
    pub(super) version: i32,
}

struct Source<'a> {
    id: String,
    uri: Uri,
    text: Cow<'a, str>,
    revision: u64,
}

/// 一つのプロジェクトの解析に使うソース集合。
pub(super) struct SourceSet<'a> {
    sources: Vec<Source<'a>>,
}

pub(super) type SourcePaths = Vec<(String, PathBuf)>;
pub(super) type ProjectKey = (PathBuf, Vec<String>);

pub(super) struct ConfiguredSources {
    id: String,
    paths: SourcePaths,
    root: PathBuf,
}

impl ConfiguredSources {
    /// Windows の設定ルートは実体でそろえ、プロジェクト内の仮想 ID は設定の綴りを使う。
    pub(super) fn for_diagnostics(self) -> (ProjectKey, String, SourcePaths) {
        #[cfg(windows)]
        let root = fs::canonicalize(&self.root).unwrap_or(self.root);
        #[cfg(not(windows))]
        let root = self.root;
        let names = self.paths.iter().map(|(id, _)| id.clone()).collect();
        ((root, names), self.id, self.paths)
    }
}

/// 設定を読み、問い合わせ元の ID とプロジェクトのソースを選ぶ。
pub(super) fn select(uri: &Uri) -> Result<Option<ConfiguredSources>, String> {
    match file_path(uri) {
        Some(file) => configured_sources(&file),
        None => Ok(None),
    }
}

impl<'a> SourceSet<'a> {
    /// 開いている本文を優先して、対象プロジェクトのソースを集める。
    ///
    /// # Errors
    ///
    /// 設定が不正か、必要なソースを取得できない場合に理由を返す。
    pub(super) fn load(
        current: OpenDocument<'a>,
        open: impl Iterator<Item = OpenDocument<'a>>,
    ) -> Result<(String, Self), String> {
        let (current_id, sources) = match select(current.uri)? {
            Some(configured) => {
                let sources =
                    load_sources(configured.paths, [(configured.id.as_str(), current)], open)?;
                (configured.id, sources)
            }
            None => standalone(&current)?,
        };
        Ok((current_id, Self { sources }))
    }

    /// 同じ設定を使う開いた文書の版を、一つのソース集合に固定する。
    pub(super) fn load_group(
        paths: SourcePaths,
        documents: &[(String, OpenDocument<'a>)],
        open: impl Iterator<Item = OpenDocument<'a>>,
    ) -> Result<Self, String> {
        let overrides = documents.iter().map(|(id, doc)| (id.as_str(), *doc));
        let sources = load_sources(paths, overrides, open)?;
        Ok(Self { sources })
    }

    /// 設定に属さない文書の本文を使って、一つのソース集合を作る。
    pub(super) fn single(current: OpenDocument<'a>) -> Result<Self, String> {
        let (_, sources) = standalone(&current)?;
        Ok(Self { sources })
    }

    /// Core に渡す本文と版を、識別子に対応させる。
    pub(super) fn snapshots(&self) -> Vec<SourceSnapshot<'_>> {
        self.sources
            .iter()
            .map(|source| SourceSnapshot {
                path: &source.id,
                text: &source.text,
                revision: source.revision,
            })
            .collect()
    }

    /// Core が指す範囲を、そのソースと版の本文から LSP の位置に変換する。
    pub(super) fn location(
        &self,
        position: LocatedName<'_, '_>,
        encoding: PositionEncoding,
    ) -> Option<Location> {
        let source = position.source;
        let target = self
            .sources
            .iter()
            .find(|loaded| loaded.revision == source.revision && from_file(source, &loaded.id))?;
        let text = source.text();
        let lines = LineIndex::new(text);
        let range = wire_range(&lines, text.as_bytes(), &position.span, encoding)?;
        Some(Location::new(target.uri.clone(), range))
    }
}

type OpenSources<'a> = BTreeMap<PathBuf, Option<OpenDocument<'a>>>;

fn load_sources<'a, 'id>(
    paths: SourcePaths,
    documents: impl IntoIterator<Item = (&'id str, OpenDocument<'a>)>,
    open: impl Iterator<Item = OpenDocument<'a>>,
) -> Result<Vec<Source<'a>>, String> {
    let mut overlays = OpenSources::new();
    for (path, doc) in open.filter_map(|doc| file_path(doc.uri).map(|path| (path, doc))) {
        match overlays.entry(path) {
            Entry::Vacant(entry) => {
                entry.insert(Some(doc));
            }
            Entry::Occupied(mut entry) => {
                entry.insert(None);
            }
        }
    }
    // 設定の綴りと異なる URI も、設定で選んだ仮想 ID と対応させる。
    let mut overrides = BTreeMap::new();
    for (id, document) in documents {
        match overrides.entry(id) {
            Entry::Vacant(entry) => {
                entry.insert(Some(document));
            }
            Entry::Occupied(mut entry) => {
                entry.insert(None);
            }
        }
    }
    for (id, path) in &paths {
        if let Some(document) = overrides.get(id.as_str()) {
            overlays.insert(path.clone(), *document);
        }
    }
    #[cfg(windows)]
    add_windows_overlays(&paths, &mut overlays);
    paths
        .into_iter()
        .map(|(id, path)| match overlays.get(&path) {
            Some(Some(doc)) => opened(id, doc),
            Some(None) => Err(format!("{} に開いている本文が複数ある。", path.display())),
            None => on_disk(id, &path),
        })
        .collect()
}

#[cfg(windows)]
fn add_windows_overlays(paths: &[(String, PathBuf)], overlays: &mut OpenSources<'_>) {
    let declared: BTreeSet<_> = paths.iter().map(|(_, path)| path).collect();
    let mut unmatched = OpenSources::new();
    for (path, doc) in overlays.iter().filter(|(path, _)| !declared.contains(path)) {
        if let Ok(host_id) = fs::canonicalize(path) {
            match unmatched.entry(host_id) {
                Entry::Vacant(entry) => {
                    entry.insert(*doc);
                }
                Entry::Occupied(mut entry) => {
                    entry.insert(None);
                }
            }
        }
    }
    if unmatched.is_empty() {
        return;
    }
    // 実体が同じ設定パスが複数あれば、仮想 ID ごとに一致する URI だけを使う。
    let mut hosts = BTreeMap::new();
    for (_, path) in paths {
        if let Ok(host_id) = fs::canonicalize(path)
            && unmatched.contains_key(&host_id)
        {
            match hosts.entry(host_id) {
                Entry::Vacant(entry) => {
                    entry.insert(Some(path));
                }
                Entry::Occupied(mut entry) => {
                    entry.insert(None);
                }
            }
        }
    }
    for (host_id, path) in hosts {
        if let Some(path) = path
            && let Some(doc) = unmatched.remove(&host_id)
        {
            overlays.entry(path.clone()).or_insert(doc);
        }
    }
}

fn from_file(project_source: &ProjectSource<'_>, id: &str) -> bool {
    let Ok(id) = normalize_path(id) else {
        return false;
    };
    project_source.id.same_file(&id)
}

fn standalone<'a>(doc: &OpenDocument<'a>) -> Result<(String, Vec<Source<'a>>), String> {
    let id = "document.ncg2";
    Ok((id.into(), vec![opened(id.into(), doc)?]))
}

fn opened<'a>(id: String, doc: &OpenDocument<'a>) -> Result<Source<'a>, String> {
    let text = doc
        .text
        .ok_or_else(|| format!("{} の最新版の本文を取得できない。", doc.uri.as_str()))?;
    // ディスクから読んだ版は 0。LSP は負の版番号も使えるため、符号付きの順序を保ってずらす。
    let revision = (i64::from(doc.version) - i64::from(i32::MIN) + 1) as u64;
    Ok(Source {
        id,
        uri: doc.uri.clone(),
        text: Cow::Borrowed(text),
        revision,
    })
}

fn on_disk<'a>(id: String, path: &Path) -> Result<Source<'a>, String> {
    let uri: Uri = Url::from_file_path(path)
        .map_err(|_| format!("{} をファイル URI にできない。", path.display()))?
        .as_str()
        .parse()
        .map_err(|_| format!("{} のファイル URI を解釈できない。", path.display()))?;
    let text = fs::read_to_string(path)
        .map_err(|error| format!("{} を UTF-8 で読み込めない ({error})。", path.display()))?;
    Ok(Source {
        id,
        uri,
        text: Cow::Owned(text),
        revision: 0,
    })
}

fn file_path(uri: &Uri) -> Option<PathBuf> {
    let url = Url::parse(uri.as_str()).ok()?;
    if url.scheme() != "file" || url.fragment().is_some() || url.query().is_some() {
        return None;
    }
    url.to_file_path().ok()
}

/// 最寄りの設定が選んだソースだけを、設定の初出順に返す。
fn configured_sources(file: &Path) -> Result<Option<ConfiguredSources>, String> {
    let Some(parent) = file.parent() else {
        return Ok(None);
    };
    let Some((root, manifest)) = nearest_manifest(parent)? else {
        return Ok(None);
    };
    let settings = config::read_settings(&manifest)?;
    let mut seen = BTreeSet::new();
    let mut paths = Vec::new();
    let mut current_id = None;
    for source in settings.sources.into_inner() {
        let path = normalize_path(source.get_ref()).map_err(|error| {
            config::at(
                &manifest,
                Some(source.span()),
                &format!("ソースのパスを正規化できない ({error:?})。"),
            )
        })?;
        if path.source_kind().is_none() {
            return Err(config::at(
                &manifest,
                Some(source.span()),
                "ソースの拡張子には .ncg2 または .ncg2m を指定する。",
            ));
        }
        let id = path.components().join("/");
        if !seen.insert(id.clone()) {
            continue;
        }
        if Path::new(&id)
            .components()
            .any(|part| matches!(part, Component::Prefix(_) | Component::RootDir))
        {
            return Err(config::at(
                &manifest,
                Some(source.span()),
                "ソースは設定ファイルからの相対パスで指定する。",
            ));
        }
        let host = root.join(&id);
        if host == file {
            current_id = Some(id.clone());
        }
        paths.push((id, host));
    }
    #[cfg(windows)]
    if current_id.is_none() {
        current_id = windows_source_id(file, &paths);
    }
    Ok(current_id.map(|id| ConfiguredSources {
        id,
        paths,
        root: root.to_path_buf(),
    }))
}

#[cfg(windows)]
fn windows_source_id(file: &Path, paths: &[(String, PathBuf)]) -> Option<String> {
    let host_id = fs::canonicalize(file).ok()?;
    let mut matching = paths
        .iter()
        .filter(|(_, path)| fs::canonicalize(path).is_ok_and(|identity| identity == host_id));
    let (id, _) = matching.next()?;
    matching.next().is_none().then(|| id.clone())
}

fn nearest_manifest(parent: &Path) -> Result<Option<(&Path, PathBuf)>, String> {
    for root in parent.ancestors() {
        let manifest = root.join("circuitgame2.toml");
        match fs::metadata(&manifest) {
            Ok(_) => return Ok(Some((root, manifest))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(config::at(
                    &manifest,
                    None,
                    &format!("設定の有無を確認できない ({error})。"),
                ));
            }
        }
    }
    Ok(None)
}
