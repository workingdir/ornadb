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
        // Exercise neighboring row images across the retained-pin boundary: key 2
        // changes as key 1 is deleted, then is deleted as key 1 is restored. Key 3
        // remains stable to prove row deletion doesn't shift a retained neighbor.
        // Stable rows on both sides also cover compaction at either edge of the pair.
        // The stable key [1, 0] extends changing key [1], so deletion and reinsertion
        // must leave the prefix-related row image distinct.
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
            let mut stable_neighbor_mutation_id = mutation_id;
            stable_neighbor_mutation_id[0] = 3;
            mutations.push(
                TableMutation::new(
                    stable_neighbor_mutation_id,
                    "records",
                    vec![3],
                    Some(b"tail-survivor".to_vec()),
                )
                .expect("valid stable-neighbor seed mutation"),
            );
            let mut leading_neighbor_mutation_id = mutation_id;
            leading_neighbor_mutation_id[0] = 4;
            mutations.push(
                TableMutation::new(
                    leading_neighbor_mutation_id,
                    "records",
                    vec![0],
                    Some(b"head-survivor".to_vec()),
                )
                .expect("valid leading-neighbor seed mutation"),
            );
            let mut prefix_neighbor_mutation_id = mutation_id;
            prefix_neighbor_mutation_id[0] = 5;
            mutations.push(
                TableMutation::new(
                    prefix_neighbor_mutation_id,
                    "records",
                    vec![1, 0],
                    Some(b"prefix-survivor".to_vec()),
                )
                .expect("valid stable prefix-neighbor seed mutation"),
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
        &[
            (vec![0], b"head-survivor".to_vec()),
            (vec![1], b"after-256".to_vec()),
            (vec![1, 0], b"prefix-survivor".to_vec()),
            (vec![3], b"tail-survivor".to_vec()),
        ]
    );

    for (generation, descriptor, selected) in &pins {
        let resolved = state
            .resolve_historical_snapshot(descriptor)
            .await
            .expect("resolve exact boundary pin after later generation");
        assert_eq!(&resolved, selected);
        let expected_rows = if *generation == 256 {
            vec![
                (vec![0], b"head-survivor".to_vec()),
                (vec![1, 0], b"prefix-survivor".to_vec()),
                (vec![2], b"neighbor-256".to_vec()),
                (vec![3], b"tail-survivor".to_vec()),
            ]
        } else {
            vec![
                (vec![0], b"head-survivor".to_vec()),
                (vec![1], b"steady".to_vec()),
                (vec![1, 0], b"prefix-survivor".to_vec()),
                (vec![2], b"survivor".to_vec()),
                (vec![3], b"tail-survivor".to_vec()),
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
            vec![
                (vec![0], b"head-survivor".to_vec()),
                (vec![1, 0], b"prefix-survivor".to_vec()),
                (vec![2], b"neighbor-256".to_vec()),
                (vec![3], b"tail-survivor".to_vec()),
            ]
        } else {
            vec![
                (vec![0], b"head-survivor".to_vec()),
                (vec![1], b"steady".to_vec()),
                (vec![1, 0], b"prefix-survivor".to_vec()),
                (vec![2], b"survivor".to_vec()),
                (vec![3], b"tail-survivor".to_vec()),
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

#[tokio::test]
async fn stable_prefix_neighbor_survives_extension_pin_tails() {
    let (_directory, repository) = repository();
    let identity = RuntimeIdentity {
        database_id: [52; 16],
        repository_id: [53; 16],
    };
    let state = RuntimeState::open(&repository, identity, [54; 32])
        .await
        .expect("open runtime");
    let writer = state.acquire_lease([55; 16]).await.expect("acquire writer");
    let mut pins = Vec::new();

    for generation in 1..=258_u64 {
        let mut mutation_id = [0; 16];
        mutation_id[8..].copy_from_slice(&generation.to_be_bytes());
        // Exercise the reverse prefix orientation: the shorter key is stable while
        // its longer neighbor is deleted at 256 and restored at 257. At 258 it
        // returns to its original bytes, proving an identical row image is still
        // addressed by a distinct historical generation pin.
        let extension_value = match generation {
            256 => None,
            257 => Some(b"restored-extension".to_vec()),
            258 => Some(b"steady-extension".to_vec()),
            _ => Some(b"steady-extension".to_vec()),
        };
        let mut mutations = vec![
            TableMutation::new(mutation_id, "records", vec![5, 0], extension_value)
                .expect("valid extension-row generation mutation"),
        ];
        if generation == 1 {
            let mut prefix_mutation_id = mutation_id;
            prefix_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    prefix_mutation_id,
                    "records",
                    vec![5],
                    Some(b"stable-prefix".to_vec()),
                )
                .expect("valid stable prefix-row mutation"),
            );
        }
        commit(&state, writer, &mutations, 56).await;

        if matches!(generation, 255 | 256 | 257 | 258) {
            let selected = state
                .select_historical_snapshot(generation)
                .await
                .expect("select adjacent prefix boundary pin");
            let descriptor = selected.capture().snapshot().clone();
            let encoded = descriptor.encode().expect("encode prefix boundary pin");
            let decoded = Snapshot::decode_bytes(&encoded).expect("decode prefix boundary pin");
            assert_eq!(decoded, descriptor);
            pins.push((generation, decoded, selected));
        }
    }

    assert_eq!(
        pins.iter().map(|(generation, _, _)| *generation).collect::<Vec<_>>(),
        [255, 256, 257, 258]
    );
    assert!(pins.windows(2).all(|pair| {
        pair[0].2.capture().generation_digest() == pair[1].2.capture().generation_digest()
    }));
    for pair in pins.windows(2) {
        assert_ne!(
            pair[0].2.snapshot_id(),
            pair[1].2.snapshot_id(),
            "prefix boundary pins remain distinct at generations {} and {}",
            pair[0].0,
            pair[1].0
        );
    }
    let latest = state
        .select_historical_snapshot(258)
        .await
        .expect("select generation after restoring the original extension bytes");
    assert_eq!(
        state
            .read_table_at(&latest, "records")
            .await
            .expect("read generation 258")
            .rows(),
        &[
            (vec![5], b"stable-prefix".to_vec()),
            (vec![5, 0], b"steady-extension".to_vec()),
        ]
    );

    for (generation, descriptor, selected) in &pins {
        let resolved = state
            .resolve_historical_snapshot(descriptor)
            .await
            .expect("resolve adjacent prefix pin after later commits");
        assert_eq!(&resolved, selected);
        let expected_rows = match *generation {
            255 => vec![
                (vec![5], b"stable-prefix".to_vec()),
                (vec![5, 0], b"steady-extension".to_vec()),
            ],
            256 => vec![(vec![5], b"stable-prefix".to_vec())],
            257 => vec![
                (vec![5], b"stable-prefix".to_vec()),
                (vec![5, 0], b"restored-extension".to_vec()),
            ],
            258 => vec![
                (vec![5], b"stable-prefix".to_vec()),
                (vec![5, 0], b"steady-extension".to_vec()),
            ],
            _ => unreachable!("only prefix boundary pins are retained"),
        };
        assert_eq!(
            state
                .read_table_at(&resolved, "records")
                .await
                .expect("read exact prefix boundary pin")
                .rows(),
            expected_rows.as_slice(),
            "generation {generation} retains the stable prefix row"
        );
    }

    drop(state);
    let reopened = RuntimeState::open(&repository, identity, [54; 32])
        .await
        .expect("reopen runtime");
    for (generation, descriptor, selected) in &pins {
        let resolved = reopened
            .resolve_historical_snapshot(descriptor)
            .await
            .expect("resolve prefix pin after reopen");
        assert_eq!(resolved, *selected);
        assert_eq!(resolved.generation(), *generation);
        let expected_rows = match *generation {
            255 => vec![
                (vec![5], b"stable-prefix".to_vec()),
                (vec![5, 0], b"steady-extension".to_vec()),
            ],
            256 => vec![(vec![5], b"stable-prefix".to_vec())],
            257 => vec![
                (vec![5], b"stable-prefix".to_vec()),
                (vec![5, 0], b"restored-extension".to_vec()),
            ],
            258 => vec![
                (vec![5], b"stable-prefix".to_vec()),
                (vec![5, 0], b"steady-extension".to_vec()),
            ],
            _ => unreachable!("only prefix boundary pins are retained"),
        };
        assert_eq!(
            reopened
                .read_table_at(&resolved, "records")
                .await
                .expect("read retained prefix pin after reopen")
                .rows(),
            expected_rows.as_slice()
        );
    }
}

#[tokio::test]
async fn identical_prefix_restoration_preserves_boundary_pins() {
    let (_directory, repository) = repository();
    let identity = RuntimeIdentity {
        database_id: [62; 16],
        repository_id: [63; 16],
    };
    let state = RuntimeState::open(&repository, identity, [64; 32])
        .await
        .expect("open runtime");
    let writer = state.acquire_lease([65; 16]).await.expect("acquire writer");
    let mut pins = Vec::new();

    for generation in 1..=434_u64 {
        let mut mutation_id = [0; 16];
        mutation_id[8..].copy_from_slice(&generation.to_be_bytes());
        // Delete the prefix while its extended neighbor remains stable, restore an
        // intermediate value, return to the original bytes, repeat delete and
        // identical restoration twice, then retain the terminal delete after the
        // third restoration. Restore the prefix once more after the neighbor update,
        // then prove the following terminal delete remains visible before restoring
        // the prefix again at the final retained generation. Finally restore the
        // neighboring extension to its original bytes without rewriting the
        // restored prefix, proving that an identical neighbor image does not
        // collapse the generation-specific snapshot pin. Delete that neighbor,
        // then recreate it while keeping the restored prefix untouched. Finish
        // with another terminal neighbor delete to prove the restored prefix's
        // row image survives a delete after that recreate. With the neighbor
        // absent, delete and restore the prefix to exercise the final row-image
        // boundary in both directions. Recreate the neighbor after that prefix
        // restoration, then delete its tail edge once more. Finally delete the
        // prefix, recreate and delete the tail while the prefix is absent, and
        // restore only the prefix. Close this sequence by recreating the tail,
        // deleting and restoring the prefix around it, deleting both edge rows
        // together, then rebuilding the tail before restoring the prefix. Finish
        // by deleting that restored tail, closing the prefix, rebuilding each
        // edge in turn, and deleting the final tail after its prefix is absent.
        // Finish by rebuilding the prefix and tail, deleting the prefix while its
        // tail survives, and closing the final tail from both row arrangements.
        // Continue the edge round tail-first, restore the prefix, delete the
        // prefix before its tail, then repeat with the opposite deletion order.
        // Continue from the empty image with a prefix and tail, delete the prefix
        // while its tail survives, restore the prefix, then close both rows in
        // turn. Reopen and close one final tail-only edge.
        // Finish with a final pair of joint edge closures, including a tail
        // restored after the first closure.
        // Replay the tail beside a restored prefix, close that tail after its
        // prefix is absent, then pin one final prefix-only closure.
        // Recreate and delete the tail with the prefix intact, then delete the
        // prefix first and close the remaining tail before the last prefix pin.
        // Rebuild the edge, delete the prefix around a surviving tail, delete
        // the tail with its prefix present, then finish with a tail-only close.
        // Restore two tail images around deletions, remove the prefix while the
        // final tail survives, then close that tail to the terminal empty image.
        // Rebuild the edge, exercise both deletion orders, and finish with a
        // tail-only restoration followed by its final closure.
        // Continue that order through another edge recreation and pin the final
        // tail-only closure after both row arrangements.
        // Continue with the tail beside the prefix, delete the prefix first,
        // restore and delete the tail while the prefix survives, then close the
        // prefix and prove one last tail-only closure. The reference is silent
        // on these terminal edge interleavings; retain each concrete row image.
        // Reopen the tail beside the prefix with a changed image, remove the
        // prefix, restore it, and delete the tail before closing the prefix.
        // Finish with a distinct tail-only image and its terminal empty image;
        // the reference does not prescribe these replacement-order cases.
        // Replace the tail twice before deleting the prefix, then restore the
        // prefix and replace the tail in one generation. Close the tail and
        // prefix separately, and pin one final tail-only closure. The reference
        // is silent on the combined row-image boundary, so the fixture proof
        // records both resulting rows explicitly.
        // Continue that closure after another pair of tail images, then delete
        // the prefix alongside a tail replacement and restore the prefix. Close
        // the tail and prefix separately, then pin one final tail-only closure.
        // The reference leaves this history boundary unspecified.
        // Reopen both edges, close the prefix first, restore the prefix while
        // replacing the tail, then close the tail first on another paired
        // image. Record the joint restore and both terminal row arrangements.
        // Continue the final closure edge with two tail images, remove the
        // prefix, reopen it while replacing the tail, then close the tail before
        // deleting the prefix and finally close the tail-only image. The
        // reference does not prescribe this terminal ordering, so keep each
        // concrete generation image as the compatibility proof.
        // Continue from empty with a prefix and two changing tail images, close
        // the prefix first, restore it alongside another tail replacement,
        // close and recreate the tail, then close prefix and tail separately.
        // The reference is silent on this repeated edge order; pin every image.
        let prefix_value = match generation {
            256 | 259 | 261 | 263 | 264 | 266 | 273 | 277 | 282 | 284 | 288 | 291 | 295 | 298
            | 303 | 308 | 311 | 314 | 319 | 321 | 324 | 329 | 332 | 337 | 340 | 343 | 346
            | 353 | 355 | 359 | 362 | 367 | 370 | 375 | 378 | 384 | 387 | 393 | 396 | 402
            | 405 | 410 | 413 | 415 | 420 | 424 | 429 | 433 => None,
            257 => Some(b"intermediate-prefix".to_vec()),
            265 => Some(b"original-prefix".to_vec()),
            _ => Some(b"original-prefix".to_vec()),
        };
        let mut mutations = Vec::new();
        if !matches!(
            generation,
            264 | 268 | 269 | 270 | 271 | 272 | 278 | 279 | 281 | 285 | 287 | 290 | 292
                | 294 | 297 | 299 | 300 | 301 | 304 | 306 | 307 | 310 | 313 | 315 | 316
                | 318 | 322 | 326 | 327 | 328 | 330 | 334 | 335 | 336 | 338 | 342 | 345 | 347
                | 348 | 350 | 351 | 352 | 356 | 358 | 361 | 363 | 364 | 366 | 369 | 371 | 372
                | 374 | 377 | 379 | 380 | 382 | 383 | 386 | 388 | 389 | 391 | 392 | 395 | 397
                | 398 | 400 | 401 | 404 | 406 | 407 | 409 | 412 | 416 | 418 | 419 | 422 | 423
                | 425 | 427 | 428 | 431 | 432 | 434
        ) {
            mutations.push(
                TableMutation::new(mutation_id, "records", vec![5], prefix_value)
                    .expect("valid prefix-row generation mutation"),
            );
        }
        if generation == 1 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"stable-extension".to_vec()),
                )
                .expect("valid stable extension-row mutation"),
            );
        } else if generation == 264 {
            // Change only the neighboring extension after the terminal prefix delete.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"post-delete-extension".to_vec()),
                )
                .expect("valid post-delete extension update"),
            );
        } else if generation == 268 {
            // Change the extension again after the prefix has been restored at 267.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"post-restoration-extension".to_vec()),
                )
                .expect("valid post-restoration extension update"),
            );
        } else if generation == 269 {
            // The format does not prescribe a special merge rule for restoring
            // an adjacent row after prefix restoration; retain the captured
            // generation's ordinary row image and leave the prefix untouched.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"stable-extension".to_vec()),
                )
                .expect("valid restored extension-row mutation"),
            );
        } else if generation == 270 {
            // Keep the restored prefix's row image when its neighbor is deleted.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid terminal neighbor deletion"),
            );
        } else if generation == 271 {
            // A later neighbor recreation must not rewrite the retained prefix.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"stable-extension".to_vec()),
                )
                .expect("valid recreated neighbor-row mutation"),
            );
        } else if generation == 272 {
            // The reference is silent on repeated terminal deletes after a
            // neighbor recreate; keep the restored prefix image as the ordinary
            // generation-local result.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid repeated terminal neighbor deletion"),
            );
        } else if generation == 275 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"continued-extension".to_vec()),
                )
                .expect("valid continued neighbor recreation"),
            );
        } else if generation == 276 {
            // No special merge rule is specified for deleting the tail neighbor
            // after the prefix delete/restore cycle; retain each generation's
            // ordinary row image, including the restored prefix.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid terminal tail-edge deletion after prefix restoration"),
            );
        } else if generation == 278 {
            // The reference is silent on recreating the tail while its prefix is
            // absent; retain the ordinary generation-local row image.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"tail-only-extension".to_vec()),
                )
                .expect("valid tail recreation without its prefix"),
            );
        } else if generation == 279 {
            // Preserve the ordinary empty image when that tail is deleted again
            // before the prefix is restored.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid terminal tail deletion without its prefix"),
            );
        } else if generation == 281 {
            // Recreate the tail while retaining the prefix row from the prior pin.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"closure-extension".to_vec()),
                )
                .expect("valid tail recreation before closure delete"),
            );
        } else if generation == 284 {
            // The reference does not define a special result for deleting both
            // adjacent edge rows in one activation; retain the ordinary empty image.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid terminal tail deletion with prefix closure"),
            );
        } else if generation == 285 {
            // Reopen the empty edge as a tail-only row before restoring its prefix.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"closure-tail-only".to_vec()),
                )
                .expect("valid tail recreation after edge closure"),
            );
        } else if generation == 287 {
            // Preserve the restored prefix when the closure tail is deleted again.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid tail deletion after prefix restoration"),
            );
        } else if generation == 290 {
            // Recreate the tail after restoring only the prefix from an empty edge.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"remainder-extension".to_vec()),
                )
                .expect("valid tail recreation for remainder closure"),
            );
        } else if generation == 292 {
            // The reference is silent on the last absent-prefix tail delete; keep
            // its ordinary empty image, closing the remainder sequence.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid final tail deletion after prefix closure"),
            );
        } else if generation == 294 {
            // Recreate the tail alongside the prefix restored from the empty edge.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"final-extension".to_vec()),
                )
                .expect("valid final tail recreation beside prefix"),
            );
        } else if generation == 297 {
            // Keep the prefix image when the final sequence deletes its tail edge.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid final tail deletion beside prefix"),
            );
        } else if generation == 299 {
            // Reopen the closed edge as a tail-only row before the final deletion.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"final-tail-only".to_vec()),
                )
                .expect("valid final tail-only recreation"),
            );
        } else if generation == 300 {
            // The reference is silent on deleting the last tail after closure;
            // record the ordinary empty row image as the final historical pin.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid final tail closure delete"),
            );
        } else if generation == 301 {
            // Start another edge round with only the tail restored from emptiness.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"edge-tail-only".to_vec()),
                )
                .expect("valid edge tail-only recreation"),
            );
        } else if generation == 304 {
            // The reference is silent on closing this tail-only edge; keep its
            // ordinary empty image after the tail deletion.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid tail deletion after prefix deletion"),
            );
        } else if generation == 306 {
            // Recreate the tail after the prefix has been restored on its own.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"edge-final-extension".to_vec()),
                )
                .expect("valid final edge extension recreation"),
            );
        } else if generation == 307 {
            // Preserve the prefix row image as the rebuilt tail is deleted.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid tail deletion beside final prefix"),
            );
        } else if generation == 310 {
            // Extend the prefix-only image with the remainder tail.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"edge-remainder-extension".to_vec()),
                )
                .expect("valid remainder tail extension"),
            );
        } else if generation == 313 {
            // Preserve the restored prefix after deleting its neighboring tail.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid remainder tail deletion beside prefix"),
            );
        } else if generation == 315 {
            // Reopen the tail after the closure image is empty.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"edge-final-remainder-tail".to_vec()),
                )
                .expect("valid final remainder tail recreation"),
            );
        } else if generation == 316 {
            // The reference is silent on deleting this final tail-only edge;
            // retain the ordinary empty row image as its boundary pin.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                .expect("valid final remainder tail closure delete"),
            );
        } else if generation == 318 {
            // Restore the edge between the prefix and its final joint closure.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"edge-final-pair".to_vec()),
                )
                .expect("valid final edge pair tail restoration"),
            );
        } else if generation == 321 {
            // The reference is silent on a same-commit prefix and tail closure;
            // record one ordinary empty image for that paired final closure.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid joint edge closure after prefix delete"),
            );
        } else if generation == 322 {
            // Reopen the tail by itself after the paired rows have closed.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"edge-terminal-tail".to_vec()),
                )
                .expect("valid terminal tail-only restoration"),
            );
        } else if generation == 324 {
            // Close the restored prefix and terminal tail together as the last
            // edge image in this proof.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                .expect("valid final joint edge closure"),
            );
        } else if generation == 326 {
            // Restore the final remainder tail beside the prefix.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"closure-final-remainder-tail".to_vec()),
                )
                .expect("valid closure final remainder tail restoration"),
            );
        } else if generation == 327 {
            // Keep the prefix row while its current tail is deleted.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid final remainder tail deletion beside prefix"),
            );
        } else if generation == 328 {
            // Recreate the tail with a new image before the prefix is deleted.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"closure-replayed-remainder-tail".to_vec()),
                )
                .expect("valid replayed remainder tail restoration"),
            );
        } else if generation == 330 {
            // The reference is silent on closing this tail-only edge; retain an
            // ordinary empty image for the final tail deletion.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                .expect("valid final tail-only closure delete"),
            );
        } else if generation == 334 {
            // Rebuild the closure edge tail beside the restored prefix.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"closure-edge-tail".to_vec()),
                )
                .expect("valid closure edge tail recreation"),
            );
        } else if generation == 335 {
            // Verify deleting the closure edge tail preserves its prefix.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid closure edge tail deletion beside prefix"),
            );
        } else if generation == 336 {
            // Recreate the edge with a distinct final image before prefix delete.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"closure-edge-final-tail".to_vec()),
                )
                .expect("valid final closure edge tail recreation"),
            );
        } else if generation == 338 {
            // The reference is silent on closing this tail-only edge; record the
            // ordinary empty image after deleting its final tail.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                .expect("valid final closure edge tail delete"),
            );
        } else if generation == 342 {
            // Restore the final edge tail beside the current prefix.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"delete-edge-final-tail".to_vec()),
                )
                .expect("valid delete-edge tail restoration"),
            );
        } else if generation == 345 {
            // Delete the tail while its restored prefix remains in the image.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid delete-edge tail deletion beside prefix"),
            );
        } else if generation == 347 {
            // Reopen the last tail after the prefix closure.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"delete-edge-terminal-tail".to_vec()),
                )
                .expect("valid terminal delete-edge tail restoration"),
            );
        } else if generation == 348 {
            // The reference is silent on this final tail-only delete; record the
            // ordinary empty row image as its terminal closure pin.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                .expect("valid terminal delete-edge tail closure"),
            );
        } else if generation == 350 {
            // Restore the closure tail beside the prefix.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"closure-tail-first-image".to_vec()),
                )
                .expect("valid closure tail first-image restoration"),
            );
        } else if generation == 351 {
            // Delete that tail while the prefix row remains present.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid closure tail deletion beside prefix"),
            );
        } else if generation == 352 {
            // Reopen the tail with a different image for the final closure path.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"closure-tail-final-image".to_vec()),
                )
                .expect("valid closure tail final-image restoration"),
            );
        } else if generation == 356 {
            // The reference is silent on this final tail-only delete; record the
            // ordinary empty image as its terminal historical pin.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                .expect("valid repeated closure tail delete"),
            );
        } else if generation == 358 {
            // Rebuild the edge tail before exercising the prefix-first close.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"edge-closure-final-image".to_vec()),
                )
                .expect("valid edge closure tail restoration"),
            );
        } else if generation == 361 {
            // Keep the prefix row after deleting its final neighboring tail.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid edge tail deletion beside prefix"),
            );
        } else if generation == 363 {
            // Restore one terminal tail without the prefix after closure.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"edge-terminal-closure-tail".to_vec()),
                )
                .expect("valid terminal closure tail restoration"),
            );
        } else if generation == 364 {
            // The reference is silent on deleting this final tail-only edge;
            // retain the ordinary empty image as its terminal pin.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                .expect("valid terminal closure tail delete"),
            );
        } else if generation == 366 {
            // Restore a second closure edge tail beside the original prefix.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"edge-tail-second-round".to_vec()),
                )
                .expect("valid second-round edge tail restoration"),
            );
        } else if generation == 369 {
            // Delete that tail while its prefix has been restored beside it.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid second-round tail deletion beside prefix"),
            );
        } else if generation == 371 {
            // Reopen the edge tail after the final prefix closure.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"edge-tail-final-round".to_vec()),
                )
                .expect("valid final-round tail restoration"),
            );
        } else if generation == 372 {
            // The reference is silent on this terminal tail-only delete; pin the
            // pragmatic empty row image for its final historical closure.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid final-round tail closure"),
            );
        } else if generation == 374 {
            // Recreate the tail with its prefix, then close the prefix first.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"edge-final-closure-tail".to_vec()),
                )
                .expect("valid edge-final closure tail restoration"),
            );
        } else if generation == 377 {
            // The surviving prefix keeps the tail deletion distinct from closure.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid tail deletion before final prefix closure"),
            );
        } else if generation == 379 {
            // Reopen the tail after the prefix and earlier tail have both closed.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"edge-final-closure-final-tail".to_vec()),
                )
                .expect("valid final tail restoration after prefix closure"),
            );
        } else if generation == 380 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid terminal tail-only closure"),
            );
        } else if generation == 382 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"edge-final-closure-tail-2".to_vec()),
                )
                .expect("valid second edge-closure tail restoration"),
            );
        } else if generation == 383 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"edge-final-closure-tail-3".to_vec()),
                )
                .expect("valid replacement edge-closure tail image"),
            );
        } else if generation == 386 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid tail deletion before its prefix closure"),
            );
        } else if generation == 388 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"edge-final-closure-terminal-tail".to_vec()),
                )
                .expect("valid terminal tail-only image restoration"),
            );
        } else if generation == 389 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid final terminal tail-only closure"),
            );
        } else if generation == 391 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"edge-closure-tail-three".to_vec()),
                )
                .expect("valid edge-closure tail restoration"),
            );
        } else if generation == 392 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"edge-closure-tail-three-replaced".to_vec()),
                )
                .expect("valid replacement of the edge-closure tail"),
            );
        } else if generation == 394 {
            // Restore the prefix and replace the tail in this same generation.
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"edge-closure-tail-three-reopened".to_vec()),
                )
                .expect("valid same-generation tail replacement beside prefix restoration"),
            );
        } else if generation == 395 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid tail closure with prefix retained"),
            );
        } else if generation == 397 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"edge-closure-tail-three-final".to_vec()),
                )
                .expect("valid final tail-only restoration"),
            );
        } else if generation == 398 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid final tail-only closure"),
            );
        } else if generation == 400 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"edge-interplay-tail-first".to_vec()),
                )
                .expect("valid tail-first interplay image"),
            );
        } else if generation == 401 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"edge-interplay-tail-replaced".to_vec()),
                )
                .expect("valid replaced tail-first interplay image"),
            );
        } else if generation == 402 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"edge-interplay-prefix-tail-restored".to_vec()),
                )
                .expect("valid tail restoration with prefix closure"),
            );
        } else if generation == 404 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid tail closure before prefix closure"),
            );
        } else if generation == 406 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"edge-interplay-final-tail".to_vec()),
                )
                .expect("valid final tail restoration after empty closure"),
            );
        } else if generation == 407 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid final tail-only interplay closure"),
            );
        } else if generation == 409 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"edge-closure-interplay-tail".to_vec()),
                )
                .expect("valid paired edge tail restoration"),
            );
        } else if generation == 411 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"edge-closure-interplay-reopened-tail".to_vec()),
                )
                .expect("valid tail replacement with prefix restoration"),
            );
        } else if generation == 412 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid tail-first closure with prefix retained"),
            );
        } else if generation == 414 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"edge-closure-interplay-joint-tail".to_vec()),
                )
                .expect("valid joint prefix-and-tail restoration"),
            );
        } else if generation == 416 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid terminal tail closure after prefix-first delete"),
            );
        } else if generation == 418 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"closure-edge-tail-recreated".to_vec()),
                )
                .expect("valid closure edge tail recreation"),
            );
        } else if generation == 419 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"closure-edge-tail-replaced".to_vec()),
                )
                .expect("valid closure edge tail replacement"),
            );
        } else if generation == 421 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"closure-edge-tail-reopened".to_vec()),
                )
                .expect("valid joint prefix restore and tail replacement"),
            );
        } else if generation == 422 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid tail closure while prefix remains"),
            );
        } else if generation == 423 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"closure-edge-tail-final".to_vec()),
                )
                .expect("valid final closure edge tail image"),
            );
        } else if generation == 425 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid terminal tail-only closure"),
            );
        } else if generation == 427 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"interplay-closure-edge-tail-first".to_vec()),
                )
                .expect("valid first interplay closure edge tail"),
            );
        } else if generation == 428 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"interplay-closure-edge-tail-replaced".to_vec()),
                )
                .expect("valid replacement interplay closure edge tail"),
            );
        } else if generation == 430 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"interplay-closure-edge-tail-reopened".to_vec()),
                )
                .expect("valid joint prefix restore and closure tail replacement"),
            );
        } else if generation == 431 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid closure tail deletion with prefix retained"),
            );
        } else if generation == 432 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"interplay-closure-edge-tail-final".to_vec()),
                )
                .expect("valid final interplay closure edge tail recreation"),
            );
        } else if generation == 434 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid terminal interplay closure edge tail deletion"),
            );
        }
        commit(&state, writer, &mutations, 57).await;

        if (255..=434).contains(&generation) {
            let selected = state
                .select_historical_snapshot(generation)
                .await
                .expect("select prefix restoration boundary pin");
            let descriptor = selected.capture().snapshot().clone();
            let encoded = descriptor.encode().expect("encode prefix restoration pin");
            let decoded = Snapshot::decode_bytes(&encoded).expect("decode prefix restoration pin");
            assert_eq!(decoded, descriptor);
            pins.push((generation, decoded, selected));
        }
    }

    assert_eq!(
        pins.iter().map(|(generation, _, _)| *generation).collect::<Vec<_>>(),
        [
            255, 256, 257, 258, 259, 260, 261, 262, 263, 264, 265, 266, 267, 268, 269, 270, 271,
            272, 273, 274, 275, 276, 277, 278, 279, 280, 281, 282, 283, 284, 285, 286, 287,
            288, 289, 290, 291, 292, 293, 294, 295, 296, 297, 298, 299, 300, 301, 302, 303,
            304, 305, 306, 307, 308, 309, 310, 311, 312, 313, 314, 315, 316, 317, 318, 319,
            320, 321, 322, 323, 324, 325, 326, 327, 328, 329, 330, 331, 332, 333, 334, 335,
            336, 337, 338, 339, 340, 341, 342, 343, 344, 345, 346, 347, 348, 349, 350, 351,
            352, 353, 354, 355, 356, 357, 358, 359, 360, 361, 362, 363, 364, 365, 366, 367,
            368, 369, 370, 371, 372, 373, 374, 375, 376, 377, 378, 379, 380, 381, 382, 383,
            384, 385, 386, 387, 388, 389, 390, 391, 392, 393, 394, 395, 396, 397, 398, 399,
            400, 401, 402, 403, 404, 405, 406, 407, 408, 409, 410, 411, 412, 413, 414, 415, 416,
            417, 418, 419, 420, 421, 422, 423, 424, 425, 426, 427, 428, 429, 430, 431, 432, 433,
            434,
        ]
    );
    assert!(pins.windows(2).all(|pair| {
        pair[0].2.capture().generation_digest() == pair[1].2.capture().generation_digest()
    }));
    for pair in pins.windows(2) {
        assert_ne!(
            pair[0].2.snapshot_id(),
            pair[1].2.snapshot_id(),
            "closure snapshots remain distinct at generations {} and {}",
            pair[0].0,
            pair[1].0
        );
    }

    let expected_rows = |generation| match generation {
        255 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"stable-extension".to_vec()),
        ],
        256 => vec![(vec![5, 0], b"stable-extension".to_vec())],
        257 => vec![
            (vec![5], b"intermediate-prefix".to_vec()),
            (vec![5, 0], b"stable-extension".to_vec()),
        ],
        258 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"stable-extension".to_vec()),
        ],
        259 => vec![(vec![5, 0], b"stable-extension".to_vec())],
        260 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"stable-extension".to_vec()),
        ],
        261 => vec![(vec![5, 0], b"stable-extension".to_vec())],
        262 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"stable-extension".to_vec()),
        ],
        263 => vec![(vec![5, 0], b"stable-extension".to_vec())],
        264 => vec![(vec![5, 0], b"post-delete-extension".to_vec())],
        265 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"post-delete-extension".to_vec()),
        ],
        266 => vec![(vec![5, 0], b"post-delete-extension".to_vec())],
        267 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"post-delete-extension".to_vec()),
        ],
        268 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"post-restoration-extension".to_vec()),
        ],
        269 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"stable-extension".to_vec()),
        ],
        270 => vec![(vec![5], b"original-prefix".to_vec())],
        271 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"stable-extension".to_vec()),
        ],
        272 => vec![(vec![5], b"original-prefix".to_vec())],
        273 => vec![],
        274 => vec![(vec![5], b"original-prefix".to_vec())],
        275 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"continued-extension".to_vec()),
        ],
        276 => vec![(vec![5], b"original-prefix".to_vec())],
        277 => vec![],
        278 => vec![(vec![5, 0], b"tail-only-extension".to_vec())],
        279 => vec![],
        280 => vec![(vec![5], b"original-prefix".to_vec())],
        281 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"closure-extension".to_vec()),
        ],
        282 => vec![(vec![5, 0], b"closure-extension".to_vec())],
        283 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"closure-extension".to_vec()),
        ],
        284 => vec![],
        285 => vec![(vec![5, 0], b"closure-tail-only".to_vec())],
        286 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"closure-tail-only".to_vec()),
        ],
        287 => vec![(vec![5], b"original-prefix".to_vec())],
        288 => vec![],
        289 => vec![(vec![5], b"original-prefix".to_vec())],
        290 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"remainder-extension".to_vec()),
        ],
        291 => vec![(vec![5, 0], b"remainder-extension".to_vec())],
        292 => vec![],
        293 => vec![(vec![5], b"original-prefix".to_vec())],
        294 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"final-extension".to_vec()),
        ],
        295 => vec![(vec![5, 0], b"final-extension".to_vec())],
        296 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"final-extension".to_vec()),
        ],
        297 => vec![(vec![5], b"original-prefix".to_vec())],
        298 => vec![],
        299 => vec![(vec![5, 0], b"final-tail-only".to_vec())],
        300 => vec![],
        301 => vec![(vec![5, 0], b"edge-tail-only".to_vec())],
        302 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"edge-tail-only".to_vec()),
        ],
        303 => vec![(vec![5, 0], b"edge-tail-only".to_vec())],
        304 => vec![],
        305 => vec![(vec![5], b"original-prefix".to_vec())],
        306 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"edge-final-extension".to_vec()),
        ],
        307 => vec![(vec![5], b"original-prefix".to_vec())],
        308 => vec![],
        309 => vec![(vec![5], b"original-prefix".to_vec())],
        310 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"edge-remainder-extension".to_vec()),
        ],
        311 => vec![(vec![5, 0], b"edge-remainder-extension".to_vec())],
        312 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"edge-remainder-extension".to_vec()),
        ],
        313 => vec![(vec![5], b"original-prefix".to_vec())],
        314 => vec![],
        315 => vec![(vec![5, 0], b"edge-final-remainder-tail".to_vec())],
        316 => vec![],
        317 => vec![(vec![5], b"original-prefix".to_vec())],
        318 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"edge-final-pair".to_vec()),
        ],
        319 => vec![(vec![5, 0], b"edge-final-pair".to_vec())],
        320 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"edge-final-pair".to_vec()),
        ],
        321 => vec![],
        322 => vec![(vec![5, 0], b"edge-terminal-tail".to_vec())],
        323 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"edge-terminal-tail".to_vec()),
        ],
        324 => vec![],
        325 => vec![(vec![5], b"original-prefix".to_vec())],
        326 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"closure-final-remainder-tail".to_vec()),
        ],
        327 => vec![(vec![5], b"original-prefix".to_vec())],
        328 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"closure-replayed-remainder-tail".to_vec()),
        ],
        329 => vec![(vec![5, 0], b"closure-replayed-remainder-tail".to_vec())],
        330 => vec![],
        331 => vec![(vec![5], b"original-prefix".to_vec())],
        332 => vec![],
        333 => vec![(vec![5], b"original-prefix".to_vec())],
        334 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"closure-edge-tail".to_vec()),
        ],
        335 => vec![(vec![5], b"original-prefix".to_vec())],
        336 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"closure-edge-final-tail".to_vec()),
        ],
        337 => vec![(vec![5, 0], b"closure-edge-final-tail".to_vec())],
        338 => vec![],
        339 => vec![(vec![5], b"original-prefix".to_vec())],
        340 => vec![],
        341 => vec![(vec![5], b"original-prefix".to_vec())],
        342 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"delete-edge-final-tail".to_vec()),
        ],
        343 => vec![(vec![5, 0], b"delete-edge-final-tail".to_vec())],
        344 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"delete-edge-final-tail".to_vec()),
        ],
        345 => vec![(vec![5], b"original-prefix".to_vec())],
        346 => vec![],
        347 => vec![(vec![5, 0], b"delete-edge-terminal-tail".to_vec())],
        348 => vec![],
        349 => vec![(vec![5], b"original-prefix".to_vec())],
        350 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"closure-tail-first-image".to_vec()),
        ],
        351 => vec![(vec![5], b"original-prefix".to_vec())],
        352 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"closure-tail-final-image".to_vec()),
        ],
        353 => vec![(vec![5, 0], b"closure-tail-final-image".to_vec())],
        354 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"closure-tail-final-image".to_vec()),
        ],
        355 => vec![(vec![5, 0], b"closure-tail-final-image".to_vec())],
        356 => vec![],
        357 => vec![(vec![5], b"original-prefix".to_vec())],
        358 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"edge-closure-final-image".to_vec()),
        ],
        359 => vec![(vec![5, 0], b"edge-closure-final-image".to_vec())],
        360 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"edge-closure-final-image".to_vec()),
        ],
        361 => vec![(vec![5], b"original-prefix".to_vec())],
        362 => vec![],
        363 => vec![(vec![5, 0], b"edge-terminal-closure-tail".to_vec())],
        364 => vec![],
        365 => vec![(vec![5], b"original-prefix".to_vec())],
        366 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"edge-tail-second-round".to_vec()),
        ],
        367 => vec![(vec![5, 0], b"edge-tail-second-round".to_vec())],
        368 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"edge-tail-second-round".to_vec()),
        ],
        369 => vec![(vec![5], b"original-prefix".to_vec())],
        370 => vec![],
        371 => vec![(vec![5, 0], b"edge-tail-final-round".to_vec())],
        372 => vec![],
        373 => vec![(vec![5], b"original-prefix".to_vec())],
        374 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"edge-final-closure-tail".to_vec()),
        ],
        375 => vec![(vec![5, 0], b"edge-final-closure-tail".to_vec())],
        376 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"edge-final-closure-tail".to_vec()),
        ],
        377 => vec![(vec![5], b"original-prefix".to_vec())],
        378 => vec![],
        379 => vec![(vec![5, 0], b"edge-final-closure-final-tail".to_vec())],
        380 => vec![],
        381 => vec![(vec![5], b"original-prefix".to_vec())],
        382 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"edge-final-closure-tail-2".to_vec()),
        ],
        383 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"edge-final-closure-tail-3".to_vec()),
        ],
        384 => vec![(vec![5, 0], b"edge-final-closure-tail-3".to_vec())],
        385 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"edge-final-closure-tail-3".to_vec()),
        ],
        386 => vec![(vec![5], b"original-prefix".to_vec())],
        387 => vec![],
        388 => vec![(vec![5, 0], b"edge-final-closure-terminal-tail".to_vec())],
        389 => vec![],
        390 => vec![(vec![5], b"original-prefix".to_vec())],
        391 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"edge-closure-tail-three".to_vec()),
        ],
        392 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"edge-closure-tail-three-replaced".to_vec()),
        ],
        393 => vec![(vec![5, 0], b"edge-closure-tail-three-replaced".to_vec())],
        394 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"edge-closure-tail-three-reopened".to_vec()),
        ],
        395 => vec![(vec![5], b"original-prefix".to_vec())],
        396 => vec![],
        397 => vec![(vec![5, 0], b"edge-closure-tail-three-final".to_vec())],
        398 => vec![],
        399 => vec![(vec![5], b"original-prefix".to_vec())],
        400 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"edge-interplay-tail-first".to_vec()),
        ],
        401 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"edge-interplay-tail-replaced".to_vec()),
        ],
        402 => vec![(vec![5, 0], b"edge-interplay-prefix-tail-restored".to_vec())],
        403 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"edge-interplay-prefix-tail-restored".to_vec()),
        ],
        404 => vec![(vec![5], b"original-prefix".to_vec())],
        405 => vec![],
        406 => vec![(vec![5, 0], b"edge-interplay-final-tail".to_vec())],
        407 => vec![],
        408 => vec![(vec![5], b"original-prefix".to_vec())],
        409 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"edge-closure-interplay-tail".to_vec()),
        ],
        410 => vec![(vec![5, 0], b"edge-closure-interplay-tail".to_vec())],
        411 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"edge-closure-interplay-reopened-tail".to_vec()),
        ],
        412 => vec![(vec![5], b"original-prefix".to_vec())],
        413 => vec![],
        414 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"edge-closure-interplay-joint-tail".to_vec()),
        ],
        415 => vec![(vec![5, 0], b"edge-closure-interplay-joint-tail".to_vec())],
        416 => vec![],
        417 => vec![(vec![5], b"original-prefix".to_vec())],
        418 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"closure-edge-tail-recreated".to_vec()),
        ],
        419 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"closure-edge-tail-replaced".to_vec()),
        ],
        420 => vec![(vec![5, 0], b"closure-edge-tail-replaced".to_vec())],
        421 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"closure-edge-tail-reopened".to_vec()),
        ],
        422 => vec![(vec![5], b"original-prefix".to_vec())],
        423 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"closure-edge-tail-final".to_vec()),
        ],
        424 => vec![(vec![5, 0], b"closure-edge-tail-final".to_vec())],
        425 => vec![],
        426 => vec![(vec![5], b"original-prefix".to_vec())],
        427 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"interplay-closure-edge-tail-first".to_vec()),
        ],
        428 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"interplay-closure-edge-tail-replaced".to_vec()),
        ],
        429 => vec![(vec![5, 0], b"interplay-closure-edge-tail-replaced".to_vec())],
        430 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"interplay-closure-edge-tail-reopened".to_vec()),
        ],
        431 => vec![(vec![5], b"original-prefix".to_vec())],
        432 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"interplay-closure-edge-tail-final".to_vec()),
        ],
        433 => vec![(vec![5, 0], b"interplay-closure-edge-tail-final".to_vec())],
        434 => vec![],
        _ => unreachable!("only closure remainder boundary generations are read"),
    };
    assert_eq!(expected_rows(255), expected_rows(258));
    assert_eq!(expected_rows(255), expected_rows(260));
    assert_eq!(expected_rows(255), expected_rows(262));
    assert_eq!(expected_rows(265), expected_rows(267));
    assert_eq!(expected_rows(267)[0], expected_rows(268)[0]);
    assert_eq!(expected_rows(255), expected_rows(269));
    assert_eq!(expected_rows(269)[0], expected_rows(270)[0]);
    assert_eq!(expected_rows(255), expected_rows(271));
    assert_eq!(expected_rows(271)[0], expected_rows(272)[0]);
    assert_ne!(expected_rows(271), expected_rows(272));
    assert!(expected_rows(273).is_empty());
    assert_eq!(expected_rows(272), expected_rows(274));
    assert_eq!(expected_rows(274)[0], expected_rows(275)[0]);
    assert_eq!(expected_rows(276), expected_rows(274));
    assert_ne!(expected_rows(275), expected_rows(276));
    assert!(expected_rows(277).is_empty());
    assert_eq!(
        expected_rows(278),
        vec![(vec![5, 0], b"tail-only-extension".to_vec())]
    );
    assert!(expected_rows(279).is_empty());
    assert_eq!(expected_rows(280), expected_rows(274));
    assert_eq!(expected_rows(281)[0], expected_rows(280)[0]);
    assert_eq!(expected_rows(282), vec![(vec![5, 0], b"closure-extension".to_vec())]);
    assert_eq!(expected_rows(283), expected_rows(281));
    assert!(expected_rows(284).is_empty());
    assert_eq!(expected_rows(285), vec![(vec![5, 0], b"closure-tail-only".to_vec())]);
    assert_eq!(expected_rows(286)[0], expected_rows(283)[0]);
    assert_eq!(expected_rows(287), vec![(vec![5], b"original-prefix".to_vec())]);
    assert!(expected_rows(288).is_empty());
    assert_eq!(expected_rows(289), expected_rows(280));
    assert_eq!(expected_rows(290)[0], expected_rows(289)[0]);
    assert_eq!(expected_rows(291), vec![(vec![5, 0], b"remainder-extension".to_vec())]);
    assert!(expected_rows(292).is_empty());
    assert_eq!(expected_rows(293), expected_rows(280));
    assert_eq!(expected_rows(294)[0], expected_rows(293)[0]);
    assert_eq!(expected_rows(295), vec![(vec![5, 0], b"final-extension".to_vec())]);
    assert_eq!(expected_rows(296), expected_rows(294));
    assert_eq!(expected_rows(297), expected_rows(293));
    assert!(expected_rows(298).is_empty());
    assert_eq!(expected_rows(299), vec![(vec![5, 0], b"final-tail-only".to_vec())]);
    assert!(expected_rows(300).is_empty());
    assert_eq!(expected_rows(301), vec![(vec![5, 0], b"edge-tail-only".to_vec())]);
    assert_eq!(
        expected_rows(302),
        vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"edge-tail-only".to_vec()),
        ]
    );
    assert_eq!(expected_rows(303), expected_rows(301));
    assert!(expected_rows(304).is_empty());
    assert_eq!(expected_rows(305), expected_rows(293));
    assert_eq!(expected_rows(306)[0], expected_rows(305)[0]);
    assert_eq!(expected_rows(307), expected_rows(305));
    assert!(expected_rows(308).is_empty());
    assert_eq!(expected_rows(309), expected_rows(305));
    assert_eq!(expected_rows(310)[0], expected_rows(309)[0]);
    assert_eq!(
        expected_rows(311),
        vec![(vec![5, 0], b"edge-remainder-extension".to_vec())]
    );
    assert_eq!(expected_rows(312), expected_rows(310));
    assert_eq!(expected_rows(313), expected_rows(309));
    assert!(expected_rows(314).is_empty());
    assert_eq!(
        expected_rows(315),
        vec![(vec![5, 0], b"edge-final-remainder-tail".to_vec())]
    );
    assert!(expected_rows(316).is_empty());
    assert_eq!(expected_rows(317), expected_rows(309));
    assert_eq!(expected_rows(318)[0], expected_rows(317)[0]);
    assert_eq!(
        expected_rows(319),
        vec![(vec![5, 0], b"edge-final-pair".to_vec())]
    );
    assert_eq!(expected_rows(320), expected_rows(318));
    assert!(expected_rows(321).is_empty());
    assert_eq!(
        expected_rows(322),
        vec![(vec![5, 0], b"edge-terminal-tail".to_vec())]
    );
    assert_eq!(expected_rows(323)[1], expected_rows(322)[0]);
    assert!(expected_rows(324).is_empty());
    assert_eq!(expected_rows(325), expected_rows(317));
    assert_eq!(expected_rows(326)[0], expected_rows(325)[0]);
    assert_eq!(expected_rows(327), expected_rows(325));
    assert_eq!(expected_rows(328)[0], expected_rows(327)[0]);
    assert_eq!(
        expected_rows(329),
        vec![(vec![5, 0], b"closure-replayed-remainder-tail".to_vec())]
    );
    assert!(expected_rows(330).is_empty());
    assert_eq!(expected_rows(331), expected_rows(325));
    assert!(expected_rows(332).is_empty());
    assert_eq!(expected_rows(333), expected_rows(331));
    assert_eq!(expected_rows(334)[0], expected_rows(333)[0]);
    assert_eq!(expected_rows(335), expected_rows(333));
    assert_eq!(expected_rows(336)[0], expected_rows(335)[0]);
    assert_eq!(
        expected_rows(337),
        vec![(vec![5, 0], b"closure-edge-final-tail".to_vec())]
    );
    assert!(expected_rows(338).is_empty());
    assert_eq!(expected_rows(339), expected_rows(333));
    assert!(expected_rows(340).is_empty());
    assert_eq!(expected_rows(341), expected_rows(339));
    assert_eq!(expected_rows(342)[0], expected_rows(341)[0]);
    assert_eq!(expected_rows(343), vec![(vec![5, 0], b"delete-edge-final-tail".to_vec())]);
    assert_eq!(expected_rows(344), expected_rows(342));
    assert_eq!(expected_rows(345), expected_rows(341));
    assert!(expected_rows(346).is_empty());
    assert_eq!(
        expected_rows(347),
        vec![(vec![5, 0], b"delete-edge-terminal-tail".to_vec())]
    );
    assert!(expected_rows(348).is_empty());
    assert_eq!(expected_rows(349), expected_rows(341));
    assert_eq!(expected_rows(350)[0], expected_rows(349)[0]);
    assert_eq!(expected_rows(351), expected_rows(349));
    assert_eq!(expected_rows(352)[0], expected_rows(351)[0]);
    assert_eq!(
        expected_rows(353),
        vec![(vec![5, 0], b"closure-tail-final-image".to_vec())]
    );
    assert_eq!(expected_rows(354), expected_rows(352));
    assert_eq!(
        expected_rows(355),
        vec![(vec![5, 0], b"closure-tail-final-image".to_vec())]
    );
    assert!(expected_rows(356).is_empty());
    assert_eq!(expected_rows(357), expected_rows(349));
    assert_eq!(expected_rows(358)[0], expected_rows(357)[0]);
    assert_eq!(
        expected_rows(359),
        vec![(vec![5, 0], b"edge-closure-final-image".to_vec())]
    );
    assert_eq!(expected_rows(360), expected_rows(358));
    assert_eq!(expected_rows(361), expected_rows(357));
    assert!(expected_rows(362).is_empty());
    assert_eq!(
        expected_rows(363),
        vec![(vec![5, 0], b"edge-terminal-closure-tail".to_vec())]
    );
    assert!(expected_rows(364).is_empty());
    assert_eq!(expected_rows(365), expected_rows(357));
    assert_eq!(expected_rows(366)[0], expected_rows(365)[0]);
    assert_eq!(
        expected_rows(367),
        vec![(vec![5, 0], b"edge-tail-second-round".to_vec())]
    );
    assert_eq!(expected_rows(368), expected_rows(366));
    assert_eq!(expected_rows(369), expected_rows(365));
    assert!(expected_rows(370).is_empty());
    assert_eq!(
        expected_rows(371),
        vec![(vec![5, 0], b"edge-tail-final-round".to_vec())]
    );
    assert!(expected_rows(372).is_empty());
    assert_eq!(expected_rows(373), vec![(vec![5], b"original-prefix".to_vec())]);
    assert_eq!(expected_rows(374)[1], expected_rows(375)[0]);
    assert_eq!(expected_rows(375)[0], expected_rows(376)[1]);
    assert_eq!(expected_rows(376)[0], expected_rows(377)[0]);
    assert!(expected_rows(378).is_empty());
    assert_eq!(
        expected_rows(379),
        vec![(vec![5, 0], b"edge-final-closure-final-tail".to_vec())]
    );
    assert!(expected_rows(380).is_empty());
    assert_eq!(expected_rows(381), vec![(vec![5], b"original-prefix".to_vec())]);
    assert_eq!(expected_rows(382)[0], expected_rows(383)[0]);
    assert_ne!(expected_rows(382)[1], expected_rows(383)[1]);
    assert_eq!(expected_rows(383)[1], expected_rows(384)[0]);
    assert_eq!(expected_rows(384)[0], expected_rows(385)[1]);
    assert_eq!(expected_rows(385)[0], expected_rows(386)[0]);
    assert!(expected_rows(387).is_empty());
    assert_eq!(
        expected_rows(388),
        vec![(vec![5, 0], b"edge-final-closure-terminal-tail".to_vec())]
    );
    assert!(expected_rows(389).is_empty());
    assert_eq!(expected_rows(390), vec![(vec![5], b"original-prefix".to_vec())]);
    assert_eq!(expected_rows(391)[0], expected_rows(392)[0]);
    assert_ne!(expected_rows(391)[1], expected_rows(392)[1]);
    assert_eq!(expected_rows(392)[1], expected_rows(393)[0]);
    assert_eq!(expected_rows(394)[0], expected_rows(390)[0]);
    assert_eq!(
        expected_rows(394)[1],
        (vec![5, 0], b"edge-closure-tail-three-reopened".to_vec())
    );
    assert_eq!(expected_rows(395)[0], expected_rows(394)[0]);
    assert!(expected_rows(396).is_empty());
    assert_eq!(
        expected_rows(397),
        vec![(vec![5, 0], b"edge-closure-tail-three-final".to_vec())]
    );
    assert!(expected_rows(398).is_empty());
    assert_eq!(expected_rows(399), vec![(vec![5], b"original-prefix".to_vec())]);
    assert_eq!(expected_rows(400)[0], expected_rows(401)[0]);
    assert_ne!(expected_rows(400)[1], expected_rows(401)[1]);
    assert_ne!(expected_rows(401)[1], expected_rows(402)[0]);
    assert_eq!(expected_rows(402)[0], expected_rows(403)[1]);
    assert_eq!(expected_rows(403)[0], expected_rows(404)[0]);
    assert!(expected_rows(405).is_empty());
    assert_eq!(
        expected_rows(406),
        vec![(vec![5, 0], b"edge-interplay-final-tail".to_vec())]
    );
    assert!(expected_rows(407).is_empty());
    assert_eq!(expected_rows(408), vec![(vec![5], b"original-prefix".to_vec())]);
    assert_eq!(expected_rows(409)[1], expected_rows(410)[0]);
    assert_eq!(
        expected_rows(410)[0],
        (vec![5, 0], b"edge-closure-interplay-tail".to_vec())
    );
    assert_eq!(expected_rows(411)[0], expected_rows(408)[0]);
    assert_ne!(expected_rows(410)[0], expected_rows(411)[1]);
    assert_eq!(expected_rows(411)[0], expected_rows(412)[0]);
    assert!(expected_rows(413).is_empty());
    assert_eq!(expected_rows(414)[1], expected_rows(415)[0]);
    assert_eq!(expected_rows(414)[0], expected_rows(408)[0]);
    assert!(expected_rows(416).is_empty());
    assert_eq!(expected_rows(417), vec![(vec![5], b"original-prefix".to_vec())]);
    assert_eq!(expected_rows(418)[0], expected_rows(419)[0]);
    assert_ne!(expected_rows(418)[1], expected_rows(419)[1]);
    assert_eq!(expected_rows(419)[1], expected_rows(420)[0]);
    assert_eq!(expected_rows(421)[0], expected_rows(417)[0]);
    assert_ne!(expected_rows(420)[0], expected_rows(421)[1]);
    assert_eq!(expected_rows(421)[0], expected_rows(422)[0]);
    assert_eq!(expected_rows(422)[0], expected_rows(423)[0]);
    assert_eq!(expected_rows(423)[1], expected_rows(424)[0]);
    assert!(expected_rows(425).is_empty());
    assert_eq!(expected_rows(426), vec![(vec![5], b"original-prefix".to_vec())]);
    assert_eq!(expected_rows(427)[0], expected_rows(428)[0]);
    assert_ne!(expected_rows(427)[1], expected_rows(428)[1]);
    assert_eq!(expected_rows(428)[1], expected_rows(429)[0]);
    assert_eq!(expected_rows(430)[0], expected_rows(426)[0]);
    assert_ne!(expected_rows(429)[0], expected_rows(430)[1]);
    assert_eq!(expected_rows(430)[0], expected_rows(431)[0]);
    assert_eq!(expected_rows(430)[0], expected_rows(432)[0]);
    assert_eq!(expected_rows(432)[1], expected_rows(433)[0]);
    assert!(expected_rows(434).is_empty());

    for (generation, descriptor, selected) in &pins {
        let resolved = state
            .resolve_historical_snapshot(descriptor)
            .await
            .expect("resolve prefix restoration pin after later generations");
        assert_eq!(&resolved, selected);
        assert_eq!(
            state
                .read_table_at(&resolved, "records")
                .await
                .expect("read exact prefix restoration pin")
                .rows(),
            expected_rows(*generation).as_slice(),
            "generation {generation} has its captured prefix and extension rows"
        );
    }

    drop(state);
    let reopened = RuntimeState::open(&repository, identity, [64; 32])
        .await
        .expect("reopen runtime");
    for (generation, descriptor, selected) in &pins {
        let resolved = reopened
            .resolve_historical_snapshot(descriptor)
            .await
            .expect("resolve prefix restoration pin after reopen");
        assert_eq!(resolved, *selected);
        assert_eq!(resolved.generation(), *generation);
        assert_eq!(
            reopened
                .read_table_at(&resolved, "records")
                .await
                .expect("read retained prefix restoration pin")
                .rows(),
            expected_rows(*generation).as_slice()
        );
    }
}
