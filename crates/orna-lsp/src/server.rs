//! The LSP server loop and request dispatch.
//!
//! The server is synchronous and single-threaded. Compiler checks for one
//! document are fast, so no worker pool is needed for the first version.

use std::collections::HashMap;
use std::io::{self, BufReader};
use std::thread::{self, JoinHandle};

use crate::analysis::{self, EditorParse as Parse};
use crate::documents::{Document, PositionMapper};
use crate::{inlay, semantic};
use lsp_server::{Connection, ErrorCode, Message, Notification, Request, Response, ResponseError};
use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOptions, CodeActionOrCommand, CodeActionParams,
    CodeActionProviderCapability, CompletionOptions, CompletionParams, CompletionResponse,
    Diagnostic, DiagnosticOptions, DiagnosticServerCapabilities, DocumentDiagnosticParams,
    DocumentDiagnosticReport, DocumentSymbolParams, DocumentSymbolResponse,
    FullDocumentDiagnosticReport, GotoDefinitionParams, GotoDefinitionResponse, Hover, HoverParams,
    HoverProviderCapability, InitializeParams, InlayHintOptions, InlayHintParams,
    InlayHintServerCapabilities, NumberOrString, OneOf, Position, PositionEncodingKind,
    PrepareRenameResponse, PublishDiagnosticsParams, Range, ReferenceParams,
    RelatedFullDocumentDiagnosticReport, RenameOptions, RenameParams, SemanticTokens,
    SemanticTokensFullOptions, SemanticTokensLegend, SemanticTokensOptions, SemanticTokensParams,
    SemanticTokensRangeParams, SemanticTokensServerCapabilities, ServerCapabilities,
    SignatureHelpOptions, TextDocumentContentChangeEvent, TextDocumentSyncCapability,
    TextDocumentSyncKind, TextDocumentSyncOptions, TextDocumentSyncSaveOptions, TextEdit, Uri,
    WorkspaceEdit,
};

/// Transport threads for the server's standard input and output streams.
///
/// `lsp_server::Connection::stdio` joins its reader before its writer. That
/// ordering is correct after a normal `exit` notification, but a rejected
/// initialize has no valid shutdown sequence and can leave the reader blocked
/// on client input. Keep the handles here so the error path can flush the
/// writer without waiting for the reader.
struct StdioIoThreads {
    reader: JoinHandle<io::Result<()>>,
    writer: JoinHandle<io::Result<()>>,
}

impl StdioIoThreads {
    fn join(self) -> io::Result<()> {
        join_io_thread(self.reader)?;
        join_io_thread(self.writer)
    }

    fn join_writer_after_error(self) -> io::Result<()> {
        drop(self.reader);
        join_io_thread(self.writer)
    }
}

