use super::spool::{CandidateSource, DRAFT_SPOOL_DIRECTORY};
use super::*;
use crate::{
    ProtectedContentTransferEvidence, RuntimeIdentity, RuntimeQueryError, RuntimeState,
    RuntimeTableIdentity, RuntimeTableRows, TableActivationCandidateValidator, TableMutation,
    TableObjectId, TableSchemaIdentity,
};
use orna_repository::{ProtectedContentTransfer, Repository};
use orna_stream::DiagnosticClass;
use orna_value::{Raw, SchemaDescriptor};
use sha2::{Digest, Sha256};
use std::fs;
use std::sync::{
    Mutex as TestMutex,
    atomic::{AtomicUsize, Ordering},
};
use std::{
    collections::BTreeMap,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};
use uuid::Uuid;

fn fixture_image() -> Arc<VfsFileSnapshot<u64>> {
    let contents = include_str!("../../tests/fixtures/publication-repository-main.orna");
    Arc::new(VfsFileSnapshot::new(
        SnapshotPin::capture(Arc::new(7)),
        Arc::<[u8]>::from(contents.as_bytes()),
    ))
}

fn test_git(path: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .unwrap()
            .success(),
        "git {args:?} succeeds"
    );
}

fn test_git_output(path: &Path, args: &[&str], input: Option<&[u8]>) -> Vec<u8> {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(path)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(input) = input {
        child.stdin.take().unwrap().write_all(input).unwrap();
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn test_git_object(path: &Path, kind: &str, bytes: &[u8]) -> String {
    String::from_utf8(test_git_output(
        path,
        &["hash-object", "-w", "-t", kind, "--stdin"],
        Some(bytes),
    ))
    .unwrap()
    .trim()
    .to_owned()
}

fn test_git_tree(path: &Path, entries: &str) -> String {
    String::from_utf8(test_git_output(path, &["mktree"], Some(entries.as_bytes())))
        .unwrap()
        .trim()
        .to_owned()
}

fn test_native_node(path: &Path, data: &[u8], dependencies: &[(&str, &str)]) -> String {
    let data_oid = test_git_object(path, "blob", data);
    let mut dependencies = dependencies.to_vec();
    dependencies.sort_by_key(|(oid, _)| *oid);
    let refs_oid = if dependencies.is_empty() {
        None
    } else {
        let refs = dependencies
            .iter()
            .map(|(oid, kind)| {
                let mode = if *kind == "tree" { "040000" } else { "100644" };
                format!("{mode} {kind} {oid}\t{oid}\n")
            })
            .collect::<String>();
        Some(test_git_tree(path, &refs))
    };
    let mut envelope = format!("100644 blob {data_oid}\tdata\n");
    if let Some(refs_oid) = refs_oid {
        envelope.push_str(&format!("040000 tree {refs_oid}\trefs\n"));
    }
    test_git_tree(path, &envelope)
}

fn append_fixture_cbor_head(output: &mut Vec<u8>, major: u8, value: usize) {
    let prefix = major << 5;
    match value {
        0..=23 => output.push(prefix | value as u8),
        24..=255 => output.extend_from_slice(&[prefix | 24, value as u8]),
        _ => panic!("fixture CBOR value is unexpectedly large"),
    }
}

fn append_fixture_cbor_bytes(output: &mut Vec<u8>, bytes: &[u8]) {
    append_fixture_cbor_head(output, 2, bytes.len());
    output.extend_from_slice(bytes);
}

fn fixture_oid_bytes(oid: &str) -> Vec<u8> {
    oid.as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

fn format3_blob_repository() -> (tempfile::TempDir, Repository, [u8; 16]) {
    let temp = tempfile::TempDir::new().unwrap();
    let root = temp.path();
    test_git(root, &["init", "--quiet"]);
    test_git(root, &["config", "user.email", "kieran@drewett.dev"]);
    test_git(root, &["config", "user.name", "kierandrewett"]);
    test_git(root, &["config", "commit.gpgsign", "false"]);

    let database_id = [0, 0, 0, 0, 0, 0, 0x40, 0, 0x80, 0, 0, 0, 0, 0, 0, 1];
    let relation_id = [0x43; 16];
    let database_uuid = "00000000-0000-4000-8000-000000000001";
    let database =
        format!("{{\n    repository_format: 3,\n    database_id: \"{database_uuid}\",\n}}\n");
    let source = b"capture transaction test schema";
    let uuid = |id: [u8; 16]| Raw::Tag(37, Box::new(Raw::Bytes(id.to_vec())));
    let field = |id: [u8; 16], name: &str, ty: Raw, role: u8| {
        Raw::Array(vec![
            uuid(id),
            Raw::Text(name.to_owned()),
            ty,
            Raw::Int(role.into()),
            Raw::Array(vec![Raw::Int(0.into())]),
        ])
    };
    let schema_bytes = SchemaDescriptor::new(Raw::Map(vec![
        (Raw::Int(0.into()), Raw::Int(1.into())),
        (Raw::Int(1.into()), uuid(relation_id)),
        (Raw::Int(2.into()), Raw::Array(vec![uuid([0x11; 16])])),
        (
            Raw::Int(3.into()),
            Raw::Array(vec![
                field(
                    [0x11; 16],
                    "id",
                    Raw::Array(vec![Raw::Int(0.into()), Raw::Text("Int".into())]),
                    0,
                ),
                field(
                    [0x22; 16],
                    "payload",
                    Raw::Array(vec![Raw::Int(0.into()), Raw::Text("Blob".into())]),
                    1,
                ),
            ]),
        ),
        (Raw::Int(4.into()), Raw::Array(Vec::new())),
    ]))
    .unwrap()
    .encode()
    .unwrap();
    std::fs::create_dir_all(root.join(".orna/store")).unwrap();
    std::fs::write(root.join(".orna/database.orna"), &database).unwrap();
    std::fs::write(root.join(".orna/store/data"), b"store marker").unwrap();
    std::fs::write(root.join("main.orna"), source).unwrap();
    test_git(root, &["add", "--all"]);
    test_git(root, &["commit", "--quiet", "-m", "format3 Blob fixture"]);

    // The chunk entry carries the chunk's SHA-256; the kind-6 node and the
    // rows domain carry the format-3 schema identity.
    let chunk_digest: [u8; 32] = Sha256::digest(&schema_bytes).into();
    let schema_digest: [u8; 32] = Sha256::new()
        .chain_update(b"orna.schema.v3\0")
        .chain_update(&schema_bytes)
        .finalize()
        .into();
    let chunk_oid = test_git_object(root, "blob", &schema_bytes);
    let mut byte_index = vec![0x85, 0x01, 0x04, 0x00];
    append_fixture_cbor_head(&mut byte_index, 0, schema_bytes.len());
    byte_index.extend_from_slice(&[0x81, 0x83]);
    append_fixture_cbor_head(&mut byte_index, 0, schema_bytes.len());
    append_fixture_cbor_bytes(&mut byte_index, &fixture_oid_bytes(&chunk_oid));
    append_fixture_cbor_bytes(&mut byte_index, &chunk_digest);
    let byte_index_oid = test_native_node(root, &byte_index, &[(chunk_oid.as_str(), "blob")]);

    let mut schema = vec![0x85, 0x01, 0x06];
    append_fixture_cbor_head(&mut schema, 0, schema_bytes.len());
    append_fixture_cbor_bytes(&mut schema, &schema_digest);
    append_fixture_cbor_bytes(&mut schema, &fixture_oid_bytes(&byte_index_oid));
    let schema_oid = test_native_node(root, &schema, &[(byte_index_oid.as_str(), "tree")]);

    let mut row_domain = vec![0x83, 0x64];
    row_domain.extend_from_slice(b"rows");
    append_fixture_cbor_bytes(&mut row_domain, &relation_id);
    append_fixture_cbor_bytes(&mut row_domain, &schema_digest);
    let mut empty_rows = vec![0x84, 0x01, 0x01];
    empty_rows.extend_from_slice(&row_domain);
    empty_rows.push(0x80);
    let row_root_oid = test_native_node(root, &empty_rows, &[]);

    let mut relation_domain = vec![0x82, 0x69];
    relation_domain.extend_from_slice(b"relations");
    append_fixture_cbor_bytes(&mut relation_domain, &database_id);
    let mut relation_map = vec![0x84, 0x01, 0x01];
    relation_map.extend_from_slice(&relation_domain);
    relation_map.extend_from_slice(&[0x81, 0x82]);
    append_fixture_cbor_bytes(&mut relation_map, &relation_id);
    relation_map.push(0x84);
    append_fixture_cbor_bytes(&mut relation_map, &fixture_oid_bytes(&schema_oid));
    append_fixture_cbor_bytes(&mut relation_map, &fixture_oid_bytes(&row_root_oid));
    relation_map.extend_from_slice(&[0xf6, 0x00]);
    let relation_map_oid = test_native_node(
        root,
        &relation_map,
        &[
            (schema_oid.as_str(), "tree"),
            (row_root_oid.as_str(), "tree"),
        ],
    );

    let mut store_root = vec![0x83, 0x01, 0x00];
    append_fixture_cbor_bytes(&mut store_root, &fixture_oid_bytes(&relation_map_oid));
    let store_root_oid =
        test_native_node(root, &store_root, &[(relation_map_oid.as_str(), "tree")]);
    let database_oid = test_git_object(root, "blob", database.as_bytes());
    let source_oid = test_git_object(root, "blob", source);
    let orna_tree = test_git_tree(
        root,
        &format!(
            "100644 blob {database_oid}\tdatabase.orna\n040000 tree {store_root_oid}\tstore\n"
        ),
    );
    let root_tree = test_git_tree(
        root,
        &format!("040000 tree {orna_tree}\t.orna\n100644 blob {source_oid}\tmain.orna\n"),
    );
    let parent = String::from_utf8(test_git_output(root, &["rev-parse", "HEAD"], None))
        .unwrap()
        .trim()
        .to_owned();
    let commit = String::from_utf8(test_git_output(
        root,
        &[
            "commit-tree",
            &root_tree,
            "-p",
            &parent,
            "-m",
            "format3 Blob graph fixture",
        ],
        None,
    ))
    .unwrap()
    .trim()
    .to_owned();
    let head_ref = String::from_utf8(test_git_output(root, &["symbolic-ref", "HEAD"], None))
        .unwrap()
        .trim()
        .to_owned();
    test_git(root, &["update-ref", &head_ref, &commit]);
    let repository = Repository::discover(root).unwrap();
    (temp, repository, relation_id)
}

fn rejected() -> SafeDiagnostic {
    SafeDiagnostic {
        code: DiagnosticCode::TableAssertionFalse,
        class: DiagnosticClass::Permanent,
    }
}

#[test]
fn unlink_refuses_row_document_and_required_blob_while_allowing_optional() {
    let row = VfsRowProjection::project(
        SnapshotPin::capture(Arc::new(17_u64)),
        &[
            VfsStoredField::document("title", "\"Live in London\""),
            VfsStoredField::content("content", "audio/mpeg", None, 99),
            VfsStoredField::content_optional("cover", "image/jpeg", None, 4096),
            VfsStoredField::null("booklet"),
        ],
    )
    .expect("row projection");

    let content = project_content_name("content", "audio/mpeg", None).unwrap();
    let cover = project_content_name("cover", "image/jpeg", None).unwrap();
    assert_eq!(content, "content.mp3");
    assert_eq!(cover, "cover.jpg");

    // Deleting `data.orna` is refused: row deletion is an explicit database
    // operation, never a VFS unlink side effect (VFS-013).
    let document = row.resolve_unlink(VFS_ROW_DOCUMENT).expect("document");
    assert_eq!(document, VfsUnlinkTarget::RowDocument);
    assert_eq!(document.refusal_errno(), Some(VFS_EPERM));
    assert_eq!(document.refusal_code(), Some("sys.vfs.unsupported"));

    // A required Blob cannot be removed, because schema validity depends on
    // its exact content descriptor (VFS-013).
    let required = row.resolve_unlink(&content).expect("required blob");
    let VfsUnlinkTarget::RequiredContent(file) = &required else {
        panic!("required blob must resolve to a required target");
    };
    assert!(file.is_required());
    assert_eq!(required.refusal_errno(), Some(VFS_EPERM));
    assert_eq!(required.refusal_code(), Some("sys.vfs.unsupported"));

    // An optional Blob may be unlinked to `null`, so the target authorizes
    // validation instead of refusing in the projection (VFS-013).
    let optional = row.resolve_unlink(&cover).expect("optional blob");
    let VfsUnlinkTarget::OptionalContent(file) = &optional else {
        panic!("optional blob must resolve to an optional target");
    };
    assert!(!file.is_required());
    assert_eq!(optional.refusal_errno(), None);
    assert_eq!(optional.refusal_code(), None);

    // Field classes come from the schema, not the host name: a name outside
    // the projection resolves to no target at all (VFS-004).
    assert_eq!(
        row.resolve_unlink("missing.mp3"),
        Err(VfsPathError::UnknownRow)
    );
    assert_eq!(
        row.resolve_unlink("~key-not-a-field.mp3"),
        Err(VfsPathError::UnknownRow)
    );
}

/// ORNA-VFS-012: suffix selection updates only compatible hints and never implies byte transcoding or row rekeying.
#[test]
fn suffix_selection_is_mime1_compatible_and_preferred_hint_is_absent() {
    // A compatible non-preferred spelling is stored as that explicit hint,
    // and the file name changes only in its suffix (MIME-1, VFS-012).
    assert_eq!(
        classify_content_suffix("audio/mpeg", Some("mp1")),
        Ok(Some("mp1".to_owned()))
    );
    assert_eq!(
        classify_content_suffix("audio/mpeg", Some("MP1")),
        Ok(Some("mp1".to_owned()))
    );
    assert_eq!(
        classify_content_suffix("image/jpeg", Some("jpeg")),
        Ok(Some("jpeg".to_owned()))
    );
    assert_eq!(
        classify_content_suffix("application/gzip", Some("tgz")),
        Ok(Some("tgz".to_owned()))
    );

    // The catalogued preferred suffix is recorded as an absent hint, never
    // as a redundant one (MIME-1 default_hint_canonicalization).
    assert_eq!(classify_content_suffix("audio/mpeg", Some("mp3")), Ok(None));
    assert_eq!(classify_content_suffix("audio/mpeg", None), Ok(None));

    // A suffix outside the MIME-1 entry is a malformed candidate rather
    // than a silent transcode or a rekeyed row (VFS-012).
    assert_eq!(
        classify_content_suffix("audio/mpeg", Some("m4a")),
        Err(VfsPathError::InvalidDocument)
    );
    assert_eq!(
        classify_content_suffix("image/jpeg", Some("png")),
        Err(VfsPathError::InvalidDocument)
    );
    // Octet-stream and unregistered essences admit any safe hint and store
    // `bin` as no hint (MIME-1 `unknown_policy` and the normative model's
    // `annotation()`), but never an unsafe spelling.
    assert_eq!(
        classify_content_suffix("application/x-unknown", Some("gz")),
        Ok(Some("gz".to_owned()))
    );
    assert_eq!(
        classify_content_suffix("application/x-unknown", Some("bin")),
        Ok(None)
    );
    assert_eq!(
        classify_content_suffix("application/octet-stream", Some("dat")),
        Ok(Some("dat".to_owned()))
    );
    for unsafe_suffix in ["", ".dat", "a..b", "a/b", "dat "] {
        assert_eq!(
            classify_content_suffix("application/octet-stream", Some(unsafe_suffix)),
            Err(VfsPathError::InvalidDocument),
            "{unsafe_suffix:?}"
        );
        assert!(!content_suffix_is_compatible(
            "application/x-unknown",
            unsafe_suffix
        ));
    }
    assert!(content_suffix_is_compatible("application/x-unknown", "xyz"));
    assert!(!content_suffix_is_compatible("image/jpeg", "png"));
    assert_eq!(
        project_content_name("content", "application/x-unknown", None),
        Ok("content.bin".to_owned())
    );
    assert_eq!(
        project_content_name("content", "application/x-unknown", Some("xyz")),
        Ok("content.xyz".to_owned())
    );
    // Projection reads the registry's selected suffix, so an alias spelling
    // projects as its canonical essence. A stored hint the registry refuses
    // is corrupt metadata: it is reported (EIO), never spelled into the host
    // name and never renamed to the octet-stream `bin` fallback (W08-09).
    assert_eq!(
        project_content_name("content", "Audio/X-WAV", None),
        Ok("content.wav".to_owned())
    );
    assert_eq!(
        project_content_name("content", "audio/mpeg", Some("../mp3")),
        Err(VfsPathError::CorruptAnnotation)
    );
    assert_eq!(VfsPathError::CorruptAnnotation.errno(), VFS_EIO);
    assert_eq!(VfsPathError::CorruptAnnotation.failure_code(), None);
    assert_eq!(
        VfsRowProjection::project(
            SnapshotPin::capture(Arc::new(7_u64)),
            &[VfsStoredField::content(
                "content",
                "audio/mpeg",
                Some("../mp3".to_owned()),
                4
            )],
        )
        .err(),
        Some(VfsPathError::CorruptAnnotation),
        "a row whose stored annotation is refused is not projected"
    );

    // The suffix rename never changes the stored content identity: the same
    // descriptor size projects under the selected hint, and the row's bytes
    // are untouched.
    let renamed = project_content_name("content", "audio/mpeg", Some("mp1")).unwrap();
    assert_eq!(renamed, "content.mp1");
    assert_eq!(
        project_content_name("content", "audio/mpeg", None),
        Ok("content.mp3".to_owned())
    );
}

#[test]
fn vfs_table_row_replacement_preserves_admitted_table_identity() {
    let table = RuntimeTableIdentity::new("Song", crate::TableObjectId::new([0x71; 16]))
        .expect("valid table identity");
    let replacement = VfsTableRowReplacement::new(
        [0x55; 16],
        table.clone(),
        vec![0x18, 0x2a],
        Some(vec![0xa1]),
    )
    .expect("valid replacement");

    assert_eq!(replacement.table(), &table);
    assert_eq!(replacement.key(), &[0x18, 0x2a]);
    assert_eq!(replacement.value(), Some(&[0xa1][..]));
    let mutation = replacement.mutation().expect("valid mutation");
    assert_eq!(mutation.table(), "Song");
    assert_eq!(mutation.table_object_id(), Some(table.object_id()));
    assert_eq!(mutation.key(), &[0x18, 0x2a]);
    assert_eq!(mutation.value(), Some(&[0xa1][..]));
}

#[test]
fn content_file_projection_uses_field_namespace_and_exact_entry_identity() {
    let long_field = "content_".repeat(40);
    let projected = project_content_name(&long_field, "audio/mpeg", None).unwrap();
    assert!(projected.starts_with("~field-"));
    assert!(projected.ends_with(".mp3"));

    let row = VfsRowProjection::project(
        SnapshotPin::capture(Arc::new(7_u64)),
        &[VfsStoredField::content(
            long_field.clone(),
            "audio/mpeg",
            None,
            99,
        )],
    )
    .expect("content field projection");
    assert_eq!(row.files()[0].name(), projected);
    let VfsProjectedEntry::Content(file) = row.lookup(&projected).expect("projected file") else {
        panic!("content lookup should return the projected file");
    };
    assert_eq!(file.descriptor_size(), 99);
    assert!(matches!(
        row.lookup("~key-not-a-field.mp3"),
        Err(VfsPathError::UnknownRow)
    ));
}

#[test]
fn nested_directory_takes_escaped_field_name_and_shares_the_file_collision_check() {
    let row = VfsRowProjection::project_with_nested_directories(
        SnapshotPin::capture(Arc::new(7_u64)),
        &[VfsStoredField::document("con", "{ x: 1 }")],
        &["con"],
    )
    .expect("nested directory projection");
    assert_eq!(row.directories().len(), 1);
    assert_eq!(row.directories()[0].field(), "con");
    assert_eq!(row.directories()[0].name(), "%63on");
    assert_eq!(row.directories()[0].name(), project_field_name("con"));

    // Two nested directories that name one host directory take the same
    // collision refusal two sibling files take (VFS-016).
    assert!(matches!(
        VfsRowProjection::project_with_nested_directories(
            SnapshotPin::capture(Arc::new(7_u64)),
            &[VfsStoredField::document("x", "{ y: 2 }")],
            &["x", "x"],
        ),
        Err(VfsPathError::NameCollision)
    ));
    // A directory name carries no dot, so it never meets a sibling file's
    // `.suffix` host name: a Text sibling `x` and a nested directory `y` coexist.
    let row = VfsRowProjection::project_with_nested_directories(
        SnapshotPin::capture(Arc::new(7_u64)),
        &[
            VfsStoredField::text("x", 3, true),
            VfsStoredField::document("y", "{ z: 3 }"),
        ],
        &["y"],
    )
    .expect("text sibling and nested directory coexist");
    assert_eq!(row.directories()[0].name(), "y");
}

#[test]
fn a_saved_row_reuses_the_captured_blob_descriptor_instead_of_re_deriving_it() {
    // A content sibling that a save admitted from the captured field keeps
    // the descriptor the store reported for the row version it came from:
    // the byte length, the raw digest, and the OGS-1 descriptor OID are the
    // captured ones, so a save that changes only the selected suffix (or
    // any other field) cannot mint a new descriptor or re-hash the payload
    // (VFS-008: an unchanged descriptor uses the captured field, not a new
    // path lookup into current state).
    let descriptor_oid = NativeObjectId::new("a".repeat(40)).expect("well-formed SHA-1 object id");
    let captured = CapturedContentDescriptor::new(4096, [0x5a; 32], descriptor_oid.clone());
    let row = VfsRowProjection::project(
        SnapshotPin::capture(Arc::new(7_u64)),
        &[VfsStoredField::captured_content(
            "content",
            "audio/mpeg",
            Some("mp1".to_owned()),
            captured.clone(),
            true,
        )],
    )
    .expect("captured content projection");

    let VfsProjectedEntry::Content(file) = row
        .lookup("content.mp1")
        .expect("the compatible suffix names the content file")
    else {
        panic!("content lookup should return the projected file");
    };

    let projected = file
        .captured_descriptor()
        .expect("the captured descriptor reaches the projected file");
    assert_eq!(projected.length(), 4096);
    assert_eq!(projected.sha256(), &[0x5a; 32]);
    assert_eq!(projected.descriptor_oid(), &descriptor_oid);
    assert_eq!(projected, &captured);
    assert_eq!(file.descriptor_size(), captured.length());
    assert!(file.is_required());

    // A projection that was given no captured descriptor reports none, so a
    // file can never claim an identity the store did not hand it.
    let plain = VfsRowProjection::project(
        SnapshotPin::capture(Arc::new(7_u64)),
        &[VfsStoredField::content("content", "audio/mpeg", None, 4096)],
    )
    .expect("plain content projection");
    let VfsProjectedEntry::Content(file) = plain.lookup("content.mp3").expect("content file")
    else {
        panic!("content lookup should return the projected file");
    };
    assert!(file.captured_descriptor().is_none());

    // The descriptor is admitted only for the payload the store captured:
    // a save that keeps those exact bytes rebuilds the field from it, while
    // a save that replaced the payload reports no admission, so the new
    // bytes mint their own descriptor instead of inheriting one that no
    // longer names them (VFS-008, VFS-020).
    let VfsProjectedEntry::Content(content) = row
        .lookup("content.mp1")
        .expect("the compatible suffix names the content file")
    else {
        panic!("content lookup should return the projected file");
    };
    assert_eq!(content.admitted_capture(4096, &[0x5a; 32]), Some(&captured));
    assert!(content.admitted_capture(4096, &[0x5b; 32]).is_none());
    assert!(content.admitted_capture(4095, &[0x5a; 32]).is_none());
}

#[test]
fn content_file_name_stays_within_the_component_bound_with_its_suffix() {
    // The projected content file is one host component, so VFS-016's bound
    // covers the whole `field.suffix` spelling. A field base that leaves no
    // room for its suffix takes the `~field-` digest alias instead of
    // overflowing the bound (VFS-016).
    let field = "f".repeat(200);
    let name = project_content_name(&field, "audio/mpeg", None).unwrap();
    assert!(name.len() <= VFS_MAX_COMPONENT_BYTES);
    assert!(name.starts_with("~field-"));
    assert!(name.ends_with(".mp3"));
    // The alias digests the stored name's canonical OVB-1 Str encoding (a
    // 200-byte Str is the CBOR text head `78 c8` then its bytes), so the
    // snapshot-bound long-name index re-derives and verifies the full name
    // rather than trusting the truncated spelling (VFS-1, VFS-016).
    let encoding = [&[0x78, 0xc8][..], field.as_bytes()].concat();
    let digest = Sha256::digest(&encoding)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    assert_eq!(name, format!("~field-{digest}.mp3"));
    assert_eq!(field_name_digest(&field)[..], Sha256::digest(&encoding)[..]);

    // A base with room for the suffix still spells the escaped name
    // directly; only the over-bound spelling changes.
    let field = "f".repeat(196);
    let name = project_content_name(&field, "audio/mpeg", None).unwrap();
    assert!(name.len() <= VFS_MAX_COMPONENT_BYTES);
    assert!(!name.starts_with("~field-"));
    assert!(name.ends_with(".mp3"));

    // A long field base and the row document stay distinct names in one row
    // directory (VFS-016, VFS-017).
    let row = VfsRowProjection::project(
        SnapshotPin::capture(Arc::new(23_u64)),
        &[
            VfsStoredField::document("title", "\"Live in London\""),
            VfsStoredField::content("f".repeat(200), "audio/mpeg", None, 99),
        ],
    )
    .expect("row projection");
    let long = row.files()[0].name().to_owned();
    assert!(long.len() <= VFS_MAX_COMPONENT_BYTES);
    assert_ne!(long, VFS_ROW_DOCUMENT);
    assert!(matches!(
        row.lookup(&long),
        Ok(VfsProjectedEntry::Content(_))
    ));
    assert!(matches!(
        row.lookup(VFS_ROW_DOCUMENT),
        Ok(VfsProjectedEntry::Document)
    ));
}

fn vfs_row_image() -> Arc<VfsFileSnapshot<u64>> {
    let contents = include_str!("../../tests/fixtures/vfs-row-music-london.orna");
    Arc::new(VfsFileSnapshot::new(
        SnapshotPin::capture(Arc::new(17)),
        Arc::<[u8]>::from(contents.as_bytes()),
    ))
}

struct ValidatedRowStore {
    accepted: TestMutex<Vec<u8>>,
    expected: Vec<u8>,
    transactions: AtomicUsize,
}

impl ValidatedRowStore {
    fn new(expected: &[u8]) -> Self {
        Self {
            accepted: TestMutex::new(expected.to_vec()),
            expected: expected.to_vec(),
            transactions: AtomicUsize::new(0),
        }
    }

    fn accepted(&self) -> Vec<u8> {
        self.accepted
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn transaction_count(&self) -> usize {
        self.transactions.load(Ordering::SeqCst)
    }

    async fn activate(
        self: Arc<Self>,
        candidate: ActivationCandidate<u64>,
        accepted_snapshot: u64,
    ) -> Result<ActivationDecision<u64>, ()> {
        self.transactions.fetch_add(1, Ordering::SeqCst);
        if candidate.replacement_bytes() == self.expected.as_slice() {
            *self
                .accepted
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                candidate.replacement_bytes().to_vec();
            Ok(ActivationDecision::Accepted(SnapshotPin::capture(
                Arc::new(accepted_snapshot),
            )))
        } else {
            Ok(ActivationDecision::Rejected(rejected()))
        }
    }
}

#[tokio::test]
async fn direct_draft_commit_publishes_through_destination_and_rejects_stale_drafts() {
    let repository = VfsRepositoryCache::new();
    let target = repository.managed_file(fixture_image()).await.unwrap();
    let baseline = include_str!("../../tests/fixtures/publication-repository-main.orna");
    let accepted_bytes = include_str!("../../tests/fixtures/checkpoint_snapshot_v1.orna");
    let rejected_bytes = include_str!("../../tests/fixtures/case_closure_edge_tail_sixty.orna");
    let old_handle = target.open_read().await;
    assert!(old_handle.projection_is_current().await.unwrap());

    let draft = target.open_draft(1 << 20).await;
    let stale_draft = target.open_draft(1 << 20).await;

    draft.truncate(0).await.unwrap();
    draft.write_at(0, accepted_bytes.as_bytes()).await.unwrap();
    let accepted = target
        .commit_draft_with(&draft, |candidate| async move {
            assert_eq!(candidate.replacement_bytes(), accepted_bytes.as_bytes());
            Ok::<_, ()>(ActivationDecision::Accepted(SnapshotPin::capture(
                Arc::new(8),
            )))
        })
        .await
        .unwrap();
    let TemporaryRenameOutcome::Applied { generation, handle } = accepted else {
        panic!("validated direct draft commit should be accepted");
    };
    assert_eq!(generation, 1);
    assert!(!old_handle.projection_is_current().await.unwrap());
    assert!(handle.projection_is_current().await.unwrap());
    assert_eq!(
        target
            .open_read()
            .await
            .read_at(0, accepted_bytes.len())
            .unwrap(),
        accepted_bytes.as_bytes()
    );

    // A draft opened before the accepted commit cannot publish over it, so
    // the activation boundary is never reached.
    let stale_calls = Arc::new(AtomicUsize::new(0));
    let stale_calls_by_route = Arc::clone(&stale_calls);
    stale_draft.truncate(0).await.unwrap();
    stale_draft
        .write_at(0, rejected_bytes.as_bytes())
        .await
        .unwrap();
    let stale = target
        .commit_draft_with(&stale_draft, move |_| async move {
            stale_calls_by_route.fetch_add(1, Ordering::SeqCst);
            Ok::<_, ()>(ActivationDecision::Rejected(rejected()))
        })
        .await
        .unwrap();
    assert!(matches!(stale, TemporaryRenameOutcome::Stale { .. }));
    assert_eq!(stale_calls.load(Ordering::SeqCst), 0);

    // A rejection retains the candidate and leaves the destination, the
    // generation, and the earlier handle's bytes alone.
    draft.truncate(0).await.unwrap();
    draft.write_at(0, rejected_bytes.as_bytes()).await.unwrap();
    let rejected_commit = target
        .commit_draft_with(&draft, |_| async {
            Ok::<_, ()>(ActivationDecision::Rejected(rejected()))
        })
        .await
        .unwrap();
    assert!(matches!(
        rejected_commit,
        TemporaryRenameOutcome::Rejected { .. }
    ));
    assert_eq!(
        target
            .open_read()
            .await
            .read_at(0, accepted_bytes.len())
            .unwrap(),
        accepted_bytes.as_bytes()
    );
    assert_eq!(
        draft
            .retained_invalid_draft()
            .await
            .unwrap()
            .replacement_bytes()
            .unwrap(),
        rejected_bytes.as_bytes()
    );
    assert_ne!(baseline.as_bytes(), accepted_bytes.as_bytes());
}

#[tokio::test]
async fn unlink_hides_new_opens_and_keeps_earlier_handle_bytes() {
    let repository = VfsRepositoryCache::new();
    let target = repository.managed_file(fixture_image()).await.unwrap();
    let accepted_bytes = include_str!("../../tests/fixtures/checkpoint_snapshot_v1.orna");

    let draft = target.open_draft(1 << 20).await;
    draft.truncate(0).await.unwrap();
    draft.write_at(0, accepted_bytes.as_bytes()).await.unwrap();
    let seeded = target
        .commit_draft_with(&draft, |_| async {
            Ok::<_, ()>(ActivationDecision::Accepted(SnapshotPin::capture(
                Arc::new(8),
            )))
        })
        .await
        .unwrap();
    assert!(matches!(seeded, TemporaryRenameOutcome::Applied { .. }));
    let live = target.open_current().await.expect("linked before unlink");

    let refused = target
        .unlink_with(|_| async { Ok::<_, ()>(ActivationDecision::Rejected(rejected())) })
        .await
        .unwrap();
    assert!(matches!(refused, UnlinkOutcome::Rejected { .. }));
    assert_eq!(repository.generation().await, 1);
    assert!(target.open_current().await.is_some());

    let unlinked = target
        .unlink_with(|candidate| async move {
            assert!(candidate.is_removal());
            Ok::<_, ()>(ActivationDecision::Accepted(SnapshotPin::capture(
                Arc::new(9),
            )))
        })
        .await
        .unwrap();
    assert_eq!(unlinked, UnlinkOutcome::Applied { generation: 2 });
    assert_eq!(repository.generation().await, 2);
    assert!(target.open_current().await.is_none());
    assert!(!live.projection_is_current().await.unwrap());
    assert_eq!(
        live.read_at(0, accepted_bytes.len()).unwrap(),
        accepted_bytes.as_bytes()
    );

    // Replacements against the removed destination are refused before any
    // activation, and a second unlink is a no-op rather than another one.
    let late_calls = Arc::new(AtomicUsize::new(0));
    let late_calls_by_route = Arc::clone(&late_calls);
    let late = target.open_draft(1 << 20).await;
    late.truncate(0).await.unwrap();
    late.write_at(0, accepted_bytes.as_bytes()).await.unwrap();
    let stale = target
        .commit_draft_with(&late, move |_| async move {
            late_calls_by_route.fetch_add(1, Ordering::SeqCst);
            Ok::<_, ()>(ActivationDecision::Rejected(rejected()))
        })
        .await
        .unwrap();
    assert!(matches!(stale, TemporaryRenameOutcome::Stale { .. }));
    assert_eq!(late_calls.load(Ordering::SeqCst), 0);
    let again = target
        .unlink_with(|_| async { Ok::<_, ()>(ActivationDecision::Rejected(rejected())) })
        .await
        .unwrap();
    assert_eq!(again, UnlinkOutcome::AlreadyUnlinked);
    assert_eq!(repository.generation().await, 2);
}

#[tokio::test]
async fn open_read_keeps_its_captured_image_and_directory_pin() {
    let image = fixture_image();
    let handle = SnapshotReadHandle::open(Arc::clone(&image));
    let cursor = SnapshotReaddirCursor::new(image.pin().clone_pin(), 23_u64);
    let original = handle.read_at(0, handle.len() as usize).unwrap();

    let draft = EditDraft::open(image, 1 << 20, DraftStorage::with_default_quota());
    draft.write_at(0, &[0x58]).await.unwrap();

    assert_eq!(handle.read_at(0, original.len()).unwrap(), original);
    assert_eq!(*cursor.pin().snapshot(), 7);
    assert_ne!(
        draft.candidate_bytes().await.unwrap().as_ref(),
        original.as_slice()
    );
}

/// A 256 MiB sparse write allocates only the blocks it touches. This reads
/// the spool file's own block count, not process RSS, so it cannot flake
/// on other tests in the binary.
#[tokio::test]
async fn sparse_write_near_draft_bound_stays_sparse_on_disk() {
    use std::os::unix::fs::{FileExt as _, MetadataExt as _};
    const BOUND: usize = 256 << 20;
    let quota = Arc::new(DraftQuota::new(1 << 30));
    let storage = DraftStorage::new(&std::env::temp_dir(), quota);
    let draft = EditDraft::open(fixture_image(), BOUND, storage);
    draft.write_at(BOUND as u64 - 1, &[0x58]).await.unwrap();
    let state = draft.state.lock().await;
    let spool = state.candidate.as_ref().expect("spool");
    assert_eq!(spool.len, BOUND as u64);
    let allocated = spool.file.metadata().unwrap().blocks() * 512;
    assert!(
        allocated < 1 << 20,
        "a sparse 256 MiB candidate allocated {allocated} bytes on disk"
    );
    let mut last = [0u8; 1];
    spool
        .file
        .read_exact_at(&mut last, BOUND as u64 - 1)
        .unwrap();
    assert_eq!(last, [0x58]);
}

#[tokio::test]
async fn aggregate_draft_quota_returns_edquot() {
    const LIMIT: usize = 1 << 20;
    let quota = Arc::new(DraftQuota::new(LIMIT as u64));
    let storage = DraftStorage::new(&std::env::temp_dir(), Arc::clone(&quota));
    let first = EditDraft::open(fixture_image(), LIMIT, storage.clone());
    let second = EditDraft::open(fixture_image(), LIMIT, storage);
    first.write_at(LIMIT as u64 - 1, &[0x58]).await.unwrap();
    assert_eq!(
        second.write_at(0, &[0x59]).await,
        Err(EditDraftError::QuotaExceeded)
    );
    first.truncate(0).await.unwrap();
    second.write_at(0, &[0x59]).await.unwrap();
    assert_eq!(second.candidate_bytes().await.unwrap()[0], 0x59);
}

/// Growth is what the quota refuses. The baseline is charged on the first
/// write; a growth larger than the remaining room returns EDQUOT and
/// changes nothing: no content, no revision, no charge.
#[tokio::test]
async fn growth_refused_by_quota_changes_nothing() {
    let baseline_len = fixture_image().bytes.len();
    let quota = Arc::new(DraftQuota::new(baseline_len + 16));
    let storage = DraftStorage::new(&std::env::temp_dir(), Arc::clone(&quota));
    let draft = EditDraft::open(fixture_image(), 1 << 20, storage);
    draft.write_at(0, &[0x58]).await.unwrap();
    assert_eq!(quota.used.load(Ordering::Acquire), baseline_len);

    let revision = draft.state.lock().await.revision;
    let before = draft.candidate_bytes().await.unwrap();
    assert_eq!(
        draft.write_at(baseline_len, &[0x59; 32]).await,
        Err(EditDraftError::QuotaExceeded)
    );
    assert_eq!(draft.state.lock().await.revision, revision);
    assert_eq!(draft.candidate_bytes().await.unwrap(), before);
    assert_eq!(quota.used.load(Ordering::Acquire), baseline_len);

    // The room that remains is usable: a growth that fits is accepted.
    draft.write_at(baseline_len, &[0x59; 16]).await.unwrap();
    assert_eq!(quota.used.load(Ordering::Acquire), baseline_len + 16);
}

/// A refused candidate is charged once, by the live spool that already holds
/// it. Its retained copy adds no charge, so the disk record of the same
/// candidate (its header plus its bytes, one reservation) still fits beside
/// the spool at a quota of exactly `2 * candidate + header`.
#[tokio::test]
async fn refused_candidate_is_charged_once_and_leaves_room_for_its_disk_record() {
    const HEADER: u64 = 64;
    let len = fixture_image().bytes.len().max(32 << 10);
    let quota = Arc::new(DraftQuota::new(2 * len + HEADER));
    let storage = DraftStorage::new(&std::env::temp_dir(), Arc::clone(&quota));
    let draft = EditDraft::open(fixture_image(), 1 << 20, storage);
    draft.write_at(0, &[0x58; 32 << 10]).await.unwrap();
    assert_eq!(draft.candidate_len().await, len);
    assert_eq!(quota.used.load(Ordering::Acquire), len);

    let mut state = draft.state.lock().await;
    let revision = state.revision;
    state
        .retain_rejection(revision, stale_baseline_diagnostic())
        .unwrap();
    drop(state);
    assert_eq!(quota.used.load(Ordering::Acquire), len);
    let disk = QuotaCharge::reserve(&quota, HEADER + len);
    assert!(
        disk.is_ok(),
        "the disk record of the refused candidate must fit"
    );
}

/// Several drafts edited at once stay within the 1 GiB default limit when
/// each grows to a 64 MiB compact file: 8 * 64 MiB = 512 MiB.
#[tokio::test]
async fn several_drafts_edit_within_the_default_limit() {
    const BOUND: usize = 64 << 20;
    let quota = Arc::new(DraftQuota::new(1 << 30));
    let storage = DraftStorage::new(&std::env::temp_dir(), Arc::clone(&quota));
    let mut drafts = Vec::new();
    for index in 0..8u8 {
        let draft = EditDraft::open(fixture_image(), BOUND, storage.clone());
        draft
            .write_at(BOUND as u64 - 1, &[index + 1])
            .await
            .unwrap();
        drafts.push(draft);
    }
    assert_eq!(quota.used.load(Ordering::Acquire), 8 * BOUND as u64);
    drop(drafts);
    assert_eq!(quota.used.load(Ordering::Acquire), 0);
}

/// A retained rejected draft is quota accounted (profiles/vfs-1.md): its
/// copy aliases the live spool, so the spool's single charge is carried
/// over while held and released when the draft drops.
#[tokio::test]
async fn retained_rejected_draft_is_charged_and_released() {
    let quota = Arc::new(DraftQuota::new(1 << 20));
    let storage = DraftStorage::new(&std::env::temp_dir(), Arc::clone(&quota));
    let draft = EditDraft::open(fixture_image(), 1 << 20, storage);
    draft.write_at(0, &[0x58]).await.unwrap();
    let revision = draft.state.lock().await.revision;
    let used_before = quota.used.load(Ordering::Acquire);
    draft
        .state
        .lock()
        .await
        .retain_rejection(revision, rejected())
        .unwrap();
    assert_eq!(quota.used.load(Ordering::Acquire), used_before);
    drop(draft);
    assert_eq!(quota.used.load(Ordering::Acquire), 0);
}

/// An over-release cannot wrap the aggregate budget: the charge floors at
/// zero, so the budget still refuses bytes beyond its limit.
#[test]
fn draft_quota_release_saturates_at_zero() {
    let quota = DraftQuota::new(100);
    quota.reserve(40).unwrap();
    quota.release(60);
    assert_eq!(quota.used.load(Ordering::Acquire), 0);
    quota.release(1);
    assert_eq!(quota.used.load(Ordering::Acquire), 0);
    assert!(quota.reserve(101).is_err());
    quota.reserve(100).unwrap();
    assert!(quota.reserve(1).is_err());
}

#[tokio::test]
async fn spool_creation_fails_closed_when_the_spool_directory_is_missing() {
    let directory =
        std::env::temp_dir().join(format!("orna-missing-spool-{}", Uuid::new_v4().simple()));
    let storage = DraftStorage::new(&directory, Arc::new(DraftQuota::new(1 << 20)));
    let draft = EditDraft::open(fixture_image(), 1 << 20, storage);
    assert_eq!(
        draft.write_at(0, &[0x58]).await,
        Err(EditDraftError::BackingIo)
    );
}

#[test]
fn runtime_root_draft_spool_directory_is_private() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = std::env::temp_dir().join(format!("orna-spool-root-{}", Uuid::new_v4().simple()));
    fs::create_dir_all(&root).unwrap();
    let _cache = VfsRepositoryCache::in_runtime_root(&root).unwrap();
    let mode = fs::metadata(root.join(DRAFT_SPOOL_DIRECTORY))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700);
    fs::remove_dir_all(&root).unwrap();
}

#[tokio::test]
async fn stat_reports_accepted_size_pin_and_generation_and_hides_unlinked() {
    let image = fixture_image();
    let expected_len = image.len();
    let managed = ManagedFile {
        state: Mutex::new(ManagedFileState::Live { image }),
        operation: Mutex::new(()),
        cache_epoch: SharedCacheEpoch::default(),
        draft_storage: DraftStorage::with_default_quota(),
    };

    let stat = managed.stat().await.unwrap();
    assert_eq!(stat.size(), expected_len);
    assert_eq!(*stat.pin().snapshot(), 7);
    assert_eq!(stat.generation(), 0);

    managed.state.lock().await.mark_unlinked();
    assert!(managed.stat().await.is_none());
}

#[test]
fn stat_json_renders_size_and_generation_without_pin() {
    let stat = ManagedFileStat {
        size: 5,
        pin: SnapshotPin::capture(Arc::new(7_u64)),
        generation: 3,
    };
    assert_eq!(stat.to_json(), r#"{"size":5,"generation":3}"#);
}

#[test]
fn stat_human_size_switches_units_at_kib_and_mib_boundaries() {
    let sized = |size: u64| ManagedFileStat {
        size,
        pin: SnapshotPin::capture(Arc::new(7_u64)),
        generation: 0,
    };
    assert_eq!(sized(0).human_size(), "0 B");
    assert_eq!(sized(1023).human_size(), "1023 B");
    assert_eq!(sized(1024).human_size(), "1.0 KiB");
    assert_eq!(sized(1536).human_size(), "1.5 KiB");
    assert_eq!(sized(1024 * 1024 - 1).human_size(), "1024.0 KiB");
    assert_eq!(sized(1024 * 1024).human_size(), "1.0 MiB");
    assert_eq!(sized(3 * 1024 * 1024 + 512 * 1024).human_size(), "3.5 MiB");
}

/// ORNA-VFS-008 and ORNA-VFS-009: rejected draft sequences are serialized, retained with their diagnostic, and repeated sync does not replay activation.
#[tokio::test]
async fn rejected_draft_is_retained_and_repeated_fsync_is_idempotent() {
    let draft = EditDraft::open(fixture_image(), 1 << 20, DraftStorage::with_default_quota());
    draft.write_at(0, &[0x58]).await.unwrap();
    let calls = AtomicUsize::new(0);
    let first = draft
        .fsync_with(|candidate| {
            calls.fetch_add(1, Ordering::SeqCst);
            async move {
                assert!(!candidate.replacement_bytes().is_empty());
                Ok::<_, ()>(ActivationDecision::Rejected(rejected()))
            }
        })
        .await
        .unwrap();
    assert!(matches!(first, FsyncOutcome::Rejected(_)));

    let retained = draft.retained_invalid_draft().await.unwrap();
    assert_eq!(retained.diagnostic(), rejected());
    assert_ne!(
        retained.replacement_bytes().unwrap(),
        &fixture_image().bytes.to_arc().unwrap()[..]
    );

    let second = draft
        .fsync_with(|_| {
            calls.fetch_add(1, Ordering::SeqCst);
            async {
                Ok::<_, ()>(ActivationDecision::Accepted(SnapshotPin::capture(
                    Arc::new(8),
                )))
            }
        })
        .await
        .unwrap();
    assert!(matches!(second, FsyncOutcome::Rejected(_)));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// ORNA-VFS-007 and ORNA-VFS-008: accepted fsync publishes before release, preserves old open handles, and repeat sync does not accept the same sequence twice.
#[tokio::test]
async fn accepted_fsync_advances_only_future_opens_and_release_never_commits() {
    let image = fixture_image();
    let old_handle = SnapshotReadHandle::open(Arc::clone(&image));
    let draft = EditDraft::open(image, 1 << 20, DraftStorage::with_default_quota());
    draft.write_at(0, &[0x58]).await.unwrap();
    let accepted = draft
        .fsync_with(|candidate| async move {
            assert_eq!(*candidate.baseline().snapshot(), 7);
            Ok::<_, ()>(ActivationDecision::Accepted(SnapshotPin::capture(
                Arc::new(8),
            )))
        })
        .await
        .unwrap();
    let FsyncOutcome::Accepted(new_handle) = accepted else {
        panic!("activation should accept the candidate");
    };

    assert_eq!(*new_handle.pin().snapshot(), 8);
    assert_eq!(*old_handle.pin().snapshot(), 7);
    assert_ne!(
        old_handle.read_at(0, 1).unwrap(),
        new_handle.read_at(0, 1).unwrap()
    );
    let repeated_calls = AtomicUsize::new(0);
    let repeated = draft
        .fsync_with(|_| {
            repeated_calls.fetch_add(1, Ordering::SeqCst);
            async {
                Ok::<_, ()>(ActivationDecision::Accepted(SnapshotPin::capture(
                    Arc::new(9),
                )))
            }
        })
        .await
        .unwrap();
    let FsyncOutcome::Accepted(repeated_handle) = repeated else {
        panic!("repeated sync must return its accepted outcome");
    };
    assert_eq!(repeated_calls.load(Ordering::SeqCst), 0);
    assert_eq!(*repeated_handle.pin().snapshot(), 8);
    assert_eq!(repeated_handle.read_at(0, 1).unwrap(), [0x58]);

    let calls = AtomicUsize::new(0);
    let draft = EditDraft::open(fixture_image(), 1 << 20, DraftStorage::with_default_quota());
    draft.write_at(1, &[0x59]).await.unwrap();
    draft.release();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn vfs_editor_backup_temp_and_truncate_patterns_share_one_validated_transaction() {
    let repository = VfsRepositoryCache::new();
    let target = repository.managed_file(vfs_row_image()).await.unwrap();
    let row_text = include_str!("../../tests/fixtures/vfs-row-music-london.orna").as_bytes();
    let row_store = Arc::new(ValidatedRowStore::new(row_text));

    // A backup file is private scratch seeded with the accepted row bytes.
    // Fsyncing it persists only that scratch and cannot admit a row.
    let backup = target.begin_temporary_backup(1 << 20).await;
    assert_eq!(backup.candidate_bytes().await.unwrap().as_ref(), row_text);
    let backup_syncs = Arc::new(AtomicUsize::new(0));
    let backup_syncs_by_route = Arc::clone(&backup_syncs);
    let backup_revision = backup
        .fsync_scratch_with(move |revision, bytes| {
            backup_syncs_by_route.fetch_add(1, Ordering::SeqCst);
            async move {
                assert_eq!(revision, 0);
                assert_eq!(bytes.read_range(0, row_text.len()).unwrap(), row_text);
                Ok::<_, ()>(())
            }
        })
        .await
        .unwrap();
    assert_eq!(backup_revision, 0);
    let backup_syncs_by_route = Arc::clone(&backup_syncs);
    let repeated_backup_revision = backup
        .fsync_scratch_with(move |_, _| {
            backup_syncs_by_route.fetch_add(1, Ordering::SeqCst);
            async { Ok::<_, ()>(()) }
        })
        .await
        .unwrap();
    assert_eq!(repeated_backup_revision, 0);
    assert_eq!(backup_syncs.load(Ordering::SeqCst), 1);
    assert_eq!(row_store.transaction_count(), 0);
    assert_eq!(repository.generation().await, 0);

    // A temp-file editor may fsync scratch first; only rename over the
    // managed target crosses the validated row-store transaction boundary.
    let save = target.begin_temporary_replacement(1 << 20).await;
    save.write_at(0, row_text).await.unwrap();
    let temp_syncs = Arc::new(AtomicUsize::new(0));
    let temp_syncs_by_route = Arc::clone(&temp_syncs);
    let temp_revision = save
        .fsync_scratch_with(move |revision, bytes| {
            temp_syncs_by_route.fetch_add(1, Ordering::SeqCst);
            async move {
                assert_eq!(revision, 2);
                assert_eq!(bytes.read_range(0, row_text.len()).unwrap(), row_text);
                Ok::<_, ()>(())
            }
        })
        .await
        .unwrap();
    assert_eq!(temp_revision, 2);
    let temp_syncs_by_route = Arc::clone(&temp_syncs);
    let repeated_temp_revision = save
        .fsync_scratch_with(move |_, _| {
            temp_syncs_by_route.fetch_add(1, Ordering::SeqCst);
            async { Ok::<_, ()>(()) }
        })
        .await
        .unwrap();
    assert_eq!(repeated_temp_revision, 2);
    assert_eq!(temp_syncs.load(Ordering::SeqCst), 1);
    assert_eq!(row_store.transaction_count(), 0);

    let row_store_for_rename = Arc::clone(&row_store);
    let applied = target
        .rename_over(&save, move |candidate| {
            Arc::clone(&row_store_for_rename).activate(candidate, 18)
        })
        .await
        .unwrap();
    assert!(matches!(
        applied,
        TemporaryRenameOutcome::Applied { generation: 1, .. }
    ));
    assert_eq!(row_store.transaction_count(), 1);
    assert_eq!(row_store.accepted(), row_text);
    assert_eq!(repository.generation().await, 1);

    let row_store_for_repeat = Arc::clone(&row_store);
    let repeated = target
        .rename_over(&save, move |candidate| {
            Arc::clone(&row_store_for_repeat).activate(candidate, 19)
        })
        .await
        .unwrap();
    assert!(matches!(
        repeated,
        TemporaryRenameOutcome::Applied { generation: 1, .. }
    ));
    assert_eq!(row_store.transaction_count(), 1);

    // In-place truncate+write is a distinct editor pattern, but it uses
    // the same single validated activation boundary as temp rename.
    let draft = target.open_draft(1 << 20).await;
    draft.truncate(0).await.unwrap();
    draft.write_at(0, row_text).await.unwrap();
    let row_store_for_direct = Arc::clone(&row_store);
    let direct = target
        .commit_draft_with(&draft, move |candidate| {
            Arc::clone(&row_store_for_direct).activate(candidate, 19)
        })
        .await
        .unwrap();
    assert!(matches!(
        direct,
        TemporaryRenameOutcome::Applied { generation: 2, .. }
    ));
    assert_eq!(row_store.transaction_count(), 2);
    assert_eq!(row_store.accepted(), row_text);
    assert_eq!(repository.generation().await, 2);
}

#[tokio::test]
async fn vfs_rejected_editor_saves_keep_row_store_and_expose_typed_diagnostic() {
    let repository = VfsRepositoryCache::new();
    let target = repository.managed_file(vfs_row_image()).await.unwrap();
    let row_text = include_str!("../../tests/fixtures/vfs-row-music-london.orna").as_bytes();
    let invalid = &row_text[..row_text.len() / 2];
    let row_store = Arc::new(ValidatedRowStore::new(row_text));

    let save = target.begin_temporary_replacement(1 << 20).await;
    save.write_at(0, invalid).await.unwrap();
    let row_store_for_rename = Arc::clone(&row_store);
    let rejected_rename = target
        .rename_over(&save, move |candidate| {
            Arc::clone(&row_store_for_rename).activate(candidate, 18)
        })
        .await
        .unwrap();
    let TemporaryRenameOutcome::Rejected { diagnostic } = rejected_rename else {
        panic!("invalid temp rename must be rejected");
    };
    assert_eq!(diagnostic, rejected());
    assert_eq!(row_store.transaction_count(), 1);
    assert_eq!(row_store.accepted(), row_text);
    assert_eq!(
        target.open_read().await.read_at(0, row_text.len()).unwrap(),
        row_text
    );
    let retained = save.retained_invalid_draft().await.unwrap();
    assert_eq!(retained.replacement_bytes().unwrap(), invalid);
    assert_eq!(retained.diagnostic(), rejected());
    assert_ne!(retained.id(), [0u8; 16]);

    let row_store_for_repeat = Arc::clone(&row_store);
    let repeated = target
        .rename_over(&save, move |candidate| {
            Arc::clone(&row_store_for_repeat).activate(candidate, 19)
        })
        .await
        .unwrap();
    assert!(matches!(
        repeated,
        TemporaryRenameOutcome::Rejected {
            diagnostic
        } if diagnostic == rejected()
    ));
    assert_eq!(row_store.transaction_count(), 1);
    assert_eq!(repository.generation().await, 0);
    // Repeated syncs of one unchanged revision are one rejection, so the
    // retained draft keeps the identity it was first given (ORNA-VFS-009).
    assert_eq!(
        save.retained_invalid_draft().await.unwrap().id(),
        retained.id()
    );

    let draft = target.open_draft(1 << 20).await;
    draft.truncate(0).await.unwrap();
    draft.write_at(0, invalid).await.unwrap();
    let row_store_for_direct = Arc::clone(&row_store);
    let rejected_direct = target
        .commit_draft_with(&draft, move |candidate| {
            Arc::clone(&row_store_for_direct).activate(candidate, 20)
        })
        .await
        .unwrap();
    let TemporaryRenameOutcome::Rejected { diagnostic } = rejected_direct else {
        panic!("invalid direct write must be rejected");
    };
    assert_eq!(diagnostic, rejected());
    assert_eq!(row_store.transaction_count(), 2);
    assert_eq!(row_store.accepted(), row_text);
    assert_eq!(
        target.open_read().await.read_at(0, row_text.len()).unwrap(),
        row_text
    );
    let retained = draft.retained_invalid_draft().await.unwrap();
    assert_eq!(retained.replacement_bytes().unwrap(), invalid);
    assert_eq!(retained.diagnostic(), rejected());
    assert_eq!(repository.generation().await, 0);

    // A stale candidate is a rejected candidate: its bytes and diagnostic
    // stay readable through the same path as a validation rejection, and
    // the destination keeps the accepted row (ORNA-VFS-009).
    let stale = target.open_draft(1 << 20).await;
    stale.truncate(0).await.unwrap();
    stale.write_at(0, row_text).await.unwrap();
    let row_store_for_stale = Arc::clone(&row_store);
    let seeded = target
        .commit_draft_with(&stale, move |candidate| {
            Arc::clone(&row_store_for_stale).activate(candidate, 21)
        })
        .await
        .unwrap();
    assert!(matches!(seeded, TemporaryRenameOutcome::Applied { .. }));
    assert!(stale.retained_invalid_draft().await.is_none());

    // `draft` still holds the rejected candidate and its recorded baseline,
    // so its next commit is stale before any activation is consulted.
    let before_stale_commits = row_store.transaction_count();
    let row_store_for_stale_commit = Arc::clone(&row_store);
    let stale_outcome = target
        .commit_draft_with(&draft, move |candidate| {
            Arc::clone(&row_store_for_stale_commit).activate(candidate, 22)
        })
        .await
        .unwrap();
    let TemporaryRenameOutcome::Stale { diagnostic } = stale_outcome else {
        panic!("a draft over a replaced image must be stale");
    };
    let stale_diagnostic = stale_baseline_diagnostic();
    assert_eq!(diagnostic, stale_diagnostic);
    assert_eq!(row_store.transaction_count(), before_stale_commits);
    assert_eq!(row_store.accepted(), row_text);
    assert_eq!(
        target.open_read().await.read_at(0, row_text.len()).unwrap(),
        row_text
    );
    let parked = draft.retained_invalid_draft().await.unwrap();
    assert_eq!(parked.replacement_bytes().unwrap(), invalid);
    assert_eq!(parked.diagnostic(), stale_diagnostic);
    assert_ne!(parked.id(), [0u8; 16]);
    // A stale rejection of the same unchanged revision keeps the identity
    // the draft already had; only its diagnostic is superseded.
    assert_eq!(parked.id(), retained.id());

    // A changed candidate is a new revision, so its rejection is a new
    // retained draft with its own identity (ORNA-VFS-009).
    draft.write_at(0, b"x").await.unwrap();
    let row_store_for_stale_again = Arc::clone(&row_store);
    let stale_again = target
        .commit_draft_with(&draft, move |candidate| {
            Arc::clone(&row_store_for_stale_again).activate(candidate, 23)
        })
        .await
        .unwrap();
    assert!(matches!(stale_again, TemporaryRenameOutcome::Stale { .. }));
    let reparked = draft.retained_invalid_draft().await.unwrap();
    assert_eq!(reparked.revision(), parked.revision() + 1);
    assert_ne!(reparked.id(), parked.id());
    assert_eq!(row_store.transaction_count(), before_stale_commits);
    assert_eq!(row_store.accepted(), row_text);
}

#[tokio::test]
async fn temp_fsync_is_scratch_only_and_rename_preserves_rejected_and_open_state() {
    let repository = VfsRepositoryCache::new();
    let target = repository.managed_file(fixture_image()).await.unwrap();
    let old_handle = target.open_read().await;
    let old_bytes = old_handle.read_at(0, old_handle.len() as usize).unwrap();
    let scratch = target.begin_temporary_replacement(1 << 20).await;
    scratch.write_at(0, &[0x58]).await.unwrap();

    let scratch_syncs = AtomicUsize::new(0);
    let sequence = scratch
        .fsync_scratch_with(|revision, bytes| {
            scratch_syncs.fetch_add(1, Ordering::SeqCst);
            async move {
                assert_eq!(revision, 2);
                assert_eq!(bytes.read_range(0, 1).unwrap(), &[0x58]);
                Ok::<_, ()>(())
            }
        })
        .await
        .unwrap();
    assert_eq!(sequence, 2);
    assert_eq!(scratch_syncs.load(Ordering::SeqCst), 1);
    assert_eq!(target.cache_generation().await, 0);
    assert_eq!(
        target
            .open_read()
            .await
            .read_at(0, old_bytes.len())
            .unwrap(),
        old_bytes
    );

    let activations = AtomicUsize::new(0);
    let rejected_outcome = target
        .rename_over(&scratch, |candidate| {
            activations.fetch_add(1, Ordering::SeqCst);
            async move {
                assert_eq!(candidate.replacement_bytes(), &[0x58]);
                Ok::<_, ()>(ActivationDecision::Rejected(rejected()))
            }
        })
        .await
        .unwrap();
    assert!(matches!(
        rejected_outcome,
        TemporaryRenameOutcome::Rejected { .. }
    ));
    assert_eq!(target.cache_generation().await, 0);
    assert_eq!(old_handle.read_at(0, old_bytes.len()).unwrap(), old_bytes);
    let retained = scratch.retained_invalid_draft().await.unwrap();
    assert_eq!(retained.replacement_bytes().unwrap(), &[0x58]);
    assert_eq!(*retained.baseline().snapshot(), 7);

    let repeated = target
        .rename_over(&scratch, |_| {
            activations.fetch_add(1, Ordering::SeqCst);
            async {
                Ok::<_, ()>(ActivationDecision::Accepted(SnapshotPin::capture(
                    Arc::new(9),
                )))
            }
        })
        .await
        .unwrap();
    assert!(matches!(repeated, TemporaryRenameOutcome::Rejected { .. }));
    assert_eq!(activations.load(Ordering::SeqCst), 1);

    scratch.write_at(1, &[0x59]).await.unwrap();
    let accepted = target
        .rename_over(&scratch, |candidate| async move {
            assert_eq!(candidate.replacement_bytes(), &[0x58, 0x59]);
            Ok::<_, ()>(ActivationDecision::Accepted(SnapshotPin::capture(
                Arc::new(8),
            )))
        })
        .await
        .unwrap();
    let TemporaryRenameOutcome::Applied { generation, handle } = accepted else {
        panic!("validated rename should replace the managed target");
    };
    assert_eq!(generation, 1);
    assert_eq!(target.cache_generation().await, 1);
    assert_eq!(*old_handle.pin().snapshot(), 7);
    assert_eq!(old_handle.read_at(0, old_bytes.len()).unwrap(), old_bytes);
    assert_eq!(*target.open_read().await.pin().snapshot(), 8);
    assert_eq!(handle.read_at(0, 2).unwrap(), [0x58, 0x59]);
    assert_eq!(
        scratch
            .retained_invalid_draft()
            .await
            .unwrap()
            .replacement_bytes()
            .unwrap(),
        &[0x58]
    );
}

/// ORNA-VFS-019: accepted API replacement invalidates shared directory, attribute, and sibling-handle projections without retargeting open handles.
#[tokio::test]
async fn accepted_rename_invalidates_shared_directory_attribute_and_handle_projections() {
    let repository = VfsRepositoryCache::new();
    let raw_target_image = fixture_image();
    let stale_snapshot = Arc::clone(&raw_target_image.pin.snapshot);
    let target = repository
        .managed_file(Arc::clone(&raw_target_image))
        .await
        .unwrap();
    let sibling_image = Arc::new(VfsFileSnapshot::from_candidate(
        SnapshotPin::capture(Arc::clone(&stale_snapshot)),
        raw_target_image.bytes.duplicate().unwrap(),
    ));
    assert!(!Arc::ptr_eq(&raw_target_image, &sibling_image));
    let sibling = repository
        .managed_file(Arc::clone(&sibling_image))
        .await
        .unwrap();
    assert!(std::ptr::eq(
        raw_target_image.projection().unwrap(),
        sibling_image.projection().unwrap()
    ));
    let old_handle = target.open_read().await;
    let sibling_handle = sibling.open_read().await;
    let old_bytes = old_handle.read_at(0, old_handle.len() as usize).unwrap();
    let directory = SnapshotReaddirCursor::new(old_handle.pin().clone_pin(), 0_u64);
    let attributes = old_handle.project(("size", old_handle.len()));
    assert_eq!(old_handle.projection().unwrap().generation(), 0);
    assert_eq!(sibling_handle.projection().unwrap().generation(), 0);
    assert!(old_handle.projection_is_current().await.unwrap());
    assert!(directory.projection_is_current().await.unwrap());
    assert!(sibling_handle.projection_is_current().await.unwrap());
    assert!(attributes.projection_is_current().await.unwrap());

    let unrelated_scope = VfsRepositoryCache::new();
    assert!(matches!(
        unrelated_scope
            .managed_file(Arc::clone(&old_handle.image))
            .await,
        Err(VfsRepositoryCacheError::ImageAlreadyScoped)
    ));

    let scratch = target.begin_temporary_replacement(1 << 20).await;
    scratch.write_at(0, &[0x58]).await.unwrap();
    assert_eq!(repository.generation().await, 0);
    assert!(old_handle.projection_is_current().await.unwrap());
    assert!(directory.projection_is_current().await.unwrap());
    assert!(sibling_handle.projection_is_current().await.unwrap());
    assert!(attributes.projection_is_current().await.unwrap());

    let rejected_outcome = target
        .rename_over(&scratch, |_| async {
            Ok::<_, ()>(ActivationDecision::Rejected(rejected()))
        })
        .await
        .unwrap();
    assert!(matches!(
        rejected_outcome,
        TemporaryRenameOutcome::Rejected { .. }
    ));
    assert_eq!(repository.generation().await, 0);
    assert!(old_handle.projection_is_current().await.unwrap());
    assert!(directory.projection_is_current().await.unwrap());
    assert!(sibling_handle.projection_is_current().await.unwrap());
    assert!(attributes.projection_is_current().await.unwrap());

    // A new editor revision can be accepted while the prior rejected
    // bytes remain available through the retained-draft interface.
    scratch.write_at(1, &[0x59]).await.unwrap();
    let applied = target
        .rename_over(&scratch, |candidate| async move {
            assert_eq!(candidate.replacement_bytes(), &[0x58, 0x59]);
            Ok::<_, ()>(ActivationDecision::Accepted(SnapshotPin::capture(
                Arc::new(8),
            )))
        })
        .await
        .unwrap();
    let TemporaryRenameOutcome::Applied { generation, handle } = applied else {
        panic!("validated rename should advance the shared projection epoch");
    };
    assert_eq!(generation, 1);
    assert_eq!(repository.generation().await, 1);
    assert!(!old_handle.projection_is_current().await.unwrap());
    assert!(!directory.projection_is_current().await.unwrap());
    assert!(!sibling_handle.projection_is_current().await.unwrap());
    assert!(!attributes.projection_is_current().await.unwrap());
    assert_eq!(old_handle.read_at(0, old_bytes.len()).unwrap(), old_bytes);
    assert!(handle.projection_is_current().await.unwrap());
    // Admission binds every alias of the original raw image to generation
    // zero. After the accepted replacement advances the repository epoch,
    // replaying that alias cannot stamp its stale bytes as current.
    let raw_projection = raw_target_image.projection().unwrap();
    assert_eq!(raw_projection.generation(), 0);
    assert!(!raw_projection.is_current().await);
    assert!(matches!(
        repository.managed_file(Arc::clone(&raw_target_image)).await,
        Err(VfsRepositoryCacheError::ImageAlreadyScoped)
    ));
    // Re-capturing the same owner snapshot Arc must recover the original
    // binding too; making a new VfsFileSnapshot wrapper cannot bypass it.
    let rewrapped_stale_image = Arc::new(VfsFileSnapshot::from_candidate(
        SnapshotPin::capture(Arc::clone(&stale_snapshot)),
        raw_target_image.bytes.duplicate().unwrap(),
    ));
    assert_eq!(rewrapped_stale_image.projection().unwrap().generation(), 0);
    assert!(
        !rewrapped_stale_image
            .projection()
            .unwrap()
            .is_current()
            .await
    );
    assert!(matches!(
        repository.managed_file(rewrapped_stale_image).await,
        Err(VfsRepositoryCacheError::ImageAlreadyScoped)
    ));
    let fresh_handle = target.open_read().await;
    assert_eq!(fresh_handle.projection().unwrap().generation(), 1);
    assert!(fresh_handle.projection_is_current().await.unwrap());
    // A managed file that was not itself replaced keeps its old image
    // stamp; opening it cannot relabel that stale image as current.
    assert_eq!(
        sibling.open_read().await.projection().unwrap().generation(),
        0
    );
    assert_eq!(
        scratch
            .retained_invalid_draft()
            .await
            .unwrap()
            .replacement_bytes()
            .unwrap(),
        &[0x58]
    );
}

#[tokio::test]
async fn repository_scoped_rename_refuses_a_foreign_cache_scope() {
    let repository = VfsRepositoryCache::new();
    let unrelated = VfsRepositoryCache::new();
    let target = repository.managed_file(fixture_image()).await.unwrap();
    let foreign_image = Arc::new(VfsFileSnapshot::new(
        SnapshotPin::capture(Arc::new(7_u64)),
        Arc::<[u8]>::from(
            include_str!("../../tests/fixtures/publication-repository-main.orna").as_bytes(),
        ),
    ));
    let foreign = unrelated.managed_file(foreign_image).await.unwrap();
    let foreign_save = foreign.begin_temporary_replacement(1 << 20).await;
    foreign_save.write_at(0, &[0x58]).await.unwrap();

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_by_route = Arc::clone(&calls);
    let outcome = repository
        .rename_over(&target, &foreign_save, move |_| async move {
            calls_by_route.fetch_add(1, Ordering::SeqCst);
            Ok::<_, ()>(ActivationDecision::Rejected(rejected()))
        })
        .await;
    assert!(matches!(
        outcome,
        Err(TemporaryRenameError::WrongDestination)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(repository.generation().await, 0);
    assert_eq!(unrelated.generation().await, 0);

    // A scratch tied to a destination in a different repository scope is
    // refused the same way, and the target keeps its accepted bytes.
    let save = target.begin_temporary_replacement(1 << 20).await;
    save.write_at(0, &[0x59]).await.unwrap();
    let foreign_route = unrelated
        .rename_over(&target, &save, |_| async {
            Ok::<_, ()>(ActivationDecision::Rejected(rejected()))
        })
        .await;
    assert!(matches!(
        foreign_route,
        Err(TemporaryRenameError::WrongRepositoryScope)
    ));
    assert_eq!(repository.generation().await, 0);
    let baseline = include_str!("../../tests/fixtures/publication-repository-main.orna");
    assert_eq!(
        target.open_read().await.read_at(0, baseline.len()).unwrap(),
        baseline.as_bytes()
    );
}

#[tokio::test]
async fn accepted_rename_rebases_the_next_draft_on_the_live_image() {
    let repository = VfsRepositoryCache::new();
    let target = repository.managed_file(fixture_image()).await.unwrap();
    let accepted_bytes = include_str!("../../tests/fixtures/checkpoint_snapshot_v1.orna");

    let save = target.begin_temporary_replacement(1 << 20).await;
    save.write_at(0, accepted_bytes.as_bytes()).await.unwrap();
    let applied = target
        .rename_over(&save, |_| async {
            Ok::<_, ()>(ActivationDecision::Accepted(SnapshotPin::capture(
                Arc::new(8),
            )))
        })
        .await
        .unwrap();
    assert!(matches!(
        applied,
        TemporaryRenameOutcome::Applied { generation: 1, .. }
    ));

    // A draft opened after the accepted rename compares against the live
    // image, so it is not stale and it commits instead of being refused.
    let next = target.open_draft(1 << 20).await;
    assert_eq!(
        next.baseline()
            .await
            .read_at(0, accepted_bytes.len())
            .unwrap(),
        accepted_bytes.as_bytes()
    );
    next.write_at(0, &[0x5A]).await.unwrap();
    let committed = target
        .commit_draft_with(&next, |candidate| async move {
            assert_eq!(*candidate.baseline().snapshot(), 8);
            Ok::<_, ()>(ActivationDecision::Accepted(SnapshotPin::capture(
                Arc::new(11),
            )))
        })
        .await
        .unwrap();
    assert!(matches!(
        committed,
        TemporaryRenameOutcome::Applied { generation: 2, .. }
    ));
    assert_eq!(target.open_read().await.read_at(0, 1).unwrap(), [0x5A]);
}

/// A strong read baseline refuses the save once a concurrent REPL change
/// has moved the row on, keeps the rejected draft, and never reaches
/// activation; a save whose read is still current applies. An open read
/// keeps the version it captured in both cases.
#[tokio::test]
async fn strong_read_baseline_refuses_a_save_after_a_concurrent_repl_change() {
    let repository = VfsRepositoryCache::new();
    let target = repository.managed_file(fixture_image()).await.unwrap();
    let read_bytes = include_str!("../../tests/fixtures/publication-repository-main.orna");
    let repl_bytes = include_str!("../../tests/fixtures/checkpoint_snapshot_v1.orna");
    let draft_bytes = include_str!("../../tests/fixtures/case_closure_edge_tail_sixty.orna");

    // The editor reads the row and retains that exact version, with its own
    // draft over the version it read.
    let baseline = target.open_read_baseline(1 << 20).await;
    let read_handle = baseline.baseline();
    assert_eq!(
        read_handle.read_at(0, read_bytes.len()).unwrap(),
        read_bytes.as_bytes()
    );
    baseline.draft().truncate(0).await.unwrap();
    baseline
        .draft()
        .write_at(0, draft_bytes.as_bytes())
        .await
        .unwrap();

    // A concurrent REPL change is accepted first, so the row no longer
    // holds the version the editor read.
    let repl = target.open_draft(1 << 20).await;
    repl.truncate(0).await.unwrap();
    repl.write_at(0, repl_bytes.as_bytes()).await.unwrap();
    let repl_commit = target
        .commit_draft_with(&repl, |_| async {
            Ok::<_, ()>(ActivationDecision::Accepted(SnapshotPin::capture(
                Arc::new(8),
            )))
        })
        .await
        .unwrap();
    assert!(matches!(
        repl_commit,
        TemporaryRenameOutcome::Applied { generation: 1, .. }
    ));

    // The editor's save is refused on its read baseline: activation is
    // never reached and the draft is retained rather than discarded.
    let activation_calls = Arc::new(AtomicUsize::new(0));
    let calls = Arc::clone(&activation_calls);
    let stale = target
        .save_strong_with(&baseline, move |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Ok::<_, ()>(ActivationDecision::Rejected(rejected())) }
        })
        .await
        .unwrap();
    assert!(matches!(stale, TemporaryRenameOutcome::Stale { .. }));
    assert_eq!(activation_calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        baseline
            .draft()
            .retained_invalid_draft()
            .await
            .expect("a refused read-baseline save keeps its draft")
            .replacement_bytes()
            .unwrap(),
        draft_bytes.as_bytes()
    );
    // The REPL change is still what the row holds, and the read handle
    // still reads the version it captured.
    assert_eq!(
        target
            .open_read()
            .await
            .read_at(0, repl_bytes.len())
            .unwrap(),
        repl_bytes.as_bytes()
    );
    assert_eq!(
        read_handle.read_at(0, read_bytes.len()).unwrap(),
        read_bytes.as_bytes()
    );

    // A read whose version is still the accepted one saves normally, so
    // the refusal above is the baseline check and not a broken route.
    let current = target.open_read_baseline(1 << 20).await;
    current.draft().truncate(0).await.unwrap();
    current
        .draft()
        .write_at(0, draft_bytes.as_bytes())
        .await
        .unwrap();
    let applied = target
        .save_strong_with(&current, |_| async {
            Ok::<_, ()>(ActivationDecision::Accepted(SnapshotPin::capture(
                Arc::new(11),
            )))
        })
        .await
        .unwrap();
    assert!(matches!(
        applied,
        TemporaryRenameOutcome::Applied { generation: 2, .. }
    ));
    assert_eq!(
        target
            .open_read()
            .await
            .read_at(0, draft_bytes.len())
            .unwrap(),
        draft_bytes.as_bytes()
    );
}

struct DurableRenameValidator {
    table: String,
    object_id: TableObjectId,
    accepted: Vec<u8>,
    expected_content_transfer: Option<ProtectedContentTransferEvidence>,
    schema: TableSchemaIdentity,
}

impl TableActivationCandidateValidator for DurableRenameValidator {
    fn tables(&self) -> &[String] {
        std::slice::from_ref(&self.table)
    }

    fn table_object_ids(&self) -> BTreeMap<String, TableObjectId> {
        BTreeMap::from([(self.table.clone(), self.object_id)])
    }

    fn schema_identity(&mut self) -> Option<TableSchemaIdentity> {
        Some(self.schema.clone())
    }

    fn validate(&mut self, rows: &RuntimeTableRows) -> Result<(), SafeDiagnostic> {
        let accepted = rows
            .get(&self.table)
            .and_then(|rows| rows.first())
            .map(|(_, value)| value.as_slice());
        if accepted == Some(self.accepted.as_slice()) {
            Ok(())
        } else {
            Err(rejected())
        }
    }

    fn validate_content_pins(
        &mut self,
        _: &RuntimeTableRows,
        transfers: &[ProtectedContentTransfer],
    ) -> bool {
        match &self.expected_content_transfer {
            Some(expected) => {
                transfers.len() == 1
                    && ProtectedContentTransferEvidence::from_transfer(&transfers[0]) == *expected
            }
            None => transfers.is_empty(),
        }
    }
}

/// ORNA-VFS-021: durable temporary-save admission commits a replayable acknowledgement and rejects stale row baselines.
#[tokio::test]
async fn durable_temporary_rename_commits_and_refuses_changed_row_baseline() {
    let temp = tempfile::TempDir::new().unwrap();
    assert!(
        Command::new("git")
            .args(["init"])
            .current_dir(temp.path())
            .status()
            .unwrap()
            .success()
    );
    let repo = Repository::discover(temp.path()).unwrap();
    let schema = TableSchemaIdentity::new([7; 32]);
    let state = RuntimeState::open(
        &repo,
        RuntimeIdentity {
            database_id: [1; 16],
            repository_id: [2; 16],
        },
        [3; 32],
    )
    .await
    .unwrap();
    let identity = RuntimeTableIdentity::new("books", TableObjectId::new([11; 16])).unwrap();
    let key = vec![1];
    let target = VfsRepositoryCache::new()
        .managed_file(Arc::new(VfsFileSnapshot::new(
            SnapshotPin::capture(Arc::new(0_u64)),
            Arc::<[u8]>::from(b"original".as_slice()),
        )))
        .await
        .unwrap();
    let accepted_for_admit =
        include_bytes!("../../tests/fixtures/vfs-row-music-london.orna").to_vec();
    let stale_fixture =
        include_bytes!("../../tests/fixtures/publication-repository-main.orna").to_vec();
    let schema_for_admit = schema.clone();
    let accepted_baseline = accepted_for_admit.clone();
    let stale_for_validation = stale_fixture.clone();
    let scratch = target.begin_temporary_replacement(1 << 20).await;
    scratch.write_at(0, &accepted_for_admit).await.unwrap();
    let save = VfsDurableSave::new([4; 16], schema.clone(), identity.clone(), key.clone())
        .with_expected_baseline_row(None);
    let identity_for_admit = identity.clone();
    let accepted_for_validation = accepted_for_admit.clone();
    let admitted_from_spool = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let admitted_from_spool_output = Arc::clone(&admitted_from_spool);
    let applied = target
        .commit_durable_temporary_rename(
            &scratch,
            &state,
            save,
            move |coordinates, bytes| async move {
                admitted_from_spool_output.store(
                    matches!(bytes.source, CandidateSource::Spool(_)),
                    std::sync::atomic::Ordering::SeqCst,
                );
                let replacement = VfsTableRowReplacement::new(
                    [7; 16],
                    identity_for_admit.clone(),
                    coordinates.key().to_vec(),
                    Some(bytes.to_arc().unwrap().to_vec()),
                )
                .map_err(TableActivationError::Runtime)?;
                Ok(VfsSaveCandidate::new(
                    replacement,
                    Box::new(DurableRenameValidator {
                        table: identity_for_admit.table().to_owned(),
                        object_id: identity_for_admit.object_id(),
                        accepted: accepted_for_validation.clone(),
                        expected_content_transfer: None,
                        schema: schema_for_admit.clone(),
                    }),
                ))
            },
            &crate::NoFault,
        )
        .await
        .unwrap();
    assert!(matches!(
        applied,
        TemporaryRenameOutcome::Applied { generation: 1, .. }
    ));
    assert!(
        admitted_from_spool.load(std::sync::atomic::Ordering::SeqCst),
        "the admission callback must receive the spool-backed candidate, not a RAM copy"
    );
    assert_eq!(state.pending().await.unwrap().len(), 1);
    let replay = target
        .commit_durable_temporary_rename(
            &scratch,
            &state,
            VfsDurableSave::new([4; 16], schema.clone(), identity.clone(), key.clone())
                .with_expected_baseline_row(None),
            |_, _| async { panic!("an accepted temp rename must replay its outcome") },
            &crate::NoFault,
        )
        .await
        .unwrap();
    assert!(matches!(
        replay,
        TemporaryRenameOutcome::Applied { generation: 1, .. }
    ));
    assert_eq!(state.pending().await.unwrap().len(), 1);

    // A different writer may commit byte-identical row content. That is
    // not evidence this temp save was already accepted: its captured
    // baseline is still stale, and the scratch remains retryable.
    let identical_scratch = target.begin_temporary_replacement(1 << 20).await;
    identical_scratch
        .write_at(0, &accepted_for_admit)
        .await
        .unwrap();
    let identical_identity = identity.clone();
    let identical_bytes = accepted_for_admit.clone();
    let schema_for_identical = schema.clone();
    let identical = target
        .commit_durable_temporary_rename(
            &identical_scratch,
            &state,
            VfsDurableSave::new([4; 16], schema.clone(), identity.clone(), key.clone())
                .with_expected_baseline_row(None),
            move |coordinates, bytes| async move {
                let replacement = VfsTableRowReplacement::new(
                    [12; 16],
                    identical_identity.clone(),
                    coordinates.key().to_vec(),
                    Some(bytes.to_arc().unwrap().to_vec()),
                )
                .map_err(TableActivationError::Runtime)?;
                Ok(VfsSaveCandidate::new(
                    replacement,
                    Box::new(DurableRenameValidator {
                        table: identical_identity.table().to_owned(),
                        object_id: identical_identity.object_id(),
                        accepted: identical_bytes,
                        expected_content_transfer: None,
                        schema: schema_for_identical,
                    }),
                ))
            },
            &crate::NoFault,
        )
        .await;
    let Err(VfsSaveError::Stale { diagnostic, .. }) = identical else {
        panic!("byte-identical content cannot acknowledge a stale temp save");
    };
    assert_eq!(diagnostic.code, DiagnosticCode::StaleBaseline);
    assert_eq!(
        identical_scratch.candidate_bytes().await.unwrap().as_ref(),
        accepted_for_admit
    );
    assert_eq!(state.pending().await.unwrap().len(), 1);

    let stale_scratch = target.begin_temporary_replacement(1 << 20).await;
    stale_scratch.write_at(0, &stale_fixture).await.unwrap();
    let stale_identity = identity.clone();
    let schema_for_stale = schema.clone();
    let stale = target
        .commit_durable_temporary_rename(
            &stale_scratch,
            &state,
            VfsDurableSave::new([4; 16], schema.clone(), identity.clone(), key.clone())
                .with_expected_baseline_row(Some(b"not-current".to_vec())),
            move |coordinates, bytes| async move {
                let replacement = VfsTableRowReplacement::new(
                    [8; 16],
                    stale_identity.clone(),
                    coordinates.key().to_vec(),
                    Some(bytes.to_arc().unwrap().to_vec()),
                )
                .map_err(TableActivationError::Runtime)?;
                Ok(VfsSaveCandidate::new(
                    replacement,
                    Box::new(DurableRenameValidator {
                        table: stale_identity.table().to_owned(),
                        object_id: stale_identity.object_id(),
                        accepted: stale_for_validation,
                        expected_content_transfer: None,
                        schema: schema_for_stale.clone(),
                    }),
                ))
            },
            &crate::NoFault,
        )
        .await;
    let Err(VfsSaveError::Stale { diagnostic, .. }) = stale else {
        panic!("a temp rename over a changed durable row must be stale");
    };
    assert_eq!(diagnostic.code, DiagnosticCode::StaleBaseline);
    assert_eq!(
        stale_scratch.candidate_bytes().await.unwrap().as_ref(),
        stale_fixture
    );
    assert_eq!(state.pending().await.unwrap().len(), 1);
    let retry_identity = identity.clone();
    let retry_fixture = stale_fixture.clone();
    let schema_for_retry = schema.clone();
    let retried = target
        .commit_durable_temporary_rename(
            &stale_scratch,
            &state,
            VfsDurableSave::new([4; 16], schema.clone(), identity.clone(), key.clone())
                .with_expected_baseline_row(Some(accepted_baseline.clone())),
            move |coordinates, bytes| async move {
                let replacement = VfsTableRowReplacement::new(
                    [9; 16],
                    retry_identity.clone(),
                    coordinates.key().to_vec(),
                    Some(bytes.to_arc().unwrap().to_vec()),
                )
                .map_err(TableActivationError::Runtime)?;
                Ok(VfsSaveCandidate::new(
                    replacement,
                    Box::new(DurableRenameValidator {
                        table: retry_identity.table().to_owned(),
                        object_id: retry_identity.object_id(),
                        accepted: retry_fixture,
                        expected_content_transfer: None,
                        schema: schema_for_retry.clone(),
                    }),
                ))
            },
            &crate::NoFault,
        )
        .await
        .unwrap();
    assert!(matches!(
        retried,
        TemporaryRenameOutcome::Applied { generation: 2, .. }
    ));
    assert_eq!(state.pending().await.unwrap().len(), 2);

    let mut failed_lookup = VfsSaveAdmission::<u64>::begin(
        &state,
        VfsDurableSave::new([4; 16], schema.clone(), identity.clone(), key.clone()),
        &crate::NoFault,
    )
    .await
    .unwrap();
    failed_lookup.snapshot.table_rows.clear();
    assert_eq!(
        failed_lookup.admitted_row(),
        Err(RuntimeQueryError::TableNotAdmitted),
        "an unverifiable admitted row must remain a lookup error, not an absent row"
    );
    drop(failed_lookup);

    drop(state);
    let reopened = RuntimeState::open(
        &repo,
        RuntimeIdentity {
            database_id: [1; 16],
            repository_id: [2; 16],
        },
        [3; 32],
    )
    .await
    .unwrap();
    let snapshot = reopened
        .begin_admitted_table_activation(std::slice::from_ref(&identity))
        .await
        .unwrap();
    assert_eq!(
        snapshot.query_exact(identity.table(), &key).unwrap(),
        Some(stale_fixture.as_slice())
    );
}

fn source_blob_mib_row(encoded_blob: &[u8]) -> Vec<u8> {
    // Preserve the source row's Blob plus Float<MiB> tuple as one value.
    let mut row = vec![0x84, 0x64, b'n', b'a', b'm', b'e'];
    row.extend_from_slice(encoded_blob);
    row.extend_from_slice(&[0x64, b's', b'i', b'z', b'e', 0xf9, 0x3e, 0x00]);
    row
}

#[tokio::test]
async fn durable_blob_temp_rename_cas_commits_canonical_media_and_pin() {
    const PIXEL: &[u8] = include_bytes!("../../tests/fixtures/media/pixel.png");
    let (_temp, repo, relation_id) = format3_blob_repository();
    let capture = repo.capture_capability(relation_id).unwrap();
    let runtime_identity = RuntimeIdentity {
        database_id: [0, 0, 0, 0, 0, 0, 0x40, 0, 0x80, 0, 0, 0, 0, 0, 0, 1],
        repository_id: [0x61; 16],
    };
    let state = RuntimeState::open(&repo, runtime_identity, [0x62; 32])
        .await
        .unwrap();
    let cache = state.vfs_repository_cache();
    let retention = Arc::new(());
    let retained = Arc::downgrade(&retention);
    let target = cache
        .managed_file(Arc::new(VfsFileSnapshot::new(
            SnapshotPin::capture_with_retention(Arc::new(0_u64), retention),
            Arc::<[u8]>::from(PIXEL),
        )))
        .await
        .unwrap();
    let old_handle = target.open_read().await;
    let old_bytes = old_handle.read_at(0, old_handle.len() as usize).unwrap();
    let scratch = target.begin_temporary_replacement(1 << 20).await;
    scratch.write_at(8, &[0xa5, 0x5a]).await.unwrap();
    let mut sparse_hole = vec![0; 10];
    sparse_hole[8..10].copy_from_slice(&[0xa5, 0x5a]);
    assert_eq!(
        scratch.candidate_bytes().await.unwrap().as_ref(),
        sparse_hole.as_slice()
    );
    scratch.truncate(16).await.unwrap();
    sparse_hole.resize(16, 0);
    assert_eq!(
        scratch.candidate_bytes().await.unwrap().as_ref(),
        sparse_hole.as_slice()
    );
    scratch.truncate(5).await.unwrap();
    assert_eq!(scratch.candidate_bytes().await.unwrap().as_ref(), &[0; 5]);
    scratch.truncate(0).await.unwrap();
    let mut replacement_bytes = PIXEL.to_vec();
    replacement_bytes.resize(PIXEL.len() + 4, 0);
    scratch.write_at(0, PIXEL).await.unwrap();
    scratch
        .truncate(replacement_bytes.len() as u64)
        .await
        .unwrap();
    assert_eq!(
        scratch.candidate_bytes().await.unwrap().as_ref(),
        replacement_bytes.as_slice()
    );
    let scratch_for_persist = &scratch;
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        scratch.fsync_scratch_with(|_, bytes| {
            let expected_bytes = replacement_bytes.as_slice();
            async move {
                assert_eq!(
                    bytes.read_range(0, expected_bytes.len()).unwrap(),
                    expected_bytes
                );
                assert_eq!(
                    scratch_for_persist
                        .candidate_bytes()
                        .await
                        .unwrap()
                        .as_ref(),
                    expected_bytes
                );
                Ok::<_, ()>(())
            }
        }),
    )
    .await
    .expect("scratch persistence must release its state mutex")
    .unwrap();
    let owner_id = [0x63; 16];
    let table = RuntimeTableIdentity::new("media", TableObjectId::new(relation_id)).unwrap();
    let table_for_checks = table.clone();
    let key = b"image".to_vec();
    let schema = TableSchemaIdentity::new([7; 32]);
    let pins_before = test_git_output(
        repo.worktree(),
        &["for-each-ref", "--format=%(refname)", "refs/orna/pins"],
        None,
    );
    assert_eq!(
        target
            .open_read()
            .await
            .read_at(0, old_bytes.len())
            .unwrap(),
        old_bytes,
        "scratch flush leaves new opens on the accepted image"
    );
    let initial_rows = state
        .begin_admitted_table_activation(std::slice::from_ref(&table_for_checks))
        .await
        .unwrap();
    assert_eq!(
        initial_rows
            .query_exact(table_for_checks.table(), &key)
            .unwrap(),
        None
    );
    assert!(state.pending().await.unwrap().is_empty());

    let mut wrong_bytes = replacement_bytes.clone();
    wrong_bytes[0] ^= 1;
    let mismatch = cache
        .commit_durable_blob_rename(
            &target,
            &scratch,
            &state,
            VfsDurableSave::new(owner_id, schema.clone(), table.clone(), key.clone())
                .with_expected_baseline_row(None),
            &capture,
            ContentIdentity::new(
                replacement_bytes.len() as u64,
                Sha256::digest(&wrong_bytes).into(),
            )
            .unwrap(),
            "image/png",
            Some("png"),
            |_, _, _| async { panic!("identity mismatch must stop before row admission") },
            &crate::NoFault,
        )
        .await;
    assert!(matches!(
        mismatch,
        Err(VfsSaveError::BlobCapture(
            VfsBlobReplacementError::IdentityMismatch { .. }
        ))
    ));
    assert_eq!(
        scratch.candidate_bytes().await.unwrap().as_ref(),
        replacement_bytes.as_slice()
    );
    let rejected_rows = state
        .begin_admitted_table_activation(std::slice::from_ref(&table_for_checks))
        .await
        .unwrap();
    assert_eq!(
        rejected_rows
            .query_exact(table_for_checks.table(), &key)
            .unwrap(),
        None
    );
    assert!(state.pending().await.unwrap().is_empty());
    assert_eq!(
        test_git_output(
            repo.worktree(),
            &["for-each-ref", "--format=%(refname)", "refs/orna/pins"],
            None,
        ),
        pins_before,
        "identity rejection removes its provisional graph pin"
    );
    // A changed candidate sequence makes corrected bytes retryable after
    // the identity rejection was recorded for the previous sequence.
    scratch.write_at(0, &wrong_bytes).await.unwrap();
    scratch.write_at(0, &replacement_bytes).await.unwrap();
    let replacement_for_flush = replacement_bytes.clone();
    scratch
        .fsync_scratch_with(move |_, bytes| async move {
            assert_eq!(
                bytes.read_range(0, replacement_for_flush.len()).unwrap(),
                replacement_for_flush.as_slice()
            );
            Ok::<_, ()>(())
        })
        .await
        .unwrap();

    let expected_identity = ContentIdentity::new(
        replacement_bytes.len() as u64,
        Sha256::digest(&replacement_bytes).into(),
    )
    .unwrap();
    let stale_table = table.clone();
    let stale_table_name = stale_table.table().to_owned();
    let stale_object_id = stale_table.object_id();
    let stale_schema = schema.clone();
    let stale = cache
        .commit_durable_blob_rename(
            &target,
            &scratch,
            &state,
            VfsDurableSave::new(owner_id, schema.clone(), table.clone(), key.clone())
                .with_expected_baseline_row(Some(b"stale baseline".to_vec())),
            &capture,
            expected_identity,
            "image/png",
            None,
            move |coordinates, _, encoded_blob| async move {
                let expected_content_transfer = ProtectedContentTransferEvidence::from_transfer(
                    &encoded_blob.transfer_record(),
                );
                let row_value = source_blob_mib_row(encoded_blob.encoded_value());
                let pin = encoded_blob.into_pin();
                let mutation = TableMutation::new(
                    [0x64; 16],
                    stale_table.table(),
                    coordinates.key().to_vec(),
                    Some(row_value.clone()),
                )
                .map_err(TableActivationError::Runtime)?
                .with_table_object_id(stale_table.object_id())
                .with_protected_content_pin(&pin)
                .map_err(TableActivationError::Runtime)?;
                let replacement = VfsTableRowReplacement::from_admitted_mutation(
                    stale_table,
                    mutation,
                    vec![pin],
                )
                .map_err(TableActivationError::Runtime)?;
                Ok(VfsSaveCandidate::new(
                    replacement,
                    Box::new(DurableRenameValidator {
                        table: stale_table_name,
                        object_id: stale_object_id,
                        accepted: row_value,
                        expected_content_transfer: Some(expected_content_transfer),
                        schema: stale_schema,
                    }),
                ))
            },
            &crate::NoFault,
        )
        .await;
    let diagnostic = match stale {
        Err(VfsSaveError::Stale { diagnostic, .. }) => diagnostic,
        Err(error) => panic!("stale Blob temp rename returned another error: {error:?}"),
        Ok(_) => panic!("stale Blob temp rename must retain its retryable candidate"),
    };
    assert_eq!(diagnostic.code, DiagnosticCode::StaleBaseline);
    let stale_rows = state
        .begin_admitted_table_activation(std::slice::from_ref(&table_for_checks))
        .await
        .unwrap();
    assert_eq!(
        stale_rows
            .query_exact(table_for_checks.table(), &key)
            .unwrap(),
        None
    );
    assert!(state.pending().await.unwrap().is_empty());
    assert_eq!(
        test_git_output(
            repo.worktree(),
            &["for-each-ref", "--format=%(refname)", "refs/orna/pins"],
            None,
        ),
        pins_before,
        "stale rejection removes its provisional graph pin"
    );

    let cache_for_admit = cache;
    let encoded_row = Arc::new(TestMutex::new(None));
    let encoded_row_for_admit = Arc::clone(&encoded_row);
    let save_table = table.clone();
    let save_table_name = save_table.table().to_owned();
    let save_object_id = save_table.object_id();
    let save_schema = schema.clone();
    let target_for_admit = Arc::clone(&target);
    let scratch_for_admit = &scratch;
    let state_for_admit = &state;
    let nested_save = VfsDurableSave::new(owner_id, schema.clone(), table.clone(), key.clone());
    let nested_document_save =
        VfsDurableSave::new(owner_id, schema.clone(), table.clone(), key.clone())
            .with_expected_baseline_row(None);
    let save_key = key.clone();
    let replacement_for_admit = replacement_bytes.clone();
    let saved = cache
        .commit_durable_blob_rename(
            &target,
            &scratch,
            &state,
            VfsDurableSave::new(owner_id, schema, table, key.clone())
                .with_expected_baseline_row(None),
            &capture,
            expected_identity,
            "image/png",
            None,
            move |coordinates, _, encoded_blob| async move {
                assert_eq!(cache_for_admit.generation().await, 0);
                assert_eq!(
                    target_for_admit
                        .open_read()
                        .await
                        .read_at(0, PIXEL.len())
                        .unwrap(),
                    PIXEL
                );
                assert_eq!(
                    scratch_for_admit.candidate_bytes().await.unwrap().as_ref(),
                    replacement_for_admit.as_slice()
                );
                assert_eq!(
                    state_for_admit
                        .committed_table_row("media", &save_key)
                        .await
                        .unwrap(),
                    None
                );
                assert!(matches!(
                    VfsSaveAdmission::<u64>::begin(state_for_admit, nested_save, &crate::NoFault,)
                        .await,
                    Err(TableActivationError::Runtime(RuntimeError::AdminBusy))
                ));
                let nested_draft = target_for_admit.open_draft(1 << 20).await;
                let nested_result = target_for_admit
                    .commit_durable_document_save(
                        &nested_draft,
                        state_for_admit,
                        nested_document_save,
                        |_, _| async {
                            Err::<VfsSaveCandidate, TableActivationError>(
                                TableActivationError::Runtime(RuntimeError::AdminBusy),
                            )
                        },
                        &crate::NoFault,
                    )
                    .await;
                assert!(matches!(
                    nested_result,
                    Err(VfsSaveError::Activation(TableActivationError::Runtime(
                        RuntimeError::AdminBusy
                    )))
                ));
                let expected_content_transfer = ProtectedContentTransferEvidence::from_transfer(
                    &encoded_blob.transfer_record(),
                );
                let row_value = source_blob_mib_row(encoded_blob.encoded_value());
                *encoded_row_for_admit.lock().unwrap() = Some(row_value.clone());
                let pin = encoded_blob.into_pin();
                let mutation = TableMutation::new(
                    [0x65; 16],
                    save_table.table(),
                    coordinates.key().to_vec(),
                    Some(row_value.clone()),
                )
                .map_err(TableActivationError::Runtime)?
                .with_table_object_id(save_table.object_id())
                .with_protected_content_pin(&pin)
                .map_err(TableActivationError::Runtime)?;
                let replacement =
                    VfsTableRowReplacement::from_admitted_mutation(save_table, mutation, vec![pin])
                        .map_err(TableActivationError::Runtime)?;
                Ok(VfsSaveCandidate::new(
                    replacement,
                    Box::new(DurableRenameValidator {
                        table: save_table_name,
                        object_id: save_object_id,
                        accepted: row_value,
                        expected_content_transfer: Some(expected_content_transfer),
                        schema: save_schema,
                    }),
                ))
            },
            &crate::NoFault,
        )
        .await
        .unwrap();
    assert!(matches!(
        saved,
        TemporaryRenameOutcome::Applied { generation: 1, .. }
    ));
    assert_eq!(
        target
            .open_read()
            .await
            .read_at(0, replacement_bytes.len())
            .unwrap(),
        replacement_bytes
    );
    assert_eq!(
        old_handle.read_at(0, old_bytes.len()).unwrap(),
        old_bytes,
        "the already-open read handle remains pinned to its original image"
    );
    drop(scratch);
    assert!(retained.upgrade().is_some());
    drop(old_handle);
    assert!(retained.upgrade().is_none());
    let encoded_row = encoded_row.lock().unwrap().clone().unwrap();
    let committed_rows = state
        .begin_admitted_table_activation(std::slice::from_ref(&table_for_checks))
        .await
        .unwrap();
    assert_eq!(
        committed_rows
            .query_exact(table_for_checks.table(), &key)
            .unwrap(),
        Some(encoded_row.as_slice())
    );
    let pending = state.pending().await.unwrap();
    let transfers = TableMutation::decode(pending.last().unwrap())
        .unwrap()
        .protected_content_transfers()
        .to_vec();
    assert_eq!(transfers.len(), 1);
    assert_eq!(transfers[0].content_identity(), expected_identity);
    let protected_refs = test_git_output(
        repo.worktree(),
        &["for-each-ref", "--format=%(refname)", "refs/orna/pins"],
        None,
    );
    let protected_refs_text = String::from_utf8(protected_refs.clone()).unwrap();
    assert_eq!(protected_refs_text.lines().count(), 1);
    assert!(protected_refs_text.contains("/pending/"));
    assert!(!protected_refs_text.contains("/scratch/"));

    drop(target);
    drop(state);
    let reopened = RuntimeState::open(&repo, runtime_identity, [0x62; 32])
        .await
        .unwrap();
    let reopened_rows = reopened
        .begin_admitted_table_activation(std::slice::from_ref(&table_for_checks))
        .await
        .unwrap();
    assert_eq!(
        reopened_rows
            .query_exact(table_for_checks.table(), &key)
            .unwrap(),
        Some(encoded_row.as_slice())
    );
    let reopened_pending = reopened.pending().await.unwrap();
    assert_eq!(
        TableMutation::decode(reopened_pending.last().unwrap())
            .unwrap()
            .protected_content_transfers(),
        transfers
    );
    assert_eq!(
        test_git_output(
            repo.worktree(),
            &["for-each-ref", "--format=%(refname)", "refs/orna/pins"],
            None,
        ),
        protected_refs
    );
}

#[cfg(test)]
mod git_metadata_reserved_tests {
    use super::naming::is_reserved_component;

    #[test]
    fn git_metadata_spellings_are_reserved_components() {
        for name in [".git", ".GIT", "git~1", ".git. "] {
            assert!(is_reserved_component(name), "{name:?} must be reserved");
        }
        assert!(!is_reserved_component("git-notes"));
        assert!(!is_reserved_component("gitfile"));
    }

    #[test]
    fn row_document_spelling_is_a_reserved_component() {
        assert!(is_reserved_component("data.orna"));
        assert!(!is_reserved_component("data"));
        assert!(!is_reserved_component("data.orna.bak"));
    }
}
