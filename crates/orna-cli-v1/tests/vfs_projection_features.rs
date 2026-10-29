#![cfg(target_os = "linux")]

//! FUSE projection cases for `profiles/vfs-1.md` that the mount must show:
//! module-path directories (bead ornadb-5il1.464), key-component escaping and
//! the long-key alias (profiles/vfs-1.md:44-60, bead ornadb-5il1.462), the large Str
//! `.txt` projection (profiles/vfs-1.md:63-80), and nested record and list fields
//! (profiles/vfs-1.md:71-74, bead ornadb-5il1.372). Cases that are not implemented yet
//! are `#[ignore]`d with their bead id and run with `--include-ignored` to show the
//! current red output.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use orna_repository_v1::TypedKey;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

#[allow(dead_code)]
#[path = "support/format3.rs"]
mod format3;
#[allow(dead_code)]
#[path = "support/media_commit.rs"]
mod media_commit;
#[allow(dead_code)]
#[path = "support/editor_mount.rs"]
mod editor_mount;
use editor_mount::{
    repository_with_music_rows, repository_with_music_rows_and_modules, require_fuse_or_skip,
    spawn_mount_process,
};

const DEEP_SOURCE: &str = "pub fn deep(): Int = 3;\n";

fn music_row(title: &str) -> String {
    format!("{{ title: \"{title}\", artist: \"Kieran\", }}\n")
}

fn read_view(path: &Path) -> String {
    std::fs::read_to_string(path).expect("read accepted state through a new open")
}

