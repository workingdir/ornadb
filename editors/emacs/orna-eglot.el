;;; orna-eglot.el --- Orna language support for Emacs via Eglot -*- lexical-binding: t; -*-

;; Author: OrnaDB
;; Keywords: languages
;; Version: 1.0.0
;; Package-Requires: ((emacs "28.1") (eglot "1.0"))

;;; Commentary:
;;
;; A small package that registers the Orna language (.orna files) with
;; Emacs:
;;
;;   - `orna-mode': a major mode with syntax highlighting (keywords, scalar
;;     types, comments, strings, numbers, operators) and comment support.
;;   - Eglot integration: `orna-setup-eglot' registers orna-mode buffers
;;     with the orna-lsp language server.
;;
;; The language server binary is `orna-lsp'.  The tree-sitter grammar used
;; by the LSP ecosystem lives in the sibling directory
;; editors/tree-sitter-orna.
;;
;;   (require 'orna-eglot)
;;   (orna-setup-eglot)                 ; register orna-lsp with Eglot
;;   (add-hook 'orna-mode-hook #'eglot-ensure)

;;; Code:

(require 'eglot)

(defgroup orna nil
    "Orna language support."
    :group 'languages
    :prefix "orna-")

(defcustom orna-lsp-command "orna-lsp"
    "Command used to start the Orna language server."
    :type 'string
    :group 'orna)

(defface orna-operator-face
    '((t :inherit font-lock-keyword-face))
    "Face used for Orna operators."
    :group 'orna)

(defvar orna-mode-syntax-table
    (let ((table (make-syntax-table)))
	;; '...' strings and "..." quoted identifiers
	(modify-syntax-entry ?' "\"" table)
	(modify-syntax-entry ?\" "\"" table)
	table)
    "Syntax table for `orna-mode'.
Comments (-- and /* */) and doubled-quote escapes are handled by
`syntax-propertize-function' (see `orna--syntax-propertize').")

