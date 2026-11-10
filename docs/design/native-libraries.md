# Native libraries: `native fn` in any module, bound at link

Status: design proposal for owner review, recorded 2026-11-10. It amends D12 in
`docs/language/DECIDED.md` (section "Amendment 2026-11-10") and supersedes
`native-declarations.md` Q5 and the admission clauses ORNA-NATIVE-004 and
ORNA-NATIVE-012 in `docs/spec-extensions/NATIVE-1.md`. Changes no code. Items
marked [PROPOSAL] are this document's recommendation; items marked [OWNER] are
decisions the owner has not yet made. [INFERENCE] marks a claim not checked
against code or a test.

## 1. The rule

The owner's direction, verbatim: "any .orna should be able to use native
functions, you just gotta load the .so with the declaration".

Rules the amendment adds:

| Rule | Statement |
|---|---|
| R1 | A body-less `native fn` MAY appear in any module unit. The parser already accepts it there (`crates/orna-syntax/src/parser.rs:2816`); only admission changes. |
| R2 | Every native declaration is bound exactly once, at link, before evaluation. Its key is `(declaring module path, declared name)`. |
| R3 | A key in the engine-embedded modules `core` and `sys` is bound from the engine registry (`ENGINE_NATIVE_ABI`, today `1`). |
| R4 | Any other key is bound from a shared library that the root project names in its manifest (section 3). [PROPOSAL] Only the root project may name a library. A dependency pinned in Git may declare natives but cannot supply their code. [OWNER] |
| R5 | The `effect` clause stays mandatory on every native (ORNA-NATIVE-009). For a library native it is a claim the engine cannot check (section 5). |
| R6 | A declaration no registry entry and no library claims is a link error. It is never a run-time failure. |

Before and after:

| Question | Before (D12, 2026-11-10 earlier) | After (amendment) |
|---|---|---|
| Where may `native fn` appear? | only `core.orna` and `sys.orna` (ORNA-NATIVE-004, 012) | any module unit |
| Who supplies the code? | the engine, always | the engine for `core`/`sys`; a named shared library otherwise |
| Can a package ship native code? | no (D11, now superseded) | the package may declare it; only the root project can supply its library [PROPOSAL] |
| When is a missing implementation found? | not possible: no user natives | at link, before evaluation, one error per cause |

Nothing here changes the `core` and `sys` registry, `NativeAbi`, or the effect
vocabulary (`sys.EffectKind`, twelve values plus `none`; `crates/orna-check/src/effects.rs:11`).

The three current texts disagree on the admitted set, and this amendment supersedes
all three: `docs/spec-extensions/NATIVE-1.md` ORNA-NATIVE-004 and -012 (a PROPOSED
amendment, bead `ornadb-yuzot`) admit only the pinned `std` module; the code
(`lower/declare.rs`) admits only the embedded `core` and `sys` modules; reference
chapter 01 says "only in engine-embedded modules".

## 2. Where a declaration may appear

Options:

