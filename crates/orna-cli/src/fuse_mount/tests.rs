#[test]
fn orna_string_literal_round_trips_through_the_evaluator() {
    use orna_evaluator::{Limits, evaluate_expression};
    use orna_foundation::CanonicalValue;
    use orna_value::Raw;
    let value = "nul\0 quote\" back\\slash newline\n cr\r crlf\r\n tab\t brace{x} bell\u{7} astral\u{1F600}";
    let literal = super::orna_string_literal(value.as_bytes()).expect("literal is UTF-8");
    let evaluated = evaluate_expression(
        &literal,
        &std::collections::BTreeMap::new(),
        Limits::default(),
    )
    .expect("the writer's literal evaluates");
    assert_eq!(
        evaluated,
        CanonicalValue::new(Raw::Text(value.into())).expect("canonical text"),
        "NUL, quotes, backslashes, newlines, CRLF, braces and astral scalars round-trip"
    );
}

#[test]
#[allow(clippy::too_many_lines, clippy::trivially_copy_pass_by_ref)]
fn candidate_read_is_chunked_and_stops_at_candidate_end() {
    use super::{CANDIDATE_CHUNK_BYTES, read_candidate_range};
    let total = CANDIDATE_CHUNK_BYTES * 2 + 5;
    let bytes: Vec<u8> = (0..total)
        .map(|index| u8::try_from(index % 251).expect("masked to one byte"))
        .collect();
    let mut requests = Vec::new();
    let read = read_candidate_range(
        |offset, length| {
            requests.push(length);
            let start = usize::try_from(offset)
                .expect("offset fits usize")
                .min(bytes.len());
            let end = (start + length).min(bytes.len());
            Ok::<_, ()>(bytes[start..end].to_vec())
        },
        u64::try_from(CANDIDATE_CHUNK_BYTES).expect("chunk bound fits u64") / 2,
        u32::try_from(total + 10).expect("total fits u32"),
    )
    .unwrap();
    assert_eq!(read, bytes[CANDIDATE_CHUNK_BYTES / 2..].to_vec());
    assert!(
        requests
            .iter()
            .all(|length| *length <= CANDIDATE_CHUNK_BYTES)
    );
    assert!(requests.len() >= 3);
}

#[test]
fn candidate_read_propagates_backing_errors() {
    use super::read_candidate_range;
    let failed = read_candidate_range(|_, _| Err::<Vec<u8>, _>("backing"), 0, 8);
    assert_eq!(failed, Err("backing"));
}

#[test]
fn truncated_and_stashed_ranges_clamp_to_candidate_size() {
    use super::{RetentionError, stashed_range, truncated_window};
    let baseline: &[u8] = b"abcdef";
    let window = |size: u64, offset: u64, length: usize| {
        truncated_window(
            baseline.len() as u64,
            size,
            offset,
            length,
            |start, count| Ok::<_, RetentionError>(stashed_range(baseline, start, count)),
        )
        .unwrap()
    };
    assert_eq!(window(4, 0, 10), b"abcd".to_vec());
    assert_eq!(window(8, 4, 10), vec![b'e', b'f', 0, 0]);
    assert_eq!(window(4, 6, 10), Vec::<u8>::new());
    assert!(
        truncated_window(6, 4, 0, 4, |_, _| Err(RetentionError::from(String::from(
            "boom"
        ))))
        .is_err()
    );
    assert_eq!(stashed_range(baseline, 2, 3), b"cde".to_vec());
    assert_eq!(stashed_range(baseline, 9, 3), Vec::<u8>::new());
}

#[test]
fn reply_read_size_caps_each_reply_at_one_chunk() {
    let chunk = u32::try_from(super::CANDIDATE_CHUNK_BYTES).unwrap_or(u32::MAX);
    assert_eq!(super::reply_read_size(u32::MAX), chunk);
    assert_eq!(super::reply_read_size(5), 5);
}

#[test]
fn quota_exceeded_maps_to_edquot_and_backing_io_to_eio() {
    use orna_runtime::vfs::EditDraftError;
    assert_eq!(
        super::draft_errno(&Err(EditDraftError::QuotaExceeded)).code(),
        122
    );
    assert_eq!(
        super::draft_errno(&Err(EditDraftError::FileTooLarge)).code(),
        122
    );
    assert_eq!(
        super::draft_errno(&Err(EditDraftError::BackingIo)).code(),
        5
    );
}

