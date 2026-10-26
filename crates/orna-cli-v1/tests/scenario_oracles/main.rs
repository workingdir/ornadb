//! Orna 1.1.0 scenario oracle runner (§39 "Mandatory behavioral scenarios"
//! and "1.1 binary and source-hosted profiles"; ORNA-TEST-011,
//! ORNA-EVIDENCE-001).
//!
//! The oracles come from the vendored, byte-identical release file
//! `tests/fixtures/reference/tests/scenarios.json`; no oracle text is copied
//! into this code. Every FINAL-* and V110-* oracle, and every scenario that
//! backs a §39 mandatory behaviour, has one registry entry. Each entry runs in
//! a fresh database made by `orna init` and drives only the product binary
//! and Git, the repository it manages.
//!
//! An entry is either an executor, which must pass, or a known gap: the bead
//! that blocks the scenario plus a probe that reproduces the blocker with
//! product commands. A gap never counts as a pass. When a probe stops
//! reproducing its blocker the scenario is STALE. STALE does not fail the
//! release run, but the set is ratcheted by `stale-entries.txt`, so the gap
//! list cannot silently change: write the scenario's executor, or record its
//! next blocker and bead, then re-record the list. Mandatory scenarios must
//! never be STALE.
//!
//! `cargo test -p orna-cli-v1 --test scenario_oracles -- --show-output`
//! prints one line per scenario.

mod harness;
mod mandatory;
mod release;

use std::collections::BTreeSet;

use harness::{Entry, is_release_obligation, oracles, report, run_entries};

fn registered_ids(entries: &[Entry]) -> BTreeSet<&'static str> {
    let mut ids = BTreeSet::new();
    for entry in entries {
        assert!(ids.insert(entry.id), "{} is registered twice", entry.id);
    }
    ids
}

/// Every release obligation in the vendored file has exactly one entry, and
/// every entry names a published oracle.
#[test]
fn every_release_oracle_is_registered_once() {
    let registered = registered_ids(&release::entries());
    let published = oracles()
        .keys()
        .map(String::as_str)
        .filter(|id| is_release_obligation(id))
        .collect::<BTreeSet<_>>();
    let unregistered = published.difference(&registered).collect::<Vec<_>>();
    let unknown = registered.difference(&published).collect::<Vec<_>>();
    assert!(
        unregistered.is_empty() && unknown.is_empty(),
        "release oracles without an entry: {unregistered:?}; entries without a release oracle: {unknown:?}"
    );
    for id in registered_ids(&mandatory::entries()) {
        assert!(
            oracles().contains_key(id) && !is_release_obligation(id),
            "mandatory scenario {id} is not a retained oracle in the release file"
        );
    }
}

/// The committed STALE ratchet (bead ornadb-ksrh). Each ID is a known gap whose
/// blocker no longer reproduces. The set may only shrink by re-recording: a new
/// STALE entry fails the suite, and so does a listed entry that is no longer
/// STALE.
const STALE_RATCHET: &str = include_str!("stale-entries.txt");

fn ratchet_entries() -> BTreeSet<&'static str> {
    STALE_RATCHET
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect()
}

/// Runs all 157 FINAL-* and V110-* oracles and reports each one. Passes when
/// every executor passes and the STALE set equals the committed ratchet.
#[test]
fn release_scenario_oracles() {
    let entries = release::entries();
    let verdicts = run_entries(&entries);
    let stale = report(&entries, &verdicts)
        .into_iter()
        .collect::<BTreeSet<_>>();
    let listed = ratchet_entries();
    let new_stale = stale.difference(&listed).collect::<Vec<_>>();
    let no_longer_stale = listed.difference(&stale).collect::<Vec<_>>();
    println!(
        "STALE ratchet: {} listed in stale-entries.txt, {} observed",
        listed.len(),
        stale.len()
    );
    assert!(
        new_stale.is_empty() && no_longer_stale.is_empty(),
        "STALE ratchet mismatch. New STALE (write the executor or record the next blocker): \
         {new_stale:?}. Listed but no longer STALE (remove by re-recording): \
         {no_longer_stale:?}. Observed STALE set, sorted, for re-recording stale-entries.txt: \
         {stale:?}"
    );
}

