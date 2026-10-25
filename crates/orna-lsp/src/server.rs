//! The LSP server loop and request dispatch.
//!
//! The loop applies notifications in order and hands every request, with a
//! snapshot of the open documents it arrived after, to worker threads
//! (`crate::requests`). Requests read analyses cached per document version.
//! Diagnostics run the project check on a separate worker
//! (`crate::diagnostics`), so edits never wait for semantic analysis.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io::{self, BufReader};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};

use crate::analysis;
use crate::diagnostics::{self, DiagnosticsWorker, Publisher, Snapshot};
use crate::documents::{Document, PositionMapper};
use crate::names::{DeclaredKind, Role};
use crate::project::{ProjectHome, ProjectLocator};
use crate::rename::{self, ModuleEdits, Refusal};
use crate::requests::RequestPool;
use crate::views::{Documents, Views};
use crate::workspace::{OpenDocument, Target, Workspace};
use crate::{editor_ranges, hierarchy, inlay, semantic};
use lsp_server::{
    Connection, ErrorCode, Message, Notification, Request, RequestId, Response, ResponseError,
};
use orna_project_v1::{ObjectIdentityManifest, ObjectIdentityRename, ObjectKind, ProjectLoader};
use orna_repository_v1::{Repository, RepositoryFormat};
use lsp_types::{
    CallHierarchyOptions, CallHierarchyServerCapability, CodeAction, CodeActionKind,
    CodeActionOptions, CodeActionOrCommand, CodeActionParams, CodeActionProviderCapability,
    CodeLensOptions, CodeLensParams, CompletionOptions, CompletionParams, CompletionResponse,
    Diagnostic, DiagnosticOptions, DiagnosticServerCapabilities, DocumentDiagnosticParams,
    DocumentDiagnosticReport, DocumentFormattingParams, DocumentHighlight, DocumentHighlightKind,
    DocumentHighlightParams, DocumentLink, DocumentLinkOptions, DocumentLinkParams,
    DocumentRangeFormattingParams, DocumentSymbolParams, DocumentSymbolResponse, FoldingRange,
    FoldingRangeParams, FoldingRangeProviderCapability, FullDocumentDiagnosticReport,
    GotoDefinitionParams, GotoDefinitionResponse, Hover, HoverParams, HoverProviderCapability,
    InitializeParams, InlayHintOptions, InlayHintParams, InlayHintServerCapabilities, InlineValue,
    InlineValueOptions, InlineValueParams, InlineValueServerCapabilities,
    InlineValueVariableLookup, LinkedEditingRangeParams, LinkedEditingRangeServerCapabilities,
    LinkedEditingRanges, MonikerParams, NumberOrString, OneOf, Position, PositionEncodingKind,
    PrepareRenameResponse, Range, ReferenceParams, RelatedFullDocumentDiagnosticReport,
    RelatedUnchangedDocumentDiagnosticReport, RenameOptions, RenameParams, SelectionRangeParams,
    SelectionRangeProviderCapability, SemanticToken, SemanticTokens, SemanticTokensDeltaParams,
    SemanticTokensFullOptions, SemanticTokensLegend, SemanticTokensOptions, SemanticTokensParams,
    SemanticTokensRangeParams, SemanticTokensServerCapabilities, ServerCapabilities,
    SignatureHelpOptions, TextDocumentContentChangeEvent, TextDocumentSyncCapability,
    TextDocumentSyncKind, TextDocumentSyncOptions, TextDocumentSyncSaveOptions, TextEdit,
    TypeHierarchyPrepareParams, TypeHierarchySubtypesParams, TypeHierarchySupertypesParams,
    UnchangedDocumentDiagnosticReport, Uri, WorkspaceEdit,
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

struct PendingIdentityRename {
    rename: ObjectIdentityRename,
    requested_version: i32,
    pre_edit_text: String,
    // Exact full text produced by the returned WorkspaceEdit for this document.
    expected_text: String,
    applied_version: Option<i32>,
}

/// Identity renames returned to the client and not yet saved, per document.
type PendingRenames = HashMap<Uri, Vec<Vec<PendingIdentityRename>>>;

/// What requests read: the open documents when they arrived, and the caches
/// every request shares. Each request runs on a worker thread with its own
/// clone.
#[derive(Clone, Default)]
struct RequestContext {
    documents: Arc<Documents>,
    projects: Arc<ProjectLocator>,
    views: Arc<Views>,
    semantic_tokens: Arc<Mutex<SemanticTokenResults>>,
    pending_identity_renames: Arc<Mutex<PendingRenames>>,
    /// Sends `window/showMessage` to the client from request workers.
    client: Option<Arc<dyn Fn(Message) + Send + Sync>>,
}

/// The last semantic tokens sent for each document, for delta requests.
#[derive(Default)]
struct SemanticTokenResults {
    by_uri: HashMap<Uri, SemanticTokenSnapshot>,
    next: u64,
}

struct SemanticTokenSnapshot {
    result_id: String,
    data: Vec<SemanticToken>,
}

impl RequestContext {
    fn document(&self, uri: &Uri) -> Option<&Document> {
        self.documents.get(uri).map(|open| &open.document)
    }

    /// Shows the client a warning the user must see, as `window/showMessage`.
    fn show_warning(&self, message: &str) {
        if let Some(client) = &self.client {
            let params = lsp_types::ShowMessageParams {
                typ: lsp_types::MessageType::WARNING,
                message: message.to_owned(),
            };
            client(Message::Notification(Notification {
                method: "window/showMessage".to_owned(),
                params: serde_json::to_value(params).unwrap_or_default(),
            }));
        }
    }

    /// The open documents, for diagnostics.
    fn snapshot(&self) -> Snapshot {
        Arc::new(self.documents.values().cloned().collect())
    }

    /// The workspace containing `uri`, its module, and the byte offset of
    /// `position` in it.
    fn locate(&self, uri: &Uri, position: Position) -> Option<(Arc<Workspace>, usize, usize)> {
        let (workspace, module) = self.views.workspace(&self.documents, uri)?;
        let byte = PositionMapper::new(workspace.file(module).text()).byte_offset(position);
        Some((workspace, module, byte))
    }

    /// Records the tokens sent for `uri` and returns their result id.
    fn record_semantic_tokens(&self, uri: Uri, data: Vec<SemanticToken>) -> String {
        let mut results = lock(&self.semantic_tokens);
        results.next += 1;
        let result_id = format!("orna-semantic-{}", results.next);
        results.by_uri.insert(
            uri,
            SemanticTokenSnapshot {
                result_id: result_id.clone(),
                data,
            },
        );
        result_id
    }
}

/// The server loop's state: the request context it keeps current, and the
/// workers that answer requests and publish diagnostics.
struct ServerState {
    context: RequestContext,
    /// Publishes diagnostics; absent when no client is attached.
    diagnostics: Option<DiagnosticsWorker>,
    /// Answers requests; absent in tests, which call `dispatch` directly.
    requests: Option<RequestPool<RequestContext>>,
    /// Sends messages to the client; absent in tests.
    client: Option<Arc<dyn Fn(Message) + Send + Sync>>,
}

impl ServerState {
    fn new() -> Self {
        Self {
            context: RequestContext::default(),
            diagnostics: None,
            requests: None,
            client: None,
        }
    }

    /// Shows the client an error the user must see, as `window/showMessage`.
    fn show_error(&self, message: &str) {
        if let Some(client) = &self.client {
            let params = lsp_types::ShowMessageParams {
                typ: lsp_types::MessageType::ERROR,
                message: message.to_owned(),
            };
            client(Message::Notification(Notification {
                method: "window/showMessage".to_owned(),
                params: serde_json::to_value(params).unwrap_or_default(),
            }));
        }
    }

    /// Opens or replaces one document version.
    fn set_document(&mut self, document: Document, home: Option<ProjectHome>) {
        Arc::make_mut(&mut self.context.documents).insert(
            document.uri.clone(),
            Arc::new(OpenDocument::new(document, home)),
        );
    }

    /// Queues diagnostics for `uri` and whatever its project check changes.
    fn schedule_diagnostics(&self, uri: &Uri, home: Option<ProjectHome>) {
        if let Some(worker) = &self.diagnostics {
            worker.schedule(self.context.snapshot(), vec![(uri.clone(), home)]);
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A request that cannot be served, with a reason for the user
/// (`RequestFailed`, -32803).
#[derive(Debug)]
struct RequestFailed(String);

impl std::fmt::Display for RequestFailed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for RequestFailed {}

/// Worker threads answering requests: enough that a slow request leaves
/// others answered, few enough to leave cores to the diagnostics check.
fn request_threads() -> usize {
    thread::available_parallelism().map_or(2, |count| count.get().clamp(2, 4))
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
    let sender = connection.sender.clone();
    state.diagnostics = Some(DiagnosticsWorker::spawn(Publisher::new(move |message| {
        let _ = sender.send(message);
    })));
    let sender = connection.sender.clone();
    state.requests = Some(RequestPool::spawn(
        request_threads(),
        Arc::new(dispatch),
        Arc::new(move |message| {
            let _ = sender.send(message);
        }),
    ));
    let sender = connection.sender.clone();
    state.client = Some(Arc::new(move |message| {
        let _ = sender.send(message);
    }));
    state.context.client = state.client.clone();

    for message in &connection.receiver {
        match message {
            Message::Request(request) => {
                if connection.handle_shutdown(&request)? {
                    break;
                }
                if let Some(requests) = &state.requests {
                    requests.submit(state.context.clone(), request);
                }
            }
            Message::Notification(notification) => {
                if handle_notification(&mut state, notification) {
                    break;
                }
            }
            Message::Response(_) => {}
        }
    }

    // Stop the request and diagnostics workers, which hold senders, then
    // drop the connection: the writer thread terminates only when every
    // sender is gone.
    drop(state);
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
            "serverInfo": {
                "name": "orna-lsp",
                "version": env!("CARGO_PKG_VERSION"),
            },
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
        document_formatting_provider: Some(OneOf::Left(true)),
        document_range_formatting_provider: Some(OneOf::Left(true)),
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
            identifier: Some(analysis::DIAGNOSTIC_SOURCE.to_owned()),
            inter_file_dependencies: true,
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

/// Answers one request against the state it arrived after.
fn dispatch(state: &RequestContext, request: Request) -> Result<serde_json::Value, ResponseError> {
    let method = request.method.clone();
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
        "textDocument/formatting" => request_formatting(state, request),
        "textDocument/rangeFormatting" => request_range_formatting(state, request),
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
            return Err(ResponseError {
                code: ErrorCode::MethodNotFound as i32,
                message: format!("unknown method {method}"),
                data: None,
            });
        }
    };
    result.map_err(|error| ResponseError {
        code: if error.downcast_ref::<RequestFailed>().is_some() {
            ErrorCode::RequestFailed as i32
        } else {
            ErrorCode::InternalError as i32
        },
        message: error.to_string(),
        data: None,
    })
}

/// Handles one client notification. Returns true when the server must exit.
fn handle_notification(state: &mut ServerState, notification: Notification) -> bool {
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
            lock(&state.context.pending_identity_renames).remove(&uri);
            lock(&state.context.semantic_tokens).by_uri.remove(&uri);
            let home = state.context.projects.home(&uri);
            state.set_document(document, home.clone());
            state.schedule_diagnostics(&uri, home);
            false
        }
        "textDocument/didChange" => {
            let Ok(params) = serde_json::from_value::<lsp_types::DidChangeTextDocumentParams>(
                notification.params,
            ) else {
                return false;
            };
            let uri = params.text_document.uri.clone();
            let version = params.text_document.version;
            let Some(open) = state.context.documents.get(&uri).cloned() else {
                return false;
            };
            if version <= open.document.version {
                return false;
            }
            let Some(text) = apply_content_changes(&open.document.text, &params.content_changes)
            else {
                return false;
            };
            mark_applied_identity_rename(&state.context, &uri, version, &text);
            state.set_document(Document::new(uri.clone(), text, version), open.home.clone());
            state.schedule_diagnostics(&uri, open.home.clone());
            false
        }
        "textDocument/didSave" => {
            if let Ok(params) =
                serde_json::from_value::<lsp_types::DidSaveTextDocumentParams>(notification.params)
            {
                let uri = params.text_document.uri;
                if let Err(error) = persist_pending_rename_identity(&state.context, &uri) {
                    state.context.show_warning(&identity_not_carried(&error));
                }
                // Saved files are read from disk for closed modules.
                state.context.views.invalidate();
                if let Some(open) = state.context.documents.get(&uri) {
                    state.schedule_diagnostics(&uri, open.home.clone());
                }
            }
            false
        }
        "textDocument/didClose" => {
            if let Ok(params) =
                serde_json::from_value::<lsp_types::DidCloseTextDocumentParams>(notification.params)
            {
                let uri = params.text_document.uri;
                lock(&state.context.pending_identity_renames).remove(&uri);
                lock(&state.context.semantic_tokens).by_uri.remove(&uri);
                let home = Arc::make_mut(&mut state.context.documents)
                    .remove(&uri)
                    .and_then(|open| open.home.clone());
                state.schedule_diagnostics(&uri, home);
            }
            false
        }
        "workspace/didChangeWatchedFiles" => {
            state.context.views.invalidate();
            false
        }
        "$/cancelRequest" => {
            if let Some(requests) = &state.requests
                && let Ok(params) =
                    serde_json::from_value::<lsp_types::CancelParams>(notification.params)
            {
                requests.cancel(&match params.id {
                    NumberOrString::Number(id) => RequestId::from(id),
                    NumberOrString::String(id) => RequestId::from(id),
                });
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

fn request_hover(
    state: &RequestContext,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) = request.extract::<HoverParams>("textDocument/hover")?;
    let uri = params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;
    let Some((workspace, module, byte)) = state.locate(&uri, position) else {
        return Ok(serde_json::Value::Null);
    };
    let hover: Option<Hover> = analysis::hover(&workspace, module, byte);
    Ok(serde_json::to_value(hover)?)
}

fn request_signature_help(
    state: &RequestContext,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) =
        request.extract::<lsp_types::SignatureHelpParams>("textDocument/signatureHelp")?;
    let uri = params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;
    let Some((workspace, module, byte)) = state.locate(&uri, position) else {
        return Ok(serde_json::Value::Null);
    };
    let help = analysis::signature_help(&workspace, module, byte);
    Ok(serde_json::to_value(help)?)
}

fn request_code_actions(
    state: &RequestContext,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) = request.extract::<CodeActionParams>("textDocument/codeAction")?;
    let uri = params.text_document.uri;
    if !uri.as_str().ends_with(".orna") || !requests_quickfix(params.context.only.as_deref()) {
        return Ok(serde_json::to_value(Vec::<CodeActionOrCommand>::new())?);
    }
    let Some(open) = state.documents.get(&uri) else {
        return Ok(serde_json::to_value(Vec::<CodeActionOrCommand>::new())?);
    };
    let file = open.analysis();
    let document = &open.document;
    let parse = &file.parse;
    let mapper = PositionMapper::new(&document.text);
    let diagnostics = analysis::syntax_diagnostics(&uri, parse, &mapper);
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
            missing_semicolon_position(document, &file.tokens, parse, &diagnostic, &mapper)
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
    tokens: &[orna_syntax_v1::Token],
    parse: &analysis::EditorParse,
    diagnostic: &Diagnostic,
    mapper: &PositionMapper<'_>,
) -> Option<Position> {
    let diagnostic_byte = mapper.byte_offset(diagnostic.range.start);
    let previous = tokens
        .iter()
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
    let repaired_parse = orna_syntax_v1::parse_module(&repaired_source);
    let diagnostic_count = |parse: &analysis::EditorParse| {
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
    state: &RequestContext,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) = request.extract::<GotoDefinitionParams>("textDocument/definition")?;
    let uri = params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;
    let Some((workspace, module, byte)) = state.locate(&uri, position) else {
        return Ok(serde_json::Value::Null);
    };
    let response =
        analysis::definition(&workspace, module, byte).map(GotoDefinitionResponse::Scalar);
    Ok(serde_json::to_value(response)?)
}

fn request_references(
    state: &RequestContext,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) = request.extract::<ReferenceParams>("textDocument/references")?;
    let uri = params.text_document_position.text_document.uri;
    let position = params.text_document_position.position;
    let Some((workspace, module, byte)) = state.locate(&uri, position) else {
        return Ok(serde_json::Value::Null);
    };
    let locations = analysis::references(
        &workspace,
        module,
        byte,
        params.context.include_declaration,
    );
    Ok(serde_json::to_value(locations)?)
}

fn request_document_highlights(
    state: &RequestContext,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) =
        request.extract::<DocumentHighlightParams>("textDocument/documentHighlight")?;
    let uri = params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;
    let Some((workspace, module, byte)) = state.locate(&uri, position) else {
        return Ok(serde_json::Value::Null);
    };
    let Some(target) = workspace
        .resolve_at(module, byte)
        .and_then(|(_, resolved)| resolved.target())
    else {
        return Ok(serde_json::to_value(Vec::<DocumentHighlight>::new())?);
    };
    let names = &workspace.file(module).names;
    let mapper = PositionMapper::new(workspace.file(module).text());
    let highlights = workspace
        .search(&target)
        .found
        .into_iter()
        .filter(|(found_module, _)| *found_module == module)
        .map(|(_, span)| {
            let declaration = names.binding_at(&span).is_some_and(|binding| {
                let selection = &names.binding(binding).selection;
                selection.start == span.start && selection.end == span.end
            }) || names.declared_at(&span).is_some();
            let write = names.occurrence(&span).is_some_and(|occurrence| {
                matches!(
                    occurrence.role,
                    Role::Value { write: true } | Role::Member { write: true, .. }
                )
            });
            DocumentHighlight {
                kind: Some(if declaration || write {
                    DocumentHighlightKind::WRITE
                } else {
                    DocumentHighlightKind::READ
                }),
                range: mapper.range(&span),
            }
        })
        .collect::<Vec<_>>();
    Ok(serde_json::to_value(highlights)?)
}

