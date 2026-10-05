//! The LSP server loop and request dispatch.
//!
//! The server is synchronous and single-threaded. Compiler checks for one
//! document are fast, so no worker pool is needed for the first version.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io::{self, BufReader};
use std::thread::{self, JoinHandle};

use crate::analysis::{self, EditorParse as Parse};
use crate::documents::{Document, PositionMapper};
use crate::{editor_ranges, inlay, semantic};
use lsp_server::{Connection, ErrorCode, Message, Notification, Request, Response, ResponseError};
use lsp_types::{
    CallHierarchyItem, CallHierarchyOptions, CallHierarchyServerCapability, CodeAction,
    CodeActionKind, CodeActionOptions, CodeActionOrCommand, CodeActionParams,
    CodeActionProviderCapability, CodeLens, CodeLensOptions, CodeLensParams, Command,
    CompletionOptions, CompletionParams, CompletionResponse, Diagnostic, DiagnosticOptions,
    DiagnosticServerCapabilities, DocumentDiagnosticParams, DocumentDiagnosticReport,
    DocumentHighlight, DocumentHighlightKind, DocumentHighlightParams, DocumentLink,
    DocumentLinkOptions, DocumentLinkParams, DocumentSymbolParams, DocumentSymbolResponse,
    FoldingRange, FoldingRangeParams, FoldingRangeProviderCapability, FullDocumentDiagnosticReport,
    GotoDefinitionParams, GotoDefinitionResponse, Hover, HoverParams, HoverProviderCapability,
    InitializeParams, InlayHintOptions, InlayHintParams, InlayHintServerCapabilities, InlineValue,
    InlineValueOptions, InlineValueParams, InlineValueServerCapabilities,
    InlineValueVariableLookup, LinkedEditingRangeParams, LinkedEditingRangeServerCapabilities,
    LinkedEditingRanges, Moniker, MonikerKind, MonikerParams, NumberOrString, OneOf, Position,
    PositionEncodingKind, PrepareRenameResponse, PublishDiagnosticsParams, Range, ReferenceParams,
    RelatedFullDocumentDiagnosticReport, RelatedUnchangedDocumentDiagnosticReport, RenameOptions,
    RenameParams, SelectionRangeParams, SelectionRangeProviderCapability, SemanticToken,
    SemanticTokens, SemanticTokensDeltaParams, SemanticTokensFullOptions, SemanticTokensLegend,
    SemanticTokensOptions, SemanticTokensParams, SemanticTokensRangeParams,
    SemanticTokensServerCapabilities, ServerCapabilities, SignatureHelpOptions,
    TextDocumentContentChangeEvent, TextDocumentSyncCapability, TextDocumentSyncKind,
    TextDocumentSyncOptions, TextDocumentSyncSaveOptions, TextEdit, TypeHierarchyItem,
    TypeHierarchyPrepareParams, TypeHierarchySubtypesParams, TypeHierarchySupertypesParams,
    UnchangedDocumentDiagnosticReport, UniquenessLevel, Uri, WorkspaceEdit,
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
    semantic_tokens: HashMap<Uri, SemanticTokenSnapshot>,
    next_semantic_token_result: u64,
}

struct SemanticTokenSnapshot {
    result_id: String,
    data: Vec<SemanticToken>,
}

impl ServerState {
    fn new() -> Self {
        Self {
            documents: HashMap::new(),
            semantic_tokens: HashMap::new(),
            next_semantic_token_result: 0,
        }
    }

    fn document(&self, uri: &Uri) -> Option<&Document> {
        self.documents.get(uri)
    }

    fn next_semantic_token_result_id(&mut self) -> String {
        self.next_semantic_token_result += 1;
        format!("orna-semantic-{}", self.next_semantic_token_result)
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
    capabilities: serde_json::Value,
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
fn server_capabilities() -> serde_json::Value {
    let mut capabilities = serde_json::to_value(ServerCapabilities {
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
        document_highlight_provider: Some(OneOf::Left(true)),
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
        code_lens_provider: Some(CodeLensOptions {
            resolve_provider: Some(false),
        }),
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
            identifier: Some("orna-syntax-v1".to_owned()),
            inter_file_dependencies: false,
            workspace_diagnostics: false,
            work_done_progress_options: Default::default(),
        })),
        document_link_provider: Some(DocumentLinkOptions {
            resolve_provider: Some(false),
            work_done_progress_options: Default::default(),
        }),
        call_hierarchy_provider: Some(CallHierarchyServerCapability::Options(
            CallHierarchyOptions::default(),
        )),
        moniker_provider: Some(OneOf::Left(true)),
        inline_value_provider: Some(OneOf::Right(InlineValueServerCapabilities::Options(
            InlineValueOptions::default(),
        ))),
        linked_editing_range_provider: Some(LinkedEditingRangeServerCapabilities::Simple(true)),
        folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
        selection_range_provider: Some(SelectionRangeProviderCapability::Simple(true)),
        ..ServerCapabilities::default()
    })
    .expect("serializable LSP server capabilities");
    capabilities["semanticTokensProvider"]["full"] = serde_json::json!({ "delta": true });
    capabilities
        .as_object_mut()
        .expect("server capabilities serialize as an object")
        .insert("typeHierarchyProvider".to_owned(), serde_json::json!({}));
    capabilities
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
        "textDocument/documentHighlight" => request_document_highlights(state, request),
        "textDocument/prepareRename" => request_prepare_rename(state, request),
        "textDocument/rename" => request_rename(state, request),
        "textDocument/documentSymbol" => request_document_symbols(state, request),
        "textDocument/semanticTokens/full" => request_semantic_tokens_full(state, request),
        "textDocument/semanticTokens/full/delta" => request_semantic_tokens_delta(state, request),
        "textDocument/semanticTokens/range" => request_semantic_tokens_range(state, request),
        "textDocument/inlayHint" => request_inlay_hints(state, request),
        "textDocument/inlineValue" => request_inline_values(state, request),
        "textDocument/linkedEditingRange" => request_linked_editing_range(state, request),
        "textDocument/documentLink" => request_document_links(state, request),
        "textDocument/foldingRange" => request_folding_ranges(state, request),
        "textDocument/selectionRange" => request_selection_ranges(state, request),
        "textDocument/completion" => request_completion(state, request),
        "workspace/symbol" => request_workspace_symbols(state, request),
        "textDocument/codeLens" => request_code_lenses(state, request),
        "textDocument/prepareCallHierarchy" => request_prepare_call_hierarchy(state, request),
        "callHierarchy/incomingCalls" => request_incoming_calls(state, request),
        "callHierarchy/outgoingCalls" => request_outgoing_calls(state, request),
        "textDocument/prepareTypeHierarchy" => request_prepare_type_hierarchy(state, request),
        "typeHierarchy/supertypes" => request_type_hierarchy_supertypes(state, request),
        "typeHierarchy/subtypes" => request_type_hierarchy_subtypes(state, request),
        "textDocument/moniker" => request_moniker(state, request),
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
            state.semantic_tokens.remove(&uri);
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
                state.semantic_tokens.remove(&uri);
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

