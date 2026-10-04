# Playground serve integration

Work ADR 0116 binds the playground's ordinary Orna application architecture.
This note records the current `orna serve` dogfood path and its process-level
verification.

## Database-resident page and assets

The S1 Git listing remains at `/`, with committed tree and blob views at
`/tree/<commit>/<path>` and `/blob/<commit>/<path>`. Its page links to the
playground at `/playground/`. `orna serve` reads the page shell and browser
assets from committed `playground.Asset` records in the selected clone. Each
row is keyed by its normalized URL path and stores a media type and standard
padded Base64 bytes. The server resolves the row in the selected Git commit;
it does not read a generated `dist` directory from disk.

`/api/examples` returns committed `playground.Sample` records and legacy
`.orna` examples from that same clone. The page obtains a same-origin session
through `POST /orna/session`, then connects to the returned
`/orna/live/<session>` WebSocket using `orna.present.v1`. It watches the
reserved run-events presentation, sends explicit Eval requests, and
resynchronizes that watch to receive typed presentation deltas. Evaluation
runs in the server's Orna runtime. The browser does not contain a second
evaluator or a language keyword inventory.

Build and update the committed browser rows from `playground/web-ui` with:

```sh
npm ci
npm run build
npm run db-assets:sync
```

Use `npm run db-assets:check` to verify that rows match the build. Generated
rows live under `playground/Asset/`, beside the existing `playground/Sample/`
rows.

The GitHub Pages channel workflows and script loader were removed. The
playground page, examples, programs, and assets are served from OrnaDB through
`orna serve`.

## Dogfood proof

Run the process-level integration test with:

```sh
export CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=2
cargo test -p orna-cli-v1 --test serve_playground_dogfood -- --nocapture
```

The test initializes a temporary Orna Git database, commits crate-local
`.orna` fixtures for the UI shell, browser asset, schema, and examples, starts
the actual `orna-cli-v1 serve` process, and uses curl for the Git listing,
database-resident playground page and asset, committed examples, and live
session creation. A WebSocket client then follows the same `orna.present.v1`
watch, fingerprinted Eval, and Resync exchange used by the browser bridge.

The test sends two independently fingerprinted Eval requests before reading
either response, then checks that each response keeps its request identity and
value (`2` and `42`). It resynchronizes the original watch, applies the
revision `0..1` delta to the revision-zero presentation, and checks both run
events. A third Eval produces a revision `1..2` delta; applying it yields the
exact presentation from a fresh watch snapshot in the same served session.

The test checks the HTTP and live protocol boundary end to end without relying
on a separately running development server or an ignored `dist` directory.