/// The table or field identity rename a rename request implies. `Ok(None)`
/// when the file is outside a repository or the rename changes no identity.
fn identity_rename_for_request(
    state: &RequestContext,
    uri: &Uri,
    position: Position,
    new_name: &str,
) -> Result<Option<ObjectIdentityRename>, String> {
    let Some(document) = state.document(uri) else { return Ok(None); };
    let Ok(url) = url::Url::parse(uri.as_str()) else { return Ok(None); };
    let Ok(path) = url.to_file_path() else { return Ok(None); };
    // Outside a repository there is no identity store to keep.
    let Ok(repository) = Repository::discover(&path) else { return Ok(None); };
    let Ok(relative) = path.strip_prefix(repository.worktree()) else { return Ok(None); };
    let logical_path = relative.to_string_lossy().replace('\\', "/");
    let Some(old_name) = identifier_at(&document.text, position) else { return Ok(None); };
    let namespace = module_namespace(&logical_path);
    let qualify = |name: &str| if namespace.is_empty() { name.to_owned() } else { format!("{namespace}.{name}") };
    let table = qualify(&old_name);
    let pending_renames = lock(&state.pending_identity_renames);
    let pending = pending_renames.get(uri).map(Vec::as_slice).unwrap_or(&[]);

    if pending.iter().filter_map(|chain| chain.last().map(|step| &step.rename)).any(|rename| {
        rename.kind() == ObjectKind::Table && rename.new_qualified_name() == table
    }) {
        return Ok(Some(ObjectIdentityRename::new(ObjectKind::Table, table, qualify(new_name))));
    }

    let suffix = format!(".{old_name}");
    let pending_fields = pending.iter().filter_map(|chain| chain.last().map(|step| &step.rename)).filter(|rename| {
        let target = rename.new_qualified_name();
        rename.kind() == ObjectKind::Field
            && target.ends_with(&suffix)
            && target[..target.len() - suffix.len()].starts_with(&namespace)
    }).collect::<Vec<_>>();
    if pending_fields.len() > 1 {
        return Err(format!("the field name {old_name} matches several pending renames"));
    }
    if let Some(rename) = pending_fields.first() {
        let current = rename.new_qualified_name();
        let parent = &current[..current.len() - old_name.len()];
        return Ok(Some(ObjectIdentityRename::new(
            ObjectKind::Field, current, format!("{parent}{new_name}"),
        )));
    }

    let Some(previous) = staged_identities(&repository)? else {
        return Err("the staged object identities are missing".to_owned());
    };
    if previous.id(ObjectKind::Table, &table).is_some() {
        return Ok(Some(ObjectIdentityRename::new(ObjectKind::Table, table, qualify(new_name))));
    }
    let mut matches = previous.records().iter().filter(|record| {
        !record.is_retired() && record.kind() == ObjectKind::Field && record.qualified_name().ends_with(&suffix)
            && record.qualified_name()[..record.qualified_name().len() - suffix.len()].starts_with(&namespace)
    });
    let Some(record) = matches.next() else { return Ok(None); };
    if matches.next().is_some() { return Err(format!("the field name {old_name} matches several fields")); }
    let parent = &record.qualified_name()[..record.qualified_name().len() - old_name.len()];
    Ok(Some(ObjectIdentityRename::new(
        ObjectKind::Field,
        record.qualified_name().to_owned(),
        format!("{parent}{new_name}"),
    )))
}