fn request_document_highlights(
    state: &mut ServerState,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) =
        request.extract::<DocumentHighlightParams>("textDocument/documentHighlight")?;
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

    let mut write_ranges = analysis::local_declaration_spans(&parse);
    write_ranges.extend(crate::locals::write_spans(&parse.value));
    write_ranges.extend(
        analysis::declaration_symbols(&parse, &document.text)
            .into_iter()
            .map(|symbol| symbol.selection),
    );
    let write_ranges = write_ranges
        .iter()
        .map(|span| mapper.range(span))
        .collect::<Vec<_>>();

    let highlights = analysis::references(&document, &parse, position, &mapper, true)
        .into_iter()
        .filter_map(|location| {
            let kind = if write_ranges.contains(&location.range) {
                DocumentHighlightKind::WRITE
            } else {
                DocumentHighlightKind::READ
            };
            project_location(location, &mapper, &segments)
                .filter(|projected| projected.uri == uri)
                .map(|projected| DocumentHighlight {
                    kind: Some(kind),
                    range: projected.range,
                })
        })
        .collect::<Vec<_>>();
    Ok(serde_json::to_value(highlights)?)
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
    let query = params.query.trim().to_lowercase();
    let mut symbols = Vec::new();
    for document in state
        .documents
        .values()
        .filter(|document| document.uri.as_str().ends_with(".orna"))
    {
        let (parse, mapper) = parse_document(document);
        if !parse.diagnostics.is_empty() {
            continue;
        }
        for symbol in analysis::document_symbols(&parse, &document.text, &mapper) {
            if let Some(score) = workspace_symbol_score(&symbol.name, &query) {
                let range = symbol.selection_range;
                symbols.push((
                    score,
                    symbol.name.to_lowercase(),
                    document.uri.as_str().to_owned(),
                    range.start,
                    lsp_types::WorkspaceSymbol {
                        name: symbol.name,
                        kind: symbol.kind,
                        tags: symbol.tags,
                        container_name: None,
                        location: lsp_types::OneOf::Left(lsp_types::Location {
                            uri: document.uri.clone(),
                            range,
                        }),
                        data: None,
                    },
                ));
            }
        }
    }
    symbols.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.1.cmp(&right.1))
            .then_with(|| left.2.cmp(&right.2))
            .then_with(|| (left.3.line, left.3.character).cmp(&(right.3.line, right.3.character)))
    });
    Ok(serde_json::to_value(
        lsp_types::WorkspaceSymbolResponse::Nested(
            symbols
                .into_iter()
                .map(|(_, _, _, _, symbol)| symbol)
                .collect(),
        ),
    )?)
}

fn workspace_symbol_score(name: &str, query: &str) -> Option<u32> {
    if query.is_empty() {
        return Some(0);
    }
    let candidate = name.to_lowercase();
    if candidate == query {
        return Some(0);
    }
    if candidate.starts_with(query) {
        return Some(10 + candidate.chars().count() as u32 - query.chars().count() as u32);
    }
    if let Some(index) = candidate.find(query) {
        let boundary = index == 0
            || candidate[..index]
                .chars()
                .next_back()
                .is_some_and(|previous| !previous.is_alphanumeric());
        return Some(30 + index as u32 - u32::from(boundary) * 5);
    }

    let query_chars = query.chars().collect::<Vec<_>>();
    let candidate_chars = candidate.chars().collect::<Vec<_>>();
    let mut matched = Vec::with_capacity(query_chars.len());
    let mut cursor = 0usize;
    for wanted in query_chars {
        let relative = candidate_chars[cursor..]
            .iter()
            .position(|character| *character == wanted)?;
        matched.push(cursor + relative);
        cursor += relative + 1;
    }
    let first = *matched.first()?;
    let gaps = matched
        .windows(2)
        .map(|pair| pair[1].saturating_sub(pair[0] + 1) as u32)
        .sum::<u32>();
    let boundary_bonus = (first == 0
        || candidate_chars
            .get(first.wrapping_sub(1))
            .is_some_and(|previous| !previous.is_alphanumeric())) as u32
        * 5;
    Some(100 + first as u32 + gaps * 4 - boundary_bonus)
}

