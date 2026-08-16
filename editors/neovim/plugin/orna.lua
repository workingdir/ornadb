-- plugin/orna.lua
-- Orna integration for Neovim: filetype detection and LSP setup.
--
-- Registers *.orna as the "orna" filetype and starts the native LSP
-- client for the orna-lsp language server. The tree-sitter grammar used
-- for syntax highlighting lives in the sibling directory
-- ../tree-sitter-orna/ (see README.md for installation options).
--
-- The plugin configures itself eagerly on load. To customise, set
-- vim.g.orna_skip_auto_setup = true before this plugin is sourced and call
-- require("orna").setup({ ... }) from your config instead.

local M = {}

---@class orna.Config
---@field cmd string[] Command used to launch the language server
---@field settings table|nil Initialization options passed to the server

---@type orna.Config
local defaults = {
    cmd = { "orna-lsp" },
    settings = nil,
}

-- Filetype detection: *.orna -> orna.
vim.filetype.add({ extension = { orna = "orna" } })

-- Project root: nearest directory containing a marker file. OrnaDB
-- workspaces are Cargo workspaces; fall back to the buffer directory.
---@param bufnr integer
---@return string
local function root_dir(bufnr)
    local root = vim.fs.root(bufnr, { ".git", "Cargo.toml" })
    if root then
        return root
    end
    return vim.fs.dirname(vim.api.nvim_buf_get_name(bufnr))
end

local function base_capabilities()
    return vim.lsp.protocol.make_client_capabilities()
end

-- Fallback for Neovim < 0.11: vim.lsp.config does not exist, so start the
-- client directly when an Orna buffer opens.
---@param bufnr integer
---@param config orna.Config
local function legacy_attach(bufnr, config)
    local client_id = vim.lsp.start_client({
        name = "orna-lsp",
        cmd = config.cmd,
        root_dir = root_dir(bufnr),
        capabilities = base_capabilities(),
        settings = config.settings,
    })
    if client_id then
        vim.lsp.buf_attach_client(bufnr, client_id)
    end
end

---Configure and enable the Orna LSP integration.
---@param opts orna.Config|nil
function M.setup(opts)
    local config = vim.tbl_deep_extend("force", {}, defaults, opts or {})

    if vim.lsp.config then
        -- Neovim 0.11+ native configuration path.
        vim.lsp.config("orna", {
            cmd = config.cmd,
            filetypes = { "orna" },
            root_dir = root_dir,
            capabilities = base_capabilities(),
            settings = config.settings,
        })
        vim.lsp.enable("orna")
    else
        -- Legacy path: start the client on the first Orna buffer.
        local group = vim.api.nvim_create_augroup("orna-lsp", { clear = true })
        vim.api.nvim_create_autocmd("FileType", {
            group = group,
            pattern = "orna",
            callback = function(args)
                legacy_attach(args.buf, config)
            end,
        })
    end
end

-- Enable eagerly on load; set vim.g.orna_skip_auto_setup = true in your
-- config to defer and call require("orna").setup({ ... }) yourself.
if not vim.g.orna_skip_auto_setup then
    M.setup()
end

return M
