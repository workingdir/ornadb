# OrnaDB website

This directory is an isolated static website project.

- Sourcey builds the Markdown documentation as the complete website at `/`.
- Astro hosts the Sourcey integration and static build lifecycle; there is no separate marketing frontpage.
- `docs/sourcey.config.ts` defines the page order, navigation, and theme.
- `scripts/postprocess-sourcey.mjs` normalises known Sourcey 3.6 HTML issues and preserves old `/docs` URLs as redirects.

## Develop

Use Yarn 4 with Node.js 22 or later.

```bash
yarn install --immutable
yarn dev
```

The Sourcey integration serves the complete documentation website from the Astro development server.

## Build

```bash
yarn build
```

The command runs Astro diagnostics, writes the Sourcey site directly into `dist/`, normalises the generated HTML, and adds redirects for the former `/docs` routes.

Generated output includes:

```text
dist/
    index.html
    getting-started/index.html
    search-index.json
    llms.txt
    llms-full.txt
    sitemap.xml
    docs/index.html              # compatibility redirect
```

## Content rules

Read `PRODUCT.md` and `DESIGN.md` before changing the interface or copy. Keep these status categories distinct:

- `IMPLEMENTED`: present in the working repository;
- `LOCKED`: accepted conceptual design;
- `CURRENT PROPOSAL (conceptual)`: the current unresolved experiment target; never a released feature;
- `OPEN`: unresolved;
- `FUTURE`: intentionally outside the first implementation.

Use `CURRENT PROPOSAL (conceptual)` for full Studio/security-console dogfooding, reflective JSON-RPC/MCP gateways, and `CREATE USER`/`ROLE`/`GRANT` DDL sugar until canonical status accepts them. The accepted local `std.security` administration functions and bounded Inspector remain narrower boundaries.

Do not present design examples as released features. The implementation checklist in `../../TODO.md` remains the source for current repository status.