#[derive(Debug, Clone)]
struct WorkspaceFunction {
    document: Document,
    definition: analysis::FunctionDefinition,
}

#[derive(Debug, Clone)]
struct WorkspaceDeclaration {
    document: Document,
    symbol: analysis::EditorSymbol,
    public: bool,
    protocols: Vec<String>,
}

fn workspace_declarations(state: &ServerState) -> Vec<WorkspaceDeclaration> {
    let mut documents = state
        .documents
        .values()
        .filter(|document| document.uri.as_str().ends_with(".orna"))
        .collect::<Vec<_>>();
    documents.sort_by(|left, right| left.uri.as_str().cmp(right.uri.as_str()));

    let mut declarations = Vec::new();
    for document in documents {
        let parse = analysis::parse_document(document);
        if !parse.diagnostics.is_empty() {
            continue;
        }
        for symbol in analysis::declaration_symbols(&parse, &document.text) {
            let Some(item) = parse.value.items.iter().find(|item| {
                item.span.start == symbol.full.start && item.span.end == symbol.full.end
            }) else {
                continue;
            };
            let public = matches!(&item.visibility, orna_syntax_v1::Visibility::Public { .. });
            let protocols = implementation_protocols(&item.declaration);
            declarations.push(WorkspaceDeclaration {
                document: document.clone(),
                symbol,
                public,
                protocols,
            });
        }
    }
    declarations.sort_by(|left, right| {
        left.document
            .uri
            .as_str()
            .cmp(right.document.uri.as_str())
            .then_with(|| {
                left.symbol
                    .selection
                    .start
                    .cmp(&right.symbol.selection.start)
            })
    });
    declarations
}

fn implementation_protocols(declaration: &orna_syntax_v1::Declaration) -> Vec<String> {
    let mut protocols = Vec::new();
    let mut add = |implementation: &orna_syntax_v1::Implementation| {
        if let orna_syntax_v1::TypeExpr::Name { path, .. } = &implementation.protocol
            && let Some(name) = path.last()
        {
            protocols.push(name.clone());
        }
    };
    match declaration {
        orna_syntax_v1::Declaration::Table { members, .. } => {
            for member in members {
                if let orna_syntax_v1::TableMember::Implementation { implementation, .. } = member {
                    add(implementation);
                }
            }
        }
        orna_syntax_v1::Declaration::Type {
            representation:
                orna_syntax_v1::TypeRepresentation::Alias { refinements, .. }
                | orna_syntax_v1::TypeRepresentation::Nominal {
                    members: refinements,
                },
            ..
        } => {
            for member in refinements {
                if let orna_syntax_v1::TypeMember::Implementation { implementation, .. } = member {
                    add(implementation);
                }
            }
        }
        _ => {}
    }
    protocols.sort_by_key(|name| analysis::normalized_identifier(name));
    protocols.dedup_by(|left, right| {
        analysis::normalized_identifier(left) == analysis::normalized_identifier(right)
    });
    protocols
}

fn is_hierarchy_declaration(declaration: &WorkspaceDeclaration) -> bool {
    matches!(
        declaration.symbol.kind,
        analysis::EditorSymbolKind::Type
            | analysis::EditorSymbolKind::Table
            | analysis::EditorSymbolKind::Protocol
    )
}

fn hierarchy_symbol_kind(kind: analysis::EditorSymbolKind) -> Option<lsp_types::SymbolKind> {
    match kind {
        analysis::EditorSymbolKind::Type => Some(lsp_types::SymbolKind::STRUCT),
        analysis::EditorSymbolKind::Table => Some(lsp_types::SymbolKind::CLASS),
        analysis::EditorSymbolKind::Protocol => Some(lsp_types::SymbolKind::INTERFACE),
        _ => None,
    }
}

fn type_hierarchy_item(declaration: &WorkspaceDeclaration) -> Option<TypeHierarchyItem> {
    let mapper = PositionMapper::new(&declaration.document.text);
    Some(TypeHierarchyItem {
        name: declaration.symbol.name.clone(),
        kind: hierarchy_symbol_kind(declaration.symbol.kind)?,
        tags: None,
        detail: declaration.symbol.detail.clone(),
        uri: declaration.document.uri.clone(),
        range: mapper.range(&declaration.symbol.full),
        selection_range: mapper.range(&declaration.symbol.selection),
        data: Some(serde_json::json!({
            "scheme": "orna-syntax-v1",
            "uri": declaration.document.uri.as_str(),
            "name": analysis::normalized_identifier(&declaration.symbol.name),
            "start": declaration.symbol.selection.start,
            "end": declaration.symbol.selection.end,
        })),
    })
}

fn same_type_hierarchy_declaration(
    item: &TypeHierarchyItem,
    declaration: &WorkspaceDeclaration,
) -> bool {
    hierarchy_symbol_kind(declaration.symbol.kind).is_some_and(|kind| {
        let mapper = PositionMapper::new(&declaration.document.text);
        item.uri == declaration.document.uri
            && item.name == declaration.symbol.name
            && item.kind == kind
            && item.selection_range == mapper.range(&declaration.symbol.selection)
    })
}

