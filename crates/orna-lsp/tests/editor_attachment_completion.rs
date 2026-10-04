//! Real editor-client attachment proofs for syntax-v1 completion.

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

use serde_json::Value;

#[path = "support/completion_contract.rs"]
mod completion_contract;
#[allow(dead_code)]
#[path = "support/hover_semantic_contract.rs"]
mod hover_semantic_contract;
#[path = "support/syntax_v1_action_signature_contract.rs"]
mod syntax_v1_action_signature_contract;
#[path = "support/syntax_v1_depth_contract.rs"]
mod syntax_v1_depth_contract;
#[path = "support/syntax_v1_folding_selection_contract.rs"]
mod syntax_v1_folding_selection_contract;
#[path = "support/syntax_v1_workspace_hierarchy_contract.rs"]
mod syntax_v1_workspace_hierarchy_contract;

const SOURCE: &str = include_str!("fixtures/ji3t0-lsp-v1.orna");
const PROVIDER_SOURCE: &str = include_str!("fixtures/expressions-v1.orna");
const SEMANTIC_SOURCE: &str = include_str!("fixtures/editor-semantic-tokens.orna");
const HINTS_SOURCE: &str = include_str!("fixtures/editor-lsp-hints.orna");
const SIGNATURE_SOURCE: &str = include_str!("fixtures/signature-actions-v1.orna");
const ACTION_SOURCE: &str = include_str!("fixtures/missing-semicolon-code-action-v1.orna");
const WORKSPACE_HIERARCHY_PROVIDER_SOURCE: &str =
    include_str!("fixtures/workspace-hierarchy-provider-v1.orna");
const WORKSPACE_HIERARCHY_CALLER_SOURCE: &str =
    include_str!("fixtures/workspace-hierarchy-caller-v1.orna");
const FOLDING_SELECTION_SOURCE: &str = include_str!("fixtures/folding-selection-v1.orna");

#[test]
fn attachment_input_matches_its_crate_local_fixture() {
    let fixture = repo_root().join("crates/orna-lsp/tests/fixtures/ji3t0-lsp-v1.orna");
    assert_eq!(
        fs::read_to_string(&fixture).expect("read editor attachment fixture"),
        SOURCE
    );
    assert!(SOURCE.contains("pub fn caller(value: Int): Int = add(value, 2);"));
    assert!(!SOURCE.contains("pub fn add("));
    assert!(PROVIDER_SOURCE.contains("/// Add two integer values."));
    assert!(PROVIDER_SOURCE.contains("pub fn add(left: Int, right: Int): Int"));
}

