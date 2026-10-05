# Merge family 159: multi-table manifest conflict budget

The three-way storage planner reports table-manifest conflicts when a table is
deleted on one side and edited on the other, or when both sides add the same
table identity with different manifests. This proof loads its row payloads
from crate-local `.orna` fixtures using `include_str!` and pins conflict order
by ascending table ID.

Budgets zero and one stop at the first table conflict beyond the detail limit,
report the affected table prefix, and avoid row reads. At the exact budget, the
planner keeps both typed table conflicts, scans the later clean table, and
returns no partial plan. The fixture-backed row read confirms the shared
conflict and row phases remain bounded in one invocation.
