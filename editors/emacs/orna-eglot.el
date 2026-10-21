;;; orna-eglot.el --- Orna 1.0.0 lexical highlighting -*- lexical-binding: t; -*-
;; Generated from orna-syntax-v1; edit crates/orna-syntax-v1/src/editor.rs instead.
(require 'eglot)
(defvar orna-eglot-server-command '("orna-lsp")
  "Command used by Eglot to start the Orna language server.")
(defvar orna-punctuation-face
  (if (facep 'font-lock-delimiter-face) 'font-lock-delimiter-face 'default)
  "Face for Orna punctuation; `font-lock-delimiter-face' needs Emacs 29.")
(defvar orna-mode-syntax-table
  (let ((table (make-syntax-table)))
    (modify-syntax-entry ?/ ". 124b" table)
    (modify-syntax-entry ?* ". 23n" table)
    (modify-syntax-entry ?\n "> b" table)
    (modify-syntax-entry ?\" "\"" table)
    (modify-syntax-entry ?\\ "\\" table)
    (modify-syntax-entry ?_ "w" table)
    (dolist (char '(?+ ?- ?= ?< ?> ?! ?? ?| ?& ?% ?^ ?. ?, ?\; ?:))
      (modify-syntax-entry char "." table))
    table)
  "Syntax table for `orna-mode': line and nesting block comments, strings.")
(defconst orna--nested-quote-syntax (string-to-syntax ".")
  "Syntax of a quote that delimits a string nested in an interpolation.")
(defvar orna--mark-nested-quotes nil
  "Non-nil while `orna-syntax-propertize' scans strings.")
(defun orna--skip-block-comment ()
  "Move past the nesting block comment that starts at point."
  (forward-char 2)
  (let ((depth 1))
    (while (and (> depth 0) (not (eobp)))
      (cond ((looking-at "/\\*") (setq depth (1+ depth)) (forward-char 2))
            ((looking-at "\\*/") (setq depth (1- depth)) (forward-char 2))
            (t (forward-char 1))))))
(defun orna--mark-nested-quote ()
  "Give the quote at point punctuation syntax while propertizing."
  (when orna--mark-nested-quotes
    (put-text-property (point) (1+ (point)) 'syntax-table orna--nested-quote-syntax)))
(defun orna--scan-string (nested)
  "Move past the string whose opening quote is at point.
NESTED means the string sits inside an interpolation."
  (when nested (orna--mark-nested-quote))
  (forward-char 1)
  (catch 'done
    (while (not (eobp))
      (let ((char (char-after)))
        (cond ((looking-at "\\\\u{")
               (skip-chars-forward "^}")
               (unless (eobp) (forward-char 1)))
              ((eq char ?\\) (forward-char (min 2 (- (point-max) (point)))))
              ((eq char ?\")
               (when nested (orna--mark-nested-quote))
               (forward-char 1)
               (throw 'done t))
              ((eq char ?{) (forward-char 1) (orna--scan-interpolation))
              (t (forward-char 1)))))))
(defun orna--scan-interpolation ()
  "Move past the interpolation code at point and its closing brace."
  (let ((depth 0))
    (catch 'done
      (while (not (eobp))
        (let ((char (char-after)))
          (cond ((eq char ?\") (orna--scan-string t))
                ((looking-at "//") (forward-line 1))
                ((looking-at "/\\*") (orna--skip-block-comment))
                ((eq char ?{) (setq depth (1+ depth)) (forward-char 1))
                ((eq char ?})
                 (forward-char 1)
                 (if (= depth 0) (throw 'done t) (setq depth (1- depth))))
                (t (forward-char 1))))))))
(defun orna-syntax-propertize (start end)
  "Keep quotes of strings nested in interpolations between START and END
from ending the enclosing string."
  (goto-char start)
  (let ((state (syntax-ppss)))
    (when (nth 3 state) (goto-char (nth 8 state))))
  (let ((orna--mark-nested-quotes t))
    (while (re-search-forward "\"" end t)
      (let ((open (match-beginning 0)))
        (unless (nth 8 (save-excursion (syntax-ppss open)))
          (goto-char open)
          (orna--scan-string nil))))))
(defvar orna--interpolation-start nil
  "Start of the interpolation code `orna--match-interpolation' found.")
(defvar orna--interpolation-end nil
  "End of the interpolation code `orna--match-interpolation' found.")
(defun orna--escaped-brace-p (open)
  "Non-nil when the brace at OPEN belongs to an escape such as `\\u{7b}'."
  (save-excursion
    (goto-char open)
    (when (eq (char-before) ?u) (backward-char))
    (= 1 (% (- (skip-chars-backward "\\\\")) 2))))
(defun orna--match-interpolation (limit)
  "Match the opening brace of the next string interpolation before LIMIT."
  (let (found)
    (while (and (not found) (re-search-forward "{" limit t))
      (let ((open (match-beginning 0)))
        (when (and (nth 3 (save-excursion (syntax-ppss open)))
                   (not (orna--escaped-brace-p open)))
          (save-excursion
            (goto-char (1+ open))
            (orna--scan-interpolation)
            (setq orna--interpolation-start (1+ open)
                  orna--interpolation-end (point)))
          (set-match-data (list open (1+ open)))
          (setq found t))))
    found))
(defvar orna-font-lock-keywords
  `(
    ("\\(?:^\\|[^.]\\)\\.\\(\\(?:continue\\|protocol\\|assert\\|offset\\|affine\\|return\\|static\\|break\\|false\\|table\\|while\\|base\\|case\\|else\\|enum\\|impl\\|loop\\|null\\|self\\|true\\|type\\|unit\\|dim\\|for\\|let\\|pub\\|use\\|as\\|fn\\|if\\|in\\)\\)\\_>" 1 'font-lock-variable-name-face)
    ("\\_<\\(\\(?:continue\\|protocol\\|assert\\|offset\\|affine\\|return\\|static\\|break\\|table\\|while\\|base\\|case\\|else\\|enum\\|impl\\|loop\\|self\\|type\\|unit\\|dim\\|for\\|let\\|pub\\|use\\|as\\|fn\\|if\\|in\\)\\)[[:blank:]]*:" 1 'font-lock-variable-name-face)
    (,(regexp-opt '("as" "assert" "base" "break" "case" "continue" "dim" "else" "enum" "false" "fn" "for" "if" "impl" "in" "let" "loop" "null" "offset" "affine" "protocol" "pub" "return" "self" "static" "table" "true" "type" "unit" "use" "while") 'words) . 'font-lock-keyword-face)
    ("\\_<\\(?:[0-9]\\{4\\}-[0-9]\\{2\\}-[0-9]\\{2\\}T[0-9]\\{2\\}:[0-9]\\{2\\}:[0-9]\\{2\\}\\(?:\\.[0-9][0-9]*\\)?\\(?:Z\\|[+-][0-9]\\{2\\}:[0-9]\\{2\\}\\)\\|[0-9]\\{4\\}-[0-9]\\{2\\}-[0-9]\\{2\\}\\|0x[0-9A-Fa-f]\\(?:_?[0-9A-Fa-f]\\)*\\|0b[01]\\(?:_?[01]\\)*\\|[0-9]\\(?:_?[0-9]\\)*\\(?:\\.[0-9]\\(?:_?[0-9]\\)*\\)?\\(?:[Ee][+-]?[0-9]\\(?:_?[0-9]\\)*\\)?f?\\)\\_>" . 'font-lock-constant-face)
    ("\\_<[_[:alpha:]][_[:alnum:][:nonascii:]]*" . 'font-lock-variable-name-face)
    ("\\(?:\\.\\.=\\|=>\\|==\\|!=\\|<=\\|>=\\|\\?\\?\\||\\?\\|&&\\|||\\|\\+=\\|-=\\|\\*=\\|/=\\|\\.\\.\\||\\|!\\|=\\|<\\|>\\|\\+\\|-\\|\\*\\|/\\|%\\|\\^\\|\\?\\)" . 'font-lock-builtin-face)
    ("\\(?:{\\|}\\|(\\|)\\|\\[\\|]\\|,\\|;\\|:\\|\\.\\)" . orna-punctuation-face)
    (orna--match-interpolation
     (0 orna-punctuation-face t)
     ("\\_<[_[:alpha:]][_[:alnum:][:nonascii:]]*" orna--interpolation-end (goto-char orna--interpolation-start) (0 'font-lock-variable-name-face t))
     ("\\_<\\(?:[0-9]\\{4\\}-[0-9]\\{2\\}-[0-9]\\{2\\}T[0-9]\\{2\\}:[0-9]\\{2\\}:[0-9]\\{2\\}\\(?:\\.[0-9][0-9]*\\)?\\(?:Z\\|[+-][0-9]\\{2\\}:[0-9]\\{2\\}\\)\\|[0-9]\\{4\\}-[0-9]\\{2\\}-[0-9]\\{2\\}\\|0x[0-9A-Fa-f]\\(?:_?[0-9A-Fa-f]\\)*\\|0b[01]\\(?:_?[01]\\)*\\|[0-9]\\(?:_?[0-9]\\)*\\(?:\\.[0-9]\\(?:_?[0-9]\\)*\\)?\\(?:[Ee][+-]?[0-9]\\(?:_?[0-9]\\)*\\)?f?\\)\\_>" orna--interpolation-end (goto-char orna--interpolation-start) (0 'font-lock-constant-face t))
     (,(regexp-opt '("as" "assert" "base" "break" "case" "continue" "dim" "else" "enum" "false" "fn" "for" "if" "impl" "in" "let" "loop" "null" "offset" "affine" "protocol" "pub" "return" "self" "static" "table" "true" "type" "unit" "use" "while") 'words) orna--interpolation-end (goto-char orna--interpolation-start) (0 'font-lock-keyword-face t))
     ("\\(?:^\\|[^.]\\)\\.\\(\\(?:continue\\|protocol\\|assert\\|offset\\|affine\\|return\\|static\\|break\\|false\\|table\\|while\\|base\\|case\\|else\\|enum\\|impl\\|loop\\|null\\|self\\|true\\|type\\|unit\\|dim\\|for\\|let\\|pub\\|use\\|as\\|fn\\|if\\|in\\)\\)\\_>" orna--interpolation-end (goto-char orna--interpolation-start) (1 'font-lock-variable-name-face t))
     ("\\_<\\(\\(?:continue\\|protocol\\|assert\\|offset\\|affine\\|return\\|static\\|break\\|table\\|while\\|base\\|case\\|else\\|enum\\|impl\\|loop\\|self\\|type\\|unit\\|dim\\|for\\|let\\|pub\\|use\\|as\\|fn\\|if\\|in\\)\\)[[:blank:]]*:" orna--interpolation-end (goto-char orna--interpolation-start) (1 'font-lock-variable-name-face t))
     ("\\(?:\\.\\.=\\|=>\\|==\\|!=\\|<=\\|>=\\|\\?\\?\\||\\?\\|&&\\|||\\|\\+=\\|-=\\|\\*=\\|/=\\|\\.\\.\\||\\|!\\|=\\|<\\|>\\|\\+\\|-\\|\\*\\|/\\|%\\|\\^\\|\\?\\)" orna--interpolation-end (goto-char orna--interpolation-start) (0 'font-lock-builtin-face t))
     ("\\(?:{\\|}\\|(\\|)\\|\\[\\|]\\|,\\|;\\|:\\|\\.\\)" orna--interpolation-end (goto-char orna--interpolation-start) (0 orna-punctuation-face t))
     ("\"\\(?:\\\\.\\|[^\"\\\\]\\)*\"" orna--interpolation-end (goto-char orna--interpolation-start) (0 'font-lock-string-face t)))
  )
  "Lexical highlighting generated from the v1 lexer.")
(define-derived-mode orna-mode prog-mode "Orna"
  "Major mode for Orna source files."
  (setq-local comment-start "// ")
  (setq-local comment-end "")
  (setq-local syntax-propertize-function #'orna-syntax-propertize)
  (setq-local font-lock-defaults '(orna-font-lock-keywords nil nil)))
(add-to-list 'auto-mode-alist '("\\.orna\\'" . orna-mode))
(defun orna-setup-eglot ()
  "Attach Orna buffers to the configured language server with Eglot."
  (add-to-list 'eglot-server-programs
               (cons '(orna-mode) orna-eglot-server-command))
  (add-hook 'orna-mode-hook #'eglot-ensure))
(provide 'orna-eglot)