- **A. Any module unit (owner's rule; [PROPOSAL]).** A native is a normal public
  declaration in the module that uses it. For: no second place to look; the
  signature sits next to its callers; matches the owner's words. Against: every
  module can ask for host code, so the reviewer must read the manifest to know
  what a module can reach. The manifest and the digest are the only gate.
- **B. Project modules only; `std` and dependencies excluded.** For: keeps
  pinned, reviewed `std` free of host code. Against: a dependency cannot
  provide a native function to its users unless the root project binds it
  anyway, which is the same work as option A with an extra rule to test.
- **C. A single designated module per project (for example `native.orna`).**
  For: one place to audit. Against: a second convention the owner did not ask
  for; the declaration is still bound by the manifest.

The prior option in `DECISIONS.md` (D12 option C, natives in any pinned package)
was rejected for 1.x. Option A here is narrower: a package declares, and only the
root project supplies code (R4).

Consequence for lowering: the check that rejects non-embedded modules
(`crates/orna-hir/src/lower/declare.rs:276-290`, message "admitted only in the
engine-embedded core and sys modules") is removed under A. ORNA-NS-005 reserves
only `sys` and `std` today, so `core` is not reserved by the spec. [PROPOSAL]
The engine reserves the module paths `core` and `sys` for registry natives, so a
project module cannot claim the registry key space. [OWNER] Whether this is a
new reservation under the 1.2 reference or a rule in the engine only.

## 3. How a project names a library

Fact: the corpus has no project manifest file today. A dependency is a Git
commit and nothing in source names it (`DECISIONS.md`, "Package and version
declarations"). This section therefore introduces one. The file name `orna.toml`
is a placeholder [OWNER].

Options for naming a library:

- **A. Manifest table in the project root (proposed).** The code, the module
  list and the digest sit in one reviewable file, in Git, versioned with the
  source. For: no grammar change; a reader sees every host binding in one place;
  a digest change is a diff. Against: the binding is far from the declaration,
  so a reader of `text.orna` must look elsewhere to know a native is bound.
- **B. Declaration annotation, for example `native("slug") fn slugify(...)`.**
  For: the binding is beside the declaration. Against: a new grammar form and
  a new reserved shape (ORNA-NATIVE-002 would need an amendment); the digest
  still has to live somewhere, so the manifest exists anyway.
- **C. Operator configuration outside the repository.** For: no binary in Git.
  Against: a program cannot be reproduced from its Git commit alone, which
  breaks the database-for-programs principle (DECIDED.md, principle 3). Rejected.

Proposed manifest (format TOML [OWNER]; the corpus names no project manifest, and
whether the engine can read TOML was not checked [INFERENCE]):

```toml
# orna.toml at the project root (name and format are placeholders)
[[native_library]]
name = "slug"
modules = ["text"]        # logical module paths of the declaring modules this library serves
abi = 1                   # LIBRARY_NATIVE_ABI the library was built against [PROPOSAL: 1]

[native_library.target.x86_64-unknown-linux-gnu]
path = "native/x86_64-unknown-linux-gnu/libslug.so"
sha256 = "<64 lowercase hex digits>"

[native_library.target.aarch64-apple-darwin]
path = "native/aarch64-apple-darwin/libslug.dylib"
sha256 = "<64 lowercase hex digits>"
```

Rules for `path`:

- Relative to the project root; no absolute path; no `..` component after
  normalisation.
- Must name a regular file inside the Git tree of the commit being linked. A
  symlink, a file outside the tree, or an ignored file is rejected (ORNA-STD-NATIVE-LIBRARY).
- The path is per target triple. The engine selects the entry for its own build
  target. There is no fallback to another platform's file.

Rules for `sha256`:

- The digest is SHA-256 of the file bytes exactly as stored in Git. No
  normalisation, no strip, no signing.
- [PROPOSAL] SHA-256 rather than the Git blob id. Git blob ids depend on the
  repository's object format (SHA-1 or SHA-256) and hash a header as well as
  the content. A content digest is format-independent and easy to check with
  `sha256sum`. Trade-off: two identities for one file (the blob id and the
  digest) when both are present in Git.
- The digest pins bytes, not behaviour. Two builds of the same source produce
  different bytes unless the build is reproducible, so the digest says "this
  binary", never "this source".

Binding rule: a library serves every declaration whose module path is listed in
`modules`. A declaration whose module is listed by two libraries is a load error
(ORNA-STD-NATIVE-DUPLICATE). A library that lists a module with no native
declarations is accepted and reported as unused [PROPOSAL].

## 4. The C ABI and the version numbers

Two ABIs exist after the amendment and must not be confused.

| ABI | Number | Carries | Used by |
|---|---|---|---|
| Engine native ABI (`NativeAbi`) | `ENGINE_NATIVE_ABI = 1` (`crates/orna-hir/src/embedded.rs:17`) | in-process Rust function pointers `NativeFn` (specified in hir.md 4.3; not in code at `a4cf733b1`) | `core` and `sys` registry natives |
| Library native ABI | `LIBRARY_NATIVE_ABI = 1` [PROPOSAL] | a C calling convention over canonical bytes (below) | shared-library natives |

Why a separate number: the registry ABI is the engine's internal function-pointer
contract and can change with the engine. The library ABI is a public contract
with code built outside the engine; it must change on its own schedule.
Trade-off: two numbers to keep in step with documentation. [OWNER]

Library exports (C, `extern "C"`), proposed:

```c
uint32_t orna_library_abi(void);                 /* must equal LIBRARY_NATIVE_ABI */
int32_t  orna_<key>(const uint8_t *args, size_t args_len,
                    const OrnaContext *ctx, OrnaResult *out);
```

- `<key>` is the module path segments and the name joined by `__`, for example
  `orna_text__slugify`. A name outside `[A-Za-z0-9_]` is rejected at lowering
  (the mangling needs an ASCII identifier; [INFERENCE: identifier grammar not
  re-checked here]).
- `args` is the canonical encoding of the argument tuple, after the one binder
  has ordered and defaulted it (hir.md 2.7). No library re-implements argument
  matching.
- `out` is written by the engine through a callback in `OrnaResult`; the library
  never frees engine memory and the engine never frees library memory.
- The return value is `0` for success, with a canonical result in `out`. A nonzero
  value is a failure: the library writes a typed `Error` code and message, and the
  engine raises it in the ordinary failure channel (ORNA-LIB-005).
- `OrnaContext` carries: a cancellation flag the library must poll; the quota
  remaining for result bytes; and, only when the declared effect asks for them,
  the activation's fixed `now` (ORNA-TXN-005) and an entropy callback (section 6).

Alternatives:

- **B. Typed scalar C signatures** (`int64_t` for `Int`, `char*` for `Str`). For:
  trivially readable from C; no codec. Against: `Int` is arbitrary precision (D7),
  so `i64` is wrong; `Relation`, `Map` and `Blob` have no scalar form; one
  signature per type shape.
- **C. Rust-ABI dynamic library.** Rejected: the Rust ABI is not stable across
  compiler versions, so a binary could not be pinned by digest and expected to
  work.
- **D. Out-of-process helper** (argv execution, canonical bytes over stdin and
  stdout). Not the owner's wording ("load the .so"), but it is the pattern the
  corpus already names for native code: ORNA-MEDIA-005 requires bounded time and
  memory, cancellation, argument-vector execution and protected temporary
  resources (`reference/Orna-1.1.0/source/38-ingestion-inspection.md:50`). For: a
  hard time bound (kill), a memory limit, and a crash that cannot take down the
  engine. Against: a process start per call unless the helper is persistent;
  a second lifecycle to manage. [OWNER] See section 6 for the conflict this creates.

## 5. Effect enforcement

Fact: the engine cannot inspect machine code. A declared `effect` on a library
native is a statement by whoever wrote the manifest and the library. The engine
can check the statement's shape, not its truth.

What the engine can gate, and what it cannot:

| Gate | Can the engine enforce it? | Mechanism |
|---|---|---|
| Callers must have the declared effects (D2) | yes, at check time | the declared set joins the call site's inferred set |
| The library receives only the capabilities its effect class allows | yes, by construction | `OrnaContext` has a `now` only for `clock`, an entropy callback only for `randomness`, no store handle for `table_read`/`table_write` |
| The library does not reach the network, filesystem or clock itself | no | the library runs in the host process with the host's authority (ORNA-EXT-005) |
| The library does not loop forever | no, in process | cancellation is cooperative (the flag is polled) |

Options:

- **E1. Claim plus label (proposed for in-process libraries).** Trust the
  declaration; show every library native with the label "native, host trust, not
  sandboxed" in tools, as ORNA-EXT-005 requires. For: matches the corpus's own trust model. Against: a
  wrong `effect none` is silent; the reader must trust the author.
- **E2. Out-of-process with OS restrictions** (seccomp on Linux, `sandbox-exec`
  on macOS, job objects on Windows). For: real enforcement of the network and
  filesystem claim. Against: three platform mechanisms to build, test and keep
  working; none is in the workspace today [INFERENCE: not searched].
- **E3. Restrict library natives to `effect none`.** For: removes the clock,
  randomness and host-effect claims, so the determinism burden is smallest.
  Against: a library cannot then read the clock or randomness through the engine
  either, so it must be given inputs as arguments. Trade-off: a stronger rule
  with less capability. [OWNER]

The engine MUST show the declared effect of a library native in every tool that
shows native effects (hover, `orna` status, documentation), and MUST say that the
effect is a claim. That label is part of the design, not a footnote.

## 6. Determinism and replay

Facts the design must respect:

- Canonical output is the product. The browser parity gate
  (`.github/workflows/browser-profile-wasm-parity.yml`, section 7) compares the
  canonical output of the native and Wasm builds. A native that returns different
  bytes for the same arguments breaks replay and that gate.
- `parallel` children are separate activations and transactions (ORNA-CONCUR-003).
  Whether they run on threads was not checked [INFERENCE]; rule 4 serialises calls
  into one library regardless.

Rules, proposed:

1. A native with `effect none` MUST be a pure function of its arguments. The
   engine cannot check it (section 5). Its result MUST NOT depend on process
   state, environment, locale, time or address-space layout.
2. A library MUST NOT keep state that changes results between calls. Caches are
   allowed only if they do not change results.
3. Clock and randomness enter only through `OrnaContext`, and only when the
   declared effect asks for them. The engine records nothing else; replay of an
   effectful native uses the recorded arguments and the fixed `now` (ORNA-TXN-005).
4. Libraries MUST be re-entrant. [PROPOSAL] The engine serialises calls into one library unless the manifest
   states otherwise. Trade-off: serial calls cost throughput under `parallel`.
5. Floating point: a library's floating-point results depend on the platform's
   math library. The same source built for two targets can return different
   bits, and two targets have two digests, so a project that runs on both targets
   can produce two canonical outputs. [OWNER] Either forbid `Float` in library
   signatures, or require bit-exact results and accept the cost of checking it.

Cancellation and time bounds: in process, a library that does not poll the
cancellation flag cannot be stopped. ORNA-EXT-004 says implementations SHOULD
enforce coarse budgets and cancellation; ORNA-MEDIA-005 says native libraries
MUST use bounded time and memory. The in-process option cannot meet the MUST
for time. This is the clearest conflict in the amendment and is the main reason
option D (section 4) is listed. [OWNER] Decide whether the owner's "load the .so"
(in process) is accepted for a clause the corpus states as a MUST, or whether
library natives run out of process.

## 7. The Wasm browser profile

Fact: a `wasm32-unknown-unknown` build cannot load a shared library. The browser
profile's host has no filesystem, network or process I/O. hir.md 4.4 specifies
`io() = None` for the browser build; the `Host` trait is not in code yet, and
`crates/orna-evaluator/src/browser_profile.rs` has no `io()` (grep at `a4cf733b1`).

Options:

- **P1. Reject at link [PROPOSAL].** Under the browser profile, a program whose
  link set contains any library native fails before evaluation with
  ORNA-STD-NATIVE-PROFILE, naming the declaration and the library. For: a
  program's admissibility does not depend on the profile it runs in at run time;
  the failure is at the same point as every other link error. Against: a
  browser user cannot run a project that uses one library native, even if the
  native is not on the path they exercise.
- **P2. Unavailable at call.** The native raises a runtime error only when called.
  Rejected: it is the deferred failure the native-declarations design forbids
  (ORNA-NATIVE-006 spirit: "no placeholder and no deferred failure").
- **P3. Ship a Wasm module per library**, digest per `wasm32` target. Possible later;
  out of scope here because it needs a second artifact and a second digest per
  library.

Registry natives: the browser build must implement every `core` and `sys` native
its link set needs. A missing one gets the same ORNA-STD-NATIVE-PROFILE error.

Gate: `.github/workflows/browser-profile-wasm-parity.yml` runs the ignored test
`browser_profile_wasm_matches_native_canonical_output`
(`crates/orna-application/tests/browser_profile_wasm_parity.rs`) against one
fixture. [PROPOSAL] Add a second fixture that declares a library native and
asserts that the Wasm build rejects it with ORNA-STD-NATIVE-PROFILE while the
native build runs it. This document does not change the workflow.

## 8. Security and capability gating

Fact: a native extension runs with the host process's trust and MUST be labelled
as such; Orna does not sandbox it (ORNA-EXT-005, `26-security.md:23`). A library
native is one.

Threats and controls:

- **Pulled code.** A project lives in Git. A pull can change a manifest digest
  and a binary together. Review of the digest change does not show what the
  binary does. Control: the binary is a reviewed artifact in the same diff; the
  label in tools shows the library name and digest at every link.
- **Dependency code.** A pinned dependency can declare natives (R4). Control:
  only the root project names libraries, so a dependency cannot bring its own
  host code without a root manifest change. [OWNER] Option to let dependencies
  name libraries too; that moves trust into every pinned package.
- **Digest is not trust.** A matching digest proves the file is the one the
  manifest names. It does not prove the file is safe. Options:
  - **G1. Digest in repository only** [PROPOSAL minimum]. For: reproducible from
    Git. Against: any commit that changes both the manifest and the binary is
    accepted without a second party.
  - **G2. Operator allowlist of digests**, kept outside the repository (for
    example a per-user file of trusted SHA-256 values). For: an operator decides
    which binaries run on their machine. Against: a second source of truth; a
    fresh machine refuses a project until the operator adds the digest; CI needs
    the same file.
  - **G3. Interactive prompt on first load.** Rejected for default: breaks
    headless runs and CI, and cannot be replayed.
- **Load-time code runs before any symbol check.** Loading a shared library runs
  its initialisers (`.init_array` on ELF, `__mod_init_func` on Mach-O, `DllMain` on
  Windows). So: check the digest first, then load. Between the check and the load
  the file can be replaced. Linux: copy the verified bytes to an anonymous
  `memfd_create` file and load `/proc/self/fd/<n>`, which should narrow the race
  [INFERENCE: not tested on any platform].
  macOS and Windows have no equivalent [INFERENCE: not verified]; a verified private
  copy with restricted permissions narrows the race but does not remove it.
- **Historical snapshots.** A captured program names a digest. For it to replay,
  the bytes with that digest must still exist. Control: the library's path must be
  inside the commit's tree, so `git checkout` at that commit restores the file.
  Cost: binaries live in Git history; a library deleted at HEAD still loads at
  older commits; a project that moves a binary out of its tree breaks old snapshots.
- **Path escapes.** Rejected by the path rules in section 3 (absolute paths,
  `..`, symlinks, outside-tree files).

## 9. Cross-platform file names

Conventions the manifest follows; the engine never guesses a name.

| Target family | Library file name pattern | Example |
|---|---|---|
| Linux (ELF) | `lib<name>.so` | `libslug.so` |
| macOS (Mach-O) | `lib<name>.dylib` | `libslug.dylib` |
| Windows (PE) | `<name>.dll` (no `lib` prefix) | `slug.dll` |

The engine selects the `[native_library.target.<triple>]` table for its own build
target. A missing table is ORNA-STD-NATIVE-LIBRARY naming the target. An
architecture mismatch (a `.so` built for another CPU) fails at the loader; the
loader's message is passed through under ORNA-STD-NATIVE-LIBRARY, since the engine
cannot tell architecture errors apart from missing symbols without parsing ELF.

## 10. The link step

Code facts (read from the workspace, not changed by this document):

- `crates/orna-hir/src/def.rs:130` `NativeDecl { abi: NativeAbi, key: NativeKey }`;
  `def.rs:161` `NativeKey { module: ModulePath, name: Name }`; `def.rs:143` `NativeAbi(u32)`.
- `crates/orna-hir/src/embedded.rs:17` `ENGINE_NATIVE_ABI = NativeAbi::new(1)`.
- `crates/orna-hir/src/lower/items.rs:131-140` builds `NativeDecl` with
  `program.native_abi`, set to `ENGINE_NATIVE_ABI` at `lower/declare.rs:198`.
- `crates/orna-hir/src/lower/declare.rs` (around line 276) is the admission check: a
  native outside an embedded module (`is_embedded_source` is false) is rejected with
  `ORNA-NATIVE-004`.
- `crates/orna-hir/src/lower/classify.rs:129-160` decides `Callee`: `(Core, native)`
  gives `Callee::Native`, `(Sys, native)` gives `Callee::Sys`, and any other
  pending function gives `Callee::Source`. A built `FnBody::Native` gives
  `Callee::Native` in any module. Admission (declare.rs:276-290) rejects a user
  native before classification, so removing admission alone would make a user
  native classify as `Callee::Source`; the classify change is required with it.
- `crates/orna-hir/src/diag.rs:29` `NATIVE_ADMISSION = "ORNA-NATIVE-004"`.
- hir.md 4.3 specifies `link` for the registry only:
  `enum Bound { Native(NativeFn), Sys(SysFnId) }`.
- hir.md 4.3 binds by `DefId`. `NativeKey` is the declaration's identity in the
  `Program`; the link step builds a table indexed by `DefId` from it, so a call
  stores a function pointer or a library symbol, never a name. Whether the
  library path keeps `NativeKey` or `DefId` as the index was not decided here
  [PROPOSAL: `DefId`, as hir.md 4.3 says].

Proposed changes (implementation is not part of this document):

```rust
// classify: every native declaration is Callee::Native, whatever its module,
// except sys, which stays Callee::Sys. The binding is decided at link.

pub enum Bound {
    Native(NativeFn),     // engine registry, module core
    Sys(SysFnId),         // engine registry, module sys
    Library(LibraryFn),   // shared library named by the root manifest
}

pub fn link(
    program: Program,
    host: &HostManifest,
    manifest: &ProjectManifest,      // libraries, modules, digests, target
    profile: LinkProfile,            // Native or BrowserWasm
) -> Result<Linked, Vec<LinkError>>;
```

Algorithm, all before evaluation:

1. For each `FnBody::Native` declaration with key `(m, n)`:
   - `m` is `core`: registry lookup; absence is `ORNA-STD-NATIVE-UNBOUND`.
   - `m` is `sys`: the sys table; absence is `ORNA-STD-NATIVE-UNBOUND`.
   - otherwise: find the libraries whose `modules` contains `m`. None is
     `ORNA-STD-NATIVE-UNBOUND`. Two or more is `ORNA-STD-NATIVE-DUPLICATE`.
2. For each library used, once, in this order, stopping at the first failure for
   that library:
   a. profile check: `BrowserWasm` with a library in use is `ORNA-STD-NATIVE-PROFILE`;
   b. resolve the target entry and the file: missing is `ORNA-STD-NATIVE-LIBRARY`;
   c. read bytes, compute SHA-256: mismatch is `ORNA-STD-NATIVE-DIGEST`;
   d. load (memfd on Linux, section 8): failure is `ORNA-STD-NATIVE-LIBRARY`;
   e. `orna_library_abi()` against `LIBRARY_NATIVE_ABI`: mismatch is `ORNA-STD-NATIVE-ABI`;
   f. for each declaration served: `dlsym` of `orna_<key>`: absence is `ORNA-STD-NATIVE-SYMBOL`.
3. Registry declarations: the `NativeDecl.abi` must equal `ENGINE_NATIVE_ABI`,
   else `ORNA-STD-NATIVE-ABI`. Library declarations do not use `NativeDecl.abi`;
   their ABI is checked in 2e.

Every `LinkError` names the declaration `(module path, name, file:line)` and, for
library errors, the library name. Errors are collected per library and per
declaration; nothing is evaluated unless the list is empty.

Layering: `docs/design/layers.toml` puts `orna-hir` at layer 3, `orna-exec` at 4
and `orna-evaluator` at 5. hir.md 4.3 places `link` in `orna-exec`. The loader
needs the HIR and the library types, not the evaluator; a dependency on the
evaluator would invert the layering [INFERENCE: the Host and value types' layer
was not checked]. Whether the lint policy allows the `unsafe` the loader needs
was not checked [INFERENCE].

## 11. Failure modes

Each condition has its own code. Each message names the declaration and, where
one exists, the library.

| Code | Status | Condition | Must name |
|---|---|---|---|
| `ORNA-STD-NATIVE-UNBOUND` | existing (NATIVE-1 section 3), meaning widened | no registry entry and no library lists the module | declaration module path, name, file:line |
| `ORNA-STD-NATIVE-DUPLICATE` | existing | two libraries list one module, or two declarations share a key | the declarations or libraries in conflict |
| `ORNA-STD-NATIVE-LIBRARY` | proposed | no target entry, file missing or outside the tree, symlink, or the loader refuses the file | library name, target triple, path |
| `ORNA-STD-NATIVE-DIGEST` | proposed | SHA-256 of the file differs from the manifest | library name, path, expected and actual digest, declarations served |
| `ORNA-STD-NATIVE-ABI` | proposed | `orna_library_abi()` differs, or a registry declaration's ABI differs from `ENGINE_NATIVE_ABI` | library or declaration, expected and found numbers |
| `ORNA-STD-NATIVE-SYMBOL` | proposed | `orna_<key>` is not exported | declaration, library, symbol name |
| `ORNA-STD-NATIVE-PROFILE` | proposed | the browser profile links a library native, or lacks a registry native the program needs | declaration, library or native, profile |

Worked error. The project's `libslug.so` was rebuilt, the manifest was not updated,
and the link runs. The output below is an invented illustration of the message
shape, not engine output; the digests are placeholders.

```text
error[ORNA-STD-NATIVE-DIGEST]: native library `slug` does not match its manifest digest
  --> orna.toml:9 (target x86_64-unknown-linux-gnu)
   = path:     native/x86_64-unknown-linux-gnu/libslug.so
   = expected: 3f9a2c1e7b04d8a65f1c90e2b7d43a18c6f5e0d29b7a41c3e8f60b52d9a17c04
   = actual:   91bd7e04c25a6f3b0d8e41a97c2f56b8e13d09a46b8c27f14a5e90d3f2c71b68
   = serves:   text.slugify (text.orna:3)
   = nothing was loaded and nothing was run
```

## 12. Admission, parser and lowering changes

| Layer | Today | Proposed |
|---|---|---|
| Parser (`crates/orna-syntax/src/parser.rs`, native item parsing; line 2816 is the parameter parser, not the acceptance point) | accepts `native fn` at module top level in every module; visibility form is `pub native fn` in hir.md 4.2 (ORNA-NATIVE-001 shows the form; the owner's wording omitted `pub`) | unchanged; the REPL still rejects it (ORNA-NATIVE-003) |
| Admission (`lower/declare.rs`, `is_embedded_source` check around line 276) | rejects a native outside embedded modules (`ORNA-NATIVE-004`) | check removed under option A |
| Classification (`lower/classify.rs:129-160`) | `Callee::Native` for `core` natives and built `FnBody::Native`; `Callee::Sys` for `sys`; a user-module native in the pending path would fall to `Callee::Source` | `Callee::Native` for every native declaration; `Callee::Sys` unchanged |
| Body type doc (`crates/orna-hir/src/def.rs:114-130`) | says a native is in `core.orna`, `sys.orna` or the pinned `std` | rewritten to "any module; bound at link" |
| Diagnostics (`diag.rs:29`) | `ORNA-NATIVE-004` for admission | code retired; link codes of section 11 |
| Link | registry only (hir.md 4.3) | adds the library path of section 10 |
| Spec: ORNA-NATIVE-004, -012 | scope limited to the embedded modules | rewritten (see section 13) |
| Reference chapter 1, "Native declarations" | "Anywhere else it is an error" | rewritten |

## 13. Documents this amendment changes

- `docs/language/DECIDED.md`: D11 sentence on native code; D12 amended, old text
  kept as superseded; answer 10; "What this unblocks" native bullet.
- `docs/language/DECISIONS.md`: D11 and D12 summaries and sections; the open
  question on third-party native code.
- `docs/design/hir.md`: section 4.2 (who may declare), 4.3 (link), 2.2 and 2.6 (`Callee`),
  table in 8.1 (D11 and D12 rows).
- `docs/design/native-declarations.md`: section 3.5 (where a declaration may appear),
  section 3.6, Q5, the stage table (N2), and the planned test names that assert
  "user code may not declare native".
- `docs/spec-extensions/NATIVE-1.md`: ORNA-NATIVE-004 and -012; the diagnostic table.
- `docs/language/reference/00-overview.md`, `01-lexical-and-grammar.md` (native
  section and changelog), `04-effects-and-failure.md` (effect source note).

## 14. Open questions for the owner

1. [OWNER] Project manifest: the name and format (`orna.toml` with TOML is a
   placeholder; no project manifest exists today).
2. [OWNER] Only the root project names libraries (R4), or dependencies may too.
3. [OWNER] In-process `.so` (owner's wording) versus out-of-process execution, given
   the ORNA-MEDIA-005 time and memory MUST (section 6).
4. [OWNER] Library effects: claim plus label (E1) or `effect none` only (E3).
5. [OWNER] Digest only (G1) or also an operator allowlist (G2).
6. [OWNER] `Float` in library signatures, given per-target libm differences (section 6).
7. [OWNER] Whether the library ABI number is separate from `ENGINE_NATIVE_ABI` (section 4).
