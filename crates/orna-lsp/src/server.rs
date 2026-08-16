//! The LSP server loop and request dispatch.
//!
//! The server is synchronous and single-threaded. Compiler checks for one
//! document are fast, so no worker pool is needed for the first version.

use std::collections::HashMap;

use lsp_server::{Connection, ErrorCode, Message, Notification, Request, Response, ResponseError};
use lsp_types::{
    CompletionOptions, CompletionParams, CompletionResponse, DiagnosticOptions,
    DiagnosticServerCapabilities, DocumentDiagnosticParams, DocumentDiagnosticReport,
    DocumentSymbolParams, DocumentSymbolResponse, FullDocumentDiagnosticReport,
    GotoDefinitionParams, GotoDefinitionResponse, Hover, HoverParams, HoverProviderCapability,
    OneOf, PositionEncodingKind, PublishDiagnosticsParams, ReferenceParams,
    RelatedFullDocumentDiagnosticReport, SemanticTokens, SemanticTokensFullOptions,
    SemanticTokensLegend, SemanticTokensOptions, SemanticTokensParams, SemanticTokensRangeParams,
    SemanticTokensServerCapabilities, ServerCapabilities, TextDocumentSyncCapability,
    TextDocumentSyncKind, TextDocumentSyncOptions, TextDocumentSyncSaveOptions, Uri,
};
use orna_syntax::Parse;

use crate::analysis::{self, StandardLibrary};
use crate::documents::{Document, PositionMapper};
use crate::semantic;

/// Shared server state across requests and notifications.
struct ServerState {
    documents: HashMap<Uri, Document>,
    standard: Option<StandardLibrary>,
}

impl ServerState {
    fn new() -> Self {
        let standard = match StandardLibrary::load() {
            Ok(standard) => Some(standard),
            Err(error) => {
                eprintln!("orna-lsp: standard library unavailable: {error}");
                None
            }
        };
        Self {
            documents: HashMap::new(),
            standard,
        }
    }

    fn document(&self, uri: &Uri) -> Option<&Document> {
        self.documents.get(uri)
    }
}

/// Runs the server until the client exits.
pub fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (connection, io_threads) = Connection::stdio();
    let capabilities = server_capabilities();
    let _initialize = connection.initialize(serde_json::to_value(capabilities)?)?;
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

/// Returns the capabilities advertised during initialization.
fn server_capabilities() -> ServerCapabilities {
    ServerCapabilities {
        position_encoding: Some(PositionEncodingKind::UTF16),
        text_document_sync: Some(TextDocumentSyncCapability::Options(
            TextDocumentSyncOptions {
                open_close: Some(true),
                change: Some(TextDocumentSyncKind::FULL),
                will_save: None,
                will_save_wait_until: None,
                save: Some(TextDocumentSyncSaveOptions::Supported(true)),
            },
        )),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        definition_provider: Some(OneOf::Left(true)),
        references_provider: Some(OneOf::Left(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec![".".to_owned(), ":".to_owned()]),
            ..CompletionOptions::default()
        }),
        semantic_tokens_provider: Some(SemanticTokensServerCapabilities::SemanticTokensOptions(
            SemanticTokensOptions {
                legend: SemanticTokensLegend {
                    token_types: semantic::LEGEND.to_vec(),
                    token_modifiers: Vec::new(),
                },
                range: Some(true),
                full: Some(SemanticTokensFullOptions::Bool(true)),
                work_done_progress_options: Default::default(),
            },
        )),
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
        "textDocument/definition" => request_definition(state, request),
        "textDocument/references" => request_references(state, request),
        "textDocument/documentSymbol" => request_document_symbols(state, request),
        "textDocument/semanticTokens/full" => request_semantic_tokens_full(state, request),
        "textDocument/semanticTokens/range" => request_semantic_tokens_range(state, request),
        "textDocument/completion" => request_completion(state, request),
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
            if let Some(change) = params.content_changes.last() {
                let uri = params.text_document.uri.clone();
                if let Some(document) = state.documents.get_mut(&uri) {
                    document.text = change.text.clone();
                    document.version = params.text_document.version;
                }
                publish_diagnostics(state, connection, &uri);
            }
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

/// Recomputes and publishes the diagnostics for one document.
fn publish_diagnostics(state: &ServerState, connection: &Connection, uri: &Uri) {
    let Some(document) = state.document(uri) else {
        return;
    };
    let mapper = PositionMapper::new(&document.text);
    let diagnostics = analysis::check_document(document, state.standard.as_ref(), &mapper);
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
    let parse = orna_syntax::parse(&document.text);
    (parse, mapper)
}

fn request_hover(
    state: &mut ServerState,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) = request.extract::<HoverParams>("textDocument/hover")?;
    let uri = params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;
    let Some(document) = state.document(&uri) else {
        return Ok(serde_json::Value::Null);
    };
    let (parse, mapper) = parse_document(document);
    let hover: Option<Hover> =
        analysis::hover(document, &parse, state.standard.as_ref(), position, &mapper);
    Ok(serde_json::to_value(hover)?)
}

fn request_definition(
    state: &mut ServerState,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) = request.extract::<GotoDefinitionParams>("textDocument/definition")?;
    let uri = params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;
    let Some(document) = state.document(&uri) else {
        return Ok(serde_json::Value::Null);
    };
    let (parse, mapper) = parse_document(document);
    let location = analysis::definition(document, &parse, position, &mapper);
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
    let Some(document) = state.document(&uri) else {
        return Ok(serde_json::Value::Null);
    };
    let (parse, mapper) = parse_document(document);
    let locations = analysis::references(document, &parse, position, &mapper);
    Ok(serde_json::to_value(locations)?)
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
    let symbols = analysis::document_symbols(&parse, &mapper);
    let response = DocumentSymbolResponse::Nested(symbols);
    Ok(serde_json::to_value(response)?)
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
    let (parse, mapper) = parse_document(document);
    let data = semantic::semantic_tokens(&parse, &mapper, None);
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
    let (parse, mapper) = parse_document(document);
    let data = semantic::semantic_tokens(&parse, &mapper, Some(&params.range));
    Ok(serde_json::to_value(SemanticTokens {
        result_id: None,
        data,
    })?)
}

fn request_completion(
    state: &mut ServerState,
    request: Request,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (_, params) = request.extract::<CompletionParams>("textDocument/completion")?;
    let uri = params.text_document_position.text_document.uri;
    let Some(document) = state.document(&uri) else {
        return Ok(serde_json::Value::Null);
    };
    let (parse, _) = parse_document(document);
    let items = analysis::completion(&parse, state.standard.as_ref());
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
    let diagnostics = analysis::check_document(document, state.standard.as_ref(), &mapper);
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
