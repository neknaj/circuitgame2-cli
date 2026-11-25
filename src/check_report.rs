//! 検査時に得た型付き診断を、コマンド呼び出し側が読める JSON に写す。

use std::{io::Write, ops::Range, path::Path, process::ExitCode};

use circuitgame2_core::{
    project::{
        AbsoluteTarget, AliasConflict, AliasDisposition, CircuitBasisError, CircuitProjectError,
        DefinitionAnalysisError, DefinitionBatchError, DefinitionReferenceReason, ExportError,
        LocatedName, ResolvedUse, UseTargetError,
    },
    syntax::circuit::{CircuitFormError, HeaderError},
};
use serde::Serialize;

#[derive(Serialize)]
struct Location {
    path: String,
    range: Range<usize>,
}

impl Location {
    fn name(value: &LocatedName<'_, '_>) -> Self {
        Self {
            path: value.source.id.components().join("/"),
            range: value.span.clone(),
        }
    }
}

#[derive(Serialize)]
struct Diagnostic {
    severity: &'static str,
    code: &'static str,
    message: String,
    location: Option<Location>,
    related: Vec<Location>,
}

impl Diagnostic {
    fn error(code: &'static str, location: Option<Location>, related: Vec<Location>) -> Self {
        Self {
            severity: "error",
            code,
            message: String::new(),
            location,
            related,
        }
    }

    fn project(error: &CircuitProjectError<'_>) -> Self {
        match error {
            CircuitProjectError::Syntax { id, error, .. } => {
                let (code, span, related) = match error {
                    CircuitFormError::Syntax(error) => ("syntax", &error.span, vec![]),
                    CircuitFormError::Header { kind, span } => {
                        let related = match kind {
                            HeaderError::MixedTestItems {
                                first_head: (start, end),
                            } => vec![Location {
                                path: id.components().join("/"),
                                range: *start..*end,
                            }],
                            _ => vec![],
                        };
                        ("circuit_form", span, related)
                    }
                };
                Self::error(
                    code,
                    Some(Location {
                        path: id.components().join("/"),
                        range: span.clone(),
                    }),
                    related,
                )
            }
            CircuitProjectError::Sources(_) => Self::error("sources", None, Vec::new()),
            CircuitProjectError::UnsupportedLanguage { id, .. } => Self::error(
                "unsupported_language",
                Some(Location {
                    path: id.components().join("/"),
                    range: 0..0,
                }),
                Vec::new(),
            ),
        }
    }

    fn definition(error: &DefinitionBatchError<'_, '_>) -> Self {
        let (code, related) = match &error.reason {
            DefinitionReferenceReason::Missing => ("missing_reference", None),
            DefinitionReferenceReason::MissingPrefix => ("missing_reference_prefix", None),
            DefinitionReferenceReason::Private(site) => {
                ("private_reference", Some(Location::name(site)))
            }
            DefinitionReferenceReason::Namespace(site) => {
                ("namespace_reference", Some(Location::name(site)))
            }
            DefinitionReferenceReason::DeclarationPrefix { source, form } => (
                "declaration_prefix",
                Some(Location {
                    path: source.id.components().join("/"),
                    range: form.name.span.clone(),
                }),
            ),
            DefinitionReferenceReason::WrongDeclaration { source, form } => (
                "wrong_declaration_kind",
                Some(Location {
                    path: source.id.components().join("/"),
                    range: form.name.span.clone(),
                }),
            ),
        };
        Self::error(
            code,
            Some(Location {
                path: error.site.source.id.components().join("/"),
                range: error.site.name.span.clone(),
            }),
            related.into_iter().collect(),
        )
    }

    fn analysis(error: &DefinitionAnalysisError<'_, '_>) -> Self {
        match error {
            DefinitionAnalysisError::Basis(CircuitBasisError::Missing) => {
                Self::error("missing_basis", None, Vec::new())
            }
            DefinitionAnalysisError::Basis(CircuitBasisError::Duplicate { later, first }) => {
                Self::error(
                    "duplicate_basis",
                    Some(Location::name(later)),
                    vec![Location::name(first)],
                )
            }
            DefinitionAnalysisError::Definition { error, .. } => Self::definition(error),
            DefinitionAnalysisError::Alias(conflict) => {
                let (site, related) = match conflict {
                    AliasConflict::BetweenUses { later, first } => (
                        &later.site,
                        Location {
                            path: first.site.source.id.components().join("/"),
                            range: first.site.alias.span.clone(),
                        },
                    ),
                    AliasConflict::Direct {
                        directive,
                        existing,
                    } => (
                        &directive.site,
                        match existing {
                            AbsoluteTarget::Declaration { source, form } => Location {
                                path: source.id.components().join("/"),
                                range: form.name.span.clone(),
                            },
                            AbsoluteTarget::Namespace { introduced_at, .. } => {
                                Location::name(introduced_at)
                            }
                        },
                    ),
                };
                Self::error(
                    "alias_conflict",
                    Some(Location {
                        path: site.source.id.components().join("/"),
                        range: site.alias.span.clone(),
                    }),
                    vec![related],
                )
            }
            DefinitionAnalysisError::UseTarget(error) => match error {
                UseTargetError::Export(ExportError::Conflict(conflict)) => Self::error(
                    "declaration_conflict",
                    Some(Location::name(&conflict.second)),
                    vec![Location::name(&conflict.first)],
                ),
                UseTargetError::Export(ExportError::MissingDeclaration(site)) => {
                    Self::error("missing_export", Some(Location::name(site)), Vec::new())
                }
                UseTargetError::Missing(site) => Self::error(
                    "missing_use_target",
                    Some(Location {
                        path: site.source.id.components().join("/"),
                        range: site.reference.span.clone(),
                    }),
                    Vec::new(),
                ),
                UseTargetError::Private { site, declaration } => Self::error(
                    "private_use_target",
                    Some(Location {
                        path: site.source.id.components().join("/"),
                        range: site.reference.span.clone(),
                    }),
                    vec![Location::name(declaration)],
                ),
            },
        }
    }
}

