use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

use tempfile::TempDir;

const FORMAT: &str = ".orna/format.orna";
const DATABASE: &str = ".orna/database.orna";
const INIT_STATUS: &[u8] = b"initialized Orna repository\n";

fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"));
    isolate_git(&mut command);
    command
}

fn isolate_git(command: &mut Command) {
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CEILING_DIRECTORIES",
        "GIT_DISCOVERY_ACROSS_FILESYSTEM",
        "GIT_CONFIG_COUNT",
    ] {
        command.env_remove(name);
    }
    for index in 0..16 {
        command.env_remove(format!("GIT_CONFIG_KEY_{index}"));
        command.env_remove(format!("GIT_CONFIG_VALUE_{index}"));
    }
}

fn run_init(current_dir: &Path, target: Option<&Path>) -> Output {
    let mut command = command();
    command.arg("init").current_dir(current_dir);
    if let Some(target) = target {
        command.arg(target);
    }
    command.output().expect("CLI process starts")
}

fn git(current_dir: &Path, arguments: &[&str]) -> Output {
    let mut command = Command::new("git");
    isolate_git(&mut command);
    command.args(arguments).current_dir(current_dir);
    command.output().expect("Git process starts")
}

fn git_succeeds(current_dir: &Path, arguments: &[&str]) {
    assert!(
        git(current_dir, arguments).status.success(),
        "Git command succeeds"
    );
}

fn bytes(path: &Path) -> Vec<u8> {
    fs::read(path).expect("fixture bytes are readable")
}

fn assert_fixture_path_absent(output: &Output, fixture: &Path) {
    let fixture = fixture.to_string_lossy();
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains(fixture.as_ref()),
        "stdout does not disclose a fixture path"
    );
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains(fixture.as_ref()),
        "stderr does not disclose a fixture path"
    );
}

fn assert_success(output: &Output, fixture: &Path) {
    assert!(output.status.success(), "initialization succeeds");
    assert!(
        output.stdout == INIT_STATUS && output.stderr.is_empty(),
        "success emits only the stable path-free status"
    );
    assert_fixture_path_absent(output, fixture);
}

fn assert_failure(output: &Output, fixture: &Path, code: &str) {
    assert!(!output.status.success(), "initialization fails");
    assert!(
        output.stdout.is_empty() && String::from_utf8_lossy(&output.stderr).contains(code),
        "failure emits its stable redacted diagnostic code"
    );
    assert_fixture_path_absent(output, fixture);
}

fn is_uuid_v4_record(bytes: &[u8]) -> bool {
    bytes.windows(36).any(|candidate| {
        candidate[8] == b'-'
            && candidate[13] == b'-'
            && candidate[18] == b'-'
            && candidate[23] == b'-'
            && candidate[14] == b'4'
            && matches!(candidate[19], b'8' | b'9' | b'a' | b'b')
            && candidate
                .iter()
                .enumerate()
                .all(|(index, byte)| matches!(index, 8 | 13 | 18 | 23) || byte.is_ascii_hexdigit())
    })
}

fn directory_entries(root: &Path) -> Vec<String> {
    fn collect(root: &Path, current: &Path, entries: &mut Vec<String>) {
        let mut children = fs::read_dir(current)
            .expect("fixture directory is readable")
            .map(|entry| entry.expect("fixture directory entry"))
            .collect::<Vec<_>>();
        children.sort_by_key(fs::DirEntry::file_name);
        for entry in children {
            let path = entry.path();
            entries.push(
                path.strip_prefix(root)
                    .expect("fixture entry is beneath root")
                    .to_string_lossy()
                    .into_owned(),
            );
            if entry.file_type().expect("fixture entry metadata").is_dir() {
                collect(root, &path, entries);
            }
        }
    }

    let mut entries = Vec::new();
    collect(root, root, &mut entries);
    entries
}

fn initialized_repository() -> TempDir {
    let fixture = tempfile::tempdir().expect("temporary fixture");
    let output = run_init(fixture.path(), None);
    assert_success(&output, fixture.path());
    fixture
}

fn commit_fixture_source(root: &Path, source: &[u8]) {
    fs::write(root.join("main.orna"), source).expect("fixture source written");
    git_succeeds(root, &["add", "main.orna"]);
    git_succeeds(
        root,
        &[
            "-c",
            "user.name=Orna Test",
            "-c",
            "user.email=orna-test@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "fixture",
        ],
    );
}

