//! Shared format-3 repository and Git fixture builders for CLI integration
//! tests that exercise the capture path. Included by test binaries with
//! `#[path = "support/format3.rs"] mod format3;`.

use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use orna_conformance_v1::DurableTransactionalEvaluator;
use orna_evaluator_v1::{Limits, SysHostBindingRegistry, evaluate_expression_ovb2_with_effects};
use orna_repository_v1::Repository;
use orna_runtime_v1::RuntimeState;
use orna_storage_v1::LoosePath;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

pub fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

/// Publishes one committed media row through the explicit publication
/// boundary, the same path `orna` uses. The runtime only stages durable
/// mutations; this freezes the pending tail at its checkpoint and projects the
/// frozen prefix into the format-3 store as one Git commit, so the row becomes
/// reachable from the published snapshot.
///
/// `ordinal` names the publication intent; it must not repeat within one
/// runtime, because a frozen intent is durable.
pub async fn publish_media_row(
    repository: &Repository,
    state: &RuntimeState,
    key: &str,
    ordinal: u8,
) {
    let checkpoint = state
        .latest_checkpoint()
        .await
        .unwrap()
        .expect("a committed row leaves a checkpoint");
    let logical_key = key.to_owned();
    DurableTransactionalEvaluator::default()
        .publish_pending(
            repository,
            state,
            [ordinal; 16],
            &checkpoint,
            |mutation| LoosePath::for_key(mutation.table(), &[logical_key.clone()]),
            &format!("orna: publish {key}"),
        )
        .await
        .expect("the explicit publication boundary succeeds");
}

pub fn capture_failure(
    bindings: &mut SysHostBindingRegistry,
    root: &str,
    path: &str,
    max_bytes: u64,
) -> String {
    let source = format!("sys.blob.capture_file({root:?}, {path:?}, {max_bytes})");
    evaluate_expression_ovb2_with_effects(&source, &Default::default(), Limits::default(), bindings)
        .unwrap_err()
        .code()
        .to_owned()
}

