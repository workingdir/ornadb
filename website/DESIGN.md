---
name: OrnaDB documentation website
description: A documentation-first systems project site built entirely in Sourcey.
colors:
  background: "oklch(1 0 0)"
  surface: "oklch(0.965 0.009 142)"
  surface-active: "oklch(0.94 0.035 142)"
  ink: "oklch(0.16 0.018 145)"
  muted: "oklch(0.42 0.025 145)"
  line: "oklch(0.83 0.018 142)"
  brand: "oklch(0.42 0.105 142)"
  brand-deep: "oklch(0.29 0.075 142)"
  accent: "oklch(0.87 0.145 98)"
  code: "oklch(0.105 0.015 145)"
  code-ink: "oklch(0.94 0.015 142)"
typography:
  headline:
    fontFamily: "Public Sans, Arial, sans-serif"
    fontSize: "2rem"
    fontWeight: 700
    lineHeight: 1.2
    letterSpacing: "-0.025em"
  body:
    fontFamily: "Public Sans, Arial, sans-serif"
    fontSize: "1rem"
    fontWeight: 400
    lineHeight: 1.6
  code:
    fontFamily: "Martian Mono, Cascadia Mono, Liberation Mono, monospace"
    fontSize: "0.875rem"
    fontWeight: 400
    lineHeight: 1.7
rounded:
  sm: "4px"
  md: "6px"
spacing:
  xs: "8px"
  sm: "16px"
  md: "24px"
  lg: "48px"
components:
  active-navigation:
    backgroundColor: "{colors.surface-active}"
    textColor: "{colors.brand-deep}"
    rounded: "{rounded.md}"
    padding: "8px 16px"
  inline-code:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.brand-deep}"
    typography: "{typography.code}"
    rounded: "{rounded.sm}"
    padding: "2px 4px"
  source-window:
    backgroundColor: "{colors.code}"
    textColor: "{colors.code-ink}"
    typography: "{typography.code}"
    rounded: "{rounded.md}"
    padding: "20px"
---

# Design System: OrnaDB Documentation Website

## Overview

**Creative North Star: "The Maintained Systems Guide"**

The documentation is the website. There is no separate campaign-style frontpage, product pitch, or visual hand-off before readers reach technical material. Sourcey supplies the stable documentation shell at `/`; the overview page introduces the model with real syntax and links directly into the guide.

The interface should resemble a maintained open-source systems project: quiet, exact, searchable, and close to source. Navigation, status, code, and cross-references carry the experience. The site rejects generic SaaS landing pages, glossy AI database visuals, decorative terminal themes, and editorial-magazine styling.

**Key Characteristics:**

- one documentation interface at every route;
- technical content and status before promotional claims;
- persistent navigation, full-text search, and on-page headings;
- code as the primary visual material;
- a restrained moss identity on a white reading surface;
- responsive navigation and complete keyboard access.

## Colors

The light theme uses white for long reading, pale moss for selected navigation and inline code, near-black for fenced source, and a sharp yellow-green focus outline. The dark theme retains readable semantic equivalents. Status callouts use a written title and a full border; colour never carries status alone.

**The Status Is Text Rule.** Every status colour has a written label such as `IMPLEMENTED`, `LOCKED`, `CURRENT PROPOSAL`, or `OPEN`.

**The Reading Surface Rule.** Moss identifies selection and structure. It does not become a decorative page-sized field.

## Typography

**Body and Heading Font:** Public Sans with Arial and system sans-serif fallbacks.

**Code Font:** Martian Mono with Cascadia Mono and Liberation Mono fallbacks.

Public Sans keeps long technical pages neutral and legible. Martian Mono is reserved for source, commands, inline identifiers, and technical labels. Body lines stay below 72 characters where Sourcey layout permits.

**The Source Has Priority Rule.** Fenced source remains large enough to read, uses high-contrast dark syntax colours, and scrolls horizontally rather than clipping or wrapping semantics.

## Elevation

The site uses no shadows. Sourcey regions are separated with whitespace, subtle rules, selected navigation backgrounds, and solid code surfaces.

**The Flat By Default Rule.** Depth indicates structure or interaction, never decoration.

## Components

### Documentation Shell

- Sourcey owns the header, search, desktop sidebar, mobile drawer, table of contents, footer, and theme switcher.
- The OrnaDB overview is the first navigation item and the root route.
- Navigation remains visible on desktop and collapses to Sourcey's drawer on narrow screens.

### Status Callouts

- A full 1px border and pale semantic surface distinguish the callout from prose.
- The title states the category, such as `DEVELOPMENT STATUS`.
- Callouts explain whether surrounding examples are implemented or intended design.

### Source Windows

- Near-black background with Sourcey's dark syntax token set in both page themes.
- A restrained 6px radius and no shadow.
- Copy controls retain an accessible name.
- Horizontal scrolling preserves source structure.

### Inline Code

- Pale moss surface, deep moss text, a 1px border, and practical 4px corners.
- No decorative backtick pseudo-content.
- Used for identifiers and short syntax only.

### Links and Navigation

- Text links remain visibly interactive through colour or underline states.
- Active navigation uses both background and text colour.
- Focus uses the shared yellow-green outline with an offset.
- The first keyboard stop is a skip link to main content.

Motion is limited to direct Sourcey state changes. `prefers-reduced-motion` reduces transition durations and disables smooth scrolling.

## Do's and Don'ts

### Do:

- **Do** put valid Orna syntax and an honest development-status notice on the overview.
- **Do** send readers directly to language, invocation, architecture, security, examples, and status pages.
- **Do** preserve Sourcey's search, navigation, responsive layout, and generated discovery files.
- **Do** distinguish implemented work, locked design, current proposals, open questions, and future work in text.
- **Do** keep internal links rooted at their final public routes.

### Don't:

- **Don't** add a separate marketing frontpage before the documentation.
- **Don't** build conversion funnels, testimonial strips, pricing cards, or inflated claims.
- **Don't** replace technical material with gradients, abstract graphics, or decorative terminals.
- **Don't** present a current proposal or roadmap item as a released feature.
- **Don't** use ambient shadows, glass effects, large radii, or decorative grids.
- **Don't** fork or recreate Sourcey's navigation and documentation behaviours in custom Astro components.
