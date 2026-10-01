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

    for generation in 1..=865_u64 {
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
        // Continue from empty with a prefix and tail, remove the prefix before
        // replacing the tail, restore both with another tail image, then delete
        // and recreate the tail before closing the prefix and final tail. This
        // ordering is unspecified by the reference, so preserve each row image.
        // Start another closure round with both rows, replace and remove the
        // tail around a prefix delete, restore both together, then close and
        // reopen the tail before deleting the prefix and tail separately. The
        // reference does not define this interplay order; pin its row images.
        // Start from a prefix, replace its tail, close prefix and tail in turn,
        // reopen a tail alone, add a new paired image, then close tail and
        // prefix separately. The reference leaves this order open; pin each
        // resulting image explicitly.
        // Continue with a paired tail replacement, close the tail while the
        // prefix stays, then delete the prefix before replacing and restoring
        // both rows. Close the prefix and tail in order; the reference is silent
        // on this interplay, so retain every concrete generation image.
        // Reopen a pair, change the tail before and after prefix deletion, then
        // close both, rebuild a prefix and jointly update it with a tail. Close
        // tail and prefix in turn; this case ordering is unspecified, so pin each
        // row image directly.
        // Continue with a paired tail replacement, remove the prefix, replace
        // the absent-prefix tail, and close it. Restore the prefix, jointly
        // replace both rows, then close the tail and prefix separately. The
        // reference is silent on this terminal case ordering, so pin each image.
        // Add and replace a tail with the prefix present, remove the prefix before
        // replacing and deleting the tail, then restore and jointly close the edge.
        // The reference leaves this case ordering open; retain each row image as
        // the fixture proof.
        // Add another paired tail, delete the prefix alongside a replacement,
        // restore both before deleting and recreating the tail, then close the
        // prefix before the absent-prefix tail. Pin the final prefix restoration;
        // the reference does not specify this case ordering.
        // Begin with another pair and tail replacement, close the prefix before
        // changing and deleting the tail, then restore and jointly update both
        // rows. Delete tail and prefix in turn; the reference leaves this order
        // open, so pin every concrete image.
        // Reopen the edge with another tail, delete and recreate that tail beside
        // the prefix, then close the prefix before replacing the tail. Restore
        // both rows, close the prefix and tail separately, and pin the final
        // prefix-only image because the reference leaves this order unspecified.
        // Replace the tail before and after deleting the prefix, jointly restore
        // both rows, and delete the tail while the prefix remains. Reopen the
        // tail, then close prefix and tail in turn; record each image because the
        // reference does not prescribe this repeated closure order.
        // Change the tail around a prefix-first close, restore both rows jointly,
        // then close tail and prefix separately. Finish with a tail-only
        // recreation and closure; pin each image because the reference is silent
        // on this continued interplay.
        // Replace and delete the tail while the prefix is present, then delete
        // the prefix and change the remaining tail. Restore both rows together,
        // close them in turn, and replay one final tail-only image before closing
        // it; the reference leaves this extended ordering open.
        // Continue from empty with another replaced tail, close the prefix first,
        // then restore and jointly update the edge. Close both rows separately,
        // rebuild the prefix and tail, and finish with a final prefix-first
        // closure; the reference is silent on this ordering.
        // Replace the tail across another prefix-first closure, jointly restore
        // both rows, then close prefix and tail separately. Rebuild the edge and
        // close it once more; pin each image because the reference does not
        // define this repeated ordering.
        // Replace the tail through another closure sequence, delete and recreate
        // it around prefix deletion, then jointly restore the edge. Finish with
        // separate prefix and tail closes; pin the order because the reference
        // leaves these repeated edge cases unspecified.
        // Replay the sequence with another tail image: replace, close and reopen
        // the tail across prefix closure, then jointly restore and close each row
        // separately. The reference does not specify this continued ordering.
        // Exercise one more closure round with distinct tail bytes, pinning the
        // prefix-first delete, absent-prefix update, joint restore, and final
        // tail-only close where the reference remains silent.
        // Continue with a new tail image and retain the same edge sequence so
        // historical pins prove the replace, prefix close, joint restore, and
        // terminal tail delete across another unspecified closure ordering.
        // Close another recreated tail after a prefix-first delete, then prove
        // the absent-prefix replacement, joint restoration, and final edge
        // closure with generation-specific row images.
        // Repeat the closure ordering with a distinct seventeenth tail image so
        // each replacement, prefix close, joint restore, and final tail delete
        // remains independently pinned.
        // Add an eighteenth tail image and pin the same prefix/tail closure
        // interplay, including replacement without the prefix and the terminal
        // tail-only deletion that the reference leaves unspecified.
        // Repeat the sequence once more with a separate tail value, retaining
        // snapshots for replacement, prefix-first closure, joint restoration,
        // and final tail deletion while the reference is silent.
        // Add a twentieth image through replacement and recreation, close the
        // prefix before and after joint restoration, and pin the final tail-only
        // deletion as another unspecified edge ordering.
        // Record a twenty-first image through the same tail/prefix interplay,
        // with pins for replacement, prefix closure, joint restoration, and the
        // final delete where the reference does not prescribe ordering.
        // Continue with a twenty-second tail image: replace it, delete and
        // recreate it beside the prefix, close the prefix first, replace the
        // absent-prefix tail, jointly restore both rows, then close the tail
        // before another prefix-first tail-only closure. The reference is silent
        // on this continued case ordering, so pin each resulting row image.
        // Repeat the closure edge sequence with a distinct twenty-third tail:
        // replace and delete it, recreate it, close the prefix first, replace
        // the absent-prefix tail, restore both rows together, then close the
        // tail and prefix separately before one final tail-only delete. The
        // reference is silent on this ordering, so keep each image pinned.
        // Continue with a twenty-fifth tail: replace, delete and recreate it,
        // close the prefix first, replace the absent-prefix tail, restore both
        // rows, then delete the tail and prefix separately before closing one
        // final tail-only image. The reference leaves this order open, so pin
        // every concrete row image.
        // Add the twenty-sixth tail using the same order with fresh bytes:
        // replace, delete and recreate it, close the prefix first, replace the
        // absent-prefix tail, restore both rows, then close tail and prefix
        // separately before its terminal tail-only deletion. Pin each image
        // because the reference does not define this repeated edge ordering.
        // Continue with tail twenty-seven using distinct bytes: replace and
        // delete/recreate it, close the prefix first, replace the tail with the
        // prefix absent, restore the pair, then close tail and prefix in turn
        // before one last tail-only deletion. The reference is silent on this
        // repeat, so retain every generation's row image.
        // Continue with a twenty-eighth tail and the same concrete closure
        // order: replace, delete and recreate, close the prefix first, replace
        // the tail without the prefix, restore both rows, then delete the tail
        // before closing prefix and tail separately. The reference is silent
        // on this repeated edge case, so keep every row image pinned.
        // Repeat this closure path with a twenty-ninth tail: replace it,
        // delete/recreate it, close the prefix first, replace the tail alone,
        // jointly restore the pair, then close tail and prefix separately
        // before the terminal tail-only deletion. The reference is silent on
        // this ordering, so retain the explicit historical row images.
        // Add a thirtieth tail with another replacement and closure pass:
        // delete/recreate the tail, delete the prefix first, replace the
        // tail alone, jointly restore both rows, then close tail and prefix
        // separately before the terminal tail-only delete. The reference does
        // not define this repeated ordering, so pin its concrete row images.
        // Continue with a thirty-first distinct tail: replace it, delete and
        // recreate it, close the prefix first, replace the tail with no prefix,
        // restore both rows, then close tail and prefix separately before its
        // terminal tail-only delete. The reference is silent on this ordering;
        // pin each concrete historical image.
        // Add a twenty-fourth distinct tail through the same closure edge
        // order: replace, delete and recreate beside the prefix, close the
        // prefix first, replace the tail alone, restore both rows, and close
        // the tail before a prefix-first final tail deletion. The reference is
        // silent on this continued ordering, so retain every row image.
        // Continue once more with a thirty-second tail: replace and recreate it,
        // close the prefix first, replace the tail without a prefix, jointly
        // restore both rows, then close tail and prefix separately before the
        // final tail-only deletion. The reference leaves this order unspecified;
        // the fixture-backed proof pins every concrete row image.
        // Repeat with a thirty-third tail image, preserving the same replacement,
        // prefix-first close, absent-prefix update, joint restore, and separate
        // terminal closes. The reference is silent on this repeated ordering.
        // Add the thirty-fourth tail image and pin the same edge interplay through
        // its final tail-only close; each row image remains explicit in the fixture
        // proof because the reference does not define this repeated sequence.
        // Continue once more with a thirty-fifth tail, including the prefix-first
        // close, absent-prefix replacement, joint restore, and final tail-only
        // deletion. The reference leaves this repeated ordering unspecified.
        // Pin a thirty-sixth tail through the same closure interplay, including
        // its absent-prefix replacement and final empty image; the reference
        // remains silent on this repeated edge ordering.
        // Continue once more with the prefix deleted before the final tail close;
        // pin every replacement and restore because the reference does not define
        // this thirty-seventh closure ordering.
        let prefix_value = match generation {
            256 | 259 | 261 | 263 | 264 | 266 | 273 | 277 | 282 | 284 | 288 | 291 | 295 | 298
            | 303 | 308 | 311 | 314 | 319 | 321 | 324 | 329 | 332 | 337 | 340 | 343 | 346
            | 353 | 355 | 359 | 362 | 367 | 370 | 375 | 378 | 384 | 387 | 393 | 396 | 402
            | 405 | 410 | 413 | 415 | 420 | 424 | 429 | 433 | 437 | 442 | 446 | 451 | 456
            | 461 | 466 | 469 | 473 | 479 | 482 | 484 | 488 | 491 | 493 | 497 | 499 | 503
            | 505 | 509 | 511 | 515 | 520 | 523 | 528 | 533 | 538 | 543 | 551 | 554 | 560
            | 563 | 567 | 571 | 576 | 584 | 589 | 595 | 600 | 606 | 611 | 617 | 622 | 628
            | 633 | 639 | 644 | 650 | 655 | 661 | 666 | 672 | 677 | 683 | 688 | 694 | 699 | 705
            | 710 | 716 | 721 | 727 | 732 | 738 | 743 | 749 | 754 | 760 | 765 | 771 | 776 | 782
            | 787 | 793 | 798 | 804 | 809 | 815 | 820 | 826 | 831 | 837 | 842 | 848 | 853 | 859
            | 864 => None,
            257 => Some(b"intermediate-prefix".to_vec()),
            265 => Some(b"original-prefix".to_vec()),
            477 => Some(b"case-closure-prefix-final".to_vec()),
            486 => Some(b"case-closure-edge-prefix-two-final".to_vec()),
            495 => Some(b"case-closure-edge-prefix-three-final".to_vec()),
            500 => Some(b"case-closure-edge-prefix-four-restored".to_vec()),
            513 => Some(b"case-closure-edge-prefix-five-final".to_vec()),
            522 => Some(b"case-closure-edge-prefix-six-final".to_vec()),
            530 => Some(b"case-closure-edge-prefix-seven-final".to_vec()),
            540 => Some(b"case-closure-edge-prefix-eight-final".to_vec()),
            553 => Some(b"case-closure-edge-prefix-nine-final".to_vec()),
            562 => Some(b"case-closure-edge-prefix-ten-final".to_vec()),
            573 => Some(b"case-closure-edge-prefix-eleven-final".to_vec()),
            586 => Some(b"case-closure-edge-prefix-twelve-final".to_vec()),
            597 => Some(b"case-closure-edge-prefix-thirteen-final".to_vec()),
            608 => Some(b"case-closure-edge-prefix-fourteen-final".to_vec()),
            619 => Some(b"case-closure-edge-prefix-fifteen-final".to_vec()),
            630 => Some(b"case-closure-edge-prefix-sixteen-final".to_vec()),
            641 => Some(b"case-closure-edge-prefix-seventeen-final".to_vec()),
            652 => Some(b"case-closure-edge-prefix-eighteen-final".to_vec()),
            663 => Some(b"case-closure-edge-prefix-nineteen-final".to_vec()),
            674 => Some(b"case-closure-edge-prefix-twenty-final".to_vec()),
            685 => Some(b"case-closure-edge-prefix-twenty-one-final".to_vec()),
            696 => Some(b"case-closure-edge-prefix-twenty-two-final".to_vec()),
            707 => Some(b"case-closure-edge-prefix-twenty-three-final".to_vec()),
            718 => Some(b"case-closure-edge-prefix-twenty-four-final".to_vec()),
            729 => Some(b"case-closure-edge-prefix-twenty-five-final".to_vec()),
            740 => Some(b"case-closure-edge-prefix-twenty-six-final".to_vec()),
            751 => Some(b"case-closure-edge-prefix-twenty-seven-final".to_vec()),
            762 => Some(b"case-closure-edge-prefix-twenty-eight-final".to_vec()),
            773 => Some(b"case-closure-edge-prefix-twenty-nine-final".to_vec()),
            784 => Some(b"case-closure-edge-prefix-thirty-final".to_vec()),
            795 => Some(b"case-closure-edge-prefix-thirty-one-final".to_vec()),
            806 => Some(b"case-closure-edge-prefix-thirty-two-final".to_vec()),
            817 => Some(b"case-closure-edge-prefix-thirty-three-final".to_vec()),
            828 => Some(b"case-closure-edge-prefix-thirty-four-final".to_vec()),
            839 => Some(b"case-closure-edge-prefix-thirty-five-final".to_vec()),
            850 => Some(b"case-closure-edge-prefix-thirty-six-final".to_vec()),
            861 => Some(b"case-closure-edge-prefix-thirty-seven-final".to_vec()),
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
                | 425 | 427 | 428 | 431 | 432 | 434 | 436 | 438 | 440 | 441 | 443 | 445 | 447
                | 449 | 450 | 452 | 454 | 455 | 457 | 458 | 460 | 463 | 464 | 465 | 467 | 470
                | 472 | 474 | 475 | 478 | 481 | 483 | 484 | 487 | 490 | 492 | 493 | 496
                | 501 | 502 | 504 | 505 | 508 | 510 | 511 | 514 | 517 | 518 | 519 | 521 | 524
                | 527 | 529 | 531 | 532 | 534 | 537 | 539 | 541 | 542 | 544 | 545 | 546
                | 548 | 549 | 550 | 552 | 555 | 556 | 557 | 559 | 561 | 564 | 566 | 568
                | 570 | 572 | 574 | 575 | 577 | 578 | 579 | 581 | 582 | 583 | 585 | 587 | 588 | 590
                | 592 | 593 | 594 | 596 | 598 | 599 | 601 | 603 | 604 | 605 | 607 | 609 | 610 | 612
                | 614 | 615 | 616 | 618 | 620 | 621 | 623 | 625 | 626 | 627 | 629 | 631 | 632 | 634
                | 636 | 637 | 638 | 640 | 642 | 643 | 645 | 647 | 648 | 649 | 651 | 653 | 654 | 656
                | 658 | 659 | 660 | 662 | 664 | 665 | 667 | 669 | 670 | 671 | 673 | 675 | 676 | 678
                | 680 | 681 | 682 | 684 | 686 | 687 | 689 | 691 | 692 | 693 | 695 | 697 | 698 | 700
                | 702 | 703 | 704 | 706 | 708 | 709 | 711 | 713 | 714 | 715 | 717 | 719 | 720
                | 722 | 724 | 725 | 726 | 728 | 730 | 731 | 733 | 735 | 736 | 737 | 739 | 741 | 742
                | 744 | 746 | 747 | 748 | 750 | 752 | 753 | 755 | 757 | 758 | 759 | 761 | 763 | 764
                | 766 | 768 | 769 | 770 | 772 | 774 | 775 | 777 | 779 | 780 | 781 | 783 | 785 | 786
                | 788 | 790 | 791 | 792 | 794 | 796 | 797 | 799 | 810 | 821 | 832 | 843 | 854
                | 865
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
        } else if generation == 436 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"edge-tail-closure-edge-first".to_vec()),
                )
                .expect("valid first edge tail closure image"),
            );
        } else if generation == 438 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"edge-tail-closure-edge-replaced".to_vec()),
                )
                .expect("valid tail-only closure edge replacement"),
            );
        } else if generation == 439 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"edge-tail-closure-edge-restored".to_vec()),
                )
                .expect("valid joint prefix and closure edge tail restoration"),
            );
        } else if generation == 440 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid tail closure while prefix survives"),
            );
        } else if generation == 441 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"edge-tail-closure-edge-final".to_vec()),
                )
                .expect("valid final closure edge tail recreation"),
            );
        } else if generation == 443 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid terminal closure edge tail deletion"),
            );
        } else if generation == 444 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"interplay-closure-edge-tail-two-first".to_vec()),
                )
                .expect("valid joint prefix and first closure edge tail"),
            );
        } else if generation == 445 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"interplay-closure-edge-tail-two-replaced".to_vec()),
                )
                .expect("valid second closure edge tail image"),
            );
        } else if generation == 447 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"interplay-closure-edge-tail-two-absent-prefix".to_vec()),
                )
                .expect("valid tail replacement with prefix absent"),
            );
        } else if generation == 448 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"interplay-closure-edge-tail-two-reopened".to_vec()),
                )
                .expect("valid joint prefix and closure tail restoration"),
            );
        } else if generation == 449 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid closure tail deletion with prefix retained"),
            );
        } else if generation == 450 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"interplay-closure-edge-tail-two-final".to_vec()),
                )
                .expect("valid final closure edge tail recreation"),
            );
        } else if generation == 452 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid terminal interplay closure tail deletion"),
            );
        } else if generation == 454 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"closure-edge-interplay-tail-first".to_vec()),
                )
                .expect("valid first closure edge interplay tail"),
            );
        } else if generation == 455 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"closure-edge-interplay-tail-replaced".to_vec()),
                )
                .expect("valid replacement closure edge interplay tail"),
            );
        } else if generation == 457 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid tail closure after prefix closure"),
            );
        } else if generation == 458 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"closure-edge-interplay-tail-only".to_vec()),
                )
                .expect("valid tail-only closure edge recreation"),
            );
        } else if generation == 459 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"closure-edge-interplay-tail-paired".to_vec()),
                )
                .expect("valid joint prefix and tail closure image"),
            );
        } else if generation == 460 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid tail deletion with prefix retained"),
            );
        } else if generation == 462 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-tail-first".to_vec()),
                )
                .expect("valid paired case closure tail image"),
            );
        } else if generation == 463 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-tail-replaced".to_vec()),
                )
                .expect("valid case closure tail replacement"),
            );
        } else if generation == 464 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid case closure with prefix retained"),
            );
        } else if generation == 465 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-tail-second".to_vec()),
                )
                .expect("valid second case closure tail image"),
            );
        } else if generation == 467 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-tail-absent-prefix".to_vec()),
                )
                .expect("valid case tail replacement with prefix absent"),
            );
        } else if generation == 468 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-tail-restored".to_vec()),
                )
                .expect("valid joint case prefix and tail restoration"),
            );
        } else if generation == 470 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid terminal case closure tail deletion"),
            );
        } else if generation == 471 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"edge-case-closure-tail-first".to_vec()),
                )
                .expect("valid first paired edge case closure tail"),
            );
        } else if generation == 472 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"edge-case-closure-tail-replaced".to_vec()),
                )
                .expect("valid edge case closure tail replacement"),
            );
        } else if generation == 474 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"edge-case-closure-tail-absent-prefix".to_vec()),
                )
                .expect("valid absent-prefix edge case tail update"),
            );
        } else if generation == 475 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid terminal empty closure after tail update"),
            );
        } else if generation == 477 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"edge-case-closure-tail-joint".to_vec()),
                )
                .expect("valid joint prefix and edge case tail update"),
            );
        } else if generation == 478 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid edge case tail closure with prefix retained"),
            );
        } else if generation == 480 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-two-first".to_vec()),
                )
                .expect("valid first case closure edge tail image"),
            );
        } else if generation == 481 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-two-replaced".to_vec()),
                )
                .expect("valid case closure edge tail replacement"),
            );
        } else if generation == 483 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-two-absent-prefix".to_vec()),
                )
                .expect("valid absent-prefix case closure edge tail replacement"),
            );
        } else if generation == 484 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid case closure edge tail deletion"),
            );
        } else if generation == 486 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-two-joint".to_vec()),
                )
                .expect("valid joint case closure prefix and tail replacement"),
            );
        } else if generation == 487 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid case closure edge tail deletion with prefix retained"),
            );
        } else if generation == 489 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-three-first".to_vec()),
                )
                .expect("valid first case closure edge tail three image"),
            );
        } else if generation == 490 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-three-replaced".to_vec()),
                )
                .expect("valid case closure edge tail replacement"),
            );
        } else if generation == 492 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-three-absent-prefix".to_vec()),
                )
                .expect("valid absent-prefix case closure edge tail replacement"),
            );
        } else if generation == 493 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid case closure edge tail deletion with absent prefix"),
            );
        } else if generation == 495 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-three-joint".to_vec()),
                )
                .expect("valid joint case closure edge prefix and tail restoration"),
            );
        } else if generation == 496 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid case closure edge tail deletion with restored prefix"),
            );
        } else if generation == 498 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-four-first".to_vec()),
                )
                .expect("valid first case closure edge tail four image"),
            );
        } else if generation == 499 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-four-replaced".to_vec()),
                )
                .expect("valid case closure edge tail replacement with prefix deletion"),
            );
        } else if generation == 500 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-four-reopened".to_vec()),
                )
                .expect("valid joint case closure prefix and tail restoration"),
            );
        } else if generation == 501 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid case closure edge tail deletion with prefix retained"),
            );
        } else if generation == 502 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-four-recreated".to_vec()),
                )
                .expect("valid case closure edge tail recreation with prefix retained"),
            );
        } else if generation == 504 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-four-absent-prefix".to_vec()),
                )
                .expect("valid absent-prefix case closure edge tail replacement"),
            );
        } else if generation == 505 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid final absent-prefix case closure edge tail deletion"),
            );
        } else if generation == 507 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-five-first".to_vec()),
                )
                .expect("valid first case closure edge tail five image"),
            );
        } else if generation == 508 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-five-replaced".to_vec()),
                )
                .expect("valid case closure edge tail five replacement"),
            );
        } else if generation == 510 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-five-absent-prefix".to_vec()),
                )
                .expect("valid absent-prefix case closure edge tail five replacement"),
            );
        } else if generation == 511 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid absent-prefix case closure edge tail five deletion"),
            );
        } else if generation == 513 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-five-joint".to_vec()),
                )
                .expect("valid joint case closure edge prefix and tail five update"),
            );
        } else if generation == 514 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid case closure edge tail five deletion with prefix retained"),
            );
        } else if generation == 516 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-six-first".to_vec()),
                )
                .expect("valid first case closure edge tail six image"),
            );
        } else if generation == 517 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-six-replaced".to_vec()),
                )
                .expect("valid case closure edge tail six replacement"),
            );
        } else if generation == 518 || generation == 524 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid case closure edge tail six deletion"),
            );
        } else if generation == 519 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-six-recreated".to_vec()),
                )
                .expect("valid case closure edge tail six recreation"),
            );
        } else if generation == 521 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-six-absent-prefix".to_vec()),
                )
                .expect("valid absent-prefix case closure edge tail six replacement"),
            );
        } else if generation == 522 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-six-joint".to_vec()),
                )
                .expect("valid joint case closure edge prefix and tail six update"),
            );
        } else if generation == 526 || generation == 527 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            let value = if generation == 526 {
                b"case-closure-edge-tail-seven-first".to_vec()
            } else {
                b"case-closure-edge-tail-seven-replaced".to_vec()
            };
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], Some(value))
                    .expect("valid case closure edge tail seven image"),
            );
        } else if generation == 529 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-seven-absent-prefix".to_vec()),
                )
                .expect("valid absent-prefix case closure edge tail seven replacement"),
            );
        } else if generation == 530 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-seven-joint".to_vec()),
                )
                .expect("valid joint case closure edge prefix and tail seven update"),
            );
        } else if generation == 531 || generation == 534 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid case closure edge tail seven deletion"),
            );
        } else if generation == 532 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-seven-recreated".to_vec()),
                )
                .expect("valid case closure edge tail seven recreation"),
            );
        } else if generation == 536 || generation == 537 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            let value = if generation == 536 {
                b"case-closure-edge-tail-eight-first".to_vec()
            } else {
                b"case-closure-edge-tail-eight-replaced".to_vec()
            };
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], Some(value))
                    .expect("valid case closure edge tail eight image"),
            );
        } else if generation == 539 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-eight-absent-prefix".to_vec()),
                )
                .expect("valid absent-prefix case closure edge tail eight replacement"),
            );
        } else if generation == 540 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-eight-joint".to_vec()),
                )
                .expect("valid joint case closure edge prefix and tail eight update"),
            );
        } else if generation == 541 || generation == 544 || generation == 546 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid case closure edge tail eight deletion"),
            );
        } else if generation == 542 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-eight-recreated".to_vec()),
                )
                .expect("valid case closure edge tail eight recreation"),
            );
        } else if generation == 545 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-eight-final".to_vec()),
                )
                .expect("valid final tail-only case closure edge tail eight image"),
            );
        } else if generation == 547 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-nine-first".to_vec()),
                )
                .expect("valid first case closure edge tail nine image"),
            );
        } else if generation == 548 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-nine-replaced".to_vec()),
                )
                .expect("valid case closure edge tail nine replacement"),
            );
        } else if generation == 550 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-nine-recreated".to_vec()),
                )
                .expect("valid case closure edge tail nine recreation"),
            );
        } else if generation == 552 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-nine-absent-prefix".to_vec()),
                )
                .expect("valid absent-prefix case closure edge tail nine replacement"),
            );
        } else if generation == 553 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-nine-joint".to_vec()),
                )
                .expect("valid joint case closure edge prefix and tail nine update"),
            );
        } else if generation == 549 || generation == 555 || generation == 557 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid case closure edge tail nine deletion"),
            );
        } else if generation == 556 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-nine-final".to_vec()),
                )
                .expect("valid final tail-only case closure edge tail nine image"),
            );
        } else if generation == 558 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-ten-first".to_vec()),
                )
                .expect("valid first case closure edge tail ten image"),
            );
        } else if generation == 559 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-ten-replaced".to_vec()),
                )
                .expect("valid case closure edge tail ten replacement"),
            );
        } else if generation == 561 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-ten-absent-prefix".to_vec()),
                )
                .expect("valid absent-prefix case closure edge tail ten replacement"),
            );
        } else if generation == 562 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-ten-joint".to_vec()),
                )
                .expect("valid joint case closure edge prefix and tail ten update"),
            );
        } else if generation == 564 || generation == 568 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid case closure edge tail ten deletion"),
            );
        } else if generation == 566 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-ten-recreated".to_vec()),
                )
                .expect("valid case closure edge tail ten recreation"),
            );
        } else if generation == 569 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-eleven-first".to_vec()),
                )
                .expect("valid first case closure edge tail eleven image"),
            );
        } else if generation == 570 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-eleven-replaced".to_vec()),
                )
                .expect("valid case closure edge tail eleven replacement"),
            );
        } else if generation == 572 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-eleven-absent-prefix".to_vec()),
                )
                .expect("valid absent-prefix case closure edge tail eleven replacement"),
            );
        } else if generation == 573 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-eleven-joint".to_vec()),
                )
                .expect("valid joint case closure edge prefix and tail eleven update"),
            );
        } else if generation == 574 || generation == 577 || generation == 579 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid case closure edge tail eleven deletion"),
            );
        } else if generation == 575 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-eleven-recreated".to_vec()),
                )
                .expect("valid case closure edge tail eleven recreation"),
            );
        } else if generation == 578 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-eleven-final".to_vec()),
                )
                .expect("valid final tail-only case closure edge tail eleven image"),
            );
        } else if generation == 580 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twelve-first".to_vec()),
                )
                .expect("valid first case closure edge tail twelve image"),
            );
        } else if generation == 581 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twelve-replaced".to_vec()),
                )
                .expect("valid case closure edge tail twelve replacement"),
            );
        } else if generation == 582 || generation == 587 || generation == 590 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid case closure edge tail twelve deletion"),
            );
        } else if generation == 583 || generation == 588 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twelve-recreated".to_vec()),
                )
                .expect("valid case closure edge tail twelve recreation"),
            );
        } else if generation == 585 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twelve-absent-prefix".to_vec()),
                )
                .expect("valid absent-prefix case closure edge tail twelve replacement"),
            );
        } else if generation == 586 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twelve-joint".to_vec()),
                )
                .expect("valid joint case closure edge prefix and tail twelve update"),
            );
        } else if generation == 591 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-thirteen-first".to_vec()),
                )
                .expect("valid first case closure edge tail thirteen image"),
            );
        } else if generation == 592 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-thirteen-replaced".to_vec()),
                )
                .expect("valid case closure edge tail thirteen replacement"),
            );
        } else if generation == 593 || generation == 598 || generation == 601 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid case closure edge tail thirteen deletion"),
            );
        } else if generation == 594 || generation == 599 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-thirteen-recreated".to_vec()),
                )
                .expect("valid case closure edge tail thirteen recreation"),
            );
        } else if generation == 596 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-thirteen-absent-prefix".to_vec()),
                )
                .expect("valid absent-prefix case closure edge tail thirteen replacement"),
            );
        } else if generation == 597 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-thirteen-joint".to_vec()),
                )
                .expect("valid joint case closure edge prefix and tail thirteen update"),
            );
        } else if generation == 602 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-fourteen-first".to_vec()),
                )
                .expect("valid first case closure edge tail fourteen image"),
            );
        } else if generation == 603 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-fourteen-replaced".to_vec()),
                )
                .expect("valid case closure edge tail fourteen replacement"),
            );
        } else if generation == 604 || generation == 609 || generation == 612 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid case closure edge tail fourteen deletion"),
            );
        } else if generation == 605 || generation == 610 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-fourteen-recreated".to_vec()),
                )
                .expect("valid case closure edge tail fourteen recreation"),
            );
        } else if generation == 607 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-fourteen-absent-prefix".to_vec()),
                )
                .expect("valid absent-prefix case closure edge tail fourteen replacement"),
            );
        } else if generation == 608 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-fourteen-joint".to_vec()),
                )
                .expect("valid joint case closure edge prefix and tail fourteen update"),
            );
        } else if generation == 613 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-fifteen-first".to_vec()),
                )
                .expect("valid first case closure edge tail fifteen image"),
            );
        } else if generation == 614 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-fifteen-replaced".to_vec()),
                )
                .expect("valid case closure edge tail fifteen replacement"),
            );
        } else if generation == 615 || generation == 620 || generation == 623 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid case closure edge tail fifteen deletion"),
            );
        } else if generation == 616 || generation == 621 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-fifteen-recreated".to_vec()),
                )
                .expect("valid case closure edge tail fifteen recreation"),
            );
        } else if generation == 618 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-fifteen-absent-prefix".to_vec()),
                )
                .expect("valid absent-prefix case closure edge tail fifteen replacement"),
            );
        } else if generation == 619 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-fifteen-joint".to_vec()),
                )
                .expect("valid joint case closure edge prefix and tail fifteen update"),
            );
        } else if generation == 624 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-sixteen-first".to_vec()),
                )
                .expect("valid first case closure edge tail sixteen image"),
            );
        } else if generation == 625 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-sixteen-replaced".to_vec()),
                )
                .expect("valid case closure edge tail sixteen replacement"),
            );
        } else if generation == 626 || generation == 631 || generation == 634 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid case closure edge tail sixteen deletion"),
            );
        } else if generation == 627 || generation == 632 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-sixteen-recreated".to_vec()),
                )
                .expect("valid case closure edge tail sixteen recreation"),
            );
        } else if generation == 629 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-sixteen-absent-prefix".to_vec()),
                )
                .expect("valid absent-prefix case closure edge tail sixteen replacement"),
            );
        } else if generation == 630 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-sixteen-joint".to_vec()),
                )
                .expect("valid joint case closure edge prefix and tail sixteen update"),
            );
        } else if generation == 635 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-seventeen-first".to_vec()),
                )
                .expect("valid first case closure edge tail seventeen image"),
            );
        } else if generation == 636 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-seventeen-replaced".to_vec()),
                )
                .expect("valid case closure edge tail seventeen replacement"),
            );
        } else if generation == 637 || generation == 642 || generation == 645 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid case closure edge tail seventeen deletion"),
            );
        } else if generation == 638 || generation == 643 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-seventeen-recreated".to_vec()),
                )
                .expect("valid case closure edge tail seventeen recreation"),
            );
        } else if generation == 640 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-seventeen-absent-prefix".to_vec()),
                )
                .expect("valid absent-prefix case closure edge tail seventeen replacement"),
            );
        } else if generation == 641 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-seventeen-joint".to_vec()),
                )
                .expect("valid joint case closure edge prefix and tail seventeen update"),
            );
        } else if generation == 646 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-eighteen-first".to_vec()),
                )
                .expect("valid first case closure edge tail eighteen image"),
            );
        } else if generation == 647 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-eighteen-replaced".to_vec()),
                )
                .expect("valid case closure edge tail eighteen replacement"),
            );
        } else if generation == 648 || generation == 653 || generation == 656 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid case closure edge tail eighteen deletion"),
            );
        } else if generation == 649 || generation == 654 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-eighteen-recreated".to_vec()),
                )
                .expect("valid case closure edge tail eighteen recreation"),
            );
        } else if generation == 651 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-eighteen-absent-prefix".to_vec()),
                )
                .expect("valid absent-prefix case closure edge tail eighteen replacement"),
            );
        } else if generation == 652 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-eighteen-joint".to_vec()),
                )
                .expect("valid joint case closure edge prefix and tail eighteen update"),
            );
        } else if generation == 657 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-nineteen-first".to_vec()),
                )
                .expect("valid first case closure edge tail nineteen image"),
            );
        } else if generation == 658 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-nineteen-replaced".to_vec()),
                )
                .expect("valid case closure edge tail nineteen replacement"),
            );
        } else if generation == 659 || generation == 664 || generation == 667 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid case closure edge tail nineteen deletion"),
            );
        } else if generation == 660 || generation == 665 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-nineteen-recreated".to_vec()),
                )
                .expect("valid case closure edge tail nineteen recreation"),
            );
        } else if generation == 662 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-nineteen-absent-prefix".to_vec()),
                )
                .expect("valid absent-prefix case closure edge tail nineteen replacement"),
            );
        } else if generation == 663 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-nineteen-joint".to_vec()),
                )
                .expect("valid joint case closure edge prefix and tail nineteen update"),
            );
        } else if generation == 668 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-first".to_vec()),
                )
                .expect("valid first case closure edge tail twenty image"),
            );
        } else if generation == 669 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-replaced".to_vec()),
                )
                .expect("valid case closure edge tail twenty replacement"),
            );
        } else if generation == 670 || generation == 675 || generation == 678 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid case closure edge tail twenty deletion"),
            );
        } else if generation == 671 || generation == 676 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-recreated".to_vec()),
                )
                .expect("valid case closure edge tail twenty recreation"),
            );
        } else if generation == 673 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-absent-prefix".to_vec()),
                )
                .expect("valid absent-prefix case closure edge tail twenty replacement"),
            );
        } else if generation == 674 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-joint".to_vec()),
                )
                .expect("valid joint case closure edge prefix and tail twenty update"),
            );
        } else if generation == 679 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-one-first".to_vec()),
                )
                .expect("valid first case closure edge tail twenty-one image"),
            );
        } else if generation == 680 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-one-replaced".to_vec()),
                )
                .expect("valid case closure edge tail twenty-one replacement"),
            );
        } else if generation == 681 || generation == 686 || generation == 689 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid case closure edge tail twenty-one deletion"),
            );
        } else if generation == 682 || generation == 687 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-one-recreated".to_vec()),
                )
                .expect("valid case closure edge tail twenty-one recreation"),
            );
        } else if generation == 684 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-one-absent-prefix".to_vec()),
                )
                .expect("valid absent-prefix case closure edge tail twenty-one replacement"),
            );
        } else if generation == 685 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-one-joint".to_vec()),
                )
                .expect("valid joint case closure edge prefix and tail twenty-one update"),
            );
        } else if generation == 690 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-two-first".to_vec()),
                )
                .expect("valid first case closure edge tail twenty-two image"),
            );
        } else if generation == 691 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-two-replaced".to_vec()),
                )
                .expect("valid case closure edge tail twenty-two replacement"),
            );
        } else if generation == 692 || generation == 697 || generation == 700 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid case closure edge tail twenty-two deletion"),
            );
        } else if generation == 693 || generation == 698 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-two-recreated".to_vec()),
                )
                .expect("valid case closure edge tail twenty-two recreation"),
            );
        } else if generation == 695 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-two-absent-prefix".to_vec()),
                )
                .expect("valid absent-prefix case closure edge tail twenty-two replacement"),
            );
        } else if generation == 696 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-two-joint".to_vec()),
                )
                .expect("valid joint case closure edge prefix and tail twenty-two update"),
            );
        } else if generation == 701 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-three-first".to_vec()),
                )
                .expect("valid first case closure edge tail twenty-three image"),
            );
        } else if generation == 702 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-three-replaced".to_vec()),
                )
                .expect("valid case closure edge tail twenty-three replacement"),
            );
        } else if generation == 703 || generation == 708 || generation == 711 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid case closure edge tail twenty-three deletion"),
            );
        } else if generation == 704 || generation == 709 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-three-recreated".to_vec()),
                )
                .expect("valid case closure edge tail twenty-three recreation"),
            );
        } else if generation == 706 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-three-absent-prefix".to_vec()),
                )
                .expect("valid absent-prefix case closure edge tail twenty-three replacement"),
            );
        } else if generation == 707 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-three-joint".to_vec()),
                )
                .expect("valid joint case closure edge prefix and tail twenty-three update"),
            );
        } else if generation == 712 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-four-first".to_vec()),
                )
                .expect("valid first case closure edge tail twenty-four image"),
            );
        } else if generation == 713 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-four-replaced".to_vec()),
                )
                .expect("valid case closure edge tail twenty-four replacement"),
            );
        } else if generation == 714 || generation == 719 || generation == 722 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid case closure edge tail twenty-four deletion"),
            );
        } else if generation == 715 || generation == 720 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-four-recreated".to_vec()),
                )
                .expect("valid case closure edge tail twenty-four recreation"),
            );
        } else if generation == 717 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-four-absent-prefix".to_vec()),
                )
                .expect("valid absent-prefix case closure edge tail twenty-four replacement"),
            );
        } else if generation == 718 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-four-joint".to_vec()),
                )
                .expect("valid joint case closure edge prefix and tail twenty-four update"),
            );
        } else if generation == 723 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-five-first".to_vec()),
                )
                .expect("valid first case closure edge tail twenty-five image"),
            );
        } else if generation == 724 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-five-replaced".to_vec()),
                )
                .expect("valid case closure edge tail twenty-five replacement"),
            );
        } else if generation == 725 || generation == 730 || generation == 733 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid case closure edge tail twenty-five deletion"),
            );
        } else if generation == 726 || generation == 731 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-five-recreated".to_vec()),
                )
                .expect("valid case closure edge tail twenty-five recreation"),
            );
        } else if generation == 728 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-five-absent-prefix".to_vec()),
                )
                .expect("valid absent-prefix case closure edge tail twenty-five replacement"),
            );
        } else if generation == 729 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-five-joint".to_vec()),
                )
                .expect("valid joint case closure edge prefix and tail twenty-five update"),
            );
        } else if generation == 734 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-six-first".to_vec()),
                )
                .expect("valid first case closure edge tail twenty-six image"),
            );
        } else if generation == 735 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-six-replaced".to_vec()),
                )
                .expect("valid case closure edge tail twenty-six replacement"),
            );
        } else if generation == 736 || generation == 741 || generation == 744 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid case closure edge tail twenty-six deletion"),
            );
        } else if generation == 737 || generation == 742 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-six-recreated".to_vec()),
                )
                .expect("valid case closure edge tail twenty-six recreation"),
            );
        } else if generation == 739 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-six-absent-prefix".to_vec()),
                )
                .expect("valid absent-prefix case closure edge tail twenty-six replacement"),
            );
        } else if generation == 740 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-six-joint".to_vec()),
                )
                .expect("valid joint case closure edge prefix and tail twenty-six update"),
            );
        } else if generation == 745 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-seven-first".to_vec()),
                )
                .expect("valid first case closure edge tail twenty-seven image"),
            );
        } else if generation == 746 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-seven-replaced".to_vec()),
                )
                .expect("valid case closure edge tail twenty-seven replacement"),
            );
        } else if generation == 747 || generation == 752 || generation == 755 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid case closure edge tail twenty-seven deletion"),
            );
        } else if generation == 748 || generation == 753 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-seven-recreated".to_vec()),
                )
                .expect("valid case closure edge tail twenty-seven recreation"),
            );
        } else if generation == 750 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-seven-absent-prefix".to_vec()),
                )
                .expect("valid absent-prefix case closure edge tail twenty-seven replacement"),
            );
        } else if generation == 751 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-seven-joint".to_vec()),
                )
                .expect("valid joint case closure edge prefix and tail twenty-seven update"),
            );
        } else if generation == 756 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-eight-first".to_vec()),
                )
                .expect("valid first case closure edge tail twenty-eight image"),
            );
        } else if generation == 757 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-eight-replaced".to_vec()),
                )
                .expect("valid case closure edge tail twenty-eight replacement"),
            );
        } else if generation == 758 || generation == 763 || generation == 766 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid case closure edge tail twenty-eight deletion"),
            );
        } else if generation == 759 || generation == 764 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-eight-recreated".to_vec()),
                )
                .expect("valid case closure edge tail twenty-eight recreation"),
            );
        } else if generation == 761 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-eight-absent-prefix".to_vec()),
                )
                .expect("valid absent-prefix case closure edge tail twenty-eight replacement"),
            );
        } else if generation == 762 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-eight-joint".to_vec()),
                )
                .expect("valid joint case closure edge prefix and tail twenty-eight update"),
            );
        } else if generation == 767 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-nine-first".to_vec()),
                )
                .expect("valid first case closure edge tail twenty-nine image"),
            );
        } else if generation == 768 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-nine-replaced".to_vec()),
                )
                .expect("valid case closure edge tail twenty-nine replacement"),
            );
        } else if generation == 769 || generation == 774 || generation == 777 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid case closure edge tail twenty-nine deletion"),
            );
        } else if generation == 770 || generation == 775 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-nine-recreated".to_vec()),
                )
                .expect("valid case closure edge tail twenty-nine recreation"),
            );
        } else if generation == 772 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-nine-absent-prefix".to_vec()),
                )
                .expect("valid absent-prefix case closure edge tail twenty-nine replacement"),
            );
        } else if generation == 773 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-twenty-nine-joint".to_vec()),
                )
                .expect("valid joint case closure edge prefix and tail twenty-nine update"),
            );
        } else if generation == 778 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-thirty-first".to_vec()),
                )
                .expect("valid first case closure edge tail thirty image"),
            );
        } else if generation == 779 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-thirty-replaced".to_vec()),
                )
                .expect("valid case closure edge tail thirty replacement"),
            );
        } else if generation == 780 || generation == 785 || generation == 788 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid case closure edge tail thirty deletion"),
            );
        } else if generation == 781 || generation == 786 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-thirty-recreated".to_vec()),
                )
                .expect("valid case closure edge tail thirty recreation"),
            );
        } else if generation == 783 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-thirty-absent-prefix".to_vec()),
                )
                .expect("valid absent-prefix case closure edge tail thirty replacement"),
            );
        } else if generation == 784 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-thirty-joint".to_vec()),
                )
                .expect("valid joint case closure edge prefix and tail thirty update"),
            );
        } else if generation == 789 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-thirty-one-first".to_vec()),
                )
                .expect("valid first case closure edge tail thirty-one image"),
            );
        } else if generation == 790 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-thirty-one-replaced".to_vec()),
                )
                .expect("valid case closure edge tail thirty-one replacement"),
            );
        } else if generation == 791 || generation == 796 || generation == 799 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid case closure edge tail thirty-one deletion"),
            );
        } else if generation == 792 || generation == 797 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-thirty-one-recreated".to_vec()),
                )
                .expect("valid case closure edge tail thirty-one recreation"),
            );
        } else if generation == 794 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-thirty-one-absent-prefix".to_vec()),
                )
                .expect("valid absent-prefix case closure edge tail thirty-one replacement"),
            );
        } else if generation == 795 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-thirty-one-joint".to_vec()),
                )
                .expect("valid joint case closure edge prefix and tail thirty-one update"),
            );
        } else if generation == 800 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-thirty-two-first".to_vec()),
                )
                .expect("valid first case closure edge tail thirty-two image"),
            );
        } else if generation == 801 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-thirty-two-replaced".to_vec()),
                )
                .expect("valid case closure edge tail thirty-two replacement"),
            );
        } else if generation == 802 || generation == 807 || generation == 810 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid case closure edge tail thirty-two deletion"),
            );
        } else if generation == 803 || generation == 808 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-thirty-two-recreated".to_vec()),
                )
                .expect("valid case closure edge tail thirty-two recreation"),
            );
        } else if generation == 805 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-thirty-two-absent-prefix".to_vec()),
                )
                .expect("valid absent-prefix case closure edge tail thirty-two replacement"),
            );
        } else if generation == 806 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-thirty-two-joint".to_vec()),
                )
                .expect("valid joint case closure edge prefix and tail thirty-two update"),
            );
        } else if generation == 811 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-thirty-three-first".to_vec()),
                )
                .expect("valid first case closure edge tail thirty-three image"),
            );
        } else if generation == 812 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-thirty-three-replaced".to_vec()),
                )
                .expect("valid case closure edge tail thirty-three replacement"),
            );
        } else if generation == 813 || generation == 818 || generation == 821 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid case closure edge tail thirty-three deletion"),
            );
        } else if generation == 814 || generation == 819 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-thirty-three-recreated".to_vec()),
                )
                .expect("valid case closure edge tail thirty-three recreation"),
            );
        } else if generation == 816 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-thirty-three-absent-prefix".to_vec()),
                )
                .expect("valid absent-prefix case closure edge tail thirty-three replacement"),
            );
        } else if generation == 817 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-thirty-three-joint".to_vec()),
                )
                .expect("valid joint case closure edge prefix and tail thirty-three update"),
            );
        } else if generation == 822 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-thirty-four-first".to_vec()),
                )
                .expect("valid first case closure edge tail thirty-four image"),
            );
        } else if generation == 823 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-thirty-four-replaced".to_vec()),
                )
                .expect("valid case closure edge tail thirty-four replacement"),
            );
        } else if generation == 824 || generation == 829 || generation == 832 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid case closure edge tail thirty-four deletion"),
            );
        } else if generation == 825 || generation == 830 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-thirty-four-recreated".to_vec()),
                )
                .expect("valid case closure edge tail thirty-four recreation"),
            );
        } else if generation == 827 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-thirty-four-absent-prefix".to_vec()),
                )
                .expect("valid absent-prefix case closure edge tail thirty-four replacement"),
            );
        } else if generation == 828 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-thirty-four-joint".to_vec()),
                )
                .expect("valid joint case closure edge prefix and tail thirty-four update"),
            );
        } else if generation == 833 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-thirty-five-first".to_vec()),
                )
                .expect("valid first case closure edge tail thirty-five image"),
            );
        } else if generation == 834 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-thirty-five-replaced".to_vec()),
                )
                .expect("valid case closure edge tail thirty-five replacement"),
            );
        } else if generation == 835 || generation == 840 || generation == 843 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid case closure edge tail thirty-five deletion"),
            );
        } else if generation == 836 || generation == 841 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-thirty-five-recreated".to_vec()),
                )
                .expect("valid case closure edge tail thirty-five recreation"),
            );
        } else if generation == 838 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-thirty-five-absent-prefix".to_vec()),
                )
                .expect("valid absent-prefix case closure edge tail thirty-five replacement"),
            );
        } else if generation == 839 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-thirty-five-joint".to_vec()),
                )
                .expect("valid joint case closure edge prefix and tail thirty-five update"),
            );
        } else if generation == 844 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-thirty-six-first".to_vec()),
                )
                .expect("valid first case closure edge tail thirty-six image"),
            );
        } else if generation == 845 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-thirty-six-replaced".to_vec()),
                )
                .expect("valid case closure edge tail thirty-six replacement"),
            );
        } else if generation == 846 || generation == 851 || generation == 854 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid case closure edge tail thirty-six deletion"),
            );
        } else if generation == 847 || generation == 852 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-thirty-six-recreated".to_vec()),
                )
                .expect("valid case closure edge tail thirty-six recreation"),
            );
        } else if generation == 849 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-thirty-six-absent-prefix".to_vec()),
                )
                .expect("valid absent-prefix case closure edge tail thirty-six replacement"),
            );
        } else if generation == 850 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-thirty-six-joint".to_vec()),
                )
                .expect("valid joint case closure edge prefix and tail thirty-six update"),
            );
        } else if generation == 855 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-thirty-seven-first".to_vec()),
                )
                .expect("valid first case closure edge tail thirty-seven image"),
            );
        } else if generation == 856 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-thirty-seven-replaced".to_vec()),
                )
                .expect("valid case closure edge tail thirty-seven replacement"),
            );
        } else if generation == 857 || generation == 862 || generation == 865 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(extension_mutation_id, "records", vec![5, 0], None)
                    .expect("valid case closure edge tail thirty-seven deletion"),
            );
        } else if generation == 858 || generation == 863 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-thirty-seven-recreated".to_vec()),
                )
                .expect("valid case closure edge tail thirty-seven recreation"),
            );
        } else if generation == 860 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-thirty-seven-absent-prefix".to_vec()),
                )
                .expect("valid absent-prefix case closure edge tail thirty-seven replacement"),
            );
        } else if generation == 861 {
            let mut extension_mutation_id = mutation_id;
            extension_mutation_id[0] = 1;
            mutations.push(
                TableMutation::new(
                    extension_mutation_id,
                    "records",
                    vec![5, 0],
                    Some(b"case-closure-edge-tail-thirty-seven-joint".to_vec()),
                )
                .expect("valid joint case closure edge prefix and tail thirty-seven update"),
            );
        }
        commit(&state, writer, &mutations, 57).await;

        if (255..=865).contains(&generation) {
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
        (255..=865).collect::<Vec<_>>()
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
        435 => vec![(vec![5], b"original-prefix".to_vec())],
        436 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"edge-tail-closure-edge-first".to_vec()),
        ],
        437 => vec![(vec![5, 0], b"edge-tail-closure-edge-first".to_vec())],
        438 => vec![(vec![5, 0], b"edge-tail-closure-edge-replaced".to_vec())],
        439 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"edge-tail-closure-edge-restored".to_vec()),
        ],
        440 => vec![(vec![5], b"original-prefix".to_vec())],
        441 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"edge-tail-closure-edge-final".to_vec()),
        ],
        442 => vec![(vec![5, 0], b"edge-tail-closure-edge-final".to_vec())],
        443 => vec![],
        444 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"interplay-closure-edge-tail-two-first".to_vec()),
        ],
        445 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"interplay-closure-edge-tail-two-replaced".to_vec()),
        ],
        446 => vec![(vec![5, 0], b"interplay-closure-edge-tail-two-replaced".to_vec())],
        447 => vec![(vec![5, 0], b"interplay-closure-edge-tail-two-absent-prefix".to_vec())],
        448 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"interplay-closure-edge-tail-two-reopened".to_vec()),
        ],
        449 => vec![(vec![5], b"original-prefix".to_vec())],
        450 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"interplay-closure-edge-tail-two-final".to_vec()),
        ],
        451 => vec![(vec![5, 0], b"interplay-closure-edge-tail-two-final".to_vec())],
        452 => vec![],
        453 => vec![(vec![5], b"original-prefix".to_vec())],
        454 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"closure-edge-interplay-tail-first".to_vec()),
        ],
        455 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"closure-edge-interplay-tail-replaced".to_vec()),
        ],
        456 => vec![(vec![5, 0], b"closure-edge-interplay-tail-replaced".to_vec())],
        457 => vec![],
        458 => vec![(vec![5, 0], b"closure-edge-interplay-tail-only".to_vec())],
        459 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"closure-edge-interplay-tail-paired".to_vec()),
        ],
        460 => vec![(vec![5], b"original-prefix".to_vec())],
        461 => vec![],
        462 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-tail-first".to_vec()),
        ],
        463 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-tail-replaced".to_vec()),
        ],
        464 => vec![(vec![5], b"original-prefix".to_vec())],
        465 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-tail-second".to_vec()),
        ],
        466 => vec![(vec![5, 0], b"case-closure-tail-second".to_vec())],
        467 => vec![(vec![5, 0], b"case-closure-tail-absent-prefix".to_vec())],
        468 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-tail-restored".to_vec()),
        ],
        469 => vec![(vec![5, 0], b"case-closure-tail-restored".to_vec())],
        470 => vec![],
        471 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"edge-case-closure-tail-first".to_vec()),
        ],
        472 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"edge-case-closure-tail-replaced".to_vec()),
        ],
        473 => vec![(vec![5, 0], b"edge-case-closure-tail-replaced".to_vec())],
        474 => vec![(vec![5, 0], b"edge-case-closure-tail-absent-prefix".to_vec())],
        475 => vec![],
        476 => vec![(vec![5], b"original-prefix".to_vec())],
        477 => vec![
            (vec![5], b"case-closure-prefix-final".to_vec()),
            (vec![5, 0], b"edge-case-closure-tail-joint".to_vec()),
        ],
        478 => vec![(vec![5], b"case-closure-prefix-final".to_vec())],
        479 => vec![],
        480 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-two-first".to_vec()),
        ],
        481 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-two-replaced".to_vec()),
        ],
        482 => vec![(vec![5, 0], b"case-closure-edge-tail-two-replaced".to_vec())],
        483 => vec![
            (vec![5, 0], b"case-closure-edge-tail-two-absent-prefix".to_vec()),
        ],
        484 => vec![],
        485 => vec![(vec![5], b"original-prefix".to_vec())],
        486 => vec![
            (vec![5], b"case-closure-edge-prefix-two-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-two-joint".to_vec()),
        ],
        487 => vec![(vec![5], b"case-closure-edge-prefix-two-final".to_vec())],
        488 => vec![],
        489 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-three-first".to_vec()),
        ],
        490 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-three-replaced".to_vec()),
        ],
        491 => vec![(vec![5, 0], b"case-closure-edge-tail-three-replaced".to_vec())],
        492 => vec![
            (vec![5, 0], b"case-closure-edge-tail-three-absent-prefix".to_vec()),
        ],
        493 => vec![],
        494 => vec![(vec![5], b"original-prefix".to_vec())],
        495 => vec![
            (vec![5], b"case-closure-edge-prefix-three-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-three-joint".to_vec()),
        ],
        496 => vec![(vec![5], b"case-closure-edge-prefix-three-final".to_vec())],
        497 => vec![],
        498 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-four-first".to_vec()),
        ],
        499 => vec![(vec![5, 0], b"case-closure-edge-tail-four-replaced".to_vec())],
        500 => vec![
            (vec![5], b"case-closure-edge-prefix-four-restored".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-four-reopened".to_vec()),
        ],
        501 => vec![(vec![5], b"case-closure-edge-prefix-four-restored".to_vec())],
        502 => vec![
            (vec![5], b"case-closure-edge-prefix-four-restored".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-four-recreated".to_vec()),
        ],
        503 => vec![(vec![5, 0], b"case-closure-edge-tail-four-recreated".to_vec())],
        504 => vec![(vec![5, 0], b"case-closure-edge-tail-four-absent-prefix".to_vec())],
        505 => vec![],
        506 => vec![(vec![5], b"original-prefix".to_vec())],
        507 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-five-first".to_vec()),
        ],
        508 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-five-replaced".to_vec()),
        ],
        509 => vec![(vec![5, 0], b"case-closure-edge-tail-five-replaced".to_vec())],
        510 => vec![(vec![5, 0], b"case-closure-edge-tail-five-absent-prefix".to_vec())],
        511 => vec![],
        512 => vec![(vec![5], b"original-prefix".to_vec())],
        513 => vec![
            (vec![5], b"case-closure-edge-prefix-five-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-five-joint".to_vec()),
        ],
        514 => vec![(vec![5], b"case-closure-edge-prefix-five-final".to_vec())],
        515 => vec![],
        516 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-six-first".to_vec()),
        ],
        517 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-six-replaced".to_vec()),
        ],
        518 => vec![(vec![5], b"original-prefix".to_vec())],
        519 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-six-recreated".to_vec()),
        ],
        520 => vec![(vec![5, 0], b"case-closure-edge-tail-six-recreated".to_vec())],
        521 => vec![(vec![5, 0], b"case-closure-edge-tail-six-absent-prefix".to_vec())],
        522 => vec![
            (vec![5], b"case-closure-edge-prefix-six-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-six-joint".to_vec()),
        ],
        523 => vec![(vec![5, 0], b"case-closure-edge-tail-six-joint".to_vec())],
        524 => vec![],
        525 => vec![(vec![5], b"original-prefix".to_vec())],
        526 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-seven-first".to_vec()),
        ],
        527 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-seven-replaced".to_vec()),
        ],
        528 => vec![(vec![5, 0], b"case-closure-edge-tail-seven-replaced".to_vec())],
        529 => vec![(vec![5, 0], b"case-closure-edge-tail-seven-absent-prefix".to_vec())],
        530 => vec![
            (vec![5], b"case-closure-edge-prefix-seven-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-seven-joint".to_vec()),
        ],
        531 => vec![(vec![5], b"case-closure-edge-prefix-seven-final".to_vec())],
        532 => vec![
            (vec![5], b"case-closure-edge-prefix-seven-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-seven-recreated".to_vec()),
        ],
        533 => vec![(vec![5, 0], b"case-closure-edge-tail-seven-recreated".to_vec())],
        534 => vec![],
        535 => vec![(vec![5], b"original-prefix".to_vec())],
        536 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-eight-first".to_vec()),
        ],
        537 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-eight-replaced".to_vec()),
        ],
        538 => vec![(vec![5, 0], b"case-closure-edge-tail-eight-replaced".to_vec())],
        539 => vec![(vec![5, 0], b"case-closure-edge-tail-eight-absent-prefix".to_vec())],
        540 => vec![
            (vec![5], b"case-closure-edge-prefix-eight-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-eight-joint".to_vec()),
        ],
        541 => vec![(vec![5], b"case-closure-edge-prefix-eight-final".to_vec())],
        542 => vec![
            (vec![5], b"case-closure-edge-prefix-eight-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-eight-recreated".to_vec()),
        ],
        543 => vec![(vec![5, 0], b"case-closure-edge-tail-eight-recreated".to_vec())],
        544 => vec![],
        545 => vec![(vec![5, 0], b"case-closure-edge-tail-eight-final".to_vec())],
        546 => vec![],
        547 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-nine-first".to_vec()),
        ],
        548 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-nine-replaced".to_vec()),
        ],
        549 => vec![(vec![5], b"original-prefix".to_vec())],
        550 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-nine-recreated".to_vec()),
        ],
        551 => vec![(vec![5, 0], b"case-closure-edge-tail-nine-recreated".to_vec())],
        552 => vec![(vec![5, 0], b"case-closure-edge-tail-nine-absent-prefix".to_vec())],
        553 => vec![
            (vec![5], b"case-closure-edge-prefix-nine-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-nine-joint".to_vec()),
        ],
        554 => vec![(vec![5, 0], b"case-closure-edge-tail-nine-joint".to_vec())],
        555 => vec![],
        556 => vec![(vec![5, 0], b"case-closure-edge-tail-nine-final".to_vec())],
        557 => vec![],
        558 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-ten-first".to_vec()),
        ],
        559 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-ten-replaced".to_vec()),
        ],
        560 => vec![(vec![5, 0], b"case-closure-edge-tail-ten-replaced".to_vec())],
        561 => vec![(vec![5, 0], b"case-closure-edge-tail-ten-absent-prefix".to_vec())],
        562 => vec![
            (vec![5], b"case-closure-edge-prefix-ten-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-ten-joint".to_vec()),
        ],
        563 => vec![(vec![5, 0], b"case-closure-edge-tail-ten-joint".to_vec())],
        564 => vec![],
        565 => vec![(vec![5], b"original-prefix".to_vec())],
        566 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-ten-recreated".to_vec()),
        ],
        567 => vec![(vec![5, 0], b"case-closure-edge-tail-ten-recreated".to_vec())],
        568 => vec![],
        569 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-eleven-first".to_vec()),
        ],
        570 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-eleven-replaced".to_vec()),
        ],
        571 => vec![(vec![5, 0], b"case-closure-edge-tail-eleven-replaced".to_vec())],
        572 => vec![(vec![5, 0], b"case-closure-edge-tail-eleven-absent-prefix".to_vec())],
        573 => vec![
            (vec![5], b"case-closure-edge-prefix-eleven-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-eleven-joint".to_vec()),
        ],
        574 => vec![(vec![5], b"case-closure-edge-prefix-eleven-final".to_vec())],
        575 => vec![
            (vec![5], b"case-closure-edge-prefix-eleven-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-eleven-recreated".to_vec()),
        ],
        576 => vec![(vec![5, 0], b"case-closure-edge-tail-eleven-recreated".to_vec())],
        577 => vec![],
        578 => vec![(vec![5, 0], b"case-closure-edge-tail-eleven-final".to_vec())],
        579 => vec![],
        580 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twelve-first".to_vec()),
        ],
        581 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twelve-replaced".to_vec()),
        ],
        582 => vec![(vec![5], b"original-prefix".to_vec())],
        583 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twelve-recreated".to_vec()),
        ],
        584 => vec![(vec![5, 0], b"case-closure-edge-tail-twelve-recreated".to_vec())],
        585 => vec![(vec![5, 0], b"case-closure-edge-tail-twelve-absent-prefix".to_vec())],
        586 => vec![
            (vec![5], b"case-closure-edge-prefix-twelve-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twelve-joint".to_vec()),
        ],
        587 => vec![(vec![5], b"case-closure-edge-prefix-twelve-final".to_vec())],
        588 => vec![
            (vec![5], b"case-closure-edge-prefix-twelve-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twelve-recreated".to_vec()),
        ],
        589 => vec![(vec![5, 0], b"case-closure-edge-tail-twelve-recreated".to_vec())],
        590 => vec![],
        591 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-thirteen-first".to_vec()),
        ],
        592 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-thirteen-replaced".to_vec()),
        ],
        593 => vec![(vec![5], b"original-prefix".to_vec())],
        594 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-thirteen-recreated".to_vec()),
        ],
        595 => vec![(vec![5, 0], b"case-closure-edge-tail-thirteen-recreated".to_vec())],
        596 => vec![(vec![5, 0], b"case-closure-edge-tail-thirteen-absent-prefix".to_vec())],
        597 => vec![
            (vec![5], b"case-closure-edge-prefix-thirteen-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-thirteen-joint".to_vec()),
        ],
        598 => vec![(vec![5], b"case-closure-edge-prefix-thirteen-final".to_vec())],
        599 => vec![
            (vec![5], b"case-closure-edge-prefix-thirteen-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-thirteen-recreated".to_vec()),
        ],
        600 => vec![(vec![5, 0], b"case-closure-edge-tail-thirteen-recreated".to_vec())],
        601 => vec![],
        602 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-fourteen-first".to_vec()),
        ],
        603 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-fourteen-replaced".to_vec()),
        ],
        604 => vec![(vec![5], b"original-prefix".to_vec())],
        605 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-fourteen-recreated".to_vec()),
        ],
        606 => vec![(vec![5, 0], b"case-closure-edge-tail-fourteen-recreated".to_vec())],
        607 => vec![(vec![5, 0], b"case-closure-edge-tail-fourteen-absent-prefix".to_vec())],
        608 => vec![
            (vec![5], b"case-closure-edge-prefix-fourteen-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-fourteen-joint".to_vec()),
        ],
        609 => vec![(vec![5], b"case-closure-edge-prefix-fourteen-final".to_vec())],
        610 => vec![
            (vec![5], b"case-closure-edge-prefix-fourteen-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-fourteen-recreated".to_vec()),
        ],
        611 => vec![(vec![5, 0], b"case-closure-edge-tail-fourteen-recreated".to_vec())],
        612 => vec![],
        613 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-fifteen-first".to_vec()),
        ],
        614 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-fifteen-replaced".to_vec()),
        ],
        615 => vec![(vec![5], b"original-prefix".to_vec())],
        616 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-fifteen-recreated".to_vec()),
        ],
        617 => vec![(vec![5, 0], b"case-closure-edge-tail-fifteen-recreated".to_vec())],
        618 => vec![(vec![5, 0], b"case-closure-edge-tail-fifteen-absent-prefix".to_vec())],
        619 => vec![
            (vec![5], b"case-closure-edge-prefix-fifteen-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-fifteen-joint".to_vec()),
        ],
        620 => vec![(vec![5], b"case-closure-edge-prefix-fifteen-final".to_vec())],
        621 => vec![
            (vec![5], b"case-closure-edge-prefix-fifteen-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-fifteen-recreated".to_vec()),
        ],
        622 => vec![(vec![5, 0], b"case-closure-edge-tail-fifteen-recreated".to_vec())],
        623 => vec![],
        624 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-sixteen-first".to_vec()),
        ],
        625 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-sixteen-replaced".to_vec()),
        ],
        626 => vec![(vec![5], b"original-prefix".to_vec())],
        627 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-sixteen-recreated".to_vec()),
        ],
        628 => vec![(vec![5, 0], b"case-closure-edge-tail-sixteen-recreated".to_vec())],
        629 => vec![(vec![5, 0], b"case-closure-edge-tail-sixteen-absent-prefix".to_vec())],
        630 => vec![
            (vec![5], b"case-closure-edge-prefix-sixteen-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-sixteen-joint".to_vec()),
        ],
        631 => vec![(vec![5], b"case-closure-edge-prefix-sixteen-final".to_vec())],
        632 => vec![
            (vec![5], b"case-closure-edge-prefix-sixteen-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-sixteen-recreated".to_vec()),
        ],
        633 => vec![(vec![5, 0], b"case-closure-edge-tail-sixteen-recreated".to_vec())],
        634 => vec![],
        635 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-seventeen-first".to_vec()),
        ],
        636 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-seventeen-replaced".to_vec()),
        ],
        637 => vec![(vec![5], b"original-prefix".to_vec())],
        638 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-seventeen-recreated".to_vec()),
        ],
        639 => vec![(vec![5, 0], b"case-closure-edge-tail-seventeen-recreated".to_vec())],
        640 => vec![(vec![5, 0], b"case-closure-edge-tail-seventeen-absent-prefix".to_vec())],
        641 => vec![
            (vec![5], b"case-closure-edge-prefix-seventeen-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-seventeen-joint".to_vec()),
        ],
        642 => vec![(vec![5], b"case-closure-edge-prefix-seventeen-final".to_vec())],
        643 => vec![
            (vec![5], b"case-closure-edge-prefix-seventeen-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-seventeen-recreated".to_vec()),
        ],
        644 => vec![(vec![5, 0], b"case-closure-edge-tail-seventeen-recreated".to_vec())],
        645 => vec![],
        646 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-eighteen-first".to_vec()),
        ],
        647 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-eighteen-replaced".to_vec()),
        ],
        648 => vec![(vec![5], b"original-prefix".to_vec())],
        649 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-eighteen-recreated".to_vec()),
        ],
        650 => vec![(vec![5, 0], b"case-closure-edge-tail-eighteen-recreated".to_vec())],
        651 => vec![(vec![5, 0], b"case-closure-edge-tail-eighteen-absent-prefix".to_vec())],
        652 => vec![
            (vec![5], b"case-closure-edge-prefix-eighteen-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-eighteen-joint".to_vec()),
        ],
        653 => vec![(vec![5], b"case-closure-edge-prefix-eighteen-final".to_vec())],
        654 => vec![
            (vec![5], b"case-closure-edge-prefix-eighteen-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-eighteen-recreated".to_vec()),
        ],
        655 => vec![(vec![5, 0], b"case-closure-edge-tail-eighteen-recreated".to_vec())],
        656 => vec![],
        657 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-nineteen-first".to_vec()),
        ],
        658 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-nineteen-replaced".to_vec()),
        ],
        659 => vec![(vec![5], b"original-prefix".to_vec())],
        660 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-nineteen-recreated".to_vec()),
        ],
        661 => vec![(vec![5, 0], b"case-closure-edge-tail-nineteen-recreated".to_vec())],
        662 => vec![(vec![5, 0], b"case-closure-edge-tail-nineteen-absent-prefix".to_vec())],
        663 => vec![
            (vec![5], b"case-closure-edge-prefix-nineteen-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-nineteen-joint".to_vec()),
        ],
        664 => vec![(vec![5], b"case-closure-edge-prefix-nineteen-final".to_vec())],
        665 => vec![
            (vec![5], b"case-closure-edge-prefix-nineteen-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-nineteen-recreated".to_vec()),
        ],
        666 => vec![(vec![5, 0], b"case-closure-edge-tail-nineteen-recreated".to_vec())],
        667 => vec![],
        668 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-first".to_vec()),
        ],
        669 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-replaced".to_vec()),
        ],
        670 => vec![(vec![5], b"original-prefix".to_vec())],
        671 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-recreated".to_vec()),
        ],
        672 => vec![(vec![5, 0], b"case-closure-edge-tail-twenty-recreated".to_vec())],
        673 => vec![(vec![5, 0], b"case-closure-edge-tail-twenty-absent-prefix".to_vec())],
        674 => vec![
            (vec![5], b"case-closure-edge-prefix-twenty-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-joint".to_vec()),
        ],
        675 => vec![(vec![5], b"case-closure-edge-prefix-twenty-final".to_vec())],
        676 => vec![
            (vec![5], b"case-closure-edge-prefix-twenty-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-recreated".to_vec()),
        ],
        677 => vec![(vec![5, 0], b"case-closure-edge-tail-twenty-recreated".to_vec())],
        678 => vec![],
        679 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-one-first".to_vec()),
        ],
        680 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-one-replaced".to_vec()),
        ],
        681 => vec![(vec![5], b"original-prefix".to_vec())],
        682 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-one-recreated".to_vec()),
        ],
        683 => vec![(vec![5, 0], b"case-closure-edge-tail-twenty-one-recreated".to_vec())],
        684 => vec![(vec![5, 0], b"case-closure-edge-tail-twenty-one-absent-prefix".to_vec())],
        685 => vec![
            (vec![5], b"case-closure-edge-prefix-twenty-one-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-one-joint".to_vec()),
        ],
        686 => vec![(vec![5], b"case-closure-edge-prefix-twenty-one-final".to_vec())],
        687 => vec![
            (vec![5], b"case-closure-edge-prefix-twenty-one-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-one-recreated".to_vec()),
        ],
        688 => vec![(vec![5, 0], b"case-closure-edge-tail-twenty-one-recreated".to_vec())],
        689 => vec![],
        690 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-two-first".to_vec()),
        ],
        691 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-two-replaced".to_vec()),
        ],
        692 => vec![(vec![5], b"original-prefix".to_vec())],
        693 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-two-recreated".to_vec()),
        ],
        694 => vec![(vec![5, 0], b"case-closure-edge-tail-twenty-two-recreated".to_vec())],
        695 => vec![(vec![5, 0], b"case-closure-edge-tail-twenty-two-absent-prefix".to_vec())],
        696 => vec![
            (vec![5], b"case-closure-edge-prefix-twenty-two-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-two-joint".to_vec()),
        ],
        697 => vec![(vec![5], b"case-closure-edge-prefix-twenty-two-final".to_vec())],
        698 => vec![
            (vec![5], b"case-closure-edge-prefix-twenty-two-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-two-recreated".to_vec()),
        ],
        699 => vec![(vec![5, 0], b"case-closure-edge-tail-twenty-two-recreated".to_vec())],
        700 => vec![],
        701 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-three-first".to_vec()),
        ],
        702 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-three-replaced".to_vec()),
        ],
        703 => vec![(vec![5], b"original-prefix".to_vec())],
        704 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-three-recreated".to_vec()),
        ],
        705 => vec![(vec![5, 0], b"case-closure-edge-tail-twenty-three-recreated".to_vec())],
        706 => vec![(vec![5, 0], b"case-closure-edge-tail-twenty-three-absent-prefix".to_vec())],
        707 => vec![
            (vec![5], b"case-closure-edge-prefix-twenty-three-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-three-joint".to_vec()),
        ],
        708 => vec![(vec![5], b"case-closure-edge-prefix-twenty-three-final".to_vec())],
        709 => vec![
            (vec![5], b"case-closure-edge-prefix-twenty-three-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-three-recreated".to_vec()),
        ],
        710 => vec![(vec![5, 0], b"case-closure-edge-tail-twenty-three-recreated".to_vec())],
        711 => vec![],
        712 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-four-first".to_vec()),
        ],
        713 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-four-replaced".to_vec()),
        ],
        714 => vec![(vec![5], b"original-prefix".to_vec())],
        715 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-four-recreated".to_vec()),
        ],
        716 => vec![(vec![5, 0], b"case-closure-edge-tail-twenty-four-recreated".to_vec())],
        717 => vec![(vec![5, 0], b"case-closure-edge-tail-twenty-four-absent-prefix".to_vec())],
        718 => vec![
            (vec![5], b"case-closure-edge-prefix-twenty-four-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-four-joint".to_vec()),
        ],
        719 => vec![(vec![5], b"case-closure-edge-prefix-twenty-four-final".to_vec())],
        720 => vec![
            (vec![5], b"case-closure-edge-prefix-twenty-four-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-four-recreated".to_vec()),
        ],
        721 => vec![(vec![5, 0], b"case-closure-edge-tail-twenty-four-recreated".to_vec())],
        722 => vec![],
        723 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-five-first".to_vec()),
        ],
        724 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-five-replaced".to_vec()),
        ],
        725 => vec![(vec![5], b"original-prefix".to_vec())],
        726 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-five-recreated".to_vec()),
        ],
        727 => vec![(vec![5, 0], b"case-closure-edge-tail-twenty-five-recreated".to_vec())],
        728 => vec![(vec![5, 0], b"case-closure-edge-tail-twenty-five-absent-prefix".to_vec())],
        729 => vec![
            (vec![5], b"case-closure-edge-prefix-twenty-five-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-five-joint".to_vec()),
        ],
        730 => vec![(vec![5], b"case-closure-edge-prefix-twenty-five-final".to_vec())],
        731 => vec![
            (vec![5], b"case-closure-edge-prefix-twenty-five-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-five-recreated".to_vec()),
        ],
        732 => vec![(vec![5, 0], b"case-closure-edge-tail-twenty-five-recreated".to_vec())],
        733 => vec![],
        734 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-six-first".to_vec()),
        ],
        735 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-six-replaced".to_vec()),
        ],
        736 => vec![(vec![5], b"original-prefix".to_vec())],
        737 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-six-recreated".to_vec()),
        ],
        738 => vec![(vec![5, 0], b"case-closure-edge-tail-twenty-six-recreated".to_vec())],
        739 => vec![(vec![5, 0], b"case-closure-edge-tail-twenty-six-absent-prefix".to_vec())],
        740 => vec![
            (vec![5], b"case-closure-edge-prefix-twenty-six-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-six-joint".to_vec()),
        ],
        741 => vec![(vec![5], b"case-closure-edge-prefix-twenty-six-final".to_vec())],
        742 => vec![
            (vec![5], b"case-closure-edge-prefix-twenty-six-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-six-recreated".to_vec()),
        ],
        743 => vec![(vec![5, 0], b"case-closure-edge-tail-twenty-six-recreated".to_vec())],
        744 => vec![],
        745 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-seven-first".to_vec()),
        ],
        746 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-seven-replaced".to_vec()),
        ],
        747 => vec![(vec![5], b"original-prefix".to_vec())],
        748 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-seven-recreated".to_vec()),
        ],
        749 => vec![(vec![5, 0], b"case-closure-edge-tail-twenty-seven-recreated".to_vec())],
        750 => vec![(vec![5, 0], b"case-closure-edge-tail-twenty-seven-absent-prefix".to_vec())],
        751 => vec![
            (vec![5], b"case-closure-edge-prefix-twenty-seven-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-seven-joint".to_vec()),
        ],
        752 => vec![(vec![5], b"case-closure-edge-prefix-twenty-seven-final".to_vec())],
        753 => vec![
            (vec![5], b"case-closure-edge-prefix-twenty-seven-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-seven-recreated".to_vec()),
        ],
        754 => vec![(vec![5, 0], b"case-closure-edge-tail-twenty-seven-recreated".to_vec())],
        755 => vec![],
        756 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-eight-first".to_vec()),
        ],
        757 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-eight-replaced".to_vec()),
        ],
        758 => vec![(vec![5], b"original-prefix".to_vec())],
        759 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-eight-recreated".to_vec()),
        ],
        760 => vec![(vec![5, 0], b"case-closure-edge-tail-twenty-eight-recreated".to_vec())],
        761 => vec![(vec![5, 0], b"case-closure-edge-tail-twenty-eight-absent-prefix".to_vec())],
        762 => vec![
            (vec![5], b"case-closure-edge-prefix-twenty-eight-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-eight-joint".to_vec()),
        ],
        763 => vec![(vec![5], b"case-closure-edge-prefix-twenty-eight-final".to_vec())],
        764 => vec![
            (vec![5], b"case-closure-edge-prefix-twenty-eight-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-eight-recreated".to_vec()),
        ],
        765 => vec![(vec![5, 0], b"case-closure-edge-tail-twenty-eight-recreated".to_vec())],
        766 => vec![],
        767 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-nine-first".to_vec()),
        ],
        768 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-nine-replaced".to_vec()),
        ],
        769 => vec![(vec![5], b"original-prefix".to_vec())],
        770 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-nine-recreated".to_vec()),
        ],
        771 => vec![(vec![5, 0], b"case-closure-edge-tail-twenty-nine-recreated".to_vec())],
        772 => vec![(vec![5, 0], b"case-closure-edge-tail-twenty-nine-absent-prefix".to_vec())],
        773 => vec![
            (vec![5], b"case-closure-edge-prefix-twenty-nine-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-nine-joint".to_vec()),
        ],
        774 => vec![(vec![5], b"case-closure-edge-prefix-twenty-nine-final".to_vec())],
        775 => vec![
            (vec![5], b"case-closure-edge-prefix-twenty-nine-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-twenty-nine-recreated".to_vec()),
        ],
        776 => vec![(vec![5, 0], b"case-closure-edge-tail-twenty-nine-recreated".to_vec())],
        777 => vec![],
        778 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-thirty-first".to_vec()),
        ],
        779 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-thirty-replaced".to_vec()),
        ],
        780 => vec![(vec![5], b"original-prefix".to_vec())],
        781 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-thirty-recreated".to_vec()),
        ],
        782 => vec![(vec![5, 0], b"case-closure-edge-tail-thirty-recreated".to_vec())],
        783 => vec![(vec![5, 0], b"case-closure-edge-tail-thirty-absent-prefix".to_vec())],
        784 => vec![
            (vec![5], b"case-closure-edge-prefix-thirty-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-thirty-joint".to_vec()),
        ],
        785 => vec![(vec![5], b"case-closure-edge-prefix-thirty-final".to_vec())],
        786 => vec![
            (vec![5], b"case-closure-edge-prefix-thirty-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-thirty-recreated".to_vec()),
        ],
        787 => vec![(vec![5, 0], b"case-closure-edge-tail-thirty-recreated".to_vec())],
        788 => vec![],
        789 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-thirty-one-first".to_vec()),
        ],
        790 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-thirty-one-replaced".to_vec()),
        ],
        791 => vec![(vec![5], b"original-prefix".to_vec())],
        792 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-thirty-one-recreated".to_vec()),
        ],
        793 => vec![(vec![5, 0], b"case-closure-edge-tail-thirty-one-recreated".to_vec())],
        794 => vec![(vec![5, 0], b"case-closure-edge-tail-thirty-one-absent-prefix".to_vec())],
        795 => vec![
            (vec![5], b"case-closure-edge-prefix-thirty-one-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-thirty-one-joint".to_vec()),
        ],
        796 => vec![(vec![5], b"case-closure-edge-prefix-thirty-one-final".to_vec())],
        797 => vec![
            (vec![5], b"case-closure-edge-prefix-thirty-one-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-thirty-one-recreated".to_vec()),
        ],
        798 => vec![(vec![5, 0], b"case-closure-edge-tail-thirty-one-recreated".to_vec())],
        799 => vec![],
        800 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-thirty-two-first".to_vec()),
        ],
        801 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-thirty-two-replaced".to_vec()),
        ],
        802 => vec![(vec![5], b"original-prefix".to_vec())],
        803 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-thirty-two-recreated".to_vec()),
        ],
        804 => vec![(vec![5, 0], b"case-closure-edge-tail-thirty-two-recreated".to_vec())],
        805 => vec![(vec![5, 0], b"case-closure-edge-tail-thirty-two-absent-prefix".to_vec())],
        806 => vec![
            (vec![5], b"case-closure-edge-prefix-thirty-two-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-thirty-two-joint".to_vec()),
        ],
        807 => vec![(vec![5], b"case-closure-edge-prefix-thirty-two-final".to_vec())],
        808 => vec![
            (vec![5], b"case-closure-edge-prefix-thirty-two-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-thirty-two-recreated".to_vec()),
        ],
        809 => vec![(vec![5, 0], b"case-closure-edge-tail-thirty-two-recreated".to_vec())],
        810 => vec![],
        811 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-thirty-three-first".to_vec()),
        ],
        812 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-thirty-three-replaced".to_vec()),
        ],
        813 => vec![(vec![5], b"original-prefix".to_vec())],
        814 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-thirty-three-recreated".to_vec()),
        ],
        815 => vec![(vec![5, 0], b"case-closure-edge-tail-thirty-three-recreated".to_vec())],
        816 => vec![(vec![5, 0], b"case-closure-edge-tail-thirty-three-absent-prefix".to_vec())],
        817 => vec![
            (vec![5], b"case-closure-edge-prefix-thirty-three-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-thirty-three-joint".to_vec()),
        ],
        818 => vec![(vec![5], b"case-closure-edge-prefix-thirty-three-final".to_vec())],
        819 => vec![
            (vec![5], b"case-closure-edge-prefix-thirty-three-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-thirty-three-recreated".to_vec()),
        ],
        820 => vec![(vec![5, 0], b"case-closure-edge-tail-thirty-three-recreated".to_vec())],
        821 => vec![],
        822 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-thirty-four-first".to_vec()),
        ],
        823 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-thirty-four-replaced".to_vec()),
        ],
        824 => vec![(vec![5], b"original-prefix".to_vec())],
        825 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-thirty-four-recreated".to_vec()),
        ],
        826 => vec![(vec![5, 0], b"case-closure-edge-tail-thirty-four-recreated".to_vec())],
        827 => vec![(vec![5, 0], b"case-closure-edge-tail-thirty-four-absent-prefix".to_vec())],
        828 => vec![
            (vec![5], b"case-closure-edge-prefix-thirty-four-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-thirty-four-joint".to_vec()),
        ],
        829 => vec![(vec![5], b"case-closure-edge-prefix-thirty-four-final".to_vec())],
        830 => vec![
            (vec![5], b"case-closure-edge-prefix-thirty-four-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-thirty-four-recreated".to_vec()),
        ],
        831 => vec![(vec![5, 0], b"case-closure-edge-tail-thirty-four-recreated".to_vec())],
        832 => vec![],
        833 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-thirty-five-first".to_vec()),
        ],
        834 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-thirty-five-replaced".to_vec()),
        ],
        835 => vec![(vec![5], b"original-prefix".to_vec())],
        836 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-thirty-five-recreated".to_vec()),
        ],
        837 => vec![(vec![5, 0], b"case-closure-edge-tail-thirty-five-recreated".to_vec())],
        838 => vec![(vec![5, 0], b"case-closure-edge-tail-thirty-five-absent-prefix".to_vec())],
        839 => vec![
            (vec![5], b"case-closure-edge-prefix-thirty-five-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-thirty-five-joint".to_vec()),
        ],
        840 => vec![(vec![5], b"case-closure-edge-prefix-thirty-five-final".to_vec())],
        841 => vec![
            (vec![5], b"case-closure-edge-prefix-thirty-five-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-thirty-five-recreated".to_vec()),
        ],
        842 => vec![(vec![5, 0], b"case-closure-edge-tail-thirty-five-recreated".to_vec())],
        843 => vec![],
        844 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-thirty-six-first".to_vec()),
        ],
        845 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-thirty-six-replaced".to_vec()),
        ],
        846 => vec![(vec![5], b"original-prefix".to_vec())],
        847 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-thirty-six-recreated".to_vec()),
        ],
        848 => vec![(vec![5, 0], b"case-closure-edge-tail-thirty-six-recreated".to_vec())],
        849 => vec![(vec![5, 0], b"case-closure-edge-tail-thirty-six-absent-prefix".to_vec())],
        850 => vec![
            (vec![5], b"case-closure-edge-prefix-thirty-six-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-thirty-six-joint".to_vec()),
        ],
        851 => vec![(vec![5], b"case-closure-edge-prefix-thirty-six-final".to_vec())],
        852 => vec![
            (vec![5], b"case-closure-edge-prefix-thirty-six-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-thirty-six-recreated".to_vec()),
        ],
        853 => vec![(vec![5, 0], b"case-closure-edge-tail-thirty-six-recreated".to_vec())],
        854 => vec![],
        855 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-thirty-seven-first".to_vec()),
        ],
        856 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-thirty-seven-replaced".to_vec()),
        ],
        857 => vec![(vec![5], b"original-prefix".to_vec())],
        858 => vec![
            (vec![5], b"original-prefix".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-thirty-seven-recreated".to_vec()),
        ],
        859 => vec![(vec![5, 0], b"case-closure-edge-tail-thirty-seven-recreated".to_vec())],
        860 => vec![(vec![5, 0], b"case-closure-edge-tail-thirty-seven-absent-prefix".to_vec())],
        861 => vec![
            (vec![5], b"case-closure-edge-prefix-thirty-seven-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-thirty-seven-joint".to_vec()),
        ],
        862 => vec![(vec![5], b"case-closure-edge-prefix-thirty-seven-final".to_vec())],
        863 => vec![
            (vec![5], b"case-closure-edge-prefix-thirty-seven-final".to_vec()),
            (vec![5, 0], b"case-closure-edge-tail-thirty-seven-recreated".to_vec()),
        ],
        864 => vec![(vec![5, 0], b"case-closure-edge-tail-thirty-seven-recreated".to_vec())],
        865 => vec![],
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
    assert_eq!(expected_rows(435), vec![(vec![5], b"original-prefix".to_vec())]);
    assert_eq!(expected_rows(436)[1], expected_rows(437)[0]);
    assert_ne!(expected_rows(436)[0], expected_rows(437)[0]);
    assert_ne!(expected_rows(437)[0], expected_rows(438)[0]);
    assert_eq!(expected_rows(439)[0], expected_rows(435)[0]);
    assert_ne!(expected_rows(438)[0], expected_rows(439)[1]);
    assert_eq!(expected_rows(439)[0], expected_rows(440)[0]);
    assert_eq!(expected_rows(439)[0], expected_rows(441)[0]);
    assert_eq!(expected_rows(441)[1], expected_rows(442)[0]);
    assert!(expected_rows(443).is_empty());
    assert_eq!(expected_rows(444)[0], expected_rows(445)[0]);
    assert_ne!(expected_rows(444)[1], expected_rows(445)[1]);
    assert_eq!(expected_rows(445)[1], expected_rows(446)[0]);
    assert_ne!(expected_rows(446)[0], expected_rows(447)[0]);
    assert_eq!(expected_rows(448)[0], expected_rows(444)[0]);
    assert_ne!(expected_rows(447)[0], expected_rows(448)[1]);
    assert_eq!(expected_rows(448)[0], expected_rows(449)[0]);
    assert_eq!(expected_rows(450)[1], expected_rows(451)[0]);
    assert!(expected_rows(452).is_empty());
    assert_eq!(expected_rows(453), vec![(vec![5], b"original-prefix".to_vec())]);
    assert_eq!(expected_rows(454)[0], expected_rows(455)[0]);
    assert_ne!(expected_rows(454)[1], expected_rows(455)[1]);
    assert_eq!(expected_rows(455)[1], expected_rows(456)[0]);
    assert!(expected_rows(457).is_empty());
    assert_eq!(expected_rows(458), vec![(vec![5, 0], b"closure-edge-interplay-tail-only".to_vec())]);
    assert_eq!(expected_rows(459)[0], expected_rows(453)[0]);
    assert_ne!(expected_rows(458)[0], expected_rows(459)[1]);
    assert_eq!(expected_rows(459)[0], expected_rows(460)[0]);
    assert!(expected_rows(461).is_empty());
    assert_eq!(expected_rows(462)[0], expected_rows(463)[0]);
    assert_ne!(expected_rows(462)[1], expected_rows(463)[1]);
    assert_eq!(expected_rows(463)[0], expected_rows(464)[0]);
    assert_eq!(expected_rows(464)[0], expected_rows(465)[0]);
    assert_eq!(expected_rows(465)[1], expected_rows(466)[0]);
    assert_ne!(expected_rows(466)[0], expected_rows(467)[0]);
    assert_eq!(expected_rows(468)[0], expected_rows(462)[0]);
    assert_ne!(expected_rows(467)[0], expected_rows(468)[1]);
    assert_eq!(expected_rows(468)[1], expected_rows(469)[0]);
    assert!(expected_rows(470).is_empty());
    assert_eq!(expected_rows(471)[0], expected_rows(472)[0]);
    assert_ne!(expected_rows(471)[1], expected_rows(472)[1]);
    assert_eq!(expected_rows(472)[1], expected_rows(473)[0]);
    assert_ne!(expected_rows(473)[0], expected_rows(474)[0]);
    assert!(expected_rows(475).is_empty());
    assert_eq!(expected_rows(476), vec![(vec![5], b"original-prefix".to_vec())]);
    assert_ne!(expected_rows(477)[0], expected_rows(476)[0]);
    assert_eq!(expected_rows(477)[0], (vec![5], b"case-closure-prefix-final".to_vec()));
    assert_eq!(expected_rows(477)[0], expected_rows(478)[0]);
    assert!(expected_rows(479).is_empty());
    assert_eq!(expected_rows(480)[0], expected_rows(481)[0]);
    assert_ne!(expected_rows(480)[1], expected_rows(481)[1]);
    assert_eq!(expected_rows(481)[1], expected_rows(482)[0]);
    assert_ne!(expected_rows(482)[0], expected_rows(483)[0]);
    assert!(expected_rows(484).is_empty());
    assert_eq!(
        expected_rows(485),
        vec![(vec![5], b"original-prefix".to_vec())]
    );
    assert_eq!(
        expected_rows(486)[0],
        (vec![5], b"case-closure-edge-prefix-two-final".to_vec())
    );
    assert_eq!(expected_rows(486)[0], expected_rows(487)[0]);
    assert_eq!(
        expected_rows(486)[1],
        (vec![5, 0], b"case-closure-edge-tail-two-joint".to_vec())
    );
    assert!(expected_rows(488).is_empty());
    assert_eq!(expected_rows(489)[0], expected_rows(490)[0]);
    assert_ne!(expected_rows(489)[1], expected_rows(490)[1]);
    assert_eq!(expected_rows(490)[1], expected_rows(491)[0]);
    assert_eq!(expected_rows(490)[0], expected_rows(494)[0]);
    assert_ne!(expected_rows(491)[0], expected_rows(492)[0]);
    assert!(expected_rows(493).is_empty());
    assert_ne!(expected_rows(494)[0], expected_rows(495)[0]);
    assert_eq!(expected_rows(495)[0], expected_rows(496)[0]);
    assert_eq!(
        expected_rows(495)[1],
        (vec![5, 0], b"case-closure-edge-tail-three-joint".to_vec())
    );
    assert!(expected_rows(497).is_empty());
    assert_eq!(expected_rows(498)[0], expected_rows(506)[0]);
    assert_ne!(expected_rows(498)[1], expected_rows(499)[0]);
    assert_ne!(expected_rows(499), expected_rows(500));
    assert_eq!(expected_rows(500)[0], expected_rows(501)[0]);
    assert_eq!(expected_rows(500)[0], expected_rows(502)[0]);
    assert_eq!(expected_rows(502)[1], expected_rows(503)[0]);
    assert_ne!(expected_rows(503)[0], expected_rows(504)[0]);
    assert!(expected_rows(505).is_empty());
    assert_eq!(
        expected_rows(506),
        vec![(vec![5], b"original-prefix".to_vec())]
    );
    assert_eq!(expected_rows(507)[0], expected_rows(508)[0]);
    assert_ne!(expected_rows(507)[1], expected_rows(508)[1]);
    assert_eq!(expected_rows(508)[1], expected_rows(509)[0]);
    assert_ne!(expected_rows(509)[0], expected_rows(510)[0]);
    assert!(expected_rows(511).is_empty());
    assert_eq!(expected_rows(512), expected_rows(506));
    assert_eq!(
        expected_rows(513)[0],
        (vec![5], b"case-closure-edge-prefix-five-final".to_vec())
    );
    assert_eq!(expected_rows(513)[0], expected_rows(514)[0]);
    assert_eq!(
        expected_rows(513)[1],
        (vec![5, 0], b"case-closure-edge-tail-five-joint".to_vec())
    );
    assert!(expected_rows(515).is_empty());
    assert_eq!(expected_rows(516)[0], expected_rows(517)[0]);
    assert_ne!(expected_rows(516)[1], expected_rows(517)[1]);
    assert_eq!(expected_rows(518), expected_rows(512));
    assert_eq!(expected_rows(519)[1], expected_rows(520)[0]);
    assert_ne!(expected_rows(520)[0], expected_rows(521)[0]);
    assert_eq!(
        expected_rows(522)[0],
        (vec![5], b"case-closure-edge-prefix-six-final".to_vec())
    );
    assert_eq!(expected_rows(522)[1], expected_rows(523)[0]);
    assert!(expected_rows(524).is_empty());
    assert_eq!(expected_rows(525), expected_rows(506));
    assert_eq!(expected_rows(526)[0], expected_rows(527)[0]);
    assert_ne!(expected_rows(526)[1], expected_rows(527)[1]);
    assert_eq!(expected_rows(527)[1], expected_rows(528)[0]);
    assert_ne!(expected_rows(528)[0], expected_rows(529)[0]);
    assert_eq!(
        expected_rows(530)[0],
        (vec![5], b"case-closure-edge-prefix-seven-final".to_vec())
    );
    assert_eq!(expected_rows(530)[0], expected_rows(531)[0]);
    assert_eq!(expected_rows(532)[1], expected_rows(533)[0]);
    assert!(expected_rows(534).is_empty());
    assert_eq!(expected_rows(535), expected_rows(506));
    assert_eq!(expected_rows(536)[0], expected_rows(537)[0]);
    assert_ne!(expected_rows(536)[1], expected_rows(537)[1]);
    assert_eq!(expected_rows(537)[1], expected_rows(538)[0]);
    assert_ne!(expected_rows(538)[0], expected_rows(539)[0]);
    assert_eq!(
        expected_rows(540)[0],
        (vec![5], b"case-closure-edge-prefix-eight-final".to_vec())
    );
    assert_eq!(expected_rows(540)[0], expected_rows(541)[0]);
    assert_eq!(expected_rows(542)[1], expected_rows(543)[0]);
    assert!(expected_rows(544).is_empty());
    assert_eq!(
        expected_rows(545),
        vec![(vec![5, 0], b"case-closure-edge-tail-eight-final".to_vec())]
    );
    assert!(expected_rows(546).is_empty());
    assert_eq!(expected_rows(547)[0], expected_rows(548)[0]);
    assert_ne!(expected_rows(547)[1], expected_rows(548)[1]);
    assert_eq!(expected_rows(549), expected_rows(506));
    assert_eq!(expected_rows(550)[1], expected_rows(551)[0]);
    assert_ne!(expected_rows(551)[0], expected_rows(552)[0]);
    assert_eq!(
        expected_rows(553)[0],
        (vec![5], b"case-closure-edge-prefix-nine-final".to_vec())
    );
    assert_eq!(expected_rows(553)[1], expected_rows(554)[0]);
    assert!(expected_rows(555).is_empty());
    assert_eq!(
        expected_rows(556),
        vec![(vec![5, 0], b"case-closure-edge-tail-nine-final".to_vec())]
    );
    assert!(expected_rows(557).is_empty());
    assert_eq!(expected_rows(558)[0], expected_rows(559)[0]);
    assert_ne!(expected_rows(558)[1], expected_rows(559)[1]);
    assert_eq!(expected_rows(560)[0], expected_rows(559)[1]);
    assert_ne!(expected_rows(560)[0], expected_rows(561)[0]);
    assert_eq!(
        expected_rows(562)[0],
        (vec![5], b"case-closure-edge-prefix-ten-final".to_vec())
    );
    assert_eq!(expected_rows(562)[1], expected_rows(563)[0]);
    assert!(expected_rows(564).is_empty());
    assert_eq!(expected_rows(565), expected_rows(506));
    assert_eq!(expected_rows(566)[1], expected_rows(567)[0]);
    assert!(expected_rows(568).is_empty());
    assert_eq!(expected_rows(569)[0], expected_rows(570)[0]);
    assert_ne!(expected_rows(569)[1], expected_rows(570)[1]);
    assert_eq!(expected_rows(570)[1], expected_rows(571)[0]);
    assert_ne!(expected_rows(571)[0], expected_rows(572)[0]);
    assert_eq!(
        expected_rows(573)[0],
        (vec![5], b"case-closure-edge-prefix-eleven-final".to_vec())
    );
    assert_eq!(expected_rows(573)[0], expected_rows(574)[0]);
    assert_eq!(expected_rows(575)[1], expected_rows(576)[0]);
    assert!(expected_rows(577).is_empty());
    assert_eq!(
        expected_rows(578),
        vec![(vec![5, 0], b"case-closure-edge-tail-eleven-final".to_vec())]
    );
    assert!(expected_rows(579).is_empty());
    assert_eq!(expected_rows(580)[0], expected_rows(581)[0]);
    assert_ne!(expected_rows(580)[1], expected_rows(581)[1]);
    assert_eq!(expected_rows(581)[0], expected_rows(582)[0]);
    assert_eq!(expected_rows(582)[0], expected_rows(583)[0]);
    assert_eq!(expected_rows(583)[1], expected_rows(584)[0]);
    assert_ne!(expected_rows(584)[0], expected_rows(585)[0]);
    assert_eq!(
        expected_rows(586)[0],
        (vec![5], b"case-closure-edge-prefix-twelve-final".to_vec())
    );
    assert_eq!(expected_rows(586)[0], expected_rows(587)[0]);
    assert_eq!(expected_rows(588)[1], expected_rows(589)[0]);
    assert!(expected_rows(590).is_empty());
    assert_eq!(expected_rows(591)[0], expected_rows(592)[0]);
    assert_ne!(expected_rows(591)[1], expected_rows(592)[1]);
    assert_eq!(expected_rows(592)[0], expected_rows(593)[0]);
    assert_eq!(expected_rows(593)[0], expected_rows(594)[0]);
    assert_eq!(expected_rows(594)[1], expected_rows(595)[0]);
    assert_ne!(expected_rows(595)[0], expected_rows(596)[0]);
    assert_eq!(
        expected_rows(597)[0],
        (vec![5], b"case-closure-edge-prefix-thirteen-final".to_vec())
    );
    assert_eq!(expected_rows(597)[0], expected_rows(598)[0]);
    assert_eq!(expected_rows(599)[1], expected_rows(600)[0]);
    assert!(expected_rows(601).is_empty());
    assert_eq!(expected_rows(602)[0], expected_rows(603)[0]);
    assert_ne!(expected_rows(602)[1], expected_rows(603)[1]);
    assert_eq!(expected_rows(603)[0], expected_rows(604)[0]);
    assert_eq!(expected_rows(604)[0], expected_rows(605)[0]);
    assert_eq!(expected_rows(605)[1], expected_rows(606)[0]);
    assert_ne!(expected_rows(606)[0], expected_rows(607)[0]);
    assert_eq!(
        expected_rows(608)[0],
        (vec![5], b"case-closure-edge-prefix-fourteen-final".to_vec())
    );
    assert_eq!(expected_rows(608)[0], expected_rows(609)[0]);
    assert_eq!(expected_rows(610)[1], expected_rows(611)[0]);
    assert!(expected_rows(612).is_empty());
    assert_eq!(expected_rows(613)[0], expected_rows(614)[0]);
    assert_ne!(expected_rows(613)[1], expected_rows(614)[1]);
    assert_eq!(expected_rows(614)[0], expected_rows(615)[0]);
    assert_eq!(expected_rows(615)[0], expected_rows(616)[0]);
    assert_eq!(expected_rows(616)[1], expected_rows(617)[0]);
    assert_ne!(expected_rows(617)[0], expected_rows(618)[0]);
    assert_eq!(
        expected_rows(619)[0],
        (vec![5], b"case-closure-edge-prefix-fifteen-final".to_vec())
    );
    assert_eq!(expected_rows(619)[0], expected_rows(620)[0]);
    assert_eq!(expected_rows(621)[1], expected_rows(622)[0]);
    assert!(expected_rows(623).is_empty());
    assert_eq!(expected_rows(624)[0], expected_rows(625)[0]);
    assert_ne!(expected_rows(624)[1], expected_rows(625)[1]);
    assert_eq!(expected_rows(625)[0], expected_rows(626)[0]);
    assert_eq!(expected_rows(626)[0], expected_rows(627)[0]);
    assert_eq!(expected_rows(627)[1], expected_rows(628)[0]);
    assert_ne!(expected_rows(628)[0], expected_rows(629)[0]);
    assert_eq!(
        expected_rows(630)[0],
        (vec![5], b"case-closure-edge-prefix-sixteen-final".to_vec())
    );
    assert_eq!(expected_rows(630)[0], expected_rows(631)[0]);
    assert_eq!(expected_rows(632)[1], expected_rows(633)[0]);
    assert!(expected_rows(634).is_empty());
    assert_eq!(expected_rows(635)[0], expected_rows(636)[0]);
    assert_ne!(expected_rows(635)[1], expected_rows(636)[1]);
    assert_eq!(expected_rows(636)[0], expected_rows(637)[0]);
    assert_eq!(expected_rows(637)[0], expected_rows(638)[0]);
    assert_eq!(expected_rows(638)[1], expected_rows(639)[0]);
    assert_ne!(expected_rows(639)[0], expected_rows(640)[0]);
    assert_eq!(
        expected_rows(641)[0],
        (vec![5], b"case-closure-edge-prefix-seventeen-final".to_vec())
    );
    assert_eq!(expected_rows(641)[0], expected_rows(642)[0]);
    assert_eq!(expected_rows(643)[1], expected_rows(644)[0]);
    assert!(expected_rows(645).is_empty());
    assert_eq!(expected_rows(646)[0], expected_rows(647)[0]);
    assert_ne!(expected_rows(646)[1], expected_rows(647)[1]);
    assert_eq!(expected_rows(647)[0], expected_rows(648)[0]);
    assert_eq!(expected_rows(648)[0], expected_rows(649)[0]);
    assert_eq!(expected_rows(649)[1], expected_rows(650)[0]);
    assert_ne!(expected_rows(650)[0], expected_rows(651)[0]);
    assert_eq!(
        expected_rows(652)[0],
        (vec![5], b"case-closure-edge-prefix-eighteen-final".to_vec())
    );
    assert_eq!(expected_rows(652)[0], expected_rows(653)[0]);
    assert_eq!(expected_rows(654)[1], expected_rows(655)[0]);
    assert!(expected_rows(656).is_empty());
    assert_eq!(expected_rows(657)[0], expected_rows(658)[0]);
    assert_ne!(expected_rows(657)[1], expected_rows(658)[1]);
    assert_eq!(expected_rows(658)[0], expected_rows(659)[0]);
    assert_eq!(expected_rows(659)[0], expected_rows(660)[0]);
    assert_eq!(expected_rows(660)[1], expected_rows(661)[0]);
    assert_ne!(expected_rows(661)[0], expected_rows(662)[0]);
    assert_eq!(
        expected_rows(663)[0],
        (vec![5], b"case-closure-edge-prefix-nineteen-final".to_vec())
    );
    assert_eq!(expected_rows(663)[0], expected_rows(664)[0]);
    assert_eq!(expected_rows(665)[1], expected_rows(666)[0]);
    assert!(expected_rows(667).is_empty());
    assert_eq!(expected_rows(668)[0], expected_rows(669)[0]);
    assert_ne!(expected_rows(668)[1], expected_rows(669)[1]);
    assert_eq!(expected_rows(669)[0], expected_rows(670)[0]);
    assert_eq!(expected_rows(670)[0], expected_rows(671)[0]);
    assert_eq!(expected_rows(671)[1], expected_rows(672)[0]);
    assert_ne!(expected_rows(672)[0], expected_rows(673)[0]);
    assert_eq!(
        expected_rows(674)[0],
        (vec![5], b"case-closure-edge-prefix-twenty-final".to_vec())
    );
    assert_eq!(expected_rows(674)[0], expected_rows(675)[0]);
    assert_eq!(expected_rows(676)[1], expected_rows(677)[0]);
    assert!(expected_rows(678).is_empty());
    assert_eq!(expected_rows(679)[0], expected_rows(680)[0]);
    assert_ne!(expected_rows(679)[1], expected_rows(680)[1]);
    assert_eq!(expected_rows(680)[0], expected_rows(681)[0]);
    assert_eq!(expected_rows(681)[0], expected_rows(682)[0]);
    assert_eq!(expected_rows(682)[1], expected_rows(683)[0]);
    assert_ne!(expected_rows(683)[0], expected_rows(684)[0]);
    assert_eq!(
        expected_rows(685)[0],
        (vec![5], b"case-closure-edge-prefix-twenty-one-final".to_vec())
    );
    assert_eq!(expected_rows(685)[0], expected_rows(686)[0]);
    assert_eq!(expected_rows(687)[1], expected_rows(688)[0]);
    assert!(expected_rows(689).is_empty());
    assert_eq!(expected_rows(690)[0], expected_rows(691)[0]);
    assert_ne!(expected_rows(690)[1], expected_rows(691)[1]);
    assert_eq!(expected_rows(691)[0], expected_rows(692)[0]);
    assert_eq!(expected_rows(692)[0], expected_rows(693)[0]);
    assert_eq!(expected_rows(693)[1], expected_rows(694)[0]);
    assert_ne!(expected_rows(694)[0], expected_rows(695)[0]);
    assert_eq!(
        expected_rows(696)[0],
        (vec![5], b"case-closure-edge-prefix-twenty-two-final".to_vec())
    );
    assert_eq!(expected_rows(696)[0], expected_rows(697)[0]);
    assert_eq!(expected_rows(698)[1], expected_rows(699)[0]);
    assert!(expected_rows(700).is_empty());
    assert_eq!(expected_rows(701)[0], expected_rows(702)[0]);
    assert_ne!(expected_rows(701)[1], expected_rows(702)[1]);
    assert_eq!(expected_rows(702)[0], expected_rows(703)[0]);
    assert_eq!(expected_rows(703)[0], expected_rows(704)[0]);
    assert_eq!(expected_rows(704)[1], expected_rows(705)[0]);
    assert_ne!(expected_rows(705)[0], expected_rows(706)[0]);
    assert_eq!(
        expected_rows(707)[0],
        (vec![5], b"case-closure-edge-prefix-twenty-three-final".to_vec())
    );
    assert_eq!(expected_rows(707)[0], expected_rows(708)[0]);
    assert_eq!(expected_rows(709)[1], expected_rows(710)[0]);
    assert!(expected_rows(711).is_empty());
    assert_eq!(expected_rows(712)[0], expected_rows(713)[0]);
    assert_ne!(expected_rows(712)[1], expected_rows(713)[1]);
    assert_eq!(expected_rows(713)[0], expected_rows(714)[0]);
    assert_eq!(expected_rows(714)[0], expected_rows(715)[0]);
    assert_eq!(expected_rows(715)[1], expected_rows(716)[0]);
    assert_ne!(expected_rows(716)[0], expected_rows(717)[0]);
    assert_eq!(
        expected_rows(718)[0],
        (vec![5], b"case-closure-edge-prefix-twenty-four-final".to_vec())
    );
    assert_eq!(expected_rows(718)[0], expected_rows(719)[0]);
    assert_eq!(expected_rows(720)[1], expected_rows(721)[0]);
    assert!(expected_rows(722).is_empty());
    assert_eq!(expected_rows(723)[0], expected_rows(724)[0]);
    assert_ne!(expected_rows(723)[1], expected_rows(724)[1]);
    assert_eq!(expected_rows(724)[0], expected_rows(725)[0]);
    assert_eq!(expected_rows(725)[0], expected_rows(726)[0]);
    assert_eq!(expected_rows(726)[1], expected_rows(727)[0]);
    assert_ne!(expected_rows(727)[0], expected_rows(728)[0]);
    assert_eq!(
        expected_rows(729)[0],
        (vec![5], b"case-closure-edge-prefix-twenty-five-final".to_vec())
    );
    assert_eq!(expected_rows(729)[0], expected_rows(730)[0]);
    assert_eq!(expected_rows(731)[1], expected_rows(732)[0]);
    assert!(expected_rows(733).is_empty());
    assert_eq!(expected_rows(734)[0], expected_rows(735)[0]);
    assert_ne!(expected_rows(734)[1], expected_rows(735)[1]);
    assert_eq!(expected_rows(735)[0], expected_rows(736)[0]);
    assert_eq!(expected_rows(736)[0], expected_rows(737)[0]);
    assert_eq!(expected_rows(737)[1], expected_rows(738)[0]);
    assert_ne!(expected_rows(738)[0], expected_rows(739)[0]);
    assert_eq!(
        expected_rows(740)[0],
        (vec![5], b"case-closure-edge-prefix-twenty-six-final".to_vec())
    );
    assert_eq!(expected_rows(740)[0], expected_rows(741)[0]);
    assert_eq!(expected_rows(742)[1], expected_rows(743)[0]);
    assert!(expected_rows(744).is_empty());
    assert_eq!(expected_rows(745)[0], expected_rows(746)[0]);
    assert_ne!(expected_rows(745)[1], expected_rows(746)[1]);
    assert_eq!(expected_rows(746)[0], expected_rows(747)[0]);
    assert_eq!(expected_rows(747)[0], expected_rows(748)[0]);
    assert_eq!(expected_rows(748)[1], expected_rows(749)[0]);
    assert_ne!(expected_rows(749)[0], expected_rows(750)[0]);
    assert_eq!(
        expected_rows(751)[0],
        (vec![5], b"case-closure-edge-prefix-twenty-seven-final".to_vec())
    );
    assert_eq!(expected_rows(751)[0], expected_rows(752)[0]);
    assert_eq!(expected_rows(753)[1], expected_rows(754)[0]);
    assert!(expected_rows(755).is_empty());
    assert_eq!(expected_rows(756)[0], expected_rows(757)[0]);
    assert_ne!(expected_rows(756)[1], expected_rows(757)[1]);
    assert_eq!(expected_rows(757)[0], expected_rows(758)[0]);
    assert_eq!(expected_rows(758)[0], expected_rows(759)[0]);
    assert_eq!(expected_rows(759)[1], expected_rows(760)[0]);
    assert_ne!(expected_rows(760)[0], expected_rows(761)[0]);
    assert_eq!(
        expected_rows(762)[0],
        (vec![5], b"case-closure-edge-prefix-twenty-eight-final".to_vec())
    );
    assert_eq!(expected_rows(762)[0], expected_rows(763)[0]);
    assert_eq!(expected_rows(764)[1], expected_rows(765)[0]);
    assert!(expected_rows(766).is_empty());
    assert_eq!(expected_rows(767)[0], expected_rows(768)[0]);
    assert_ne!(expected_rows(767)[1], expected_rows(768)[1]);
    assert_eq!(expected_rows(768)[0], expected_rows(769)[0]);
    assert_eq!(expected_rows(769)[0], expected_rows(770)[0]);
    assert_eq!(expected_rows(770)[1], expected_rows(771)[0]);
    assert_ne!(expected_rows(771)[0], expected_rows(772)[0]);
    assert_eq!(
        expected_rows(773)[0],
        (vec![5], b"case-closure-edge-prefix-twenty-nine-final".to_vec())
    );
    assert_eq!(expected_rows(773)[0], expected_rows(774)[0]);
    assert_eq!(expected_rows(775)[1], expected_rows(776)[0]);
    assert!(expected_rows(777).is_empty());
    assert_eq!(expected_rows(778)[0], expected_rows(779)[0]);
    assert_ne!(expected_rows(778)[1], expected_rows(779)[1]);
    assert_eq!(expected_rows(779)[0], expected_rows(780)[0]);
    assert_eq!(expected_rows(780)[0], expected_rows(781)[0]);
    assert_eq!(expected_rows(781)[1], expected_rows(782)[0]);
    assert_ne!(expected_rows(782)[0], expected_rows(783)[0]);
    assert_eq!(
        expected_rows(784)[0],
        (vec![5], b"case-closure-edge-prefix-thirty-final".to_vec())
    );
    assert_eq!(expected_rows(784)[0], expected_rows(785)[0]);
    assert_eq!(expected_rows(786)[1], expected_rows(787)[0]);
    assert!(expected_rows(788).is_empty());
    assert_eq!(expected_rows(789)[0], expected_rows(790)[0]);
    assert_ne!(expected_rows(789)[1], expected_rows(790)[1]);
    assert_eq!(expected_rows(790)[0], expected_rows(791)[0]);
    assert_eq!(expected_rows(791)[0], expected_rows(792)[0]);
    assert_eq!(expected_rows(792)[1], expected_rows(793)[0]);
    assert_ne!(expected_rows(793)[0], expected_rows(794)[0]);
    assert_eq!(
        expected_rows(795)[0],
        (vec![5], b"case-closure-edge-prefix-thirty-one-final".to_vec())
    );
    assert_eq!(expected_rows(795)[0], expected_rows(796)[0]);
    assert_eq!(expected_rows(797)[1], expected_rows(798)[0]);
    assert!(expected_rows(799).is_empty());
    assert_eq!(expected_rows(800)[0], expected_rows(801)[0]);
    assert_ne!(expected_rows(800)[1], expected_rows(801)[1]);
    assert_eq!(expected_rows(801)[0], expected_rows(802)[0]);
    assert_eq!(expected_rows(802)[0], expected_rows(803)[0]);
    assert_eq!(expected_rows(803)[1], expected_rows(804)[0]);
    assert_ne!(expected_rows(804)[0], expected_rows(805)[0]);
    assert_eq!(expected_rows(806)[0], expected_rows(807)[0]);
    assert_eq!(expected_rows(808)[1], expected_rows(809)[0]);
    assert!(expected_rows(810).is_empty());
    assert_eq!(expected_rows(811)[0], expected_rows(812)[0]);
    assert_ne!(expected_rows(811)[1], expected_rows(812)[1]);
    assert_eq!(expected_rows(812)[0], expected_rows(813)[0]);
    assert_eq!(expected_rows(813)[0], expected_rows(814)[0]);
    assert_eq!(expected_rows(814)[1], expected_rows(815)[0]);
    assert_ne!(expected_rows(815)[0], expected_rows(816)[0]);
    assert_eq!(expected_rows(817)[0], expected_rows(818)[0]);
    assert_eq!(expected_rows(819)[1], expected_rows(820)[0]);
    assert!(expected_rows(821).is_empty());
    assert_eq!(expected_rows(822)[0], expected_rows(823)[0]);
    assert_ne!(expected_rows(822)[1], expected_rows(823)[1]);
    assert_eq!(expected_rows(823)[0], expected_rows(824)[0]);
    assert_eq!(expected_rows(824)[0], expected_rows(825)[0]);
    assert_eq!(expected_rows(825)[1], expected_rows(826)[0]);
    assert_ne!(expected_rows(826)[0], expected_rows(827)[0]);
    assert_eq!(expected_rows(828)[0], expected_rows(829)[0]);
    assert_eq!(expected_rows(830)[1], expected_rows(831)[0]);
    assert!(expected_rows(832).is_empty());
    assert_eq!(expected_rows(833)[0], expected_rows(834)[0]);
    assert_ne!(expected_rows(833)[1], expected_rows(834)[1]);
    assert_eq!(expected_rows(834)[0], expected_rows(835)[0]);
    assert_eq!(expected_rows(835)[0], expected_rows(836)[0]);
    assert_eq!(expected_rows(836)[1], expected_rows(837)[0]);
    assert_ne!(expected_rows(837)[0], expected_rows(838)[0]);
    assert_eq!(expected_rows(839)[0], expected_rows(840)[0]);
    assert_eq!(expected_rows(841)[1], expected_rows(842)[0]);
    assert!(expected_rows(843).is_empty());
    assert_eq!(expected_rows(844)[0], expected_rows(845)[0]);
    assert_ne!(expected_rows(844)[1], expected_rows(845)[1]);
    assert_eq!(expected_rows(845)[0], expected_rows(846)[0]);
    assert_eq!(expected_rows(846)[0], expected_rows(847)[0]);
    assert_eq!(expected_rows(847)[1], expected_rows(848)[0]);
    assert_ne!(expected_rows(848)[0], expected_rows(849)[0]);
    assert_eq!(expected_rows(850)[0], expected_rows(851)[0]);
    assert_eq!(expected_rows(852)[1], expected_rows(853)[0]);
    assert!(expected_rows(854).is_empty());
    assert_eq!(expected_rows(855)[0], expected_rows(856)[0]);
    assert_ne!(expected_rows(855)[1], expected_rows(856)[1]);
    assert_ne!(expected_rows(856)[1], expected_rows(858)[1]);
    assert_eq!(expected_rows(857)[0], expected_rows(858)[0]);
    assert_eq!(expected_rows(858)[1], expected_rows(859)[0]);
    assert_ne!(expected_rows(859)[0], expected_rows(860)[0]);
    assert_eq!(expected_rows(861)[0], expected_rows(862)[0]);
    assert_eq!(expected_rows(862).len(), 1);
    assert_eq!(expected_rows(862)[0], expected_rows(863)[0]);
    assert_eq!(expected_rows(863)[1], expected_rows(864)[0]);
    assert!(expected_rows(865).is_empty());

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
