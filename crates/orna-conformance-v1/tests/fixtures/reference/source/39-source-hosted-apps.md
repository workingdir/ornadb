# 34. Source-hosted applications and minimal devtools {#source-apps}


## Scope first

Devtools, the Git viewer and playground are one optional application. The
first usable milestone is a table list, one input, Run, a typed result and an
explicit refresh. No Watch UI, saved-query store, theme engine, WPF object
system or new frontend framework is part of the initial acceptance boundary.
Existing language watches and live-protocol contracts remain available and
are not deleted merely because the first UI does not expose them.

**ORNA-APP-001** Devtools MUST be ordinary selected Orna source, not a privileged native Git homepage or separate playground product.
**ORNA-APP-002** Browse and console results MUST use shared typed inspection. Run MUST call the real parser/resolver/evaluator; simulated results are not completion.
**ORNA-APP-003** Portable UI meaning MUST not depend on HTML, CSS or DOM. The web renderer SHOULD use native semantic controls and absent/minimal optional CSS.
**ORNA-APP-004** The selected application source/resources MUST be the actual executed inputs; removal MUST not trigger hidden reinstallation or native fallback.
**ORNA-APP-005** Pinned `std` submodules MUST retain normal Git semantics. Editing a dependency requires its own commit then a parent gitlink update, or an explicit local vendor/fork; no silent mutable overlay.
**ORNA-APP-006** CSS, browser host bindings and Wasm outputs MUST belong to renderer-package resources or ordinary Blob fields. No compulsory File/Asset registry or `std.devtools.Asset` primitive is permitted.
**ORNA-APP-007** Package resources MUST resolve in the caller's admitted dependency/source snapshot. Host paths, newest installed libraries and mutable remote URLs cannot replace those inputs silently.
**ORNA-APP-008** Builds MUST bind source, dependencies, resources, toolchain and host ABI coherently; offline claims require complete dependency closure.

## Serving and native boundary

The default local `devtools/` source may be copied from a pinned distribution
while `stdlib/std` stays a submodule. This gives editable default application
code without pretending parent Git commits include dirty submodule files.

An explicit root `pub fn serve()` is the source-hosted entry selected by
`orna serve`. It runs as a supervised service, not the root `main()` or one
long database write transaction. Routing/HTML/response composition are Orna
library behaviour over public bounded socket/evaluator/transaction interfaces.
Missing entry/library support is a diagnostic, not a guessed code path.

**ORNA-APP-009** Source-hosted conformance MUST put routing, HTML generation, asset serving and application orchestration in replaceable Orna code; a wrapper around a complete native server fails this claim.
**ORNA-APP-010** CLI serving MUST invoke only the selected serving entry and retain loopback/trusted-perimeter defaults; full Server conformance still requires the existing exact protocol endpoints.
**ORNA-APP-011** Each mutation MUST use shared short activation/request transactions; listeners and browser waits are supervised lifetimes, not open database transactions.
**ORNA-APP-012** The first web implementation MAY use request/response HTML and forms. It MUST bound input, enforce origin/CSRF/session checks, and never blindly replay an uncertain mutation. This is not a full live-protocol claim.
**ORNA-APP-013** Blob summaries MUST be metadata-only. Explicit media/content responses MUST check authority and snapshot identity; inspection of HTML/SVG/JS MUST not execute it.
**ORNA-APP-014** Media responses SHOULD support RFC 9110 HEAD, single byte ranges and immutable validators through the shared Blob reader, with correct 206/416 and truncated-stream failure behaviour.

## Orna-authored browser profile (after the minimum)

**ORNA-APP-015** The later browser profile MUST run protocol/control/rendering code authored in Orna through a Wasm build of the shared evaluator initially; a direct compiler may replace it with equivalent semantics later.
**ORNA-APP-016** Generated JS MAY bind browser APIs only; no app-specific Git/table logic or second Orna language implementation. Server handles/closures MUST NOT become client authority.
**ORNA-APP-017** Page updates MUST use explicit immutable-model transitions. The console's scratch bindings MUST NOT overwrite the page's pinned application environment.
**ORNA-APP-018** Focus/caret/unsent drafts/local feedback MUST remain responsive and not be overwritten by unrelated server results.
**ORNA-APP-019** Connection resume, durable request recovery and page-state restoration MUST be separate promises; expired sessions MUST NOT replay mutation transcripts.
**ORNA-APP-020** Source/runtime upgrades MUST switch coherent generations for new sessions; old actions MUST NOT silently acquire new meanings. Invalid app source MUST leave local core recovery usable.
**ORNA-APP-021** Replaceability MUST be demonstrated by a second ordinary application and an independent minimal Orna renderer/HTTP composition using only public interfaces.
**ORNA-APP-022** The minimum demonstration MUST use actual row/Blob storage: import, query, retrieve/play, metadata edit, historical read and complete offline reopen. A mock HTML page is not engine evidence.

No CSS-to-terminal translation exists. Web CSS lives in a selected package;
text/native renderers consume the same typed intent and use their own platform
rendering. A view or mounted path is not a permission boundary.
