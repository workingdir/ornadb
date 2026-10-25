# Reference database

This project demonstrates lending, stock transfers and resumable sensor input
using local data and the Orna intrinsic environment.

Run in a fresh disposable clone. `seed` inserts the initial rows. Repeating it
fails on duplicate keys and leaves the existing state unchanged.

1. `orna init` in this directory.
2. `orna run seed`: creates two books and two stock rows.
3. `orna run exercise`: lends book-1; stock changes from (12,4) to (9,7).
4. `orna run sensors.ingest`: reads the three supplied list items. Each callback
   commits its reading and checkpoint together. A second invocation with the same
   durable consumer/source identity resumes at exhaustion and inserts nothing.

See [`expectations.json`](expectations.json) for initial and terminal states,
and for failure cases. The [command-line reference](../../index.html#cli)
describes invocation syntax.
