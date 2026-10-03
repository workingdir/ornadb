" Generated from orna-syntax-v1.
if exists("b:current_syntax") | finish | endif
syntax case match
syntax keyword ornaKeyword as assert base break case continue dim else enum false fn for if impl in let loop null offset affine protocol pub return self static table true type unit use while
syntax region ornaString start=+"+ skip=+\\.+ end=+"+ contains=ornaInterpolation
syntax region ornaInterpolation start=+\\{+ end=+}+ contained
syntax match ornaComment +//.*$+
syntax region ornaComment start=+/\*+ end=+\*/+ contains=ornaComment
syntax match ornaNumber +\<[0-9][A-Za-z0-9_:.+-]*+
syntax match ornaOperator +\(\.\.=\|=>\|==\|!=\|<=\|>=\|??\||?\|&&\|||\|+=\|-=\|\*=\|/=\|\.\.\||\|!\|=\|<\|>\|+\|-\|\*\|/\|%\|\^\|?\)+
syntax match ornaPunctuation +\({\|}\|(\|)\|\[\|\]\|,\|;\|:\|\.\)+
syntax match ornaIdentifier +[_[:alpha:]][_[:alnum:]]*+
hi def link ornaKeyword Statement
hi def link ornaString String
hi def link ornaComment Comment
hi def link ornaNumber Number
hi def link ornaOperator Operator
hi def link ornaPunctuation Delimiter
hi def link ornaIdentifier Identifier
let b:current_syntax = "orna"