fn uniquely_resolved_protocol(declarations: &[WorkspaceDeclaration], name: &str) -> Option<usize> {
    let key = analysis::normalized_identifier(name);
    let mut matches = declarations.iter().enumerate().filter(|(_, declaration)| {
        declaration.symbol.kind == analysis::EditorSymbolKind::Protocol
            && analysis::normalized_identifier(&declaration.symbol.name) == key
    });
    let found = matches.next()?.0;
    matches.next().is_none().then_some(found)
}

fn workspace_declaration_at<'a>(
    declarations: &'a [WorkspaceDeclaration],
    document: &Document,
    selection: &orna_syntax_v1::SyntaxSpan,
    mapper: &PositionMapper<'_>,
    segments: &[SourceSegment<'_>],
) -> Option<&'a WorkspaceDeclaration> {
    let selected = project_location(
        lsp_types::Location {
            uri: document.uri.clone(),
            range: mapper.range(selection),
        },
        mapper,
        segments,
    )?;
    declarations.iter().find(|declaration| {
        declaration.document.uri == selected.uri
            && PositionMapper::new(&declaration.document.text).range(&declaration.symbol.selection)
                == selected.range
    })
}

fn workspace_functions(state: &ServerState) -> Vec<WorkspaceFunction> {
    let mut documents = state
        .documents
        .values()
        .filter(|document| document.uri.as_str().ends_with(".orna"))
        .collect::<Vec<_>>();
    documents.sort_by(|left, right| left.uri.as_str().cmp(right.uri.as_str()));
    let mut functions = Vec::new();
    for document in documents {
        let parse = analysis::parse_document(document);
        if !parse.diagnostics.is_empty() {
            continue;
        }
        functions.extend(
            analysis::function_definitions(&parse, &document.text)
                .into_iter()
                .map(|definition| WorkspaceFunction {
                    document: document.clone(),
                    definition,
                }),
        );
    }
    functions.sort_by(|left, right| {
        left.document
            .uri
            .as_str()
            .cmp(right.document.uri.as_str())
            .then_with(|| {
                left.definition
                    .selection
                    .start
                    .cmp(&right.definition.selection.start)
            })
    });
    functions
}

fn call_hierarchy_item(function: &WorkspaceFunction) -> CallHierarchyItem {
    let mapper = PositionMapper::new(&function.document.text);
    CallHierarchyItem {
        name: function.definition.name.clone(),
        kind: lsp_types::SymbolKind::FUNCTION,
        tags: None,
        detail: function.definition.detail.clone(),
        uri: function.document.uri.clone(),
        range: mapper.range(&function.definition.full),
        selection_range: mapper.range(&function.definition.selection),
        data: None,
    }
}

fn same_call_hierarchy_function(item: &CallHierarchyItem, function: &WorkspaceFunction) -> bool {
    let expected = call_hierarchy_item(function);
    item.uri == expected.uri && item.selection_range == expected.selection_range
}

fn uniquely_resolved_function(functions: &[WorkspaceFunction], name: &str) -> Option<usize> {
    let key = analysis::normalized_identifier(name);
    let mut matches = functions
        .iter()
        .enumerate()
        .filter(|(_, function)| analysis::normalized_identifier(&function.definition.name) == key);
    let found = matches.next()?.0;
    matches.next().is_none().then_some(found)
}

fn request_prepare_call_hierarchy(
    state: &mut ServerState,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) = request
        .extract::<lsp_types::CallHierarchyPrepareParams>("textDocument/prepareCallHierarchy")?;
    let uri = params.text_document_position_params.text_document.uri;
    let functions = workspace_functions(state);
    let Some(document) = state.document(&uri) else {
        return Ok(serde_json::Value::Null);
    };
    let parse = analysis::parse_document(document);
    if !parse.diagnostics.is_empty() {
        return Ok(serde_json::Value::Null);
    }
    let mapper = PositionMapper::new(&document.text);
    let byte = mapper.byte_offset(params.text_document_position_params.position);
    if let Some(function) = functions.iter().find(|function| {
        function.document.uri == uri
            && function.definition.selection.start <= byte
            && byte <= function.definition.selection.end
    }) {
        return Ok(serde_json::to_value(vec![call_hierarchy_item(function)])?);
    }

    let target_name = analysis::function_definitions(&parse, &document.text)
        .into_iter()
        .flat_map(|function| {
            function
                .calls
                .into_iter()
                .map(move |call| (function.selection.start, call))
        })
        .filter(|(_, call)| call.selection.start <= byte && byte <= call.selection.end)
        .min_by_key(|(_, call)| call.selection.end.saturating_sub(call.selection.start))
        .map(|(_, call)| call.name);
    let Some(target_name) = target_name else {
        return Ok(serde_json::Value::Null);
    };
    let Some(target) = uniquely_resolved_function(&functions, &target_name) else {
        return Ok(serde_json::Value::Null);
    };
    Ok(serde_json::to_value(vec![call_hierarchy_item(
        &functions[target],
    )])?)
}

fn request_incoming_calls(
    state: &mut ServerState,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) = request
        .extract::<lsp_types::CallHierarchyIncomingCallsParams>("callHierarchy/incomingCalls")?;
    let functions = workspace_functions(state);
    let Some(target) = functions
        .iter()
        .position(|function| same_call_hierarchy_function(&params.item, function))
    else {
        return Ok(serde_json::to_value(Vec::<
            lsp_types::CallHierarchyIncomingCall,
        >::new())?);
    };
    let target_name = analysis::normalized_identifier(&functions[target].definition.name);
    let mut incoming = Vec::new();
    for caller in &functions {
        let mut spans = caller
            .definition
            .calls
            .iter()
            .filter(|call| {
                uniquely_resolved_function(&functions, &call.name) == Some(target)
                    && analysis::normalized_identifier(&call.name) == target_name
            })
            .map(|call| call.selection.clone())
            .collect::<Vec<_>>();
        if spans.is_empty() {
            continue;
        }
        spans.sort_by_key(|span| (span.start, span.end));
        let mapper = PositionMapper::new(&caller.document.text);
        incoming.push(lsp_types::CallHierarchyIncomingCall {
            from: call_hierarchy_item(caller),
            from_ranges: spans.iter().map(|span| mapper.range(span)).collect(),
        });
    }
    Ok(serde_json::to_value(incoming)?)
}

