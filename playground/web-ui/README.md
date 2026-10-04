# Orna playground web UI

This directory holds the source for the small browser shell. `npm run build`
bundles the shell and writes HTML and JavaScript as `playground.Asset` rows.
Theme and responsive layout CSS are separate `playground.Theme` and
`playground.Layout` rows. Commit those generated rows with the source changes.
`orna serve` reads all of them from the selected database's committed Git
snapshot and serves them at `/playground/`; it does not serve `dist/` from the
filesystem.

While the page is open, it checks `/api/playground/revision` every two seconds.
A new committed `HEAD` causes both stylesheet rows to load and swap together,
so theme and layout edits appear without restarting `orna serve` or reloading
the page. Uncommitted CSS changes wait for a commit.

The page loads committed files from `playground/examples` and rows from
`playground.Sample` through `/api/examples`. Use the example selector's arrow,
Home/End, Page Up/Down, or type-to-select behavior to move through the feed.
Run requests go to the same clone's authenticated `orna.present.v1` session,
where the server's Orna runtime evaluates the source.

Build and test from this directory with:

    npm ci
    npm run build
    npm test -- --reporter=dot

The shell stays plain and responsive. It shows source in a text area and uses
the generated Orna syntax artifacts only in tooling; the page does not carry a
second language vocabulary or evaluator.