fn sorted_names(directory: &Path) -> Vec<String> {
    let mut names = std::fs::read_dir(directory)
        .expect("list the directory")
        .map(|entry| entry.expect("directory entry").file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    names.sort();
    names
}

fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// profiles/vfs-1.md Row/key directory mapping and bead ornadb-5il1.464: a
/// reachable source module at a nested logical path projects as its directory
/// chain (`lib/inner/deep.orna`) beside the table directory, and reads its
/// committed bytes. Read projection only.
#[tokio::test]
async fn profile_nested_collection_directories_follow_the_module_path() {
    if !require_fuse_or_skip() {
        return;
    }
    let (repository_directory, _repository, _relation_id) = repository_with_music_rows_and_modules(
        "use lib.inner.deep;\npub table Music(id: Str) { title: Str, artist: Str, }\n",
        &[("lib/inner/deep.orna", DEEP_SOURCE)],
        &[("london", music_row("Original").as_str())],
    )
    .await;
    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    let mut mounted = spawn_mount_process(repository_directory.path(), &mountpoint);
    let deep = mountpoint.join("lib/inner/deep.orna");
    mounted.await_path(&deep);

    assert_eq!(read_view(&deep), DEEP_SOURCE, "the nested module reads its committed bytes");
    assert_eq!(sorted_names(&mountpoint.join("lib")), vec!["inner".to_owned()]);
    assert_eq!(sorted_names(&mountpoint.join("lib/inner")), vec!["deep.orna".to_owned()]);
}

/// profiles/vfs-1.md:44-50 and ORNA-VFS-016: UTF-8 bytes outside `[A-Za-z0-9._-]`
/// project as `%HH` uppercase, literal `%` and `~` are escaped, and a key that
/// is not a reserved name keeps its spelling. Each row stays addressable by its
/// escaped directory name and reads its own committed title.
#[tokio::test]
async fn key_components_escape_utf8_percent_and_tilde_per_profile() {
    if !require_fuse_or_skip() {
        return;
    }
    let keys = [
        ("x y", "x%20y"),
        ("a%b", "a%25b"),
        ("~t", "%7Et"),
        ("\u{e9}", "%C3%A9"),
        ("a/b", "a%2Fb"),
        ("a.b", "a.b"),
    ];
    let documents = keys.iter().map(|(key, _)| music_row(key)).collect::<Vec<_>>();
    let rows = keys
        .iter()
        .zip(&documents)
        .map(|((key, _), document)| (*key, document.as_str()))
        .collect::<Vec<_>>();
    let (repository_directory, _repository, _relation_id) = repository_with_music_rows(&rows).await;
    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    let mut mounted = spawn_mount_process(repository_directory.path(), &mountpoint);
    let music = mountpoint.join("Music");
    let last = music.join("a.b/data.orna");
    mounted.await_path(&last);

    let mut expected = keys.iter().map(|(_, encoded)| (*encoded).to_owned()).collect::<Vec<_>>();
    expected.sort();
    assert_eq!(sorted_names(&music), expected, "every escaped key directory is listed");
    for (key, encoded) in keys.iter() {
        let data = music.join(encoded).join("data.orna");
        let view = read_view(&data);
        assert!(view.contains(&format!("title: \"{key}\"")), "{encoded} reads the row titled {key:?}");
        assert!(view.contains("artist: \"Kieran\""), "{encoded} keeps the row's artist field");
    }
}

/// profiles/vfs-1.md:31-33 and 44-50 (Gate E item E8): a key whose spelling is a
/// reserved component escapes its first otherwise-literal byte. `.git` and `.orna`
/// are reserved by name and `CON` is a DOS device basename, so they project as
/// `%2Egit`, `%2Eorna` and `%43ON`, each resolving its own row. profiles/vfs-1.md:46
/// names "DOS device basenames" without listing them or stating any case or extension
/// rule; the spec-silent cases are in `reserved_spelling_beyond_vfs_1_text_is_conservative`.
#[tokio::test]
async fn reserved_key_components_escape_first_byte() {
    if !require_fuse_or_skip() {
        return;
    }
    let keys = [(".git", "%2Egit"), (".orna", "%2Eorna"), ("CON", "%43ON")];
    let documents = keys.iter().map(|(key, _)| music_row(key)).collect::<Vec<_>>();
    let rows = keys
        .iter()
        .zip(&documents)
        .map(|((key, _), document)| (*key, document.as_str()))
        .collect::<Vec<_>>();
    let (repository_directory, _repository, _relation_id) = repository_with_music_rows(&rows).await;
    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    let mut mounted = spawn_mount_process(repository_directory.path(), &mountpoint);
    let music = mountpoint.join("Music");
    let last = music.join("%43ON/data.orna");
    mounted.await_path(&last);

    let mut expected = keys.iter().map(|(_, encoded)| (*encoded).to_owned()).collect::<Vec<_>>();
    expected.sort();
    assert_eq!(sorted_names(&music), expected, "every reserved key has one directory");
    for (key, encoded) in keys.iter() {
        let view = read_view(&music.join(encoded).join("data.orna"));
        assert!(view.contains(&format!("title: \"{key}\"")), "{encoded} reads the row titled {key:?}");
    }
}

/// Conservative, spec-silent (Gate E item E8). profiles/vfs-1.md:46 says only "DOS device
/// basenames": it gives no list, no case rule, no extension rule, and says nothing about
/// `CLOCK$` or about keys that share a prefix with a reserved name. The cases below pin the
/// product's conservative choice, not a spec requirement: any-case and any-extension device
/// names (`nul.tar`, `com1.txt`, `LPT9`), `CLOCK$` (whose `$` is escaped by the general
/// `%HH` rule), and lookalikes (`conx`, `com10`, `console`) that keep their spelling.
#[tokio::test]
async fn reserved_spelling_beyond_vfs_1_text_is_conservative() {
    if !require_fuse_or_skip() {
        return;
    }
    let keys = [
        ("nul.tar", "%6Eul.tar"),
        ("com1.txt", "%63om1.txt"),
        ("LPT9", "%4CPT9"),
        ("CLOCK$", "%43LOCK%24"),
        ("conx", "conx"),
        ("com10", "com10"),
        ("console", "console"),
    ];
    let documents = keys.iter().map(|(key, _)| music_row(key)).collect::<Vec<_>>();
    let rows = keys
        .iter()
        .zip(&documents)
        .map(|((key, _), document)| (*key, document.as_str()))
        .collect::<Vec<_>>();
    let (repository_directory, _repository, _relation_id) = repository_with_music_rows(&rows).await;
    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    let mut mounted = spawn_mount_process(repository_directory.path(), &mountpoint);
    let music = mountpoint.join("Music");
    let last = music.join("%43LOCK%24/data.orna");
    mounted.await_path(&last);

    let mut expected = keys.iter().map(|(_, encoded)| (*encoded).to_owned()).collect::<Vec<_>>();
    expected.sort();
    assert_eq!(sorted_names(&music), expected, "every conservative key has one directory");
    for (key, encoded) in keys.iter() {
        let view = read_view(&music.join(encoded).join("data.orna"));
        assert!(view.contains(&format!("title: \"{key}\"")), "{encoded} reads the row titled {key:?}");
    }
}

/// ORNA-VFS-016 and profiles/vfs-1.md:52-56: a component over 200 encoded bytes
/// projects as `~key-<64 lowercase hex>` where the digest is SHA-256 of the
/// canonical typed key component. The alias resolves the same row.
#[tokio::test]
async fn long_key_over_200_encoded_bytes_projects_as_sha256_key_alias() {
    if !require_fuse_or_skip() {
        return;
    }
    let key = "k".repeat(250);
    let canonical = TypedKey::Text(key.clone()).canonical_bytes().unwrap();
    let alias = format!("~key-{}", hex_lower(&Sha256::digest(&canonical)));
    let document = music_row("Long");
    let (repository_directory, _repository, _relation_id) =
        repository_with_music_rows(&[(key.as_str(), document.as_str())]).await;
    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    let mut mounted = spawn_mount_process(repository_directory.path(), &mountpoint);
    let data = mountpoint.join("Music").join(&alias).join("data.orna");
    mounted.await_path(&data);

    assert_eq!(alias.len(), "~key-".len() + 64, "the alias carries a 64-hex digest");
    let view = read_view(&data);
    assert!(view.contains("title: \"Long\""), "the alias resolves the long-key row: {view:?}");
    assert!(view.contains("artist: \"Kieran\""), "the alias keeps the row's artist field");
    assert_eq!(sorted_names(&mountpoint.join("Music")), vec![alias], "only the alias is listed");
}

/// ORNA-VFS-016 and profiles/vfs-1.md:44-47: `.`, `%` and a literal `~empty`
/// each escape their own bytes, so they never alias one another or the empty
/// key. Source admission decodes each component by the VFS-1 spelling, so these
/// keys are admitted as the rows they name.
#[tokio::test]
async fn dot_percent_and_literal_tilde_keys_are_admitted_and_escaped() {
    if !require_fuse_or_skip() {
        return;
    }
    let literal = music_row("Literal");
    let (repository_directory, _repository, _relation_id) = repository_with_music_rows(&[
        ("~empty", literal.as_str()),
        (".", music_row("Dot").as_str()),
        ("%", music_row("Percent").as_str()),
    ])
    .await;
    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    let mut mounted = spawn_mount_process(repository_directory.path(), &mountpoint);
    let literal_row = mountpoint.join("Music/%7Eempty/data.orna");
    mounted.await_path(&literal_row);

    assert!(
        read_view(&literal_row).contains("Literal"),
        "a literal ~empty key escapes its tilde"
    );
    assert!(read_view(&mountpoint.join("Music/%2E/data.orna")).contains("Dot"));
    assert!(read_view(&mountpoint.join("Music/%25/data.orna")).contains("Percent"));
}

/// ORNA-VFS-016 and profiles/vfs-1.md:41-43: the empty key is `~empty`. Blocked:
/// the empty text primary key maps to an empty `table_row.row_key`, which the
/// table schema rejects (orna-runtime-v1 lib.rs:174 `CHECK (length(row_key) > 0)`).
/// Tracked in ornadb-5il1.462.1.
#[tokio::test]
#[ignore = "ornadb-5il1.462.1: empty text key cannot be stored (table_row row_key CHECK length>0)"]
async fn empty_key_is_admitted_as_tilde_empty() {
    if !require_fuse_or_skip() {
        return;
    }
    let empty = music_row("Empty");
    let (repository_directory, _repository, _relation_id) =
        repository_with_music_rows(&[("", empty.as_str())]).await;
    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    let mut mounted = spawn_mount_process(repository_directory.path(), &mountpoint);
    let empty_row = mountpoint.join("Music/~empty/data.orna");
    mounted.await_path(&empty_row);

    assert!(read_view(&empty_row).contains("Empty"), "the empty key is ~empty");
}

/// ORNA-VFS-016 and profiles/vfs-1.md:52-56: a key over 300 characters is
/// admissible and projects through the `~key-` alias. Source admission decodes
/// its component by the VFS-1 spelling (profiles/vfs-1.md:52-56), so it is admitted.
#[tokio::test]
async fn key_over_300_characters_is_admitted_and_projects_as_alias() {
    if !require_fuse_or_skip() {
        return;
    }
    let key = "q".repeat(300);
    let canonical = TypedKey::Text(key.clone()).canonical_bytes().unwrap();
    let alias = format!("~key-{}", hex_lower(&Sha256::digest(&canonical)));
    let document = music_row("Huge");
    let (repository_directory, _repository, _relation_id) =
        repository_with_music_rows(&[(key.as_str(), document.as_str())]).await;
    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    let mut mounted = spawn_mount_process(repository_directory.path(), &mountpoint);
    let data = mountpoint.join("Music").join(&alias).join("data.orna");
    mounted.await_path(&data);
    assert!(read_view(&data).contains("Huge"));
}

/// profiles/vfs-1.md:63-66 (large Str) and ORNA-VFS-020 neighbourhood: a Str over
/// 64 KiB projects as `<field>.txt` beside `data.orna`, and `data.orna` carries
/// `{text: "./<field>.txt", encoding: "utf-8"}` instead of the text. A UTF-8 write to
/// the `.txt` replaces the Str (not a Blob).
#[tokio::test]
async fn large_str_projects_as_txt_and_utf8_write_replaces_the_str() {
    if !require_fuse_or_skip() {
        return;
    }
    let big = "a".repeat(70 * 1024);
    let document = format!("{{ title: \"{big}\", artist: \"Kieran\", }}\n");
    let (repository_directory, _repository, _relation_id) = repository_with_music_rows_and_modules(
        "pub table Music(id: Str) { title: Str, artist: Str, }\n",
        &[],
        &[("london", document.as_str())],
    )
    .await;
    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    let mut mounted = spawn_mount_process(repository_directory.path(), &mountpoint);
    let row = mountpoint.join("Music/london");
    let title = row.join("title.txt");
    mounted.await_path(&title);

    assert!(std::fs::read(&title).unwrap() == big.as_bytes(), "the .txt is the full Str, byte-exact");
    let data = read_view(&row.join("data.orna"));
    assert!(data.contains("./title.txt"), "data.orna names the .txt file: {data:?}");
    assert!(data.contains("encoding: \"utf-8\""), "data.orna names utf-8: {data:?}");
    assert!(!data.contains(&big), "data.orna no longer carries the 70 KiB text");
    editor_mount::write_and_sync(&title, "short replacement")
        .expect("a UTF-8 write replaces the Str");
    // profiles/vfs-1.md:63-66: the 64 KiB bound applies to the replaced value, so a short
    // Str projects inline again and the `.txt` sibling is gone.
    assert!(!title.exists(), "a short replacement is inline; no title.txt remains");
    let data = read_view(&row.join("data.orna"));
    assert!(data.contains("short replacement"), "data.orna shows the replaced Str: {data:?}");
    assert!(!data.contains("./title.txt"), "data.orna no longer names a .txt: {data:?}");
}

/// profiles/vfs-1.md:63-66: the 64 KiB bound is exclusive. A Str of exactly 65536 bytes stays
/// inline in `data.orna` with no `.txt` sibling; one byte more projects as `title.txt`.
#[tokio::test]
async fn large_str_bound_is_exclusive_at_64_kib() {
    if !require_fuse_or_skip() {
        return;
    }
    let at_bound = "b".repeat(64 * 1024);
    let over_bound = "c".repeat(64 * 1024 + 1);
    let inline_document = format!("{{ title: \"{at_bound}\", artist: \"Kieran\", }}\n");
    let over_document = format!("{{ title: \"{over_bound}\", artist: \"Kieran\", }}\n");
    let (repository_directory, _repository, _relation_id) = repository_with_music_rows(&[
        ("inline", inline_document.as_str()),
        ("over", over_document.as_str()),
    ])
    .await;
    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    let mut mounted = spawn_mount_process(repository_directory.path(), &mountpoint);
    let over = mountpoint.join("Music/over/title.txt");
    mounted.await_path(&over);

    assert_eq!(std::fs::read(&over).unwrap(), over_bound.as_bytes());
    let inline = mountpoint.join("Music/inline");
    assert_eq!(sorted_names(&inline), vec!["data.orna".to_owned()], "no .txt at the bound");
    assert!(read_view(&inline.join("data.orna")).contains(&at_bound));
}

/// profiles/vfs-1.md:63-66 and ORNA-VFS-009: a `.txt` write whose bytes are not UTF-8 is refused
/// at fsync with EINVAL, and the accepted row and its `.txt` stay unchanged.
#[tokio::test]
async fn large_str_text_rejects_invalid_utf8_and_keeps_the_row() {
    if !require_fuse_or_skip() {
        return;
    }
    let big = "d".repeat(70 * 1024);
    let document = format!("{{ title: \"{big}\", artist: \"Kieran\", }}\n");
    let (repository_directory, _repository, _relation_id) =
        repository_with_music_rows(&[("london", document.as_str())]).await;
    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    let mut mounted = spawn_mount_process(repository_directory.path(), &mountpoint);
    let row = mountpoint.join("Music/london");
    let title = row.join("title.txt");
    mounted.await_path(&title);

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&title)
        .expect("a write-intent open of the .txt");
    std::io::Write::write_all(&mut file, &[0xff, 0xfe, b'x']).expect("the bytes are buffered");
    assert_eq!(
        file.sync_all().unwrap_err().raw_os_error(),
        Some(libc::EINVAL),
        "non-UTF-8 text is refused"
    );
    drop(file);
    assert_eq!(std::fs::read(&title).unwrap(), big.as_bytes(), "the accepted .txt is unchanged");
    assert!(read_view(&row.join("data.orna")).contains("./title.txt"));
}

