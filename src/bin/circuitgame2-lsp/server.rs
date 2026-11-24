//! LSP セッションと、通知順に更新する開いた文書を保持する。

use std::{
    borrow::Cow,
    collections::HashMap,
    io::{self, BufRead, Write},
    process::ExitCode,
};

use circuitgame2_core::editor::{LineIndex, PositionEncoding, TextPosition, apply_source_edit};
use lsp_server::{ErrorCode, Message, Notification, Request, Response};
use lsp_types::{
    DidCloseTextDocumentParams, DidOpenTextDocumentParams, InitializeParams, InitializeResult,
    PositionEncodingKind, PublishDiagnosticsParams, ServerCapabilities, ServerInfo,
    TextDocumentContentChangeEvent, TextDocumentSyncCapability, TextDocumentSyncKind,
    TextDocumentSyncOptions, Uri, VersionedTextDocumentIdentifier,
};

use super::diagnostic;

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
    text: Option<String>,
}

#[derive(Default)]
struct Server {
    state: State,
    encoding: Option<PositionEncoding>,
    documents: HashMap<Uri, Document>,
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
                if let Some(result) = server.notification(notification) {
                    result.write(output)?;
                }
            }
            Message::Response(_) => {}
        }
    }
    Ok(ExitCode::FAILURE)
}

impl Server {
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
        self.state = State::AwaitingInitialized;
        self.encoding = Some(encoding);
        Response::new_ok(
            request.id,
            InitializeResult {
                capabilities: ServerCapabilities {
                    position_encoding: Some(wire_encoding),
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

    fn notification(&mut self, notification: Notification) -> Option<Message> {
        match (self.state, notification.method.as_str()) {
            (State::AwaitingInitialized, "initialized") => {
                self.state = State::Ready;
                None
            }
            (State::Ready, "textDocument/didOpen") => {
                let params: DidOpenTextDocumentParams = decode(notification.params)?;
                let item = params.text_document;
                if item.language_id != "ncg2" || self.documents.contains_key(&item.uri) {
                    return None;
                }
                let document = Document {
                    version: item.version,
                    text: Some(item.text),
                };
                let result = publish(&item.uri, &document, self.encoding?);
                self.documents.insert(item.uri, document);
                Some(result)
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
                document.change(item.version, changes, encoding);
                Some(publish(&item.uri, document, encoding))
            }
            (State::Ready, "textDocument/didClose") => {
                let params: DidCloseTextDocumentParams = decode(notification.params)?;
                let uri = params.text_document.uri;
                self.documents.remove(&uri)?;
                Some(diagnostics_notification(uri, None, Vec::new()))
            }
            _ => None,
        }
    }
}

impl Document {
    /// 同じ通知の変更を前から適用し、最終結果だけを保持する。
    /// 範囲を適用できなかった本文は、次の全文置換で再取得する。
    fn change(
        &mut self,
        version: i32,
        changes: Option<Vec<TextDocumentContentChangeEvent>>,
        encoding: PositionEncoding,
    ) {
        let result = changes
            .ok_or("変更一覧の構造が不正。")
            .and_then(|changes| apply_changes(self.text.as_deref(), changes, encoding));
        self.version = version;
        match result {
            Ok(text) => self.text = Some(text.into_owned()),
            Err(reason) => {
                self.text = None;
                eprintln!("文書版 {version} の変更を適用できない。{reason}");
            }
        }
    }
}

fn apply_changes(
    text: Option<&str>,
    changes: Vec<TextDocumentContentChangeEvent>,
    encoding: PositionEncoding,
) -> Result<Cow<'_, str>, &'static str> {
    let mut current = text.map(Cow::Borrowed);
    for change in changes {
        let Some(range) = change.range else {
            current = Some(Cow::Owned(change.text));
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
        let edited = apply_source_edit(source, start..end, &change.text)
            .map_err(|_| "変更範囲の順序、長さ、または文字境界が不正。")?;
        current = Some(Cow::Owned(edited.as_str().to_owned()));
    }
    current.ok_or("本文を全文置換で受け取る必要がある。")
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

fn publish(uri: &Uri, document: &Document, encoding: PositionEncoding) -> Message {
    let diagnostics = document
        .text
        .as_ref()
        .map(|text| diagnostic::collect(uri, text, encoding))
        .unwrap_or_default();
    diagnostics_notification(uri.clone(), Some(document.version), diagnostics)
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
