# Storage family 96: paired restore fold identities across extension depth omissions

MERGE-1 defines canonical row identity and restore ordering, but it is silent
about a paired view for a key path and its extensions. The v1
`column_restore_paired_path_extension_folds()` projection treats canonical text
keys as related when one begins with the other followed by `/`. The separator
boundary is strict: `root/child` extends `root`, while `rooted` does not. This
relation is local to the projection; storage continues to treat row keys as
opaque canonical values.

Each prefix/extension pair retains a dense timeline for every stable column:

- A missing column yields `None` for both paths.
- A present column with one path absent has `Some(Vec::new())` for that path.
- Each path lists only its own local depth labels, including when an extension
  disappears and later returns at a different depth.
- Storm ranges and wave orders remain attached to both path identities.

This policy makes depth omissions explicit without carrying a sibling path's
label into the omitted slot.