fn request_outgoing_calls(
    state: &mut ServerState,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) = request
        .extract::<lsp_types::CallHierarchyOutgoingCallsParams>("callHierarchy/outgoingCalls")?;
    let functions = workspace_functions(state);
    let Some(source) = functions
        .iter()
        .position(|function| same_call_hierarchy_function(&params.item, function))
    else {
        return Ok(serde_json::to_value(Vec::<
            lsp_types::CallHierarchyOutgoingCall,
        >::new())?);
    };
    let mut groups: Vec<(usize, Vec<orna_syntax_v1::SyntaxSpan>)> = Vec::new();
    for call in &functions[source].definition.calls {
        let Some(target) = uniquely_resolved_function(&functions, &call.name) else {
            continue;
        };
        if let Some((_, spans)) = groups.iter_mut().find(|(index, _)| *index == target) {
            spans.push(call.selection.clone());
        } else {
            groups.push((target, vec![call.selection.clone()]));
        }
    }
    groups.sort_by_key(|(target, _)| *target);
    let mapper = PositionMapper::new(&functions[source].document.text);
    let outgoing = groups
        .into_iter()
        .map(|(target, mut spans)| {
            spans.sort_by_key(|span| (span.start, span.end));
            lsp_types::CallHierarchyOutgoingCall {
                to: call_hierarchy_item(&functions[target]),
                from_ranges: spans.iter().map(|span| mapper.range(span)).collect(),
            }
        })
        .collect::<Vec<_>>();
    Ok(serde_json::to_value(outgoing)?)
}

fn request_prepare_type_hierarchy(
    state: &mut ServerState,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) =
        request.extract::<TypeHierarchyPrepareParams>("textDocument/prepareTypeHierarchy")?;
    let uri = params.text_document_position_params.text_document.uri;
    let Some((document, segments, selected_start)) = combined_workspace(&state.documents, &uri)
    else {
        return Ok(serde_json::Value::Null);
    };
    let (parse, mapper) = parse_document(&document);
    let Some(position) = workspace_position(
        &document,
        selected_start,
        params.text_document_position_params.position,
        &state.documents,
        &uri,
    ) else {
        return Ok(serde_json::Value::Null);
    };
    let declarations = workspace_declarations(state);
    if let Some((_, selection)) = analysis::identifier_at(&document, position, &mapper)
        && let Some(declaration) =
            workspace_declaration_at(&declarations, &document, &selection, &mapper, &segments)
        && is_hierarchy_declaration(declaration)
    {
        let Some(item) = type_hierarchy_item(declaration) else {
            return Ok(serde_json::Value::Null);
        };
        return Ok(serde_json::to_value(vec![item])?);
    }
    let Some(definition) = analysis::definition(&document, &parse, position, &mapper) else {
        return Ok(serde_json::Value::Null);
    };
    let Some(definition) = project_location(definition, &mapper, &segments) else {
        return Ok(serde_json::Value::Null);
    };
    let Some(declaration) = declarations.iter().find(|declaration| {
        is_hierarchy_declaration(declaration)
            && declaration.document.uri == definition.uri
            && PositionMapper::new(&declaration.document.text).range(&declaration.symbol.selection)
                == definition.range
    }) else {
        return Ok(serde_json::Value::Null);
    };
    let Some(item) = type_hierarchy_item(declaration) else {
        return Ok(serde_json::Value::Null);
    };
    Ok(serde_json::to_value(vec![item])?)
}

fn request_type_hierarchy_supertypes(
    state: &mut ServerState,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) =
        request.extract::<TypeHierarchySupertypesParams>("typeHierarchy/supertypes")?;
    let declarations = workspace_declarations(state);
    let Some(target) = declarations.iter().find(|declaration| {
        is_hierarchy_declaration(declaration)
            && same_type_hierarchy_declaration(&params.item, declaration)
    }) else {
        return Ok(serde_json::to_value(Vec::<TypeHierarchyItem>::new())?);
    };

    let mut supertypes = target
        .protocols
        .iter()
        .filter_map(|name| uniquely_resolved_protocol(&declarations, name))
        .filter_map(|index| type_hierarchy_item(&declarations[index]))
        .collect::<Vec<_>>();
    sort_type_hierarchy_items(&mut supertypes);
    Ok(serde_json::to_value(supertypes)?)
}

