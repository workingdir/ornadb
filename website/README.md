# OrnaDB website

This directory is an isolated static website project.

- Astro builds the marketing frontpage at `/`.
- Sourcey builds the Markdown documentation at `/docs`.
- `docs/sourcey.config.ts` defines the documentation order and theme.
- `scripts/postprocess-sourcey.mjs` adds the production skip link and normalises known Sourcey 3.6 HTML issues.

## Develop

Use Yarn 4 with Node.js 22 or later.

```bash
yarn install --immutable
yarn dev
```

Astro serves the frontpage and the Sourcey integration serves the documentation from the same development server.

## Build

```bash
yarn build
```

The command runs Astro diagnostics, builds the static site into `dist/`, builds the Sourcey documentation, and post-processes the generated documentation HTML.

Generated documentation includes:

```text
dist/docs/
    index.html
    search-index.json
    llms.txt
    llms-full.txt
    sitemap.xml
```

## Content rules

Read `PRODUCT.md` and `DESIGN.md` before changing the interface or copy. Keep these status categories distinct:

- `IMPLEMENTED`: present in the working repository;
- `LOCKED`: accepted conceptual design;
- `CURRENT PROPOSAL`: the current experiment target;
- `OPEN`: unresolved;
- `FUTURE`: intentionally outside the first implementation.

Do not present design examples as released features. The implementation checklist in `../TODO.md` remains the source for current repository status.