#[test]
fn vim_lsp_attaches_and_exposes_hover_semantic_tokens_and_completion() {
    assert_v1_attachment_fixture();
    let Some(vim) = executable("ORNA_TEST_VIM", "vim") else {
        eprintln!("SKIP: Vim is not installed; set ORNA_TEST_VIM to its executable");
        return;
    };
    let Some(vim_lsp) = vim_lsp_runtime() else {
        eprintln!(
            "SKIP: vim-lsp is unavailable; set ORNA_TEST_VIM_LSP_RUNTIME to its runtime directory"
        );
        return;
    };
    let version = Command::new(&vim)
        .arg("--version")
        .output()
        .unwrap_or_else(|error| panic!("probe Vim at {}: {error}", vim.display()));
    let version = String::from_utf8_lossy(&version.stdout);
    if !version.contains("+channel") || !version.contains("+job") {
        eprintln!("SKIP: Vim lacks +channel/+job support required by vim-lsp");
        return;
    }

    let root = repo_root();
    let fixture = root.join("crates/orna-lsp/tests/fixtures/ji3t0-lsp-v1.orna");
    assert_eq!(fs::read_to_string(&fixture).unwrap(), SOURCE);
    let provider = root.join("crates/orna-lsp/tests/fixtures/expressions-v1.orna");
    assert_eq!(fs::read_to_string(&provider).unwrap(), PROVIDER_SOURCE);
    assert!(
        provider < fixture,
        "provider URI must sort before consumer URI"
    );
    let script = temporary_path("vim");
    let result_path = temporary_path("json");
    let semantic_fixture = root.join("crates/orna-lsp/tests/fixtures/editor-semantic-tokens.orna");
    assert_eq!(
        fs::read_to_string(&semantic_fixture).unwrap(),
        SEMANTIC_SOURCE
    );
    let depth_fixture = root.join("crates/orna-lsp/tests/fixtures/editor-lsp-hints.orna");
    assert_eq!(fs::read_to_string(&depth_fixture).unwrap(), HINTS_SOURCE);
    let depth_ranges = syntax_v1_depth_contract::request_ranges(HINTS_SOURCE);
    let signature_fixture = root.join("crates/orna-lsp/tests/fixtures/signature-actions-v1.orna");
    assert_eq!(
        fs::read_to_string(&signature_fixture).unwrap(),
        SIGNATURE_SOURCE
    );
    let action_fixture =
        root.join("crates/orna-lsp/tests/fixtures/missing-semicolon-code-action-v1.orna");
    assert_eq!(fs::read_to_string(&action_fixture).unwrap(), ACTION_SOURCE);
    let action_signature_requests =
        syntax_v1_action_signature_contract::request_data(SIGNATURE_SOURCE, ACTION_SOURCE);
    let workspace_hierarchy_requests = syntax_v1_workspace_hierarchy_contract::request_data(
        WORKSPACE_HIERARCHY_PROVIDER_SOURCE,
        WORKSPACE_HIERARCHY_CALLER_SOURCE,
    );
    let workspace_provider =
        root.join("crates/orna-lsp/tests/fixtures/workspace-hierarchy-provider-v1.orna");
    let workspace_caller =
        root.join("crates/orna-lsp/tests/fixtures/workspace-hierarchy-caller-v1.orna");
    assert_eq!(
        fs::read_to_string(&workspace_provider).unwrap(),
        WORKSPACE_HIERARCHY_PROVIDER_SOURCE
    );
    assert_eq!(
        fs::read_to_string(&workspace_caller).unwrap(),
        WORKSPACE_HIERARCHY_CALLER_SOURCE
    );
    let folding_selection_fixture =
        root.join("crates/orna-lsp/tests/fixtures/folding-selection-v1.orna");
    assert_eq!(
        fs::read_to_string(&folding_selection_fixture).unwrap(),
        FOLDING_SELECTION_SOURCE
    );
    let folding_selection_requests =
        syntax_v1_folding_selection_contract::request_data(FOLDING_SELECTION_SOURCE);
    let lsp_binary = env!("CARGO_BIN_EXE_orna-lsp");
    let vim_script = format!(
        r#"
set nocompatible
let g:lsp_auto_enable = 0
let g:lsp_async_completion = 0
let g:orna_lsp_command = [{}]
execute 'set runtimepath^=' . fnameescape($ORNA_VIM_LSP_RUNTIME)
execute 'set runtimepath^=' . fnameescape($ORNA_VIM_RUNTIME)
runtime plugin/orna-lsp.vim
runtime plugin/lsp.vim
call lsp#enable()
filetype on
syntax on
execute 'edit ' . fnameescape($ORNA_TEST_FIXTURE)
call append(line('$'), '')
call cursor(line('$'), 1)
call assert_equal('orna', &filetype)
let s:deadline = reltimefloat(reltime()) + 10.0
while !lsp#capabilities#has_completion_provider('orna') && reltimefloat(reltime()) < s:deadline
    sleep 10m
endwhile
call assert_true(lsp#capabilities#has_completion_provider('orna'), 'consumer did not attach to orna-lsp')
let s:consumer_buffer = bufnr('%')
execute 'edit ' . fnameescape($ORNA_TEST_PROVIDER)
call assert_equal('orna', &filetype)
let s:deadline = reltimefloat(reltime()) + 10.0
while !lsp#capabilities#has_completion_provider('orna') && reltimefloat(reltime()) < s:deadline
    sleep 10m
endwhile
call assert_true(lsp#capabilities#has_completion_provider('orna'), 'provider did not attach to orna-lsp')
let s:provider_document = lsp#get_text_document_identifier()
execute 'buffer ' . s:consumer_buffer
call cursor(line('$'), 1)

let s:deadline = reltimefloat(reltime()) + 10.0
while !lsp#capabilities#has_completion_provider('orna') && reltimefloat(reltime()) < s:deadline
    sleep 10m
endwhile
call assert_equal('lsp#complete', &l:omnifunc)
call assert_true(lsp#capabilities#has_completion_provider('orna'), 'orna-lsp did not attach with completion support')

function! OrnaCaptureCompletion(data) abort
    let g:orna_completion_response = a:data['response']
endfunction
function! OrnaCaptureHover(data) abort
    let g:orna_hover_response = a:data['response']
endfunction
function! OrnaCaptureSemanticTokens(data) abort
    let g:orna_semantic_response = a:data['response']
endfunction
function! OrnaCaptureReferences(data) abort
    let g:orna_references_response = a:data['response']
endfunction
function! OrnaCaptureReferencesWithoutDeclaration(data) abort
    let g:orna_references_without_declaration_response = a:data['response']
endfunction
function! OrnaCaptureRename(data) abort
    let g:orna_rename_response = a:data['response']
endfunction
function! OrnaCaptureDepth(name, data) abort
    let g:orna_depth_responses[a:name] = a:data['response']['result']
endfunction
call lsp#send_request('orna', {{
    \ 'method': 'textDocument/completion',
    \ 'params': {{
    \     'textDocument': lsp#get_text_document_identifier(),
    \     'position': lsp#get_position(),
    \     'context': {{ 'triggerKind': 1 }},
    \ }},
    \ 'on_notification': function('OrnaCaptureCompletion'),
\ }})
let s:deadline = reltimefloat(reltime()) + 10.0
while !exists('g:orna_completion_response') && reltimefloat(reltime()) < s:deadline
    sleep 10m
endwhile
call assert_true(exists('g:orna_completion_response'), 'vim-lsp completion request timed out')
let s:response = g:orna_completion_response
let s:adapted = lsp#omni#get_vim_completion_items({{
    \ 'server': lsp#get_server_info('orna'),
    \ 'position': lsp#get_position(),
    \ 'response': s:response,
\ }})
call cursor(1, 1)
let s:call_line = search('add(value, 2', 'W')
call assert_true(s:call_line > 0, 'consumer fixture omitted the add call')
let s:call_column = stridx(getline(s:call_line), 'add(value, 2') + 1
call assert_true(s:call_column > 0, 'consumer add call has no column')
call cursor(s:call_line, s:call_column)
call lsp#send_request('orna', {{
    \ 'method': 'textDocument/hover',
    \ 'params': {{
    \     'textDocument': lsp#get_text_document_identifier(),
    \     'position': lsp#get_position(),
    \ }},
    \ 'on_notification': function('OrnaCaptureHover'),
\ }})
let s:deadline = reltimefloat(reltime()) + 10.0
while !exists('g:orna_hover_response') && reltimefloat(reltime()) < s:deadline
    sleep 10m
endwhile
call assert_true(exists('g:orna_hover_response'), 'vim-lsp hover request timed out')
let s:hover = g:orna_hover_response['result']
let s:consumer_document = lsp#get_text_document_identifier()
call lsp#send_request('orna', {{
    \ 'method': 'textDocument/references',
    \ 'params': {{
    \     'textDocument': s:consumer_document,
    \     'position': lsp#get_position(),
    \     'context': {{ 'includeDeclaration': v:true }},
    \ }},
    \ 'on_notification': function('OrnaCaptureReferences'),