fn join_io_thread(thread: JoinHandle<io::Result<()>>) -> io::Result<()> {
    match thread.join() {
        Ok(result) => result,
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

/// Creates a framed stdio transport with independently joinable reader and
/// writer threads.
fn stdio_connection() -> (Connection, StdioIoThreads) {
    let (connection, transport) = Connection::memory();
    let reader_sender = transport.sender;
    let writer_receiver = transport.receiver;

    let reader = thread::Builder::new()
        .name("OrnaLspReader".to_owned())
        .spawn(move || {
            let stdin = io::stdin();
            let mut stdin = BufReader::new(stdin.lock());
            while let Some(message) = Message::read(&mut stdin)? {
                let is_exit = matches!(
                    &message,
                    Message::Notification(notification) if notification.method == "exit"
                );
                if reader_sender.send(message).is_err() || is_exit {
                    break;
                }
            }
            Ok(())
        })
        .expect("spawn Orna LSP reader");

    let writer = thread::Builder::new()
        .name("OrnaLspWriter".to_owned())
        .spawn(move || {
            let stdout = io::stdout();
            let mut stdout = stdout.lock();
            while let Ok(message) = writer_receiver.recv() {
                message.write(&mut stdout)?;
            }
            Ok(())
        })
        .expect("spawn Orna LSP writer");

    (connection, StdioIoThreads { reader, writer })
}

/// Shared state across requests and notifications.
struct ServerState {
    documents: HashMap<Uri, Document>,
}

impl ServerState {
    fn new() -> Self {
        Self {
            documents: HashMap::new(),
        }
    }

    fn document(&self, uri: &Uri) -> Option<&Document> {
        self.documents.get(uri)
    }
}

/// Runs the server until the client exits.
pub fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (connection, io_threads) = stdio_connection();
    let capabilities = server_capabilities();
    if let Err(error) = initialize(&connection, capabilities) {
        // A failed initialize handshake has no valid LSP shutdown sequence.
        // Drop the connection, flush the response, and detach the blocked
        // reader. The process exits after the writer has finished.
        drop(connection);
        io_threads.join_writer_after_error()?;
        return Err(error);
    }
    let mut state = ServerState::new();

    for message in &connection.receiver {
        match message {
            Message::Request(request) => {
                if connection.handle_shutdown(&request)? {
                    break;
                }
                handle_request(&mut state, &connection, request);
            }
            Message::Notification(notification) => {
                if handle_notification(&mut state, &connection, notification) {
                    break;
                }
            }
            Message::Response(_) => {}
        }
    }

    // Drop the connection first: the writer thread terminates only when its
    // channel sender is gone.
    drop(connection);
    io_threads.join()?;
    Ok(())
}

/// Negotiates the position encoding before completing the initialize handshake.
///
/// `PositionMapper` only understands UTF-16 positions. The LSP defaults to
/// UTF-16 when the client omits `general.positionEncodings`, but an explicit
/// list that excludes UTF-16 cannot be silently accepted.
fn initialize(
    connection: &Connection,
    capabilities: ServerCapabilities,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (request_id, raw_params) = connection.initialize_start()?;
    let params: InitializeParams = serde_json::from_value(raw_params)?;

    if let Some(position_encodings) = params
        .capabilities
        .general
        .and_then(|general| general.position_encodings)
        && !position_encodings
            .iter()
            .any(|encoding| encoding == &PositionEncodingKind::UTF16)
    {
        let offered = position_encodings
            .iter()
            .map(PositionEncodingKind::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        let message = format!(
            "unsupported position encoding: orna-lsp supports UTF-16 positions, but the client offered only [{offered}]"
        );
        let _ = connection.sender.send(Message::Response(Response::new_err(
            request_id,
            ErrorCode::InvalidParams as i32,
            message.clone(),
        )));
        return Err(message.into());
    }

    connection.initialize_finish(
        request_id,
        serde_json::json!({
            "capabilities": capabilities,
        }),
    )?;
    Ok(())
}

/// Returns the capabilities advertised during initialization.
fn server_capabilities() -> ServerCapabilities {
    ServerCapabilities {
        position_encoding: Some(PositionEncodingKind::UTF16),
        text_document_sync: Some(TextDocumentSyncCapability::Options(
            TextDocumentSyncOptions {
                open_close: Some(true),
                change: Some(TextDocumentSyncKind::INCREMENTAL),
                will_save: None,
                will_save_wait_until: None,
                save: Some(TextDocumentSyncSaveOptions::Supported(true)),
            },
        )),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        signature_help_provider: Some(SignatureHelpOptions {
            trigger_characters: Some(vec!["(".to_owned(), ",".to_owned()]),
            retrigger_characters: Some(vec![",".to_owned()]),
            work_done_progress_options: Default::default(),
        }),
        code_action_provider: Some(CodeActionProviderCapability::Options(CodeActionOptions {
            code_action_kinds: Some(vec![CodeActionKind::QUICKFIX]),
            resolve_provider: Some(false),
            ..CodeActionOptions::default()
        })),
        definition_provider: Some(OneOf::Left(true)),
        references_provider: Some(OneOf::Left(true)),
        rename_provider: Some(OneOf::Right(RenameOptions {
            prepare_provider: Some(true),
            work_done_progress_options: Default::default(),
        })),
        document_symbol_provider: Some(OneOf::Left(true)),
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec![".".to_owned(), ":".to_owned()]),
            ..CompletionOptions::default()
        }),
        workspace_symbol_provider: Some(OneOf::Left(true)),
        semantic_tokens_provider: Some(SemanticTokensServerCapabilities::SemanticTokensOptions(
            SemanticTokensOptions {
                legend: SemanticTokensLegend {
                    token_types: semantic::legend(),
                    token_modifiers: semantic::modifiers(),
                },
                range: Some(true),
                full: Some(SemanticTokensFullOptions::Bool(true)),
                work_done_progress_options: Default::default(),
            },
        )),
        inlay_hint_provider: Some(OneOf::Right(InlayHintServerCapabilities::Options(
            InlayHintOptions {
                resolve_provider: Some(false),
                ..InlayHintOptions::default()
            },
        ))),
        diagnostic_provider: Some(DiagnosticServerCapabilities::Options(DiagnosticOptions {
            identifier: None,
            inter_file_dependencies: false,
            workspace_diagnostics: false,
            work_done_progress_options: Default::default(),
        })),
        ..ServerCapabilities::default()
    }
}

