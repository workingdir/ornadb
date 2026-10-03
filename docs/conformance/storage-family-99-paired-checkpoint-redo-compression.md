# Storage family 99: redo chain compression across paired log checkpoints

MERGE-1 treats checkpoint generation and position as opaque state and is
silent about compressing paired redo checkpoint histories. The v1
`compress_paired_checkpoint_redo_chain()` projection groups frames by stable
checkpoint ID and coalesces a run only when both sides have exactly equal
checkpoint values at consecutive committed orders.

- A change on either side starts a new run; generations and position bytes are
  never ordered or combined.
- A missing checkpoint entry is retained as `None`. A present checkpoint
  whose position is `None` remains `Some(CheckpointGeneration)`.
- Order gaps stop compression even when the surrounding states are equal, so
  the projection does not infer unobserved redo frames.
- Output keeps each checkpoint ID and the first and last order of every run.

This is a read-only v1 projection over paired log snapshots. It compresses
repeated state only; it does not rewrite checkpoints or merge the two logs.
