# Storage family 78: column restore-ladder depth labels

The Orna 1.0.0 storage reference requires stable column identities and defines
three-way merge behavior, but it does not prescribe how adapters retain local
restore-depth labels when sibling columns have uneven fragment counts. The
storage adapter policy is therefore:

- Identify each branch by the stable `(table ObjectId, column ObjectId)` pair.
- Require every branch to supply all depth positions in
  `0..fragment_count`. Empty fragments still occupy their labeled position.
- Keep each column's count independent. A shorter or longer sibling column
  never supplies missing labels for another column.
- Queue a complete paired wave behind earlier lineage positions and release
  its cells in table, column, depth, and canonical primary-key order.
- Bind committed retries to the same table-column set, each local depth count,
  and each fragment's canonical row-key/value pairs. A changed count reports
  the column-specific count mismatch; changed cell identity is a conflict.

This keeps adapter recovery deterministic without treating fragment indexes as
cross-column schema positions.
