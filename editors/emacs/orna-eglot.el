;;; Orna 1.0 syntax-v1 support -*- lexical-binding: t; -*-
;; Generated from orna-syntax-v1.

(defvar orna-v1-keywords '("as" "assert" "base" "break" "case" "continue" "dim" "else" "enum" "false" "fn" "for" "if" "impl" "in" "let" "loop" "null" "offset" "affine" "protocol" "pub" "return" "self" "static" "table" "true" "type" "unit" "use" "while"))
(defconst orna-v1-font-lock-keywords
  `((,(regexp-opt orna-v1-keywords 'words) . font-lock-keyword-face)
    ("//.*$" . font-lock-comment-face)
    ("/\\*\\(?:.\\|\\n\\)*?\\*/" . font-lock-comment-face)
    ("\\\"\\(?:\\\\.\\|[^\\\"\\\\]\\)*\\\"" . font-lock-string-face)
    ("[0-9]+\\(?:\\.[0-9]+\\)?" . font-lock-constant-face)))
(define-derived-mode orna-v1-mode prog-mode "Orna"
  "Major mode for Orna 1.0 source."
  (setq-local comment-start "// ")
  (setq-local comment-end "")
  (setq-local font-lock-defaults '(orna-v1-font-lock-keywords)))
(add-to-list 'auto-mode-alist '("\\.orna" . orna-v1-mode))
(provide 'orna-eglot)
;;; orna-eglot.el ends here
