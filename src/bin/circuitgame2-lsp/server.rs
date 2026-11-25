//! LSP セッションと、通知順に更新する開いた文書を保持する。

use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap},
    io::{self, BufRead, Write},
    ops::Range,
    process::ExitCode,
};

use circuitgame2_core::{
    editor::{LineIndex, PositionEncoding, TextPosition, prepare_source_edit},
    ncg2m::file::recovery::{MetaFileRecovery, recover_meta_file_with_provisional},
    project::SourceKind,
    syntax::circuit::CachedCircuitRecovery,
};
use lsp_server::{ErrorCode, Message, Notification, Request, Response};
use lsp_types::{
    CompletionOptions, CompletionParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DocumentSymbolParams, GotoDefinitionParams, HoverParams, InitializeParams, InitializeResult,
    InlayHintParams, OneOf, PositionEncodingKind, PublishDiagnosticsParams, SemanticTokensParams,
    ServerCapabilities, ServerInfo, TextDocumentContentChangeEvent, TextDocumentSyncCapability,
    TextDocumentSyncKind, TextDocumentSyncOptions, Uri, VersionedTextDocumentIdentifier,
};

use super::{
    completion, definition,
    diagnostic::{self, LocalRecovery},
    inlay_hint, navigation,
    sources::{
        self, DiskSyntaxCache, OpenDocument, ProjectKey, STANDALONE_ID, SourcePaths, SourceSet,
    },
    symbols,
};

#[derive(Clone, Copy, Default)]
enum State {
    #[default]
    Start,
    AwaitingInitialized,
    Ready,
    Shutdown,
}

struct Document {
    version: i32,
    kind: SourceKind,
    analysis: Option<DocumentAnalysis>,
    syntax: Vec<lsp_types::Diagnostic>,
    published: Option<(i32, Vec<lsp_types::Diagnostic>)>,
}

enum DocumentAnalysis {
    Full(CachedCircuitRecovery),
    Local {
        text: String,
        recovery: LocalRecovery,
    },
    Functional {
        text: String,
        outline: symbols::FunctionalOutline,
    },
}

impl DocumentAnalysis {
    fn analyze(
        kind: SourceKind,
        uri: &Uri,
        text: String,
        encoding: PositionEncoding,
    ) -> (Self, Vec<lsp_types::Diagnostic>) {
        match kind {
            SourceKind::Circuit => {
                let analysis = CachedCircuitRecovery::analyze_with_build_index(text);
                let diagnostics = diagnostic::collect(uri, &analysis, encoding);
                (Self::Full(analysis), diagnostics)
            }
            SourceKind::Functional => {
                let (recovery, provisional) = recover_meta_file_with_provisional(&text);
                let (outline, diagnostics) = match recovery {
                    MetaFileRecovery::Complete(items) => {
                        (symbols::FunctionalOutline::Complete(items), Vec::new())
                    }
                    MetaFileRecovery::Incomplete {
                        first,
                        syntax,
                        lists,
                    } => {
                        let diagnostics = diagnostic::render_meta_recovery(
                            uri,
                            &text,
                            encoding,
                            &first,
                            &syntax.repairs,
                            &lists,
                        );
                        (
                            symbols::FunctionalOutline::Fragments { lists, provisional },
                            diagnostics,
                        )
                    }
                };
                (Self::Functional { text, outline }, diagnostics)
            }
        }
    }

    fn text(&self) -> &str {
        match self {
            Self::Full(analysis) => analysis.text(),
            Self::Local { text, .. } | Self::Functional { text, .. } => text,
        }
    }

    fn full(&self) -> Option<&CachedCircuitRecovery> {
        match self {
            Self::Full(analysis) => Some(analysis),
            Self::Local { .. } | Self::Functional { .. } => None,
        }
    }
}

struct DocumentUpdate {
    uri: Uri,
    kind: SourceKind,
    encoding: PositionEncoding,
    syntax: Option<Message>,
}

#[derive(Default)]
struct Server {
    state: State,
    encoding: Option<PositionEncoding>,
    documents: HashMap<Uri, Document>,
    inlay_refresh: bool,
    hierarchical_symbols: bool,
    semantic_palette: Option<navigation::SemanticPalette>,
    hint_dependencies: HashMap<Uri, Vec<Uri>>,
    disk_syntax: DiskSyntaxCache,
    next_refresh_id: u64,
}