/// Handles one client request and sends its response.
fn handle_request(state: &mut ServerState, connection: &Connection, request: Request) {
    let method = request.method.clone();
    let id = request.id.clone();
    let result = match method.as_str() {
        "textDocument/hover" => request_hover(state, request),
        "textDocument/signatureHelp" => request_signature_help(state, request),
        "textDocument/codeAction" => request_code_actions(state, request),
        "textDocument/definition" => request_definition(state, request),
        "textDocument/references" => request_references(state, request),
        "textDocument/prepareRename" => request_prepare_rename(state, request),
        "textDocument/rename" => request_rename(state, request),
        "textDocument/documentSymbol" => request_document_symbols(state, request),
        "textDocument/semanticTokens/full" => request_semantic_tokens_full(state, request),
        "textDocument/semanticTokens/range" => request_semantic_tokens_range(state, request),
        "textDocument/inlayHint" => request_inlay_hints(state, request),
        "textDocument/completion" => request_completion(state, request),
        "workspace/symbol" => request_workspace_symbols(state, request),
        "textDocument/diagnostic" => request_document_diagnostic(state, request),
        _ => {
            let _ = connection.sender.send(Message::Response(Response {
                id,
                response_result: Err(ResponseError {
                    code: ErrorCode::MethodNotFound as i32,
                    message: format!("unknown method {method}"),
                    data: None,
                }),
            }));
            return;
        }
    };
    let _ = connection.sender.send(Message::Response(Response {
        id,
        response_result: result.map_err(|error| ResponseError {
            code: ErrorCode::InternalError as i32,
            message: error.to_string(),
            data: None,
        }),
    }));
}

/// Handles one client notification. Returns true when the server must exit.
fn handle_notification(
    state: &mut ServerState,
    connection: &Connection,
    notification: Notification,
) -> bool {
    let method = notification.method.clone();
    match method.as_str() {
        "exit" => true,
        "initialized" => false,
        "textDocument/didOpen" => {
            let Ok(params) =
                serde_json::from_value::<lsp_types::DidOpenTextDocumentParams>(notification.params)
            else {
                return false;
            };
            let uri = params.text_document.uri.clone();
            let document = Document::new(
                uri.clone(),
                params.text_document.text,
                params.text_document.version,
            );
            state.documents.insert(uri.clone(), document);
            publish_diagnostics(state, connection, &uri);
            false
        }
        "textDocument/didChange" => {
            let Ok(params) = serde_json::from_value::<lsp_types::DidChangeTextDocumentParams>(
                notification.params,
            ) else {
                return false;
            };
            let uri = params.text_document.uri.clone();
            let Some(document) = state.documents.get_mut(&uri) else {
                return false;
            };
            if params.text_document.version <= document.version {
                return false;
            }
            let Some(text) = apply_content_changes(&document.text, &params.content_changes) else {
                return false;
            };
            document.text = text;
            document.version = params.text_document.version;
            publish_diagnostics(state, connection, &uri);
            false
        }
        "textDocument/didSave" => {
            if let Ok(params) =
                serde_json::from_value::<lsp_types::DidSaveTextDocumentParams>(notification.params)
            {
                publish_diagnostics(state, connection, &params.text_document.uri);
            }
            false
        }
        "textDocument/didClose" => {
            if let Ok(params) =
                serde_json::from_value::<lsp_types::DidCloseTextDocumentParams>(notification.params)
            {
                let uri = params.text_document.uri;
                state.documents.remove(&uri);
                let _ = connection.sender.send(Message::Notification(Notification {
                    method: "textDocument/publishDiagnostics".to_owned(),
                    params: serde_json::to_value(PublishDiagnosticsParams {
                        uri,
                        diagnostics: Vec::new(),
                        version: None,
                    })
                    .expect("serialisable diagnostics"),
                }));
            }
            false
        }
        _ => false,
    }
}

/// Applies one ordered `didChange` batch to a candidate copy of the current
/// source. Ranged changes use UTF-16 positions against the text produced by
/// all preceding changes in the batch, as required by LSP.
fn apply_content_changes(
    source: &str,
    changes: &[TextDocumentContentChangeEvent],
) -> Option<String> {
    let mut candidate = source.to_owned();
    for change in changes {
        let Some(range) = change.range else {
            candidate = change.text.clone();
            continue;
        };
        let mapper = PositionMapper::new(&candidate);
        let start = mapper.byte_offset(range.start);
        let end = mapper.byte_offset(range.end);
        if start > end || !candidate.is_char_boundary(start) || !candidate.is_char_boundary(end) {
            return None;
        }
        candidate.replace_range(start..end, &change.text);
    }
    Some(candidate)
}

/// Recomputes and publishes the diagnostics for one document.
fn publish_diagnostics(state: &ServerState, connection: &Connection, uri: &Uri) {
    let Some(document) = state.document(uri) else {
        return;
    };
    let mapper = PositionMapper::new(&document.text);
    let diagnostics = analysis::check_document(document, &mapper);
    let params = PublishDiagnosticsParams {
        uri: uri.clone(),
        diagnostics,
        version: Some(document.version),
    };
    let _ = connection.sender.send(Message::Notification(Notification {
        method: "textDocument/publishDiagnostics".to_owned(),
        params: serde_json::to_value(params).expect("serialisable diagnostics"),
    }));
}

/// Parses one document with its current mapper.
fn parse_document(document: &Document) -> (Parse, PositionMapper<'_>) {
    let mapper = PositionMapper::new(&document.text);
    let parse = analysis::parse_document(document);
    (parse, mapper)
}

