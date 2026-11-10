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
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use crate::analysis;
use crate::diagnostics::{self, DiagnosticsWorker, Publisher, Snapshot};
use crate::documents::{Document, PositionMapper};
use crate::identity_rename::{PendingRenames, carries_identity, identity_not_carried};
use crate::project::{ProjectHome, ProjectLocator};
use crate::rename::{self, ModuleEdits, Refusal};
use crate::requests::RequestPool;
use crate::sync::lock;
use crate::views::{Documents, Views};
use crate::workspace::{OpenDocument, Workspace};
use crate::{editor_ranges, hierarchy, inlay, semantic};
use lsp_server::{
    Connection, ErrorCode, Message, Notification, Request, RequestId, Response, ResponseError,
};
use lsp_types::{
    CallHierarchyOptions, CallHierarchyServerCapability, CodeAction, CodeActionKind,
    CodeActionOptions, CodeActionOrCommand, CodeActionParams, CodeActionProviderCapability,
    CodeLensOptions, CodeLensParams, CompletionOptions, CompletionParams, CompletionResponse,
    Diagnostic, DiagnosticOptions, DiagnosticServerCapabilities, DocumentDiagnosticParams,
    DocumentDiagnosticReport, DocumentFormattingParams, DocumentHighlight, DocumentHighlightKind,
    DocumentHighlightParams, DocumentLinkOptions, DocumentLinkParams,
    DocumentRangeFormattingParams, DocumentSymbolParams, DocumentSymbolResponse, FoldingRange,
    FoldingRangeParams, FoldingRangeProviderCapability, FullDocumentDiagnosticReport,
    GotoDefinitionParams, GotoDefinitionResponse, HoverParams, HoverProviderCapability,
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
use orna_syntax::MissingSemicolon;

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

/// What requests read: the open documents when they arrived, and the caches
/// every request shares. Each request runs on a worker thread with its own
/// clone.
#[derive(Clone, Default)]
struct RequestContext {
    documents: Arc<Documents>,
    projects: Arc<ProjectLocator>,
    views: Arc<Views>,
    semantic_tokens: Arc<Mutex<SemanticTokenResults>>,
    pending_identity_renames: Arc<PendingRenames>,
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
        let result_id = format!("orna-check-{}", results.next);
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

/// The threads behind a connected server.
struct Workers {
    /// Publishes diagnostics.
    diagnostics: DiagnosticsWorker,
    /// Answers requests.
    requests: RequestPool<RequestContext>,
}

/// The server loop's state: the request context it keeps current, and the
/// workers that answer requests and publish diagnostics. Without workers,
/// which is how the tests run, the handlers are called directly.
struct ServerState {
    context: RequestContext,
    workers: Option<Workers>,
}

impl ServerState {
    fn new() -> Self {
        Self {
            context: RequestContext::default(),
            workers: None,
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
        if let Some(workers) = &self.workers {
            workers
                .diagnostics
                .schedule(self.context.snapshot(), vec![(uri.clone(), home)]);
        }
    }
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
    let diagnostics = DiagnosticsWorker::spawn(Publisher::new(move |message| {
        let _ = sender.send(message);
    }));
    let sender = connection.sender.clone();
    let requests = RequestPool::spawn(
        request_threads(),
        Arc::new(dispatch),
        Arc::new(move |message| {
            let _ = sender.send(message);
        }),
    );
    state.workers = Some(Workers {
        diagnostics,
        requests,
    });
    let sender = connection.sender.clone();
    state.context.client = Some(Arc::new(move |message| {
        let _ = sender.send(message);
    }));

    for message in &connection.receiver {
        match message {
            Message::Request(request) => {
                if connection.handle_shutdown(&request)? {
                    break;
                }
                if let Some(workers) = &state.workers {
                    workers.requests.submit(state.context.clone(), request);
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
            discard_pending_identity_renames(&state.context, &uri);
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
            state
                .context
                .pending_identity_renames
                .mark_applied(&uri, version, &text);
            state.set_document(Document::new(uri.clone(), text, version), open.home.clone());
            state.schedule_diagnostics(&uri, open.home.clone());
            false
        }
        "textDocument/didSave" => {
            if let Ok(params) =
                serde_json::from_value::<lsp_types::DidSaveTextDocumentParams>(notification.params)
            {
                let uri = params.text_document.uri;
                if let Err(error) = state
                    .context
                    .pending_identity_renames
                    .persist(&uri, state.context.document(&uri))
                {
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
                discard_pending_identity_renames(&state.context, &uri);
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
            if let Some(workers) = &state.workers
                && let Ok(params) =
                    serde_json::from_value::<lsp_types::CancelParams>(notification.params)
            {
                workers.requests.cancel(&match params.id {
                    NumberOrString::Number(id) => RequestId::from(id),
                    NumberOrString::String(id) => RequestId::from(id),
                });
            }
            false
        }
        _ => false,
    }
}

/// Drops the pending identity renames of a closed or re-opened document. A
/// rename applied to the buffer but never saved is reported, because its
/// identity is not carried; a declined rename is dropped silently.
fn discard_pending_identity_renames(state: &RequestContext, uri: &Uri) {
    if state.pending_identity_renames.discard(uri) {
        state.show_warning(&identity_not_carried("the rename was not saved"));
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

/// The result every request handler returns.
type HandlerResult = Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>>;

/// Answers a request addressed to one document position.
///
/// `extract` decodes the request's typed params and `serialise` encodes the
/// analysis result. Both stay at the call site because this crate depends
/// only on `serde_json`: a generic `serde` bound would need a dependency the
/// crate does not declare. `null` answers a position outside every analysable
/// document.
fn locate_request<P, T>(
    state: &RequestContext,
    request: Request,
    extract: impl FnOnce(Request) -> Result<P, lsp_server::ExtractError<Request>>,
    position: impl FnOnce(&P) -> (&Uri, Position),
    analyse: impl FnOnce(&P, &Arc<Workspace>, usize, usize) -> T,
    serialise: impl FnOnce(T) -> HandlerResult,
) -> HandlerResult {
    let params = extract(request)?;
    let (uri, at) = position(&params);
    let Some((workspace, module, byte)) = state.locate(uri, at) else {
        return Ok(serde_json::Value::Null);
    };
    serialise(analyse(&params, &workspace, module, byte))
}

/// Answers a request addressed to a whole document that belongs to a
/// workspace. A document outside every workspace answers `null`.
fn workspace_request<P, T>(
    state: &RequestContext,
    request: Request,
    extract: impl FnOnce(Request) -> Result<P, lsp_server::ExtractError<Request>>,
    uri: impl FnOnce(&P) -> &Uri,
    analyse: impl FnOnce(&P, &Arc<Workspace>, usize) -> T,
    serialise: impl FnOnce(T) -> HandlerResult,
) -> HandlerResult {
    let params = extract(request)?;
    let Some((workspace, module)) = state.views.workspace(&state.documents, uri(&params)) else {
        return Ok(serde_json::Value::Null);
    };
    serialise(analyse(&params, &workspace, module))
}

fn request_hover(
    state: &RequestContext,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    locate_request(
        state,
        request,
        |request| {
            request
                .extract::<HoverParams>("textDocument/hover")
                .map(|(_, params)| params)
        },
        |params| {
            (
                &params.text_document_position_params.text_document.uri,
                params.text_document_position_params.position,
            )
        },
        |_, workspace, module, byte| analysis::hover(workspace, module, byte),
        |value| Ok(serde_json::to_value(value)?),
    )
}

fn request_signature_help(
    state: &RequestContext,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    locate_request(
        state,
        request,
        |request| {
            request
                .extract::<lsp_types::SignatureHelpParams>("textDocument/signatureHelp")
                .map(|(_, params)| params)
        },
        |params| {
            (
                &params.text_document_position_params.text_document.uri,
                params.text_document_position_params.position,
            )
        },
        |_, workspace, module, byte| analysis::signature_help(workspace, module, byte),
        |value| Ok(serde_json::to_value(value)?),
    )
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
        if start > request_end || request_start > end {
            continue;
        }
        let Some(reason) = missing_semicolon(&diagnostic) else {
            continue;
        };
        if !params.context.diagnostics.is_empty()
            && !params.context.diagnostics.iter().any(|requested| {
                requested.range == diagnostic.range
                    && requested.code == diagnostic.code
                    && requested.message == diagnostic.message
            })
        {
            continue;
        }
        let Some(edit_position) =
            missing_semicolon_position(document, &file.tokens, parse, &diagnostic, reason, &mapper)
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
    only.is_none_or(|kinds| {
        kinds
            .iter()
            .any(|kind| kind.as_str() == CodeActionKind::QUICKFIX.as_str())
    })
}

/// The repairable missing terminator a diagnostic reports, classified by the
/// parser's own table rather than by matching its message text here.
fn missing_semicolon(diagnostic: &Diagnostic) -> Option<MissingSemicolon> {
    match diagnostic.code.as_ref() {
        Some(NumberOrString::String(code)) => {
            MissingSemicolon::classify(code, diagnostic.message.as_str())
        }
        _ => None,
    }
}

fn missing_semicolon_position(
    document: &Document,
    tokens: &[orna_syntax::Token],
    parse: &analysis::EditorParse,
    diagnostic: &Diagnostic,
    reason: MissingSemicolon,
    mapper: &PositionMapper<'_>,
) -> Option<Position> {
    let diagnostic_byte = mapper.byte_offset(diagnostic.range.start);
    let previous = tokens
        .iter()
        .filter(|token| {
            !matches!(&token.kind, orna_syntax::TokenKind::Eof) && token.span.end <= diagnostic_byte
        })
        .max_by_key(|token| token.span.end)?;
    if matches!(&previous.kind, orna_syntax::TokenKind::Punct(";")) {
        return None;
    }

    let mut repaired_source = document.text.clone();
    repaired_source.insert(previous.span.end, ';');
    let repaired_parse = orna_syntax::parse_module(&repaired_source);
    let diagnostic_count = |parse: &analysis::EditorParse| {
        parse
            .diagnostics
            .iter()
            .filter(|item| {
                MissingSemicolon::classify(item.code, item.message.as_str()) == Some(reason)
            })
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
    locate_request(
        state,
        request,
        |request| {
            request
                .extract::<GotoDefinitionParams>("textDocument/definition")
                .map(|(_, params)| params)
        },
        |params| {
            (
                &params.text_document_position_params.text_document.uri,
                params.text_document_position_params.position,
            )
        },
        |_, workspace, module, byte| {
            analysis::definition(workspace, module, byte).map(GotoDefinitionResponse::Scalar)
        },
        |value| Ok(serde_json::to_value(value)?),
    )
}

fn request_references(
    state: &RequestContext,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    locate_request(
        state,
        request,
        |request| {
            request
                .extract::<ReferenceParams>("textDocument/references")
                .map(|(_, params)| params)
        },
        |params| {
            (
                &params.text_document_position.text_document.uri,
                params.text_document_position.position,
            )
        },
        |params, workspace, module, byte| {
            analysis::references(workspace, module, byte, params.context.include_declaration)
        },
        |value| Ok(serde_json::to_value(value)?),
    )
}

fn request_document_highlights(
    state: &RequestContext,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    locate_request(
        state,
        request,
        |request| {
            request
                .extract::<DocumentHighlightParams>("textDocument/documentHighlight")
                .map(|(_, params)| params)
        },
        |params| {
            (
                &params.text_document_position_params.text_document.uri,
                params.text_document_position_params.position,
            )
        },
        |_, workspace, module, byte| {
            let Some(target) = workspace
                .resolve_at(module, byte)
                .and_then(|(_, resolved)| resolved.target())
            else {
                return Vec::new();
            };
            let mapper = PositionMapper::new(workspace.file(module).text());
            workspace
                .search(&target)
                .found
                .into_iter()
                .filter(|(found_module, _)| *found_module == module)
                .map(|(_, span)| {
                    let declaration = workspace
                        .binding_at(module, &span)
                        .is_some_and(|binding| {
                            let selection = &workspace.binding(module, binding).selection;
                            selection.start == span.start && selection.end == span.end
                        })
                        || workspace.declared_at(module, &span).is_some();
                    let write = workspace
                        .occurrence(module, &span)
                        .is_some_and(|occurrence| occurrence.written);
                    DocumentHighlight {
                        kind: Some(if declaration || write {
                            DocumentHighlightKind::WRITE
                        } else {
                            DocumentHighlightKind::READ
                        }),
                        range: mapper.range(&span),
                    }
                })
                .collect()
        },
        |value| Ok(serde_json::to_value(value)?),
    )
}

fn apply_text_edits(source: &str, edits: &[TextEdit]) -> Option<String> {
    let mapper = PositionMapper::new(source);
    let mut edits = edits
        .iter()
        .map(|edit| {
            Some((
                mapper.byte_offset(edit.range.start),
                mapper.byte_offset(edit.range.end),
                edit.new_text.as_str(),
            ))
        })
        .collect::<Option<Vec<_>>>()?;
    edits.sort_by_key(|(start, _, _)| std::cmp::Reverse(*start));
    let mut updated = source.to_owned();
    for (start, end, replacement) in edits {
        if start > end || end > updated.len() {
            return None;
        }
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
    let (renamable, edits) =
        rename::plan(&workspace, module, byte, &params.new_name).map_err(refused)?;
    if let Some(home) = state.documents.get(&uri).and_then(|open| open.home.clone()) {
        verify_project_check(state, &home, &workspace, &edits)?;
    }
    let changes = workspace_changes(&workspace, &edits);
    // Only declared tables and fields have identities. Local names, functions
    // and modules never consult the identity store, so they never warn. An
    // identity that cannot be carried never fails an ordinary rename: the plain
    // workspace edit is returned with a warning.
    let identity = if carries_identity(&workspace, &renamable.target) {
        state
            .pending_identity_renames
            .rename_for_request(state.document(&uri), &uri, position, &params.new_name)
            .unwrap_or_else(|error| {
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
                state
                    .pending_identity_renames
                    .stage(&uri, rename, document, expected_text);
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
            has_syntax_errors: !orna_syntax::parse_module(text).diagnostics.is_empty(),
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
    Ok(serde_json::to_value(
        PrepareRenameResponse::RangeWithPlaceholder {
            range: mapper.range(&renamable.span),
            placeholder: renamable.placeholder,
        },
    )?)
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
    locate_request(
        state,
        request,
        |request| {
            request
                .extract::<lsp_types::CallHierarchyPrepareParams>(
                    "textDocument/prepareCallHierarchy",
                )
                .map(|(_, params)| params)
        },
        |params| {
            (
                &params.text_document_position_params.text_document.uri,
                params.text_document_position_params.position,
            )
        },
        |_, workspace, module, byte| {
            // A module with syntax errors has no reliable declarations to
            // prepare, so preparation fails closed as the other parser-error
            // refusals do.
            if !workspace.file(module).parse.diagnostics.is_empty() {
                return None;
            }
            hierarchy::prepare_call_hierarchy(workspace, module, byte)
        },
        |value| Ok(serde_json::to_value(value)?),
    )
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
    locate_request(
        state,
        request,
        |request| {
            request
                .extract::<TypeHierarchyPrepareParams>("textDocument/prepareTypeHierarchy")
                .map(|(_, params)| params)
        },
        |params| {
            (
                &params.text_document_position_params.text_document.uri,
                params.text_document_position_params.position,
            )
        },
        |_, workspace, module, byte| hierarchy::prepare_type_hierarchy(workspace, module, byte),
        |value| Ok(serde_json::to_value(value)?),
    )
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
    locate_request(
        state,
        request,
        |request| {
            request
                .extract::<MonikerParams>("textDocument/moniker")
                .map(|(_, params)| params)
        },
        |params| {
            (
                &params.text_document_position_params.text_document.uri,
                params.text_document_position_params.position,
            )
        },
        |_, workspace, module, byte| hierarchy::moniker(workspace, module, byte),
        |value| Ok(serde_json::to_value(value)?),
    )
}

fn request_code_lenses(
    state: &RequestContext,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    workspace_request(
        state,
        request,
        |request| {
            request
                .extract::<CodeLensParams>("textDocument/codeLens")
                .map(|(_, params)| params)
        },
        |params| &params.text_document.uri,
        |_, workspace, module| hierarchy::code_lenses(workspace, module),
        |value| Ok(serde_json::to_value(value)?),
    )
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
    workspace_request(
        state,
        request,
        |request| {
            request
                .extract::<SemanticTokensRangeParams>("textDocument/semanticTokens/range")
                .map(|(_, params)| params)
        },
        |params| &params.text_document.uri,
        |params, workspace, module| {
            let mapper = PositionMapper::new(workspace.file(module).text());
            SemanticTokens {
                result_id: None,
                data: semantic::semantic_tokens(workspace, module, &mapper, Some(&params.range)),
            }
        },
        |value| Ok(serde_json::to_value(value)?),
    )
}

fn request_inlay_hints(
    state: &RequestContext,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    workspace_request(
        state,
        request,
        |request| {
            request
                .extract::<InlayHintParams>("textDocument/inlayHint")
                .map(|(_, params)| params)
        },
        |params| &params.text_document.uri,
        |params, workspace, module| inlay::inlay_hints(workspace, module, &params.range),
        |value| Ok(serde_json::to_value(value)?),
    )
}

fn request_inline_values(
    state: &RequestContext,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) = request.extract::<InlineValueParams>("textDocument/inlineValue")?;
    let Some((workspace, module)) =
        state.views.workspace(&state.documents, &params.text_document.uri)
    else {
        return Ok(serde_json::Value::Null);
    };
    let file = workspace.file(module);
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

    let mut values = workspace
        .visible_bindings(module, stopped_at)
        .into_iter()
        .filter_map(|binding| {
            let index = workspace.binding_at(module, &binding.selection)?;
            workspace
                .binding_references(module, index)
                .into_iter()
                .chain(std::iter::once(binding.selection.clone()))
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
    locate_request(
        state,
        request,
        |request| {
            request
                .extract::<LinkedEditingRangeParams>("textDocument/linkedEditingRange")
                .map(|(_, params)| params)
        },
        |params| {
            (
                &params.text_document_position_params.text_document.uri,
                params.text_document_position_params.position,
            )
        },
        |_, workspace, module, byte| {
            let file = workspace.file(module);
            if !file.parse.diagnostics.is_empty() {
                return None;
            }
            let (span, target) = workspace
                .resolve_at(module, byte)
                .and_then(|(span, resolved)| Some((span, resolved.target()?)))?;
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
            (ranges.len() >= 2).then_some(LinkedEditingRanges {
                ranges,
                word_pattern: None,
            })
        },
        |value| Ok(serde_json::to_value(value)?),
    )
}

fn request_completion(
    state: &RequestContext,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    locate_request(
        state,
        request,
        |request| {
            request
                .extract::<CompletionParams>("textDocument/completion")
                .map(|(_, params)| params)
        },
        |params| {
            (
                &params.text_document_position.text_document.uri,
                params.text_document_position.position,
            )
        },
        |params, workspace, module, byte| {
            CompletionResponse::Array(analysis::completion_at(
                workspace,
                module,
                Some(byte),
                params.context.as_ref(),
            ))
        },
        |value| Ok(serde_json::to_value(value)?),
    )
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
        .map_or((None, Vec::new()), |report| {
            (report.version, report.diagnostics)
        });
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
    workspace_request(
        state,
        request,
        |request| {
            request
                .extract::<DocumentLinkParams>("textDocument/documentLink")
                .map(|(_, params)| params)
        },
        |params| &params.text_document.uri,
        |_, workspace, module| analysis::document_links(workspace, module),
        |value| Ok(serde_json::to_value(value)?),
    )
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
    let ranges =
        editor_ranges::selection_ranges(&file.parse.value, file.text(), &params.positions, &mapper);
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
    use orna_syntax::Keyword;

    const EXPRESSIONS_SOURCE: &str = include_str!("../tests/fixtures/expressions-v1.orna");
    const RENAME_TARGET_SOURCE: &str =
        include_str!("../tests/fixtures/rename-reserved-target-v1.orna");
    const IDENTITY_SOURCE: &str = include_str!("../tests/fixtures/identity-rename-follow-v1.orna");

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
        let uri: Uri = "file:///workspace/rename-reserved-target-v1.orna"
            .parse()
            .unwrap();
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
        let uri: Uri = "file:///workspace/identity-rename-follow-v1.orna"
            .parse()
            .unwrap();
        let mut state = open(&uri, IDENTITY_SOURCE);
        let declared = IDENTITY_SOURCE.find("total(value").unwrap() + 1;
        let mut edits = rename(&mut state, &uri, declared, "sum")
            .expect("a plain identifier renames the function");
        let file_edits = edits.remove(&uri).unwrap();
        assert_eq!(
            file_edits.len(),
            2,
            "declaration and call are both rewritten"
        );
        let renamed = apply_text_edits(IDENTITY_SOURCE, &file_edits).unwrap();
        assert_eq!(renamed, IDENTITY_SOURCE.replace("total", "sum"));

        let parse = orna_syntax::parse_module(&renamed);
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
    use orna_check::ModuleInput;
    use orna_project::{ObjectIdentityManifest, ObjectKind};
    use orna_repository::Repository;
    use std::{
        fs,
        path::{Path, PathBuf},
        process::Command,
    };

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
        let response =
            request_rename(&state.context, rename_request(uri.clone(), new_name)).unwrap();
        let workspace_edit: WorkspaceEdit = serde_json::from_value(response).unwrap();
        let source = state.context.document(uri).unwrap().text.clone();
        let edits = workspace_edit.changes.unwrap().remove(uri).unwrap();
        let renamed_source = apply_text_edits(&source, &edits).unwrap();
        notify(
            state,
            Notification {
                method: "textDocument/didChange".to_owned(),
                params: serde_json::json!({
                    "textDocument": { "uri": uri, "version": version },
                    "contentChanges": [{ "text": &renamed_source }],
                }),
            },
        );
        renamed_source
    }

    fn git(directory: &Path, args: &[&str]) {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(directory)
                .status()
                .unwrap()
                .success()
        );
    }

    /// A format-3 repository whose `main.orna` is committed as `source`, with no
    /// identity inventory staged.
    fn format3_committed(source: &str) -> (tempfile::TempDir, Repository, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let repository = orna_repository::initialize_repository(directory.path())
            .unwrap()
            .into_repository();
        let source_path = directory.path().join("main.orna");
        fs::write(&source_path, source).unwrap();
        git(
            directory.path(),
            &["add", "main.orna", ".orna/database.orna"],
        );
        git(
            directory.path(),
            &[
                "-c",
                "user.name=orna-test",
                "-c",
                "user.email=orna-test@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-m",
                "base",
            ],
        );
        (directory, repository, source_path)
    }

    /// A format-3 repository whose index stages `main.orna` as `source`, with
    /// the identity inventory `orna commit` would stage for it.
    fn format3_fixture(
        source: &str,
    ) -> (
        tempfile::TempDir,
        Repository,
        PathBuf,
        ObjectIdentityManifest,
    ) {
        let (directory, repository, source_path) = format3_committed(source);
        let module = ModuleInput::new("main.orna", source);
        let initial =
            ObjectIdentityManifest::reconcile(std::slice::from_ref(&module), None).unwrap();
        let staged = repository.capture_staged_candidate().unwrap();
        repository
            .stage_object_identities(&staged, &initial.to_stored(), &[])
            .unwrap();
        (directory, repository, source_path, initial)
    }

    /// The identity inventory the staged index carries now.
    fn staged_manifest(repository: &Repository) -> ObjectIdentityManifest {
        let staged = repository.capture_staged_candidate().unwrap();
        ObjectIdentityManifest::read_staged(repository, &staged)
            .unwrap()
            .unwrap()
    }

    fn file_uri(path: &Path) -> Uri {
        url::Url::from_file_path(path)
            .unwrap()
            .as_str()
            .parse::<Uri>()
            .unwrap()
    }

    /// Records every message the server sends to its client.
    fn capture_client(state: &mut ServerState) -> Arc<Mutex<Vec<Message>>> {
        let messages = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&messages);
        state.context.client = Some(Arc::new(move |message| lock(&sink).push(message)));
        messages
    }

    /// The warnings shown to the client, as `window/showMessage`.
    fn shown_warnings(messages: &Mutex<Vec<Message>>) -> Vec<String> {
        lock(messages)
            .iter()
            .filter_map(|message| match message {
                Message::Notification(notification)
                    if notification.method == "window/showMessage" =>
                {
                    let params: lsp_types::ShowMessageParams =
                        serde_json::from_value(notification.params.clone()).unwrap();
                    (params.typ == lsp_types::MessageType::WARNING).then_some(params.message)
                }
                _ => None,
            })
            .collect()
    }

    /// Asserts exactly one warning was shown, carrying the rename code and remedy.
    fn assert_one_rename_warning(messages: &Mutex<Vec<Message>>, reason: &str) {
        let warnings = shown_warnings(messages);
        assert_eq!(warnings.len(), 1, "exactly one warning: {warnings:?}");
        assert!(
            warnings[0].starts_with("ORNA-LSP-RENAME-001"),
            "{warnings:?}"
        );
        assert!(
            warnings[0].contains(reason),
            "{warnings:?} should mention {reason}"
        );
        assert!(
            warnings[0].contains("orna mv"),
            "{warnings:?} should name the orna mv remedy"
        );
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
        let declined_response =
            request_rename(&state.context, rename_request(uri.clone(), "Vault")).unwrap();
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
        let saved_source =
            format!("{vault_source}\npub table Archive(id: Str) {{ payload: Blob, }}");
        notify(
            &mut state,
            Notification {
                method: "textDocument/didChange".to_owned(),
                params: serde_json::json!({
                    "textDocument": { "uri": uri, "version": 4 },
                    "contentChanges": [{ "text": &saved_source }],
                }),
            },
        );
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
        let declined_response =
            request_rename(&state.context, rename_request(uri.clone(), "Vault")).unwrap();
        assert!(!declined_response.is_null());
        drop(declined_response);
        let saved_source = format!("{saved_source}\npub table Vault(id: Str) {{ payload: Blob, }}");
        notify(
            &mut state,
            Notification {
                method: "textDocument/didChange".to_owned(),
                params: serde_json::json!({
                    "textDocument": { "uri": uri, "version": 3 },
                    "contentChanges": [{ "text": &saved_source }],
                }),
            },
        );
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

        let response =
            request_rename(&state.context, rename_request(uri.clone(), "Archive")).unwrap();
        assert_eq!(staged_manifest(&repository), initial);

        let workspace_edit: WorkspaceEdit = serde_json::from_value(response).unwrap();
        let edits = workspace_edit.changes.unwrap().remove(&uri).unwrap();
        let renamed_source = apply_text_edits(source, &edits).unwrap();
        notify(
            &mut state,
            Notification {
                method: "textDocument/didChange".to_owned(),
                params: serde_json::json!({
                    "textDocument": { "uri": uri, "version": 2 },
                    "contentChanges": [{ "text": renamed_source }],
                }),
            },
        );
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

        let lost_response =
            request_rename(&state.context, rename_request(uri.clone(), "Archive")).unwrap();
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
        git(
            directory.path(),
            &["rm", "--cached", "-q", "-r", ".orna/store"],
        );
        let uri = file_uri(&source_path);
        let mut state = ServerState::new();
        let messages = capture_client(&mut state);
        open_document(&mut state, &uri, source);

        let response = request_rename(&state.context, rename_request(uri, "Archive"))
            .expect("an unreadable identity never fails an ordinary rename");
        let edit: WorkspaceEdit = serde_json::from_value(response).unwrap();
        assert_eq!(
            edit.changes.unwrap().values().map(Vec::len).sum::<usize>(),
            1
        );
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
        assert_one_rename_warning(&messages, "RepositoryBusy");
        let edit: WorkspaceEdit = serde_json::from_value(response).unwrap();
        assert_eq!(
            edit.changes.unwrap().values().map(Vec::len).sum::<usize>(),
            1
        );
        let saved_source = apply_rename(&mut state, &uri, "Archive", 2);
        fs::write(&source_path, &saved_source).unwrap();
        did_save(&mut state, &uri);
        // `apply_rename` issues a second rename request, which warns the same way.
        // The save adds no warning: no identity was recorded to carry.
        assert_eq!(
            shown_warnings(&messages).len(),
            2,
            "one warning per rename request, none on save"
        );
        fs::remove_file(merge_head).unwrap();
        assert_eq!(staged_manifest(&repository), initial);
    }

    /// A rename request at `line`/`character`, for any declaration in the buffer.
    fn rename_request_at(uri: &Uri, line: u32, character: u32, new_name: &str) -> Request {
        Request::new(
            1.into(),
            "textDocument/rename".to_owned(),
            serde_json::json!({
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character },
                "newName": new_name,
            }),
        )
    }

    /// The number of text edits in a rename response.
    fn edit_count(response: serde_json::Value) -> usize {
        let edit: WorkspaceEdit = serde_json::from_value(response).unwrap();
        edit.changes.unwrap().values().map(Vec::len).sum()
    }

    #[test]
    fn rename_of_an_uncommitted_table_is_plain_and_silent() {
        // Only `Media` has a committed identity; `Draft` exists in the buffer alone.
        let committed = "pub table Media(id: Str) { payload: Blob, }\n";
        let (_directory, _repository, source_path, _initial) = format3_fixture(committed);
        let uri = file_uri(&source_path);
        let mut state = ServerState::new();
        let messages = capture_client(&mut state);
        open_document(
            &mut state,
            &uri,
            &format!("{committed}pub table Draft(id: Str) {{ note: Str, }}\n"),
        );

        // No committed identity for `Draft`: the lookup is `Ok(None)`, so the
        // plain edit is returned and nothing is shown.
        let response =
            request_rename(&state.context, rename_request_at(&uri, 1, 11, "Final")).unwrap();
        assert!(edit_count(response) > 0);
        assert!(
            shown_warnings(&messages).is_empty(),
            "an uncommitted table has no identity to carry"
        );
    }

    #[test]
    fn ambiguous_field_rename_warns_when_a_committed_identity_exists() {
        let source =
            "pub table Media(id: Str) { title: Str, }\npub table Music(id: Str) { title: Str, }\n";
        let (_directory, _repository, source_path, _initial) = format3_fixture(source);
        let uri = file_uri(&source_path);
        let mut state = ServerState::new();
        let messages = capture_client(&mut state);
        open_document(&mut state, &uri, source);

        // Both tables hold a committed `title`, so the field cannot be matched to one identity.
        let response =
            request_rename(&state.context, rename_request_at(&uri, 0, 28, "heading")).unwrap();
        assert!(edit_count(response) > 0, "the plain edit still returns");
        assert_one_rename_warning(&messages, "matches several fields");
    }

    #[test]
    fn legacy_format_rename_warns_that_identity_is_not_carried() {
        let directory = tempfile::tempdir().unwrap();
        orna_repository::write_legacy_format1_fixture(directory.path());
        let source_path = directory.path().join("main.orna");
        let source = "pub table Media(id: Str) { payload: Blob, }\n";
        let uri = file_uri(&source_path);
        let mut state = ServerState::new();
        let messages = capture_client(&mut state);
        open_document(&mut state, &uri, source);

        // Format 1 and 2 repositories have no object identities to carry.
        let response =
            request_rename(&state.context, rename_request(uri.clone(), "Archive")).unwrap();
        assert_eq!(edit_count(response), 1);
        assert_one_rename_warning(&messages, "object identity was not carried");
    }

    #[test]
    fn rename_without_a_staged_identity_inventory_warns_and_keeps_the_text() {
        let source = "pub table Media(id: Str) { payload: Blob, }\n";
        let (_directory, _repository, source_path) = format3_committed(source);
        let uri = file_uri(&source_path);
        let mut state = ServerState::new();
        let messages = capture_client(&mut state);
        open_document(&mut state, &uri, source);

        let response =
            request_rename(&state.context, rename_request(uri.clone(), "Archive")).unwrap();
        assert_eq!(edit_count(response), 1);
        assert_one_rename_warning(&messages, "the staged object identities are missing");
    }

    #[test]
    fn local_rename_never_warns() {
        let source = "pub fn total(value: Int): Int = value;\n";
        let (_directory, _repository, source_path, _initial) = format3_fixture(source);
        let uri = file_uri(&source_path);
        let mut state = ServerState::new();
        let messages = capture_client(&mut state);
        open_document(&mut state, &uri, source);

        // Column 15 is inside the parameter `value`, a local name with no identity.
        let response =
            request_rename(&state.context, rename_request_at(&uri, 0, 15, "amount")).unwrap();
        assert!(edit_count(response) > 0);
        assert!(
            shown_warnings(&messages).is_empty(),
            "a local rename carries no identity"
        );
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
        git(
            directory.path(),
            &["rm", "--cached", "-q", "-r", ".orna/store"],
        );
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

        assert!(
            shown_warnings(&messages).is_empty(),
            "a declined rename is not a warning"
        );
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
        fs::write(
            &source_path,
            "pub table Other(id: Str) { payload: Blob, }\n",
        )
        .unwrap();
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
        Arc::get_mut(&mut state.context.documents)
            .expect("no other owner")
            .remove(&uri);
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
        let widened =
            ObjectIdentityManifest::reconcile(std::slice::from_ref(&module), None).unwrap();
        let staged = repository.capture_staged_candidate().unwrap();
        repository
            .stage_object_identities(&staged, &widened.to_stored(), &[])
            .unwrap();
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
        assert!(
            shown_warnings(&messages).is_empty(),
            "a function rename carries no identity"
        );
    }
}
