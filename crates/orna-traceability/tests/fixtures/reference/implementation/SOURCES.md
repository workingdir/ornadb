# Primary reference sources

Reviewed 5 October 2026. The package format/profile definitions are self-contained;
links below are rationale, not dependencies that readers fetch to decode data.

- Git object model: https://git-scm.com/book/en/v2/Git-Internals-Git-Objects
- Git repository layout/fanout/worktrees: https://git-scm.com/docs/gitrepository-layout
- Git partial clone and object granularity: https://git-scm.com/docs/partial-clone
- Git garbage collection/reachability: https://git-scm.com/docs/git-gc
- Linux FUSE cache constraints: https://docs.kernel.org/filesystems/fuse/fuse-io.html
- libfuse flush/fsync/release behavior: https://libfuse.github.io/doxygen/structfuse__operations.html
- MIME grammar and range HTTP: https://www.rfc-editor.org/rfc/rfc9110.html
- JavaScript media type/module suffixes: https://www.rfc-editor.org/rfc/rfc9239.html
- IANA MPEG audio registration: https://www.iana.org/assignments/media-types/audio/mpeg
- IANA gzip registration: https://www.iana.org/assignments/media-types/application/gzip
- Dolt prolly tree prior art: https://www.dolthub.com/blog/2024-02-29-storage-engine/
- bup native Git content graph prior art: https://github.com/bup/bup/blob/main/DESIGN.md

Evidence scope matters: native Git supports the object graph; it does not supply
Orna's schema validation, row-query semantics, MIME policy or FUSE transactions.
The current-repository facts in the prior plan were observed at its pinned
commit. Implementation must inspect its actual current checkout independently.