/// 受信した通知を順に処理し、診断を同じイベント内で確定する。
pub(super) fn run(input: &mut impl BufRead, output: &mut impl Write) -> io::Result<ExitCode> {
    let mut server = Server::default();
    while let Some(message) = Message::read(input)? {
        match message {
            Message::Request(request) => server.request(request).write(output)?,
            Message::Notification(notification) if notification.method == "exit" => {
                return Ok(if matches!(server.state, State::Shutdown) {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::FAILURE
                });
            }
            Message::Notification(notification) => {
                if let Some(change) = server.notification(notification) {
                    if let Some(syntax) = change.syntax {
                        syntax.write(output)?;
                    }
                    // 回路の意味解析は .ncg2 だけで構成したプロジェクトを対象とする。
                    if change.kind == SourceKind::Circuit {
                        if let Some(refresh) = server.refresh_hints(&change.uri) {
                            refresh.write(output)?;
                        }
                        server.publish_changed(change.encoding, output)?;
                    }
                }
            }
            Message::Response(_) => {}
        }
    }
    Ok(ExitCode::FAILURE)
}

impl Server {
    fn circuit_document(&self, uri: &Uri) -> Option<&Document> {
        self.documents
            .get(uri)
            .filter(|doc| doc.kind == SourceKind::Circuit)
    }

    fn request(&mut self, request: Request) -> Message {
        let response = match (self.state, request.method.as_str()) {
            (State::Start, "initialize") => self.initialize(request),
            (State::Start, _) => Response::new_err(
                request.id,
                ErrorCode::ServerNotInitialized as i32,
                "サーバーが初期化されていない。".into(),
            ),
            (State::Ready, "shutdown") => {
                self.state = State::Shutdown;
                Response::new_ok(request.id, ())
            }
            (State::Ready, "textDocument/completion") => self.completion(request),
            (State::Ready, "textDocument/definition") => self.definition(request),
            (State::Ready, "textDocument/documentSymbol") => self.document_symbols(request),
            (State::Ready, "textDocument/hover") => self.hover(request),
            (State::Ready, "textDocument/semanticTokens/full") => self.semantic_tokens(request),
            (State::Ready, "textDocument/inlayHint") => self.circuit_hints(request),
            (State::Ready, method) if method != "initialize" => Response::new_err(
                request.id,
                ErrorCode::MethodNotFound as i32,
                "この LSP 要求には対応していない。".into(),
            ),
            _ => Response::new_err(
                request.id,
                ErrorCode::InvalidRequest as i32,
                "現在のセッションではこの要求を受け付けられない。".into(),
            ),
        };
        response.into()
    }

    fn initialize(&mut self, request: Request) -> Response {
        let parameters: InitializeParams = match serde_json::from_value(request.params) {
            Ok(parameters) => parameters,
            Err(error) => {
                return Response::new_err(
                    request.id,
                    ErrorCode::InvalidParams as i32,
                    error.to_string(),
                );
            }
        };
        let (wire_encoding, encoding) = parameters
            .capabilities
            .general
            .and_then(|general| general.position_encodings)
            .into_iter()
            .flatten()
            .find_map(|wire| match wire.as_str() {
                "utf-8" => Some((wire, PositionEncoding::Utf8)),
                "utf-16" => Some((wire, PositionEncoding::Utf16)),
                "utf-32" => Some((wire, PositionEncoding::Utf32)),
                _ => None,
            })
            .unwrap_or((PositionEncodingKind::UTF16, PositionEncoding::Utf16));
        self.inlay_refresh = parameters
            .capabilities
            .workspace
            .and_then(|workspace| workspace.inlay_hint)
            .is_some_and(|hint| hint.refresh_support == Some(true));
        let document = parameters.capabilities.text_document.as_ref();
        self.hierarchical_symbols = document
            .and_then(|document| document.document_symbol.as_ref())
            .is_some_and(|symbols| symbols.hierarchical_document_symbol_support == Some(true));
        let custom = parameters
            .initialization_options
            .as_ref()
            .and_then(|options| options.get("ncg2SemanticTokens"))
            .and_then(serde_json::Value::as_bool)
            == Some(true);
        self.semantic_palette = document
            .and_then(|document| document.semantic_tokens.as_ref())
            .and_then(|capabilities| navigation::SemanticPalette::new(capabilities, custom));
        self.state = State::AwaitingInitialized;
        self.encoding = Some(encoding);
        Response::new_ok(
            request.id,
            InitializeResult {
                capabilities: ServerCapabilities {
                    position_encoding: Some(wire_encoding),
                    completion_provider: Some(CompletionOptions::default()),
                    definition_provider: Some(OneOf::Left(true)),
                    document_symbol_provider: Some(OneOf::Left(true)),
                    hover_provider: Some(true.into()),
                    semantic_tokens_provider: self
                        .semantic_palette
                        .as_ref()
                        .map(navigation::SemanticPalette::capability),
                    inlay_hint_provider: Some(OneOf::Left(true)),
                    text_document_sync: Some(TextDocumentSyncCapability::Options(
                        TextDocumentSyncOptions {
                            open_close: Some(true),
                            change: Some(TextDocumentSyncKind::INCREMENTAL),
                            ..TextDocumentSyncOptions::default()
                        },
                    )),
                    ..ServerCapabilities::default()
                },
                server_info: Some(ServerInfo {
                    name: "circuitgame2-lsp".into(),
                    version: Some(env!("CARGO_PKG_VERSION").into()),
                }),
            },
        )
    }

