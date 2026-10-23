# Migration into the final 1.1.0 / repository format 3

The previous authoring draft used format 2, bytes-only Blob and three possible
row layouts. The final specification deliberately changes those choices.
It is not safe to replace a header and claim the old data is already format 3.

Read `source/40-upgrade-contract.md` for UPGRADE-3. The implementation must:

1. Preserve original commits, old readers, legacy API/codecs and golden vectors.
2. Inspect actual source, dependencies, index, CWD and pending runtime ownership.
3. Freeze and protect one consistent candidate and its new content before writes.
4. Decode exactly the old authoritative row per key and build the one new map.
5. Default previously unlabeled content to octet-stream/null without media scans.
6. Reuse byte chunks where verified; replace old path catalogs with native edges.
7. Move only published system observations, not live leases, into the store graph.
8. Bind old/new consumer identity domains explicitly so streams do not restart.
9. Produce one journalled reviewed migration commit and retain safe recovery roots.
10. Verify old/new snapshots and offline dependency closure before new writes.

New-format source admits the updated API. Storage-preference/rewrite operations
are retired; explicit history readers retain their previous contracts. A
representation-only migration can be byte preserving while annotation defaults
are still an intentional new value interpretation. Report those separately.

The publication revision and checksum disambiguate the two 1.1.0 authoring
artifacts. Runtime decoders use the repository/value coordinates, not a filename
called “1.1.0”. Do not remove migration guards because version strings match.

No force-push, history purge, destructive schema conversion or overwritten
user/submodule work is authorized by this document. A rollback selects a valid
old context or applies a separately specified conversion; it does not relabel
new bytes as the old format.
