# Merge family 160: multi-table manifest resolution

The storage planner resolves table manifests by digest before reading row
segments. This proof covers one-sided additions, equal-digest additions,
deletion against an unchanged peer, one-sided updates, and equal-digest
concurrent updates across distinct table IDs.

The result pins the selected branch for reusable manifests, omission of tables
deleted against an unchanged base, and the zero-row-read behavior under a zero
row budget. Since the case is specifically about manifest metadata and must
perform no row decoding, it does not add a row payload fixture.
