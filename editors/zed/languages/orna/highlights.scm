; Orna syntax highlighting for Zed.
;
; These rules reference the standard capture set produced by the vendored
; tree-sitter-orna grammar (editors/tree-sitter-orna): @keyword, @type,
; @function, @variable, @property, @string, @number, @comment, @operator,
; @punctuation, @namespace, @parameter. Node names below are the
; conventional ones for a SQL-like grammar; adjust a rule if its node never
; matches the actual grammar output.

(comment) @comment
(string) @string
(number) @number
(keyword) @keyword
(identifier) @variable