\ }})
let s:deadline = reltimefloat(reltime()) + 10.0
while !exists('g:orna_references_response') && reltimefloat(reltime()) < s:deadline
    sleep 10m
endwhile
call assert_true(exists('g:orna_references_response'), 'vim-lsp references request timed out')
let s:references = g:orna_references_response['result']
call lsp#send_request('orna', {{
    \ 'method': 'textDocument/references',
    \ 'params': {{
    \     'textDocument': s:consumer_document,
    \     'position': lsp#get_position(),
    \     'context': {{ 'includeDeclaration': v:false }},
    \ }},
    \ 'on_notification': function('OrnaCaptureReferencesWithoutDeclaration'),
\ }})
let s:deadline = reltimefloat(reltime()) + 10.0
while !exists('g:orna_references_without_declaration_response') && reltimefloat(reltime()) < s:deadline
    sleep 10m
endwhile
call assert_true(exists('g:orna_references_without_declaration_response'), 'vim-lsp call-site references request timed out')
let s:references_without_declaration = g:orna_references_without_declaration_response['result']
call lsp#send_request('orna', {{
    \ 'method': 'textDocument/rename',
    \ 'params': {{
    \     'textDocument': s:consumer_document,
    \     'position': lsp#get_position(),
    \     'newName': 'sum',
    \ }},
    \ 'on_notification': function('OrnaCaptureRename'),
\ }})
let s:deadline = reltimefloat(reltime()) + 10.0
while !exists('g:orna_rename_response') && reltimefloat(reltime()) < s:deadline
    sleep 10m
endwhile
call assert_true(exists('g:orna_rename_response'), 'vim-lsp rename request timed out')
let s:rename = g:orna_rename_response['result']

execute 'edit ' . fnameescape($ORNA_TEST_SEMANTIC_FIXTURE)
call assert_equal('orna', &filetype)
let s:deadline = reltimefloat(reltime()) + 10.0
while !lsp#capabilities#has_completion_provider('orna') && reltimefloat(reltime()) < s:deadline
    sleep 10m
endwhile
call assert_true(lsp#capabilities#has_completion_provider('orna'), 'semantic fixture did not attach to orna-lsp')
call lsp#send_request('orna', {{
    \ 'method': 'textDocument/semanticTokens/full',
    \ 'params': {{ 'textDocument': lsp#get_text_document_identifier() }},
    \ 'on_notification': function('OrnaCaptureSemanticTokens'),
\ }})
let s:deadline = reltimefloat(reltime()) + 10.0
while !exists('g:orna_semantic_response') && reltimefloat(reltime()) < s:deadline
    sleep 10m
endwhile
call assert_true(exists('g:orna_semantic_response'), 'vim-lsp semantic token request timed out')
let s:semantic = g:orna_semantic_response['result']

execute 'edit ' . fnameescape($ORNA_TEST_DEPTH_FIXTURE)
call assert_equal('orna', &filetype)
let s:deadline = reltimefloat(reltime()) + 10.0
while !lsp#capabilities#has_completion_provider('orna') && reltimefloat(reltime()) < s:deadline
    sleep 10m
endwhile
call assert_true(lsp#capabilities#has_completion_provider('orna'), 'depth fixture did not attach to orna-lsp')
let s:depth_document = lsp#get_text_document_identifier()
let s:depth_ranges = json_decode($ORNA_DEPTH_RANGES)
let g:orna_depth_responses = {{}}
call lsp#send_request('orna', {{
    \ 'method': 'textDocument/semanticTokens/full',
    \ 'params': {{ 'textDocument': s:depth_document }},
    \ 'on_notification': function('OrnaCaptureDepth', ['semantic']),
\ }})
call lsp#send_request('orna', {{
    \ 'method': 'textDocument/semanticTokens/range',
    \ 'params': {{ 'textDocument': s:depth_document, 'range': s:depth_ranges.semantic }},
    \ 'on_notification': function('OrnaCaptureDepth', ['semantic_range']),
\ }})
for [s:name, s:range_key] in items({{
    \ 'hints_full': 'hints_full',
    \ 'hints_call': 'hints_call',
    \ 'hints_inferred': 'hints_inferred',
    \ 'hints_annotated': 'hints_annotated',
    \ 'hints_shadowed': 'hints_shadowed',
\ }})
    call lsp#send_request('orna', {{
        \ 'method': 'textDocument/inlayHint',
        \ 'params': {{ 'textDocument': s:depth_document, 'range': s:depth_ranges[s:range_key] }},
        \ 'on_notification': function('OrnaCaptureDepth', [s:name]),
    \ }})
endfor
let s:deadline = reltimefloat(reltime()) + 10.0
while len(g:orna_depth_responses) < 7 && reltimefloat(reltime()) < s:deadline
    sleep 10m
endwhile
call assert_equal(7, len(g:orna_depth_responses), 'vim-lsp semantic/inlay depth requests timed out')

let s:action_signature_requests = json_decode($ORNA_ACTION_SIGNATURE_REQUESTS)
execute 'edit ' . fnameescape($ORNA_TEST_SIGNATURE_FIXTURE)
call assert_equal('orna', &filetype)
let s:deadline = reltimefloat(reltime()) + 10.0
while !lsp#capabilities#has_completion_provider('orna') && reltimefloat(reltime()) < s:deadline
    sleep 10m