/// Reads the identity inventory the staged index carries, refusing a store
/// that is not retained, so renames never write to an unverified store.
fn staged_identities(repository: &Repository) -> Result<Option<ObjectIdentityManifest>, String> {
    repository
        .refuse_unretained_committed_store()
        .map_err(|error| format!("{error:?}"))?;
    require_identity_format(repository)?;
    let staged = repository
        .capture_staged_candidate()
        .map_err(|error| format!("{error:?}"))?;
    ObjectIdentityManifest::read_staged(repository, &staged).map_err(|error| format!("{error:?}"))
}

/// Object identities exist only in format 3. Older repositories refuse the
/// carry rather than renaming without an identity.
fn require_identity_format(repository: &Repository) -> Result<(), String> {
    let format = repository
        .open_format_context()
        .map_err(|error| format!("{error:?}"))?;
    if format.repository_format() == RepositoryFormat::Format3 {
        Ok(())
    } else {
        Err("this repository uses a legacy format without object identities".to_owned())
    }
}

/// Whether a rename of `target` concerns an object identity. Only declared
/// tables and fields have one; local names, functions and modules do not.
fn carries_identity(workspace: &Workspace, target: &Target) -> bool {
    match target {
        Target::Declared { module, index } => matches!(
            workspace.declaration(*module, *index).kind,
            DeclaredKind::Table
                | DeclaredKind::Field
                | DeclaredKind::KeyField
                | DeclaredKind::PayloadField
        ),
        _ => false,
    }
}

/// The warning for an object identity that a rename or save did not carry.
/// The text is already changed, so the remedy is `orna mv`.
fn identity_not_carried(reason: &str) -> String {
    format!(
        "ORNA-LSP-RENAME-001: object identity was not carried ({reason}); use orna mv to rename a table with its identity"
    )
}

