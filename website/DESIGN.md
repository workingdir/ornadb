---
name: OrnaDB website
description: A technical systems manual with a clear, inspectable product argument.
colors:
  background: "oklch(1 0 0)"
  surface: "oklch(0.965 0.009 142)"
  surface-strong: "oklch(0.925 0.018 142)"
  ink: "oklch(0.16 0.018 145)"
  muted: "oklch(0.42 0.025 145)"
  line: "oklch(0.83 0.018 142)"
  brand: "oklch(0.42 0.105 142)"
  brand-deep: "oklch(0.29 0.075 142)"
  brand-light: "oklch(0.75 0.09 140)"
  accent: "oklch(0.87 0.145 98)"
  code: "oklch(0.105 0.015 145)"
  code-line: "oklch(0.27 0.025 145)"
  code-ink: "oklch(0.94 0.015 142)"
typography:
  display:
    fontFamily: "Public Sans, Arial, sans-serif"
    fontSize: "clamp(3.25rem, 6.6vw, 5.75rem)"
    fontWeight: 700
    lineHeight: 0.94
    letterSpacing: "-0.04em"
  headline:
    fontFamily: "Public Sans, Arial, sans-serif"
    fontSize: "clamp(2.25rem, 4.8vw, 4.75rem)"
    fontWeight: 700
    lineHeight: 1
    letterSpacing: "-0.04em"
  body:
    fontFamily: "Public Sans, Arial, sans-serif"
    fontSize: "1rem"
    fontWeight: 400
    lineHeight: 1.6
  label:
    fontFamily: "Martian Mono, Cascadia Mono, Liberation Mono, monospace"
    fontSize: "0.6875rem"
    fontWeight: 600
    lineHeight: 1.4
rounded:
  sm: "4px"
  md: "8px"
spacing:
  xs: "8px"
  sm: "16px"
  md: "24px"
  lg: "48px"
  section: "clamp(5rem, 10vw, 9rem)"
components:
  button-brand:
    backgroundColor: "{colors.brand-deep}"
    textColor: "{colors.background}"
    typography: "{typography.label}"
    rounded: "{rounded.sm}"
    padding: "12px 16px"
    height: "48px"
  button-light:
    backgroundColor: "{colors.background}"
    textColor: "{colors.brand-deep}"
    typography: "{typography.label}"
    rounded: "{rounded.sm}"
    padding: "12px 16px"
    height: "48px"
  source-window:
    backgroundColor: "{colors.code}"
    textColor: "{colors.code-ink}"
    typography: "{typography.label}"
    rounded: "{rounded.md}"
    padding: "16px 24px"
  status-label:
    backgroundColor: "{colors.background}"
    textColor: "{colors.brand-deep}"
    typography: "{typography.label}"
    rounded: "{rounded.sm}"
    padding: "4px 6px"
---

# Design System: OrnaDB Website

## Overview

**Creative North Star: "The Executable Systems Manual"**

The interface makes OrnaDB feel like a maintained systems manual whose first page also proves the product argument. The design combines the direct project-page structure of PipeWire and Wayland with the stronger product demonstration used by Handy. Real source, result paths, process boundaries, and implementation status provide the visual material.

The frontpage commits to moss green, black, white, and a sharp yellow-green accent. Documentation uses the same identity on a quieter white reading surface. The system rejects generic SaaS landing pages, glossy AI database sites, decorative terminal themes, and editorial-magazine styling.

**Key Characteristics:**

- technical content before promotional claims;
- visible CLIENT, SERVER, protocol, and runtime boundaries;
- code and architecture as the primary imagery;
- flat surfaces separated by colour, spacing, and solid rules;
- restrained motion with a complete reduced-motion path;
- explicit text labels for implementation and design status.

## Colors

The palette uses a committed moss field for major brand moments, pure white for long reading, near-black for source specimens, and yellow-green for focus and status markers. Muted colours remain strong enough for body-size text.