(defun orna--syntax-propertize (start end)
    "Propertize Orna comments and doubled-quote escapes in START..END.
Handles `--' line comments, `/* ... */' block comments, and the doubled
quote escapes (`''', `\"\"') that keep strings and quoted identifiers
open.  Guards every rule so a match inside an existing string or comment
never changes its meaning."
    (goto-char start)
    (while (and (< (point) end)
		(re-search-forward "\\(--\\)\\|\\(/\\*\\)\\|\\(\\*/\\)\\|\\(['\"]\\)\\(\\4\\)" end t))
	(cond
	 ;; -- line comment: comment-start on the marker, comment-end on the
	 ;; newline so the comment content between them stays comment.
	 ((match-beginning 1)
	  (let ((ppss (syntax-ppss (match-beginning 1))))
              (unless (or (nth 3 ppss) (nth 4 ppss))
		  (put-text-property (match-beginning 1) (match-end 1)
				     'syntax-table (string-to-syntax "<"))
		  (let ((eol (line-end-position)))
		      (when (< eol (point-max))
			  (put-text-property eol (1+ eol)
					     'syntax-table (string-to-syntax ">")))))))
	 ;; /* block comment opener
	 ((match-beginning 2)
	  (let ((ppss (syntax-ppss (match-beginning 2))))
              (unless (or (nth 3 ppss) (nth 4 ppss))
		  (put-text-property (match-beginning 2) (match-end 2)
				     'syntax-table (string-to-syntax "<")))))
	 ;; */ block comment closer (must apply inside a comment to close it;
	 ;; skip only inside strings, where */ is literal text)
	 ((match-beginning 3)
	  (let ((ppss (syntax-ppss (match-beginning 3))))
              (unless (nth 3 ppss)
		  (put-text-property (match-beginning 3) (match-end 3)
				     'syntax-table (string-to-syntax ">")))))
	 ;; '' / "" doubled quote inside a string: inert so the string stays open.
	 (t
	  (let ((ppss (syntax-ppss (match-beginning 4))))
              (unless (nth 4 ppss)
		  (put-text-property (match-beginning 4) (match-end 5)
				     'syntax-table (string-to-syntax "."))))))
	;; syntax-ppss above moves point to the queried position; always advance
	;; past the current match so the scan keeps moving forward.
	(goto-char (match-end 0))))

(defvar orna-keywords
    '("ADD" "ALL" "ALTER" "AND" "AS" "ASC" "ATOMIC" "AWAIT" "BEGIN"
      "BETWEEN" "BY" "CALL" "CAPABILITY" "CASCADE" "CASE" "CHECK" "CLIENT"
      "CONST" "CONTRACT" "CREATE" "CROSS" "DEFAULT" "DEFINER" "DELETE"
      "DESC" "DISABLED" "DISTINCT" "DOCUMENTATION" "DROP" "ELSE" "ELSIF"
      "END" "ENUM" "EXECUTE" "EXISTS" "EXPORT" "EXTERNAL" "FALSE" "FIELD"
      "FINAL" "FIRST" "FOR" "FROM" "FULL" "FUNCTION" "GRANT" "GROUP"
      "HAVING" "IF" "ILIKE" "IMMUTABLE" "IN" "INNER" "INSERT" "INSPECT"
      "INTO" "INVOKER" "IS" "JOIN" "KERNEL" "LAST" "LEFT" "LET" "LIKE"
      "LIMIT" "LIST" "LOCAL" "LOOP" "MANUAL" "MAP" "NOT" "NULL" "NULLS"
      "OBJECT" "OFFSET" "ON" "ONLY" "OPAQUE" "OPTION" "OR" "ORDER" "OUTER"
      "PERSISTABLE" "PRELUDE" "PRIMITIVE" "READ" "REF" "RENAME" "REQUIRES" "RESTRICT"
      "RETURN" "RETURNING" "RETURNS" "REVOKE" "RIGHT" "ROLE" "ROWS"
      "RUNTIME" "SCHEMA" "SCOPE" "SEALED" "SECURITY" "SELECT" "SERVER"
      "SESSION" "SET" "STABLE" "STATE" "STREAM" "TABLE" "THEN" "TO"
      "TRANSACTION" "TRANSIENT" "TRUE" "TYPE" "UNION" "UNIQUE" "UPDATE"
      "USER" "VALUE" "VALUES" "VOLATILE" "VOLATILITY" "WHEN" "WHERE"
      "WHILE")
    "Orna and SQL keywords (case-insensitive).")

(defvar orna-types
    '("BIGINT" "BINARY LARGE OBJECT" "BOOL" "BOOLEAN" "BYTES"
      "CHARACTER LARGE OBJECT" "DATE" "DECIMAL" "DURATION" "FLOAT"
      "INT" "INTEGER" "TEXT" "TIME" "TIMESTAMP" "UUID" "VOID")
    "Standard scalar type names (case-insensitive).")

(defvar orna-font-lock-keywords
    `((,(regexp-opt orna-keywords 'words) . font-lock-keyword-face)
      (,(regexp-opt orna-types 'words) . font-lock-type-face)
      ;; Numbers: keep so comment/string faces win inside those regions.
      ("\\(?:[0-9]+\\(?:\\.[0-9]+\\)?\\(?:[eE][+-]?[0-9]+\\)?\\)"
       (0 font-lock-constant-face keep))
      ;; Operators: keep preserves comment/string faces on -- and /* */.
      ("\\(?:[<>!=]=\\|[<>!=]\\|[-+*/%]\\|::\\|||\\)"
       (0 'orna-operator-face keep)))
    "Font lock keywords for `orna-mode'.")

;;;###autoload
(define-derived-mode orna-mode prog-mode "Orna"
    "Major mode for editing Orna source files."
    (setq-local comment-start "-- ")
    (setq-local comment-end "")
    (setq-local comment-start-skip "--[ \t]*")
    (setq-local font-lock-defaults '(orna-font-lock-keywords nil t))
    (setq-local syntax-propertize-function #'orna--syntax-propertize)
    (add-hook 'syntax-propertize-extend-region-functions
              #'syntax-propertize-wholelines nil t))

;;;###autoload
(add-to-list 'auto-mode-alist '("\\.orna\\'" . orna-mode))

;;;###autoload
(defun orna-setup-eglot ()
    "Register `orna-mode' with Eglot using `orna-lsp-command'."
    (add-to-list 'eglot-server-programs
		 (cons '(orna-mode) (list orna-lsp-command))))

(provide 'orna-eglot)

;;; orna-eglot.el ends here
