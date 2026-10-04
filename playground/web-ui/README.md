# OrnaDB playground UI

This Vite app is the browser entry served by `orna serve` at `/playground/`.
The editor uses Monaco. Its example list comes from committed
`playground/examples/*.orna` records through `GET /api/examples`.

The existing `orna-wasm` `run()` API executes in a replaceable browser worker,
so Stop terminates that run. Diagnostics, completion, hover, and signature
help are thin JSON adapters over the existing `orna-lsp` analysis functions,
also built to WebAssembly and loaded in a browser worker.

Build requirements are Node.js and `wasm-pack`:

```sh
npm ci
npm test
npm run build
```

Then start OrnaDB from a Git worktree containing the generated build. Keep the
generated `dist` directory beside the source checkout so `orna serve` can serve
the built page:

```sh
cargo run -p orna-cli-v1 -- serve --port 8181
```

For local Vite development, build both WASM packages first. The examples API is
served by OrnaDB, while the Vite dev server serves the editor assets. Its API
proxy targets `http://127.0.0.1:8181`; set `ORNA_SERVE_URL` to override it.

The page can also be embedded with `<iframe src="/playground/" title="Orna playground"></iframe>`.
