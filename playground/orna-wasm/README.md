# Orna wasm core

`orna-wasm` exposes the bounded Orna typed REPL as a WebAssembly module built
with `wasm-pack --target web`. The exported `run(source)` function creates a
fresh in-memory session and returns stable JSON with `ok`, `values`, `stdout`,
and located `errors`. The exported `ReplSession` class retains bindings between
calls to `evaluate(line)` and returns `value`, `error`, or `echo` JSON events.

The wasm build disables evaluator project loading and native host providers.
Pure parsing, semantic admission, evaluation, and session storage remain in the
existing Orna crates. Inputs stay subject to the evaluator's default resource
limits.

Build and run the focused Node tests with:

```sh
wasm-pack build playground/orna-wasm --target web
wasm-pack test playground/orna-wasm --node
```
