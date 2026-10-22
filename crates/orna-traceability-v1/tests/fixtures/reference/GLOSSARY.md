# Names for the concepts

**Conceptual integrity:** one coherent set of concepts across language, storage
and tools; not making every internal object identical.

**Co-versioned code and data:** a descriptive name for Orna's promise that a
revision selects the source/schema and the data they understand together.

**Persistent programming environment:** language-level operations work with
long-lived data. Do not claim full orthogonal persistence without meeting its
stronger definition.

**Physical data independence:** a logical row does not depend on storage paths.
**Content addressing:** immutable content identified by a content-derived hash.
**Merkle DAG:** hash-linked acyclic objects, possibly sharing children.
**Structural sharing / copy on write:** unchanged nodes reused between versions.
**Prolly-style ordered index:** content-derived bounded partitioning over
logically ordered keys; ORP-1 is Orna's explicit profile, not Dolt compatibility.
**Content-defined chunking:** byte-derived cut points that can survive insertions.
**Out-of-line value / overflow:** a small row contains a reference to larger data.
**Lazy materialization:** fetch/decode only what an operation needs.
**Semantic identity:** logical fields/annotations independent of physical layout.
**Partial clone:** omitted Git objects retrieved on demand.
**Sparse checkout:** selected paths materialized in a working directory.
**VFS projection:** file/directory interface over stored logical objects.
**MVCC-style pinned read:** an open read observes one admitted generation.
**Compare and swap (CAS):** mutation accepted only against its expected state.
**Logical transaction:** atomic accepted database change, not a public Git commit.
**Publication:** journalled Git ref/index transition exposing a frozen snapshot.
**Dependency pinning:** exact library inputs, not newest host defaults.
**MIME declaration:** metadata describing content, not proof of decoding/safety.
**Directory fanout:** spreading loose files by hash prefixes; Git already does it.
**Recovery draft:** rejected/unaccepted edit retained locally, not application data.

Files remain useful interfaces. Blob is a language value. A row has its ordinary
primary-key identity. No compulsory global File/Asset table follows from any of
these concepts.