fn request_type_hierarchy_subtypes(
    state: &mut ServerState,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) = request.extract::<TypeHierarchySubtypesParams>("typeHierarchy/subtypes")?;
    let declarations = workspace_declarations(state);
    let Some((target_index, target)) = declarations.iter().enumerate().find(|(_, declaration)| {
        is_hierarchy_declaration(declaration)
            && same_type_hierarchy_declaration(&params.item, declaration)
    }) else {
        return Ok(serde_json::to_value(Vec::<TypeHierarchyItem>::new())?);
    };

    if target.symbol.kind != analysis::EditorSymbolKind::Protocol
        || uniquely_resolved_protocol(&declarations, &target.symbol.name) != Some(target_index)
    {
        return Ok(serde_json::to_value(Vec::<TypeHierarchyItem>::new())?);
    }
    let mut subtypes = declarations
        .iter()
        .filter(|declaration| {
            matches!(
                declaration.symbol.kind,
                analysis::EditorSymbolKind::Type | analysis::EditorSymbolKind::Table
            ) && declaration
                .protocols
                .iter()
                .any(|name| uniquely_resolved_protocol(&declarations, name) == Some(target_index))
        })
        .filter_map(type_hierarchy_item)
        .collect::<Vec<_>>();
    sort_type_hierarchy_items(&mut subtypes);
    Ok(serde_json::to_value(subtypes)?)
}

fn sort_type_hierarchy_items(items: &mut [TypeHierarchyItem]) {
    items.sort_by(|left, right| {
        left.uri
            .as_str()
            .cmp(right.uri.as_str())
            .then_with(|| {
                left.selection_range
                    .start
                    .line
                    .cmp(&right.selection_range.start.line)
            })
            .then_with(|| {
                left.selection_range
                    .start
                    .character
                    .cmp(&right.selection_range.start.character)
            })
            .then_with(|| left.name.cmp(&right.name))
    });
}

fn request_moniker(
    state: &mut ServerState,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) = request.extract::<MonikerParams>("textDocument/moniker")?;
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
    let Some((name, selection)) = analysis::identifier_at(&document, position, &mapper) else {
        return Ok(serde_json::Value::Null);
    };

    if let Some(binding) = crate::locals::binding_at(&parse.value, &name, &selection) {
        let binding_location = project_location(
            lsp_types::Location {
                uri: document.uri.clone(),
                range: mapper.range(&binding.selection),
            },
            &mapper,
            &segments,
        );
        let Some(binding_location) = binding_location else {
            return Ok(serde_json::Value::Null);
        };
        let Some(binding_document) = state.document(&binding_location.uri) else {
            return Ok(serde_json::Value::Null);
        };
        let binding_mapper = PositionMapper::new(&binding_document.text);
        let byte = binding_mapper.byte_offset(binding_location.range.start);
        return Ok(serde_json::to_value(vec![Moniker {
            scheme: "orna-syntax-v1".to_owned(),
            identifier: format!(
                "{}#local:{}:{}",
                binding_location.uri.as_str(),
                byte,
                analysis::normalized_identifier(&binding.name)
            ),
            unique: UniquenessLevel::Document,
            kind: Some(MonikerKind::Local),
        }])?);
    }

    let declarations = workspace_declarations(state);
    let clicked = project_location(
        lsp_types::Location {
            uri: document.uri.clone(),
            range: mapper.range(&selection),
        },
        &mapper,
        &segments,
    );
    let direct_declaration =
        workspace_declaration_at(&declarations, &document, &selection, &mapper, &segments);
    let resolved_declaration = analysis::definition(&document, &parse, position, &mapper)
        .and_then(|definition| project_location(definition, &mapper, &segments))
        .and_then(|definition| {
            declarations.iter().find(|declaration| {
                declaration.document.uri == definition.uri
                    && PositionMapper::new(&declaration.document.text)
                        .range(&declaration.symbol.selection)
                        == definition.range
            })
        });
    let Some(declaration) = direct_declaration.or(resolved_declaration) else {
        return Ok(serde_json::Value::Null);
    };
    let at_declaration = clicked.is_some_and(|clicked| {
        clicked.uri == declaration.document.uri
            && clicked.range
                == PositionMapper::new(&declaration.document.text)
                    .range(&declaration.symbol.selection)
    });
    let kind = if !declaration.public {
        MonikerKind::Local
    } else if at_declaration {
        MonikerKind::Export
    } else if declaration.document.uri != uri {
        MonikerKind::Import
    } else {
        MonikerKind::Local
    };
    let moniker_kind = match declaration.symbol.kind {
        analysis::EditorSymbolKind::Function => "function",
        analysis::EditorSymbolKind::Type => "type",
        analysis::EditorSymbolKind::Enum => "enum",
        analysis::EditorSymbolKind::EnumVariant => "enum-member",
        analysis::EditorSymbolKind::Table => "table",
        analysis::EditorSymbolKind::Protocol => "protocol",
        analysis::EditorSymbolKind::Method => "method",
        analysis::EditorSymbolKind::Field => "field",
        analysis::EditorSymbolKind::Constant => "constant",
        analysis::EditorSymbolKind::Other => "symbol",
    };
    Ok(serde_json::to_value(vec![Moniker {
        scheme: "orna-syntax-v1".to_owned(),
        identifier: format!(
            "{}#{}:{}",
            declaration.document.uri.as_str(),
            moniker_kind,
            analysis::normalized_identifier(&declaration.symbol.name)
        ),
        unique: UniquenessLevel::Project,
        kind: Some(kind),
    }])?)
}

