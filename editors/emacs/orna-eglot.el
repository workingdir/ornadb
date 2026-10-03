;;; orna-eglot.el --- Orna 1.0.0 lexical highlighting -*- lexical-binding: t; -*-
;; Generated from orna-syntax-v1.
(require 'eglot)
(defvar orna-font-lock-keywords
  `(
    (,(regexp-opt '("as" "assert" "base" "break" "case" "continue" "dim" "else" "enum" "false" "fn" "for" "if" "impl" "in" "let" "loop" "null" "offset" "affine" "protocol" "pub" "return" "self" "static" "table" "true" "type" "unit" "use" "while") 'words) . font-lock-keyword-face)
    ("[_[:alpha:]][_[:alnum:]]*" . font-lock-variable-name-face)
    ("\\(?:[0-9]\\{4\\}-[0-9]\\{2\\}-[0-9]\\{2\\}T[0-9]\\{2\\}:[0-9]\\{2\\}:[0-9]\\{2\\}\\(?:\\.[0-9]*\\)\\?\\(?:Z\\|[+-][0-9]\\{2\\}:[0-9]\\{2\\}\\)\\|[0-9]\\{4\\}-[0-9]\\{2\\}-[0-9]\\{2\\}\\|0x[0-9A-Fa-f_]*\\|0b[01_]*\\|[0-9][0-9_]*\\(?:\\.[0-9_]\\+\\)\\?\\(?:[eE][+-]\\?[0-9_]*\\)\\?f\\?\\)" . font-lock-constant-face)
    ("\"\\(?:\\\\.\\|[^\"\\\\]\\)*\"" . font-lock-string-face)
    ("//.*$" . font-lock-comment-face)
    ("/\\*\\(?:.\\|\\n\\)*\\?\\*/" . font-lock-comment-face)
    (,(regexp-opt '("..=" "=>" "==" "!=" "<=" ">=" "??" "|?" "&&" "||" "+=" "-=" "*=" "/=" ".." "|" "!" "=" "<" ">" "+" "-" "*" "/" "%" "^" "?")) . font-lock-builtin-face)
    (,(regexp-opt '("{" "}" "(" ")" "[" "]" "," ";" ":" ".")) . font-lock-delimiter-face)
  )
  "Lexical highlighting generated from the v1 lexer.")
(define-derived-mode orna-mode prog-mode "Orna"
  "Major mode for Orna source files."
  (setq-local comment-start "// ")
  (setq-local comment-end "")
  (setq-local font-lock-defaults '(orna-font-lock-keywords nil nil)))
(add-to-list 'auto-mode-alist '("\\.orna\\'" . orna-mode))
(defun orna-setup-eglot ()
  "Register Orna buffers with the orna-lsp language server."
  (add-to-list 'eglot-server-programs (cons '(orna-mode) '("orna-lsp"))))
(provide 'orna-eglot)
