;;; emacs-faces.el --- Print orna-mode classes for a sample  -*- lexical-binding: t; -*-
;; Load editors/emacs/orna-eglot.el first, then this file:
;;   emacs --batch -Q -l orna-eglot.el -l emacs-faces.el SAMPLE
;; Prints one "LINE START END CLASS" row per character, with UTF-8 byte
;; columns counted from zero.

(defun orna-test--class (face)
  "Lexical class name for the FACE property of one character."
  (let ((faces (if (listp face) face (list face))))
    (cond ((or (memq 'font-lock-comment-face faces)
               (memq 'font-lock-comment-delimiter-face faces))
           "comment")
          ((memq 'font-lock-string-face faces) "string")
          ((memq 'font-lock-keyword-face faces) "keyword")
          ((memq 'font-lock-variable-name-face faces) "identifier")
          ((memq 'font-lock-constant-face faces) "number")
          ((memq 'font-lock-builtin-face faces) "operator")
          ((memq orna-punctuation-face faces) "punctuation")
          (t "none"))))

(let ((sample (pop command-line-args-left)))
  (with-temp-buffer
    (insert-file-contents sample)
    (orna-mode)
    (font-lock-ensure)
    (goto-char (point-min))
    (let ((line 0))
      (while (not (eobp))
        (let ((end (line-end-position))
              (column 0))
          (while (< (point) end)
            (let ((width (string-bytes (string (char-after)))))
              (princ (format "%d %d %d %s\n" line column (+ column width)
                             (orna-test--class (get-text-property (point) 'face))))
              (setq column (+ column width))
              (forward-char 1))))
        (forward-line 1)
        (setq line (1+ line))))))
