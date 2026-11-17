//! The `sys.rt` provider of the HIR host (hir-host.md A5, slice HA8). PARTIAL.
//!
//! `sys.rt` is linked by registering its static and each relation member with the executor's [`RuntimeHostRows`].
//! Registration makes a member link; it does not make it answer. A member answers only when every one of its rows is
//! encoded; every other member is registered unsupported, so a read is `ORNA-EVAL-UNSUPPORTED` until its producer is
//! complete. The relation names are the `sys.rt.*` members of the frozen contract
//! (`crates/orna-sys/src/system_api_inventory.json`). Member status and the bead that closes each gap:
//!
//! | Member | Status | Gap, bead |
//! |---|---|---|
//! | `sys.rt` (static) | unsupported | no complete producer for its scalar fields, `ornadb-jjgvi` |
//! | `sys.rt.runs` | answers the live runs of the projection | unsupported while a run records a failure: `sys.Run.failure` is a `sys.Diagnostic`, diagnostic mapping `ornadb-5il1.280` |
//! | `sys.rt.streams` | unsupported | no `sys.Stream` row encoder yet; the projection already carries the `producer` and `consumer` identities the runtime retains since `ornadb-jdq5h`, `ornadb-csenl.32` (this slice) |
//! | `sys.rt.diagnostics`, `sys.rt.failures` | unsupported | diagnostic mapping, `ornadb-5il1.280` |
//! | every `sys.rt.*` member | unsupported | source-level `sys.rt.<member>` paths do not execute yet, `ornadb-5il1.249` |
//! | the other `sys.rt.*` relations (`info`, `invocations`, `queries`, `sessions`, `clients`, `traces`, `leases`, `listeners`, `plans`, `plan_nodes`, `invocation_arguments`, `spans`, `events`, `checkpoints`, `checkpoint_updates`) | unsupported | no producer projected for them yet, `ornadb-csenl.32` (this slice) |
//!
//! `sys.rt.runs` carries the runs of the projection and nothing else: the current runtime's observation, as the
//! contract's grouped handle selects. A run enters a projection only with the `sys.FunctionRef` that the runtime stored
//! with it, so this module never invents a `function`; a row without one fails the projection upstream.

use orna_exec::{RuntimeHostRows, TableStore};

use crate::RuntimeState;
use crate::error::RuntimeError;
use crate::lease::WriterLease;
use crate::observation::SysCurrentRuntimeProjection;
use crate::run_rows::sys_run_rows;

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
/// observation instant for the `sys.rt` rows: pass it to [`runtime_host_rows`].
///
/// # Errors
///
/// [`RuntimeError`] when the lease is not the current owner or the observation fails validation, including a run or
/// stream whose catalogue identity is missing or not confirmed by its pinned snapshot.
pub async fn capture_sys_rt(
    state: &RuntimeState,
    lease: WriterLease,
) -> Result<SysCurrentRuntimeProjection, RuntimeError> {
    let fence = state.runtime_observation_fence(lease).await?;
    state.current_runtime_sys_projections(&fence).await
}

/// The executor host for the `sys.rt` record, over `store`: every member linked, `sys.rt.runs` answering the runs of
/// `projection` and every other member unsupported.
///
/// `sys.rt.runs` is unsupported instead when a run cannot be encoded (see the module table), because a relation that
/// omitted a run would answer a count wrongly. Built synchronously from no async state; call [`capture_sys_rt`] first
/// for the projection.
#[must_use]
pub fn runtime_host_rows(
    store: Box<dyn TableStore>,
    projection: &SysCurrentRuntimeProjection,
) -> RuntimeHostRows {
    let host = SYS_RT_RELATIONS
        .iter()
        .filter(|relation| **relation != "runs")
        .fold(
            RuntimeHostRows::new(store).with_unsupported(SYS_RT_STATIC),
            |host, relation| host.with_unsupported(&format!("{SYS_RT_STATIC}.{relation}")),
        );
    let runs = format!("{SYS_RT_STATIC}.runs");
    match sys_run_rows(&projection.runs) {
        Some(rows) => host.with_relation(&runs, rows),
        None => host.with_unsupported(&runs),
    }
}
