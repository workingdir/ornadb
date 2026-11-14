//! The `sys.rt` provider of the HIR host (hir-host.md A5, slice HA8). PARTIAL.
//!
//! `sys.rt` is linked by registering its static and each relation member with the executor's [`RuntimeHostRows`].
//! Registration makes a member link; it does not make it answer. Every member is registered unsupported, so a read is
//! `ORNA-EVAL-UNSUPPORTED` until its producer is complete. The relation names are the `sys.rt.*` members of the frozen
//! contract (`crates/orna-sys/src/system_api_inventory.json`). Member status and the bead that closes each gap:
//!
//! | Member | Status | Gap, bead |
//! |---|---|---|
//! | `sys.rt` (static) | unsupported | no complete producer for its scalar fields, `ornadb-jjgvi` |
//! | `sys.rt.runs`, `sys.rt.streams` | unsupported | `sys.Run.function` and `sys.Stream.producer` are non-null `FunctionRef`/`ObjectRef`; the runtime retains only names, `ornadb-jdq5h` |
//! | `sys.rt.diagnostics`, `sys.rt.failures` | unsupported | diagnostic mapping, `ornadb-5il1.280` |
//! | every `sys.rt.*` member | unsupported | source-level `sys.rt.<member>` paths do not execute yet, `ornadb-5il1.249` |
//! | the other `sys.rt.*` relations (`info`, `invocations`, `queries`, `sessions`, `clients`, `traces`, `leases`, `listeners`, `plans`, `plan_nodes`, `invocation_arguments`, `spans`, `events`, `checkpoints`, `checkpoint_updates`) | unsupported | no producer projected for them yet, `ornadb-csenl.32` (this slice) |
//!
//! No `sys.Run` or `sys.Stream` row is encoded: no field set is complete without the coordinates above, and a row with
//! a null in a non-null field would violate the frozen contract.

use orna_exec::{RuntimeHostRows, TableStore};

use crate::RuntimeState;
use crate::error::RuntimeError;
use crate::lease::WriterLease;
use crate::observation::SysCurrentRuntimeProjection;

/// The `sys.rt` static record.
pub const SYS_RT_STATIC: &str = "sys.rt";

/// The 19 relation members of `sys.rt`, each registered as `sys.rt.<name>`. Matches the contract inventory.
pub const SYS_RT_RELATIONS: [&str; 19] = [
    "runs",
    "streams",
    "info",
    "invocations",
    "queries",
    "sessions",
    "clients",
    "traces",
    "failures",
    "diagnostics",
    "leases",
    "listeners",
    "plans",
    "plan_nodes",
    "invocation_arguments",
    "spans",
    "events",
    "checkpoints",
    "checkpoint_updates",
];

/// Captures the current-runtime `sys.rt` projection under one owner-fenced transaction.
///
/// The capture is async and must run before the synchronous executor host is built. The projection is the coherent
/// observation instant for the `sys.rt` rows; it is not yet encoded into them (see the module table).
///
/// # Errors
///
/// [`RuntimeError`] when the lease is not the current owner or the observation fails validation.
pub async fn capture_sys_rt(
    state: &RuntimeState,
    lease: WriterLease,
) -> Result<SysCurrentRuntimeProjection, RuntimeError> {
    let fence = state.runtime_observation_fence(lease).await?;
    state.current_runtime_sys_projections(&fence).await
}

/// The executor host for the `sys.rt` record: every member linked and unsupported, over `store`.
///
/// Built synchronously from no async state; call [`capture_sys_rt`] first when a coherent observation is needed.
#[must_use]
pub fn runtime_host_rows(store: Box<dyn TableStore>) -> RuntimeHostRows {
    SYS_RT_RELATIONS.iter().fold(
        RuntimeHostRows::new(store).with_unsupported(SYS_RT_STATIC),
        |host, relation| host.with_unsupported(&format!("{SYS_RT_STATIC}.{relation}")),
    )
}