#[test]
fn init_directory_creates_a_git_repository_and_persists_one_uuid_identity() {
    let fixture = tempfile::tempdir().expect("temporary fixture");
    let target = fixture.path().join("project");

    let first = run_init(fixture.path(), Some(&target));
    assert_success(&first, fixture.path());
    assert!(target.join(".git").is_dir(), "Git metadata exists");
    assert!(target.join("main.orna").is_file(), "main source exists");
    assert!(
        bytes(&target.join("main.orna")).is_empty(),
        "new main source is empty"
    );
    assert!(target.join(FORMAT).is_file(), "format record exists");
    assert!(target.join(DATABASE).is_file(), "database record exists");
    assert!(
        is_uuid_v4_record(&bytes(&target.join(DATABASE))),
        "database identity is UUIDv4"
    );
    git_succeeds(&target, &["rev-parse", "--is-inside-work-tree"]);

    let mut check = command();
    let check = check
        .arg("check")
        .current_dir(&target)
        .output()
        .expect("CLI process starts");
    assert!(check.status.success(), "fresh repository passes CLI check");
    assert!(
        check.stdout == b"project valid\n" && check.stderr.is_empty(),
        "check emits only its stable success status"
    );
    assert_fixture_path_absent(&check, fixture.path());

    let identity = bytes(&target.join(DATABASE));
    let second = run_init(fixture.path(), Some(&target));
    assert_success(&second, fixture.path());
    assert_eq!(
        bytes(&target.join(DATABASE)),
        identity,
        "identity bytes persist"
    );
}

#[test]
fn init_defaults_to_current_directory_and_preserves_existing_source() {
    let fixture = tempfile::tempdir().expect("temporary fixture");
    let source = b"pub fn preserved(): Int = 42;\n";
    fs::write(fixture.path().join("main.orna"), source).expect("fixture source written");

    let output = run_init(fixture.path(), None);
    assert_success(&output, fixture.path());
    assert!(fixture.path().join(".git").is_dir(), "Git metadata exists");
    assert_eq!(
        bytes(&fixture.path().join("main.orna")),
        source.to_vec(),
        "source is preserved"
    );
}

#[test]
fn init_accepts_a_dash_prefixed_directory_after_the_option_terminator() {
    let fixture = tempfile::tempdir().expect("temporary fixture");
    let output = command()
        .args(["init", "--", "--bare"])
        .current_dir(fixture.path())
        .output()
        .expect("CLI process starts");
    assert!(output.status.success(), "literal directory is initialized");
    assert_eq!(output.stdout, b"initialized Orna repository\n");
    assert!(output.stderr.is_empty());
    let target = fixture.path().join("--bare");
    assert!(target.join(".git").is_dir());
    assert!(target.join(DATABASE).is_file());
    assert_eq!(
        git(&target, &["rev-parse", "--is-bare-repository"]).stdout,
        b"false\n"
    );
}

#[test]
fn malformed_or_partial_metadata_fails_without_changing_repository_state() {
    for partial in [false, true] {
        let fixture = initialized_repository();
        commit_fixture_source(fixture.path(), b"pub fn preserved(): Int = 42;\n");
        let format = fixture.path().join(FORMAT);
        let database = fixture.path().join(DATABASE);
        if partial {
            fs::remove_file(&database).expect("database record removed for partial fixture");
        } else {
            fs::write(&format, b"not an Orna record\n").expect("format record corrupted");
        }
        let root_marker = fixture.path().join(".orna/root-marker");
        fs::write(&root_marker, b"root marker bytes").expect("metadata root marker written");

        let before_format = bytes(&format);
        let before_database = (!partial).then(|| bytes(&database));
        let before_source = bytes(&fixture.path().join("main.orna"));
        let before_index = bytes(&fixture.path().join(".git/index"));
        let before_head = bytes(&fixture.path().join(".git/HEAD"));
        let before_root_entries = directory_entries(&fixture.path().join(".orna"));

        let output = run_init(fixture.path(), None);
        let code = if partial {
            "error[ORNA-REPO-INIT-005]"
        } else {
            "error[ORNA-REPO-INIT-006]"
        };
        assert_failure(&output, fixture.path(), code);
        assert_eq!(bytes(&format), before_format, "format bytes are unchanged");
        if partial {
            assert!(
                !database.exists(),
                "missing database record remains missing"
            );
        } else {
            assert_eq!(
                bytes(&database),
                before_database.expect("complete fixture has database bytes"),
                "database bytes are unchanged"
            );
        }
        assert_eq!(
            bytes(&root_marker),
            b"root marker bytes".to_vec(),
            "metadata root bytes are unchanged"
        );
        assert_eq!(
            bytes(&fixture.path().join("main.orna")),
            before_source,
            "source bytes are unchanged"
        );
        assert_eq!(
            bytes(&fixture.path().join(".git/index")),
            before_index,
            "Git index bytes are unchanged"
        );
        assert_eq!(
            bytes(&fixture.path().join(".git/HEAD")),
            before_head,
            "Git HEAD bytes are unchanged"
        );
        assert_eq!(
            directory_entries(&fixture.path().join(".orna")),
            before_root_entries,
            "metadata root entries are unchanged"
        );
    }
}