endwhile
call assert_true(lsp#capabilities#has_completion_provider('orna'), 'signature fixture did not attach to orna-lsp')
let s:signature_document = lsp#get_text_document_identifier()
for [s:name, s:position_key] in items({{
    \ 'signature_nested_tuple': 'signature_nested_tuple',
    \ 'signature_named_argument': 'signature_named_argument',
    \ 'signature_nested_named_argument': 'signature_nested_named_argument',
    \ 'signature_shadowed_call': 'signature_shadowed_call',
\ }})
    call lsp#send_request('orna', {{
        \ 'method': 'textDocument/signatureHelp',
        \ 'params': {{ 'textDocument': s:signature_document, 'position': s:action_signature_requests[s:position_key] }},
        \ 'on_notification': function('OrnaCaptureDepth', [s:name]),
    \ }})
endfor

execute 'edit ' . fnameescape($ORNA_TEST_ACTION_FIXTURE)
call assert_equal('orna', &filetype)
let s:deadline = reltimefloat(reltime()) + 10.0
while !lsp#capabilities#has_completion_provider('orna') && reltimefloat(reltime()) < s:deadline
    sleep 10m
endwhile
call assert_true(lsp#capabilities#has_completion_provider('orna'), 'code-action fixture did not attach to orna-lsp')
let s:action_document = lsp#get_text_document_identifier()
let s:action_uri = s:action_document['uri']
for [s:name, s:range_key, s:kind] in [
    \ ['code_action_quickfix', 'code_action_full_range', 'quickfix'],
    \ ['code_action_wrong_kind', 'code_action_full_range', 'refactor'],
    \ ['code_action_outside_range', 'code_action_outside_range', 'quickfix'],
\ ]
    call lsp#send_request('orna', {{
        \ 'method': 'textDocument/codeAction',
        \ 'params': {{
        \     'textDocument': s:action_document,
        \     'range': s:action_signature_requests[s:range_key],
        \     'context': {{ 'diagnostics': [], 'only': [s:kind] }},
        \ }},
        \ 'on_notification': function('OrnaCaptureDepth', [s:name]),
    \ }})
endfor
let s:deadline = reltimefloat(reltime()) + 10.0
while len(g:orna_depth_responses) < 14 && reltimefloat(reltime()) < s:deadline
    sleep 10m
endwhile
call assert_equal(14, len(g:orna_depth_responses), 'vim-lsp code-action/signature-help requests timed out')

function! OrnaCaptureWorkspaceHierarchy(name, data) abort
    let g:orna_workspace_hierarchy_responses[a:name] = a:data['response']['result']
endfunction
function! OrnaCaptureWorkspacePrepare(name, data) abort
    let l:result = a:data['response']['result']
    let g:orna_workspace_hierarchy_responses[a:name] = l:result
    if type(l:result) != v:t_list || empty(l:result)
        return
    endif
    let l:item = l:result[0]
    if a:name ==# 'call_root_item'
        call lsp#send_request('orna', {{
            \ 'method': 'callHierarchy/outgoingCalls',
            \ 'params': {{ 'item': l:item }},
            \ 'on_notification': function('OrnaCaptureWorkspaceHierarchy', ['call_root_outgoing']),
        \ }})
        call lsp#send_request('orna', {{
            \ 'method': 'callHierarchy/incomingCalls',
            \ 'params': {{ 'item': l:item }},
            \ 'on_notification': function('OrnaCaptureWorkspaceHierarchy', ['call_root_incoming']),
        \ }})
    elseif a:name ==# 'call_seed_item'
        call lsp#send_request('orna', {{
            \ 'method': 'callHierarchy/incomingCalls',
            \ 'params': {{ 'item': l:item }},
            \ 'on_notification': function('OrnaCaptureWorkspaceHierarchy', ['call_seed_incoming']),
        \ }})
    elseif a:name ==# 'call_shadowed_item'
        call lsp#send_request('orna', {{
            \ 'method': 'callHierarchy/outgoingCalls',
            \ 'params': {{ 'item': l:item }},
            \ 'on_notification': function('OrnaCaptureWorkspaceHierarchy', ['call_shadowed_outgoing']),
        \ }})
    elseif a:name ==# 'call_unresolved_item'
        call lsp#send_request('orna', {{
            \ 'method': 'callHierarchy/outgoingCalls',
            \ 'params': {{ 'item': l:item }},
            \ 'on_notification': function('OrnaCaptureWorkspaceHierarchy', ['call_unresolved_outgoing']),
        \ }})
    endif
endfunction

execute 'edit ' . fnameescape($ORNA_WORKSPACE_PROVIDER_FIXTURE)
call assert_equal('orna', &filetype)
let s:deadline = reltimefloat(reltime()) + 10.0
while !lsp#capabilities#has_completion_provider('orna') && reltimefloat(reltime()) < s:deadline
    sleep 10m
endwhile
call assert_true(lsp#capabilities#has_completion_provider('orna'), 'workspace provider fixture did not attach')
let s:workspace_provider_document = lsp#get_text_document_identifier()
execute 'edit ' . fnameescape($ORNA_WORKSPACE_CALLER_FIXTURE)
call assert_equal('orna', &filetype)
let s:deadline = reltimefloat(reltime()) + 10.0
while !lsp#capabilities#has_completion_provider('orna') && reltimefloat(reltime()) < s:deadline
    sleep 10m
endwhile
call assert_true(lsp#capabilities#has_completion_provider('orna'), 'workspace caller fixture did not attach')
let s:workspace_caller_document = lsp#get_text_document_identifier()
let s:workspace_hierarchy_requests = json_decode($ORNA_WORKSPACE_HIERARCHY_REQUESTS)
let g:orna_workspace_hierarchy_responses = {{}}
for [s:name, s:query] in items({{
    \ 'workspace_mid': 'mid',
    \ 'workspace_mid_repeat': 'mid',
    \ 'workspace_rem': 'rem',
    \ 'workspace_remi': 'remi',
}})
    call lsp#send_request('orna', {{
        \ 'method': 'workspace/symbol',
        \ 'params': {{ 'query': s:query }},
        \ 'on_notification': function('OrnaCaptureWorkspaceHierarchy', [s:name]),
    \ }})
