# Merge family 158: multi-table row-read budget boundary

The storage merge planner visits table IDs in ascending order and reads each
changed table in base, left, right order. This proof pins the row-read budget
boundary across two changed tables with fixtures loaded from inside
`orna-storage-v1` using `include_str!`.

At a budget that exactly covers the first table, the next row in the second
table reports one row beyond the cap, includes both affected table ranges, and
returns no partial plan. At the exact budget for both tables, the planner
returns both complete row segments in stable table and side order. This
complements family 157, which proves multi-table conflict-detail ordering.