**The Status Is Text Rule.** Every status colour has a written label such as `IMPLEMENTED`, `LOCKED`, `PROPOSAL`, or `OPEN`.

**The Four-Material Rule.** Build pages from white, moss, near-black, and yellow-green. Do not add decorative hues to create artificial variety.

## Typography

**Display and Body Font:** Public Sans with Arial and system sans-serif fallbacks.

**Label and Code Font:** Martian Mono with Cascadia Mono and Liberation Mono fallbacks.

**Character:** Public Sans gives the prose a sturdy manual quality. Martian Mono identifies source, commands, labels, and system data without turning the whole site into a simulated terminal.

### Hierarchy

- **Display** (700, fluid to 5.75rem, 0.94 line-height): frontpage proposition only.
- **Headline** (700, fluid to 4.75rem, 1 line-height): major section claims.
- **Title** (700, fluid to 2.75rem): feature and diagram titles.
- **Body** (400, 1rem, 1.6 line-height): explanatory prose, normally limited to 65-75 characters.
- **Label** (600, 0.6875rem, compact line-height): commands, navigation, status, and technical metadata.

**The Source Has Priority Rule.** Source stays large enough to read and never becomes a faint background texture.

## Elevation

The system uses no shadows. Tonal layers, spacing, and one solid border establish structure. Code specimens sit on a near-black material rather than floating over the page.

**The Flat By Default Rule.** Depth indicates structure or interaction, never decoration.

## Components

### Buttons

- **Shape:** practical corners (4px) and a minimum height of 48px.
- **Primary:** deep moss with white text; used for documentation and status routes.
- **Light:** white with deep moss text; used only on the moss hero.
- **Hover:** a small upward transform and a higher-contrast background.
- **Focus:** a visible yellow-green outline with a clear offset.

### Status Labels

- **Style:** compact mono text, a full 1px border, and a 4px radius.
- **Rule:** the text states the status. Colour only supports the label.

### Source Windows

- **Corner Style:** defined but restrained (8px).
- **Background:** near-black with pale code text and a darker structural rule.
- **Header:** filename on the left and design status on the right.
- **Overflow:** horizontal scrolling preserves source without wrapping or clipping it.

### Architecture and Presentation Maps

- **Structure:** labelled nodes connected with real rules, not a decorative grid.
- **Responsive behaviour:** multi-column flows become one readable column below 832px.
- **Status:** captions identify whether a diagram is a locked model, proposal, or implementation view.

### Navigation

- **Style:** white project header, text links, and a visible brand mark.
- **Mobile:** links wrap in place. There is no hidden menu or required JavaScript.
- **States:** underline on hover and the global yellow-green focus outline.

Motion uses one short hero entrance and direct state transitions. All movement uses transforms. `prefers-reduced-motion` reduces animation and transition durations to an immediate state.

## Do's and Don'ts

### Do:

- **Do** put valid Orna syntax and copyable commands near the top of the page.
- **Do** show CLIENT, SERVER, runtime, protocol, and security boundaries as labelled structures.
- **Do** use semantic HTML, visible focus, and text labels for every status.
- **Do** keep the documentation visually quieter than the marketing frontpage.
- **Do** state plainly when a feature is locked design, current proposal, open question, or implemented work.

### Don't:

- **Don't** build a generic SaaS landing page with conversion funnels, testimonial strips, pricing cards, or inflated claims.
- **Don't** make a glossy AI database site with gradients, abstract blobs, or vague promises.
- **Don't** write a beginner SQL tutorial for an expert audience.
- **Don't** use terminal-themed decoration as a substitute for technical substance.
- **Don't** present a current proposal or roadmap item as a released feature.
- **Don't** use gradient text, glass cards, decorative grids, repeated eyebrow labels, coloured side-stripe borders, or ambient shadows.
- **Don't** round cards, sections, or inputs beyond 8px.
