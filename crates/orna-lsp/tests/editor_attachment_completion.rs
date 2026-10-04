//! Real editor-client attachment proofs for syntax-v1 completion.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

use serde_json::Value;

#[path = "support/completion_contract.rs"]
mod completion_contract;

const SOURCE: &str = include_str!("fixtures/ji3t0-lsp-v1.orna");
const PROVIDER_SOURCE: &str = include_str!("fixtures/expressions-v1.orna");

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
fn vim_lsp_attaches_and_exposes_syntax_v1_completion() {
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
call writefile([json_encode({{
    \ 'omnifunc': &l:omnifunc,
    \ 'completion': s:response['result'],
    \ 'adapted': s:adapted['items'],
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
    completion_contract::assert_vim_completion_projection(&evidence["adapted"]);
    println!("Vim integration evidence: ATTACHED=pass DEPENDENCY_ORDER=consumer-before-provider OMNIFUNC=pass COMPLETION=pass");
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
            (unless (equal (orna-test-get add "detail") "fn add(left: Int, right: Int): Int")
              (error "unexpected add completion detail: %S" add))
            (unless (equal (orna-test-get add "documentation") "Add two integer values.")
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