fn request_code_lenses(
    state: &mut ServerState,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) = request.extract::<CodeLensParams>("textDocument/codeLens")?;
    let uri = params.text_document.uri;
    if state.document(&uri).is_none() {
        return Ok(serde_json::Value::Null);
    }
    let functions = workspace_functions(state);
    let mut references_by_target = (0..functions.len())
        .map(|_| Vec::new())
        .collect::<Vec<Vec<lsp_types::Location>>>();
    for caller in &functions {
        let mapper = PositionMapper::new(&caller.document.text);
        for call in &caller.definition.calls {
            if let Some(target_index) = uniquely_resolved_function(&functions, &call.name) {
                references_by_target[target_index].push(lsp_types::Location {
                    uri: caller.document.uri.clone(),
                    range: mapper.range(&call.selection),
                });
            }
        }
    }

    let mut lenses = Vec::new();
    for (target_index, target) in functions
        .iter()
        .enumerate()
        .filter(|(_, function)| function.document.uri == uri)
    {
        let references = &mut references_by_target[target_index];
        references.sort_by(|left, right| {
            left.uri
                .as_str()
                .cmp(right.uri.as_str())
                .then_with(|| left.range.start.line.cmp(&right.range.start.line))
                .then_with(|| left.range.start.character.cmp(&right.range.start.character))
        });
        let mapper = PositionMapper::new(&target.document.text);
        let selection = &target.definition.selection;
        let lens_position = mapper.position(selection.start);
        let reference_count = references.len();
        let arguments = Some(vec![
            serde_json::to_value(&target.document.uri)?,
            serde_json::to_value(lens_position)?,
            serde_json::to_value(&*references)?,
        ]);
        lenses.push(CodeLens {
            range: mapper.range(selection),
            command: Some(Command {
                title: format!("{reference_count} incoming calls"),
                command: "editor.action.showReferences".to_owned(),
                arguments,
            }),
            data: None,
        });
    }
    Ok(serde_json::to_value(lenses)?)
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
    let result_id = state.next_semantic_token_result_id();
    state.semantic_tokens.insert(
        uri,
        SemanticTokenSnapshot {
            result_id: result_id.clone(),
            data: data.clone(),
        },
    );
    Ok(serde_json::to_value(SemanticTokens {
        result_id: Some(result_id),
        data,
    })?)
}

fn request_semantic_tokens_delta(
    state: &mut ServerState,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) =
        request.extract::<SemanticTokensDeltaParams>("textDocument/semanticTokens/full/delta")?;
    let uri = params.text_document.uri;
    let Some(document) = state.document(&uri) else {
        return Ok(serde_json::Value::Null);
    };
    let mapper = PositionMapper::new(&document.text);
    let data = semantic::semantic_tokens(&document.text, &mapper, None);
    let edits = state
        .semantic_tokens
        .get(&uri)
        .filter(|snapshot| snapshot.result_id == params.previous_result_id)
        .map(|snapshot| semantic_token_delta_edits(&snapshot.data, &data));
    let result_id = state.next_semantic_token_result_id();
    state.semantic_tokens.insert(
        uri,
        SemanticTokenSnapshot {
            result_id: result_id.clone(),
            data: data.clone(),
        },
    );
    match edits {
        Some(edits) => Ok(serde_json::json!({
            "resultId": result_id,
            "edits": edits,
        })),
        None => Ok(serde_json::to_value(SemanticTokens {
            result_id: Some(result_id),
            data,
        })?),
    }
}

fn semantic_token_delta_edits(
    previous: &[SemanticToken],
    current: &[SemanticToken],
) -> Vec<serde_json::Value> {
    let prefix = previous
        .iter()
        .zip(current)
        .take_while(|(previous, current)| previous == current)
        .count();
    let suffix = previous[prefix..]
        .iter()
        .rev()
        .zip(current[prefix..].iter().rev())
        .take_while(|(previous, current)| previous == current)
        .count();
    let delete_count = previous.len() - prefix - suffix;
    let inserted = current[prefix..current.len() - suffix]
        .iter()
        .flat_map(|token| {
            [
                token.delta_line,
                token.delta_start,
                token.length,
                token.token_type,
                token.token_modifiers_bitset,
            ]
        })
        .collect::<Vec<_>>();
    if delete_count == 0 && inserted.is_empty() {
        return Vec::new();
    }

    let mut edit = serde_json::Map::new();
    edit.insert(
        "start".to_owned(),
        serde_json::json!(u32::try_from(prefix * 5).expect("semantic token edit fits u32")),
    );
    edit.insert(
        "deleteCount".to_owned(),
        serde_json::json!(
            u32::try_from(delete_count * 5).expect("semantic token deletion fits u32")
        ),
    );
    if !inserted.is_empty() {
        edit.insert("data".to_owned(), serde_json::json!(inserted));
    }
    vec![serde_json::Value::Object(edit)]
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

fn request_inline_values(
    state: &mut ServerState,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) = request.extract::<InlineValueParams>("textDocument/inlineValue")?;
    let Some(document) = state.document(&params.text_document.uri) else {
        return Ok(serde_json::Value::Null);
    };
    let parse = analysis::parse_document(document);
    if !parse.diagnostics.is_empty() {
        return Ok(serde_json::to_value(Vec::<InlineValue>::new())?);
    }

    let mapper = PositionMapper::new(&document.text);
    let requested_start = mapper.byte_offset(params.range.start);
    let requested_end = mapper.byte_offset(params.range.end);
    let stopped_at = mapper.byte_offset(params.context.stopped_location.end);
    if requested_start > requested_end || stopped_at < requested_start || stopped_at > requested_end
    {
        return Ok(serde_json::to_value(Vec::<InlineValue>::new())?);
    }

    let mut values = crate::locals::visible_bindings(&parse.value, stopped_at)
        .into_iter()
        .filter_map(|binding| {
            let mut occurrences = crate::locals::references(&parse.value, &binding);
            occurrences.push(binding.selection);
            occurrences
                .into_iter()
                .filter(|span| {
                    span.start >= requested_start
                        && span.end <= requested_end
                        && span.end <= stopped_at
                })
                .max_by_key(|span| span.start)
                .map(|span| {
                    (
                        span.start,
                        InlineValue::VariableLookup(InlineValueVariableLookup {
                            range: mapper.range(&span),
                            variable_name: Some(binding.name),
                            case_sensitive_lookup: false,
                        }),
                    )
                })
        })
        .collect::<Vec<_>>();
    values.sort_by_key(|(start, _)| *start);
    Ok(serde_json::to_value(
        values
            .into_iter()
            .map(|(_, value)| value)
            .collect::<Vec<_>>(),
    )?)
}