    fn circuit_hints(&mut self, request: Request) -> Response {
        let params: InlayHintParams = match serde_json::from_value(request.params) {
            Ok(params) => params,
            Err(error) => {
                return Response::new_err(
                    request.id,
                    ErrorCode::InvalidParams as i32,
                    error.to_string(),
                );
            }
        };
        let positions = text_position(params.range.start).and_then(|start| {
            let end = text_position(params.range.end)?;
            Ok((start, end))
        });
        let (start, end) = match positions {
            Ok(positions) => positions,
            Err(error) => {
                return Response::new_err(
                    request.id,
                    ErrorCode::InvalidParams as i32,
                    error.into(),
                );
            }
        };
        let uri = &params.text_document.uri;
        let (Some(document), Some(encoding)) = (self.circuit_document(uri), self.encoding) else {
            return Response::new_ok(request.id, Vec::<lsp_types::InlayHint>::new());
        };
        let open = self
            .documents
            .iter()
            .map(|(uri, document)| document.open_snapshot(uri));
        let snapshot = inlay_hint::collect(
            document.open_snapshot(uri),
            start,
            end,
            encoding,
            open,
            &self.disk_syntax,
        );
        let hints = match snapshot {
            Some(snapshot) => {
                if self.inlay_refresh {
                    self.hint_dependencies
                        .insert(uri.clone(), snapshot.dependencies);
                }
                snapshot.hints
            }
            None => Vec::new(),
        };
        Response::new_ok(request.id, hints)
    }

    fn completion(&self, request: Request) -> Response {
        let params: CompletionParams = match serde_json::from_value(request.params) {
            Ok(params) => params,
            Err(error) => {
                return Response::new_err(
                    request.id,
                    ErrorCode::InvalidParams as i32,
                    error.to_string(),
                );
            }
        };
        let position = params.text_document_position;
        let uri = &position.text_document.uri;
        let position = match text_position(position.position) {
            Ok(position) => position,
            Err(error) => {
                return Response::new_err(
                    request.id,
                    ErrorCode::InvalidParams as i32,
                    error.into(),
                );
            }
        };
        let (Some(document), Some(encoding)) = (self.circuit_document(uri), self.encoding) else {
            return Response::new_ok(request.id, ());
        };
        let open = self
            .documents
            .iter()
            .map(|(uri, doc)| doc.open_snapshot(uri));
        let result = completion::collect(document.open_snapshot(uri), position, encoding, open);
        Response::new_ok(request.id, result)
    }