endfor
for [s:name, s:position_key, s:document] in [
    \ ['call_root_item', 'root_definition', s:workspace_provider_document],
    \ ['call_seed_item', 'seed_definition', s:workspace_provider_document],
    \ ['call_shadowed_item', 'shadowed_definition', s:workspace_provider_document],
    \ ['call_unresolved_item', 'unresolved_definition', s:workspace_provider_document],
    \ ['call_root_reference', 'root_reference', s:workspace_caller_document],
    \ ['call_ambiguous_reference', 'ambiguous_reference', s:workspace_caller_document],
]
    call lsp#send_request('orna', {{
        \ 'method': 'textDocument/prepareCallHierarchy',
        \ 'params': {{ 'textDocument': s:document, 'position': s:workspace_hierarchy_requests[s:position_key] }},
        \ 'on_notification': function('OrnaCaptureWorkspacePrepare', [s:name]),
    \ }})
endfor
let s:deadline = reltimefloat(reltime()) + 10.0
while len(g:orna_workspace_hierarchy_responses) < 15 && reltimefloat(reltime()) < s:deadline
    sleep 10m
endwhile
call assert_equal(15, len(g:orna_workspace_hierarchy_responses), 'vim-lsp workspace-symbol/call-hierarchy requests timed out')

function! OrnaCaptureFoldingSelection(name, data) abort
    let g:orna_folding_selection_responses[a:name] = a:data['response']['result']
endfunction
execute 'edit ' . fnameescape($ORNA_FOLDING_SELECTION_FIXTURE)
call assert_equal('orna', &filetype)
let s:deadline = reltimefloat(reltime()) + 10.0
while !lsp#capabilities#has_completion_provider('orna') && reltimefloat(reltime()) < s:deadline
    sleep 10m
endwhile
call assert_true(lsp#capabilities#has_completion_provider('orna'), 'folding/selection fixture did not attach')
let s:folding_selection_document = lsp#get_text_document_identifier()
let s:folding_selection_requests = json_decode($ORNA_FOLDING_SELECTION_REQUESTS)
let g:orna_folding_selection_responses = {{}}
call lsp#send_request('orna', {{
    \ 'method': 'textDocument/foldingRange',
    \ 'params': {{ 'textDocument': s:folding_selection_document }},
    \ 'on_notification': function('OrnaCaptureFoldingSelection', ['folding_ranges']),
\ }})
call lsp#send_request('orna', {{
    \ 'method': 'textDocument/selectionRange',
    \ 'params': {{ 'textDocument': s:folding_selection_document, 'positions': s:folding_selection_requests.selection_positions }},
    \ 'on_notification': function('OrnaCaptureFoldingSelection', ['selection_ranges']),
\ }})
let s:deadline = reltimefloat(reltime()) + 10.0
while len(g:orna_folding_selection_responses) < 2 && reltimefloat(reltime()) < s:deadline
    sleep 10m
endwhile
call assert_equal(2, len(g:orna_folding_selection_responses), 'vim-lsp folding/selection requests timed out')
call writefile([json_encode({{
    \ 'omnifunc': &l:omnifunc,
    \ 'completion': s:response['result'],
    \ 'adapted': s:adapted['items'],
    \ 'hover': s:hover,
    \ 'semantic': s:semantic,
    \ 'depth_semantic': g:orna_depth_responses.semantic,
    \ 'depth_semantic_range': g:orna_depth_responses.semantic_range,
    \ 'depth_hints_full': g:orna_depth_responses.hints_full,
    \ 'depth_hints_call': g:orna_depth_responses.hints_call,
    \ 'depth_hints_inferred': g:orna_depth_responses.hints_inferred,
    \ 'depth_hints_annotated': g:orna_depth_responses.hints_annotated,
    \ 'depth_hints_shadowed': g:orna_depth_responses.hints_shadowed,
    \ 'signature_nested_tuple': g:orna_depth_responses.signature_nested_tuple,
    \ 'signature_named_argument': g:orna_depth_responses.signature_named_argument,
    \ 'signature_nested_named_argument': g:orna_depth_responses.signature_nested_named_argument,
    \ 'signature_shadowed_call': g:orna_depth_responses.signature_shadowed_call,
    \ 'code_action_quickfix': g:orna_depth_responses.code_action_quickfix,
    \ 'code_action_wrong_kind': g:orna_depth_responses.code_action_wrong_kind,
    \ 'code_action_outside_range': g:orna_depth_responses.code_action_outside_range,
    \ 'action_uri': s:action_uri,
    \ 'workspace_mid': g:orna_workspace_hierarchy_responses.workspace_mid,
    \ 'workspace_mid_repeat': g:orna_workspace_hierarchy_responses.workspace_mid_repeat,
    \ 'workspace_rem': g:orna_workspace_hierarchy_responses.workspace_rem,
    \ 'workspace_remi': g:orna_workspace_hierarchy_responses.workspace_remi,
    \ 'call_root_item': g:orna_workspace_hierarchy_responses.call_root_item,
    \ 'call_root_outgoing': g:orna_workspace_hierarchy_responses.call_root_outgoing,
    \ 'call_root_incoming': g:orna_workspace_hierarchy_responses.call_root_incoming,
    \ 'call_seed_item': g:orna_workspace_hierarchy_responses.call_seed_item,
    \ 'call_seed_incoming': g:orna_workspace_hierarchy_responses.call_seed_incoming,
    \ 'call_shadowed_outgoing': g:orna_workspace_hierarchy_responses.call_shadowed_outgoing,
    \ 'call_unresolved_outgoing': g:orna_workspace_hierarchy_responses.call_unresolved_outgoing,
    \ 'call_root_reference': g:orna_workspace_hierarchy_responses.call_root_reference,
    \ 'call_ambiguous_reference': g:orna_workspace_hierarchy_responses.call_ambiguous_reference,
    \ 'workspace_provider_uri': s:workspace_provider_document['uri'],
    \ 'workspace_caller_uri': s:workspace_caller_document['uri'],
    \ 'folding_ranges': g:orna_folding_selection_responses.folding_ranges,
    \ 'selection_ranges': g:orna_folding_selection_responses.selection_ranges,
    \ 'folding_selection_uri': s:folding_selection_document['uri'],
    \ 'consumer_uri': s:consumer_document['uri'],
    \ 'provider_uri': s:provider_document['uri'],
    \ 'references': s:references,
    \ 'references_without_declaration': s:references_without_declaration,
    \ 'rename': s:rename,
\ }})], $ORNA_EDITOR_RESULT)
if !empty(v:errors)
    call writefile(v:errors, $ORNA_EDITOR_RESULT . '.errors')
    cquit 1