#[test]
fn aggregate_quota_refusal_at_save_is_edquot_on_every_commit_path() {
    use orna_runtime::vfs::{EditDraftError, VfsSaveError};
    // The document, fsync and flush replies all return `DocumentCommitError::errno()`
    // of the error `save_failure` classifies (41-vfs.md:105: EDQUOT for resource limits).
    for error in [EditDraftError::QuotaExceeded, EditDraftError::FileTooLarge] {
        let failure = super::save_failure(
            "mounted row save failed",
            VfsSaveError::<orna_runtime::TableActivationError>::Backing(error),
        );
        assert_eq!(failure.errno().code(), 122, "{failure:?}");
    }
    let io = super::save_failure(
        "mounted row save failed",
        VfsSaveError::<orna_runtime::TableActivationError>::Backing(EditDraftError::BackingIo),
    );
    assert_eq!(io.errno().code(), 5, "{io:?}");
}

#[test]
fn temporary_commit_backing_io_failure_is_eio_and_malformed_draft_is_einval() {
    use orna_runtime::vfs::{EditDraftError, VfsSaveError};
    use orna_runtime::{RuntimeError, TableActivationError};
    let malformed = || {
        VfsSaveError::<TableActivationError>::Activation(TableActivationError::Runtime(
            RuntimeError::InvalidTableMutation,
        ))
    };
    let cases: [(bool, VfsSaveError<TableActivationError>, i32); 3] = [
        (true, malformed(), 5),
        (false, malformed(), 22),
        (false, VfsSaveError::Backing(EditDraftError::BackingIo), 5),
    ];
    for (backing_io_failed, error, errno) in cases {
        let failure = super::temporary_commit_failure(backing_io_failed, error);
        assert_eq!(failure.errno().code(), errno, "{failure:?}");
    }
    let io = super::temporary_commit_failure(true, malformed());
    assert!(
        format!("{io:?}").contains("backing I/O failure"),
        "the I/O cause is distinct from a malformed draft: {io:?}"
    );
}

use std::ffi::OsStr;

use super::{EDITABLE_OPEN_FLAGS, EDITOR_INIT_CAPABILITIES, ROW_PAGE_LIMIT, WorkspaceFilesystem};
use super::{
    Expr, FsBuilder, NodeKind, ROOT_INODE, RowProjectionSource, StoredFieldShape,
    is_replacement_scratch_for, key_directory, parse_row, runtime_row_baseline_from_rows,
    runtime_table_key_bytes, select_runtime_row_fields, source_without_fields,
};
use orna_repository::TypedKey;
use orna_runtime::{RuntimeTableIdentity, RuntimeTableRows, TableObjectId, vfs::project_component};
use orna_value::{Raw, Value};
fn encode_fields(fields: Vec<Raw>) -> Vec<u8> {
    Value::new(Raw::Array(fields)).unwrap().encode().unwrap()
}

