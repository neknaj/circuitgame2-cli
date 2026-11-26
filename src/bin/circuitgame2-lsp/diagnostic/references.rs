//! 定義参照の診断を整形し、版をまたいで照合できる診断を保持する。

use std::{collections::BTreeMap, ops::Range};

use circuitgame2_core::{
    editor::PositionEncoding,
    project::{
        CachedAbsoluteReferenceDiagnostic, CircuitProject, DefinitionPosition,
        DefinitionPositionStep, DefinitionReferenceError, DefinitionReferenceReason,
        DefinitionReferenceSite, LocatedName, ReferenceDiagnosticKind, check_use_aliases,
        index_use_targets, validate_circuit_basis,
    },
    syntax::circuit::Head,
};
use lsp_types::{
    Diagnostic, DiagnosticRelatedInformation, DiagnosticSeverity, Location, NumberOrString, Uri,
};
use serde_json::json;

use super::{super::sources::SourceSet, head_name};

type PositionKey = (usize, usize, usize, usize);

/// 元の参照順で保持する、完全修飾名の診断。
pub(crate) type ReferenceCache = BTreeMap<PositionKey, CachedAbsoluteReferenceDiagnostic>;

/// 同じソース集合の構文、宣言、別名を確定できたときに参照診断を返す。
pub(crate) fn collect_references(
    project: &CircuitProject<'_>,
    sources: &SourceSet<'_>,
    encoding: PositionEncoding,
) -> Vec<(Uri, Diagnostic)> {
    refresh_references(project, sources, encoding, ReferenceCache::new(), 0).0
}

/// 新版と候補が一致する診断を再利用し、結果と保存候補を元の参照順で返す。
pub(crate) fn refresh_references(
    project: &CircuitProject<'_>,
    sources: &SourceSet<'_>,
    encoding: PositionEncoding,
    mut previous: ReferenceCache,
    limit: usize,
) -> (Vec<(Uri, Diagnostic)>, ReferenceCache) {
    let mut diagnostics = Vec::new();
    let mut retained = ReferenceCache::new();
    if validate_circuit_basis(project).is_err() {
        return (diagnostics, retained);
    }
    let Ok(targets) = index_use_targets(project) else {
        return (diagnostics, retained);
    };
    let Ok(scope) = check_use_aliases(&targets) else {
        return (diagnostics, retained);
    };
    if previous.is_empty() && limit == 0 {
        diagnostics.extend(
            scope
                .resolve_definitions_with_diagnostics()
                .diagnostics
                .into_iter()
                .filter_map(|error| fresh_diagnostic(error.site, error.reason, sources, encoding)),
        );
        return (diagnostics, retained);
    }

    let mut positions = project.definition_positions();
    loop {
        let at = match positions.step() {
            DefinitionPositionStep::Reference(at) => at,
            DefinitionPositionStep::Advanced => continue,
            DefinitionPositionStep::Finished => break,
        };
        let DefinitionPosition {
            file,
            entry,
            definition,
            reference,
        } = at;
        let key = (file, entry, definition, reference);
        if let Some(cached) = previous.remove(&key) {
            let updated = if scope
                .reuse_absolute_reference_diagnostic(at, &cached)
                .is_some()
            {
                Some(cached)
            } else {
                scope.cache_absolute_reference_diagnostic(at)
            };
            if let Some(updated) = updated {
                diagnostics.extend(stored_diagnostic(&updated, sources, encoding));
                if updated.problem().is_some() && retained.len() < limit {
                    retained.insert(key, updated);
                }
                continue;
            }
        }

        let Err(DefinitionReferenceError::Rejected { site, reason }) =
            scope.resolve_definition_reference(file, entry, definition, reference)
        else {
            continue;
        };
        // 成功した参照や相対名の診断は所有キャッシュに入れない。
        if retained.len() < limit
            && site.name.word.starts_with("::")
            && let Some(cached) = scope.cache_absolute_reference_diagnostic(at)
        {
            retained.insert(key, cached);
        }
        diagnostics.extend(fresh_diagnostic(site, reason, sources, encoding));
    }
    (diagnostics, retained)
}

fn fresh_diagnostic(
    site: DefinitionReferenceSite<'_, '_>,
    reason: DefinitionReferenceReason<'_, '_>,
    sources: &SourceSet<'_>,
    encoding: PositionEncoding,
) -> Option<(Uri, Diagnostic)> {
    let span = site.name.span;
    let primary = sources.location(
        LocatedName {
            source: site.source,
            span: span.clone(),
        },
        encoding,
    )?;
    let (kind, declaration, related) = match reason {
        DefinitionReferenceReason::Missing => (ReferenceDiagnosticKind::Missing, None, None),
        DefinitionReferenceReason::MissingPrefix => {
            (ReferenceDiagnosticKind::MissingPrefix, None, None)
        }
        DefinitionReferenceReason::DeclarationPrefix { source, form } => (
            ReferenceDiagnosticKind::DeclarationPrefix,
            None,
            Some(LocatedName {
                source,
                span: form.name.span.clone(),
            }),
        ),
        DefinitionReferenceReason::Private(location) => {
            (ReferenceDiagnosticKind::Private, None, Some(location))
        }
        DefinitionReferenceReason::Namespace(location) => {
            (ReferenceDiagnosticKind::Namespace, None, Some(location))
        }
        DefinitionReferenceReason::WrongDeclaration { source, form } => (
            ReferenceDiagnosticKind::WrongDeclaration,
            Some(form.head),
            Some(LocatedName {
                source,
                span: form.name.span.clone(),
            }),
        ),
    };
    let related = related.and_then(|location| sources.location(location, encoding));
    render_diagnostic(primary, &span, site.name.word, &kind, declaration, related)
}

