# Work ADR 0116: Playground as a Database-Hosted Application

**Status:** Binding architecture for S2; execution-session work is S4 and generic renderer work is S5

## Decision

The Orna playground is an ordinary application in the served clone. Its Orna
modules, page functions, tables, sample rows, and static assets are reachable
from the database's Git-backed source and records. `orna serve` serves the
application from that same clone and uses the existing in-process Orna runtime
to evaluate explicit playground actions.

The playground is a page in the `std.devtools` application, at
`/playground`. The root mounted by S1 remains `/` and retains its Git-backed
repository views; the ordinary `std.devtools` home leads with database tables
as ORNA-SERVE-009 requires and provides a plain link to the playground page.
Both routes use the same `orna serve` process, clone, CWD, page/query
endpoints, and `orna.present.v1` transport. This does not add a second
static-site host or make the browser a second Orna implementation.

This decision binds the record layout, callable page surface, execution
boundary, renderer inputs, and crate direction below. S4 implements sessions
and in-DB execution against this boundary. S5 implements the generic
introspection renderer against the same boundary. Neither slice needs a new
application manifest or a new language execution model.

## Record model

Application membership follows the ordinary module graph and reachable
records. There is no `CREATE APPLICATION` declaration, application manifest,
or separate Git namespace for the playground. `std.devtools` is the module
namespace; its public functions and pages are its callable surface.

The committed program is ordinary `.orna` source in the `std.devtools`
namespace. Its root module supplies the table-first landing page and simple
links. A playground module supplies the `/playground` page, typed page actions,
and functions used by those actions. Reachability from the root module
controls which declarations are part of the application. Adding an unrelated
`.orna` file does not add it to the running application.

The application owns ordinary tables for its editable content:

| Logical record | Required fields | Purpose |
|---|---|---|
| `std.devtools.Sample` | stable key, display name, source text | Starter programs and user-visible examples. Source remains plain Orna text and is evaluated only after an explicit Run action. |
| `std.devtools.Asset` | normalized relative path, media type, Base64-encoded bytes | HTML shell, small theme CSS, generated editor artifacts, and the shared presentation-protocol client needed by the browser renderer. The committed row stores standard padded Base64 text; `orna serve` decodes the bytes when it serves the asset. |

These are regular Orna table declarations and rows. Their schema declarations,
source modules, and loose row files participate in ordinary Git history and
diffs. The existing editable/compact storage mapping remains transparent to
the application; no frontend code depends on physical row placement. The
asset key is unique within the application, rejects traversal or ambiguous
paths, and is resolved through the asset table rather than a separately
checked-in public directory. Git object identity and repository verification
remain authoritative for committed bytes.

A small first version can ship with just the page module, one sample table,
and the asset rows needed to render and submit a page action. The record model
does not require an exhaustive developer console or an up-front catalogue of
all possible views.

## Page and function surface

`/` continues to lead with database tables, then links to repository and
runtime views already supplied by S1. The playground is a sibling page, not a
replacement for the S1 Git listing. Its page function returns an ordinary
`Page` value at `/playground`; its view callback constructs a renderer-neutral
presentation tree from the current typed input, selected sample, execution
result, and available database/runtime metadata.

The first page surface consists of:

* a source input and a small list of sample rows;
* an explicit Run action and a result view for values, standard output, or
  located diagnostics;
* an optional persistent REPL input backed by the same session contract;
* plain links back to `/` and to any exposed files, commits, branches,
  functions, dependencies, storage, or runtime state.

Page actions call ordinary Orna functions. The browser submits typed input
through the server-issued page action handle; a text value is not executable
merely because it appears in a page or protocol message. A full-buffer Run
starts a fresh evaluator session, preserving the existing `run(source)`
expectation that independent runs do not inherit bindings. REPL input uses a
named live session and keeps that session's imports, bindings, and declared
helper functions according to the remote REPL model. Both paths parse,
resolve, type-check, and execute on the server using the normal evaluator and
activation transaction rules. The browser does not send an authoritative AST,
bytecode, query plan, or language metadata.

The result is a typed application value containing rendered values, captured
stdout, and structured diagnostics with source locations. The server builds
this result from evaluator values and diagnostics, then the ordinary
presentation and query response paths encode it. A compatibility `run(source)
-> JSON` adapter may serialize this typed result, but JSON is an output
encoding, not a parallel evaluator contract. Database writes use the served
clone's CWD and normal transaction semantics. External effects retain their
normal delivery and recovery limits.

## Execution sessions and runtime state

S4 uses the existing evaluator and page-action machinery in the server
process. Each browser REPL has a server-issued session identity, isolated from
other sessions. The full-buffer Run operation uses a new session for each
request. The live REPL operation retains its module environment while that
session is active. Input is admitted through the ordinary source, type,
resource, and effect checks before activation.

Session identity, pending multiline input, admitted bindings, request
fingerprints, and compact terminal outcomes are local runtime state under the
clone's `.git/orna/` administrative directory (in the embedded runtime state
store), never committed application rows. Rebuilding caches or refreshing the
browser cannot change the committed HEAD or turn session state into source.
Session lifetimes and retained payloads are bounded; reconnecting obtains a
fresh page snapshot and either resumes the server-owned session where it is
still available or opens a new one. A missing session is an explicit new
session outcome, not fabricated prior state.

Mutating actions and explicit evaluation use a 128-bit request identity bound
to a canonical operation fingerprint. Repeated delivery with the same
identity and operation returns its recorded terminal outcome; reuse with a
different operation is rejected. This deduplicates Orna-controlled writes
only. Page action handles are server-issued and revision-bound; stale or
unknown handles do not run. A successful activation commits its Orna writes
together; an escaping error or cancellation rolls them back. The host does not
claim transactional guarantees for external effects.