#[test]
fn renamed_tables_use_current_identity_while_rekeys_keep_the_pinned_boundary() {
    let id = [1; 16];
    let current_table = RuntimeTableIdentity::new("Renamed", TableObjectId::new(id)).unwrap();
    let pinned_table = RuntimeTableIdentity::new("OldName", TableObjectId::new(id)).unwrap();
    let layout = [StoredFieldShape {
        field_id: id,
        field_type: Raw::Text("Str".into()),
    }];
    let pinned = Raw::Array(vec![Raw::Text("pinned value".into())]);
    let current_bytes = encode_fields(vec![Raw::Text("current value".into())]);
    let renamed_rows: RuntimeTableRows = [(
        "Renamed".to_owned(),
        vec![(b"same-key".to_vec(), current_bytes.clone())],
    )]
    .into();

    let current_row =
        runtime_row_baseline_from_rows(&renamed_rows, &current_table, b"same-key").unwrap();
    assert_eq!(current_row, Some(current_bytes.as_slice()));
    let (projected, source) = select_runtime_row_fields(
        current_row,
        pinned.clone(),
        &layout,
        Some(&layout),
        [id].into_iter(),
    )
    .unwrap();
    assert_eq!(source, RowProjectionSource::RuntimeSnapshot);
    assert_eq!(
        projected,
        Raw::Array(vec![Raw::Text("current value".into())])
    );
    assert_eq!(
        runtime_row_baseline_from_rows(&renamed_rows, &pinned_table, b"same-key"),
        None
    );

    let rekeyed_rows: RuntimeTableRows = [(
        "Renamed".to_owned(),
        vec![(b"new-key".to_vec(), current_bytes)],
    )]
    .into();
    let current =
        runtime_row_baseline_from_rows(&rekeyed_rows, &current_table, b"old-key").unwrap();
    assert_eq!(current, None);
    let (projected, source) = select_runtime_row_fields(
        current,
        pinned.clone(),
        &layout,
        Some(&layout),
        [id].into_iter(),
    )
    .unwrap();
    assert_eq!(source, RowProjectionSource::PinnedSnapshot);
    assert_eq!(projected, pinned);
}

/// W08-08: a CWD row whose stored layout no longer matches the mounted
/// schema is reported, never replaced by the pinned HEAD row.
#[test]
fn equal_length_field_order_and_schema_changes_report_runtime_rows() {
    let pinned_layout = [
        StoredFieldShape {
            field_id: [1; 16],
            field_type: Raw::Text("Str".into()),
        },
        StoredFieldShape {
            field_id: [2; 16],
            field_type: Raw::Text("Int".into()),
        },
    ];
    let runtime_bytes = encode_fields(vec![Raw::Text("title".into()), Raw::Int(7.into())]);
    let pinned_fields = Raw::Array(vec![Raw::Text("old title".into()), Raw::Int(6.into())]);
    let reordered_layout = [pinned_layout[1].clone(), pinned_layout[0].clone()];
    let changed_schema_layout = [
        pinned_layout[0].clone(),
        StoredFieldShape {
            field_id: [2; 16],
            field_type: Raw::Text("Bool".into()),
        },
    ];

    for runtime_layout in [&reordered_layout[..], &changed_schema_layout[..]] {
        let projected = select_runtime_row_fields(
            Some(&runtime_bytes),
            pinned_fields.clone(),
            &pinned_layout,
            Some(runtime_layout),
            [[1; 16], [2; 16]].into_iter(),
        );
        assert!(
            projected.is_err(),
            "a CWD row in another layout must not project as HEAD: {projected:?}"
        );
    }
    // ORNA-VFS-001: a row is validated under the mounted snapshot's schema, so
    // with no runtime layout a valid CWD row still projects from the CWD.
    let unresolved_schema = select_runtime_row_fields(
        Some(&runtime_bytes),
        pinned_fields,
        &pinned_layout,
        None,
        [[1; 16], [2; 16]].into_iter(),
    );
    let (projected, source) =
        unresolved_schema.expect("a CWD row valid under the mounted schema projects");
    assert_eq!(source, RowProjectionSource::RuntimeSnapshot);
    assert_eq!(
        projected,
        Raw::Array(vec![Raw::Text("title".into()), Raw::Int(7.into())])
    );
}

/// W08-08: an undecodable CWD row is reported while the pinned HEAD row
/// exists; the projection never falls back to that HEAD row, whether or not
/// a runtime layout is resolved.
#[test]
fn undecodable_cwd_row_is_reported_when_a_head_row_exists() {
    let pinned_layout = [
        StoredFieldShape {
            field_id: [1; 16],
            field_type: Raw::Text("Str".into()),
        },
        StoredFieldShape {
            field_id: [2; 16],
            field_type: Raw::Text("Int".into()),
        },
    ];
    let pinned_fields = Raw::Array(vec![Raw::Text("old title".into()), Raw::Int(6.into())]);
    for runtime_layout in [None, Some(&pinned_layout[..])] {
        let projected = select_runtime_row_fields(
            Some(&[0xff]),
            pinned_fields.clone(),
            &pinned_layout,
            runtime_layout,
            [[1; 16], [2; 16]].into_iter(),
        );
        let error = projected.expect_err("an undecodable CWD row is reported");
        assert!(
            error.contains("cannot be decoded"),
            "an undecodable CWD row must not project HEAD: {error}"
        );
    }
}

