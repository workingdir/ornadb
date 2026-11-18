# Previous-draft compatibility inputs (historical, not current spec)

`previous-1.1.0-draft-blob-vectors.json` is the member
`Orna-1.1.0/tests/blob-vectors.json` extracted byte-for-byte from
`crates/orna-conformance/tests/fixtures/reference/compatibility/previous-1.1.0-draft.zip`
(zip SHA-256 `096273779e0ce773ef5ede4eb9b1fe0af4817776fb78097fb58fadb366449e59`,
member SHA-256 `0a12fd87250433644ccbf3c88fb0c0495c4f6832c691bfc08dd61f7b794df8bb`).

It supplies the `stored_reference_cbor` and `length` golden values checked by
`inline_golden_vectors_match_vendored_draft_blob_vectors` in
`crates/orna-format3/src/format2_rov.rs`. It is used for golden-byte comparison
of retained format-2 ROV-2 storage references only; it is not current 1.1.0 final
execution evidence. The test pins the member SHA-256 so the input cannot drift
silently.
