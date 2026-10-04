# Orna playground web UI

The Vite page is served by the selected orna serve clone at /playground/.
It loads committed examples from /api/examples, runs source through the
clone's authenticated orna.present.v1 session, and renders the server's
run-event presentation in the result tabs. Orna evaluation stays in the
server runtime. The browser worker for editor intelligence loads
orna-lsp's shared analysis core.

Build the page and editor worker artifacts with:

    npm ci
    npm run build

Then run orna serve from the clone and open
http://127.0.0.1:8181/playground/. The Monaco tokenizer and its keyword
metadata are generated from orna-syntax-v1; check drift with:

    cargo run --locked -p orna-syntax-v1 --example generate_editor_artifacts -- --check