fn request_hover(
    state: &mut ServerState,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) = request.extract::<HoverParams>("textDocument/hover")?;
    let uri = params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;
    let Some((document, _, selected_start)) = combined_workspace(&state.documents, &uri) else {
        return Ok(serde_json::Value::Null);
    };
    let (parse, mapper) = parse_document(&document);
    let Some(position) =
        workspace_position(&document, selected_start, position, &state.documents, &uri)
    else {
        return Ok(serde_json::Value::Null);
    };
    let hover: Option<Hover> = analysis::hover(&document, &parse, position, &mapper);
    Ok(serde_json::to_value(hover)?)
}

fn request_signature_help(
    state: &mut ServerState,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) =
        request.extract::<lsp_types::SignatureHelpParams>("textDocument/signatureHelp")?;
    let uri = params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;
    let Some((document, _, selected_start)) = combined_workspace(&state.documents, &uri) else {
        return Ok(serde_json::Value::Null);
    };
    let (parse, mapper) = parse_document(&document);
    let Some(position) =
        workspace_position(&document, selected_start, position, &state.documents, &uri)
    else {
        return Ok(serde_json::Value::Null);
    };
    let help = analysis::signature_help(&document, &parse, position, &mapper);
    Ok(serde_json::to_value(help)?)
}

fn request_code_actions(
    state: &mut ServerState,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) = request.extract::<CodeActionParams>("textDocument/codeAction")?;
    let uri = params.text_document.uri;
    if !uri.as_str().ends_with(".orna") || !requests_quickfix(params.context.only.as_deref()) {
        return Ok(serde_json::to_value(Vec::<CodeActionOrCommand>::new())?);
    }
    let Some(document) = state.document(&uri) else {
        return Ok(serde_json::to_value(Vec::<CodeActionOrCommand>::new())?);
    };
    let mapper = PositionMapper::new(&document.text);
    let parse = analysis::parse_document(document);
    let diagnostics = analysis::check_document(document, &mapper);
    let request_start = mapper.byte_offset(params.range.start);
    let request_end = mapper.byte_offset(params.range.end);
    let mut actions = Vec::new();
    for diagnostic in diagnostics {
        let start = mapper.byte_offset(diagnostic.range.start);
        let end = mapper.byte_offset(diagnostic.range.end);
        if start > request_end
            || request_start > end
            || !matches_missing_semicolon_diagnostic(&diagnostic)
            || (!params.context.diagnostics.is_empty()
                && !params.context.diagnostics.iter().any(|requested| {
                    requested.range == diagnostic.range
                        && requested.code == diagnostic.code
                        && requested.message == diagnostic.message
                }))
        {
            continue;
        }
        let Some(edit_position) =
            missing_semicolon_position(document, &parse, &diagnostic, &mapper)
        else {
            continue;
        };
        actions.push(CodeActionOrCommand::CodeAction(CodeAction {
            title: "Insert missing `;`".to_owned(),
            kind: Some(CodeActionKind::QUICKFIX),
            diagnostics: Some(vec![diagnostic]),
            edit: Some(WorkspaceEdit {
                changes: Some(HashMap::from([(
                    uri.clone(),
                    vec![TextEdit {
                        range: Range::new(edit_position, edit_position),
                        new_text: ";".to_owned(),
                    }],
                )])),
                document_changes: None,
                change_annotations: None,
            }),
            command: None,
            is_preferred: Some(true),
            disabled: None,
            data: None,
        }));
    }
    Ok(serde_json::to_value(actions)?)
}

fn requests_quickfix(only: Option<&[CodeActionKind]>) -> bool {
    only.map_or(true, |kinds| {
        kinds
            .iter()
            .any(|kind| kind.as_str() == CodeActionKind::QUICKFIX.as_str())
    })
}

fn matches_missing_semicolon_diagnostic(diagnostic: &Diagnostic) -> bool {
    let is_parse_002 = matches!(
        diagnostic.code.as_ref(),
        Some(NumberOrString::String(code)) if code == "ORNA-PARSE-002"
    );
    is_parse_002
        && matches!(
            diagnostic.message.as_str(),
            "dimension declarations require `;`"
                | "unit declarations require `;`"
                | "assertion statements require `;`"
                | "let statements require `;`"
                | "use declarations require `;`"
                | "expected `;` after function expression"
                | "implementation static properties require `;`"
                | "expected `;` after let statement"
                | "expected `;` after control statement"
                | "expected `;` after expression statement"
                | "expected `;` after table assertion"
                | "expected `;` after static property"
                | "protocol functions require `;`"
                | "expected `;` after type assertion"
        )
}

