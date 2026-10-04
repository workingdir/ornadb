# Orna playground web UI

`orna serve` reads the playground page and every browser asset from committed
`playground.Asset` rows in the selected database. The row key is the URL path
relative to `/playground/`; each row stores its media type and standard padded
Base64 bytes. The server reads rows from the selected Git commit, so edits in
the worktree appear after they are committed.

Examples come from the committed `playground.Sample` rows and legacy `.orna`
example files through `/api/examples`. Source runs use the selected clone's
authenticated `orna.present.v1` session. Orna evaluation stays in the server
runtime. The browser worker for editor intelligence loads orna-lsp's shared
analysis core, and editor language metadata comes from generated syntax-v1
artifacts.

Build the browser bundle and refresh its database asset rows with:

    npm ci
    npm run build
    npm run db-assets:sync

`npm run db-assets:check` verifies that the committed rows exactly match the
current build. Commit the resulting files under `playground/Asset/` with the
web UI source changes. The generator rejects oversized files and unsupported
paths.

Run `orna serve` from the clone and open
http://127.0.0.1:8181/playground/. A page can also use
`/playground/embed` on that same server; it reads the same database rows,
examples, and runtime as the regular entry.

The Monaco tokenizer and its keyword metadata are generated from
orna-syntax-v1; check drift with:

    cargo run --locked -p orna-syntax-v1 --example generate_editor_artifacts -- --check