/// 1回の検査の結果。ソース範囲は各ソースの UTF-8 バイトを基準とする。
#[derive(Serialize)]
pub(super) struct Report<'a> {
    version: u32,
    ok: bool,
    project: Option<&'a str>,
    pub(super) files: Option<usize>,
    diagnostics: Vec<Diagnostic>,
}

impl<'a> Report<'a> {
    pub(super) fn new(project: Option<&'a str>) -> Self {
        Self {
            version: 1,
            ok: false,
            project,
            files: None,
            diagnostics: Vec::new(),
        }
    }

    /// 設定ファイルと、その中で診断が指すバイト範囲を保存する。
    pub(super) fn settings_error(
        &mut self,
        code: &'static str,
        path: &Path,
        span: Option<Range<usize>>,
    ) {
        let location = span.map(|range| Location {
            path: path.display().to_string(),
            range,
        });
        self.diagnostics
            .push(Diagnostic::error(code, location, vec![]));
    }
    pub(super) fn project_error(&mut self, error: &CircuitProjectError<'_>) {
        self.diagnostics.push(Diagnostic::project(error));
    }

    pub(super) fn analysis_error(&mut self, error: &DefinitionAnalysisError<'_, '_>) {
        self.diagnostics.push(Diagnostic::analysis(error));
    }

    pub(super) fn definition_error(&mut self, error: &DefinitionBatchError<'_, '_>) {
        self.diagnostics.push(Diagnostic::definition(error));
    }

    /// 診断と関連位置を、元ソースのバイト範囲として保存する。
    pub(super) fn body_error<'s, 'p: 's, 'text: 's>(
        &mut self,
        code: &'static str,
        location: &LocatedName<'p, 'text>,
        related: impl IntoIterator<Item = &'s LocatedName<'p, 'text>>,
    ) {
        self.diagnostics.push(Diagnostic::error(
            code,
            Some(Location::name(location)),
            related.into_iter().map(Location::name).collect(),
        ));
    }
    pub(super) fn warnings(
        &mut self,
        directives: &[ResolvedUse<'_, '_>],
        dispositions: &[AliasDisposition],
    ) {
        for (alias, disposition) in directives.iter().zip(dispositions) {
            if let AliasDisposition::Duplicate { first } = disposition
                && let Some(first) = directives.get(*first)
            {
                self.diagnostics.push(Diagnostic {
                    severity: "warning",
                    code: "duplicate_use",
                    message: "同じ別名と参照先が重複している。".into(),
                    location: Some(Location {
                        path: alias.site.source.id.components().join("/"),
                        range: alias.site.alias.span.clone(),
                    }),
                    related: vec![Location {
                        path: first.site.source.id.components().join("/"),
                        range: first.site.alias.span.clone(),
                    }],
                });
            }
        }
    }

    /// 終了コードと同じ成否を JSON に出し、端末用メッセージは内容にだけ含める。
    pub(super) fn finish(
        mut self,
        result: Result<ExitCode, String>,
        stdout: &mut impl Write,
    ) -> Result<ExitCode, String> {
        let status = match result {
            Ok(code) => {
                self.ok = code == ExitCode::SUCCESS;
                code
            }
            Err(message) => {
                let error = match self.diagnostics.last_mut() {
                    Some(error) if error.severity == "error" => error,
                    _ => {
                        self.diagnostics
                            .push(Diagnostic::error("check_failed", None, Vec::new()));
                        self.diagnostics.last_mut().unwrap()
                    }
                };
                error.message = message;
                ExitCode::FAILURE
            }
        };
        serde_json::to_writer(&mut *stdout, &self)
            .map_err(|error| format!("検査結果を JSON で出力できない ({error})。"))?;
        writeln!(stdout).map_err(super::output_error)?;
        Ok(status)
    }
}