fn missing_semicolon_position(
    document: &Document,
    parse: &Parse,
    diagnostic: &Diagnostic,
    mapper: &PositionMapper<'_>,
) -> Option<Position> {
    let diagnostic_byte = mapper.byte_offset(diagnostic.range.start);
    let previous = orna_syntax_v1::lex(&document.text)
        .ok()?
        .into_iter()
        .filter(|token| {
            !matches!(&token.kind, orna_syntax_v1::TokenKind::Eof)
                && token.span.end <= diagnostic_byte
        })
        .max_by_key(|token| token.span.end)?;
    if matches!(&previous.kind, orna_syntax_v1::TokenKind::Punct(";")) {
        return None;
    }

    let mut repaired_source = document.text.clone();
    repaired_source.insert_str(previous.span.end, ";");
    let repaired_document = Document::new(document.uri.clone(), repaired_source, document.version);
    let repaired_parse = analysis::parse_document(&repaired_document);
    let diagnostic_count = |parse: &Parse| {
        parse
            .diagnostics
            .iter()
            .filter(|item| item.code == "ORNA-PARSE-002" && item.message == diagnostic.message)
            .count()
    };
    if repaired_parse.diagnostics.len() >= parse.diagnostics.len()
        || diagnostic_count(&repaired_parse) >= diagnostic_count(parse)
    {
        return None;
    }
    Some(mapper.position(previous.span.end))
}

fn request_definition(
    state: &mut ServerState,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) = request.extract::<GotoDefinitionParams>("textDocument/definition")?;
    let uri = params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;
    let Some((document, segments, selected_start)) = combined_workspace(&state.documents, &uri)
    else {
        return Ok(serde_json::Value::Null);
    };
    let (parse, mapper) = parse_document(&document);
    let Some(position) =
        workspace_position(&document, selected_start, position, &state.documents, &uri)
    else {
        return Ok(serde_json::Value::Null);
    };
    let location = analysis::definition(&document, &parse, position, &mapper)
        .and_then(|location| project_location(location, &mapper, &segments));
    let response = location.map(GotoDefinitionResponse::Scalar);
    Ok(serde_json::to_value(response)?)
}

fn request_references(
    state: &mut ServerState,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) = request.extract::<ReferenceParams>("textDocument/references")?;
    let uri = params.text_document_position.text_document.uri;
    let position = params.text_document_position.position;
    let Some((document, segments, selected_start)) = combined_workspace(&state.documents, &uri)
    else {
        return Ok(serde_json::Value::Null);
    };
    let (parse, mapper) = parse_document(&document);
    let Some(position) =
        workspace_position(&document, selected_start, position, &state.documents, &uri)
    else {
        return Ok(serde_json::Value::Null);
    };
    let locations = analysis::references(
        &document,
        &parse,
        position,
        &mapper,
        params.context.include_declaration,
    )
    .into_iter()
    .filter_map(|location| project_location(location, &mapper, &segments))
    .collect::<Vec<_>>();
    Ok(serde_json::to_value(locations)?)
}

fn request_rename(
    state: &mut ServerState,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) = request.extract::<RenameParams>("textDocument/rename")?;
    let uri = params.text_document_position.text_document.uri;
    let position = params.text_document_position.position;
    let Some(changes) = semantic_rename(&state.documents, &uri, position, &params.new_name) else {
        return Ok(serde_json::Value::Null);
    };
    let workspace_edit = WorkspaceEdit {
        changes: Some(changes),
        document_changes: None,
        change_annotations: None,
    };
    Ok(serde_json::to_value(workspace_edit)?)
}

fn request_prepare_rename(
    state: &mut ServerState,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) =
        request.extract::<lsp_types::TextDocumentPositionParams>("textDocument/prepareRename")?;
    let uri = params.text_document.uri;
    let response = semantic_prepare_rename(&state.documents, &uri, params.position);
    Ok(serde_json::to_value(response)?)
}

struct SourceSegment<'a> {
    document: &'a Document,
    start: usize,
    end: usize,
}

/// Builds a deterministic source view from open Orna documents so editor
/// features can resolve declarations and references across file boundaries.
fn combined_workspace<'a>(
    documents: &'a HashMap<Uri, Document>,
    uri: &Uri,
) -> Option<(Document, Vec<SourceSegment<'a>>, usize)> {
    let mut sources: Vec<_> = documents
        .values()
        .filter(|document| document.uri.as_str().ends_with(".orna"))
        .collect();
    sources.sort_unstable_by(|left, right| left.uri.as_str().cmp(right.uri.as_str()));

    let mut combined_source = String::new();
    let mut segments = Vec::with_capacity(sources.len());
    for document in sources {
        if !segments.is_empty() {
            combined_source.push('\n');
        }
        let start = combined_source.len();
        combined_source.push_str(&document.text);
        let end = combined_source.len();
        segments.push(SourceSegment {
            document,
            start,
            end,
        });
    }
    let selected = segments
        .iter()
        .find(|segment| &segment.document.uri == uri)?;
    let selected_start = selected.start;
    let version = selected.document.version;
    Some((
        Document::new(uri.clone(), combined_source, version),
        segments,
        selected_start,
    ))
}

fn workspace_position(
    workspace: &Document,
    selected_start: usize,
    position: Position,
    documents: &HashMap<Uri, Document>,
    uri: &Uri,
) -> Option<Position> {
    let selected = documents.get(uri)?;
    let selected_mapper = PositionMapper::new(&selected.text);
    let workspace_mapper = PositionMapper::new(&workspace.text);
    let byte = selected_start.checked_add(selected_mapper.byte_offset(position))?;
    Some(workspace_mapper.position(byte))
}