    fn hover(&self, request: Request) -> Response {
        let params: HoverParams = match serde_json::from_value(request.params) {
            Ok(params) => params,
            Err(error) => {
                return Response::new_err(
                    request.id,
                    ErrorCode::InvalidParams as i32,
                    error.to_string(),
                );
            }
        };
        let params = params.text_document_position_params;
        let position = match text_position(params.position) {
            Ok(position) => position,
            Err(error) => {
                return Response::new_err(
                    request.id,
                    ErrorCode::InvalidParams as i32,
                    error.into(),
                );
            }
        };
        let uri = &params.text_document.uri;
        let (Some(document), Some(encoding)) = (self.circuit_document(uri), self.encoding) else {
            return Response::new_ok(request.id, ());
        };
        let open = self
            .documents
            .iter()
            .map(|(uri, document)| document.open_snapshot(uri));
        Response::new_ok(
            request.id,
            navigation::hover(
                document.open_snapshot(uri),
                position,
                encoding,
                open,
                &self.disk_syntax,
            ),
        )
    }

    fn semantic_tokens(&self, request: Request) -> Response {
        let params: SemanticTokensParams = match serde_json::from_value(request.params) {
            Ok(params) => params,
            Err(error) => {
                return Response::new_err(
                    request.id,
                    ErrorCode::InvalidParams as i32,
                    error.to_string(),
                );
            }
        };
        let uri = &params.text_document.uri;
        let (Some(document), Some(encoding), Some(palette)) = (
            self.circuit_document(uri),
            self.encoding,
            self.semantic_palette.as_ref(),
        ) else {
            return Response::new_ok(request.id, ());
        };
        let open = self
            .documents
            .iter()
            .map(|(uri, document)| document.open_snapshot(uri));
        Response::new_ok(
            request.id,
            navigation::semantic(
                palette,
                document.open_snapshot(uri),
                encoding,
                open,
                &self.disk_syntax,
            ),
        )
    }

    fn document_symbols(&self, request: Request) -> Response {
        let params: DocumentSymbolParams = match serde_json::from_value(request.params) {
            Ok(params) => params,
            Err(error) => {
                return Response::new_err(
                    request.id,
                    ErrorCode::InvalidParams as i32,
                    error.to_string(),
                );
            }
        };
        let uri = &params.text_document.uri;
        let (Some(document), Some(encoding)) = (self.documents.get(uri), self.encoding) else {
            return Response::new_ok(request.id, ());
        };
        let result = match (document.kind, &document.analysis) {
            (SourceKind::Circuit, _) => symbols::collect(
                document.open_snapshot(uri),
                encoding,
                self.hierarchical_symbols,
            ),
            (SourceKind::Functional, Some(DocumentAnalysis::Functional { text, outline })) => {
                outline.symbols(uri, text, encoding, self.hierarchical_symbols)
            }
            (SourceKind::Functional, _) => None,
        };
        Response::new_ok(request.id, result)
    }

    fn definition(&self, request: Request) -> Response {
        let params: GotoDefinitionParams = match serde_json::from_value(request.params) {
            Ok(params) => params,
            Err(error) => {
                return Response::new_err(
                    request.id,
                    ErrorCode::InvalidParams as i32,
                    error.to_string(),
                );
            }
        };
        let parameters = params.text_document_position_params;
        let uri = &parameters.text_document.uri;
        let position = match text_position(parameters.position) {
            Ok(position) => position,
            Err(error) => {
                return Response::new_err(
                    request.id,
                    ErrorCode::InvalidParams as i32,
                    error.into(),
                );
            }
        };
        let (Some(document), Some(encoding)) = (self.circuit_document(uri), self.encoding) else {
            return Response::new_ok(request.id, ());
        };
        let current = document.open_snapshot(uri);
        let open = self
            .documents
            .iter()
            .map(|(uri, document)| document.open_snapshot(uri));
        match definition::locate(current, position, encoding, open, &self.disk_syntax) {
            Ok(location) => Response::new_ok(request.id, location),
            Err(error) => Response::new_err(request.id, ErrorCode::RequestFailed as i32, error),
        }
    }

    fn refresh_hints(&mut self, uri: &Uri) -> Option<Message> {
        let before = self.hint_dependencies.len();
        self.hint_dependencies.retain(|document, dependencies| {
            document != uri
                && !dependencies
                    .iter()
                    .any(|dependency| sources::same_source_uri(dependency, uri))
        });
        if self.hint_dependencies.len() == before {
            return None;
        }
        let id = format!("ncg2:inlay-refresh:{}", self.next_refresh_id);
        self.next_refresh_id = self.next_refresh_id.wrapping_add(1);
        Some(Request::new(id.into(), "workspace/inlayHint/refresh".into(), ()).into())
    }