/// ORNA-VFS-009 and profiles/vfs-1.md:63-66: a `.txt` write-intent handle opened before
/// another writer's accepted save is stale. Its fsync refuses the candidate, and the
/// candidate is kept in recovery storage before the fsync replies.
#[tokio::test]
async fn stale_large_str_candidate_is_retained_before_fsync_replies() {
    if !require_fuse_or_skip() {
        return;
    }
    let big = "e".repeat(70 * 1024);
    let document = format!("{{ title: \"{big}\", artist: \"Kieran\", }}\n");
    let (repository_directory, repository, _relation_id) =
        repository_with_music_rows(&[("london", document.as_str())]).await;
    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    let mut mounted = spawn_mount_process(repository_directory.path(), &mountpoint);
    let row = mountpoint.join("Music/london");
    let title = row.join("title.txt");
    let data = row.join("data.orna");
    mounted.await_path(&title);

    let mut stale_text = std::fs::OpenOptions::new()
        .write(true)
        .open(&title)
        .expect("a write-intent open of the .txt");

    // Another writer's accepted save advances the row past the open handle.
    let advanced = read_view(&data).replace("artist: \"Kieran\"", "artist: \"Other\"");
    let mut other = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&data)
        .expect("a write-intent open of data.orna");
    std::io::Write::write_all(&mut other, advanced.as_bytes()).expect("the bytes are buffered");
    other.sync_all().expect("the other writer's save is accepted");
    drop(other);
    assert!(read_view(&data).contains("artist: \"Other\""));

    let candidate = "stale candidate text";
    std::io::Write::write_all(&mut stale_text, candidate.as_bytes())
        .expect("the bytes are buffered");
    assert!(stale_text.sync_all().is_err(), "a stale .txt candidate is refused at fsync");
    drop(stale_text);

    let records = editor_mount::retained_drafts(&repository);
    assert!(
        records.iter().any(|record| {
            record.contains("path Music/london/title.txt")
                && record.contains(&format!("\n\n{candidate}"))
        }),
        "the stale candidate is retained in recovery storage: {records:?}"
    );
    assert_eq!(std::fs::read(&title).unwrap(), big.as_bytes(), "the accepted .txt is unchanged");
}

