# OrnaDB website delivery

## Context

- [x] Read the canonical v0.2 handbook, APIs, ADRs, examples, machine specifications, and implementation status.
- [x] Review PipeWire, Handy, Wayland, and Sourcey.
- [x] Select Astro for the frontpage and Sourcey for Markdown documentation at `/docs`.

## Foundation

- [x] Configure the Astro static build and Sourcey integration.
- [x] Add the initial Sourcey documentation index and theme override.
- [x] Validate a minimal production build.

## Frontpage

- [x] Build the responsive frontpage with real Orna source and invocation examples.
- [x] Show product model, features, use cases, architecture, dogfooding, and project status.
- [x] Add keyboard focus, reduced-motion handling, metadata, and a skip link.

## Documentation

- [x] Write the documentation pages as Markdown in this `docs/` directory.
- [x] Keep locked decisions, current proposals, open details, and implementation status distinct.
- [x] Verify internal links, search output, and `llms.txt`.

## Review

- [x] Run formatting, type checks, build, link checks, and similarity checks.
- [x] Inspect the built site at desktop and mobile sizes.
- [x] Run accessibility and performance checks where local tools permit.
- [x] Update this checklist and the design record to match the shipped code.

## Validation record

- Astro diagnostics: 0 errors, 0 warnings, and 0 hints across 9 files.
- Production build: 1 frontpage, 11 Sourcey pages, and 12 normalised documentation routes.
- Nu HTML Checker: 0 errors and 0 warnings on the frontpage and documentation entry.
- Internal navigation: 532 local links and fragments checked across 13 HTML files.
- Generated discovery: 94 search entries, 11 `llms.txt` links, and a 47,624-byte `llms-full.txt`.
- Browser checks: no page-level overflow at 320, 390, 768, or 1,440 pixels; named controls, landmarks, reduced motion, first-tab skip navigation, search, and theme switching verified.
- Contrast: sampled text pairs range from 5.70:1 to 19.36:1.
- Security and duplication: Yarn reported no audit suggestions; `similarity-ts` found no duplicate functions or types.