#[test]
fn init_rejects_explicit_database_endpoint_without_creating_the_target() {
    let fixture = tempfile::tempdir().expect("temporary fixture");
    let target = fixture.path().join("target");
    let mut invocation = command();
    let output = invocation
        .arg("--db")
        .arg(&target)
        .arg("init")
        .current_dir(fixture.path())
        .output()
        .expect("CLI process starts");

    assert_failure(&output, fixture.path(), "error[E1002]");
    assert!(!target.exists(), "rejected command creates no target");
}

#[cfg(unix)]
#[test]
fn init_rejects_symlinked_metadata_or_source_without_touching_targets() {
    use std::os::unix::fs::symlink;

    let fixture = tempfile::tempdir().expect("temporary fixture");
    let metadata_target = fixture.path().join("metadata-target");
    fs::create_dir(&metadata_target).expect("metadata target directory");
    let metadata_marker = metadata_target.join("marker");
    fs::write(&metadata_marker, b"metadata target bytes").expect("metadata marker written");
    let metadata_project = fixture.path().join("metadata-project");
    fs::create_dir(&metadata_project).expect("metadata project directory");
    symlink(&metadata_target, metadata_project.join(".orna")).expect("metadata symlink created");

    let metadata_output = run_init(fixture.path(), Some(&metadata_project));
    assert_failure(
        &metadata_output,
        fixture.path(),
        "error[ORNA-REPO-INIT-004]",
    );
    assert_eq!(
        bytes(&metadata_marker),
        b"metadata target bytes".to_vec(),
        "metadata target is untouched"
    );

    let source_target = fixture.path().join("source-target.orna");
    fs::write(&source_target, b"source target bytes").expect("source target written");
    let source_project = fixture.path().join("source-project");
    fs::create_dir(&source_project).expect("source project directory");
    symlink(&source_target, source_project.join("main.orna")).expect("source symlink created");

    let source_output = run_init(fixture.path(), Some(&source_project));
    assert_failure(&source_output, fixture.path(), "error[ORNA-REPO-INIT-004]");
    assert_eq!(
        bytes(&source_target),
        b"source target bytes".to_vec(),
        "source target is untouched"
    );
    assert!(
        !source_project.join(".orna").exists(),
        "source symlink fails before metadata creation"
    );
}

#[test]
fn successful_reinitialization_preserves_git_and_metadata_state() {
    let fixture = initialized_repository();
    commit_fixture_source(fixture.path(), b"pub fn version(): Int = 1;\n");
    fs::write(
        fixture.path().join("main.orna"),
        b"pub fn version(): Int = 2;\n",
    )
    .expect("unstaged source written");

    let before_head = git(fixture.path(), &["rev-parse", "HEAD"]).stdout;
    let before_index = bytes(&fixture.path().join(".git/index"));
    let before_format = bytes(&fixture.path().join(FORMAT));
    let before_database = bytes(&fixture.path().join(DATABASE));
    let before_source = bytes(&fixture.path().join("main.orna"));

    let output = run_init(fixture.path(), None);
    assert_success(&output, fixture.path());
    assert_eq!(
        git(fixture.path(), &["rev-parse", "HEAD"]).stdout,
        before_head,
        "HEAD commit identity is unchanged"
    );
    assert_eq!(
        bytes(&fixture.path().join(".git/index")),
        before_index,
        "Git index bytes are unchanged"
    );
    assert_eq!(
        bytes(&fixture.path().join(FORMAT)),
        before_format,
        "format bytes are unchanged"
    );
    assert_eq!(
        bytes(&fixture.path().join(DATABASE)),
        before_database,
        "database identity bytes are unchanged"
    );
    assert_eq!(
        bytes(&fixture.path().join("main.orna")),
        before_source,
        "unstaged source bytes are unchanged"
    );
}