fn project_location(
    location: lsp_types::Location,
    workspace_mapper: &PositionMapper<'_>,
    segments: &[SourceSegment<'_>],
) -> Option<lsp_types::Location> {
    let start = workspace_mapper.byte_offset(location.range.start);
    let end = workspace_mapper.byte_offset(location.range.end);
    let segment = segments
        .iter()
        .find(|segment| start >= segment.start && end <= segment.end && start < end)?;
    let mapper = PositionMapper::new(&segment.document.text);
    Some(lsp_types::Location {
        uri: segment.document.uri.clone(),
        range: mapper.range(&orna_syntax_v1::SyntaxSpan::new(
            start - segment.start,
            end - segment.start,
        )),
    })
}

fn semantic_rename(
    documents: &HashMap<Uri, Document>,
    uri: &Uri,
    position: Position,
    new_name: &str,
) -> Option<HashMap<Uri, Vec<TextEdit>>> {
    let (workspace_document, segments, selected_start) = combined_workspace(documents, uri)?;
    let combined_position = workspace_position(
        &workspace_document,
        selected_start,
        position,
        documents,
        uri,
    )?;
    let workspace_parse = analysis::parse_document(&workspace_document);
    let workspace_mapper = PositionMapper::new(&workspace_document.text);
    let edits = semantic_rename_in_source(
        &workspace_document,
        &workspace_parse,
        combined_position,
        &workspace_mapper,
        new_name,
    )?;

    let mut changes: HashMap<Uri, Vec<TextEdit>> = HashMap::new();
    for edit in edits {
        let start = workspace_mapper.byte_offset(edit.range.start);
        let end = workspace_mapper.byte_offset(edit.range.end);
        let segment = segments
            .iter()
            .find(|segment| start >= segment.start && end <= segment.end && start < end)?;
        let source_mapper = PositionMapper::new(&segment.document.text);
        let range = source_mapper.range(&orna_syntax_v1::SyntaxSpan::new(
            start - segment.start,
            end - segment.start,
        ));
        changes
            .entry(segment.document.uri.clone())
            .or_default()
            .push(TextEdit {
                range,
                new_text: edit.new_text,
            });
    }
    Some(changes)
}

fn semantic_prepare_rename(
    documents: &HashMap<Uri, Document>,
    uri: &Uri,
    position: Position,
) -> Option<PrepareRenameResponse> {
    let (workspace_document, segments, selected_start) = combined_workspace(documents, uri)?;
    let selected_document = documents.get(uri)?;
    if !uri.as_str().ends_with(".orna") {
        return None;
    }
    let combined_position = workspace_position(
        &workspace_document,
        selected_start,
        position,
        documents,
        uri,
    )?;
    let parse = analysis::parse_document(&workspace_document);
    if !parse.diagnostics.is_empty() {
        return None;
    }
    let workspace_mapper = PositionMapper::new(&workspace_document.text);
    let (placeholder, span) =
        analysis::identifier_at(&workspace_document, combined_position, &workspace_mapper)?;
    semantic_rename_in_source(
        &workspace_document,
        &parse,
        combined_position,
        &workspace_mapper,
        &placeholder,
    )?;

    let projected = project_location(
        lsp_types::Location {
            uri: workspace_document.uri.clone(),
            range: workspace_mapper.range(&span),
        },
        &workspace_mapper,
        &segments,
    )?;
    let selected_mapper = PositionMapper::new(&selected_document.text);
    let selected_token = analysis::identifier_at(selected_document, position, &selected_mapper)?;
    if selected_token.0 != placeholder {
        return None;
    }
    Some(PrepareRenameResponse::RangeWithPlaceholder {
        range: projected.range,
        placeholder,
    })
}

fn semantic_rename_in_source(
    document: &Document,
    parse: &Parse,
    position: Position,
    mapper: &PositionMapper<'_>,
    new_name: &str,
) -> Option<Vec<TextEdit>> {
    if !document.uri.as_str().ends_with(".orna") || !parse.diagnostics.is_empty() {
        return None;
    }
    let definition = analysis::definition(document, parse, position, mapper)?;
    if definition.uri != document.uri {
        return None;
    }
    let declarations = persistent_declaration_ranges(parse, &document.text, mapper);
    let target = declarations
        .iter()
        .find(|(_, name_range)| name_range == &definition.range)?;
    let target_name_range = target.1.clone();

    let references = analysis::references(document, parse, position, mapper, true);
    if references.is_empty()
        || references
            .iter()
            .any(|reference| reference.uri != document.uri)
    {
        return None;
    }
    let resolved_declarations = references
        .iter()
        .filter(|reference| {
            declarations
                .iter()
                .any(|(_, name_range)| name_range == &reference.range)
        })
        .count();
    if resolved_declarations != 1
        || !references
            .iter()
            .any(|reference| reference.range == target_name_range)
        || !valid_rename_identifier(new_name)
        || !rename_preserves_unique_resolution(
            document,
            &declarations,
            &target_name_range,
            &references,
            mapper,
            new_name,
        )
    {
        return None;
    }

    Some(
        references
            .into_iter()
            .map(|reference| TextEdit {
                range: reference.range,
                new_text: new_name.to_owned(),
            })
            .collect(),
    )
}

fn persistent_declaration_ranges(
    parse: &Parse,
    text: &str,
    mapper: &PositionMapper<'_>,
) -> Vec<(Range, Range)> {
    let mut declarations = analysis::declaration_symbols(parse, text)
        .into_iter()
        .map(|symbol| (mapper.range(&symbol.full), mapper.range(&symbol.selection)))
        .collect::<Vec<_>>();
    declarations.extend(
        analysis::local_declaration_spans(parse)
            .into_iter()
            .map(|span| {
                let selection = mapper.range(&span);
                (selection.clone(), selection)
            }),
    );
    declarations
}

fn valid_rename_identifier(new_name: &str) -> bool {
    let source = format!("fn {new_name}() = 0;");
    let parse = orna_syntax_v1::parse_module(&source);
    parse.diagnostics.is_empty()
        && analysis::declaration_symbols(&parse, &source)
            .iter()
            .any(|symbol| {
                symbol.name == new_name
                    && source[symbol.selection.start..symbol.selection.end] == *new_name
            })
}

fn rename_preserves_unique_resolution(
    document: &Document,
    declarations: &[(Range, Range)],
    target_name_range: &Range,
    references: &[lsp_types::Location],
    mapper: &PositionMapper<'_>,
    new_name: &str,
) -> bool {
    let mut byte_edits = Vec::with_capacity(references.len());
    for reference in references {
        let start = mapper.byte_offset(reference.range.start);
        let end = mapper.byte_offset(reference.range.end);
        if start >= end || end > document.text.len() {
            return false;
        }
        byte_edits.push((start, end));
    }
    byte_edits.sort_unstable_by_key(|(start, _)| *start);
    if byte_edits
        .windows(2)
        .any(|pair| pair[0].1 > pair[1].0 || pair[0].0 == pair[1].0)
    {
        return false;
    }

    let mut expected_ranges = Vec::with_capacity(byte_edits.len());
    let mut prior_shift = 0i128;
    for (start, end) in &byte_edits {
        let Ok(updated_start) = usize::try_from(*start as i128 + prior_shift) else {
            return false;
        };
        let Some(updated_end) = updated_start.checked_add(new_name.len()) else {
            return false;
        };
        expected_ranges.push((updated_start, updated_end));
        prior_shift += new_name.len() as i128 - (end - start) as i128;
    }

    let target_start = mapper.byte_offset(target_name_range.start);
    let shift: i128 = byte_edits
        .iter()
        .filter(|(start, _)| *start < target_start)
        .map(|(start, end)| new_name.len() as i128 - (end - start) as i128)
        .sum();
    let Ok(updated_target_start) = usize::try_from(target_start as i128 + shift) else {
        return false;
    };

    let mut updated_source = document.text.clone();
    for (start, end) in byte_edits.into_iter().rev() {
        updated_source.replace_range(start..end, new_name);
    }
    let updated_document = Document::new(document.uri.clone(), updated_source, document.version);
    let updated_parse = analysis::parse_document(&updated_document);
    if !updated_parse.diagnostics.is_empty() {
        return false;
    }
    let updated_mapper = PositionMapper::new(&updated_document.text);
    let updated_position = updated_mapper.position(updated_target_start);
    let Some(updated_definition) = analysis::definition(
        &updated_document,
        &updated_parse,
        updated_position,
        &updated_mapper,
    ) else {
        return false;
    };
    let updated_declarations =
        persistent_declaration_ranges(&updated_parse, &updated_document.text, &updated_mapper);
    let Some((_, updated_target_name_range)) = updated_declarations
        .iter()
        .find(|(_, name_range)| name_range == &updated_definition.range)
    else {
        return false;
    };
    let updated_references = analysis::references(
        &updated_document,
        &updated_parse,
        updated_position,
        &updated_mapper,
        true,
    );
    let mut resolved_ranges = Vec::with_capacity(updated_references.len());
    for reference in &updated_references {
        let start = updated_mapper.byte_offset(reference.range.start);
        let end = updated_mapper.byte_offset(reference.range.end);
        if start >= end || end > updated_document.text.len() {
            return false;
        }
        resolved_ranges.push((start, end));
    }
    resolved_ranges.sort_unstable();
    if resolved_ranges != expected_ranges {
        return false;
    }
    updated_references
        .iter()
        .filter(|reference| {
            updated_declarations
                .iter()
                .any(|(_, name_range)| name_range == &reference.range)
        })
        .count()
        == 1
        && updated_references
            .iter()
            .any(|reference| reference.range == *updated_target_name_range)
        && declarations
            .iter()
            .any(|(_, name_range)| name_range == target_name_range)
}

fn request_document_symbols(
    state: &mut ServerState,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) = request.extract::<DocumentSymbolParams>("textDocument/documentSymbol")?;
    let uri = params.text_document.uri;
    let Some(document) = state.document(&uri) else {
        return Ok(serde_json::Value::Null);
    };
    let (parse, mapper) = parse_document(document);
    let symbols = analysis::document_symbols(&parse, &document.text, &mapper);
    let response = DocumentSymbolResponse::Nested(symbols);
    Ok(serde_json::to_value(response)?)
}