pub fn empty_format3_repository() -> (TempDir, Repository, [u8; 16]) {
    let directory = TempDir::new().unwrap();
    let root = directory.path();
    git(root, &["init", "--quiet"]);
    git(
        root,
        &["config", "user.email", "kieran@drewett.dev"],
    );
    git(root, &["config", "user.name", "kierandrewett"]);
    git(root, &["config", "commit.gpgsign", "false"]);

    let database_id = [0, 0, 0, 0, 0, 0, 0x40, 0, 0x80, 0, 0, 0, 0, 0, 0, 1];
    let relation_id = [0x43; 16];
    let database_uuid = "00000000-0000-4000-8000-000000000001";
    let database =
        format!("{{\n    repository_format: 3,\n    database_id: \"{database_uuid}\",\n}}\n");
    let schema_source = b"capture test schema";
    std::fs::create_dir_all(root.join(".orna/store")).unwrap();
    std::fs::write(root.join(".orna/database.orna"), &database).unwrap();
    std::fs::write(root.join(".orna/store/data"), b"store marker").unwrap();
    std::fs::write(root.join("main.orna"), schema_source).unwrap();
    git(root, &["add", "--all"]);
    git(
        root,
        &["commit", "--quiet", "-m", "format3 capture fixture"],
    );

    let schema_digest: [u8; 32] = Sha256::digest(schema_source).into();
    let chunk_oid = git_object(root, "blob", schema_source);
    let mut byte_index = vec![0x85, 0x01, 0x04, 0x00];
    append_cbor_head(&mut byte_index, 0, schema_source.len());
    byte_index.push(0x81);
    byte_index.push(0x83);
    append_cbor_head(&mut byte_index, 0, schema_source.len());
    append_cbor_bytes(&mut byte_index, &git_oid_bytes(&chunk_oid));
    append_cbor_bytes(&mut byte_index, &schema_digest);
    let byte_index_oid = native_fixture_node(root, &byte_index, &[(chunk_oid.as_str(), "blob")]);

    let mut schema = vec![0x85, 0x01, 0x06];
    append_cbor_head(&mut schema, 0, schema_source.len());
    append_cbor_bytes(&mut schema, &schema_digest);
    append_cbor_bytes(&mut schema, &git_oid_bytes(&byte_index_oid));
    let schema_oid = native_fixture_node(root, &schema, &[(byte_index_oid.as_str(), "tree")]);

    let mut row_domain = vec![0x83, 0x64];
    row_domain.extend_from_slice(b"rows");
    append_cbor_bytes(&mut row_domain, &relation_id);
    append_cbor_bytes(&mut row_domain, &schema_digest);
    let mut empty_rows = vec![0x84, 0x01, 0x01];
    empty_rows.extend_from_slice(&row_domain);
    empty_rows.push(0x80);
    let row_root_oid = native_fixture_node(root, &empty_rows, &[]);

    let mut relation_domain = vec![0x82, 0x69];
    relation_domain.extend_from_slice(b"relations");
    append_cbor_bytes(&mut relation_domain, &database_id);
    let mut relation_map = vec![0x84, 0x01, 0x01];
    relation_map.extend_from_slice(&relation_domain);
    relation_map.push(0x81);
    relation_map.push(0x82);
    append_cbor_bytes(&mut relation_map, &relation_id);
    relation_map.push(0x84);
    append_cbor_bytes(&mut relation_map, &git_oid_bytes(&schema_oid));
    append_cbor_bytes(&mut relation_map, &git_oid_bytes(&row_root_oid));
    relation_map.push(0xf6);
    relation_map.push(0x00);
    let relation_map_oid = native_fixture_node(
        root,
        &relation_map,
        &[
            (schema_oid.as_str(), "tree"),
            (row_root_oid.as_str(), "tree"),
        ],
    );

    let mut store_root = vec![0x83, 0x01, 0x00];
    append_cbor_bytes(&mut store_root, &git_oid_bytes(&relation_map_oid));
    let store_root_oid =
        native_fixture_node(root, &store_root, &[(relation_map_oid.as_str(), "tree")]);
    let database_oid = git_object(root, "blob", database.as_bytes());
    let source_oid = git_object(root, "blob", schema_source);
    let orna_tree = git_tree(
        root,
        &format!(
            "100644 blob {database_oid}\tdatabase.orna\n040000 tree {store_root_oid}\tstore\n"
        ),
    );
    let root_tree = git_tree(
        root,
        &format!("040000 tree {orna_tree}\t.orna\n100644 blob {source_oid}\tmain.orna\n"),
    );
    let parent = String::from_utf8(git_output(root, &["rev-parse", "HEAD"], None))
        .unwrap()
        .trim()
        .to_owned();
    let commit = String::from_utf8(git_output(
        root,
        &[
            "commit-tree",
            &root_tree,
            "-p",
            &parent,
            "-m",
            "format3 graph fixture",
        ],
        None,
    ))
    .unwrap()
    .trim()
    .to_owned();
    let head_ref = String::from_utf8(git_output(root, &["symbolic-ref", "HEAD"], None))
        .unwrap()
        .trim()
        .to_owned();
    git(root, &["update-ref", &head_ref, &commit]);

    let repository = Repository::discover(root).unwrap();
    (directory, repository, relation_id)
}

pub fn git(path: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .unwrap()
            .success()
    );
}

pub fn git_output(path: &Path, args: &[&str], input: Option<&[u8]>) -> Vec<u8> {
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

pub fn git_object(path: &Path, kind: &str, bytes: &[u8]) -> String {
    String::from_utf8(git_output(
        path,
        &["hash-object", "-w", "-t", kind, "--stdin"],
        Some(bytes),
    ))
    .unwrap()
    .trim()
    .to_owned()
}

pub fn git_tree(path: &Path, entries: &str) -> String {
    String::from_utf8(git_output(path, &["mktree"], Some(entries.as_bytes())))
        .unwrap()
        .trim()
        .to_owned()
}

pub fn native_fixture_node(path: &Path, data: &[u8], dependencies: &[(&str, &str)]) -> String {
    let data_oid = git_object(path, "blob", data);
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
        Some(git_tree(path, &refs))
    };
    let mut envelope = format!("100644 blob {data_oid}\tdata\n");
    if let Some(refs_oid) = refs_oid {
        envelope.push_str(&format!("040000 tree {refs_oid}\trefs\n"));
    }
    git_tree(path, &envelope)
}

pub fn append_cbor_head(output: &mut Vec<u8>, major: u8, value: usize) {
    let prefix = major << 5;
    match value {
        0..=23 => output.push(prefix | value as u8),
        24..=255 => output.extend_from_slice(&[prefix | 24, value as u8]),
        _ => panic!("fixture CBOR value is unexpectedly large"),
    }
}

pub fn append_cbor_bytes(output: &mut Vec<u8>, bytes: &[u8]) {
    append_cbor_head(output, 2, bytes.len());
    output.extend_from_slice(bytes);
}

pub fn git_oid_bytes(oid: &str) -> Vec<u8> {
    oid.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let hex = std::str::from_utf8(pair).unwrap();
            u8::from_str_radix(hex, 16).unwrap()
        })
        .collect()
}
