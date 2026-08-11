<!-- SEED: re-run $impeccable document once there is code to capture the actual tokens and components. -->

---
name: OrnaDB website
description: A technical systems manual with a clear, inspectable product argument.
---

# Design System: OrnaDB Website

## Overview

**Creative North Star: "The Executable Systems Manual"**

The site combines the directness of the PipeWire and Wayland project pages with the stronger product demonstration used by Handy. It should feel maintained, specific, and close to source code. The frontpage can carry a distinct identity, but it must still read as the first page of technical documentation.

Use a committed colour field, flat surfaces, real Orna syntax, and visible system structure. Reject generic SaaS landing pages, glossy AI database sites, decorative terminal themes, and editorial-magazine styling.

**Key Characteristics:**

- technical content before promotional claims;
- a light reading surface for long documentation;
- one committed moss-green brand field against black and white;
- code and architecture used as the primary imagery;
- restrained motion and square, practical geometry.

## Colors

Use a committed strategy. A moss-green primary carries the hero and major structural moments. Pure white supports long-form reading. Near-black carries code and high-contrast text. A sharp yellow-green accent can identify status or active paths, but it must not become decoration.

**The Status Is Text Rule.** Every status colour must have a written label such as `IMPLEMENTED`, `LOCKED`, `PROPOSAL`, or `OPEN`.

## Typography

Use a sturdy humanist sans for prose and a clearly different technical mono for code, commands, labels, and data. The type should resemble a systems manual, not a simulated terminal.

Keep body lines between 65 and 75 characters. Display headings can be strong but must remain below 6rem and use letter spacing no tighter than -0.04em.

**The Source Has Priority Rule.** Code must stay large enough to read and must never become a faint background texture.

## Elevation

Use flat tonal layers and solid rules. Do not use ambient drop shadows. A surface can separate from its background through colour, spacing, or one defined border.

**The Flat By Default Rule.** Depth indicates structure or interaction, never decoration.

## Components

Buttons, code specimens, navigation, status labels, and architecture nodes should use practical geometry with small corner radii. Interactive states need clear hover and focus treatments. Cards are permitted only when the content is a true independent object; use tables, ruled lists, and open layouts for most feature content.

Motion is restrained. Use one short page-load sequence and direct state transitions. Disable non-essential motion when `prefers-reduced-motion` is active.

## Do's and Don'ts

### Do:

- **Do** put valid Orna syntax and copyable commands near the top of the page.
- **Do** show CLIENT, SERVER, runtime, and security boundaries as labelled structures.
- **Do** use semantic HTML, visible focus, and text labels for every status.
- **Do** keep the documentation visually quieter than the marketing frontpage.

### Don't:

- **Don't** build a generic SaaS landing page with conversion funnels, testimonial strips, pricing cards, or inflated claims.
- **Don't** make a glossy AI database site with gradients, abstract blobs, or vague promises.
- **Don't** write a beginner SQL tutorial for an expert audience.
- **Don't** use terminal-themed decoration as a substitute for technical substance.
- **Don't** present a current proposal or roadmap item as a released feature.
- **Don't** use gradient text, glass cards, decorative grids, repeated eyebrow labels, or coloured side-stripe borders.