/// Saves the identity renames the saved source applies, into the staged store.
/// Every pending rename that cannot be carried is returned as an error for the
/// caller to report; the saved text is saved either way.
fn persist_pending_rename_identity(state: &RequestContext, uri: &Uri) -> Result<(), String> {
    let Some(chains) = lock(&state.pending_identity_renames).remove(uri) else { return Ok(()); };
    let url = url::Url::parse(uri.as_str()).map_err(|error| format!("{error:?}"))?;
    let path = url.to_file_path().map_err(|()| format!("{uri:?} is not a file"))?;
    let Some(open) = state.documents.get(uri) else {
        return Err("the document is no longer open".to_owned());
    };
    let saved_version = open.document.version;
    let saved_text = std::fs::read_to_string(&path).map_err(|error| format!("{error:?}"))?;
    if saved_text != open.document.text {
        return Err("the saved text differs from the editor buffer".to_owned());
    }
    let repository = Repository::discover(&path).map_err(|error| format!("{error:?}"))?;
    let project = ProjectLoader::default()
        .load(&repository)
        .map_err(|error| format!("{error:?}"))?;
    let modules = project.into_modules();
    repository
        .refuse_unretained_committed_store()
        .map_err(|error| format!("{error:?}"))?;
    require_identity_format(&repository)?;
    let staged = repository
        .capture_staged_candidate()
        .map_err(|error| format!("{error:?}"))?;
    let previous = ObjectIdentityManifest::read_staged(&repository, &staged)
        .map_err(|error| format!("{error:?}"))?
        .ok_or_else(|| "the staged object identities are missing".to_owned())?;
    // Intermediate destinations are not in the saved source, so the furthest
    // applied step that reconciles against it is the one saved.
    // `Ok(None)`: no step of the chain reached the saved text, so the rename was
    // declined and there is no identity to carry.
    let rename_for = |chain: &[PendingIdentityRename]| -> Result<Option<ObjectIdentityRename>, String> {
        let Some(first) = chain.first() else { return Ok(None); };
        let applied = chain
            .iter()
            .take_while(|step| step.applied_version.is_some_and(|version| version <= saved_version))
            .collect::<Vec<_>>();
        if applied.is_empty() { return Ok(None); }
        let mut reason = format!(
            "the saved source does not show the rename of {}",
            first.rename.old_qualified_name()
        );
        for step in applied.into_iter().rev() {
            let rename = ObjectIdentityRename::new(
                first.rename.kind(),
                first.rename.old_qualified_name(),
                step.rename.new_qualified_name(),
            );
            match previous.with_renames(&modules, std::slice::from_ref(&rename)) {
                Ok(_) => return Ok(Some(rename)),
                Err(error) => reason = format!("{error:?}"),
            }
        }
        Err(reason)
    };
    let mut applicable = Vec::new();
    let mut failures = Vec::new();
    for chain in &chains {
        match rename_for(chain) {
            Ok(Some(rename)) => applicable.push(rename),
            Ok(None) => {}
            Err(reason) => failures.push(reason),
        }
    }
    if !applicable.is_empty() {
        let next = applicable.iter().try_fold(previous, |manifest, rename| {
            manifest
                .with_renames(&modules, std::slice::from_ref(rename))
                .map_err(|error| format!("{error:?}"))
        })?;
        let relative = path
            .strip_prefix(repository.worktree())
            .map_err(|error| format!("{error:?}"))?;
        let logical_path = relative.to_string_lossy().replace('\\', "/");
        repository
            .stage_object_identities(
                &staged,
                &next.to_stored(),
                &[(logical_path.as_str(), saved_text.as_bytes())],
            )
            .map_err(|error| format!("{error:?}"))?;
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

fn mark_applied_identity_rename(state: &RequestContext, uri: &Uri, version: i32, text: &str) {
    let mut pending = lock(&state.pending_identity_renames);
    let Some(chains) = pending.get_mut(uri) else { return; };
    for step in chains.iter_mut().flatten() {
        if version > step.requested_version && text == step.expected_text {
            step.applied_version = Some(version);
        }
    }
}

fn module_namespace(path: &str) -> String {
    let mut parts = path.strip_suffix(".orna").unwrap_or(path).split('/').collect::<Vec<_>>();
    let file = parts.pop().unwrap_or_default();
    if file != "main" { parts.push(file); }
    parts.join(".")
}

fn identifier_at(source: &str, position: Position) -> Option<String> {
    let mapper = PositionMapper::new(source);
    let byte = mapper.byte_offset(position).min(source.len());
    let start = source[..byte].char_indices().rev().find_map(|(index, ch)| (!is_identifier_char(ch)).then_some(index + ch.len_utf8())).unwrap_or(0);
    let end = source[byte..].char_indices().find_map(|(offset, ch)| (!is_identifier_char(ch)).then_some(byte + offset)).unwrap_or(source.len());
    let identifier = source.get(start..end)?;
    (!identifier.is_empty()).then(|| identifier.to_owned())
}

fn is_identifier_char(ch: char) -> bool { ch == '_' || ch.is_alphanumeric() }

fn apply_text_edits(source: &str, edits: &[TextEdit]) -> Option<String> {
    let mapper = PositionMapper::new(source);
    let mut edits = edits.iter().map(|edit| {
        Some((mapper.byte_offset(edit.range.start), mapper.byte_offset(edit.range.end), edit.new_text.as_str()))
    }).collect::<Option<Vec<_>>>()?;
    edits.sort_by_key(|(start, _, _)| std::cmp::Reverse(*start));
    let mut updated = source.to_owned();
    for (start, end, replacement) in edits {
        if start > end || end > updated.len() { return None; }
        updated.replace_range(start..end, replacement);
    }
    Some(updated)
}
fn request_rename(
    state: &RequestContext,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) = request.extract::<RenameParams>("textDocument/rename")?;
    let uri = params.text_document_position.text_document.uri;
    let position = params.text_document_position.position;
    let Some((workspace, module, byte)) = state.locate(&uri, position) else {
        return Err(RequestFailed("cannot rename: the document is not open".to_owned()).into());
    };
    let (renamable, edits) = rename::plan(&workspace, module, byte, &params.new_name).map_err(refused)?;
    if let Some(home) = state.documents.get(&uri).and_then(|open| open.home.clone()) {
        verify_project_check(state, &home, &workspace, &edits)?;
    }
    let changes = workspace_changes(&workspace, &edits);
    // Only declared tables and fields have identities. Local names, functions
    // and modules never consult the identity store, so they never warn. An
    // identity that cannot be carried never fails an ordinary rename: the plain
    // workspace edit is returned with a warning.
    let identity = if carries_identity(&workspace, &renamable.target) {
        identity_rename_for_request(state, &uri, position, &params.new_name).unwrap_or_else(|error| {
            state.show_warning(&identity_not_carried(&error));
            None
        })
    } else {
        None
    };
    if let Some(rename) = identity {
        let matched = state.document(&uri).and_then(|document| {
            changes
                .get(&uri)
                .and_then(|edits| apply_text_edits(&document.text, edits))
                .map(|expected_text| (document, expected_text))
        });
        match matched {
            Some((document, expected_text)) => {
                let requested_version = document.version;
                let pre_edit_text = document.text.clone();
                let mut pending_renames = lock(&state.pending_identity_renames);
                let pending = pending_renames.entry(uri.clone()).or_default();
                let continues = pending.iter().position(|chain| {
                    chain.last().is_some_and(|last| {
                        last.rename.kind() == rename.kind()
                            && last.rename.new_qualified_name() == rename.old_qualified_name()
                            && last.expected_text == pre_edit_text
                    })
                });
                let step = PendingIdentityRename {
                    rename,
                    requested_version,
                    pre_edit_text,
                    expected_text,
                    applied_version: None,
                };
                match continues {
                    Some(index) => pending[index].push(step),
                    None => pending.push(vec![step]),
                }
            }
            None => state.show_warning(&identity_not_carried(
                "the renamed text could not be matched to the document",
            )),
        }
    }
    let workspace_edit = WorkspaceEdit {
        changes: Some(changes),
        document_changes: None,
        change_annotations: None,
    };
    Ok(serde_json::to_value(workspace_edit)?)
}

fn refused(refusal: Refusal) -> Box<dyn std::error::Error + Send + Sync> {
    Box::new(RequestFailed(format!("cannot rename: {}", refusal.0)))
}

/// Text edits per document URI.
fn workspace_changes(workspace: &Workspace, edits: &ModuleEdits) -> HashMap<Uri, Vec<TextEdit>> {
    edits
        .iter()
        .map(|(module, module_edits)| {
            let file = workspace.file(*module);
            let mapper = PositionMapper::new(file.text());
            (
                file.uri().clone(),
                module_edits
                    .iter()
                    .map(|(span, new_text)| TextEdit {
                        range: mapper.range(span),
                        new_text: new_text.clone(),
                    })
                    .collect(),
            )
        })
        .collect()
}

/// Refuses a rename after which `orna check` would report a diagnostic it
/// does not report now.
fn verify_project_check(
    state: &RequestContext,
    home: &ProjectHome,
    workspace: &Workspace,
    edits: &ModuleEdits,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let before = state
        .documents
        .values()
        .filter_map(|open| {
            let open_home = open.home.as_ref()?;
            Arc::ptr_eq(&open_home.project, &home.project)
                .then(|| (open_home.logical_path.clone(), open.document.text.clone()))
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut after = before.clone();
    for (module, module_edits) in edits {
        if let Some(logical_path) = &workspace.modules[*module].logical_path {
            after.insert(
                logical_path.clone(),
                rename::apply(workspace.file(*module).text(), module_edits),
            );
        }
    }
    let counts_before = check_counts(&home.project, &before);
    for ((logical_path, code, message), count) in check_counts(&home.project, &after) {
        let previous = counts_before
            .get(&(logical_path.clone(), code.clone(), message.clone()))
            .copied()
            .unwrap_or(0);
        if count > previous {
            return Err(Box::new(RequestFailed(format!(
                "cannot rename: `orna check` would then report {code} in {logical_path}: {}",
                message.lines().next().unwrap_or_default()
            ))));
        }
    }
    Ok(())
}

fn check_counts(
    project: &crate::project::Project,
    sources: &std::collections::BTreeMap<String, String>,
) -> std::collections::BTreeMap<(String, String, String), usize> {
    let open = sources
        .iter()
        .map(|(logical_path, text)| crate::project::OpenModule {
            logical_path,
            text,
            has_syntax_errors: !orna_syntax_v1::parse_module(text).diagnostics.is_empty(),
        })
        .collect::<Vec<_>>();
    let mut counts = std::collections::BTreeMap::new();
    for (logical_path, diagnostics) in crate::project::check(project, &open).diagnostics {
        for diagnostic in diagnostics {
            let code = match &diagnostic.code {
                Some(NumberOrString::String(code)) => code.clone(),
                Some(NumberOrString::Number(code)) => code.to_string(),
                None => String::new(),
            };
            *counts
                .entry((logical_path.clone(), code, diagnostic.message))
                .or_insert(0) += 1;
        }
    }
    counts
}

fn request_prepare_rename(
    state: &RequestContext,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) =
        request.extract::<lsp_types::TextDocumentPositionParams>("textDocument/prepareRename")?;
    let Some((workspace, module, byte)) = state.locate(&params.text_document.uri, params.position)
    else {
        return Ok(serde_json::Value::Null);
    };
    let renamable = rename::prepare(&workspace, module, byte).map_err(refused)?;
    let mapper = PositionMapper::new(workspace.file(module).text());
    Ok(serde_json::to_value(PrepareRenameResponse::RangeWithPlaceholder {
        range: mapper.range(&renamable.span),
        placeholder: renamable.placeholder,
    })?)
}

fn request_document_symbols(
    state: &RequestContext,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) = request.extract::<DocumentSymbolParams>("textDocument/documentSymbol")?;
    let uri = params.text_document.uri;
    let Some(open) = state.documents.get(&uri) else {
        return Ok(serde_json::Value::Null);
    };
    let file = open.analysis();
    let mapper = PositionMapper::new(file.text());
    let symbols = analysis::document_symbols(&file.symbols, &mapper);
    let response = DocumentSymbolResponse::Nested(symbols);
    Ok(serde_json::to_value(response)?)
}

fn request_workspace_symbols(
    state: &RequestContext,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) = request.extract::<lsp_types::WorkspaceSymbolParams>("workspace/symbol")?;
    let query = params.query.trim().to_lowercase();
    let mut symbols = Vec::new();
    for open in state
        .documents
        .values()
        .filter(|open| open.document.uri.as_str().ends_with(".orna"))
    {
        let file = open.analysis();
        if !file.parse.diagnostics.is_empty() {
            continue;
        }
        let mapper = PositionMapper::new(file.text());
        for symbol in analysis::document_symbols(&file.symbols, &mapper) {
            if let Some(score) = workspace_symbol_score(&symbol.name, &query) {
                let range = symbol.selection_range;
                symbols.push((
                    score,
                    symbol.name.to_lowercase(),
                    file.uri().as_str().to_owned(),
                    range.start,
                    lsp_types::WorkspaceSymbol {
                        name: symbol.name,
                        kind: symbol.kind,
                        tags: symbol.tags,
                        container_name: None,
                        location: lsp_types::OneOf::Left(lsp_types::Location {
                            uri: file.uri().clone(),
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

fn request_prepare_call_hierarchy(
    state: &RequestContext,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) = request
        .extract::<lsp_types::CallHierarchyPrepareParams>("textDocument/prepareCallHierarchy")?;
    let uri = params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;
    let Some((workspace, module, byte)) = state.locate(&uri, position) else {
        return Ok(serde_json::Value::Null);
    };
    // A module with syntax errors has no reliable declarations to prepare,
    // so preparation fails closed as the other parser-error refusals do.
    if !workspace.file(module).parse.diagnostics.is_empty() {
        return Ok(serde_json::Value::Null);
    }
    Ok(serde_json::to_value(hierarchy::prepare_call_hierarchy(
        &workspace, module, byte,
    ))?)
}

fn request_incoming_calls(
    state: &RequestContext,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) = request
        .extract::<lsp_types::CallHierarchyIncomingCallsParams>("callHierarchy/incomingCalls")?;
    let calls = state
        .views
        .workspace_containing(&state.documents, &params.item.uri)
        .map(|(workspace, module)| hierarchy::incoming_calls(&workspace, module, &params.item))
        .unwrap_or_default();
    Ok(serde_json::to_value(calls)?)
}

fn request_outgoing_calls(
    state: &RequestContext,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) = request
        .extract::<lsp_types::CallHierarchyOutgoingCallsParams>("callHierarchy/outgoingCalls")?;
    let calls = state
        .views
        .workspace_containing(&state.documents, &params.item.uri)
        .map(|(workspace, module)| hierarchy::outgoing_calls(&workspace, module, &params.item))
        .unwrap_or_default();
    Ok(serde_json::to_value(calls)?)
}

fn request_prepare_type_hierarchy(
    state: &RequestContext,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) =
        request.extract::<TypeHierarchyPrepareParams>("textDocument/prepareTypeHierarchy")?;
    let uri = params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;
    let Some((workspace, module, byte)) = state.locate(&uri, position) else {
        return Ok(serde_json::Value::Null);
    };
    Ok(serde_json::to_value(hierarchy::prepare_type_hierarchy(
        &workspace, module, byte,
    ))?)
}

fn request_type_hierarchy_supertypes(
    state: &RequestContext,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) =
        request.extract::<TypeHierarchySupertypesParams>("typeHierarchy/supertypes")?;
    let supertypes = state
        .views
        .workspace_containing(&state.documents, &params.item.uri)
        .map(|(workspace, module)| hierarchy::supertypes(&workspace, module, &params.item))
        .unwrap_or_default();
    Ok(serde_json::to_value(supertypes)?)
}

fn request_type_hierarchy_subtypes(
    state: &RequestContext,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) = request.extract::<TypeHierarchySubtypesParams>("typeHierarchy/subtypes")?;
    let subtypes = state
        .views
        .workspace_containing(&state.documents, &params.item.uri)
        .map(|(workspace, module)| hierarchy::subtypes(&workspace, module, &params.item))
        .unwrap_or_default();
    Ok(serde_json::to_value(subtypes)?)
}

fn request_moniker(
    state: &RequestContext,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) = request.extract::<MonikerParams>("textDocument/moniker")?;
    let uri = params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;
    let Some((workspace, module, byte)) = state.locate(&uri, position) else {
        return Ok(serde_json::Value::Null);
    };
    Ok(serde_json::to_value(hierarchy::moniker(
        &workspace, module, byte,
    ))?)
}

fn request_code_lenses(
    state: &RequestContext,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) = request.extract::<CodeLensParams>("textDocument/codeLens")?;
    let Some((workspace, module)) = state
        .views
        .workspace(&state.documents, &params.text_document.uri)
    else {
        return Ok(serde_json::Value::Null);
    };
    Ok(serde_json::to_value(hierarchy::code_lenses(
        &workspace, module,
    ))?)
}

fn request_semantic_tokens_full(
    state: &RequestContext,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) =
        request.extract::<SemanticTokensParams>("textDocument/semanticTokens/full")?;
    let uri = params.text_document.uri;
    let Some((workspace, module)) = state.views.workspace(&state.documents, &uri) else {
        return Ok(serde_json::Value::Null);
    };
    let mapper = PositionMapper::new(workspace.file(module).text());
    let data = semantic::semantic_tokens(&workspace, module, &mapper, None);
    let result_id = state.record_semantic_tokens(uri, data.clone());
    Ok(serde_json::to_value(SemanticTokens {
        result_id: Some(result_id),
        data,
    })?)
}

