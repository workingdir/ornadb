" Generated from orna-syntax-v1 language metadata.
if !exists('g:orna_lsp_command')
    let g:orna_lsp_command = ['orna-lsp']
endif
function! s:on_lsp_buffer_enabled() abort
    if &l:filetype ==# 'orna'
        setlocal omnifunc=lsp#complete
    endif
endfunction
augroup orna_lsp
    au!
    au User lsp_setup call lsp#register_server({
        \ 'name': 'orna',
        \ 'cmd': {server_info -> copy(g:orna_lsp_command)},
        \ 'allowlist': ['orna'],
        \ })
    au User lsp_buffer_enabled call <SID>on_lsp_buffer_enabled()
augroup END