#[test]
fn lazy_row_lookups_share_the_captured_runtime_generation() {
    let generation_one: RuntimeTableRows = [(
        "Music".to_owned(),
        vec![
            (
                b"one".to_vec(),
                encode_fields(vec![Raw::Text("one-v1".into())]),
            ),
            (
                b"two".to_vec(),
                encode_fields(vec![Raw::Text("two-v1".into())]),
            ),
        ],
    )]
    .into();
    let generation_two: RuntimeTableRows = [(
        "Music".to_owned(),
        vec![
            (
                b"one".to_vec(),
                encode_fields(vec![Raw::Text("one-v2".into())]),
            ),
            (
                b"two".to_vec(),
                encode_fields(vec![Raw::Text("two-v2".into())]),
            ),
        ],
    )]
    .into();
    let table = RuntimeTableIdentity::new("Music", TableObjectId::new([3; 16])).unwrap();
    let mount_snapshot = std::sync::OnceLock::new();
    assert!(mount_snapshot.set(generation_one).is_ok());

    let first = runtime_row_baseline_from_rows(mount_snapshot.get().unwrap(), &table, b"one")
        .unwrap()
        .unwrap();
    let second = runtime_row_baseline_from_rows(mount_snapshot.get().unwrap(), &table, b"two")
        .unwrap()
        .unwrap();
    let later_second = runtime_row_baseline_from_rows(&generation_two, &table, b"two")
        .unwrap()
        .unwrap();
    assert_eq!(
        Value::decode(first).unwrap().raw(),
        &Raw::Array(vec![Raw::Text("one-v1".into())]),
    );
    assert_eq!(
        Value::decode(second).unwrap().raw(),
        &Raw::Array(vec![Raw::Text("two-v1".into())]),
    );
    assert_eq!(
        Value::decode(later_second).unwrap().raw(),
        &Raw::Array(vec![Raw::Text("two-v2".into())]),
    );
}

#[test]
fn runtime_table_keys_use_persisted_text_bytes_and_typed_encoding_otherwise() {
    assert_eq!(
        runtime_table_key_bytes(&TypedKey::Text("london".into())).unwrap(),
        b"london"
    );

    let numeric = TypedKey::Int(7);
    assert_eq!(
        runtime_table_key_bytes(&numeric).unwrap(),
        numeric.canonical_bytes().unwrap()
    );
}
#[test]
fn source_row_blob_fields_are_omitted_for_owner_bindings() {
    let source = r#"{ title: "song", content: { path: "./content.mp3", media_type: "audio/mpeg" }, artist: "A", }"#;
    let without_content = source_without_fields(source, &["content".to_owned()]).unwrap();
    let parsed = parse_row(&without_content);
    assert!(parsed.diagnostics.is_empty());
    let Expr::Record { fields, .. } = parsed.value else {
        panic!("row remains a record");
    };
    assert_eq!(
        fields
            .iter()
            .map(|field| field.name.as_str())
            .collect::<Vec<_>>(),
        ["title", "artist"]
    );
}

#[test]
fn blob_replacement_scratch_names_stay_bound_to_their_target() {
    assert!(is_replacement_scratch_for(
        ".content.mp3.tmp",
        "content.mp3"
    ));
    assert!(is_replacement_scratch_for(
        ".content.mp3.12345",
        "content.mp3"
    ));
    assert!(!is_replacement_scratch_for("unrelated", "content.mp3"));
    assert!(!is_replacement_scratch_for(".content.mp3.", "content.mp3"));
}