fn request_linked_editing_range(
    state: &mut ServerState,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) =
        request.extract::<LinkedEditingRangeParams>("textDocument/linkedEditingRange")?;
    let uri = params.text_document_position_params.text_document.uri;
    let Some((document, segments, selected_start)) = combined_workspace(&state.documents, &uri)
    else {
        return Ok(serde_json::Value::Null);
    };
    let (parse, mapper) = parse_document(&document);
    if !parse.diagnostics.is_empty() {
        return Ok(serde_json::Value::Null);
    }
    let Some(position) = workspace_position(
        &document,
        selected_start,
        params.text_document_position_params.position,
        &state.documents,
        &uri,
    ) else {
        return Ok(serde_json::Value::Null);
    };
    let Some((name, _)) = analysis::identifier_at(&document, position, &mapper) else {
        return Ok(serde_json::Value::Null);
    };
    let Some(selected_document) = state.document(&uri) else {
        return Ok(serde_json::Value::Null);
    };
    let selected_mapper = PositionMapper::new(&selected_document.text);
    let mut ranges = analysis::references(&document, &parse, position, &mapper, true)
        .into_iter()
        .filter_map(|location| project_location(location, &mapper, &segments))
        .filter(|location| location.uri == uri)
        .filter_map(|location| {
            let start = selected_mapper.byte_offset(location.range.start);
            let end = selected_mapper.byte_offset(location.range.end);
            (selected_document.text.get(start..end) == Some(name.as_str()))
                .then_some((start, location.range))
        })
        .collect::<Vec<_>>();
    ranges.sort_by_key(|(start, _)| *start);
    ranges.dedup_by_key(|(start, range)| (*start, range.end));
    if ranges.len() < 2 {
        return Ok(serde_json::Value::Null);
    }
    Ok(serde_json::to_value(LinkedEditingRanges {
        ranges: ranges.into_iter().map(|(_, range)| range).collect(),
        word_pattern: None,
    })?)
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
    let (version, diagnostics) = if let Some(document) = state.document(&uri) {
        let mapper = PositionMapper::new(&document.text);
        (
            Some(document.version),
            analysis::check_document(document, &mapper),
        )
    } else {
        (None, Vec::new())
    };
    let result_id = diagnostic_result_id(&uri, version, &diagnostics);
    let report = if params.previous_result_id.as_deref() == Some(result_id.as_str()) {
        DocumentDiagnosticReport::Unchanged(RelatedUnchangedDocumentDiagnosticReport {
            related_documents: None,
            unchanged_document_diagnostic_report: UnchangedDocumentDiagnosticReport { result_id },
        })
    } else {
        DocumentDiagnosticReport::Full(RelatedFullDocumentDiagnosticReport {
            related_documents: None,
            full_document_diagnostic_report: FullDocumentDiagnosticReport {
                result_id: Some(result_id),
                items: diagnostics,
            },
        })
    };
    Ok(serde_json::to_value(report)?)
}

fn diagnostic_result_id(uri: &Uri, version: Option<i32>, diagnostics: &[Diagnostic]) -> String {
    // Result IDs are opaque to clients and are only compared within this
    // server session. Include the document version as well as the report so
    // same-version edits cannot accidentally retain a stale pull result.
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    "orna-syntax-v1-diagnostics".hash(&mut hasher);
    uri.as_str().hash(&mut hasher);
    version.hash(&mut hasher);
    serde_json::to_vec(diagnostics)
        .expect("serialisable syntax diagnostics")
        .hash(&mut hasher);
    format!("orna-syntax-v1-{:016x}", hasher.finish())
}

fn request_document_links(
    state: &mut ServerState,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) = request.extract::<DocumentLinkParams>("textDocument/documentLink")?;
    let links: Vec<DocumentLink> = state
        .document(&params.text_document.uri)
        .map(|document| analysis::document_links(document, &state.documents))
        .unwrap_or_default();
    Ok(serde_json::to_value(links)?)
}

fn request_folding_ranges(
    state: &mut ServerState,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) = request.extract::<FoldingRangeParams>("textDocument/foldingRange")?;
    let Some(document) = state.document(&params.text_document.uri) else {
        return Ok(serde_json::Value::Null);
    };
    let parse = analysis::parse_document(document);
    let mapper = PositionMapper::new(&document.text);
    let ranges: Vec<FoldingRange> =
        editor_ranges::folding_ranges(&parse.value, &document.text, &mapper);
    Ok(serde_json::to_value(ranges)?)
}

fn request_selection_ranges(
    state: &mut ServerState,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) = request.extract::<SelectionRangeParams>("textDocument/selectionRange")?;
    let Some(document) = state.document(&params.text_document.uri) else {
        return Ok(serde_json::Value::Null);
    };
    let parse = analysis::parse_document(document);
    let mapper = PositionMapper::new(&document.text);
    let ranges =
        editor_ranges::selection_ranges(&parse.value, &document.text, &params.positions, &mapper);
    Ok(serde_json::to_value(ranges)?)
}
