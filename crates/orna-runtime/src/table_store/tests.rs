use super::*;

use crate::table_snapshot::MAX_RUNTIME_QUERY_PAGE_ROWS;

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

/// A committed relation: `rows` as `(storage name, key, value)` and the
/// committed identities `identities` as `(table name, object id)`.
async fn committed_relation(
    rows: &[(String, Vec<u8>, Vec<u8>)],
    identities: &[(&str, [u8; 16])],
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
    connection
        .execute(
            "CREATE TABLE table_identity (
                 table_name TEXT NOT NULL,
                 object_id BLOB NOT NULL,
                 PRIMARY KEY (table_name, object_id)
             )",
            (),
        )
        .await
        .unwrap();
    for (storage, key, value) in rows {
        connection
            .execute(
                "INSERT INTO table_row (table_id, row_key, row_value) VALUES (?1, ?2, ?3)",
                params![storage.clone(), key.clone(), value.clone()],
            )
            .await
            .unwrap();
    }
    for (table, id) in identities {
        connection
            .execute(
                "INSERT INTO table_identity (table_name, object_id) VALUES (?1, ?2)",
                params![table.to_string(), id.to_vec()],
            )
            .await
            .unwrap();
    }
    (database, connection)
}

/// Pages `table` `limit` rows at a time until no page follows, returning the
/// rows in order and the number of pages read.
async fn paged_rows(
    connection: &Connection,
    table: &str,
    limit: usize,
) -> (Vec<(Vec<u8>, Vec<u8>)>, usize) {
    let mut collected = Vec::new();
    let mut after: Option<Vec<u8>> = None;
    let mut pages = 0;
    loop {
        let page = committed_relation_page(connection, table, after.as_deref(), limit)
            .await
            .unwrap();
        pages += 1;
        collected.extend(page.rows);
        match page.next {
            Some(next) => after = Some(next),
            None => return (collected, pages),
        }
    }
}

#[tokio::test]
async fn committed_relation_page_walks_the_relation_in_key_order() {
    // Inserted in reverse, so the page order must come from the keys.
    let rows: Vec<(String, Vec<u8>, Vec<u8>)> = (0..25u8)
        .rev()
        .map(|n| ("items".to_string(), vec![n], vec![b'v', n]))
        .collect();
    let (_database, connection) = committed_relation(&rows, &[]).await;
    let whole = committed_relation_rows(&connection, "items").await.unwrap();
    assert_eq!(whole.len(), 25);
    assert!(whole.windows(2).all(|pair| pair[0].0 < pair[1].0));
    for (limit, pages) in [(1, 25), (10, 3), (25, 1)] {
        assert_eq!(
            paged_rows(&connection, "items", limit).await,
            (whole.clone(), pages)
        );
    }
}

#[tokio::test]
async fn committed_relation_page_of_an_empty_relation_is_empty_and_final() {
    let (_database, connection) = committed_relation(&[], &[]).await;
    let page = committed_relation_page(&connection, "items", None, 4)
        .await
        .unwrap();
    assert!(page.rows.is_empty());
    assert_eq!(page.next, None);
}

#[tokio::test]
async fn committed_relation_page_refuses_a_limit_outside_the_page_bound() {
    let (_database, connection) = committed_relation(&[], &[]).await;
    for limit in [0, MAX_RUNTIME_QUERY_PAGE_ROWS + 1] {
        assert!(matches!(
            committed_relation_page(&connection, "items", None, limit).await,
            Err(RuntimeError::InvalidTablePageLimit)
        ));
    }
}