fn request_semantic_tokens_delta(
    state: &RequestContext,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) =
        request.extract::<SemanticTokensDeltaParams>("textDocument/semanticTokens/full/delta")?;
    let uri = params.text_document.uri;
    let Some((workspace, module)) = state.views.workspace(&state.documents, &uri) else {
        return Ok(serde_json::Value::Null);
    };
    let mapper = PositionMapper::new(workspace.file(module).text());
    let data = semantic::semantic_tokens(&workspace, module, &mapper, None);
    let edits = lock(&state.semantic_tokens)
        .by_uri
        .get(&uri)
        .filter(|snapshot| snapshot.result_id == params.previous_result_id)
        .map(|snapshot| semantic_token_delta_edits(&snapshot.data, &data));
    let result_id = state.record_semantic_tokens(uri, data.clone());
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
    state: &RequestContext,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) =
        request.extract::<SemanticTokensRangeParams>("textDocument/semanticTokens/range")?;
    let uri = params.text_document.uri;
    let Some((workspace, module)) = state.views.workspace(&state.documents, &uri) else {
        return Ok(serde_json::Value::Null);
    };
    let mapper = PositionMapper::new(workspace.file(module).text());
    let data = semantic::semantic_tokens(&workspace, module, &mapper, Some(&params.range));
    Ok(serde_json::to_value(SemanticTokens {
        result_id: None,
        data,
    })?)
}

fn request_inlay_hints(
    state: &RequestContext,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) = request.extract::<InlayHintParams>("textDocument/inlayHint")?;
    let Some((workspace, module)) = state
        .views
        .workspace(&state.documents, &params.text_document.uri)
    else {
        return Ok(serde_json::Value::Null);
    };
    Ok(serde_json::to_value(inlay::inlay_hints(
        &workspace,
        module,
        &params.range,
    ))?)
}

fn request_inline_values(
    state: &RequestContext,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) = request.extract::<InlineValueParams>("textDocument/inlineValue")?;
    let Some(open) = state.documents.get(&params.text_document.uri) else {
        return Ok(serde_json::Value::Null);
    };
    let file = open.analysis();
    if !file.parse.diagnostics.is_empty() {
        return Ok(serde_json::to_value(Vec::<InlineValue>::new())?);
    }

    let mapper = PositionMapper::new(file.text());
    let requested_start = mapper.byte_offset(params.range.start);
    let requested_end = mapper.byte_offset(params.range.end);
    let stopped_at = mapper.byte_offset(params.context.stopped_location.end);
    if requested_start > requested_end || stopped_at < requested_start || stopped_at > requested_end
    {
        return Ok(serde_json::to_value(Vec::<InlineValue>::new())?);
    }

    let names = &file.names;
    let mut values = names
        .visible_bindings(stopped_at)
        .into_iter()
        .filter_map(|binding| {
            let index = names.binding_at(&binding.selection)?;
            names
                .references(index)
                .map(|occurrence| &occurrence.span)
                .chain(std::iter::once(&binding.selection))
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
                            range: mapper.range(span),
                            variable_name: Some(binding.name.clone()),
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
    state: &RequestContext,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) =
        request.extract::<LinkedEditingRangeParams>("textDocument/linkedEditingRange")?;
    let uri = params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;
    let Some((workspace, module, byte)) = state.locate(&uri, position) else {
        return Ok(serde_json::Value::Null);
    };
    let file = workspace.file(module);
    if !file.parse.diagnostics.is_empty() {
        return Ok(serde_json::Value::Null);
    }
    let Some((span, target)) = workspace
        .resolve_at(module, byte)
        .and_then(|(span, resolved)| Some((span, resolved.target()?)))
    else {
        return Ok(serde_json::Value::Null);
    };
    let name = &file.text()[span.start..span.end];
    let mapper = PositionMapper::new(file.text());
    let ranges = workspace
        .search(&target)
        .found
        .into_iter()
        .filter(|(found_module, found)| {
            *found_module == module && file.text().get(found.start..found.end) == Some(name)
        })
        .map(|(_, found)| mapper.range(&found))
        .collect::<Vec<_>>();
    if ranges.len() < 2 {
        return Ok(serde_json::Value::Null);
    }
    Ok(serde_json::to_value(LinkedEditingRanges {
        ranges,
        word_pattern: None,
    })?)
}

fn request_completion(
    state: &RequestContext,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) = request.extract::<CompletionParams>("textDocument/completion")?;
    let uri = params.text_document_position.text_document.uri;
    let position = params.text_document_position.position;
    let Some((workspace, module, byte)) = state.locate(&uri, position) else {
        return Ok(serde_json::Value::Null);
    };
    let items = analysis::completion_at(&workspace, module, Some(byte), params.context.as_ref());
    let response = CompletionResponse::Array(items);
    Ok(serde_json::to_value(response)?)
}

fn request_formatting(
    state: &RequestContext,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) = request.extract::<DocumentFormattingParams>("textDocument/formatting")?;
    let edits = state
        .document(&params.text_document.uri)
        .map_or_else(Vec::new, |document| {
            crate::formatting::format(&document.text, None, &params.options)
        });
    Ok(serde_json::to_value(edits)?)
}

fn request_range_formatting(
    state: &RequestContext,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) =
        request.extract::<DocumentRangeFormattingParams>("textDocument/rangeFormatting")?;
    let edits = state
        .document(&params.text_document.uri)
        .map_or_else(Vec::new, |document| {
            crate::formatting::format(&document.text, Some(params.range), &params.options)
        });
    Ok(serde_json::to_value(edits)?)
}

fn request_document_diagnostic(
    state: &RequestContext,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) = request.extract::<DocumentDiagnosticParams>("textDocument/diagnostic")?;
    let uri = params.text_document.uri;
    let (version, diagnostics) = diagnostics::document_report(&state.snapshot(), &uri)
        .map_or((None, Vec::new()), |report| (report.version, report.diagnostics));
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
    "orna-diagnostics".hash(&mut hasher);
    uri.as_str().hash(&mut hasher);
    version.hash(&mut hasher);
    serde_json::to_vec(diagnostics)
        .expect("serialisable diagnostics")
        .hash(&mut hasher);
    format!("orna-{:016x}", hasher.finish())
}

fn request_document_links(
    state: &RequestContext,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) = request.extract::<DocumentLinkParams>("textDocument/documentLink")?;
    let links: Vec<DocumentLink> = state
        .views
        .workspace(&state.documents, &params.text_document.uri)
        .map(|(workspace, module)| analysis::document_links(&workspace, module))
        .unwrap_or_default();
    Ok(serde_json::to_value(links)?)
}

fn request_folding_ranges(
    state: &RequestContext,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) = request.extract::<FoldingRangeParams>("textDocument/foldingRange")?;
    let Some(open) = state.documents.get(&params.text_document.uri) else {
        return Ok(serde_json::Value::Null);
    };
    let file = open.analysis();
    let mapper = PositionMapper::new(file.text());
    let ranges: Vec<FoldingRange> =
        editor_ranges::folding_ranges(&file.parse.value, file.text(), &mapper);
    Ok(serde_json::to_value(ranges)?)
}