endif
qa!
"#,
        vim_string(lsp_binary),
    );
    fs::write(&script, vim_script).expect("write Vim LSP integration script");

    let output = Command::new(&vim)
        .args(["-Nu", "NONE", "-n", "-es", "-S"])
        .arg(&script)
        .current_dir(&root)
        .env("ORNA_VIM_LSP_RUNTIME", vim_lsp)
        .env("ORNA_VIM_RUNTIME", root.join("editors/vim"))
        .env("ORNA_TEST_FIXTURE", &fixture)
        .env("ORNA_TEST_PROVIDER", &provider)
        .env("ORNA_TEST_SEMANTIC_FIXTURE", &semantic_fixture)
        .env("ORNA_TEST_DEPTH_FIXTURE", &depth_fixture)
        .env("ORNA_TEST_SIGNATURE_FIXTURE", &signature_fixture)
        .env("ORNA_TEST_ACTION_FIXTURE", &action_fixture)
        .env("ORNA_WORKSPACE_PROVIDER_FIXTURE", &workspace_provider)
        .env("ORNA_WORKSPACE_CALLER_FIXTURE", &workspace_caller)
        .env("ORNA_FOLDING_SELECTION_FIXTURE", &folding_selection_fixture)
        .env(
            "ORNA_DEPTH_RANGES",
            serde_json::to_string(&depth_ranges).unwrap(),
        )
        .env(
            "ORNA_ACTION_SIGNATURE_REQUESTS",
            serde_json::to_string(&action_signature_requests).unwrap(),
        )
        .env(
            "ORNA_WORKSPACE_HIERARCHY_REQUESTS",
            serde_json::to_string(&workspace_hierarchy_requests).unwrap(),
        )
        .env(
            "ORNA_FOLDING_SELECTION_REQUESTS",
            serde_json::to_string(&folding_selection_requests).unwrap(),
        )
        .env("ORNA_EDITOR_RESULT", &result_path)
        .output()
        .unwrap_or_else(|error| panic!("start Vim at {}: {error}", vim.display()));
    let result = fs::read_to_string(&result_path);
    let vim_errors = fs::read_to_string(result_path.with_extension("json.errors"));
    let _ = fs::remove_file(&script);
    let _ = fs::remove_file(&result_path);
    let _ = fs::remove_file(result_path.with_extension("json.errors"));
    let result = result.unwrap_or_else(|error| {
        panic!(
            "Vim did not write completion evidence (exit {:?}): {error}\n{}\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            format!(
                "{}\n{}",
                String::from_utf8_lossy(&output.stderr),
                vim_errors.as_deref().unwrap_or("")
            ),
        )
    });
    assert!(
        output.status.success(),
        "Vim LSP completion failed (exit {:?}):\n{}\n{}\n{result}\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
        vim_errors.as_deref().unwrap_or(""),
    );
    let evidence: Value = serde_json::from_str(&result).expect("Vim completion evidence JSON");
    assert_eq!(evidence["omnifunc"], "lsp#complete");
    completion_contract::assert_lsp_completion_contract(&evidence["completion"], "Vim vim-lsp");
    assert_vim_completion_projection(&evidence["adapted"]);
    hover_semantic_contract::assert_hover_contract(&evidence["hover"], "Vim vim-lsp");
    hover_semantic_contract::assert_semantic_token_contract(
        SEMANTIC_SOURCE,
        &evidence["semantic"],
        "Vim vim-lsp",
    );
    syntax_v1_depth_contract::assert_semantic_depth_contract(
        HINTS_SOURCE,
        &evidence["depth_semantic"],
        &evidence["depth_semantic_range"],
        "Vim vim-lsp",
    );
    syntax_v1_depth_contract::assert_inlay_hint_depth_contract(
        HINTS_SOURCE,
        &evidence["depth_hints_full"],
        &evidence["depth_hints_call"],
        &evidence["depth_hints_inferred"],
        &evidence["depth_hints_annotated"],
        &evidence["depth_hints_shadowed"],
        "Vim vim-lsp",
    );
    syntax_v1_action_signature_contract::assert_signature_help_contract(
        &evidence["signature_nested_tuple"],
        &evidence["signature_named_argument"],
        &evidence["signature_nested_named_argument"],
        &evidence["signature_shadowed_call"],
        "Vim vim-lsp",
    );
    syntax_v1_action_signature_contract::assert_code_action_contract(
        ACTION_SOURCE,
        evidence["action_uri"]
            .as_str()
            .expect("Vim action document URI"),
        &evidence["code_action_quickfix"],
        &evidence["code_action_wrong_kind"],
        &evidence["code_action_outside_range"],
        "Vim vim-lsp",
    );
    syntax_v1_workspace_hierarchy_contract::assert_contract(
        WORKSPACE_HIERARCHY_PROVIDER_SOURCE,
        WORKSPACE_HIERARCHY_CALLER_SOURCE,
        evidence["workspace_provider_uri"]
            .as_str()
            .expect("Vim workspace provider URI"),
        evidence["workspace_caller_uri"]
            .as_str()
            .expect("Vim workspace caller URI"),
        &evidence,
        "Vim vim-lsp",
    );
    syntax_v1_folding_selection_contract::assert_contract(
        FOLDING_SELECTION_SOURCE,
        &evidence["folding_ranges"],
        &evidence["selection_ranges"],
        "Vim vim-lsp",
    );
    let consumer_uri = evidence["consumer_uri"]
        .as_str()
        .expect("Vim consumer document URI");
    let provider_uri = evidence["provider_uri"]
        .as_str()
        .expect("Vim provider document URI");
    let symbol_sources = [(consumer_uri, SOURCE), (provider_uri, PROVIDER_SOURCE)];
    hover_semantic_contract::assert_references_contract(
        &symbol_sources,
        &evidence["references"],
        true,
        "Vim vim-lsp",
    );
    hover_semantic_contract::assert_references_contract(
        &symbol_sources,
        &evidence["references_without_declaration"],
        false,
        "Vim vim-lsp without declaration",
    );
    hover_semantic_contract::assert_rename_contract(
        &symbol_sources,
        &evidence["rename"],
        "sum",
        "Vim vim-lsp",
    );
    println!(
        "Vim integration evidence: ATTACHED=pass DEPENDENCY_ORDER=consumer-before-provider OMNIFUNC=pass COMPLETION=pass HOVER=pass REFERENCES=pass RENAME=pass SEMANTIC_TOKENS=pass SEMANTIC_DEPTH=pass RANGED_TOKENS=pass INLAY_HINT_DEPTH=pass SIGNATURE_HELP=pass CODE_ACTION=pass WORKSPACE_SYMBOL=pass CALL_HIERARCHY=pass FOLDING_RANGE=pass SELECTION_RANGE=pass"
    );
}

