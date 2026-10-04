# Orna playground web UI

The Vite page is served by the selected `orna serve` database at
`/playground/`. The page and bundled assets are read from the selected Git
snapshot; examples come from `/api/examples`. It runs source through the
database's authenticated `orna.present.v1` session and renders the server's
run-event presentation in the result tabs. Orna evaluation stays in the
server runtime. The browser worker loads `orna-lsp`'s shared analysis core for
completion, hover, diagnostics, and signature help.

Build the page and editor worker artifacts with:

    npm ci
    npm run build

Commit the generated `playground/web-ui/dist/` bundle into the database
snapshot (`git add -f playground/web-ui/dist` while the directory remains
ignored). `orna serve` exposes only the committed page and assets. Then run it
from that clone and open http://127.0.0.1:8181/playground/. The Monaco
tokenizer and its keyword metadata are generated from `orna-syntax-v1`; check
drift with:

    cargo run --locked -p orna-syntax-v1 --example generate_editor_artifacts -- --check