/// profiles/vfs-1.md:71-74 (nested records and lists): a record field projects as
/// an escaped directory whose children are its fields, and a list field projects
/// as a directory of decimal element indices. Source admission accepts this row in
/// encoded-key order (ornadb-5il1.372), so the mount shows its pinned directories.
#[tokio::test]
async fn nested_record_and_list_fields_project_as_pinned_directories() {
    if !require_fuse_or_skip() {
        return;
    }
    let document = "{ meta: { label: \"x\", rank: 1, }, tags: [\"a\", \"b\"], }\n";
    let (repository_directory, _repository, _relation_id) = repository_with_music_rows_and_modules(
        "pub table Music(id: Str) { meta: { label: Str, rank: Int, }, tags: [Str], }\n",
        &[],
        &[("london", document)],
    )
    .await;
    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    let mut mounted = spawn_mount_process(repository_directory.path(), &mountpoint);
    let row = mountpoint.join("Music/london");
    let label = row.join("meta/label");
    mounted.await_path(&label);

    assert_eq!(
        read_view(&label),
        "x",
        "the record field is a file under its directory"
    );
    assert_eq!(read_view(&row.join("meta/rank")), "1");
    assert_eq!(
        sorted_names(&row.join("tags")),
        vec!["0".to_owned(), "1".to_owned()]
    );
    assert_eq!(
        read_view(&row.join("tags/1")),
        "b",
        "list elements are decimal indices"
    );
}

