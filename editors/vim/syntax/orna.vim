" Generated from orna-syntax-v1; edit crates/orna-syntax-v1/src/editor.rs instead.
if exists("b:current_syntax") | finish | endif
syntax case match
syntax match ornaIdentifier /\v<%([_a-zA-Z]|[^\x00-\x7f])%([_a-zA-Z0-9]|[^\x00-\x7f])*/
syntax keyword ornaKeyword as assert base break case continue dim else enum false fn for if impl in let loop null offset affine protocol pub return self static table true type unit use while
syntax match ornaPunctuation /\v%([{]|[}]|[(]|[)]|[\[]|[\]]|[,]|[;]|[:]|[.])/
syntax match ornaOperator #\v%([.][.][=]|[=][>]|[=][=]|[!][=]|[<][=]|[>][=]|[?][?]|[|][?]|[&][&]|[|][|]|[+][=]|[\-][=]|[*][=]|[/][=]|[.][.]|[|]|[!]|[=]|[<]|[>]|[+]|[\-]|[*]|[/]|[%]|[\^]|[?])#
syntax match ornaNumber /\v<%([0-9]{4}[\-][0-9]{2}[\-][0-9]{2}T[0-9]{2}[:][0-9]{2}[:][0-9]{2}%([.][0-9][0-9]*)?%(Z|[+\-][0-9]{2}[:][0-9]{2})|[0-9]{4}[\-][0-9]{2}[\-][0-9]{2}|0x[0-9A-Fa-f]%(_?[0-9A-Fa-f])*|0b[01]%(_?[01])*|[0-9]%(_?[0-9])*%([.][0-9]%(_?[0-9])*)?%([Ee][+\-]?[0-9]%(_?[0-9])*)?f?)>/
syntax match ornaContextualName /\v%([.])@<![.]%(continue|protocol|assert|offset|affine|return|static|break|false|table|while|base|case|else|enum|impl|loop|null|self|true|type|unit|dim|for|let|pub|use|as|fn|if|in)>/ contains=ornaPunctuation
syntax match ornaContextualName /\v%(^\s+|[(,{]\s*)%(continue|protocol|assert|offset|affine|return|static|break|table|while|base|case|else|enum|impl|loop|self|type|unit|dim|for|let|pub|use|as|fn|if|in)>\ze\s*:/ contains=ornaPunctuation
syntax region ornaString start=+"+ skip=+\\.+ end=+"+ contains=ornaEscape,ornaInterpolation
syntax match ornaEscape /\v[\\]%(["0\\nrt]|u[{][0-9A-Fa-f]{1,6}[}])/ contained
syntax region ornaInterpolation matchgroup=ornaPunctuation start=+{+ end=+}+ contained contains=@ornaExpression
syntax region ornaBraces matchgroup=ornaPunctuation start=+{+ end=+}+ contained contains=@ornaExpression
syntax cluster ornaExpression contains=ornaIdentifier,ornaKeyword,ornaPunctuation,ornaOperator,ornaNumber,ornaContextualName,ornaString,ornaBraces,ornaLineComment,ornaBlockComment
syntax match ornaLineComment +\v[/][/].*$+
syntax region ornaBlockComment start=+\v[/][*]+ end=+\v[*][/]+ contains=ornaBlockComment
hi def link ornaKeyword Statement
hi def link ornaString String
hi def link ornaEscape SpecialChar
hi def link ornaLineComment Comment
hi def link ornaBlockComment Comment
hi def link ornaNumber Number
hi def link ornaOperator Operator
hi def link ornaPunctuation Delimiter
hi def link ornaIdentifier Identifier
hi def link ornaContextualName Identifier
let b:current_syntax = "orna"
