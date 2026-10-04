# Orna Playground web UI

The web UI uses Monaco for `.orna` editing and the bounded pure evaluator compiled to WebAssembly.

## Run locally

Install Node.js, Rust with the `wasm32-unknown-unknown` target, and `wasm-pack`. Then run:

```sh
npm ci
npm run wasm:build
npm run dev
```

`npm run wasm:build` writes generated JavaScript and WebAssembly files to `playground/orna-wasm/pkg/`. Vite serves them during development and copies them into the production build.

The browser runtime supports the pure admitted REPL subset. It returns values and redacted diagnostics; host effects are rejected.

## Verify

```sh
npm test
npm run build
cargo test --manifest-path ../orna-wasm/Cargo.toml
```