fn request_selection_ranges(
    state: &RequestContext,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) = request.extract::<SelectionRangeParams>("textDocument/selectionRange")?;
    let Some(open) = state.documents.get(&params.text_document.uri) else {
        return Ok(serde_json::Value::Null);
    };
    let file = open.analysis();
    let mapper = PositionMapper::new(file.text());
    let ranges = editor_ranges::selection_ranges(
        &file.parse.value,
        file.text(),
        &params.positions,
        &mapper,
    );
    Ok(serde_json::to_value(ranges)?)
}

#[cfg(test)]
mod analysis_cache_tests {
    use super::*;
    use crate::workspace::ANALYSES;
    use serde_json::json;

    const SOURCE: &str = "/// Adds two values.\n\
        pub fn add(left: Int, right: Int): Int = left + right;\n\
        pub fn twice(value: Int): Int = add(value, value);\n";

    fn notify(state: &mut ServerState, method: &str, params: serde_json::Value) {
        handle_notification(
            state,
            Notification {
                method: method.to_owned(),
                params,
            },
        );
    }

    fn analyses() -> usize {
        ANALYSES.with(std::cell::Cell::get)
    }

    /// Every request the server answers from analyses, at the call of `add`.
    fn requests(uri: &Uri, text: &str) -> Vec<(&'static str, serde_json::Value)> {
        let mapper = PositionMapper::new(text);
        let position = mapper.position(text.find("add(value").unwrap() + 1);
        let at = json!({"textDocument": {"uri": uri}, "position": position});
        let document = json!({"textDocument": {"uri": uri}});
        let whole = json!({
            "textDocument": {"uri": uri},
            "range": {"start": mapper.position(0), "end": mapper.position(text.len())},
        });
        vec![
            ("textDocument/hover", at.clone()),
            ("textDocument/definition", at.clone()),
            (
                "textDocument/references",
                json!({
                    "textDocument": {"uri": uri},
                    "position": position,
                    "context": {"includeDeclaration": true},
                }),
            ),
            ("textDocument/documentHighlight", at.clone()),
            ("textDocument/signatureHelp", at.clone()),
            ("textDocument/completion", at.clone()),
            ("textDocument/prepareRename", at.clone()),
            ("textDocument/linkedEditingRange", at.clone()),
            ("textDocument/prepareCallHierarchy", at.clone()),
            ("textDocument/moniker", at),
            ("textDocument/documentSymbol", document.clone()),
            ("textDocument/semanticTokens/full", document.clone()),
            ("textDocument/codeLens", document.clone()),
            ("textDocument/foldingRange", document.clone()),
            ("textDocument/documentLink", document),
            ("textDocument/semanticTokens/range", whole.clone()),
            ("textDocument/inlayHint", whole),
            ("workspace/symbol", json!({"query": "add"})),
        ]
    }

    fn answer_all(state: &ServerState, uri: &Uri, text: &str) {
        for (method, params) in requests(uri, text) {
            let response = dispatch(
                &state.context,
                Request::new(1.into(), method.to_owned(), params),
            );
            assert!(response.is_ok(), "{method}: {response:?}");
        }
    }

    #[test]
    fn requests_share_one_analysis_per_document_version() {
        let mut state = ServerState::new();
        let uri: Uri = "file:///workspace/cached.orna".parse().unwrap();
        notify(
            &mut state,
            "textDocument/didOpen",
            json!({"textDocument": {"uri": uri, "languageId": "orna", "version": 1, "text": SOURCE}}),
        );
        let before = analyses();
        answer_all(&state, &uri, SOURCE);
        assert_eq!(analyses() - before, 1, "one analysis serves every request");

        let edited = SOURCE.replace("left + right", "right + left");
        notify(
            &mut state,
            "textDocument/didChange",
            json!({"textDocument": {"uri": uri, "version": 2}, "contentChanges": [{"text": edited}]}),
        );
        answer_all(&state, &uri, &edited);
        assert_eq!(analyses() - before, 2, "an edit costs one more analysis");
    }
}

#[cfg(test)]
mod rename_request_tests {
    use super::*;
    use orna_syntax_v1::Keyword;

    const EXPRESSIONS_SOURCE: &str = include_str!("../tests/fixtures/expressions-v1.orna");
    const RENAME_TARGET_SOURCE: &str =
        include_str!("../tests/fixtures/rename-reserved-target-v1.orna");
    const IDENTITY_SOURCE: &str =
        include_str!("../tests/fixtures/identity-rename-follow-v1.orna");

    fn open(uri: &Uri, text: &str) -> ServerState {
        let mut state = ServerState::new();
        let home = state.context.projects.home(uri);
        state.set_document(Document::new(uri.clone(), text.to_owned(), 1), home);
        state
    }

    /// The edits renaming the name at `byte`, or the refusal message.
    fn rename(
        state: &mut ServerState,
        uri: &Uri,
        byte: usize,
        new_name: &str,
    ) -> Result<HashMap<Uri, Vec<TextEdit>>, String> {
        let text = state.context.document(uri).unwrap().text.clone();
        let request = Request::new(
            1.into(),
            "textDocument/rename".to_owned(),
            serde_json::json!({
                "textDocument": { "uri": uri },
                "position": PositionMapper::new(&text).position(byte),
                "newName": new_name,
            }),
        );
        match request_rename(&state.context, request) {
            Ok(response) => Ok(serde_json::from_value::<WorkspaceEdit>(response)
                .unwrap()
                .changes
                .unwrap_or_default()),
            Err(error) => {
                assert!(error.downcast_ref::<RequestFailed>().is_some(), "{error}");
                Err(error.to_string())
            }
        }
    }

    #[test]
    fn every_reserved_word_is_rejected_as_a_rename_target() {
        for keyword in Keyword::ALL {
            assert!(
                !rename::valid_identifier(keyword.spelling()),
                "reserved word `{}` must not be a rename target",
                keyword.spelling()
            );
        }
        assert!(rename::valid_identifier("total"));
    }

    #[test]
    fn rename_to_a_reserved_word_is_refused_and_a_plain_name_is_not() {
        let uri: Uri = "file:///workspace/expressions.orna".parse().unwrap();
        let mut state = open(&uri, EXPRESSIONS_SOURCE);
        let call_byte = EXPRESSIONS_SOURCE.find("add(value, 2)").unwrap() + 1;

        for keyword in Keyword::ALL {
            let refusal = rename(&mut state, &uri, call_byte, keyword.spelling())
                .expect_err("a reserved word is refused");
            assert!(refusal.starts_with("cannot rename: "), "{refusal}");
        }
        let edits = rename(&mut state, &uri, call_byte, "total")
            .expect("a plain identifier renames the function");
        assert_eq!(edits.values().map(Vec::len).sum::<usize>(), 2);
    }

    #[test]
    fn reserved_word_target_from_a_real_fixture_is_refused() {
        let uri: Uri = "file:///workspace/rename-reserved-target-v1.orna".parse().unwrap();
        let mut state = open(&uri, RENAME_TARGET_SOURCE);
        let declared_byte = RENAME_TARGET_SOURCE.find("total(value").unwrap() + 1;

        for keyword in Keyword::ALL {
            assert!(
                rename(&mut state, &uri, declared_byte, keyword.spelling()).is_err(),
                "rename of `total` to `{}` must be refused",
                keyword.spelling()
            );
        }
        let edits = rename(&mut state, &uri, declared_byte, "sum")
            .expect("a plain identifier renames the function");
        assert_eq!(edits.values().map(Vec::len).sum::<usize>(), 2);
    }

    #[test]
    fn rename_then_query_keeps_one_identity_for_the_renamed_function() {
        let uri: Uri = "file:///workspace/identity-rename-follow-v1.orna".parse().unwrap();
        let mut state = open(&uri, IDENTITY_SOURCE);
        let declared = IDENTITY_SOURCE.find("total(value").unwrap() + 1;
        let mut edits = rename(&mut state, &uri, declared, "sum")
            .expect("a plain identifier renames the function");
        let file_edits = edits.remove(&uri).unwrap();
        assert_eq!(file_edits.len(), 2, "declaration and call are both rewritten");
        let renamed = apply_text_edits(IDENTITY_SOURCE, &file_edits).unwrap();
        assert_eq!(renamed, IDENTITY_SOURCE.replace("total", "sum"));

        let parse = orna_syntax_v1::parse_module(&renamed);
        assert!(parse.diagnostics.is_empty());
        let symbols = analysis::declaration_symbols(&parse, &renamed);
        assert!(symbols.iter().any(|symbol| symbol.name == "sum"));
        assert!(!symbols.iter().any(|symbol| symbol.name == "total"));

        // Query the renamed function again: the same declaration is renamed
        // once more.
        handle_notification(
            &mut state,
            Notification {
                method: "textDocument/didChange".to_owned(),
                params: serde_json::json!({
                    "textDocument": { "uri": uri, "version": 2 },
                    "contentChanges": [{ "text": &renamed }],
                }),
            },
        );
        let declared = renamed.find("sum(value").unwrap() + 1;
        let again = rename(&mut state, &uri, declared, "count")
            .expect("the follow-up rename resolves the renamed declaration");
        assert_eq!(again.values().map(Vec::len).sum::<usize>(), 2);
    }
}

#[cfg(test)]
mod persisted_object_rename_tests {
    use super::*;
    use orna_semantic_v1::ModuleInput;
    use std::{fs, path::{Path, PathBuf}, process::Command};

