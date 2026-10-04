# Orna playground web UI

This directory holds the source for the small browser shell. `npm run build`
bundles the shell and writes each HTML, CSS, JavaScript, and WebAssembly asset
as a `playground.Asset` row, including the shared live modules and browser LSP
package. Commit those generated rows with the source changes.
`orna serve` reads the rows from the selected database's committed Git snapshot
and serves them at `/playground/`; it does not serve `dist/` from the
filesystem.

The page loads committed files from `playground/examples` and rows from
`playground.Sample` through `/api/examples`. Use the example selector's arrow,
Home/End, Page Up/Down, or type-to-select behavior to move through the feed.
Run requests go to the same clone's authenticated `orna.present.v1` session,
where the server's Orna runtime evaluates the source.

Build and test from this directory with:

    npm ci
    npm run build
    npm test -- --reporter=dot

The shell stays responsive. Monaco requests syntax diagnostics, signature
help, standard-library completion, and hover details from the in-process Orna
LSP worker. The page does not carry a second language vocabulary or evaluator.