fn request_workspace_symbols(
    state: &mut ServerState,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) = request.extract::<lsp_types::WorkspaceSymbolParams>("workspace/symbol")?;
    let query = params.query.to_ascii_lowercase();
    let mut symbols = Vec::new();
    for document in state.documents.values() {
        let (parse, mapper) = parse_document(document);
        for symbol in analysis::document_symbols(&parse, &document.text, &mapper) {
            if symbol.name.to_ascii_lowercase().contains(&query) {
                symbols.push(lsp_types::WorkspaceSymbol {
                    name: symbol.name,
                    kind: symbol.kind,
                    tags: symbol.tags,
                    container_name: symbol.detail,
                    location: lsp_types::OneOf::Left(lsp_types::Location {
                        uri: document.uri.clone(),
                        range: symbol.selection_range,
                    }),
                    data: None,
                });
            }
        }
    }
    Ok(serde_json::to_value(
        lsp_types::WorkspaceSymbolResponse::Nested(symbols),
    )?)
}

fn request_semantic_tokens_full(
    state: &mut ServerState,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) =
        request.extract::<SemanticTokensParams>("textDocument/semanticTokens/full")?;
    let uri = params.text_document.uri;
    let Some(document) = state.document(&uri) else {
        return Ok(serde_json::Value::Null);
    };
    let mapper = PositionMapper::new(&document.text);
    let data = semantic::semantic_tokens(&document.text, &mapper, None);
    Ok(serde_json::to_value(SemanticTokens {
        result_id: None,
        data,
    })?)
}