    fn notify(state: &mut ServerState, notification: Notification) {
        handle_notification(state, notification);
    }

    /// Opens `source` as `didOpen` does, in its project when it has one.
    fn open_document(state: &mut ServerState, uri: &Uri, source: &str) {
        let home = state.context.projects.home(uri);
        state.set_document(Document::new(uri.clone(), source.to_owned(), 1), home);
    }

    fn did_save(state: &mut ServerState, uri: &Uri) {
        notify(
            state,
            Notification {
                method: "textDocument/didSave".to_owned(),
                params: serde_json::json!({ "textDocument": { "uri": uri } }),
            },
        );
    }

    fn rename_request(uri: Uri, new_name: &str) -> Request {
        Request::new(
            1.into(),
            "textDocument/rename".to_owned(),
            serde_json::json!({
                "textDocument": { "uri": uri },
                "position": { "line": 0, "character": 11 },
                "newName": new_name,
            }),
        )
    }
    fn apply_rename(state: &mut ServerState, uri: &Uri, new_name: &str, version: i32) -> String {
        let response = request_rename(&state.context, rename_request(uri.clone(), new_name)).unwrap();
        let workspace_edit: WorkspaceEdit = serde_json::from_value(response).unwrap();
        let source = state.context.document(uri).unwrap().text.clone();
        let edits = workspace_edit.changes.unwrap().remove(uri).unwrap();
        let renamed_source = apply_text_edits(&source, &edits).unwrap();
        notify(state, Notification {
            method: "textDocument/didChange".to_owned(),
            params: serde_json::json!({
                "textDocument": { "uri": uri, "version": version },
                "contentChanges": [{ "text": &renamed_source }],
            }),
        });
        renamed_source
    }

    fn git(directory: &Path, args: &[&str]) {
        assert!(Command::new("git").args(args).current_dir(directory).status().unwrap().success());
    }

    /// A format-3 repository whose index stages `main.orna` as `source`, with
    /// the identity inventory `orna commit` would stage for it.
    fn format3_fixture(source: &str) -> (tempfile::TempDir, Repository, PathBuf, ObjectIdentityManifest) {
        let directory = tempfile::tempdir().unwrap();
        let repository = orna_repository_v1::initialize_repository(directory.path())
            .unwrap()
            .into_repository();
        let source_path = directory.path().join("main.orna");
        fs::write(&source_path, source).unwrap();
        git(directory.path(), &["add", "main.orna", ".orna/database.orna"]);
        git(directory.path(), &[
            "-c", "user.name=orna-test", "-c", "user.email=orna-test@example.invalid",
            "-c", "commit.gpgsign=false", "commit", "-q", "-m", "base",
        ]);
        let module = ModuleInput::new("main.orna", source);
        let initial = ObjectIdentityManifest::reconcile(std::slice::from_ref(&module), None).unwrap();
        let staged = repository.capture_staged_candidate().unwrap();
        repository.stage_object_identities(&staged, &initial.to_stored(), &[]).unwrap();
        (directory, repository, source_path, initial)
    }

    /// The identity inventory the staged index carries now.
    fn staged_manifest(repository: &Repository) -> ObjectIdentityManifest {
        let staged = repository.capture_staged_candidate().unwrap();
        ObjectIdentityManifest::read_staged(repository, &staged).unwrap().unwrap()
    }

    fn file_uri(path: &Path) -> Uri {
        url::Url::from_file_path(path).unwrap().as_str().parse::<Uri>().unwrap()
    }

