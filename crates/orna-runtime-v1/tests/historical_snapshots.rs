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

    for generation in 1..=308_u64 {
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
        let prefix_value = match generation {
            256 | 259 | 261 | 263 | 264 | 266 | 273 | 277 | 282 | 284 | 288 | 291 | 295 | 298
            | 303 | 308 => None,
            257 => Some(b"intermediate-prefix".to_vec()),
            265 => Some(b"original-prefix".to_vec()),
            _ => Some(b"original-prefix".to_vec()),
        };
        let mut mutations = Vec::new();
        if !matches!(
            generation,
            264 | 268 | 269 | 270 | 271 | 272 | 278 | 279 | 281 | 285 | 287 | 290 | 292
                | 294 | 297 | 299 | 300 | 301 | 304 | 306 | 307
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
        }
        commit(&state, writer, &mutations, 57).await;

        if (255..=308).contains(&generation) {
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
            304, 305, 306, 307, 308,
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
