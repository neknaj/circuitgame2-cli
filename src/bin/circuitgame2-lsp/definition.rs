//! カーソルの参照を Core で解決し、宣言の位置を返す。

use circuitgame2_core::{
    editor::{LineIndex, PositionEncoding, TextPosition},
    project::{
        CircuitProject, DefinitionBinding, LocatedName, VirtualPath, check_use_aliases,
        index_use_targets, normalize_path, parse_circuit_project, validate_circuit_basis,
    },
};
use lsp_types::Location;

use super::sources::{OpenDocument, SourceSet};

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
    let (current_id, sources) = SourceSet::load(current, open)?;
    let snapshots = sources.snapshots();
    let Ok(project) = parse_circuit_project(&snapshots) else {
        return Ok(None);
    };
    let current_path = normalize_path(&current_id)
        .map_err(|_| format!("{current_id} の仮想パスを解釈できない。"))?;
    let target = resolved_location(&project, &current_path, offset);
    let result = target.and_then(|location| sources.location(location, encoding));
    Ok(result.filter(|_| sources.still_current([(current.uri, current_id.as_str())])))
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
