# OrnaDB website delivery

## Context

- [x] Read the canonical v0.2 handbook, APIs, ADRs, examples, machine specifications, and implementation status.
- [x] Review PipeWire, Handy, Wayland, and Sourcey.
- [x] Select Sourcey as the complete documentation-first website at `/`, with Astro as its build host.

## Foundation

- [x] Configure the Astro static build and root-mounted Sourcey integration.
- [x] Add the Sourcey documentation index, navigation, search, and theme override.
- [x] Preserve former `/docs` routes with generated compatibility redirects.

## Overview

- [x] Use the Markdown documentation index as the website homepage.
- [x] Establish the product model with real Orna source, invocation commands, and explicit development status.
- [x] Route readers directly into language, execution, architecture, security, examples, and status pages.
- [x] Remove the separate custom marketing frontpage and its bespoke Astro components and styles.

## Documentation

- [x] Write the documentation pages as Markdown in this `docs/` directory.
- [x] Keep locked decisions, current proposals, open details, and implementation status distinct.
- [x] Verify internal links, search output, `llms.txt`, and legacy redirects.

## Review

- [x] Run formatting, type checks, build, link checks, and similarity checks.
- [x] Inspect the built site at desktop and mobile sizes.
- [x] Run accessibility, interaction, and performance checks where local tools permit.
- [x] Update this checklist and the design record to match the shipped code.

## Validation record

- Astro diagnostics: 0 errors, 0 warnings, and 0 hints across 4 files.
- Production build: 11 root-mounted Sourcey pages and 12 legacy `/docs` redirects.
- Nu HTML Checker: 0 errors and 0 warnings on the root, guide, and redirect outputs.
- Internal navigation: 488 local links and fragments checked across 23 HTML files.
- Generated discovery: 94 search entries, 11 `llms.txt` links, and a 48,072-byte `llms-full.txt`.
- Browser checks: no page-level overflow at 320, 390, 768, or 1,440 pixels; named controls, landmarks, reduced motion, first-tab skip navigation, search, theme switching, and deep-route redirects verified.
- Security and duplication: Yarn reported no audit suggestions; `similarity-ts` found no duplicate functions or types.
