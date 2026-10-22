# 43. Normative dependencies {#references}

The editions below are the adopted external definitions. A newer installed library may implement them, but a newer edition does not silently change the Orna contract. Where this specification restricts a format, its stated profile applies. External errata require explicit adoption.

| Reference | Adopted edition and scope |
|---|---|
| <span id="ref-bcp14"></span>[RFC 2119](https://www.rfc-editor.org/rfc/rfc2119) and [RFC 8174](https://www.rfc-editor.org/rfc/rfc8174) | BCP 14 requirement words; normative meaning applies only to uppercase forms. |
| [Unicode 16.0.0](https://www.unicode.org/versions/Unicode16.0.0/) | Character repertoire and data for identifiers, canonical normalisation and case portability checks. |
| [UAX #31, revision 41](https://www.unicode.org/reports/tr31/tr31-41.html) | Unicode 16 identifier properties; the Orna XID/NFC profile is specified in [lexical structure](#lexical). |
| [UAX #15, revision 56](https://www.unicode.org/reports/tr15/tr15-56.html) | Unicode 16 normalisation. Program strings are not implicitly normalised merely because identifiers are. |
| [IEEE 754-2019](https://standards.ieee.org/ieee/754/6210/) | Binary64 arithmetic, rounding and total-order basis, with Orna's NaN aggregation rules stated in [types](#types). |
| <span id="ref-cbor"></span>[RFC 8949](https://www.rfc-editor.org/rfc/rfc8949) | CBOR structure. OVB-1 selects its own deterministic restrictions, including binary64 floats, as defined in [canonical values](#formats). |
| [RFC 9562](https://www.rfc-editor.org/rfc/rfc9562) | UUID representation and version 7. UUID network-order bytes are used in canonical encodings. |
| [RFC 4648](https://www.rfc-editor.org/rfc/rfc4648) | Base16/Base64 alphabets; each Orna profile states its padding and alphabet choice. |
| [RFC 3339](https://www.rfc-editor.org/rfc/rfc3339) | Timestamp interchange basis; Orna's calendar range, precision and leap-second restrictions are explicit in [lexical structure](#lexical). |
| [RFC 8259](https://www.rfc-editor.org/rfc/rfc8259) | JSON syntax for the selected codec and session HTTP structures. Orna rejects duplicate keys and non-scalar Unicode. |
| [RFC 6455](https://www.rfc-editor.org/rfc/rfc6455) | WebSocket framing, upgrades and control frames for `orna.present.v1`. Application messages are specified in [the live protocol](#protocol). |
| [RFC 8878](https://www.rfc-editor.org/rfc/rfc8878) | Zstandard frames used by compact storage; no out-of-band dictionary is assumed. |
| [Apache parquet-format 2.13.0](https://github.com/apache/parquet-format/tree/apache-parquet-format-2.13.0) | Legacy compact-input interpretation only. The new writer uses ORP-1, not Parquet; retained numeric vectors do not establish a new columnar writer. |
| [Git reference updates](https://git-scm.com/docs/git-update-ref), [index tree updates](https://git-scm.com/docs/git-read-tree), [branch](https://git-scm.com/docs/git-branch) and [switch](https://git-scm.com/docs/git-switch) | External command behaviour is informative background; the adopted branch, staging, ref-CAS and publication rules are fully stated in [branching](#branching) and [publication](#publication), not delegated to an unspecified future Git feature. |

IANA timezone data and optional codec/connector packages change independently of the language. Operations whose result depends on those datasets must identify the selected dataset/package snapshot. The library reference specifies how that context is captured. A historical program must not silently replace it with the host's newest dataset.

A build records the versions of its implementation dependencies, including Turso, SOPS, Git libraries and renderer frameworks. These dependencies implement the observable behaviour specified here; their internal APIs and additional features are not part of the Orna interface.

## Background checked for the 1.1 design

The following primary documentation informed this edition. Its implementation details are not substituted for the fully stated Orna profile algorithms.

| Source | Relevant boundary |
|---|---|
| [Git object model](https://git-scm.com/book/en/v2/Git-Internals-Git-Objects) | Blobs, trees, native graph reachability and content-addressed storage. |
| [Git partial clone](https://git-scm.com/docs/partial-clone) | Promised missing objects, demand fetching, filters, metadata and over-fetching limitations. |
| [Git GC](https://git-scm.com/docs/git-gc) | Reachability, pruning and concurrent-writer limitations; content IDs in application files are not roots. |
| [Git submodules](https://git-scm.com/docs/gitsubmodules) | A gitlink pins another repository's commit; complete export includes that repository. |
| [Git namespace security](https://git-scm.com/docs/gitnamespaces) | Repository views are not an isolation boundary for untrusted raw-Git clients. |
| [bup](https://github.com/bup/bup) | Prior art for chunked binary backups using Git-format storage; OGB-2 is not the bup format. |
| [RFC 9110](https://www.rfc-editor.org/rfc/rfc9110.html) | Adopted HTTP conditional and byte-range response semantics for the optional web Blob response. |
| [WebAssembly concepts](https://developer.mozilla.org/en-US/docs/WebAssembly/Guides/Concepts) | Browser platform access through host bindings; Wasm is not a DOM or networking standard library. |

Historical reference sources were checked on 2026-10-04; final-profile rationale is also recorded in implementation/SOURCES.md. No external service, host or commercial storage plan is required by OGB-2.
