use std::{collections::HashSet, fs, path::Path, process::Command};

use num_bigint::BigInt;
use orna_foundation_v1::{GitHash, Snapshot};
use orna_repository_v1::Repository;
use orna_runtime_v1::{
    HistoricalSnapshot, NoFault, RuntimeError, RuntimeIdentity, RuntimeState, TableMutation,
};
use tempfile::TempDir;

fn git(directory: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .current_dir(directory)
        .args(arguments)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn repository() -> (TempDir, Repository) {
    let directory = TempDir::new().expect("create temporary repository");
    git(
        directory.path(),
        &["init", "--quiet", "--initial-branch=main", "--template="],
    );
    git(
        directory.path(),
        &["config", "user.name", "kierandrewett"],
    );
    git(
        directory.path(),
        &["config", "user.email", "kieran@drewett.dev"],
    );
    fs::write(
        directory.path().join("main.orna"),
        include_str!("fixtures/historical_snapshot_main.orna"),
    )
    .expect("write fixture root module");
    git(directory.path(), &["add", "main.orna"]);
    git(directory.path(), &["commit", "--quiet", "-m", "initial snapshot"]);
    let repository = Repository::discover(directory.path()).expect("discover repository");
    (directory, repository)
}

fn table_mutation(id: u8, key: u8, value: Option<&[u8]>) -> TableMutation {
    TableMutation::new(
        [id; 16],
        "records",
        vec![key],
        value.map(<[u8]>::to_vec),
    )
    .expect("valid typed table mutation")
}

async fn commit(
    state: &RuntimeState,
    writer: orna_runtime_v1::WriterLease,
    mutations: &[TableMutation],
    digest: u8,
) {
    let context = state.begin_activation().await.expect("capture activation");
    state
        .commit_table_activation(writer, &context, mutations, [digest; 32], &NoFault)
        .await
        .expect("commit table generation");
}

#[tokio::test]
async fn historical_reads_are_pinned_to_checkpoint_generations_and_cover_deletes() {
    let (_directory, repository) = repository();
    let state = RuntimeState::open(
        &repository,
        RuntimeIdentity {
            database_id: [1; 16],
            repository_id: [2; 16],
        },
        [3; 32],
    )
    .await
    .expect("open runtime");
    let writer = state.acquire_lease([4; 16]).await.expect("acquire writer");

    commit(&state, writer, &[table_mutation(5, 1, Some(b"one"))], 6).await;
    let generation_one = state
        .select_historical_snapshot(1)
        .await
        .expect("select first checkpoint generation");
    let generation_one_descriptor = generation_one.capture().snapshot().clone();
    commit(
        &state,
        writer,
        &[
            table_mutation(7, 1, Some(b"two")),
            table_mutation(8, 2, Some(b"also two")),
        ],
        6,
    )
    .await;
    let generation_two = state
        .select_historical_snapshot(2)
        .await
        .expect("select second checkpoint generation");
    commit(&state, writer, &[table_mutation(10, 1, None)], 11).await;

    let generation_zero = state
        .select_historical_snapshot(0)
        .await
        .expect("select initialized generation");
    let generation_three = state
        .select_historical_snapshot(3)
        .await
        .expect("select third checkpoint generation");
    let repeated_generation_one = state
        .select_historical_snapshot(1)
        .await
        .expect("reselect first checkpoint generation");

    assert_eq!(
        generation_one.capture().generation_digest(),
        generation_two.capture().generation_digest(),
        "fixture repeats the payload digest to isolate generation identity"
    );
    assert_ne!(
        generation_one.snapshot_id(),
        generation_two.snapshot_id(),
        "canonical snapshot IDs include generation even when payload digests repeat"
    );
    assert_eq!(
        generation_one.snapshot_id(),
        repeated_generation_one.snapshot_id(),
        "reselecting the same generation yields a stable canonical ID"
    );
    let resolved_generation_one = state
        .resolve_historical_snapshot(&generation_one_descriptor)
        .await
        .expect("resolve the exact as-of descriptor after later generations");
    assert_eq!(resolved_generation_one, generation_one);

    let mut forged_descriptor = generation_one_descriptor.clone();
    if let Snapshot::Cwd { id, .. } = &mut forged_descriptor {
        id[0] ^= 1;
    }
    assert_eq!(
        state
            .resolve_historical_snapshot(&forged_descriptor)
            .await
            .unwrap_err(),
        RuntimeError::SnapshotContextMismatch,
        "an invalid canonical ID cannot be rebound by generation alone"
    );
    let future_descriptor = Snapshot::cwd(
        [1; 16],
        generation_one.capture().runtime_id(),
        generation_three.capture().generation().clone() + BigInt::from(1_u8),
    )
    .expect("construct a canonical future pin");
    assert_eq!(
        state
            .resolve_historical_snapshot(&future_descriptor)
            .await
            .unwrap_err(),
        RuntimeError::SnapshotNotFound
    );
    let committed_descriptor = Snapshot::Commit {
        database: [1; 16],
        algorithm: GitHash::Sha1,
        oid: vec![7; 20],
    };
    assert_eq!(
        state
            .resolve_historical_snapshot(&committed_descriptor)
            .await
            .unwrap_err(),
        RuntimeError::SnapshotNotFound,
        "the runtime resolver does not reinterpret Git snapshots as CWD pins"
    );

    assert!(state
        .read_table_at(&generation_zero, "records")
        .await
        .expect("read initial generation")
        .rows()
        .is_empty());
    assert_eq!(
        state
            .read_table_at(&generation_one, "records")
            .await
            .expect("read first generation")
            .rows(),
        &[(vec![1], b"one".to_vec())]
    );
    assert_eq!(
        state
            .read_table_at(&generation_two, "records")
            .await
            .expect("read second generation")
            .rows(),
        &[
            (vec![1], b"two".to_vec()),
            (vec![2], b"also two".to_vec()),
        ]
    );
    assert_eq!(
        state
            .read_table_at(&generation_three, "records")
            .await
            .expect("read deletion generation")
            .rows(),
        &[(vec![2], b"also two".to_vec())]
    );
    assert_eq!(
        state
            .capture()
            .await
            .expect("read current capture")
            .generation()
            .to_string(),
        "3"
    );
    assert_eq!(
        generation_one.require_same_context(&generation_two),
        Err(RuntimeError::SnapshotContextMismatch)
    );
    let generation_one_rows = state
        .read_table_at(&generation_one, "records")
        .await
        .expect("read first generation with context");
    let generation_two_rows = state
        .read_table_at(&generation_two, "records")
        .await
        .expect("read second generation with context");
    assert_eq!(generation_one_rows.snapshot_id(), generation_one.snapshot_id());
    assert_eq!(generation_two_rows.snapshot_id(), generation_two.snapshot_id());
    assert_eq!(
        generation_one_rows.require_same_context(&generation_two_rows),
        Err(RuntimeError::SnapshotContextMismatch)
    );
    assert_eq!(
        state.select_historical_snapshot(4).await.unwrap_err(),
        RuntimeError::SnapshotNotFound
    );
    assert_eq!(
        generation_one.capture().generation().to_string(),
        "1",
        "later commits cannot move an already selected pin"
    );

    drop(state);
    let reopened = RuntimeState::open(
        &repository,
        RuntimeIdentity {
            database_id: [1; 16],
            repository_id: [2; 16],
        },
        [3; 32],
    )
    .await
    .expect("reopen runtime with retained row history");
    let reopened_generation_one = reopened
        .select_historical_snapshot(1)
        .await
        .expect("reselect historical generation after reopen");
    assert_eq!(
        generation_one.snapshot_id(),
        reopened_generation_one.snapshot_id(),
        "canonical generation identity survives runtime reopen"
    );
    let reopened_as_of = reopened
        .resolve_historical_snapshot(&generation_one_descriptor)
        .await
        .expect("resolve the original as-of descriptor after reopen");
    assert_eq!(reopened_as_of.snapshot_id(), generation_one.snapshot_id());
    assert_eq!(
        reopened
            .read_table_at(&generation_one, "records")
            .await
            .expect("read old generation after restart")
            .rows(),
        &[(vec![1], b"one".to_vec())]
    );
}

#[tokio::test]
async fn historical_snapshot_cannot_be_rebound_to_another_runtime_identity() {
    let (_directory, first_repository) = repository();
    let (_other_directory, other_repository) = repository();
    let first = RuntimeState::open(
        &first_repository,
        RuntimeIdentity {
            database_id: [12; 16],
            repository_id: [13; 16],
        },
        [14; 32],
    )
    .await
    .expect("open first runtime");
    let first_pin = first
        .select_historical_snapshot(0)
        .await
        .expect("select first runtime generation");

    let mut alternate_database = first_pin.capture().database_id();
    alternate_database[0] ^= 1;
    let mut alternate_runtime = first_pin.capture().runtime_id();
    alternate_runtime[0] ^= 1;
    let generation_zero = BigInt::from(first_pin.generation());
    let database_variant = Snapshot::cwd(
        alternate_database,
        first_pin.capture().runtime_id(),
        generation_zero.clone(),
    )
    .expect("construct pin with only the database coordinate changed");
    let runtime_variant = Snapshot::cwd(
        first_pin.capture().database_id(),
        alternate_runtime,
        generation_zero.clone(),
    )
    .expect("construct pin with only the runtime coordinate changed");
    for (descriptor, edge) in [
        (&database_variant, "database identity"),
        (&runtime_variant, "runtime identity"),
    ] {
        let descriptor_id = match descriptor {
            Snapshot::Cwd { id, .. } => *id,
            Snapshot::Commit { .. } => unreachable!("constructed CWD pin"),
        };
        assert_ne!(
            descriptor_id,
            first_pin.snapshot_id(),
            "snapshot IDs include the {edge} coordinate"
        );
        assert_eq!(
            first
                .resolve_historical_snapshot(descriptor)
                .await
                .unwrap_err(),
            RuntimeError::SnapshotContextMismatch,
            "as-of resolution rejects a changed {edge} at the same generation"
        );
    }
    let swapped_coordinates = Snapshot::cwd(
        first_pin.capture().runtime_id(),
        first_pin.capture().database_id(),
        generation_zero,
    )
    .expect("construct pin with database and runtime coordinates swapped");
    let swapped_id = match &swapped_coordinates {
        Snapshot::Cwd { id, .. } => *id,
        Snapshot::Commit { .. } => unreachable!("constructed CWD pin"),
    };
    assert_ne!(
        swapped_id,
        first_pin.snapshot_id(),
        "the snapshot identity tuple preserves database/runtime coordinate order"
    );
    assert_eq!(
        first
            .resolve_historical_snapshot(&swapped_coordinates)
            .await
            .unwrap_err(),
        RuntimeError::SnapshotContextMismatch,
        "swapping valid identity coordinates cannot redirect an as-of pin"
    );

    let second = RuntimeState::open(
        &other_repository,
        RuntimeIdentity {
            database_id: [22; 16],
            repository_id: [23; 16],
        },
        [24; 32],
    )
    .await
    .expect("open second runtime");
    assert_eq!(
        second.read_table_at(&first_pin, "records").await.unwrap_err(),
        RuntimeError::SnapshotContextMismatch
    );
    assert_eq!(
        second
            .resolve_historical_snapshot(first_pin.capture().snapshot())
            .await
            .unwrap_err(),
        RuntimeError::SnapshotContextMismatch
    );

    // The public snapshot type exposes no write operation; it remains a
    // context-bearing selection token for time-scoped reads only.
    let _: &HistoricalSnapshot = &first_pin;
}

#[tokio::test]
async fn as_of_pins_stay_exact_across_generation_encoding_boundaries() {
    let (_directory, repository) = repository();
    let state = RuntimeState::open(
        &repository,
        RuntimeIdentity {
            database_id: [32; 16],
            repository_id: [33; 16],
        },
        [34; 32],
    )
    .await
    .expect("open runtime");
    let writer = state.acquire_lease([35; 16]).await.expect("acquire writer");
    let initial = state
        .select_historical_snapshot(0)
        .await
        .expect("select initial generation");
    let initial_descriptor = initial.capture().snapshot().clone();
    let encoded_initial_descriptor = initial_descriptor
        .encode()
        .expect("encode exact generation-zero descriptor");
    let decoded_initial_descriptor = Snapshot::decode_bytes(&encoded_initial_descriptor)
        .expect("decode exact generation-zero descriptor");
    assert_eq!(decoded_initial_descriptor, initial_descriptor);

    commit(&state, writer, &[table_mutation(36, 1, Some(b"later"))], 37).await;
    let resolved_initial = state
        .resolve_historical_snapshot(&decoded_initial_descriptor)
        .await
        .expect("resolve decoded generation zero after a later commit");
    assert_eq!(resolved_initial, initial);
    assert!(state
        .read_table_at(&resolved_initial, "records")
        .await
        .expect("read the pinned initial generation")
        .rows()
        .is_empty());

    let Snapshot::Cwd {
        database,
        runtime,
        generation,
        ..
    } = &decoded_initial_descriptor
    else {
        unreachable!("decoded an exact CWD pin")
    };
    let swapped_descriptor = Snapshot::cwd(*runtime, *database, generation.clone())
        .expect("construct serialized pin with swapped coordinates");
    let decoded_swapped_descriptor =
        Snapshot::decode_bytes(&swapped_descriptor.encode().expect("encode swapped pin"))
            .expect("decode swapped pin");
    assert_ne!(decoded_swapped_descriptor, decoded_initial_descriptor);
    assert_eq!(
        state
            .resolve_historical_snapshot(&decoded_swapped_descriptor)
            .await
            .unwrap_err(),
        RuntimeError::SnapshotContextMismatch,
        "serialized database/runtime coordinate reordering cannot rebind the pin"
    );

    // CWD snapshot IDs hash the structural generation integer. Exercise the
    // neighboring small-integer and multi-byte boundaries as well as the
    // largest runtime generation and the first value outside its u64 domain.
    let boundary_generations = [
        BigInt::from(0_u8),
        BigInt::from(23_u8),
        BigInt::from(24_u8),
        BigInt::from(255_u16),
        BigInt::from(256_u16),
        BigInt::from(u64::MAX),
        BigInt::from(u64::MAX) + BigInt::from(1_u8),
    ];
    let boundary_snapshots: Vec<_> = boundary_generations
        .iter()
        .map(|generation| {
            Snapshot::cwd([32; 16], initial.capture().runtime_id(), generation.clone())
                .expect("canonical nonnegative generation")
        })
        .collect();
    let boundary_ids: Vec<_> = boundary_snapshots
        .iter()
        .map(|snapshot| match snapshot {
            Snapshot::Cwd { id, .. } => *id,
            Snapshot::Commit { .. } => unreachable!("constructed CWD pin"),
        })
        .collect();
    assert_eq!(
        boundary_ids.iter().collect::<HashSet<_>>().len(),
        boundary_ids.len(),
        "distinct generation encodings retain distinct snapshot IDs"
    );
    for (generation, snapshot) in boundary_generations.iter().zip(&boundary_snapshots) {
        assert_eq!(
            Snapshot::cwd([32; 16], initial.capture().runtime_id(), generation.clone())
                .expect("recompute canonical pin"),
            *snapshot,
            "generation {generation} receives a stable canonical descriptor"
        );
    }

    let round_tripped_boundaries: Vec<_> = boundary_snapshots
        .iter()
        .map(|snapshot| {
            let encoded = snapshot.encode().expect("encode boundary descriptor");
            let decoded = Snapshot::decode_bytes(&encoded).expect("decode boundary descriptor");
            assert_eq!(&decoded, snapshot, "round-trip preserves every coordinate");
            assert_eq!(
                decoded.encode().expect("re-encode boundary descriptor"),
                encoded,
                "round-trip preserves canonical bytes"
            );
            decoded
        })
        .collect();
    for snapshot in round_tripped_boundaries.iter().skip(1) {
        assert_eq!(
            state.resolve_historical_snapshot(snapshot).await.unwrap_err(),
            RuntimeError::SnapshotNotFound,
            "an unretained generation boundary must not resolve to current CWD"
        );
    }
    assert!(Snapshot::cwd([32; 16], initial.capture().runtime_id(), BigInt::from(-1))
        .is_err());
}

#[tokio::test]
async fn retained_pins_cross_generation_encoding_boundaries() {
    let (_directory, repository) = repository();
    let identity = RuntimeIdentity {
        database_id: [42; 16],
        repository_id: [43; 16],
    };
    let state = RuntimeState::open(&repository, identity, [44; 32])
        .await
        .expect("open runtime");
    let writer = state.acquire_lease([45; 16]).await.expect("acquire writer");
    let mut pins = Vec::new();

    for generation in 1..=257_u64 {
        let mut mutation_id = [0; 16];
        mutation_id[8..].copy_from_slice(&generation.to_be_bytes());
        let value = match generation {
            256 => None,
            257 => Some(b"after-256".to_vec()),
            _ => Some(b"steady".to_vec()),
        };
        let mut mutations = vec![
            TableMutation::new(mutation_id, "records", vec![1], value)
                .expect("valid generation-specific table mutation"),
        ];
        // Exercise independent neighboring row images across the retained-pin boundary:
        // key 2 changes as key 1 is deleted, then is deleted as key 1 is restored.
        let neighboring_value = match generation {
            256 => Some(b"neighbor-256".to_vec()),
            257 => None,
            _ => None,
        };
        if generation == 1 {
            let mut retained_row_mutation_id = mutation_id;
            retained_row_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    retained_row_mutation_id,
                    "records",
                    vec![2],
                    Some(b"survivor".to_vec()),
                )
                .expect("valid unaffected-row seed mutation"),
            );
        } else if neighboring_value.is_some() || generation == 257 {
            let mut neighboring_row_mutation_id = mutation_id;
            neighboring_row_mutation_id[0] = 2;
            mutations.push(
                TableMutation::new(
                    neighboring_row_mutation_id,
                    "records",
                    vec![2],
                    neighboring_value,
                )
                .expect("valid neighboring-row boundary mutation"),
            );
        }
        commit(&state, writer, &mutations, 55).await;
        if matches!(generation, 23 | 24 | 255 | 256) {
            let selected = state
                .select_historical_snapshot(generation)
                .await
                .expect("select retained generation around integer boundary");
            let descriptor = selected.capture().snapshot().clone();
            let encoded = descriptor.encode().expect("encode retained boundary pin");
            let decoded = Snapshot::decode_bytes(&encoded).expect("decode retained boundary pin");
            assert_eq!(decoded, descriptor);
            pins.push((generation, decoded, selected));
        }
    }

    assert_eq!(
        pins.iter().map(|(generation, _, _)| *generation).collect::<Vec<_>>(),
        [23, 24, 255, 256]
    );
    assert!(
        pins.windows(2).all(|pair| {
            pair[0].2.capture().generation_digest() == pair[1].2.capture().generation_digest()
        }),
        "all boundary generations deliberately share a payload digest"
    );
    for pair in pins.windows(2) {
        assert_ne!(
            pair[0].2.snapshot_id(),
            pair[1].2.snapshot_id(),
            "snapshot IDs remain distinct at generations {} and {}",
            pair[0].0,
            pair[1].0
        );
    }

    let current = state
        .select_historical_snapshot(257)
        .await
        .expect("select generation after the boundary pins");
    assert_eq!(
        state
            .read_table_at(&current, "records")
        .await
        .expect("read generation 257")
        .rows(),
        &[(vec![1], b"after-256".to_vec())]
    );

    for (generation, descriptor, selected) in &pins {
        let resolved = state
            .resolve_historical_snapshot(descriptor)
            .await
            .expect("resolve exact boundary pin after later generation");
        assert_eq!(&resolved, selected);
        let expected_rows = if *generation == 256 {
            vec![(vec![2], b"neighbor-256".to_vec())]
        } else {
            vec![
                (vec![1], b"steady".to_vec()),
                (vec![2], b"survivor".to_vec()),
            ]
        };
        assert_eq!(
            state
                .read_table_at(&resolved, "records")
                .await
                .expect("read exact boundary generation")
                .rows(),
            expected_rows.as_slice(),
            "generation {generation} resolves to its retained row image"
        );
    }

    drop(state);
    let reopened = RuntimeState::open(&repository, identity, [44; 32])
        .await
        .expect("reopen runtime");
    for (generation, descriptor, selected) in &pins {
        let resolved = reopened
            .resolve_historical_snapshot(descriptor)
            .await
            .expect("resolve boundary pin after reopen");
        assert_eq!(resolved, *selected);
        assert_eq!(resolved.generation(), *generation);
        let expected_rows = if *generation == 256 {
            vec![(vec![2], b"neighbor-256".to_vec())]
        } else {
            vec![
                (vec![1], b"steady".to_vec()),
                (vec![2], b"survivor".to_vec()),
            ]
        };
        assert_eq!(
            reopened
                .read_table_at(&resolved, "records")
                .await
                .expect("read retained boundary generation after reopen")
                .rows(),
            expected_rows.as_slice()
        );
    }
}