#[tokio::test]
async fn committed_relation_page_reads_rows_under_the_committed_identity_name() {
    let id = [7u8; 16];
    let named = table_storage_key("items", Some(TableObjectId::new(id)));
    let rows = vec![
        ("items".to_string(), vec![1], vec![b'a']),
        (named, vec![2], vec![b'b']),
        ("items".to_string(), vec![3], vec![b'c']),
    ];
    let (_database, connection) = committed_relation(&rows, &[("items", id)]).await;
    let whole = committed_relation_rows(&connection, "items").await.unwrap();
    assert_eq!(
        whole.iter().map(|(key, _)| key[0]).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(paged_rows(&connection, "items", 1).await, (whole, 3));
}

#[tokio::test]
async fn committed_relation_page_fails_closed_on_a_key_stored_under_two_names_at_the_boundary() {
    let id = [9u8; 16];
    let named = table_storage_key("items", Some(TableObjectId::new(id)));
    let rows = vec![
        ("items".to_string(), vec![1], vec![b'a']),
        ("items".to_string(), vec![2], vec![b'b']),
        (named, vec![2], vec![b'B']),
    ];
    let (_database, connection) = committed_relation(&rows, &[("items", id)]).await;
    // The two rows at key 2 straddle a two-row page, so no resume is unambiguous.
    assert!(matches!(
        committed_relation_page(&connection, "items", None, 2).await,
        Err(RuntimeError::RecoveryInvalid)
    ));
    // Wholly inside one page, the same two rows are returned.
    let inside = committed_relation_page(&connection, "items", None, 3)
        .await
        .unwrap();
    assert_eq!(inside.rows.len(), 3);
    assert_eq!(inside.next, None);
}

/// Peak-memory fixture: a 20,000-row file database of 1 KiB values, read by one
/// mode per process under `/usr/bin/time -v`. `ORNA_PAGE_RSS_MODE` is `build`
/// (write the database at `ORNA_PAGE_RSS_DB`), `open` (open it, read nothing),
/// `whole` (`committed_table_rows`) or `paged` (`committed_table_page` at the
/// largest page limit).
#[ignore = "RSS fixture: run one mode per process under /usr/bin/time -v"]
#[tokio::test]
async fn committed_relation_page_rss_fixture() {
    let mode = std::env::var("ORNA_PAGE_RSS_MODE").unwrap();
    let path = std::env::var("ORNA_PAGE_RSS_DB").unwrap();
    let database = libsql::Builder::new_local(&path).build().await.unwrap();
    let connection = database.connect().unwrap();
    if mode == "build" {
        connection
            .execute(
                "CREATE TABLE table_row (table_id TEXT NOT NULL, row_key BLOB NOT NULL,
                 row_value BLOB NOT NULL, PRIMARY KEY (table_id, row_key))",
                (),
            )
            .await
            .unwrap();
        connection
            .execute(
                "CREATE TABLE table_identity (table_name TEXT NOT NULL, object_id BLOB NOT NULL,
                 PRIMARY KEY (table_name, object_id))",
                (),
            )
            .await
            .unwrap();
        connection.execute("BEGIN", ()).await.unwrap();
        for n in 0..20_000u32 {
            connection
                .execute(
                    "INSERT INTO table_row (table_id, row_key, row_value) VALUES ('items', ?1, ?2)",
                    params![n.to_be_bytes().to_vec(), vec![0xA5u8; 1024]],
                )
                .await
                .unwrap();
        }
        connection.execute("COMMIT", ()).await.unwrap();
        return;
    }
    let mut count = 0usize;
    match mode.as_str() {
        "open" => {}
        "whole" => {
            count = committed_relation_rows(&connection, "items")
                .await
                .unwrap()
                .len();
        }
        "paged" => {
            let mut after: Option<Vec<u8>> = None;
            loop {
                let page = committed_relation_page(
                    &connection,
                    "items",
                    after.as_deref(),
                    MAX_RUNTIME_QUERY_PAGE_ROWS,
                )
                .await
                .unwrap();
                count += page.rows.len();
                match page.next {
                    Some(next) => after = Some(next),
                    None => break,
                }
            }
        }
        other => panic!("unknown ORNA_PAGE_RSS_MODE {other}"),
    }
    println!("mode={mode} rows={count}");
    if mode != "open" {
        assert_eq!(count, 20_000);
    }
}
