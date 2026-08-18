" Vim syntax file
" Language: Orna
" Maintainer: OrnaDB
" Latest Revision: 2026-08-16

if exists("b:current_syntax")
    finish
endif

" Keywords and type names are case-insensitive.
syntax case ignore

" Statements, declarations, control flow and SQL keywords.
syntax keyword ornaStatement ADD ALL ALTER AND AS ASC ATOMIC AWAIT BEGIN BETWEEN BY
            \ CALL CAPABILITY CASCADE CASE CHECK CLIENT CONST CONTRACT CREATE CROSS
            \ DEFAULT DEFINER DELETE DESC DISABLED DISTINCT DOCUMENTATION DROP ELSE
            \ ELSIF END ENUM EXECUTE EXISTS EXPORT EXTERNAL FIELD FINAL FIRST FOR FROM
            \ FULL FUNCTION GRANT GROUP HAVING IF ILIKE IMMUTABLE IN INNER INSERT
            \ INSPECT INTO INVOKER IS JOIN KERNEL LAST LEFT LET LIKE LIMIT LIST LOCAL
            \ LOOP MANUAL MAP NOT NULLS OBJECT OFFSET ON ONLY OPAQUE OPTION OR ORDER
            \ OUTER PERSISTABLE PRIMITIVE READ REF RENAME REQUIRES RESTRICT RETURN
            \ RETURNING RETURNS REVOKE RIGHT ROLE ROWS RUNTIME SCHEMA SCOPE SEALED
            \ SECURITY SELECT SERVER SESSION SET STABLE STATE STREAM TABLE THEN TO
            \ TRANSACTION TRANSIENT TYPE UNION UNIQUE UPDATE USER VALUE VALUES
            \ VOLATILE VOLATILITY WHEN WHERE WHILE

" Boolean and null literals.
syntax keyword ornaBoolean FALSE NULL TRUE

" Standard scalar types.
syntax keyword ornaType BIGINT BOOL BYTES DATE DECIMAL DURATION FLOAT INT TEXT
            \ TIME TIMESTAMP UUID VOID

" Strings: '...' with '' as the escape for a literal quote.
syntax region ornaString start=+'+ skip=+''+ end=+'+

" Quoted identifiers: "..." with "" as the escape.
syntax region ornaQuotedIdentifier start=+"+ skip=+""+ end=+"+

" Comments: -- line comments and /* ... */ block comments.
syntax match ornaComment "--.*$" contains=@Spell
syntax region ornaComment start=+/\*+ end=+\*/+ contains=@Spell

" Numbers: integers, decimals and exponents.
syntax match ornaNumber "\<\d\+\(\.\d\+\)\?\([eE][+-]\?\d\+\)\?\>"

" Operators.
syntax match ornaOperator "[-+*/%=<>!]"
syntax match ornaOperator "::"
syntax match ornaOperator "||"

" Function calls: identifier immediately followed by (.
syntax match ornaFunction "\<[A-Za-z_][A-Za-z0-9_]*\(\s*(\)"me=e-1

" Default identifiers (keywords take precedence over this match).
syntax match ornaIdentifier "\<[A-Za-z_][A-Za-z0-9_]*\>"

hi def link ornaStatement Statement
hi def link ornaBoolean Boolean
hi def link ornaType Type
hi def link ornaString String
hi def link ornaQuotedIdentifier Identifier
hi def link ornaComment Comment
hi def link ornaNumber Number
hi def link ornaOperator Operator
hi def link ornaFunction Function
hi def link ornaIdentifier Identifier

let b:current_syntax = "orna"
