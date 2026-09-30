# 24. Git history, remotes and partial clones {#git-history}

## Complete logical commits

**ORNA-GIT-001** A commit MUST logically name the complete database snapshot, including all segment objects it references.

**ORNA-GIT-002** A partial clone MAY omit local copies of promised blobs while preserving their object IDs and reachability.

**ORNA-GIT-003** Laziness is a local materialization property, not permission to create incomplete logical commits.

## Partial clone

Recommended clone behavior for large repositories:

```bash
git clone --filter=blob:none --no-checkout <remote>
```

followed by selective checkout/materialization.

**ORNA-GIT-004** A partial-clone-aware implementation MUST distinguish sparse checkout (working-tree paths) from partial clone (locally present Git objects).

**ORNA-GIT-005** Query planning SHOULD use committed manifests to identify required segment blobs before fetching them.

**ORNA-GIT-006** Missing promised objects MAY be fetched lazily from configured promisor remotes.

## Large Object Promisors

Git's Large Object Promisor work is relevant but not assumed universally available.

**ORNA-GIT-007** Orna MUST NOT require github.com or any specific host to support an external large-object promisor.

**ORNA-GIT-008** A self-hosted implementation MAY place large Git blobs on a separate promisor remote while the main remote serves commits, trees and normal blobs.

## Remotes

Normal Git semantics apply:

```bash
orna remote add origin server:~/git/example.git
orna push -u origin main
```

**ORNA-REMOTE-001** Moving a repository to another host MUST use ordinary remote, push and set-url operations rather than requiring commit conversion.

**ORNA-REMOTE-002** Orna MUST NOT invent a `remote migrate` command for behavior already expressible by Git.

**ORNA-REMOTE-003** Orna-aware `clone`, `fetch`, `pull` and `push` MUST synchronize required internal refs under `refs/orna/*` in addition to ordinary requested branch/tag refs.

**ORNA-REMOTE-004** Before a branch snapshot whose allocator watermark depends on an internal allocator ref becomes visible on a remote, that remote's allocator ref MUST be advanced to at least that watermark. An implementation MAY use an atomic multi-ref update; otherwise it advances the allocator ref first. A failed later branch update may create gaps but MUST NOT permit ID reuse.

**ORNA-REMOTE-005** A repository transferred with plain Git remains readable. If required `refs/orna/*` continuity is absent or stale, Orna MUST diagnose the condition before allocating IDs or claiming checkpoint/allocator continuity; it MUST NOT silently guess.

## No rolling-window history

**ORNA-GIT-009** Orna MUST NOT silently delete old table rows from HEAD merely because older commits retain them.

**ORNA-GIT-010** Retention or history rewriting MUST be explicit and destructive behavior MUST be clearly diagnosed.

