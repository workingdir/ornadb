# Playground serve integration

Work ADR 0116 binds the playground's ordinary Orna application architecture.
This note records the current `orna serve` dogfood path and its process-level
verification; it does not replace that architecture decision.

## Served surface

The S1 Git listing remains at `/`, with committed tree and blob views at
`/tree/<commit>/<path>` and `/blob/<commit>/<path>`. Its page links to the
playground at `/playground/`. The Git listing handler serves the page template
and browser assets from `playground/web-ui/dist/` in the selected committed
snapshot; it does not read the mutable worktree. The same handler reads the
presentation and live bridge modules from their committed Git blobs. The built
bundle is stored with the database content so `orna serve` is the web host.
`/api/examples` reads committed rows from `playground.Sample` when
`playground.orna` declares that table, plus committed `.orna` files under
`playground/examples`.

`/playground/embed` serves the same committed page with its marked page header
hidden. It keeps the database session bridge and allows framing by another
origin. Both entries use the same sample feed and server runtime.

The page obtains a same-origin session through `POST /orna/session`, then
connects to the returned `/orna/live/<session>` WebSocket using
`orna.present.v1`. It watches the reserved run-events presentation, sends
explicit Eval requests, and resynchronizes that watch to receive typed
presentation deltas. Evaluation runs in the server's Orna runtime. The browser
does not contain a second evaluator or a language keyword inventory.

## Dogfood proof

Run the focused process-level integration test with:

```sh
export CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=2
cargo test -p orna-cli-v1 --test serve_playground_dogfood -- --nocapture
```

The test initializes a temporary Orna Git database, commits the `Sample`
schema and row, a legacy `.orna` example, and HTML/JavaScript fixtures, changes
the worktree copies, starts the actual `orna-cli-v1 serve` process, and uses
curl for the Git listing, database-resident playground and embed entries,
committed browser and live bridge assets, committed examples, and live session
creation. A WebSocket client then
follows the same `orna.present.v1`
watch, fingerprinted Eval, and Resync exchange used by the browser bridge.

The test sends two independently fingerprinted Eval requests before reading
either response, then checks that each response keeps its request identity and
value (`2` and `42`). It resynchronizes the original watch, applies the
revision `0..1` delta to the revision-zero presentation, and checks both run
events. A third Eval produces a revision `1..2` delta; applying it yields the
exact presentation from a fresh watch snapshot in the same served session.

The test checks the HTTP and live protocol boundary end to end without relying
on a separately running development server. Separate LSP browser-wrapper tests
exercise standard-library completion and hover data through the exports used
by the served web worker.
