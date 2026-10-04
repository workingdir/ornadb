# Playground serve integration

Work ADR 0116 binds the playground as a database-hosted Orna application.
`orna serve` is the web host; the selected database's committed Git snapshot
is the source for the shell, assets, programs, and examples.

## Database records and routes

`playground.orna` declares `Sample` and `Asset` tables. Sample rows and the
legacy `playground/examples/*.orna` files feed `/api/examples`. The shell,
stylesheet, browser client, live bridge modules, and Orna LSP WebAssembly
package, Monaco workers, generated editor configuration, and the embeddable
entry script are generated as `playground.Asset` rows under `playground/Asset/`.
Each row records a normalized relative path and media type. Text content is
stored as UTF-8; WebAssembly bytes use base64 in the row and are decoded when
served. `orna-syntax-v1` generates the editor configuration from the language
lexer; the browser loads it from `/playground/assets/orna-editor-config.json`.

`npm run build` creates the small browser bundle and refreshes these rows.
Commit the generated rows along with source changes. At request time,
`orna serve` reads the row for the requested asset from the committed `HEAD`
using the Git listing handler; it does not read `playground/web-ui/dist/` or
compile the browser helpers into the server binary.
The `/playground/` and `/playground/embed` routes use the same shell record,
with the embed route hiding the marked page header and applying its framing
policy. Static assets are returned with their checked media type and
`X-Content-Type-Options: nosniff`.

Monaco uses the database-served shell assets to run the Orna LSP worker. The
worker supplies standard-library completion and hover alongside signature
help and diagnostics. The `/playground/assets/embed.js` classic script creates an iframe pointed at
the same database's `/playground/embed` route. Set `data-target` to append the
iframe to a container, and optionally set `data-height`, `data-title`, or
`data-loading`. The entry script itself is a committed Asset row.

The browser sends explicit Run requests to the same clone's
`orna.present.v1` session. Source evaluation and presentation remain in the
server runtime. The browser does not contain a second evaluator or a
language-keyword inventory.

## Responsive and keyboard behavior

The two panes share the wide layout and stack on narrower screens. The example
selector retains native type-to-select and supports arrow keys, Home/End, and
five-row Page Up/Down movement. Loading a row updates Monaco and a
polite live announcement. Run is available by button or Ctrl/Command+Enter;
result tabs support the standard arrow and Home/End keys.

## Dogfood proof

Run the focused process-level integration test with:

```sh
export CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=2
cargo test --locked -p orna-cli-v1 --test serve_playground_dogfood -- --nocapture
```

The test initializes a temporary Orna Git database, commits crate-local
`.orna` schema, sample, and asset fixtures, starts the real `orna serve`
process without a build directory, then uses curl to check the Git listing,
the database-resident shell, editor configuration, embeddable script, and
CSS/JavaScript rows, committed examples, and the live session. A WebSocket client follows the existing watch,
fingerprinted Eval, and Resync exchange to prove independent results and
presentation deltas.
