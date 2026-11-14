//! The `sys.rt` record links through `RuntimeHostRows` and every member is `ORNA-EVAL-UNSUPPORTED` until its producer is
//! complete (hir-host.md A5, slice HA8). `orna-runtime` registers the same names (`orna_runtime::SYS_RT_STATIC`,
//! `SYS_RT_RELATIONS`); this test cannot depend on it without a cycle, so it mirrors the list.
//!
//! The assertions run at the `Host` trait boundary. The front end does not yet type a nested `sys.rt.<member>` path
//! (a pending member call on it stays `NotYet`), so a source-level `sys.rt.runs.count()` cannot be used as the proof.

#![cfg(feature = "test-support")]

use orna_exec::{Host, MemoryStore, RuntimeHostRows};

/// Mirrors `orna_runtime::SYS_RT_RELATIONS`; keep the two lists in step.
const RELATIONS: [&str; 19] = [
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

/// The `sys.rt` record as `orna-runtime` registers it: the static and all 19 relations, every one unsupported.
fn sys_rt_host() -> RuntimeHostRows {
    RELATIONS.iter().fold(
        RuntimeHostRows::new(Box::new(MemoryStore::new())).with_unsupported("sys.rt"),
        |host, relation| host.with_unsupported(&format!("sys.rt.{relation}")),
    )
}

#[test]
fn every_sys_rt_member_is_linked() {
    let host = sys_rt_host();
    assert!(host.manifest().slot("sys.rt").is_some());
    for relation in RELATIONS {
        assert!(
            host.manifest()
                .slot(&format!("sys.rt.{relation}"))
                .is_some(),
            "sys.rt.{relation} must link"
        );
    }
}

#[test]
fn sys_rt_runs_is_unsupported_until_the_producer_is_complete() {
    // The contract's `sys.Run.function` is a non-null FunctionRef the runtime cannot yet produce; see the P1
    // blocker bead filed for ornadb-csenl.32 (hir-host.md A5). Until it closes, `sys.rt.runs` must fail closed.
    let mut host = sys_rt_host();
    let slot = host
        .manifest()
        .slot("sys.rt.runs")
        .expect("sys.rt.runs is linked");
    let failure = host
        .sys_relation(slot, None)
        .err()
        .expect("sys.rt.runs must not answer rows");
    assert_eq!(failure.code, "ORNA-EVAL-UNSUPPORTED");
}
