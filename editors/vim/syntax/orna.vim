" Vim syntax file
" Language: Orna
" Maintainer: OrnaDB
" Latest Revision: 2026-08-16

if exists("b:current_syntax")
    finish
endif

" Keywords and type names are case-insensitive.
syntax case ignore

" Keep keyword declarations and explicit identifier boundaries on the same
" syntax-local option. Vim documents \k/\K as option-backed keyword atoms;
" its Unicode word classification is broader than Rust Alphabetic/Numeric
" (notably, it can include emoji), so this is a bounded fallback rather than
" exact Unicode-category parity.
syntax iskeyword @,48-57,_

" Statements, declarations, control flow and SQL keywords.
syntax keyword ornaStatement ADD ALL ALTER AND AS ASC ATOMIC AWAIT BEGIN BETWEEN BY
            \ CALL CAPABILITY CASCADE CASE CHECK CLIENT CONST CONTRACT CREATE CROSS
            \ DEFAULT DEFINER DELETE DESC DISABLED DISTINCT DOCUMENTATION DROP ELSE
            \ ELSIF END ENUM EXECUTE EXISTS EXPORT EXTERNAL FIELD FINAL FIRST FOR FROM
            \ FULL FUNCTION GRANT GROUP HAVING IF ILIKE IMMUTABLE IN INNER INSERT
            \ INSPECT INTO INVOKER IS JOIN KERNEL LAST LEFT LET LIKE LIMIT LIST LOCAL
            \ LOOP MANUAL MAP NOT NULLS OBJECT OFFSET ON ONLY OPAQUE OPTION OR ORDER
            \ OUTER PERSISTABLE PRELUDE PRIMITIVE READ REF RENAME REQUIRES RESTRICT RETURN
            \ RETURNING RETURNS REVOKE RIGHT ROLE ROWS RUNTIME SCHEMA SCOPE SEALED
            \ SECURITY SELECT SERVER SESSION SET STABLE STATE STREAM TABLE THEN TO
            \ TRANSACTION TRANSIENT TYPE UNION UNIQUE UPDATE USER VALUE VALUES
            \ VOLATILE VOLATILITY WHEN WHERE WHILE

" Boolean and null literals.
syntax keyword ornaBoolean FALSE NULL TRUE

" Standard scalar types.
syntax keyword ornaType BIGINT BOOL BOOLEAN BYTES DATE DECIMAL DURATION FLOAT INT INTEGER TEXT
            \ TIME TIMESTAMP UUID VOID
syntax match ornaType "\%#=2\k\@<!\%(BINARY LARGE OBJECT\|CHARACTER LARGE OBJECT\)\k\@!"

" Strings: '...' with '' as the escape for a literal quote.
syntax region ornaString start=+'+ skip=+''+ end=+'+

" Quoted identifiers: "..." with "" as the escape.
syntax region ornaQuotedIdentifier start=+"+ skip=+""+ end=+"+

" Comments: -- line comments and /* ... */ block comments.
syntax match ornaComment "--.*$" contains=@Spell
syntax region ornaComment start=+/\*+ end=+\*/+ contains=@Spell

" Numbers: integers, decimals and exponents.
syntax match ornaNumber "\%#=2\k\@<![0-9]\+\(\.[0-9]\+\)\?\([eE][+-]\?[0-9]\+\)\?\k\@!"

" Operators.
syntax match ornaOperator "[-+*/%=<>!]"
syntax match ornaOperator "::"
syntax match ornaOperator "||"

" Function calls: identifier immediately followed by (.
syntax match ornaFunction "\%#=2\k\@<!\K\k*\ze\s*("

" Default identifiers (keywords take precedence over this match). The
" explicit \k lookarounds avoid buffer-local word-boundary settings.
syntax match ornaIdentifier "\%#=2\k\@<!\K\k*\k\@!"

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