#[test]
fn table_and_text_key_paths_use_runtime_components() {
    let mut builder = FsBuilder::new();
    let table = builder.table_directory("Music", "data.orna").unwrap();
    let row = key_directory(&mut builder, table, &TypedKey::Text(String::new())).unwrap();

    let NodeKind::Directory(root) = &builder.nodes.get(&ROOT_INODE).unwrap().kind else {
        panic!("root is a directory");
    };
    let module = root.entry(OsStr::new("Music")).unwrap();
    let NodeKind::Directory(tables) = &builder.nodes.get(&module.inode).unwrap().kind else {
        panic!("module path is a directory");
    };
    let table_entry = tables
        .entry(OsStr::new(&project_component("data.orna")))
        .unwrap();
    assert_eq!(table_entry.inode, table);
    let NodeKind::Directory(rows) = &builder.nodes.get(&table).unwrap().kind else {
        panic!("table path is a directory");
    };
    let row_entry = rows.entry(OsStr::new(&project_component(""))).unwrap();
    assert_eq!(row_entry.inode, row);
    assert!(matches!(
        builder.nodes.get(&row).unwrap().kind,
        NodeKind::KeyDirectory(_)
    ));
}

#[test]
fn composite_keys_use_one_projected_path_component_per_part() {
    let mut builder = FsBuilder::new();
    let table = builder.table_directory("Music", "Music").unwrap();
    let row = key_directory(
        &mut builder,
        table,
        &TypedKey::Tuple(vec![TypedKey::Text("live/in".into()), TypedKey::Int(7)]),
    )
    .unwrap();

    let NodeKind::Directory(table_entries) = &builder.nodes.get(&table).unwrap().kind else {
        panic!("table path is a directory");
    };
    let first = table_entries
        .entry(OsStr::new(&project_component("live/in")))
        .unwrap();
    let NodeKind::KeyDirectory(first_entries) = &builder.nodes.get(&first.inode).unwrap().kind
    else {
        panic!("first key component is a directory");
    };
    let second = first_entries
        .directory
        .entry(OsStr::new(&project_component("7")))
        .unwrap();
    assert_eq!(second.inode, row);
}
#[allow(clippy::too_many_lines)]
#[test]
fn editor_open_flags_bypass_kernel_cache_without_writeback_capability() {
    assert!(EDITOR_INIT_CAPABILITIES.contains(fuser::InitFlags::FUSE_ATOMIC_O_TRUNC));
    assert!(!EDITOR_INIT_CAPABILITIES.contains(fuser::InitFlags::FUSE_WRITEBACK_CACHE));
    assert_eq!(EDITABLE_OPEN_FLAGS, fuser::FopenFlags::FOPEN_DIRECT_IO);
}