fn request_semantic_tokens_range(
    state: &mut ServerState,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) =
        request.extract::<SemanticTokensRangeParams>("textDocument/semanticTokens/range")?;
    let uri = params.text_document.uri;
    let Some(document) = state.document(&uri) else {
        return Ok(serde_json::Value::Null);
    };
    let mapper = PositionMapper::new(&document.text);
    let data = semantic::semantic_tokens(&document.text, &mapper, Some(&params.range));
    Ok(serde_json::to_value(SemanticTokens {
        result_id: None,
        data,
    })?)
}

fn request_inlay_hints(
    state: &mut ServerState,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) = request.extract::<InlayHintParams>("textDocument/inlayHint")?;
    let Some(document) = state.document(&params.text_document.uri) else {
        return Ok(serde_json::Value::Null);
    };
    let parse = analysis::parse_document(document);
    let mapper = PositionMapper::new(&document.text);
    Ok(serde_json::to_value(inlay::inlay_hints(
        &parse,
        &document.text,
        &mapper,
        &params.range,
    ))?)
}

fn request_completion(
    state: &mut ServerState,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) = request.extract::<CompletionParams>("textDocument/completion")?;
    let uri = params.text_document_position.text_document.uri;
    let Some((document, _, selected_start)) = combined_workspace(&state.documents, &uri) else {
        return Ok(serde_json::Value::Null);
    };
    let (parse, mapper) = parse_document(&document);
    let position = params.text_document_position.position;
    let Some(position) =
        workspace_position(&document, selected_start, position, &state.documents, &uri)
    else {
        return Ok(serde_json::Value::Null);
    };
    let byte = mapper.byte_offset(position);
    let items =
        analysis::completion_at(&parse, &document.text, Some(byte), params.context.as_ref());
    let response = CompletionResponse::Array(items);
    Ok(serde_json::to_value(response)?)
}

fn request_document_diagnostic(
    state: &mut ServerState,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) = request.extract::<DocumentDiagnosticParams>("textDocument/diagnostic")?;
    let uri = params.text_document.uri;
    let Some(document) = state.document(&uri) else {
        let report = empty_diagnostic_report();
        return Ok(serde_json::to_value(report)?);
    };
    let mapper = PositionMapper::new(&document.text);
    let diagnostics = analysis::check_document(document, &mapper);
    let report = DocumentDiagnosticReport::Full(RelatedFullDocumentDiagnosticReport {
        related_documents: None,
        full_document_diagnostic_report: FullDocumentDiagnosticReport {
            result_id: None,
            items: diagnostics,
        },
    });
    Ok(serde_json::to_value(report)?)
}

fn empty_diagnostic_report() -> DocumentDiagnosticReport {
    DocumentDiagnosticReport::Full(RelatedFullDocumentDiagnosticReport {
        related_documents: None,
        full_document_diagnostic_report: FullDocumentDiagnosticReport {
            result_id: None,
            items: Vec::new(),
        },
    })
}