fn mandatory(ids: &[&str]) {
    let entries = mandatory::entries()
        .into_iter()
        .filter(|entry| ids.contains(&entry.id))
        .collect::<Vec<_>>();
    assert_eq!(
        entries.len(),
        ids.len(),
        "every mandatory scenario has an entry: {ids:?}"
    );
    let verdicts = run_entries(&entries);
    let stale = report(&entries, &verdicts);
    assert!(
        stale.is_empty(),
        "mandatory scenarios must not be STALE: {stale:?}"
    );
}

/// §39: CWD includes a local row while HEAD does not.
#[test]
fn mandatory_cwd_includes_a_local_row_while_head_does_not() {
    mandatory(&["STATE-001"]);
}

/// §39: automatic IDs never reuse a deleted or reset value.
#[test]
fn mandatory_automatic_ids_never_reuse_a_deleted_or_reset_value() {
    mandatory(&["KEY-001", "KEY-002"]);
}

/// §39: nested function writes roll back with the activation.
#[test]
fn mandatory_nested_function_writes_roll_back_with_the_activation() {
    mandatory(&["TXN-001"]);
}

/// §39: an external HTTP effect is not rolled back.
#[test]
fn mandatory_an_external_http_effect_is_not_rolled_back() {
    mandatory(&["TXN-003"]);
}

/// §39: checkpoint and row writes commit together.
#[test]
fn mandatory_checkpoint_and_row_writes_commit_together() {
    mandatory(&["CP-001"]);
}

/// §39: poison failure pauses without infinite durable rows.
#[test]
fn mandatory_poison_failure_pauses_without_infinite_durable_rows() {
    mandatory(&["FAIL-001"]);
}

/// §39: explicit skip preserves or references the payload and advances the
/// checkpoint.
#[test]
fn mandatory_explicit_skip_preserves_payload_and_advances_checkpoint() {
    mandatory(&["FAIL-002", "FAIL-003"]);
}

/// §39: publication crashes before and after the ref update recover
/// correctly. Crash points exist only in a binary built with the
/// `crash-barriers` feature, so the default build reports this test as
/// ignored rather than passing it.
#[test]
#[cfg_attr(
    not(feature = "crash-barriers"),
    ignore = "publication crash points need `--features crash-barriers`"
)]
fn mandatory_publication_crashes_before_and_after_ref_update_recover() {
    mandatory(&["PUB-001", "PUB-002"]);
}

/// §39: automatic data commit excludes staged human changes.
#[test]
fn mandatory_automatic_data_commit_excludes_staged_human_changes() {
    mandatory(&["PUB-004", "PUB-006"]);
}

/// §39: semantic merge handles compatible and conflicting schema/row edits.
/// The release publishes no dedicated merge oracle; ASSERT-MERGE-091 is the
/// semantic-merge oracle it does publish.
#[test]
fn mandatory_semantic_merge_handles_compatible_and_conflicting_edits() {
    mandatory(&["ASSERT-MERGE-091"]);
}

/// §39: historical `sys.Run.as_of(HEAD)` is valid.
#[test]
fn mandatory_historical_sys_run_as_of_head_is_valid() {
    mandatory(&["SYS-001"]);
}

/// §39: page delta falls back to subtree replacement for an unkeyed value.
#[test]
fn mandatory_page_delta_falls_back_to_subtree_replacement() {
    mandatory(&["LIVE-002", "LIVE-004"]);
}

/// §39: display override does not change codec output.
#[test]
fn mandatory_display_override_does_not_change_codec_output() {
    mandatory(&["PRES-001"]);
}

/// §39: partial clone fetches only the required bounded objects.
#[test]
fn mandatory_partial_clone_fetches_only_required_objects() {
    mandatory(&["GIT-001"]);
}