#[test]
fn emacs_eglot_attaches_and_exposes_syntax_v1_completion_at_point() {
    assert_v1_attachment_fixture();
    let Some(emacs) = executable("ORNA_TEST_EMACS", "emacs") else {
        eprintln!("SKIP: Emacs is not installed; set ORNA_TEST_EMACS to its executable");
        return;
    };
    let eglot = Command::new(&emacs)
        .args([
            "--batch",
            "--quick",
            "--eval",
            "(require 'package)(package-initialize)(princ (if (require 'eglot nil t) \"available\" \"unavailable\"))",
        ])
        .output()
        .unwrap_or_else(|error| panic!("probe Eglot through Emacs: {error}"));
    if !eglot.status.success() || !String::from_utf8_lossy(&eglot.stdout).contains("available") {
        eprintln!(
            "SKIP: Eglot is unavailable in this Emacs; install or enable Eglot to run the host probe"
        );
        return;
    }

    let root = repo_root();
    let fixture = root.join("crates/orna-lsp/tests/fixtures/ji3t0-lsp-v1.orna");
    assert_eq!(fs::read_to_string(&fixture).unwrap(), SOURCE);
    let provider = root.join("crates/orna-lsp/tests/fixtures/expressions-v1.orna");
    assert_eq!(fs::read_to_string(&provider).unwrap(), PROVIDER_SOURCE);
    assert!(
        provider < fixture,
        "provider URI must sort before consumer URI"
    );
    let script = temporary_path("el");
    let completion_result_path = temporary_path("json");
    let plugin = root.join("editors/emacs/orna-eglot.el");
    let expected = completion_contract::expected_keywords()
        .iter()
        .map(|keyword| elisp_string(keyword))
        .collect::<Vec<_>>()
        .join(" ");
    let elisp = format!(
        r#"
(require 'package)
(package-initialize)
(require 'cl-lib)
(require 'jsonrpc)
(require 'json)
(load-file {})
(setq orna-eglot-server-command (list {}))
(orna-setup-eglot)

(defun orna-test-get (object key)
  (let ((keyword (intern (concat ":" key)))
        (symbol (intern key)))
    (cond
     ((hash-table-p object) (or (gethash key object) (gethash keyword object) (gethash symbol object)))
     ((and (listp object) (keywordp (car-safe object))) (plist-get object keyword))
     ((listp object) (or (cdr (assoc-string key object)) (cdr (assq symbol object)))))))
(defun orna-test-list (value)
  (cond ((stringp value) nil)
        ((vectorp value) (append value nil))
        ((listp value) value)
        (t nil)))

(let ((buffer (find-file-noselect {}))
      provider-buffer)
  (unwind-protect
      (with-current-buffer buffer
        (unless (eq major-mode 'orna-mode) (error "Orna major mode did not load"))
        (let ((deadline (+ (float-time) 12.0)))
          (while (and (not (eglot-managed-p)) (< (float-time) deadline))
            (accept-process-output nil 0.05)))
        (unless (eglot-managed-p) (error "Eglot did not attach orna-lsp"))
        (let ((consumer-server (eglot-current-server)))
          (setq provider-buffer (find-file-noselect {}))
          (with-current-buffer provider-buffer
            (let ((deadline (+ (float-time) 12.0)))
              (while (and (not (eglot-managed-p)) (< (float-time) deadline))
                (accept-process-output nil 0.05)))
            (unless (eglot-managed-p) (error "Eglot did not attach provider buffer"))
            (unless (eq (eglot-current-server) consumer-server)
              (error "consumer and provider attached to different Orna servers"))))
        (unless (memq 'eglot-completion-at-point completion-at-point-functions)
          (error "Eglot did not install completion-at-point"))
        (goto-char (point-max))
        (let* ((server (eglot-current-server))
               (response (jsonrpc-request server :textDocument/completion
                                          (eglot--TextDocumentPositionParams)))
               (items (orna-test-list (or (orna-test-get response "items") response)))
               (keywords
                (sort
                 (mapcar (lambda (item) (orna-test-get item "label"))
                         (cl-remove-if-not
                          (lambda (item) (= (or (orna-test-get item "kind") 0) 14))
                          items))
                 #'string<))
               (expected '({})))
          (with-temp-file (getenv "ORNA_COMPLETION_RESULT")
            (insert (json-encode response)))
          (unless (equal keywords expected)
            (error "syntax-v1 completion keywords differ: %S" keywords))
          (let ((add (cl-find-if (lambda (item) (equal (orna-test-get item "label") "add")) items)))
            (unless add (error "completion omitted fixture function add"))
            (unless (equal (orna-test-get add "detail") "pub fn add(left: Int, right: Int): Int")
              (error "unexpected add completion detail: %S" add))
            (unless (equal (orna-test-get (orna-test-get add "documentation") "kind") "markdown")
              (error "add completion documentation is not Markdown: %S" add))
            (unless (equal (orna-test-get (orna-test-get add "documentation") "value") "Add two integer values.")
              (error "add completion lost fixture documentation: %S" add))
            (unless (equal (orna-test-get add "insertText") "add(${{1:left}}, ${{2:right}})")
              (error "unexpected add completion snippet: %S" add))
            (unless (= (orna-test-get add "insertTextFormat") 2)
              (error "add completion did not advertise snippet formatting: %S" add)))
          (goto-char (point-min))
          (search-forward "add(value")
          (backward-char 6)
          (let* ((capf (eglot-completion-at-point))
                 (candidates (and capf (all-completions "add" (nth 2 capf)))))
            (unless capf (error "Eglot completion-at-point returned no completion surface"))
            (unless (member "add" (mapcar #'substring-no-properties candidates))
              (error "completion-at-point omitted add: %S" candidates)))
          (princ "EMACS_LSP_ATTACHMENT=pass\n")
          (princ "EMACS_DEPENDENCY_ORDER=consumer-before-provider\n")
          (princ "EMACS_COMPLETION_KEYWORDS=pass\n")
          (princ "EMACS_COMPLETION_ADD=pass\n")
          (princ "EMACS_COMPLETION_AT_POINT=pass\n")))
    (kill-buffer buffer)
    (when (buffer-live-p provider-buffer) (kill-buffer provider-buffer))))
"#,
        elisp_string(&plugin.display().to_string()),
        elisp_string(env!("CARGO_BIN_EXE_orna-lsp")),
        elisp_string(&fixture.display().to_string()),
        elisp_string(&provider.display().to_string()),
        expected,
    );
    fs::write(&script, elisp).expect("write Emacs Eglot integration script");
    let output = Command::new(&emacs)
        .args(["--batch", "--quick", "--script"])
        .arg(&script)
        .current_dir(&root)
        .env("ORNA_COMPLETION_RESULT", &completion_result_path)
        .output()
        .unwrap_or_else(|error| panic!("start Emacs at {}: {error}", emacs.display()));
    let _ = fs::remove_file(script);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "Emacs Eglot completion failed (exit {:?}):\n{stdout}\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    let completion_result =
        fs::read_to_string(&completion_result_path).expect("Emacs Eglot completion evidence JSON");
    let _ = fs::remove_file(&completion_result_path);
    let completion_result: Value =
        serde_json::from_str(&completion_result).expect("decode Emacs Eglot completion evidence");
    completion_contract::assert_lsp_completion_contract(&completion_result, "Emacs Eglot");
    for evidence in [
        "EMACS_LSP_ATTACHMENT=pass",
        "EMACS_DEPENDENCY_ORDER=consumer-before-provider",
        "EMACS_COMPLETION_KEYWORDS=pass",
        "EMACS_COMPLETION_ADD=pass",
        "EMACS_COMPLETION_AT_POINT=pass",
    ] {
        assert!(
            stdout.contains(evidence),
            "Emacs omitted {evidence}: {stdout}"
        );
    }
    println!("Emacs integration evidence:\n{stdout}");
}

fn assert_v1_attachment_fixture() {
    for (name, source) in [("consumer", SOURCE), ("provider", PROVIDER_SOURCE)] {
        let parsed = orna_syntax_v1::parse_module(source);
        assert!(
            parsed.is_ok(),
            "the editor attachment {name} fixture must contain only syntax-v1 source: {:?}",
            parsed.diagnostics
        );
    }
}

fn assert_vim_completion_projection(items: &Value) {
    let items = items
        .as_array()
        .expect("vim-lsp completion projection list");
    let actual_keywords = items
        .iter()
        .filter(|item| item["kind"] == "keyword")
        .map(|item| item["abbr"].as_str().unwrap().to_owned())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        actual_keywords,
        completion_contract::expected_keywords(),
        "Vim omni completion inventory"
    );
    assert!(
        items.iter().any(|item| item["abbr"] == "add~"),
        "Vim completion adapter omitted the add snippet candidate"
    );
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("orna-lsp is under crates")
        .to_path_buf()
}

fn executable(override_name: &str, name: &str) -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(override_name) {
        return Some(PathBuf::from(path));
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
}

fn vim_lsp_runtime() -> Option<PathBuf> {
    let path = std::env::var_os("ORNA_TEST_VIM_LSP_RUNTIME")?;
    let path = PathBuf::from(path);
    (path.join("plugin/lsp.vim").is_file() && path.join("autoload/lsp.vim").is_file())
        .then_some(path)
}

fn vim_string(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn elisp_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

static TEMP_FILE_ID: AtomicU64 = AtomicU64::new(0);

fn temporary_path(extension: &str) -> PathBuf {
    let id = TEMP_FILE_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "orna-editor-attach-{}-{id}.{extension}",
        std::process::id()
    ))
}
