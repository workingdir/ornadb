use super::*;

/// An in-memory `table_row` relation holding `live` rows of `storage`. The
/// database handle is returned so it outlives the connection.
async fn live_relation(
    storage: &str,
    live: Vec<(Vec<u8>, Vec<u8>)>,
) -> (libsql::Database, Connection) {
    let database = libsql::Builder::new_local(":memory:")
        .build()
        .await
        .unwrap();
    let connection = database.connect().unwrap();
    connection
        .execute(
            "CREATE TABLE table_row (
                 table_id TEXT NOT NULL,
                 row_key BLOB NOT NULL,
                 row_value BLOB NOT NULL,
                 PRIMARY KEY (table_id, row_key)
             )",
            (),
        )
        .await
        .unwrap();
    for (key, value) in live {
        connection
            .execute(
                "INSERT INTO table_row (table_id, row_key, row_value) VALUES (?1, ?2, ?3)",
                params![storage.to_string(), key, value],
            )
            .await
            .unwrap();
    }
    (database, connection)
}

/// The `items` relation as `table_rows_tx` reads it beneath `base`.
async fn items(connection: &Connection, base: &CommittedTableBase<'_>) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut rows = table_rows_tx(connection, &["items".to_string()], &BTreeMap::new(), base)
        .await
        .unwrap();
    rows.remove("items").unwrap()
}

#[tokio::test]
async fn bound_base_merges_untouched_committed_rows_with_overridden_live_rows() {
    let storage = table_storage_key("items", None);
    let committed = BTreeMap::from([
        (b"a".to_vec(), b"A0".to_vec()),
        (b"c".to_vec(), b"C0".to_vec()),
        (b"e".to_vec(), b"E0".to_vec()),
        (b"g".to_vec(), b"G0".to_vec()),
    ]);
    // `b` is live but untouched since the pin, so the base does not admit it.
    let (_database, connection) = live_relation(
        &storage,
        vec![
            (b"b".to_vec(), b"B1".to_vec()),
            (b"d".to_vec(), b"D1".to_vec()),
            (b"e".to_vec(), b"E1".to_vec()),
        ],
    )
    .await;
    let base = CommittedTableBase {
        tables: Some(BTreeMap::from([(
            storage,
            CommittedTable {
                rows: &committed,
                // `c` was deleted after the pin; `d` and `e` were written.
                overridden: BTreeSet::from([b"c".to_vec(), b"d".to_vec(), b"e".to_vec()]),
            },
        )])),
    };
    assert_eq!(
        items(&connection, &base).await,
        vec![
            (b"a".to_vec(), b"A0".to_vec()),
            (b"d".to_vec(), b"D1".to_vec()),
            (b"e".to_vec(), b"E1".to_vec()),
            (b"g".to_vec(), b"G0".to_vec()),
        ]
    );
}

#[tokio::test]
async fn unbound_base_yields_the_whole_live_relation_in_key_order() {
    let storage = table_storage_key("items", None);
    let (_database, connection) = live_relation(
        &storage,
        vec![
            (b"y".to_vec(), b"Y1".to_vec()),
            (b"x".to_vec(), b"X1".to_vec()),
        ],
    )
    .await;
    assert_eq!(
        items(&connection, &CommittedTableBase::default()).await,
        vec![
            (b"x".to_vec(), b"X1".to_vec()),
            (b"y".to_vec(), b"Y1".to_vec()),
        ]
    );
}

/// Peak-RSS fixture: 100k committed rows beneath a bound base, every 100th
/// key overridden by a live row. Run alone under `/usr/bin/time -f %M`.
#[ignore = "RSS fixture: run alone under /usr/bin/time"]
#[tokio::test]
async fn committed_relation_of_100k_rows_merges_in_key_order() {
    let storage = table_storage_key("items", None);
    let committed: BTreeMap<Vec<u8>, Vec<u8>> = (0u32..100_000)
        .map(|n| (n.to_be_bytes().to_vec(), vec![b'c'; 32]))
        .collect();
    let overridden: BTreeSet<Vec<u8>> = (0u32..100_000)
        .step_by(100)
        .map(|n| n.to_be_bytes().to_vec())
        .collect();
    let live: Vec<(Vec<u8>, Vec<u8>)> = overridden
        .iter()
        .map(|key| (key.clone(), b"L".to_vec()))
        .collect();
    let (_database, connection) = live_relation(&storage, live).await;
    let base = CommittedTableBase {
        tables: Some(BTreeMap::from([(
            storage,
            CommittedTable {
                rows: &committed,
                overridden,
            },
        )])),
    };
    let rows = items(&connection, &base).await;
    assert_eq!(rows.len(), 100_000);
    assert!(rows.windows(2).all(|pair| pair[0].0 < pair[1].0));
    assert_eq!(rows[1], (1u32.to_be_bytes().to_vec(), vec![b'c'; 32]));
    assert_eq!(rows[100], (100u32.to_be_bytes().to_vec(), b"L".to_vec()));
}
