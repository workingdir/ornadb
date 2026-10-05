# Playground serve integration

Work ADR 0116 binds the playground as a database-hosted Orna application.
`orna serve` is the web host; the selected database's committed Git snapshot
is the source for the shell, assets, programs, and examples.

## Database records and routes

`playground.orna` declares `Sample`, `Asset`, `Entry`, `Route`, `Theme`, and
`Layout` tables.
Sample rows and the legacy `playground/examples/*.orna` files feed
`/api/examples`. The shell, browser client, live bridge modules, Orna LSP
WebAssembly package, Monaco workers, generated editor configuration, and
embeddable entry script are stored as `playground.Asset` rows under
`playground/Asset/`. Each row records a normalized relative path and media
type. Text content is UTF-8; WebAssembly bytes are base64-encoded in the row
and decoded when served. `playground.Entry` rows select a page, embed, or
static-asset entry and point to an Asset path. `playground.Route` rows map
exact public paths to those entries. The build generates Route and Entry rows
alongside the Asset rows. `orna-syntax-v1` generates editor configuration
from the language lexer; the browser loads it from
`/playground/assets/orna-editor-config.json`.

Theme and responsive layout CSS are stored as named `playground.Theme` and
`playground.Layout` rows. The build keeps Monaco's support styles as Asset rows
and writes the shell's theme and layout styles to those dedicated tables.
`/playground/theme.css` and `/playground/layout.css` read their rows from the
committed Git listing, with an optional revision query selecting a specific
commit.

`npm run build` creates the browser bundle and refreshes these rows. Commit the
generated rows with source changes. For each request, `orna serve` resolves
Route, Entry, and Asset rows from one committed `HEAD`; it does not read
`playground/web-ui/dist/` or compile browser helpers into the server binary.
The `/playground/` and `/playground/embed` routes use the same shell record,
with the embed entry hiding the marked page header and applying its framing
policy. Static assets are returned with their checked media type and
`X-Content-Type-Options: nosniff`. A commit that changes a Route, Entry, or
Asset row is visible on the next request without restarting `orna serve`.
Accepted connections run independently. Mutable live protocol state remains
serialized, while Git listings, database assets, and the example feed keep
serving during an open presentation WebSocket. Each asset or example response
resolves its rows from one committed `HEAD`.
`/api/playground/revision` reports the committed Git object ID. The open page
polls it every two seconds and swaps both stylesheet links only after both
revision-pinned rows load, then updates Monaco's theme from the new CSS tokens.

Monaco uses the database-served shell assets to run the Orna LSP worker. The
worker supplies standard-library completion, hover, signature help, and
parameter and inferred type hints alongside diagnostics. Standard call hints
use the same pinned source catalogue as hover and signature help. The
`/playground/assets/embed.js` classic script creates an iframe pointed at the
same database's `/playground/embed` route. Set `data-target` to append the
iframe to a container, and optionally set `data-height`, `data-title`, or
`data-loading`. The entry script itself is a committed Asset row.

The browser sends explicit Run requests to the same clone's
`orna.present.v1` session. Source evaluation and presentation remain in the
server runtime. The browser does not contain a second evaluator or a
language-keyword inventory.

## Responsive and keyboard behavior

The two panes share the wide layout and stack on narrower screens. The example
selector retains native type-to-select and supports arrow keys, Home/End, and
five-row Page Up/Down movement. Loading a row updates the text editor and a
polite live announcement. Run is available by button or Ctrl/Command+Enter;
result tabs support the standard arrow and Home/End keys.

## Dogfood proof

Run the focused process-level integration test with:

```sh
export CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=2
cargo test --locked -p orna-cli-v1 --test serve_playground_dogfood -- --nocapture
```

The test initializes a temporary Orna Git database, commits crate-local
`.orna` schema, sample, route, entry, and asset fixtures, starts the real
`orna serve` process without a build directory, then uses curl to check the
Git listing, database-resident shell, editor configuration, embeddable script,
CSS/JavaScript and WebAssembly rows, committed examples, and live session. It
commits new Route and Entry rows after startup and proves their URLs change
from 404 to an HTML page and a JavaScript asset with the checked media type,
without restarting the server. A WebSocket client follows the existing watch,
fingerprinted Eval, and Resync exchange to prove independent results and
presentation deltas. While that WebSocket remains open, the test commits a new
Route and Entry rows after startup and proves their URLs change from 404 to
an HTML page and a JavaScript asset with the checked media type, without
restarting the server. It also checks DB-resident Theme/Layout CSS,
an uncommitted style remaining invisible, the committed revision changing both
styles, and the old revision continuing to serve both old styles. A WebSocket
client follows the existing watch, fingerprinted Eval, and Resync exchange to
prove independent results and presentation deltas. While that WebSocket
remains open, the test commits a new Route, Entry, Asset, and Sample snapshot
and concurrently fetches the asset and example feed to prove the responses use
committed database rows during live presentation deltas.

The browser assist proof loads the LSP JavaScript and WebAssembly from an
active `orna serve` database:

```sh
npm run prove:served-assists -- http://127.0.0.1:18087
```

It verifies completion ranking, standard-library hover and signature help, and
imported and qualified standard-library inlay hints from the served module.
It also starts two independent Node workers that fetch the shell, LSP binding,
and WebAssembly from `orna serve` concurrently. Each worker requests hints for
a different standard-library source and checks its own labels and source
positions, which catches cross-client response or document-state leakage.
