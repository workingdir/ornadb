;;; orna-eglot.el --- Orna 1.0.0 lexical highlighting -*- lexical-binding: t; -*-
;; Generated from orna-syntax-v1. Install packaging is maintained separately.
(require 'eglot)
(defvar orna-keywords '("as" "assert" "base" "break" "case" "continue" "dim" "else" "enum" "false" "fn" "for" "if" "impl" "in" "let" "loop" "null" "offset" "affine" "protocol" "pub" "return" "self" "static" "table" "true" "type" "unit" "use" "while"))
(defvar orna-operators '("..=" "=>" "==" "!=" "<=" ">=" "??" "|?" "&&" "||" "+=" "-=" "*=" "/=" ".." "|" "!" "=" "<" ">" "+" "-" "*" "/" "%" "^" "?"))
(defvar orna-font-lock-keywords
  `((,(regexp-opt orna-keywords 'words) . font-lock-keyword-face)
    ("//.*$" . font-lock-comment-face)
    ("/\\\\*\\\\(?:.\\\\|\\\\n\\\\)*?\\\\*/" . font-lock-comment-face)
    ("\\\"\\\\(?:\\\\\\\\.\\\\|[^\\\"\\\\]\\\\)*\\\"" . font-lock-string-face)
    ("\\(?:[0-9]\\{4\\}-[0-9]\\{2\\}-[0-9]\\{2\\}T[0-9]\\{2\\}:[0-9]\\{2\\}:[0-9]\\{2\\}\\(?:\\.[0-9]*)?\\(?:Z|[+-][0-9]\\{2\\}:[0-9]\\{2\\})|[0-9]\\{4\\}-[0-9]\\{2\\}-[0-9]\\{2\\}|0x[0-9A-Fa-f_]*|0b[01_]*|[0-9][0-9_]*\\(?:\\.[0-9_]+)?\\(?:[eE][+-]?[0-9_]*)?f?)" . font-lock-constant-face)
    (,(regexp-opt orna-operators) . font-lock-builtin-face))
  "Lexical highlighting generated from the 1.0.0 lexer.")
(define-derived-mode orna-mode prog-mode "Orna"
  "Major mode for Orna source files."
  (setq-local comment-start "// ")
  (setq-local comment-end "")
  (setq-local font-lock-defaults '(orna-font-lock-keywords nil t)))
(add-to-list 'auto-mode-alist '("\\\\.orna\\\\'" . orna-mode))
(defun orna-setup-eglot ()
  "Register Orna buffers with the orna-lsp language server."
  (add-to-list 'eglot-server-programs (cons '(orna-mode) '("orna-lsp"))))
(provide 'orna-eglot)
