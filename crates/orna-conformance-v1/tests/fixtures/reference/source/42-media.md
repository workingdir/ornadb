# 36. MIME declarations and deterministic suffixes {#media-policy}


MIME is a declared interpretation, not a content hash, format validator or
security guarantee. File extensions are a host-view preference, not a schema
column name. The shared MIME-1 policy lives in the profile, not `.orna/` or a
host OS database. Registered aliases/preferences are pinned for a mount.

**ORNA-MIME-001** Every new Blob MUST have a valid concrete declared MIME value; constructors without knowledge use application/octet-stream, never a wildcard or empty string.
**ORNA-MIME-002** Normalization MUST lowercase type/subtype/parameter names, sort parameters, normalize charset case, and preserve other case-sensitive parameter values; duplicate names, controls and malformed quoted strings MUST fail.
**ORNA-MIME-003** A preferred suffix MUST be chosen from MIME-1 by essence; a compatible explicit safe hint overrides it, otherwise unknown types use bin.
**ORNA-MIME-004** Explicit hints MUST be lowercase safe ASCII suffix segments of at most 32 bytes, without leading dot, slash, backslash, empty segments or traversal. Known-type incompatible hints MUST fail.
**ORNA-MIME-005** Defaults matching the preferred suffix MUST canonicalize to no hint; normal spelling/quoting/case variations MUST not create different annotated values.
**ORNA-MIME-006** Import resolution MUST be explicit declaration first, then bounded trusted detection, then a uniquely mapped suffix, then octet-stream. Conflicting observations MUST be disclosed; host-specific MIME registries cannot silently decide persistent metadata.
**ORNA-MIME-007** Detection MUST NOT run on listings/stat/metadata-only edits. Unsupported or damaged content MUST remain preservable unless an explicit schema constraint requires a valid format.
**ORNA-MIME-008** MIME changes MUST update projected filenames atomically while keeping existing content handles pinned. Renames do not transcode or grant executable status.
**ORNA-MIME-009** HTML/SVG/JS previews MUST not execute merely because the stored declaration requests it. Trusted application assets and arbitrary user-content downloads require different explicit serving policies.

Examples: audio/mpeg prefers mp3 but admits mp1/mp2; text/javascript prefers
js but admits mjs; image/jpeg prefers jpg but admits jpeg/jpe; gzip prefers
gz and uses tar.gz only when explicitly supplied. MIME alone cannot infer
MPEG layer, ECMAScript module mode, or an inner tar archive.

Detailed parameter grammar/bounds, aliases and exact preferred suffixes are
in `profiles/media-1.json` and the normative reference normalizer. The table
is an Orna projection policy, not an assertion that IANA mandates one filename
per media type. Changing this table requires a new profile and explicit view
upgrade; persistent content hashes do not change.
