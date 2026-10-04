# Orna playground web UI

`orna serve` hosts the playground from the selected database snapshot. The page
and bundled browser assets come from committed `playground/web-ui/dist/`
entries. `/api/examples` reads committed `playground.Sample` rows when
`playground.orna` declares that table, along with committed `.orna` files under
`playground/examples`. Source runs through the database's authenticated
`orna.present.v1` session in the server runtime. The browser worker uses
`orna-lsp`'s shared analysis core for completion, hover, diagnostics, and
signature help.

Build the page and editor worker artifacts with:

    npm ci
    npm run build

Commit the generated bundle into the database snapshot with
`git add -f playground/web-ui/dist` while the directory remains ignored.
`orna serve` exposes only the committed page and assets. Then run it from that
clone and open http://127.0.0.1:8181/playground/.

The shared presentation runtime and serve bridge modules are also loaded from
committed Git blobs through `orna serve`'s listing handler. The server does not
serve worktree-only copies of these UI assets.

The Monaco tokenizer and its keyword metadata are generated from
`orna-syntax-v1`; check drift with:

    cargo run --locked -p orna-syntax-v1 --example generate_editor_artifacts -- --check

Embed the editor in another page with an iframe pointed at
`/playground/embed` on the same served clone:

    <iframe src="https://your-orna-host/playground/embed" title="Orna playground"></iframe>

That entry hides the database link and allows framing by another origin. It
uses the same committed shell, examples, editor, and OrnaDB runtime as
`/playground/`.
