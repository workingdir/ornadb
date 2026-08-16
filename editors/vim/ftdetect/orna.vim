" ftdetect/orna.vim
" Detect the Orna language by file extension.

augroup orna_filetype
  au!
  au BufRead,BufNewFile *.orna setfiletype orna
augroup END