    fn notification(&mut self, notification: Notification) -> Option<DocumentUpdate> {
        match (self.state, notification.method.as_str()) {
            (State::AwaitingInitialized, "initialized") => {
                self.state = State::Ready;
                None
            }
            (State::Ready, "textDocument/didOpen") => {
                let params: DidOpenTextDocumentParams = decode(notification.params)?;
                let item = params.text_document;
                let kind = match item.language_id.as_str() {
                    "ncg2" => SourceKind::Circuit,
                    "ncg2m" => SourceKind::Functional,
                    _ => return None,
                };
                if self.documents.contains_key(&item.uri) {
                    return None;
                }
                let encoding = self.encoding?;
                let (analysis, syntax) =
                    DocumentAnalysis::analyze(kind, &item.uri, item.text, encoding);
                let mut document = Document {
                    version: item.version,
                    kind,
                    syntax,
                    analysis: Some(analysis),
                    published: None,
                };
                let syntax = document.publish_syntax(&item.uri);
                self.documents.insert(item.uri.clone(), document);
                Some(DocumentUpdate {
                    uri: item.uri,
                    kind,
                    encoding,
                    syntax,
                })
            }
            (State::Ready, "textDocument/didChange") => {
                let mut parameters = notification.params;
                let item: VersionedTextDocumentIdentifier =
                    decode(parameters.get_mut("textDocument")?.take())?;
                let encoding = self.encoding?;
                let document = self.documents.get_mut(&item.uri)?;
                if item.version <= document.version {
                    return None;
                }
                let changes = parameters
                    .get_mut("contentChanges")
                    .and_then(|changes| decode(changes.take()));
                document.change(&item.uri, item.version, changes, encoding);
                Some(DocumentUpdate {
                    uri: item.uri.clone(),
                    kind: document.kind,
                    encoding,
                    syntax: document.publish_syntax(&item.uri),
                })
            }
            (State::Ready, "textDocument/didClose") => {
                let params: DidCloseTextDocumentParams = decode(notification.params)?;
                let uri = params.text_document.uri;
                let document = self.documents.remove(&uri)?;
                Some(DocumentUpdate {
                    uri: uri.clone(),
                    kind: document.kind,
                    encoding: self.encoding?,
                    syntax: Some(diagnostics_notification(uri, None, Vec::new())),
                })
            }
            _ => None,
        }
    }

