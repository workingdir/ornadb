# Orna playground web UI

The Vite page is served by the selected `orna serve` clone at `/playground/`.
It loads committed files from `playground/examples` and rows from
`playground.Sample` through `/api/examples`, runs source through the clone's
authenticated `orna.present.v1` session, and renders the server's run-event
presentation in the result tabs. Orna evaluation stays in the server runtime.
The browser worker for editor intelligence loads orna-lsp's shared analysis
core.

Build the page and editor worker artifacts with:

    npm ci
    npm run build

Then run `orna serve` from the clone and open
http://127.0.0.1:8181/playground/. The Monaco tokenizer and its keyword
metadata are generated from orna-syntax-v1; check drift with:

    cargo run --locked -p orna-syntax-v1 --example generate_editor_artifacts -- --check

Embed the editor in another page with an iframe pointed at
`/playground/embed` on the same served clone:

    <iframe src="https://your-orna-host/playground/embed" title="Orna playground"></iframe>

That entry hides the database link and allows framing by another origin. It
uses the same examples, editor, and OrnaDB runtime as `/playground/`.

For pages that need a script entry, the stable and development channels publish
`embed.js` at `/ornadb/embed.js` and `/ornadb/dev/embed.js`. Point `data-src` at
the `/playground/embed` route on an `orna serve` clone; the Pages host serves the
loader and editor assets, while the clone still supplies examples and evaluation.

```html
<div id="orna-playground"></div>
<script
  defer
  src="https://workingdir.github.io/ornadb/embed.js"
  data-target="#orna-playground"
  data-src="https://your-orna-host/playground/embed"
></script>
```

Use `https://workingdir.github.io/ornadb/dev/embed.js` to load the development
channel's entry script. The loader creates a lazy, full-width iframe in the
selected target; `data-src` must point to a running `orna serve` playground.

## GitHub Pages channels

The Pages workflow publishes the stable channel from `main` at
https://workingdir.github.io/ornadb/ and the development channel from the
playground milestone branch at https://workingdir.github.io/ornadb/dev/.
Pull requests run the build checks without publishing. The Pages smoke checks
each channel's HTML, built assets, and editor WebAssembly modules. Examples and
source evaluation still use the `orna serve` API described above.