#[test]
fn init_honours_the_configured_git_initial_branch() {
    let fixture = tempfile::tempdir().expect("temporary fixture");
    let target = fixture.path().join("configured-branch");
    let mut invocation = command();
    let output = invocation
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "init.defaultBranch")
        .env("GIT_CONFIG_VALUE_0", "custom-init-branch")
        .arg("init")
        .arg(&target)
        .current_dir(fixture.path())
        .output()
        .expect("CLI process starts");

    assert_success(&output, fixture.path());
    assert_eq!(
        git(&target, &["symbolic-ref", "--short", "HEAD"]).stdout,
        b"custom-init-branch\n",
        "configured initial branch is retained"
    );
}

#[test]
fn init_ignores_hostile_git_routing_variables() {
    let fixture = tempfile::tempdir().expect("temporary fixture");
    let foreign = fixture.path().join("foreign");
    fs::create_dir(&foreign).expect("foreign repository directory");
    git_succeeds(&foreign, &["init", "--quiet"]);
    fs::write(
        foreign.join("tracked.orna"),
        b"pub fn foreign(): Int = 7;\n",
    )
    .expect("foreign source written");
    git_succeeds(&foreign, &["add", "tracked.orna"]);
    git_succeeds(
        &foreign,
        &[
            "-c",
            "user.name=Orna Test",
            "-c",
            "user.email=orna-test@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "foreign fixture",
        ],
    );
    let foreign_head = bytes(&foreign.join(".git/HEAD"));
    let foreign_index = bytes(&foreign.join(".git/index"));
    let foreign_source = bytes(&foreign.join("tracked.orna"));
    let target = fixture.path().join("target");

    let mut invocation = command();
    let output = invocation
        .env("GIT_DIR", foreign.join(".git"))
        .env("GIT_WORK_TREE", &foreign)
        .env("GIT_INDEX_FILE", foreign.join(".git/index"))
        .arg("init")
        .arg(&target)
        .current_dir(fixture.path())
        .output()
        .expect("CLI process starts");

    assert_success(&output, fixture.path());
    assert!(target.join(".git").is_dir(), "target Git metadata exists");
    assert_eq!(
        bytes(&foreign.join(".git/HEAD")),
        foreign_head,
        "foreign Git HEAD bytes are unchanged"
    );
    assert_eq!(
        bytes(&foreign.join(".git/index")),
        foreign_index,
        "foreign Git index bytes are unchanged"
    );
    assert_eq!(
        bytes(&foreign.join("tracked.orna")),
        foreign_source,
        "foreign source bytes are unchanged"
    );
}

#[test]
fn init_then_check_accepts_unchanged_authoritative_reference_sources() {
    let fixture = tempfile::tempdir().expect("temporary reference project");
    let reference = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../reference/Orna-1.0.0/examples/reference");
    let names = [
        "main.orna",
        "library.orna",
        "warehouse.orna",
        "sensors.orna",
        "values.orna",
    ];
    let original = names
        .iter()
        .map(|name| {
            let source = bytes(&reference.join(name));
            fs::write(fixture.path().join(name), &source).expect("reference source copied");
            (*name, source)
        })
        .collect::<Vec<_>>();

    let initialized = run_init(fixture.path(), None);
    assert_success(&initialized, fixture.path());
    for (name, source) in original {
        assert_eq!(
            bytes(&fixture.path().join(name)),
            source,
            "reference source bytes are unchanged"
        );
    }

    let mut check = command();
    let check = check
        .arg("check")
        .current_dir(fixture.path())
        .output()
        .expect("CLI process starts");
    assert!(
        check.status.success(),
        "authoritative reference project passes CLI check"
    );
    assert!(
        check.stdout == b"project valid\n" && check.stderr.is_empty(),
        "check emits only its stable success status"
    );
    assert_fixture_path_absent(&check, fixture.path());
}