    fn publish_changed(
        &mut self,
        encoding: PositionEncoding,
        output: &mut impl Write,
    ) -> io::Result<()> {
        let mut groups =
            BTreeMap::<ProjectKey, (SourcePaths, Vec<(String, OpenDocument<'_>)>)>::new();
        let mut published = Vec::new();
        let mut open_documents: Vec<_> = self.documents.iter().collect();
        open_documents.sort_by(|left, right| left.0.as_str().cmp(right.0.as_str()));
        for (uri, document) in open_documents {
            if document.kind == SourceKind::Functional {
                continue;
            }
            let current = document.open_snapshot(uri);
            match sources::select(uri) {
                Ok(Some(configured)) => {
                    let (project, id, paths) = configured.for_diagnostics();
                    groups
                        .entry(project)
                        .or_insert_with(|| (paths, Vec::new()))
                        .1
                        .push((id, current));
                }
                selection => {
                    let mut diagnostics = document.syntax.clone();
                    if selection.is_ok()
                        && diagnostics.is_empty()
                        && let Ok(snapshot) = SourceSet::single(current)
                        && let Ok(project) = snapshot.parse_strict()
                    {
                        let references =
                            diagnostic::collect_references(&project, &snapshot, encoding);
                        if snapshot.still_current([(uri, STANDALONE_ID)]) {
                            diagnostics.extend(references.into_iter().map(|(_, item)| item));
                        }
                    }
                    write_diagnostics(uri, document, diagnostics, &mut published, output)?;
                }
            }
        }
        for (project, (paths, documents)) in groups {
            let open = self
                .documents
                .iter()
                .map(|(uri, doc)| doc.open_snapshot(uri));
            let mut reports: HashMap<_, _> = documents
                .iter()
                .map(|(_, doc)| (doc.uri.clone(), self.documents[doc.uri].syntax.clone()))
                .collect();
            // 次のプロジェクトの解析を始める前に、照合した診断を送る。
            if let Ok(snapshot) = SourceSet::load_group(project, paths, &documents, open)
                && let Ok(project) = snapshot.parse_strict_with_disk_cache(Some(&self.disk_syntax))
            {
                let references = diagnostic::collect_references(&project, &snapshot, encoding);
                let staged = snapshot.stage_disk_indexes(&self.disk_syntax);
                let members = documents.iter().map(|(id, doc)| (doc.uri, id.as_str()));
                if snapshot.still_current(members) {
                    self.disk_syntax.extend(staged);
                    for (uri, item) in references {
                        if let Some(diagnostics) = reports.get_mut(&uri) {
                            diagnostics.push(item);
                        }
                    }
                }
            }
            for (_, doc) in documents {
                let diagnostics = reports.remove(doc.uri).unwrap();
                write_diagnostics(
                    doc.uri,
                    &self.documents[doc.uri],
                    diagnostics,
                    &mut published,
                    output,
                )?;
            }
        }
        for (uri, version, diagnostics) in published {
            self.documents.get_mut(&uri).unwrap().published = Some((version, diagnostics));
        }
        Ok(())
    }
}

fn write_diagnostics(
    uri: &Uri,
    document: &Document,
    diagnostics: Vec<lsp_types::Diagnostic>,
    published: &mut Vec<(Uri, i32, Vec<lsp_types::Diagnostic>)>,
    output: &mut impl Write,
) -> io::Result<()> {
    let version = document.version;
    if document
        .published
        .as_ref()
        .is_some_and(|(old_version, previous)| *old_version == version && *previous == diagnostics)
    {
        return Ok(());
    }
    diagnostics_notification(uri.clone(), Some(version), diagnostics.clone()).write(output)?;
    published.push((uri.clone(), version, diagnostics));
    Ok(())
}

impl Document {
    fn publish_syntax(&mut self, uri: &Uri) -> Option<Message> {
        if self.kind == SourceKind::Circuit
            && self.syntax.is_empty()
            && self
                .published
                .as_ref()
                .is_none_or(|(_, diagnostics)| diagnostics.is_empty())
        {
            return None;
        }
        self.published = Some((self.version, self.syntax.clone()));
        Some(diagnostics_notification(
            uri.clone(),
            Some(self.version),
            self.syntax.clone(),
        ))
    }

    fn open_snapshot<'a>(&'a self, uri: &'a Uri) -> OpenDocument<'a> {
        OpenDocument {
            uri,
            text: self.analysis.as_ref().map(DocumentAnalysis::text),
            analysis: self.analysis.as_ref().and_then(DocumentAnalysis::full),
            version: self.version,
        }
    }

    /// 同じ通知の変更を順に適用し、最終本文と診断を新しい文書バージョンで保持する。
    /// 範囲を適用できなかった本文は、次の全文置換で再取得する。
    fn change(
        &mut self,
        uri: &Uri,
        version: i32,
        changes: Option<Vec<TextDocumentContentChangeEvent>>,
        encoding: PositionEncoding,
    ) {
        let previous_version = self.version;
        let prior = self.analysis.take();
        let result = changes.ok_or("変更一覧の構造が不正。").and_then(|changes| {
            apply_changes(
                prior.as_ref().map(DocumentAnalysis::text),
                changes,
                encoding,
            )
        });
        self.version = version;
        let applied = match result {
            Ok(applied) => applied,
            Err(reason) => {
                self.syntax.clear();
                eprintln!("文書版 {version} の変更を適用できない。{reason}");
                return;
            }
        };
        if prior
            .as_ref()
            .is_some_and(|old| old.text() == applied.text.as_ref())
        {
            self.analysis = prior;
            return;
        }
        let AppliedChanges { text, edit_range } = applied;
        let text = text.into_owned();
        let local = match prior {
            Some(DocumentAnalysis::Local { recovery, .. }) => Some(recovery),
            Some(DocumentAnalysis::Full(old)) => edit_range.and_then(|range| {
                LocalRecovery::remember(&old, sources::core_revision(previous_version), range)
            }),
            Some(DocumentAnalysis::Functional { .. }) | None => None,
        };
        if let Some(recovery) = local
            && let Some(syntax) =
                recovery.collect(uri, &text, sources::core_revision(version), encoding)
        {
            self.syntax = syntax;
            self.analysis = Some(DocumentAnalysis::Local { text, recovery });
        } else {
            let (analysis, syntax) = DocumentAnalysis::analyze(self.kind, uri, text, encoding);
            self.syntax = syntax;
            self.analysis = Some(analysis);
        }
    }
}

struct AppliedChanges<'a> {
    text: Cow<'a, str>,
    // 編集前の本文上で範囲編集が収まる範囲。全文置換を含む場合は使わない。
    edit_range: Option<Range<usize>>,
}

fn apply_changes(
    text: Option<&str>,
    changes: Vec<TextDocumentContentChangeEvent>,
    encoding: PositionEncoding,
) -> Result<AppliedChanges<'_>, &'static str> {
    let mut current = text.map(Cow::Borrowed);
    let mut original_len = text.map(str::len);
    let mut edit_range: Option<Range<usize>> = None;
    for change in changes {
        let Some(range) = change.range else {
            current = Some(Cow::Owned(change.text));
            original_len = None;
            edit_range = None;
            continue;
        };
        let source = current
            .as_deref()
            .ok_or("本文を全文置換で受け取る必要がある。")?;
        let lines = LineIndex::new(source);
        let start = lines
            .position_to_byte(text_position(range.start)?, encoding)
            .ok_or("変更範囲の始点が文字境界または既存の行を指していない。")?;
        let end = lines
            .position_to_byte(text_position(range.end)?, encoding)
            .ok_or("変更範囲の終点が文字境界または既存の行を指していない。")?;
        let parts = prepare_source_edit(source, start..end, &change.text)
            .map_err(|_| "変更範囲の順序、長さ、または文字境界が不正。")?;
        let mut edited = String::with_capacity(parts.byte_len);
        edited.push_str(parts.prefix);
        edited.push_str(parts.replacement);
        edited.push_str(parts.suffix);
        if let Some(original_len) = original_len {
            let suffix = source.len() - end;
            // 後の編集位置は変更後の本文を指す。前後に残る未変更のバイト数で元の範囲を広げる。
            edit_range = Some(match edit_range {
                None => start..end,
                Some(prior) => {
                    let unchanged_suffix = (original_len - prior.end).min(suffix);
                    prior.start.min(start)..original_len - unchanged_suffix
                }
            });
        }
        current = Some(Cow::Owned(edited));
    }
    let text = current.ok_or("本文を全文置換で受け取る必要がある。")?;
    Ok(AppliedChanges { text, edit_range })
}

fn text_position(position: lsp_types::Position) -> Result<TextPosition, &'static str> {
    const MAX: u32 = i32::MAX as u32;
    if position.line > MAX || position.character > MAX {
        return Err("行番号または列番号が LSP の整数上限を越えている。");
    }
    Ok(TextPosition {
        line: position.line as usize,
        column: position.character as usize,
    })
}