    /// Records every message the server sends to its client.
    fn capture_client(state: &mut ServerState) -> Arc<Mutex<Vec<Message>>> {
        let messages = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&messages);
        state.client = Some(Arc::new(move |message| lock(&sink).push(message)));
        state.context.client = state.client.clone();
        messages
    }

    /// The warnings shown to the client, as `window/showMessage`.
    fn shown_warnings(messages: &Mutex<Vec<Message>>) -> Vec<String> {
        lock(messages).iter().filter_map(|message| match message {
            Message::Notification(notification) if notification.method == "window/showMessage" => {
                let params: lsp_types::ShowMessageParams =
                    serde_json::from_value(notification.params.clone()).unwrap();
                (params.typ == lsp_types::MessageType::WARNING).then_some(params.message)
            }
            _ => None,
        }).collect()
    }

    /// Asserts exactly one warning was shown, carrying the rename code and remedy.
    fn assert_one_rename_warning(messages: &Mutex<Vec<Message>>, reason: &str) {
        let warnings = shown_warnings(messages);
        assert_eq!(warnings.len(), 1, "exactly one warning: {warnings:?}");
        assert!(warnings[0].starts_with("ORNA-LSP-RENAME-001"), "{warnings:?}");
        assert!(warnings[0].contains(reason), "{warnings:?} should mention {reason}");
        assert!(warnings[0].contains("orna mv"), "{warnings:?} should name the orna mv remedy");
    }

    #[test]
    fn sequential_unsaved_renames_preserve_original_object_identity() {
        let source = "pub table Media(id: Str) { payload: Blob, }";
        let (_directory, repository, source_path, initial) = format3_fixture(source);
        let original_id = *initial.id(ObjectKind::Table, "Media").unwrap();
        let uri = file_uri(&source_path);
        let mut state = ServerState::new();
        open_document(&mut state, &uri, source);

        apply_rename(&mut state, &uri, "Archive", 2);
        let vault_source = apply_rename(&mut state, &uri, "Vault", 3);
        fs::write(&source_path, &vault_source).unwrap();
        did_save(&mut state, &uri);

        let updated = staged_manifest(&repository);
        assert_eq!(updated.id(ObjectKind::Table, "Vault"), Some(&original_id));
    }

    #[test]
    fn declined_followup_rename_saves_latest_applied_identity() {
        let source = "pub table Media(id: Str) { payload: Blob, }";
        let (_directory, repository, source_path, initial) = format3_fixture(source);
        let original_id = *initial.id(ObjectKind::Table, "Media").unwrap();
        let uri = file_uri(&source_path);
        let mut state = ServerState::new();
        open_document(&mut state, &uri, source);

        let saved_source = apply_rename(&mut state, &uri, "Archive", 2);
        let declined_response = request_rename(&state.context, rename_request(uri.clone(), "Vault")).unwrap();
        assert!(!declined_response.is_null());
        drop(declined_response);
        fs::write(&source_path, &saved_source).unwrap();
        did_save(&mut state, &uri);

        let updated = staged_manifest(&repository);
        assert_eq!(updated.id(ObjectKind::Table, "Archive"), Some(&original_id));
    }

    #[test]
    fn fully_applied_rename_chain_with_reused_intermediate_name_keeps_id_at_final_target() {
        let source = "pub table Media(id: Str) { payload: Blob, }";
        let (_directory, repository, source_path, initial) = format3_fixture(source);
        let original_id = *initial.id(ObjectKind::Table, "Media").unwrap();
        let uri = file_uri(&source_path);
        let mut state = ServerState::new();
        open_document(&mut state, &uri, source);

        apply_rename(&mut state, &uri, "Archive", 2);
        let vault_source = apply_rename(&mut state, &uri, "Vault", 3);
        let saved_source = format!("{vault_source}\npub table Archive(id: Str) {{ payload: Blob, }}");
        notify(&mut state, Notification {
            method: "textDocument/didChange".to_owned(),
            params: serde_json::json!({
                "textDocument": { "uri": uri, "version": 4 },
                "contentChanges": [{ "text": &saved_source }],
            }),
        });
        fs::write(&source_path, &saved_source).unwrap();
        did_save(&mut state, &uri);

        let updated = staged_manifest(&repository);
        assert_eq!(updated.id(ObjectKind::Table, "Vault"), Some(&original_id));
        assert_ne!(updated.id(ObjectKind::Table, "Archive"), Some(&original_id));
    }

    #[test]
    fn unrelated_declaration_matching_declined_target_does_not_steal_identity() {
        let source = "pub table Media(id: Str) { payload: Blob, }";
        let (_directory, repository, source_path, initial) = format3_fixture(source);
        let original_id = *initial.id(ObjectKind::Table, "Media").unwrap();
        let uri = file_uri(&source_path);
        let mut state = ServerState::new();
        open_document(&mut state, &uri, source);

        let saved_source = apply_rename(&mut state, &uri, "Archive", 2);
        let declined_response = request_rename(&state.context, rename_request(uri.clone(), "Vault")).unwrap();
        assert!(!declined_response.is_null());
        drop(declined_response);
        let saved_source = format!("{saved_source}\npub table Vault(id: Str) {{ payload: Blob, }}");
        notify(&mut state, Notification {
            method: "textDocument/didChange".to_owned(),
            params: serde_json::json!({
                "textDocument": { "uri": uri, "version": 3 },
                "contentChanges": [{ "text": &saved_source }],
            }),
        });
        fs::write(&source_path, &saved_source).unwrap();
        did_save(&mut state, &uri);

        let updated = staged_manifest(&repository);
        assert_eq!(updated.id(ObjectKind::Table, "Archive"), Some(&original_id));
        assert_ne!(updated.id(ObjectKind::Table, "Vault"), Some(&original_id));
    }

    #[test]
    fn sequential_rename_chain_not_saved_does_not_persist_identity() {
        let source = "pub table Media(id: Str) { payload: Blob, }";
        let (_directory, repository, source_path, initial) = format3_fixture(source);
        let uri = file_uri(&source_path);
        let mut state = ServerState::new();
        open_document(&mut state, &uri, source);

        apply_rename(&mut state, &uri, "Archive", 2);
        apply_rename(&mut state, &uri, "Vault", 3);
        did_save(&mut state, &uri);

        assert_eq!(staged_manifest(&repository), initial);
    }

    #[test]
    fn rename_preserves_object_identity() {
        let source = "pub table Media(id: Str) { payload: Blob, }";
        let (_directory, repository, source_path, initial) = format3_fixture(source);
        let original_id = *initial.id(ObjectKind::Table, "Media").unwrap();
        let uri = file_uri(&source_path);
        let mut state = ServerState::new();
        open_document(&mut state, &uri, source);

        let response = request_rename(&state.context, rename_request(uri.clone(), "Archive")).unwrap();
        assert_eq!(staged_manifest(&repository), initial);

        let workspace_edit: WorkspaceEdit = serde_json::from_value(response).unwrap();
        let edits = workspace_edit.changes.unwrap().remove(&uri).unwrap();
        let renamed_source = apply_text_edits(source, &edits).unwrap();
        notify(&mut state, Notification {
            method: "textDocument/didChange".to_owned(),
            params: serde_json::json!({
                "textDocument": { "uri": uri, "version": 2 },
                "contentChanges": [{ "text": renamed_source }],
            }),
        });
        fs::write(&source_path, &renamed_source).unwrap();
        did_save(&mut state, &uri);

        let updated = staged_manifest(&repository);
        assert_eq!(updated.id(ObjectKind::Table, "Archive"), Some(&original_id));
    }

    #[test]
    fn declined_or_lost_rename_response_does_not_persist_identity() {
        let source = "pub table Media(id: Str) { payload: Blob, }";
        let (_directory, repository, source_path, initial) = format3_fixture(source);
        let uri = file_uri(&source_path);
        let mut state = ServerState::new();
        open_document(&mut state, &uri, source);

        let lost_response = request_rename(&state.context, rename_request(uri.clone(), "Archive")).unwrap();
        assert!(!lost_response.is_null());
        drop(lost_response);
        assert_eq!(staged_manifest(&repository), initial);
        did_save(&mut state, &uri);

        assert_eq!(staged_manifest(&repository), initial);
    }

    #[test]
    fn rename_request_returns_the_plain_edit_when_staged_identities_are_unreadable() {
        let source = "pub table Media(id: Str) { payload: Blob, }";
        let (directory, _repository, source_path, _initial) = format3_fixture(source);
        git(directory.path(), &["rm", "--cached", "-q", "-r", ".orna/store"]);
        let uri = file_uri(&source_path);
        let mut state = ServerState::new();
        let messages = capture_client(&mut state);
        open_document(&mut state, &uri, source);

        let response = request_rename(&state.context, rename_request(uri, "Archive"))
            .expect("an unreadable identity never fails an ordinary rename");
        let edit: WorkspaceEdit = serde_json::from_value(response).unwrap();
        assert_eq!(edit.changes.unwrap().values().map(Vec::len).sum::<usize>(), 1);
        assert_one_rename_warning(&messages, "object identity was not carried");
    }

    #[test]
    fn rename_during_a_merge_returns_the_plain_edit_and_keeps_identity() {
        let source = "pub table Media(id: Str) { payload: Blob, }";
        let (directory, repository, source_path, initial) = format3_fixture(source);
        let merge_head = directory.path().join(".git/MERGE_HEAD");
        fs::write(&merge_head, "0000000000000000000000000000000000000000\n").unwrap();
        let uri = file_uri(&source_path);
        let mut state = ServerState::new();
        let messages = capture_client(&mut state);
        open_document(&mut state, &uri, source);

        let response = request_rename(&state.context, rename_request(uri.clone(), "Archive"))
            .expect("a merge in progress never fails the plain rename edit");
        // The index cannot be read mid-merge, which is a typed error: the plain
        // edit still returns, and the client is told the identity was not carried.
        assert_one_rename_warning(&messages, "object identity was not carried");
        let edit: WorkspaceEdit = serde_json::from_value(response).unwrap();
        assert_eq!(edit.changes.unwrap().values().map(Vec::len).sum::<usize>(), 1);
        let saved_source = apply_rename(&mut state, &uri, "Archive", 2);
        fs::write(&source_path, &saved_source).unwrap();
        did_save(&mut state, &uri);
        // `apply_rename` issues a second rename request, which warns the same way.
        // The save adds no warning: no identity was recorded to carry.
        assert_eq!(shown_warnings(&messages).len(), 2, "one warning per rename request, none on save");
        fs::remove_file(merge_head).unwrap();
        assert_eq!(staged_manifest(&repository), initial);
    }

    #[test]
    fn failed_identity_persist_is_reported_to_client() {
        let source = "pub table Media(id: Str) { payload: Blob, }";
        let (directory, _repository, source_path, _initial) = format3_fixture(source);
        let uri = file_uri(&source_path);
        let mut state = ServerState::new();
        let messages = capture_client(&mut state);
        open_document(&mut state, &uri, source);

        let saved_source = apply_rename(&mut state, &uri, "Archive", 2);
        git(directory.path(), &["rm", "--cached", "-q", "-r", ".orna/store"]);
        fs::write(&source_path, &saved_source).unwrap();
        did_save(&mut state, &uri);

        assert_one_rename_warning(&messages, "object identity was not carried");
    }

    #[test]
    fn declined_rename_saves_without_warning() {
        let source = "pub table Media(id: Str) { payload: Blob, }";
        let (_directory, _repository, source_path, _initial) = format3_fixture(source);
        let uri = file_uri(&source_path);
        let mut state = ServerState::new();
        let messages = capture_client(&mut state);
        open_document(&mut state, &uri, source);

        // The client asks for the rename, then never applies it: the chain is
        // declined, so there is no identity to carry and no warning to show.
        drop(request_rename(&state.context, rename_request(uri.clone(), "Archive")).unwrap());
        did_save(&mut state, &uri);

        assert!(shown_warnings(&messages).is_empty(), "a declined rename is not a warning");
    }

    #[test]
    fn saved_text_that_differs_from_the_buffer_warns_with_the_remedy() {
        let source = "pub table Media(id: Str) { payload: Blob, }";
        let (_directory, repository, source_path, initial) = format3_fixture(source);
        let uri = file_uri(&source_path);
        let mut state = ServerState::new();
        let messages = capture_client(&mut state);
        open_document(&mut state, &uri, source);

        apply_rename(&mut state, &uri, "Archive", 2);
        fs::write(&source_path, "pub table Other(id: Str) { payload: Blob, }\n").unwrap();
        did_save(&mut state, &uri);

        assert_one_rename_warning(&messages, "differs from the editor buffer");
        assert_eq!(staged_manifest(&repository), initial);
    }

    #[test]
    fn save_after_the_document_closed_warns_with_the_remedy() {
        let source = "pub table Media(id: Str) { payload: Blob, }";
        let (_directory, repository, source_path, initial) = format3_fixture(source);
        let uri = file_uri(&source_path);
        let mut state = ServerState::new();
        let messages = capture_client(&mut state);
        open_document(&mut state, &uri, source);

        let saved_source = apply_rename(&mut state, &uri, "Archive", 2);
        fs::write(&source_path, &saved_source).unwrap();
        Arc::get_mut(&mut state.context.documents).expect("no other owner").remove(&uri);
        did_save(&mut state, &uri);

        assert_one_rename_warning(&messages, "the document is no longer open");
        assert_eq!(staged_manifest(&repository), initial);
    }

    #[test]
    fn identity_that_cannot_be_carried_warns_with_the_remedy() {
        let source = "pub table Media(id: Str) { payload: Blob, }";
        let (_directory, repository, source_path, initial) = format3_fixture(source);
        let uri = file_uri(&source_path);
        let mut state = ServerState::new();
        let messages = capture_client(&mut state);
        open_document(&mut state, &uri, source);

        let saved_source = apply_rename(&mut state, &uri, "Archive", 2);
        // Stage an inventory that already holds `Archive`, so carrying `Media`
        // to that name collides and `with_renames` refuses it.
        let both = "pub table Media(id: Str) { payload: Blob, }\npub table Archive(id: Str) { payload: Blob, }\n";
        let module = ModuleInput::new("main.orna", both);
        let widened = ObjectIdentityManifest::reconcile(std::slice::from_ref(&module), None).unwrap();
        let staged = repository.capture_staged_candidate().unwrap();
        repository.stage_object_identities(&staged, &widened.to_stored(), &[]).unwrap();
        fs::write(&source_path, &saved_source).unwrap();
        did_save(&mut state, &uri);

        assert_one_rename_warning(&messages, "object identity was not carried");
        assert_eq!(staged_manifest(&repository), widened);
        assert_ne!(initial, widened);
    }

    #[test]
    fn function_rename_never_warns() {
        let source = "pub fn total(value: Int): Int = value;\n";
        let (_directory, _repository, source_path, _initial) = format3_fixture(source);
        let uri = file_uri(&source_path);
        let mut state = ServerState::new();
        let messages = capture_client(&mut state);
        open_document(&mut state, &uri, source);

        // Column 8 is inside `total`. A function has no identity, so the rename
        // is plain and the client is shown nothing.
        let request = Request::new(
            1.into(),
            "textDocument/rename".to_owned(),
            serde_json::json!({
                "textDocument": { "uri": uri },
                "position": { "line": 0, "character": 8 },
                "newName": "sum",
            }),
        );
        let response = request_rename(&state.context, request).unwrap();
        let edit: WorkspaceEdit = serde_json::from_value(response).unwrap();
        assert!(edit.changes.unwrap().values().map(Vec::len).sum::<usize>() > 0);
        assert!(shown_warnings(&messages).is_empty(), "a function rename carries no identity");
    }
}
