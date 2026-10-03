" Generated from orna-syntax-v1.
if exists("b:current_syntax") | finish | endif
syntax keyword ornaKeyword as assert base break case continue dim else enum false fn for if impl in let loop null offset affine protocol pub return self static table true type unit use while
syntax match ornaComment +//.*$+
syntax region ornaBlockComment start=+/\*+ end=+\*/+ fold contains=ornaBlockComment
syntax region ornaString start=+"+ skip=+\\.+ end=+"+
syntax match ornaNumber +\<\d\+\(\.\d\+\)\=\>+
syntax match ornaIdentifier +\<[[:alpha:]_][[:alnum:]_]*\>+
highlight default link ornaKeyword Keyword
highlight default link ornaComment Comment
highlight default link ornaBlockComment Comment
highlight default link ornaString String
highlight default link ornaNumber Number
highlight default link ornaIdentifier Identifier
let b:current_syntax = "orna"