fn stored_diagnostic(
    cached: &CachedAbsoluteReferenceDiagnostic,
    sources: &SourceSet<'_>,
    encoding: PositionEncoding,
) -> Option<(Uri, Diagnostic)> {
    let problem = cached.problem()?;
    let primary = sources.cached_location(cached.primary(), encoding)?;
    let related = problem
        .related
        .as_ref()
        .and_then(|item| sources.cached_location(item, encoding));
    render_diagnostic(
        primary,
        &cached.primary().span,
        cached.word(),
        &problem.kind,
        problem.declaration_kind,
        related,
    )
}

fn render_diagnostic(
    primary: Location,
    span: &Range<usize>,
    name: &str,
    kind: &ReferenceDiagnosticKind,
    declaration: Option<Head>,
    related: Option<Location>,
) -> Option<(Uri, Diagnostic)> {
    let (code, message, label) = match kind {
        ReferenceDiagnosticKind::Missing => (
            "MissingReference",
            format!("「{name}」の宣言が見つからない。"),
            None,
        ),
        ReferenceDiagnosticKind::MissingPrefix => (
            "MissingReferencePrefix",
            format!("「{name}」の先頭の名前を、このスコープで解決できない。"),
            None,
        ),
        ReferenceDiagnosticKind::DeclarationPrefix => (
            "ExpectedNamespacePrefix",
            format!("「{name}」の先頭には名前空間が必要。"),
            Some("先頭の名前が指す宣言"),
        ),
        ReferenceDiagnosticKind::Private => (
            "PrivateReference",
            format!("「{name}」は参照元の名前空間に公開されていない。"),
            Some("非公開の宣言"),
        ),
        ReferenceDiagnosticKind::Namespace => (
            "ExpectedDeclaration",
            format!("「{name}」の位置には宣言名が必要。"),
            Some("参照先の名前空間"),
        ),
        ReferenceDiagnosticKind::WrongDeclaration => (
            "WrongDeclarationKind",
            format!(
                "「{name}」は、この位置では使えない {} の宣言を指している。",
                head_name(declaration?)
            ),
            Some("参照先の宣言"),
        ),
    };
    let related_information = related.zip(label).map(|(location, label)| {
        vec![DiagnosticRelatedInformation {
            location,
            message: label.to_owned(),
        }]
    });
    Some((
        primary.uri,
        Diagnostic {
            range: primary.range,
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("NCG2".to_owned()),
            code: Some(NumberOrString::String(code.to_owned())),
            message,
            related_information,
            data: Some(json!({ "byteStart": span.start, "byteEnd": span.end })),
            ..Diagnostic::default()
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::OpenDocument;

    fn input<'a>(uri: &'a Uri, text: &'a str, version: i32) -> SourceSet<'a> {
        SourceSet::single(OpenDocument {
            uri,
            text: Some(text),
            version,
            analysis: None,
        })
        .unwrap()
    }

    #[test]
    fn retains_the_previous_diagnostic_when_its_core_observation_matches() {
        let uri: Uri = "untitled:references.ncg2".parse().unwrap();
        let text = concat!(
            "(using nor) (namespace lib (func hidden (in x) (out x))) ",
            "(func top (in x) (out q) (def q (::lib::hidden x)) (def r (missing x)))",
        );
        let initial = input(&uri, text, 1);
        let project = initial.parse_strict().unwrap();
        let (before, cache) = refresh_references(
            &project,
            &initial,
            PositionEncoding::Utf16,
            ReferenceCache::new(),
            1,
        );
        let old_spelling = cache.values().next().unwrap().word().as_ptr();

        let edited = text.replacen(") (namespace", ")\n(namespace", 1);
        let current = input(&uri, &edited, 2);
        let project = current.parse_strict().unwrap();
        let (after, cache) =
            refresh_references(&project, &current, PositionEncoding::Utf16, cache, 1);
        assert_eq!(cache.values().next().unwrap().word().as_ptr(), old_spelling);
        assert_ne!(after, before);
        assert_eq!(
            after,
            collect_references(&project, &current, PositionEncoding::Utf16)
        );
        let (uncached, retained) =
            refresh_references(&project, &current, PositionEncoding::Utf16, cache, 0);
        assert_eq!(uncached, after);
        assert!(retained.is_empty());
    }
}