fn decode<T: serde::de::DeserializeOwned>(parameters: serde_json::Value) -> Option<T> {
    match serde_json::from_value(parameters) {
        Ok(parameters) => Some(parameters),
        Err(error) => {
            eprintln!("LSP 通知の引数を読み取れない。{error}");
            None
        }
    }
}

fn diagnostics_notification(
    uri: Uri,
    version: Option<i32>,
    diagnostics: Vec<lsp_types::Diagnostic>,
) -> Message {
    Notification::new(
        "textDocument/publishDiagnostics".into(),
        PublishDiagnosticsParams::new(uri, diagnostics, version),
    )
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn replacement(source: &str, old: &str, new: &str) -> TextDocumentContentChangeEvent {
        let start = source.find(old).unwrap();
        let column = |byte| source[..byte].encode_utf16().count() as u32;
        TextDocumentContentChangeEvent {
            range: Some(lsp_types::Range::new(
                lsp_types::Position::new(0, column(start)),
                lsp_types::Position::new(0, column(start + old.len())),
            )),
            range_length: None,
            text: new.to_owned(),
        }
    }

    #[test]
    fn local_syntax_diagnostics_match_full_recovery_across_document_revisions() {
        let uri: Uri = "untitled:incremental.ncg2".parse().unwrap();
        let encoding = PositionEncoding::Utf16;
        let original =
            r#"(using nor)(first "🐈") (test demo (reset) (case)) (using) (x "bad\q") (unfinished"#;
        let analysis = CachedCircuitRecovery::analyze(original.to_owned());
        let mut document = Document {
            kind: SourceKind::Circuit,
            version: -4,
            syntax: diagnostic::collect(&uri, &analysis, encoding),
            analysis: Some(DocumentAnalysis::Full(analysis)),
            published: None,
        };
        let name = replacement(original, "first", "first-long");
        let renamed = original.replace("first", "first-long");
        let emoji = replacement(&renamed, "🐈", "🦊漢");
        let after_first = renamed.replace("🐈", "🦊漢");
        document.change(&uri, -3, Some(vec![name, emoji]), encoding);
        assert!(matches!(
            &document.analysis,
            Some(DocumentAnalysis::Local { .. })
        ));
        assert_eq!(
            document.open_snapshot(&uri).text,
            Some(after_first.as_str())
        );
        let expect = |text: &str| {
            diagnostic::collect(
                &uri,
                &CachedCircuitRecovery::analyze(text.to_owned()),
                encoding,
            )
        };
        assert_eq!(document.syntax, expect(&after_first));

        let first = replacement(&after_first, "first-long", "f");
        let intermediate = after_first.replace("first-long", "f");
        let second = replacement(&intermediate, "漢", "あいう");
        let after_both = intermediate.replace("漢", "あいう");
        document.change(&uri, -2, Some(vec![first, second]), encoding);
        assert!(matches!(
            &document.analysis,
            Some(DocumentAnalysis::Local { .. })
        ));
        assert_eq!(document.open_snapshot(&uri).text, Some(after_both.as_str()));
        assert_eq!(document.syntax, expect(&after_both));

        let mut server = Server {
            state: State::Ready,
            encoding: Some(encoding),
            ..Server::default()
        };
        server.documents.insert(uri.clone(), document);
        let stale = Notification::new(
            "textDocument/didChange".to_owned(),
            serde_json::json!({
                "textDocument": {"uri": uri, "version": -3},
                "contentChanges": [{"text": ")"}],
            }),
        );
        assert!(server.notification(stale).is_none());
        let document = server.documents.get_mut(&uri).unwrap();
        assert_eq!(document.version, -2);
        assert_eq!(document.open_snapshot(&uri).text, Some(after_both.as_str()));

        document.change(
            &uri,
            -1,
            Some(vec![replacement(&after_both, "nor", "nand")]),
            encoding,
        );
        assert!(matches!(
            &document.analysis,
            Some(DocumentAnalysis::Full(_))
        ));
        let fallback_text = after_both.replace("nor", "nand");
        assert_eq!(
            document.open_snapshot(&uri).text,
            Some(fallback_text.as_str())
        );
        assert_eq!(document.syntax, expect(&fallback_text));
        let Some(Message::Notification(published)) = document.publish_syntax(&uri) else {
            panic!("expected syntax diagnostics");
        };
        assert_eq!(published.params["version"], -1);
        assert_eq!(published.params["uri"], uri.as_str());

        let extra_close = "(using nor))(func gate (in x y) (out z) (def z (nor x y)))";
        document.change(
            &uri,
            0,
            Some(vec![TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: extra_close.to_owned(),
            }]),
            encoding,
        );
        assert!(!document.syntax.is_empty());
        let change = replacement(extra_close, "))(", ")(");
        let complete = extra_close.replace("))(", ")(");
        document.change(&uri, 1, Some(vec![change]), encoding);
        assert!(matches!(
            &document.analysis,
            Some(DocumentAnalysis::Full(_))
        ));
        assert_eq!(document.open_snapshot(&uri).text, Some(complete.as_str()));
        assert!(document.syntax.is_empty());
        let Some(Message::Notification(cleared)) = document.publish_syntax(&uri) else {
            panic!("expected cleared diagnostics");
        };
        assert_eq!(cleared.params["version"], 1);
        assert_eq!(cleared.params["diagnostics"], serde_json::json!([]));
    }
}