const EPERM: i32 = 1;
const EAGAIN: i32 = 11;

/// Replaces `Music/<key>` through `orna edit` on the repository path: a writer
/// outside the mount, as in vfs_mount_sessions.rs `edit_elsewhere`.
fn edit_outside_mount(repository: &Path, key: &str, document: &str) {
    let scripts = TempDir::new().unwrap();
    let editor = scripts.path().join("edit-outside");
    std::fs::write(
        &editor,
        format!("#!/bin/sh\ncat > \"$1\" <<'ORNA_EDIT'\n{document}ORNA_EDIT\n"),
    )
    .unwrap();
    std::fs::set_permissions(&editor, std::fs::Permissions::from_mode(0o755)).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_orna"))
        .arg("--db")
        .arg(repository)
        .args(["edit", &format!("Music/{key}/data.orna")])
        .env("EDITOR", &editor)
        .output()
        .expect("run orna edit outside the mount");
    assert!(
        output.status.success(),
        "orna edit outside the mount failed: stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

/// profiles/vfs-1.md:71-74 and the stale-save refresh (fuse_mount
/// `refresh_stale_document`): after another writer changes a row, a stale save of
/// `data.orna` is refused with EAGAIN and new opens show the current row. The
/// nested directories are replaced with it, so `meta/label` matches the row, and a
/// pending scratch file in the row directory survives the refresh.
#[tokio::test]
async fn stale_save_refreshes_nested_directories_and_keeps_scratch() {
    if !require_fuse_or_skip() {
        return;
    }
    let document = "{ meta: { label: \"x\", rank: 1, }, tags: [\"a\", \"b\"], }\n";
    let (repository_directory, _repository, _relation_id) = repository_with_music_rows_and_modules(
        "pub table Music(id: Str) { meta: { label: Str, rank: Int, }, tags: [Str], }\n",
        &[],
        &[("london", document)],
    )
    .await;
    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    let mut mounted = spawn_mount_process(repository_directory.path(), &mountpoint);
    let row = mountpoint.join("Music/london");
    let label = row.join("meta/label");
    mounted.await_path(&label);
    assert_eq!(read_view(&label), "x");
    assert_eq!(
        sorted_names(&row.join("meta")),
        vec!["label".to_owned(), "rank".to_owned()]
    );

    let scratch = row.join("pending.tmp");
    editor_mount::write_and_sync(&scratch, "pending bytes").expect("create a pending scratch file");
    edit_outside_mount(
        repository_directory.path(),
        "london",
        "{ meta: { label: \"y\", rank: 2, }, tags: [\"a\"], }\n",
    );
    let error = editor_mount::write_and_sync(
        &row.join("data.orna"),
        "{ meta: { label: \"z\", rank: 3, }, tags: [\"q\"], }\n",
    )
    .expect_err("a save against a row another writer changed is stale");
    assert_eq!(
        error.raw_os_error(),
        Some(EAGAIN),
        "the stale save is refused with EAGAIN"
    );

    assert_eq!(
        read_view(&label),
        "y",
        "the nested directory shows the current row"
    );
    assert_eq!(read_view(&row.join("meta/rank")), "2");
    assert_eq!(sorted_names(&row.join("tags")), vec!["0".to_owned()]);
    assert!(
        read_view(&row.join("data.orna")).contains("\"y\""),
        "data.orna shows the current row"
    );
    assert_eq!(
        read_view(&scratch),
        "pending bytes",
        "the pending scratch file survives"
    );
}

/// profiles/vfs-1.md:71-74 with the optional and tuple shapes the nested projection
/// maps: a present optional record field projects as its payload, tuple positions
/// are decimal directory names, and a write-open of a nested leaf is refused with
/// EPERM (the leaf is a read-only Bytes node; unsupported mutation, profiles/vfs-1.md:139).
#[tokio::test]
async fn nested_optional_and_tuple_fields_project_and_nested_leaves_refuse_writes() {
    if !require_fuse_or_skip() {
        return;
    }
    let document = "{ meta: { label: \"x\", rank: 1, note: Some(\"n\"), }, tags: [\"a\"], pair: (51, \"north\"), }\n";
    let (repository_directory, _repository, _relation_id) = repository_with_music_rows_and_modules(
        "pub table Music(id: Str) { meta: { label: Str, rank: Int, note: Str?, }, tags: [Str], pair: (Int, Str), }\n",
        &[],
        &[("london", document)],
    )
    .await;
    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    let mut mounted = spawn_mount_process(repository_directory.path(), &mountpoint);
    let row = mountpoint.join("Music/london");
    let note = row.join("meta/note");
    mounted.await_path(&note);
    assert_eq!(
        read_view(&note),
        "n",
        "a present optional record field reads its payload"
    );
    assert_eq!(
        read_view(&row.join("pair/0")),
        "51",
        "tuple positions are decimal names"
    );
    assert_eq!(read_view(&row.join("pair/1")), "north");
    let error = std::fs::OpenOptions::new()
        .write(true)
        .open(&note)
        .expect_err("a nested field is read-only");
    assert_eq!(
        error.raw_os_error(),
        Some(EPERM),
        "a write-open of a nested field is refused"
    );
}
