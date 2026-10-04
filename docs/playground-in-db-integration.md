# Playground serve integration

Work ADR 0116 binds the playground as a database-hosted Orna application.
`orna serve` is the web host; the selected database's committed Git snapshot
is the source for the shell, assets, programs, and examples.

## Database records and routes

`playground.orna` declares `Sample`, `Asset`, `Theme`, and `Layout` tables.
Sample rows and the legacy `playground/examples/*.orna` files feed
`/api/examples`. The shell and browser client are generated as
`playground.Asset` rows under `playground/Asset/`. Each row records a
normalized relative path, media type, and UTF-8 content. Theme and responsive
layout CSS are separate named rows under `playground/Theme/` and
`playground/Layout/`.

`npm run build` creates the small browser bundle and refreshes the asset,
theme, and layout rows. Commit the generated rows along with source changes.
At request time, `orna serve` reads each row from the committed `HEAD` using
the repository listing API; it does not read `playground/web-ui/dist/`.
The `/playground/` and `/playground/embed` routes use the same shell record,
with the embed route hiding the marked page header and applying its framing
policy. Static assets are returned with their checked media type and
`X-Content-Type-Options: nosniff`.

`/api/playground/revision` reports the current committed Git object ID with
`Cache-Control: no-store`. The browser checks it every two seconds. If `HEAD`
changes, it loads both stylesheet rows before replacing the active theme and
layout links. This updates committed CSS without restarting `orna serve` or
reloading the page. Uncommitted style edits remain invisible until committed.

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
`.orna` schema, sample, and asset fixtures, starts the real `orna serve`
process without a build directory, then uses curl to check the Git listing,
the database-resident shell and its JavaScript, theme, and layout rows,
committed examples, and the live session. It commits a theme update and proves
the stylesheet and revision change without restarting the server. A WebSocket
client follows the existing watch,
fingerprinted Eval, and Resync exchange to prove independent results and
presentation deltas.
