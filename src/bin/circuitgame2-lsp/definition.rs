//! 設定と開いている文書からスナップショットを作り、Core が解決した宣言の位置を返す。

use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet, btree_map::Entry},
    fs, io,
    path::{Component, Path, PathBuf},
};

use circuitgame2_cli::config;
use circuitgame2_core::{
    editor::{LineIndex, PositionEncoding, TextPosition},
    project::{
        CircuitProject, DefinitionBinding, LocatedName, ProjectSource, SourceSnapshot, VirtualPath,
        check_use_aliases, index_use_targets, normalize_path, parse_circuit_project,
        validate_circuit_basis,
    },
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

/// 指定位置にある `def` の参照、`use`、`module-part` の対象が指す宣言を探す。
///
/// 未保存の本文を優先し、選んだソースの同じ版を解析と位置変換に使う。
///
/// # Errors
///
/// ファイルを読めない場合や設定の形式が不正な場合はエラーを返す。
pub(super) fn locate<'a>(
    current: OpenDocument<'a>,
    position: TextPosition,
    encoding: PositionEncoding,
    open: impl Iterator<Item = OpenDocument<'a>>,
) -> Result<Option<Location>, String> {
    let Some(text) = current.text else {
        return Ok(None);
    };
    let Some(offset) = LineIndex::new(text).position_to_byte(position, encoding) else {
        return Ok(None);
    };
    let (current_id, sources) = match file_path(current.uri) {
        Some(file) => match configured_sources(&file)? {
            Some((current_id, paths)) => {
                let sources = load_sources(paths, &current_id, current, open)?;
                (current_id, sources)
            }
            None => standalone(&current)?,
        },
        None => standalone(&current)?,
    };
    let snapshots: Vec<_> = sources
        .iter()
        .map(|source| SourceSnapshot {
            path: &source.id,
            text: &source.text,
            revision: source.revision,
        })
        .collect();
    let Ok(project) = parse_circuit_project(&snapshots) else {
        return Ok(None);
    };
    let current = normalize_path(&current_id)
        .map_err(|_| format!("{current_id} の仮想パスを解釈できない。"))?;
    let target = resolved_location(&project, &current, offset);
    Ok(target.and_then(|location| {
        let source = location.source;
        let target = sources
            .iter()
            .find(|loaded| from_file(source, &loaded.id))?;
        let lines = LineIndex::new(source.text);
        let range = wire_range(&lines, source.text.as_bytes(), &location.span, encoding)?;
        Some(Location::new(target.uri.clone(), range))
    }))
}

fn resolved_location<'p, 'a>(
    project: &'p CircuitProject<'a>,
    current: &VirtualPath<'_>,
    offset: usize,
) -> Option<LocatedName<'p, 'a>> {
    validate_circuit_basis(project).ok()?;
    let targets = index_use_targets(project).ok()?;
    let scope = check_use_aliases(&targets).ok()?;
    let file = project
        .sources()
        .iter()
        .position(|source| source.id.same_file(current))?;
    if let Some(target) = scope.resolve_reference_at_offset(file, offset) {
        let target = target.ok()?;
        return match target.binding {
            DefinitionBinding::Declaration { source, form } => Some(LocatedName {
                source,
                span: form.name.span.clone(),
            }),
            DefinitionBinding::Nor => None,
        };
    }
    if let Some(target) = targets.resolve_module_part_at_offset(file, offset) {
        let target = target.ok()?;
        return Some(LocatedName {
            source: target.module_source,
            span: target.module.name.span.clone(),
        });
    }
    targets.alias_location(current, offset)
}

type OpenSources<'a> = BTreeMap<PathBuf, Option<OpenDocument<'a>>>;

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{Position, Range};

    #[test]
    fn finds_definition_with_an_unresolved_call_elsewhere() {
        let uri = "untitled:local.ncg2".parse().unwrap();
        let bad = "(func bad (in x) (out y) (def y (missing x)))";
        let caller = "(func caller (in x) (out y) (def y (invert x)))";
        let text = format!(
            "(using nor)\n(func invert (in x) (out y) (def y (nor x x)))\n{bad}\n{caller}\n"
        );
        let current = OpenDocument {
            uri: &uri,
            text: Some(&text),
            version: 1,
        };
        let query = |line, column| {
            locate(
                current,
                TextPosition { line, column },
                PositionEncoding::Utf16,
                std::iter::once(current),
            )
            .unwrap()
        };

        let call = caller.find("invert").unwrap();
        assert_eq!(
            query(3, call),
            Some(Location::new(
                uri.clone(),
                Range::new(Position::new(1, 6), Position::new(1, 12)),
            )),
        );
        assert_eq!(query(3, call + "invert".len()), None);
        assert_eq!(query(2, bad.find("missing").unwrap()), None);
    }

    #[test]
    fn module_part_jumps_to_its_module_in_the_same_namespace() {
        let uri = "untitled:parts.ncg2".parse().unwrap();
        let text = "(using nor)\n\
                    (namespace other (module owner (in x) (out y) (def y x)))\n\
                    (namespace local (module owner (in x) (out y)))\n\
                    (namespace local (module-part owner (def y x)))\n\
                    (namespace local (module-part missing (def y x)))\n";
        let current = OpenDocument {
            uri: &uri,
            text: Some(text),
            version: 1,
        };
        let query = |line, column| {
            locate(
                current,
                TextPosition { line, column },
                PositionEncoding::Utf16,
                std::iter::once(current),
            )
            .unwrap()
        };

        assert_eq!(
            query(3, 30),
            Some(Location::new(
                uri.clone(),
                Range::new(Position::new(2, 25), Position::new(2, 30)),
            )),
        );
        assert_eq!(query(3, 35), None);
        assert_eq!(query(4, 30), None);
    }
}

fn load_sources<'a>(
    paths: Vec<(String, PathBuf)>,
    current_id: &str,
    current: OpenDocument<'a>,
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
    // 依頼元の URI が設定の綴りと異なっていても、選んだ仮想 ID にはその本文を使う。
    for (id, path) in &paths {
        if id == current_id {
            overlays.insert(path.clone(), Some(current));
            break;
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

type ConfiguredSources = (String, Vec<(String, PathBuf)>);

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
    Ok(current_id.map(|id| (id, paths)))
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