/// Upper bound on one stored chunk, mirroring orna-repository's private
/// `blob_store::GEAR_MAXIMUM`. Windows of at most 64 KiB read at most two chunks.
const BLOB_CHUNK_MAXIMUM_BYTES: u64 = 1_048_576;

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn mount_blob_window_reads_charge_only_intersecting_chunks() {
    const PAYLOAD_BYTES: u64 = 4 << 20;
    const WINDOW_OFFSET: u64 = 2 << 20;
    const WINDOW_BYTES: u32 = 4096;
    let payload_len = usize::try_from(PAYLOAD_BYTES).unwrap();
    let mut xorshift = 0x9e37_79b9_7f4a_7c15_u64;
    let payload: Vec<u8> = (0..payload_len)
        .map(|_| {
            xorshift ^= xorshift << 13;
            xorshift ^= xorshift >> 7;
            xorshift ^= xorshift << 17;
            xorshift.to_le_bytes()[0]
        })
        .collect();
    let source = tempfile::TempDir::new().unwrap();
    std::fs::write(source.path().join("payload.bin"), &payload).unwrap();

    let (_repository_directory, repository, relation_id, format_context) =
        crate::format3::format3_repository_with_context(
            include_str!("../../tests/fixtures/media/empty-media-main.orna"),
            "media",
        );
    let (identity, digest) = crate::format3::cli_runtime_identity(&repository);
    let runtime = orna_runtime::RuntimeState::open(&repository, identity, digest)
        .await
        .unwrap();
    let writer = runtime.acquire_lease(identity.repository_id).await.unwrap();
    let capability = repository.capture_capability(relation_id).unwrap();
    let mut provider = orna_sys::FilesystemProvider::with_limits(payload_len, 16).unwrap();
    provider.allow_root(source.path()).unwrap();
    let mut bindings =
        orna_evaluator::SysHostBindingRegistry::new(orna_sys::EnvironmentProvider::default())
            .with_filesystem_provider(provider)
            .with_repository_capture_capability(capability);
    let root = format!("{:?}", source.path().to_string_lossy().as_ref());
    let capture = format!("sys.blob.capture_file({root}, \"payload.bin\", {PAYLOAD_BYTES})");
    crate::media_commit::commit_source_captures_for_table(
        &repository,
        &runtime,
        writer,
        &mut bindings,
        relation_id,
        &format_context,
        "media",
        &[crate::media_commit::SourceCaptureRow {
            key: "payload",
            blob_fields: &[("content", &capture)],
            source_row: "{ }\n",
            ordinal: 0,
            insert_only: true,
        }],
    )
    .await;
    drop(runtime);

    let context = repository.open_format_context_at_selector("HEAD").unwrap();
    let mount =
        WorkspaceFilesystem::from_snapshot(&repository, context.commit(), &context, None).unwrap();
    let (graph, reference) = {
        let mut builder = mount.state.lock().unwrap();
        let table = builder
            .directory(ROOT_INODE)
            .unwrap()
            .entry(OsStr::new("media"))
            .unwrap()
            .inode;
        let row = builder
            .lookup_child(table, OsStr::new(&project_component("payload")))
            .unwrap()
            .expect("the committed payload key is a row of the media table");
        let blob = builder
            .lookup_child(row, OsStr::new("content.bin"))
            .unwrap()
            .expect("the committed row projects its content.bin Blob");
        match &builder.nodes.get(&blob).unwrap().kind {
            NodeKind::Blob {
                graph, reference, ..
            } => (std::sync::Arc::clone(graph), reference.clone()),
            _ => panic!("the content field of a committed Blob row is a Blob node"),
        }
    };
    let length = reference.content_identity().length();
    assert_eq!(length, PAYLOAD_BYTES);

    // Every non-final chunk is at least GEAR_MINIMUM (64 KiB), so a window no
    // larger than that crosses at most one chunk boundary and reads at most two
    // chunks. The final chunk may be short; a window that ends at the Blob's end
    // still crosses at most one boundary.
    let cases = [
        ("interior", WINDOW_OFFSET, WINDOW_BYTES),
        (
            "mid-chunk near 1 MiB",
            BLOB_CHUNK_MAXIMUM_BYTES - 100,
            WINDOW_BYTES,
        ),
        ("end of blob", PAYLOAD_BYTES - 100, WINDOW_BYTES),
        ("at end of blob", PAYLOAD_BYTES, WINDOW_BYTES),
    ];
    for (label, offset, size) in cases {
        let scope = graph.open_read_scope().unwrap();
        let window = super::read_blob_window(&graph, &reference, offset, size, &scope).unwrap();
        let start = offset.min(length);
        let end = offset.saturating_add(u64::from(size)).min(length);
        let requested = end - start;
        let start_index = usize::try_from(start).unwrap();
        let end_index = usize::try_from(end).unwrap();
        assert_eq!(
            window.bytes(),
            &payload[start_index..end_index],
            "{label}: the window returned the wrong bytes"
        );
        let charged = scope.payload_bytes_read();
        let chunks = window.chunks_read();
        assert!(
            chunks <= 2,
            "{label}: a window of at most 64 KiB reads at most two chunks: read {chunks}"
        );
        assert!(
            charged >= requested,
            "{label}: the window's bytes are charged: {charged} < {requested}"
        );
        assert!(
            charged <= chunks * BLOB_CHUNK_MAXIMUM_BYTES,
            "{label}: charged {charged} exceeds the {chunks} chunks the window reads"
        );
        assert!(
            charged < length,
            "{label}: the read does not fetch the whole Blob: charged {charged} of {length}"
        );
        eprintln!(
            "Gate B: range mount window {label}: requested={requested} charged={charged} chunks={chunks} length={length}"
        );
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn workspace_readdir_uses_lazy_indexed_pages() {
    const SOURCE: &str = "pub table Music(id: Str) { title: Str, }\n";
    let fixture_rows = (0..ROW_PAGE_LIMIT)
        .map(|index| {
            let key = format!("row-{index:03}");
            let source = format!("{{ title: \"{key}\", }}\n");
            (key, source)
        })
        .collect::<Vec<_>>();
    let (_repository_directory, repository, relation_id, format_context) =
        crate::format3::format3_repository_with_context(SOURCE, "Music");
    let (identity, digest) = crate::format3::cli_runtime_identity(&repository);
    let state = orna_runtime::RuntimeState::open(&repository, identity, digest)
        .await
        .unwrap();
    let writer = state.acquire_lease(identity.repository_id).await.unwrap();
    let mut bindings =
        orna_evaluator::SysHostBindingRegistry::new(orna_sys::EnvironmentProvider::default());
    let captures = fixture_rows
        .iter()
        .enumerate()
        .map(
            |(index, (key, source))| crate::media_commit::SourceCaptureRow {
                key,
                blob_fields: &[],
                source_row: source,
                ordinal: u16::try_from(index).unwrap(),
                insert_only: true,
            },
        )
        .collect::<Vec<_>>();
    crate::media_commit::commit_source_captures_for_table(
        &repository,
        &state,
        writer,
        &mut bindings,
        relation_id,
        &format_context,
        "Music",
        &captures,
    )
    .await;
    drop(state);

    let context = repository.open_format_context_at_selector("HEAD").unwrap();
    let filesystem =
        WorkspaceFilesystem::from_snapshot(&repository, context.commit(), &context, None).unwrap();
    let mut builder = filesystem.state.lock().unwrap();
    let table_inode = builder
        .directory(ROOT_INODE)
        .unwrap()
        .entry(OsStr::new("Music"))
        .unwrap()
        .inode;

    let indexed_name = project_component("row-255");
    let indexed_inode = builder
        .lookup_child(table_inode, OsStr::new(&indexed_name))
        .unwrap()
        .expect("indexed row lookup finds a stored text key");
    let NodeKind::Table(table) = &builder.nodes.get(&table_inode).unwrap().kind else {
        panic!("projected Music path is a table directory");
    };
    assert!(
        table.directory.entries.is_empty(),
        "lookup does not materialize a readdir page"
    );
    assert!(
        table.lower.is_none(),
        "indexed lookup leaves the page cursor untouched"
    );
    assert_eq!(
        table.indexed_rows.len(),
        1,
        "the direct lookup is retained outside the ordered page"
    );

    let mut first_page = Vec::with_capacity(ROW_PAGE_LIMIT);
    builder
        .readdir_page(table_inode, 0, |_, entry| {
            first_page.push(entry.name.to_string_lossy().into_owned());
            true
        })
        .unwrap();
    assert_eq!(first_page.len(), ROW_PAGE_LIMIT);
    assert_eq!(first_page.first().map(String::as_str), Some("row-000"));
    assert_eq!(first_page.last().map(String::as_str), Some("row-255"));
    assert!(builder.nodes.contains_key(&indexed_inode));
    let NodeKind::Table(table) = &builder.nodes.get(&table_inode).unwrap().kind else {
        panic!("projected Music path is a table directory");
    };
    assert_eq!(table.directory.entries.len(), ROW_PAGE_LIMIT);
    assert!(
        table.indexed_rows.is_empty(),
        "enumeration absorbs the cached indexed row in key order"
    );
    assert!(
        !table.exhausted,
        "a full page does not prove that another page is absent"
    );

    let mut next_page = Vec::new();
    let resume = u64::try_from(ROW_PAGE_LIMIT + 2).expect("the page limit fits a cursor");
    builder
        .readdir_page(table_inode, resume, |_, entry| {
            next_page.push(entry.name.to_string_lossy().into_owned());
            true
        })
        .unwrap();
    assert!(
        next_page.is_empty(),
        "the exact page boundary does not duplicate the inclusive cursor"
    );
    let NodeKind::Table(table) = &builder.nodes.get(&table_inode).unwrap().kind else {
        panic!("projected Music path is a table directory");
    };
    assert!(
        table.exhausted,
        "the empty final page marks the directory exhausted"
    );
    assert_eq!(
        builder
            .lookup_child(table_inode, OsStr::new(&indexed_name))
            .unwrap(),
        Some(indexed_inode),
    );
}
