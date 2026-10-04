-- Generated from orna-syntax-v1 language metadata.
local M = {}

function M.setup(options)
  options = options or {}
  local command = options.cmd or { "orna-lsp" }
  vim.filetype.add({ extension = { orna = "orna" } })
  local group = vim.api.nvim_create_augroup("orna_lsp", { clear = true })

  vim.api.nvim_create_autocmd("FileType", {
    group = group,
    pattern = "orna",
    callback = function(event)
      local root_dir = options.root_dir
      if type(root_dir) == "function" then
        root_dir = root_dir(event.buf)
      end
      vim.lsp.start({
        name = "orna",
        cmd = command,
        root_dir = root_dir or vim.fn.getcwd(),
      }, { bufnr = event.buf })
    end,
  })
end

return M
