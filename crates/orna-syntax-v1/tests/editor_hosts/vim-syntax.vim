" Print the syntax class of every byte of $ORNA_SAMPLE highlighted by the
" generated $ORNA_VIM_SYNTAX into $ORNA_OUTPUT. Needs Vim with +eval and
" +syntax:
"   vim -Es -u NONE -i NONE -N -S vim-syntax.vim
" Each row is "LINE START END CLASS" with zero-based UTF-8 byte columns.
set nocompatible
syntax on
execute 'edit ' . fnameescape($ORNA_SAMPLE)
execute 'source ' . fnameescape($ORNA_VIM_SYNTAX)
let s:classes = {
      \ 'ornaKeyword': 'keyword',
      \ 'ornaIdentifier': 'identifier',
      \ 'ornaContextualName': 'identifier',
      \ 'ornaNumber': 'number',
      \ 'ornaString': 'string',
      \ 'ornaEscape': 'string',
      \ 'ornaLineComment': 'comment',
      \ 'ornaBlockComment': 'comment',
      \ 'ornaOperator': 'operator',
      \ 'ornaPunctuation': 'punctuation',
      \ }
let s:rows = []
for s:line in range(1, line('$'))
  for s:column in range(1, strlen(getline(s:line)))
    let s:name = synIDattr(synID(s:line, s:column, 1), 'name')
    call add(s:rows, printf('%d %d %d %s', s:line - 1, s:column - 1, s:column,
          \ get(s:classes, s:name, 'none')))
  endfor
endfor
call writefile(s:rows, $ORNA_OUTPUT)
qall!