Execution sessions follow the clone's existing trusted-host model. This
architecture adds no principals, roles, grants, or row-level authorization.
An application action does not acquire administrative authority merely
because it is reachable from a page.

## Introspection-built presentation and browser behavior

Page functions construct renderer-neutral Present trees from introspected
values and one coherent database/runtime observation. The web renderer maps
known generic nodes and typed tables to semantic HTML. It uses stable field,
row-key, or explicit child identities for updates and applies ordered
`orna.present.v1` deltas. If a fine-grained patch is unavailable, it replaces
the nearest stable subtree; if a presenter fails or the renderer encounters
an unknown rich node, it displays the Inspect-compatible fallback. The
terminal and browser renderers consume the same tree rather than maintaining
separate page schemas.

The first view is deliberately plain: readable content, ordinary blue links,
small amounts of CSS, and a few CSS custom properties for background, text,
link, and border colors. It does not impose a dashboard layout, decorative
cards, animation, or a custom navigation system. The same properties let a
site stylesheet change the theme. The renderer starts with basic text, table,
code, form, button, and Inspect nodes and grows only when an ordinary
presentation value needs a view.

Application logic stays in Orna functions and page actions. Browser
JavaScript is limited to the shared generic renderer and protocol client:
submitting typed page actions, subscribing/resubscribing to watches, applying
presentation deltas, and requesting a complete snapshot when revisions do
not match. It does not implement Orna evaluation, application navigation,
query planning, or custom sample behavior.

No playground application, renderer, or browser adapter may maintain its own
syntax, token, keyword, scalar-type, or operator inventory. Editor metadata
consumed by the page comes from the checked-in generated artifacts under
`editors/` (or the byte-identical artifact row in `std.devtools.Asset`),
emitted and drift-checked by the existing `orna-syntax-v1` generator. A page
may initially show plain source text; if it highlights source, it consumes
those generated artifacts. The renderer must not add a second vocabulary or
fallback keyword list.

## Crate and asset direction

The production browser path replaces `playground/orna-wasm` with
Git-backed `std.devtools` source and asset rows. The WASM crate is not moved
into `crates/`: its purpose is to run a second evaluator in the browser, while
this architecture requires all execution to use the served clone's normal
in-process evaluator, CWD, activation, and session state. Moving that crate
would preserve the wrong execution boundary. The existing syntax, evaluator,
application, live-session, and serving crates remain the reusable Rust
implementation layers; the app itself is Orna records plus browser assets in
the database.

After the in-DB page is wired, S4/S5 retire the WASM package and generated
WASM bundle from the served playground path. Only small generic client assets
remain browser code, and they are ordinary Git-backed rows. Existing native
parity examples may remain as test fixtures for evaluator behavior; they do
not define a client runtime. If future work needs a separate WASM target for a
different product, it requires a new decision and cannot be the playground's
execution path by default.

## Normative basis and reference mapping

This architecture implements the following frozen Orna 1.0.0 boundaries:

* `ORNA-SERVE-001`: `orna serve` supplies Git transport, page/query endpoints,
  and WebSocket presentation deltas for the current clone.
* `ORNA-SERVE-008` and `ORNA-SERVE-009`: the default frontend is an ordinary
  `std.devtools`-line Orna application, leads with tables, and exposes files,
  commits, branches, functions, dependencies, storage, and runtime state.
* Applications and renderers: an application is a reachable collection of
  functions, pages, tables, and assets; it needs no manifest. Presentation
  trees are renderer-neutral and unknown nodes retain Inspect-compatible
  output.
* `ORNA-PAGE-001/002`, `ORNA-LIVE-001..004`, `ORNA-WIRE-001..008`, and
  `ORNA-EVAL-001..011`: pages and widgets are values returned by functions;
  watches and actions use typed snapshots, revisioned deltas, explicit
  evaluation, and request identities.
* `ORNA-REPO-001..004`, `ORNA-STATE-001..004`, and `ORNA-LOCAL-001..004`:
  reachable modules define the program; CWD is distinct from HEAD; uncommitted
  runtime state and rebuildable caches stay in the per-worktree administrative
  area.
* `ORNA-STORAGE-001..004` and `ORNA-PUB-005..012`: table reads are independent
  of physical placement, editable row changes remain Git-reviewable, and
  runtime publication does not sweep unrelated staged or working-tree edits
  into an application commit.
* `ORNA-PRES-002`, `ORNA-PRES-006`, `ORNA-PRES-008..010`, and
  `ORNA-SECRET-002/004`: inspection remains bounded and redacted, presentation
  stays deterministic and read-only, and unknown or failed renderings retain
  Inspect-compatible output without revealing secret values.

The request names `source/24-application.md` and `source/23-repository.md`.
Those paths do not exist in the frozen reference tree available for this
branch. The same concepts are published there as `source/03-source-modules.md`
and `source/14-pages.md` (modules and pages), `source/19-repository.md`
(repository and CWD), `source/22-publication.md`, `source/23-storage.md`,
`source/24-git-history.md`, and `source/28-serving.md`. This record cites the
current frozen requirement IDs and chapter names rather than treating a
renamed chapter as a new or changed contract.

## Consequences and precedence

The default development UI remains an Orna application served from the
selected clone. S1's root listing remains useful without WASM, and the
playground can be checked out, committed, merged, reviewed, or replaced using
ordinary database and Git operations. S4 may add only the runtime session
admission/storage needed to honor this contract. S5 may add only generic
renderers for self-describing presentation values and their Inspect fallback.

This work ADR chooses the implementation boundary for the playground; it does
not amend Orna 1.0.0 semantics. The frozen reference is authoritative if an
implementation detail recorded here conflicts with it.
