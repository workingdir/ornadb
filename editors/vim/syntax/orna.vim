" Vim syntax file
" Language: Orna
" Generated from orna-syntax grammar metadata.

if exists("b:current_syntax")
    finish
endif

syntax case ignore
syntax iskeyword @,48-57,_

syntax keyword ornaKeyword ADD ALL ALTER AND AS ASC ATOMIC AWAIT
            \ BEGIN BETWEEN BY CALL CAPABILITY CASCADE CASE CHECK
            \ CLIENT CONST CONTRACT CREATE CROSS DEFAULT DEFINER DELETE
            \ DESC DISABLED DISTINCT DOCUMENTATION DROP ELSE ELSIF END
            \ ENUM EXECUTE EXISTS EXPORT EXTERNAL FALSE FIELD FINAL
            \ FIRST FOR FROM FULL FUNCTION GRANT GROUP HAVING
            \ IF ILIKE IMMUTABLE IN INNER INSERT INSPECT INTO
            \ INVOKER IS JOIN KERNEL LAST LEFT LET LIKE
            \ LIMIT LIST LOCAL LOOP MANUAL MAP NOT NULL
            \ NULLS OBJECT OFFSET ON ONLY OPAQUE OPTION OR
            \ ORDER OUTER PERSISTABLE PRELUDE PRIMITIVE READ REF RENAME
            \ REQUIRES RESTRICT RETURN RETURNING RETURNS REVOKE RIGHT ROLE
            \ ROWS RUNTIME SCHEMA SCOPE SEALED SECURITY SELECT SERVER
            \ SESSION SET STABLE STATE STREAM TABLE THEN TO
            \ TRANSACTION TRANSIENT TRUE TYPE UNION UNIQUE UPDATE USER
            \ VALUE VALUES VOLATILE VOLATILITY WHEN WHERE WHILE
syntax keyword ornaType BIGINT BOOL BOOLEAN BYTES DATE DECIMAL DURATION FLOAT
            \ INT INTEGER TEXT TIME TIMESTAMP UUID VOID
syntax match ornaType /\c\<BINARY\s\+LARGE\s\+OBJECT\>/
syntax match ornaType /\c\<CHARACTER\s\+LARGE\s\+OBJECT\>/
syntax region ornaString start=+'+ skip=+''+ end=+'+
syntax region ornaQuotedIdentifier start=+"+ skip=+""+ end=+"+
syntax match ornaComment "--.*$" contains=@Spell
syntax region ornaComment start=+/\*+ end=+\*/+ contains=@Spell
syntax match ornaNumber "\<[0-9]\+\(\.[0-9]\+\)\?\>"
syntax match ornaOperator /\%(:=\|=>\|<>\|!=\|<=\|>=\|||\|->\|=\|<\|>\|+\|-\|\*\|\/\|%\|:\|?\)/
syntax match ornaPunctuation /\%((\|)\|,\|;\|\.\|\[\|\]\|{\|}\)/
syntax match ornaFunction /\<\k\+\ze\s*(/
syntax match ornaIdentifier /\<\k\+\>/

hi def link ornaKeyword Statement
hi def link ornaType Type
hi def link ornaString String
hi def link ornaQuotedIdentifier Identifier
hi def link ornaComment Comment
hi def link ornaNumber Number
hi def link ornaOperator Operator
hi def link ornaPunctuation Delimiter
hi def link ornaFunction Function
hi def link ornaIdentifier Identifier

let b:current_syntax = "orna"
