use zed_extension_api as zed;

struct OrnaExtension;

impl zed::Extension for OrnaExtension {
    fn new() -> Self {
        Self
    }

    fn language_server_command(
        &mut self,
        _language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> zed::Result<zed::Command> {
        let command = worktree
            .which("orna-lsp")
            .ok_or_else(|| "orna-lsp was not found on PATH; build it with cargo build -p orna-lsp --release".to_owned())?;

        Ok(zed::Command {
            command,
            args: Vec::new(),
            env: Vec::new(),
        })
    }
}

zed::register_extension!(OrnaExtension);
