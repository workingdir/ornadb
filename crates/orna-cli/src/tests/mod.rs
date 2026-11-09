use super::*;
use crate::cli_args::parse_cli;
use orna_runtime::{Mutation, NoFault, RunObservationStatus, RuntimeState};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
#[test]
fn color_mode_respects_stream_tty_and_explicit_overrides() {
    assert!(!ColorMode::Auto.enabled(false));
    assert!(ColorMode::Auto.enabled(true));
    assert!(ColorMode::Always.enabled(false));
    assert!(ColorMode::Always.enabled(true));
    assert!(!ColorMode::Never.enabled(false));
    assert!(!ColorMode::Never.enabled(true));
}

#[test]
fn fetch_accepts_only_continuous_witnesses() {
    assert!(fetch_continuity_diagnostic(orna_repository::RemoteContinuity::Continuous).is_none());

    for continuity in [
        orna_repository::RemoteContinuity::Missing,
        orna_repository::RemoteContinuity::Stale,
        orna_repository::RemoteContinuity::InvalidEvidence,
        orna_repository::RemoteContinuity::Unverifiable,
    ] {
        let diagnostic =
            fetch_continuity_diagnostic(continuity).expect("non-continuous witnesses are rejected");
        assert_eq!(diagnostic.code, "E2100");
        assert_eq!(diagnostic.title, "Orna continuity witness was rejected");
        assert_eq!(
            diagnostic.help,
            "do not claim allocator or checkpoint continuity until required refs/orna witnesses match"
        );
        assert_eq!(
            diagnostic.detail.as_deref(),
            Some(format!("remote continuity state: {continuity:?}").as_str())
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one table-driven behavioral scenario documents stable CLI parse precedence"
)]
fn parsing_is_bounded_and_diagnostic_is_stable() {
    let parsed = parse_cli(&[
        "--db".into(),
        "orna://host/reference".into(),
        "run".into(),
        "seed".into(),
    ])
    .expect("parses");
    assert_eq!(parsed.command, Command::Run(Invocation::Seed));
    assert_eq!(
        parsed.endpoint,
        Endpoint::RemoteTls("orna://host/reference".into())
    );
    assert_eq!(
        parse_cli(&["status".into()])
            .expect("default color mode parses")
            .color,
        ColorMode::Auto
    );
    let colored = parse_cli(&[
        "--color".into(),
        "always".into(),
        "--db".into(),
        "project".into(),
        "status".into(),
    ])
    .expect("global color and database options parse before the command");
    assert_eq!(colored.color, ColorMode::Always);
    assert_eq!(colored.endpoint, Endpoint::Path("project".into()));
    assert_eq!(
        parse_cli(&["--color".into(), "never".into(), "status".into()])
            .expect("never mode parses")
            .color,
        ColorMode::Never
    );
    let error = parse_cli(&["--color".into(), "invalid".into(), "status".into()])
        .expect_err("invalid color mode is rejected");
    assert_eq!(
        (error.code, error.title.as_ref(), error.help),
        (
            "E1002",
            "unsupported value for `--color`",
            "choose `auto`, `always`, or `never`"
        )
    );
    let error = parse_cli(&["--color".into()]).expect_err("color mode value is required");
    assert_eq!(
        (error.code, error.title.as_ref()),
        ("E1001", "option `--color` needs a value")
    );
    assert_eq!(
        parse_cli(&["check".into()]).unwrap().command,
        Command::Check
    );
    assert_eq!(
        parse_cli(&["invoke".into(), "library.value".into()])
            .expect("parses")
            .command,
        Command::Invoke("library.value".into())
    );
    assert_eq!(
        parse_cli(&["repl".into(), "1 + 2".into()])
            .expect("parses")
            .command,
        Command::Repl(Some("1 + 2".into()))
    );
    assert_eq!(
        parse_cli(&["status".into(), "--porcelain".into()])
            .expect("parses")
            .command,
        Command::Status {
            format: StatusFormat::Porcelain,
        }
    );
    assert_eq!(
        parse_cli(&["status".into()])
            .expect("human status parses")
            .command,
        Command::Status {
            format: StatusFormat::Human,
        }
    );
    assert_eq!(
        parse_cli(&["status".into(), "--short".into()])
            .expect("short status parses")
            .command,
        Command::Status {
            format: StatusFormat::Short,
        }
    );
    assert_eq!(
        parse_cli(&["status".into(), "--format".into(), "json".into()])
            .expect("JSON status parses")
            .command,
        Command::Status {
            format: StatusFormat::Json,
        }
    );
    let error =
        parse_cli(&["status".into(), "--porcelain=v2".into()]).expect_err("status format is exact");
    assert_eq!(error.code, "E1002");
    assert_eq!(
        (error.title.as_ref(), error.help),
        (
            "unsupported `status` option or option placement",
            "use `status`, `status --porcelain`, `status --short`, `status --format human|short|json`, or `--format human|short|json status`"
        )
    );
    assert_eq!(
        parse_cli(&["init".into()]).expect("parses").command,
        Command::Init(None)
    );
    assert_eq!(
        parse_cli(&["init".into(), "project".into()])
            .expect("parses")
            .command,
        Command::Init(Some(PathBuf::from("project")))
    );
    assert_eq!(
        parse_cli(&["init".into(), "--".into(), "--bare".into()])
            .expect("literal target parses")
            .command,
        Command::Init(Some(PathBuf::from("--bare")))
    );
    assert_eq!(
        parse_cli(&["--db".into(), "project".into(), "init".into()])
            .expect_err("init must not use a database endpoint")
            .code,
        "E1002"
    );
    assert_eq!(
        parse_cli(&["init".into(), "--bare".into()])
            .expect_err("unsupported init option")
            .code,
        "E1002"
    );
    assert_eq!(
        parse_cli(&["init".into(), "one".into(), "two".into()])
            .expect_err("only one target is accepted")
            .code,
        "E1003"
    );
    assert_eq!(
        parse_cli(&["invoke".into()])
            .expect_err("target is required")
            .code,
        "E1001"
    );
    assert_eq!(
        parse_cli(&["serve".into()]).unwrap().command,
        Command::Serve {
            port: 8080,
            allow_push: false
        }
    );
    assert_eq!(
        parse_cli(&["serve".into(), "--port".into(), "0".into()])
            .unwrap()
            .command,
        Command::Serve {
            port: 0,
            allow_push: false
        }
    );
}

#[test]
fn explain_documents_a_stable_diagnostic_and_rejects_unknown_codes() {
    assert_eq!(
        parse_cli(&["explain".into(), "ORNA-S010-IMPORT".into()])
            .expect("explain parses")
            .command,
        Command::Explain("ORNA-S010-IMPORT".into())
    );
    assert_eq!(
        explain_diagnostic("ORNA-S010-IMPORT").expect("known diagnostic"),
        "ORNA-S010-IMPORT: imported module is unavailable\nThe project names a module that is not part of its captured dependencies, so the module cannot be checked or executed.\nhelp: use a captured standard dependency or remove the import"
    );
    let error = explain_diagnostic("ORNA-NOT-A-CODE").expect_err("unknown diagnostic");
    assert_eq!((error.code, error.exit), ("E1002", Exit::Usage));
}

#[test]
fn semantic_legend_unknown_class_or_option_exits_with_usage_code_two() {
    for words in [
        &["semantic-legend", "--class", "string"][..],
        &["semantic-legend", "--format", "xml"][..],
        &["semantic-legend", "--limit", "0"][..],
    ] {
        let arguments = words
            .iter()
            .map(|word| (*word).to_owned())
            .collect::<Vec<_>>();
        match parse_cli(&arguments) {
            Ok(_) => panic!("{words:?} should be rejected"),
            Err(error) => {
                assert_eq!((error.code, error.exit as i32), ("E1002", 2), "{words:?}");
            }
        }
    }
}

#[test]
fn explain_documents_frozen_assertion_and_syntax_diagnostics() {
    assert_eq!(
        explain_diagnostic("ORNA-A091-001").expect("assertion diagnostic is documented"),
        "ORNA-A091-001: refined type uses the legacy `where self` form\nRefined Orna types state their invariant in a brace-delimited assertion block owned by the type.\nhelp: replace `where self` with a brace-delimited `assert` block"
    );
    for code in [
        "ORNA-A091-002",
        "ORNA-A091-003",
        "ORNA-A091-004",
        "ORNA-A091-005",
        "ORNA-A091-006",
        "ORNA-A091-007",
        "ORNA-A091-008",
        "ORNA-A091-009",
        "ORNA-A091-010",
        "ORNA-A091-011",
        "ORNA-A091-012",
        "ORNA091-E-MATCH",
        "ORNA091-E-RESULT",
        "ORNA091-E-POSTFIX-QUESTION",
        "ORNA091-E-CURRENCY",
        "ORNA091-E-CURRENCY-SYMBOL",
        "ORNA091-E-IMPL-FOR",
        "ORNA091-E-BOUND-COLON",
        "ORNA091-E-STATIC-FN",
        "ORNA091-E-TRYFROM",
        "ORNA091-E-CONVERSION-CHAIN",
        "ORNA091-E-OPAQUE",
        "ORNA091-E-FIELD-CONSTRAINT",
    ] {
        assert!(
            explain_diagnostic(code).is_ok(),
            "frozen diagnostic {code} must have user guidance"
        );
    }
}

/// ORNA-CLI-006: supported terminals receive safe OSC 8 status-path hyperlinks.
#[test]
#[allow(clippy::too_many_lines)]
fn human_status_links_safe_paths_only_for_supported_terminal_capabilities() {
    const FIXTURE: &str = include_str!("../../tests/fixtures/function-expression.orna");
    let directory = tempfile::tempdir().expect("status repository");
    assert!(
        std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(directory.path())
            .status()
            .expect("git init")
            .success()
    );
    for name in ["function-expression.orna", "function expression#?.orna"] {
        std::fs::write(directory.path().join(name), FIXTURE).expect("fixture source");
    }

    let human_output = std::process::Command::new("git")
        .args([
            "-c",
            "color.status=false",
            "status",
            "--untracked-files=all",
        ])
        .current_dir(directory.path())
        .output()
        .expect("human Git status");
    assert!(human_output.status.success());
    let root = directory
        .path()
        .canonicalize()
        .expect("absolute repository root");
    let mut without_links = Vec::new();
    write_human_status(&human_output.stdout, &mut without_links, true, None)
        .expect("unlinked human status");
    let mut linked = Vec::new();
    write_human_status(&human_output.stdout, &mut linked, true, Some(&root))
        .expect("linked human status");

    let linked_text = std::str::from_utf8(&linked).expect("UTF-8 human status");
    for (name, encoded_suffix) in [
        ("function-expression.orna", "/function-expression.orna"),
        (
            "function expression#?.orna",
            "/function%20expression%23%3F.orna",
        ),
    ] {
        let url = file_url_for_status_path(&root, name.as_bytes()).expect("safe file URL");
        assert!(url.starts_with("file:///"));
        assert!(url.ends_with(encoded_suffix), "{url}");
        assert!(
            linked_text.contains(&format!("\x1b]8;;{url}\x1b\\")),
            "missing OSC 8 URL for {name:?}"
        );
        assert!(linked_text.contains(name), "missing visible path {name:?}");
    }
    assert_eq!(visible_without_osc8(&linked), without_links);
    assert!(file_url_for_status_path(&root, b"\"quoted path\"").is_none());
    assert!(file_url_for_status_path(&root, b"control\npath").is_none());
    assert!(file_url_for_status_path(&root, b"../outside").is_none());
    assert!(file_url_for_status_path(&root, &[0xff]).is_none());

    assert!(osc8_terminal_supports(
        Some("xterm-kitty"),
        None,
        None,
        None,
        None,
        false,
    ));
    assert!(!osc8_terminal_supports(
        Some("xterm-256color"),
        None,
        None,
        None,
        None,
        false,
    ));
    assert!(!osc8_terminal_supports(
        Some("xterm-kitty"),
        None,
        None,
        None,
        None,
        true,
    ));
    assert!(osc8_terminal_supports(
        None,
        Some("iTerm.app"),
        Some("3.1.0"),
        None,
        None,
        false,
    ));
    assert!(!osc8_terminal_supports(
        None,
        Some("iTerm.app"),
        Some("3.0.9"),
        None,
        None,
        false,
    ));

    let short_output = std::process::Command::new("git")
        .args(["status", "--short", "--untracked-files=all"])
        .current_dir(directory.path())
        .output()
        .expect("short Git status");
    assert!(short_output.status.success());
    let mut rendered_short = Vec::new();
    write_short_status(&short_output.stdout, &mut rendered_short, false)
        .expect("rendered short status");
    assert_eq!(rendered_short, short_output.stdout);
}

#[test]
fn unpublished_status_summary_reports_zero_and_remaining_runtime_mutations() {
    let mut rendered = Vec::new();
    write_unpublished_summary(&mut rendered, 0).expect("empty unpublished summary");
    write_unpublished_summary(&mut rendered, 1).expect("singular unpublished summary");
    write_unpublished_summary(&mut rendered, 2).expect("plural unpublished summary");
    assert_eq!(
            rendered,
            b"Unpublished CWD changes: 0 runtime mutations\nUnpublished CWD changes: 1 runtime mutation\nUnpublished CWD changes: 2 runtime mutations\n"
        );
}

#[test]
fn json_status_includes_unpublished_runtime_mutation_count() {
    assert_eq!(
        append_unpublished_count_json(b"{\"branch\":\"main\",\"changes\":[]}\n".to_vec(), 4,)
            .expect("JSON status terminator"),
        b"{\"branch\":\"main\",\"changes\":[],\"unpublished_runtime_mutations\":4}\n"
    );
    assert!(append_unpublished_count_json(b"{}".to_vec(), 0).is_none());
}

#[test]
fn unpublished_mutation_count_reads_the_runtime_pending_tail() {
    let directory = tempfile::tempdir().expect("status repository");
    assert!(
        std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(directory.path())
            .status()
            .expect("git init")
            .success()
    );
    orna_repository::initialize_repository(directory.path()).expect("initialize Orna repository");
    let repository = orna_repository::Repository::discover(directory.path())
        .expect("discover initialized repository");
    let (identity, initial_digest) = runtime_identity(&repository).expect("runtime identity");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime executor");
    let state = runtime
        .block_on(RuntimeState::open(&repository, identity, initial_digest))
        .expect("open runtime state");
    assert_eq!(unpublished_mutation_count(&repository), Ok(0));

    let payload = b"pending table mutation".to_vec();
    let mutation = Mutation {
        id: [1; 16],
        digest: Sha256::digest(&payload).into(),
        payload,
    };
    runtime
        .block_on(async {
            let lease = state.acquire_lease(identity.repository_id).await?;
            let capture = state.capture().await?;
            state
                .commit(lease, &capture, &mutation, [2; 32], &NoFault)
                .await
        })
        .expect("append durable runtime mutation");
    drop(state);

    assert_eq!(unpublished_mutation_count(&repository), Ok(1));
}

fn visible_without_osc8(output: &[u8]) -> Vec<u8> {
    let mut visible = Vec::with_capacity(output.len());
    let mut remaining = output;
    while let Some(start) = remaining.windows(4).position(|window| window == b"\x1b]8;") {
        visible.extend_from_slice(&remaining[..start]);
        let link = &remaining[start..];
        let end = link
            .windows(2)
            .position(|window| window == b"\x1b\\")
            .expect("OSC 8 terminator");
        remaining = &link[end + 2..];
    }
    visible.extend_from_slice(remaining);
    visible
}

#[test]
fn bare_status_dispatches_to_git_human_status() {
    let directory = tempfile::tempdir().expect("status repository");
    assert!(
        std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(directory.path())
            .status()
            .expect("git init")
            .success()
    );
    let parsed = parse_cli(&["status".into()]).expect("human status parses");
    assert_eq!(
        execute(&Parsed {
            endpoint: Endpoint::Path(directory.path().to_string_lossy().into_owned()),
            command: parsed.command,
            color: ColorMode::Auto,
        }),
        Ok(())
    );
}

#[test]
fn project_repl_factory_retains_imports_in_a_scripted_session() {
    let directory = tempfile::tempdir().expect("temporary project");
    std::fs::write(
        directory.path().join("main.orna"),
        "use library; pub fn run(): Int = library.twice(21);",
    )
    .expect("main source");
    std::fs::write(
        directory.path().join("library.orna"),
        "pub fn twice(value: Int): Int = value + value; fn hidden(value: Int): Int = value;",
    )
    .expect("library source");
    assert!(
        std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(directory.path())
            .status()
            .expect("git")
            .success()
    );

    let endpoint = Endpoint::Path(directory.path().to_string_lossy().into_owned());
    let mut runtime = repl_session(&endpoint).expect("project session");
    let mut input =
        b"use library;\nlet n: Int = 21;\nlibrary.hidden(n)\nlibrary.twice(n)\n:quit\n".as_slice();
    let mut output = Vec::new();
    repl::run(&mut input, &mut output, &mut runtime.session).expect("scripted session");
    let output = String::from_utf8(output).expect("UTF-8");
    assert!(output.contains("error[ORNA-S012-UNRESOLVED]: "), "{output}");
    assert!(output.contains("hidden"), "{output}");
    assert!(output.contains("line 1, column 9"), "{output}");
    assert!(output.contains("> 42 : Int\n> "), "{output}");
}

#[test]
fn project_repl_rejects_an_uncaptured_standard_import() {
    let directory = tempfile::tempdir().expect("temporary project");
    std::fs::write(
        directory.path().join("main.orna"),
        include_str!("../../tests/fixtures/reference-project/page.orna"),
    )
    .expect("accepted source");
    assert!(
        std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(directory.path())
            .status()
            .expect("git")
            .success()
    );

    let endpoint = Endpoint::Path(directory.path().to_string_lossy().into_owned());
    let error = repl_session(&endpoint)
        .err()
        .expect("uncaptured standard module");
    assert_eq!(error.code, "ORNA-S010-IMPORT");
    assert_eq!(error.exit, Exit::Target);
    assert_eq!(error.title, "imported module is unavailable");
    assert_eq!(
        error.help,
        "use a captured standard dependency or remove the import"
    );
}

#[test]
fn repl_repository_authority_is_reused_for_cwd_head_and_named_refs_without_git_mutation() {
    let directory = tempfile::tempdir().expect("temporary project");
    assert!(
        std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(directory.path())
            .status()
            .expect("git init")
            .success()
    );
    std::fs::write(directory.path().join("main.orna"), "use library;").expect("committed source");
    std::fs::write(
        directory.path().join("library.orna"),
        "pub fn answer(): Int = 42;",
    )
    .expect("committed library source");
    assert!(
        std::process::Command::new("git")
            .args(["add", "main.orna", "library.orna"])
            .current_dir(directory.path())
            .status()
            .expect("git add")
            .success()
    );
    assert!(
        std::process::Command::new("git")
            .args([
                "-c",
                "user.email=kieran@drewett.dev",
                "-c",
                "user.name=kierandrewett",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--quiet",
                "-m",
                "snapshot",
            ])
            .current_dir(directory.path())
            .status()
            .expect("git commit")
            .success()
    );
    assert!(
        std::process::Command::new("git")
            .args(["branch", "snapshot"])
            .current_dir(directory.path())
            .status()
            .expect("git branch")
            .success()
    );
    std::fs::write(
        directory.path().join("library.orna"),
        "pub fn answer(): Int = 7;",
    )
    .expect("CWD source");
    let status = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(directory.path())
        .output()
        .expect("Git status before snapshot reads");

    let endpoint = Endpoint::Path(directory.path().to_string_lossy().into_owned());
    let runtime = repl_session(&endpoint).expect("project REPL runtime");
    let repository = runtime
        .repository
        .as_ref()
        .expect("opened project repository remains session authority");
    assert_eq!(repository.worktree(), directory.path());
    let loader = ReplSnapshotSessionLoader {
        repository: Some(repository),
        host_bindings: Rc::new(RefCell::new(None)),
    };
    for (target, expected) in [
        (repl::SnapshotTarget::Cwd, "7 : Int"),
        (repl::SnapshotTarget::Head, "42 : Int"),
        (repl::SnapshotTarget::Ref("snapshot".into()), "42 : Int"),
    ] {
        let mut session = repl::SnapshotSessionLoader::load_snapshot(&loader, target)
            .expect("snapshot project is admitted");
        let imported = session.submit("use library;");
        assert!(imported.is_ok(), "snapshot import failed: {imported:?}");
        let value = session
            .submit("library.answer()")
            .expect("snapshot function evaluates")
            .expect("visible result");
        assert_eq!(repl::inspect(&value), expected);
    }

    let after = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(directory.path())
        .output()
        .expect("Git status after snapshot reads");
    assert_eq!(after.stdout, status.stdout);
    assert_eq!(after.stderr, status.stderr);
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the regression preserves every Git observation needed to prove read-only snapshot selection"
)]
fn repl_snapshot_loader_keeps_a_selected_ref_stable_after_it_moves() {
    let directory = tempfile::tempdir().expect("temporary project");
    assert!(
        std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(directory.path())
            .status()
            .expect("git init")
            .success()
    );
    std::fs::write(directory.path().join("main.orna"), "use library;")
        .expect("initial main source");
    std::fs::write(
        directory.path().join("library.orna"),
        "pub fn answer(): Int = 42;",
    )
    .expect("initial library source");
    assert!(
        std::process::Command::new("git")
            .args(["add", "main.orna", "library.orna"])
            .current_dir(directory.path())
            .status()
            .expect("git add")
            .success()
    );
    assert!(
        std::process::Command::new("git")
            .args([
                "-c",
                "user.email=orna@example.test",
                "-c",
                "user.name=Orna Test",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--quiet",
                "-m",
                "selected snapshot",
            ])
            .current_dir(directory.path())
            .status()
            .expect("initial git commit")
            .success()
    );
    assert!(
        std::process::Command::new("git")
            .args(["branch", "snapshot"])
            .current_dir(directory.path())
            .status()
            .expect("snapshot branch")
            .success()
    );
    std::fs::write(
        directory.path().join("library.orna"),
        "pub fn answer(): Int = 7;",
    )
    .expect("advanced library source");
    assert!(
        std::process::Command::new("git")
            .args(["add", "library.orna"])
            .current_dir(directory.path())
            .status()
            .expect("git add advanced source")
            .success()
    );
    assert!(
        std::process::Command::new("git")
            .args([
                "-c",
                "user.email=orna@example.test",
                "-c",
                "user.name=Orna Test",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--quiet",
                "-m",
                "advanced source",
            ])
            .current_dir(directory.path())
            .status()
            .expect("advanced git commit")
            .success()
    );

    let git_output = |arguments: &[&str]| {
        std::process::Command::new("git")
            .args(arguments)
            .current_dir(directory.path())
            .output()
            .expect("git output")
    };
    let selected_ref = git_output(&["rev-parse", "snapshot"]);
    assert!(selected_ref.status.success());
    let head = git_output(&["rev-parse", "HEAD"]);
    assert!(head.status.success());
    assert_ne!(selected_ref.stdout, head.stdout);
    let symbolic_head = git_output(&["symbolic-ref", "--quiet", "HEAD"]);
    assert!(symbolic_head.status.success());
    let index_path = git_output(&["rev-parse", "--git-path", "index"]);
    assert!(index_path.status.success());
    let index_path = directory.path().join(
        std::str::from_utf8(&index_path.stdout)
            .expect("UTF-8 index path")
            .trim(),
    );
    let index = std::fs::read(&index_path).expect("index before snapshot selection");
    let main = std::fs::read(directory.path().join("main.orna")).expect("main before selection");
    let library =
        std::fs::read(directory.path().join("library.orna")).expect("library before selection");
    let status = git_output(&["status", "--porcelain=v1", "-z"]);
    assert!(status.status.success());

    let endpoint = Endpoint::Path(directory.path().to_string_lossy().into_owned());
    let runtime = repl_session(&endpoint).expect("project REPL runtime");
    let repository = runtime
        .repository
        .as_ref()
        .expect("opened project repository remains session authority");
    let host_bindings = Rc::new(RefCell::new(Some(SysHostBindingRegistry::new(
        EnvironmentProvider::default(),
    ))));
    let loader = ReplSnapshotSessionLoader {
        repository: Some(repository),
        host_bindings: host_bindings.clone(),
    };
    let mut session = repl::SnapshotSessionLoader::load_snapshot(
        &loader,
        repl::SnapshotTarget::Ref("snapshot".into()),
    )
    .expect("selected snapshot project is admitted");
    assert!(host_bindings.borrow().is_none());

    assert!(
        std::process::Command::new("git")
            .args(["branch", "--force", "snapshot", "HEAD"])
            .current_dir(directory.path())
            .status()
            .expect("move selected branch")
            .success()
    );
    let moved_ref = git_output(&["rev-parse", "snapshot"]);
    assert!(moved_ref.status.success());
    assert_eq!(moved_ref.stdout, head.stdout);

    session
        .submit("use library;")
        .expect("selected import succeeds");
    let value = session
        .submit("library.answer()")
        .expect("selected function evaluates")
        .expect("visible result");
    assert_eq!(repl::inspect(&value), "42 : Int");

    let after_head = git_output(&["rev-parse", "HEAD"]);
    let after_symbolic_head = git_output(&["symbolic-ref", "--quiet", "HEAD"]);
    let after_status = git_output(&["status", "--porcelain=v1", "-z"]);
    assert_eq!(after_head.stdout, head.stdout);
    assert_eq!(after_symbolic_head.stdout, symbolic_head.stdout);
    assert_eq!(
        std::fs::read(&index_path).expect("index after selection"),
        index
    );
    assert_eq!(
        std::fs::read(directory.path().join("main.orna")).expect("main after selection"),
        main
    );
    assert_eq!(
        std::fs::read(directory.path().join("library.orna")).expect("library after selection"),
        library
    );
    assert_eq!(after_status.stdout, status.stdout);
    assert_eq!(after_status.stderr, status.stderr);
}

#[test]
fn repl_snapshot_loader_rejects_non_local_endpoints_as_at_errors() {
    let endpoint = Endpoint::RemoteTls("orna://host/project".into());
    let mut runtime = repl_session(&endpoint).expect("core-only remote REPL session");
    let loader = ReplSnapshotSessionLoader {
        repository: runtime.repository.as_ref(),
        host_bindings: Rc::new(RefCell::new(None)),
    };
    let mut input = b":at HEAD\n:at snapshot\n:quit\n".as_slice();
    let mut output = Vec::new();

    repl::run_with_snapshot_loader(&mut input, &mut output, &mut runtime.session, &loader)
        .expect("REPL runs");

    assert_eq!(
        String::from_utf8(output).expect("UTF-8"),
        "> error[ORNA-REPL-AT]: requested project snapshot could not be loaded\nhelp: choose an available snapshot with `:at CWD`, `:at HEAD`, or `:at ref`\n> error[ORNA-REPL-AT]: requested project snapshot could not be loaded\nhelp: choose an available snapshot with `:at CWD`, `:at HEAD`, or `:at ref`\n> "
    );
}

#[test]
fn project_source_rejects_repl_only_bindings() {
    let directory = tempfile::tempdir().expect("temporary project");
    std::fs::write(directory.path().join("main.orna"), "pub fn status() = $?;")
        .expect("main source");
    assert!(
        std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(directory.path())
            .status()
            .expect("git")
            .success()
    );

    let endpoint = Endpoint::Path(directory.path().to_string_lossy().into_owned());
    assert_eq!(
        repl_session(&endpoint)
            .err()
            .expect("module source rejects status binding")
            .code,
        "E2101"
    );
}

#[test]
fn diagnostic_output_preserves_detail_help_and_final_newline() {
    let diagnostic = Diagnostic::target_with_detail(
        "E2300",
        "request failed",
        "retry the operation",
        "the expected remote response was unavailable",
    );
    let mut plain = Vec::new();
    write_diagnostic(&mut plain, &diagnostic, false, false).expect("plain diagnostic write");
    assert_eq!(
        plain, b"error[E2300]: request failed\nhelp: retry the operation\n",
        "technical details are hidden in normal output"
    );

    let mut colored = Vec::new();
    write_diagnostic(&mut colored, &diagnostic, true, true).expect("colored diagnostic write");
    assert_eq!(
        String::from_utf8(colored).expect("diagnostic is UTF-8"),
        "\x1b[31merror\x1b[0m[\x1b[31mE2300\x1b[0m]: \x1b[31mrequest failed\x1b[0m\n  \x1b[2mthe expected remote response was unavailable\x1b[0m\nhelp: \x1b[2mretry the operation\x1b[0m\n"
    );
}

#[test]
fn durable_invocation_failure_carries_public_code_and_hides_it_without_debug() {
    let diagnostic = durable_invocation_failed(&orna_runtime::RuntimeError::AdminBusy);
    assert_eq!(diagnostic.code, "E2200");
    assert_eq!(
        diagnostic.detail.as_deref(),
        Some("runtime failure: sys.admin.busy")
    );

    let mut plain = Vec::new();
    write_diagnostic(&mut plain, &diagnostic, false, false).expect("plain write");
    let plain = String::from_utf8(plain).expect("diagnostic is UTF-8");
    assert!(!plain.contains("sys.admin.busy"), "plain output: {plain}");
    assert!(!plain.contains("runtime failure"), "plain output: {plain}");

    let mut debug = Vec::new();
    write_diagnostic(&mut debug, &diagnostic, false, true).expect("debug write");
    let debug = String::from_utf8(debug).expect("diagnostic is UTF-8");
    assert!(
        debug.contains("runtime failure: sys.admin.busy"),
        "debug output: {debug}"
    );
}

#[test]
fn durable_invocation_failure_without_public_code_carries_display_text() {
    let diagnostic = durable_invocation_failed(&orna_runtime::RuntimeError::LeaseHeld);
    assert_eq!(diagnostic.code, "E2200");
    assert_eq!(
        diagnostic.detail.as_deref(),
        Some("runtime failure: runtime writer is held")
    );
}

#[test]
fn failure_detail_formats_attached_cause_codes() {
    let severity = orna_foundation::DiagnosticSeverity::Error;
    let message = || orna_foundation::SafeText::new("redacted").expect("safe text");
    let cause = orna_foundation::Diagnostic::new(
        orna_foundation::SafeText::new("ORNA-EVAL-DIVIDE-BY-ZERO").expect("safe code"),
        severity,
        message(),
    )
    .expect("cause diagnostic");
    let failure = orna_foundation::Diagnostic::new(
        orna_foundation::SafeText::new("ORNA-EVAL-SURFACE").expect("safe code"),
        severity,
        message(),
    )
    .expect("failure diagnostic")
    .with_cause(cause);

    let delivery = stream_delivery_failed(&failure);
    assert_eq!(delivery.code, "E2200");
    assert_eq!(
        delivery.detail.as_deref(),
        Some("failure code: ORNA-EVAL-SURFACE (causes: ORNA-EVAL-DIVIDE-BY-ZERO)")
    );
}

/// One single-expression REPL submission against a bare session, the path
/// `orna repl EXPR` takes without a project transaction authority.
fn run_repl_submission<W: io::Write>(
    session: &mut AdmittedReplSession,
    source: &str,
    writer: &mut W,
    color_enabled: bool,
) -> Result<(), Diagnostic> {
    let mut submitter = |session: &mut AdmittedReplSession, source: &str| {
        session.submit(source).map_err(repl::ReplFailure::from)
    };
    run_repl_submission_with(session, source, writer, color_enabled, &mut submitter)
}

#[test]
fn single_expression_repl_reports_visible_success_failure_and_quit() {
    let mut session = AdmittedReplSession::new(Limits::default());
    let mut output = Vec::new();
    assert_eq!(
        run_repl_submission(&mut session, "1 + 2", &mut output, false),
        Ok(())
    );
    let error = run_repl_submission(&mut session, "missing()", &mut output, false)
        .expect_err("failure is reported");
    assert_eq!(error.code, "ORNA-S012-UNRESOLVED");
    assert_eq!(error.exit, Exit::Target);
    let rendered = error.to_string();
    assert_eq!(
        rendered,
        "error[ORNA-S012-UNRESOLVED]: name `missing` cannot be resolved\n  --> line 1, column 1\n  |\n1 | missing()\n  | ^^^^^^^\nnote: `missing` is not declared in this session and is not imported\nhelp: declare `missing` before using it, for example `let missing = <value>`"
    );
    assert_eq!(
        run_repl_submission(&mut session, ":quit", &mut output, false),
        Ok(())
    );
    assert_eq!(String::from_utf8(output).expect("UTF-8"), "3 : Int\n");
}

struct BrokenWriter;

impl std::io::Write for BrokenWriter {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("writer failed"))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn single_expression_repl_converts_writer_failure_to_console_diagnostic() {
    let mut session = AdmittedReplSession::new(Limits::default());
    let error = run_repl_submission(&mut session, "1", &mut BrokenWriter, false)
        .expect_err("writer failure");
    assert_eq!((error.code, error.exit), ("E2200", Exit::Target));
}
#[test]
fn execution_project_preserves_reachable_loose_row_metadata() {
    let directory = tempfile::tempdir().expect("temporary project");
    std::fs::create_dir_all(directory.path().join("contacts/Contact")).expect("row directory");
    std::fs::write(
        directory.path().join("main.orna"),
        "use contacts.main; pub fn run() {}",
    )
    .expect("root source");
    std::fs::write(
        directory.path().join("contacts/main.orna"),
        "pub table Contact(id: Str) { name: Str, }",
    )
    .expect("table source");
    let row_source = "{ name: \"Alice Smith\" }\n";
    std::fs::write(
        directory.path().join("contacts/Contact/alice-smith.orna"),
        row_source,
    )
    .expect("row source");
    assert!(
        std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(directory.path())
            .status()
            .expect("git")
            .success()
    );

    let repository = orna_repository::Repository::discover(directory.path()).expect("repository");
    let project = orna_project::ProjectLoader::default()
        .load(&repository)
        .expect("project loads");
    let execution = execution_project(&project);
    assert_eq!(execution.loose_rows.len(), 1);
    let row = &execution.loose_rows[0];
    assert_eq!(row.source_id, "contacts/Contact/alice-smith.orna");
    assert_eq!(row.parse_as, "row_unit");
    assert_eq!(row.source.as_bytes(), row_source.as_bytes());
}

#[test]
fn check_loads_reachable_project_sources_and_ignores_unreachable_modules() {
    let directory = tempfile::tempdir().expect("temporary project");
    std::fs::write(
        directory.path().join("main.orna"),
        "use library; pub fn run() {}",
    )
    .expect("main source");
    std::fs::write(directory.path().join("library.orna"), "pub fn seed() {}").expect("library");
    std::fs::write(directory.path().join("unused.orna"), "not a module").expect("unused");
    assert!(
        std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(directory.path())
            .status()
            .expect("git")
            .success()
    );

    let endpoint = Endpoint::Path(directory.path().to_string_lossy().into_owned());
    assert_eq!(check_project(&endpoint, false), Ok(()));
}

#[test]
fn authoritative_reference_project_runs_seed_exercise_and_sensor_stream() {
    let directory = tempfile::tempdir().expect("temporary reference project");
    for (name, source) in [
        (
            "main.orna",
            include_str!("../../tests/fixtures/reference-project/main.orna"),
        ),
        (
            "library.orna",
            include_str!("../../tests/fixtures/reference-project/library.orna"),
        ),
        (
            "warehouse.orna",
            include_str!("../../tests/fixtures/reference-project/warehouse.orna"),
        ),
        (
            "sensors.orna",
            include_str!("../../tests/fixtures/reference-project/sensors.orna"),
        ),
        (
            "values.orna",
            include_str!("../../tests/fixtures/reference-project/values.orna"),
        ),
    ] {
        std::fs::write(directory.path().join(name), source).expect("reference source");
    }
    assert!(
        std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(directory.path())
            .status()
            .expect("git")
            .success()
    );
    assert!(
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(directory.path())
            .status()
            .expect("git")
            .success()
    );
    orna_repository::initialize_repository(directory.path()).expect("runtime metadata");
    let endpoint = Endpoint::Path(directory.path().display().to_string());
    assert_eq!(
        execute(&Parsed {
            endpoint: endpoint.clone(),
            command: Command::Check,
            color: ColorMode::Auto,
        }),
        Ok(())
    );
    assert_eq!(
        execute(&Parsed {
            endpoint: endpoint.clone(),
            command: Command::Run(Invocation::Seed),
            color: ColorMode::Auto,
        }),
        Ok(())
    );
    let repository = orna_repository::Repository::discover(directory.path()).expect("repository");
    let (identity, initial_digest) = runtime_identity(&repository).expect("runtime identity");
    let observations = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(async {
            RuntimeState::open(&repository, identity, initial_digest)
                .await
                .expect("reopened runtime")
                .run_observations()
                .await
                .expect("run observations")
        });
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].status, RunObservationStatus::Completed);
    assert!(!observations[0].live);
    let error = execute(&Parsed {
        endpoint: endpoint.clone(),
        command: Command::Run(Invocation::Seed),
        color: ColorMode::Auto,
    })
    .expect_err("duplicate seed must be rejected");
    assert_eq!((error.code, error.exit), ("E2200", Exit::Target));
    assert_eq!(
        execute(&Parsed {
            endpoint: endpoint.clone(),
            command: Command::Run(Invocation::Exercise),
            color: ColorMode::Auto,
        }),
        Ok(())
    );
    assert_eq!(
        execute(&Parsed {
            endpoint,
            command: Command::Run(Invocation::SensorsIngest),
            color: ColorMode::Auto,
        }),
        Ok(())
    );
}

#[test]
fn run_seed_executes_a_reachable_standard_free_project() {
    let directory = tempfile::tempdir().expect("temporary project");
    std::fs::write(
        directory.path().join("main.orna"),
        "use library; pub fn seed(): Int = library.value();",
    )
    .expect("main source");
    std::fs::write(
        directory.path().join("library.orna"),
        "pub fn value(): Int = 42;",
    )
    .expect("library source");
    assert!(
        std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(directory.path())
            .status()
            .expect("git")
            .success()
    );
    orna_repository::initialize_repository(directory.path()).expect("runtime metadata");

    let parsed = Parsed {
        endpoint: Endpoint::Path(directory.path().to_string_lossy().into_owned()),
        command: Command::Run(Invocation::Seed),
        color: ColorMode::Auto,
    };
    assert_eq!(execute(&parsed), Ok(()));
}

#[test]
fn invoke_executes_a_qualified_reachable_pure_function() {
    let directory = tempfile::tempdir().expect("temporary project");
    std::fs::write(
        directory.path().join("main.orna"),
        "use library; pub fn seed(): Int = library.value();",
    )
    .expect("main source");
    std::fs::write(
        directory.path().join("library.orna"),
        "pub fn value(): Int = 42;",
    )
    .expect("library source");
    assert!(
        std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(directory.path())
            .status()
            .expect("git")
            .success()
    );
    let parsed = Parsed {
        endpoint: Endpoint::Path(directory.path().to_string_lossy().into_owned()),
        command: Command::Invoke("library.value".into()),
        color: ColorMode::Auto,
    };
    assert_eq!(execute(&parsed), Ok(()));
}

#[test]
fn check_rejects_an_uncaptured_standard_module() {
    let directory = tempfile::tempdir().expect("temporary project");
    std::fs::write(
        directory.path().join("main.orna"),
        "use std.math; pub fn run(): Int = std.math.increment(41);",
    )
    .expect("main source");
    assert!(
        std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(directory.path())
            .status()
            .expect("git")
            .success()
    );

    let endpoint = Endpoint::Path(directory.path().to_string_lossy().into_owned());
    let error = check_project(&endpoint, false).expect_err("uncaptured standard module");
    assert_eq!(
        (error.code, error.exit, error.title.as_ref(), error.help),
        (
            "ORNA-S010-IMPORT",
            Exit::Target,
            "imported module is unavailable",
            "use a captured standard dependency or remove the import",
        )
    );
}

#[test]
fn check_rejects_each_uncaptured_standard_module() {
    let directory = tempfile::tempdir().expect("temporary project");
    std::fs::write(
        directory.path().join("main.orna"),
        "use std.math; use std.text; use std.json; use std.prelude as _; pub fn run() {}",
    )
    .expect("main source");
    assert!(
        std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(directory.path())
            .status()
            .expect("git")
            .success()
    );

    let endpoint = Endpoint::Path(directory.path().to_string_lossy().into_owned());
    let error = check_project(&endpoint, false).expect_err("unbundled standard module");
    assert_eq!(
        (error.code, error.exit, error.title.as_ref(), error.help),
        (
            "ORNA-S010-IMPORT",
            Exit::Target,
            "imported module is unavailable",
            "use a captured standard dependency or remove the import",
        )
    );
}

#[test]
fn invoke_rejects_an_uncaptured_standard_function() {
    let directory = tempfile::tempdir().expect("temporary project");
    std::fs::write(
        directory.path().join("main.orna"),
        "use std.math; pub fn seed(): Bool = std.math.is_zero(std.math.decrement(1));",
    )
    .expect("main source");
    assert!(
        std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(directory.path())
            .status()
            .expect("git")
            .success()
    );
    let parsed = Parsed {
        endpoint: Endpoint::Path(directory.path().to_string_lossy().into_owned()),
        command: Command::Invoke("seed".into()),
        color: ColorMode::Auto,
    };
    let error = execute(&parsed).expect_err("uncaptured standard module");
    assert_eq!(
        (error.code, error.exit, error.title.as_ref(), error.help),
        (
            "ORNA-S010-IMPORT",
            Exit::Target,
            "imported module is unavailable",
            "use a captured standard dependency or remove the import",
        )
    );
}

#[test]
fn invoke_rejects_composed_uncaptured_standard_math() {
    let directory = tempfile::tempdir().expect("temporary project");
    std::fs::write(
        directory.path().join("main.orna"),
        "use std.math; pub fn seed(): Int = std.math.clamp(std.math.max(2, 9), 3, 7);",
    )
    .expect("main source");
    assert!(
        std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(directory.path())
            .status()
            .expect("git")
            .success()
    );
    let parsed = Parsed {
        endpoint: Endpoint::Path(directory.path().to_string_lossy().into_owned()),
        command: Command::Invoke("seed".into()),
        color: ColorMode::Auto,
    };
    let error = execute(&parsed).expect_err("uncaptured standard module");
    assert_eq!(
        (error.code, error.exit, error.title.as_ref(), error.help),
        (
            "ORNA-S010-IMPORT",
            Exit::Target,
            "imported module is unavailable",
            "use a captured standard dependency or remove the import",
        )
    );
}

#[test]
fn check_rejects_semantic_errors_with_a_stable_cli_diagnostic() {
    let directory = tempfile::tempdir().expect("temporary project");
    std::fs::write(
        directory.path().join("main.orna"),
        "pub fn run(): Int = true;",
    )
    .expect("main source");
    assert!(
        std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(directory.path())
            .status()
            .expect("git")
            .success()
    );

    let endpoint = Endpoint::Path(directory.path().to_string_lossy().into_owned());
    let error = check_project(&endpoint, false).expect_err("semantic error");
    let rendered = error.to_string();
    assert_eq!((error.code, error.exit), ("E2101", Exit::Target));
    assert!(rendered.contains("ORNA-S021-TYPE"));
    assert!(rendered.contains("static types are incompatible"));
    assert!(rendered.contains("fix the first reported source contract error"));
}

#[test]
fn stream_failure_diagnostic_describes_delivery_scoped_rollback() {
    let diagnostic = Diagnostic::target(
        "E2200",
        "durable project stream delivery failed",
        "the failed delivery was rolled back; prior committed deliveries and their checkpoint progress remain; inspect the recorded failure before retrying the stream",
    );
    let rendered = diagnostic.to_string();

    assert!(rendered.contains("the failed delivery was rolled back"));
    assert!(rendered.contains("prior committed deliveries and their checkpoint progress remain"));
    assert!(rendered.contains("inspect the recorded failure before retrying the stream"));
    assert!(!rendered.contains("failed atomically"));
}

#[test]
fn cancellation_mapping_retains_safe_identity_and_cancelled_exit() {
    let diagnostic = orna_foundation::Diagnostic::new(
        orna_foundation::SafeText::new("ORNA-LIST-STREAM-CANCELLED").unwrap(),
        orna_foundation::DiagnosticSeverity::Error,
        orna_foundation::SafeText::redacted(),
    )
    .unwrap();
    let mapped = cancellation_diagnostic(&diagnostic);

    assert_eq!(mapped.code, "ORNA-LIST-STREAM-CANCELLED");
    assert_eq!(mapped.exit, Exit::Cancelled);
    assert!(mapped.detail.is_none());
}

#[test]
fn endpoints_reject_secrets() {
    assert_eq!(
        Endpoint::parse("orna://a@host/db")
            .expect_err("credentials")
            .code,
        "E1005"
    );
    assert_eq!(
        Endpoint::parse("orna://host")
            .expect_err("no database")
            .code,
        "E1004"
    );
}

#[test]
fn endpoint_parsing_matches_the_closed_client_transport_grammar() {
    for (endpoint, code) in [
        ("postgres://host/db", "E1004"),
        ("orna+unix://relative.sock", "E1004"),
        ("orna+unix:///tmp/orna.sock?debug=1", "E1005"),
        ("orna://local:7443/default", "E1004"),
        ("orna://db.example.test:0/work", "E1004"),
        ("orna://db.example.test/%zz", "E1004"),
        ("orna://[::1/work", "E1004"),
        ("orna://db.example.test/work\u{1f}", "E1004"),
    ] {
        let error = Endpoint::parse(endpoint).expect_err(endpoint);
        assert_eq!(error.code, code, "{endpoint}");
        assert_eq!(error.exit, Exit::Usage, "{endpoint}");
    }
    assert_eq!(
        Endpoint::parse("orna+unix:///tmp/orna.sock").expect("absolute Unix socket"),
        Endpoint::UnixSocket("/tmp/orna.sock".into())
    );
    assert_eq!(
        Endpoint::parse("orna://[::1]:7443/team%2Fwork").expect("IPv6 TLS endpoint"),
        Endpoint::RemoteTls("orna://[::1]:7443/team%2Fwork".into())
    );
}
#[test]
fn init_parse_defaults_to_the_current_directory_target() {
    assert_eq!(
        parse_cli(&["init".into()]).expect("parses").command,
        Command::Init(None)
    );
}

#[test]
fn repository_initialization_errors_keep_codes_and_give_specific_remedies() {
    for (error, code, help) in [
        (
            orna_repository::RepositoryInitError::GitUnavailable,
            "ORNA-REPO-INIT-001",
            "install or enable Git, then retry `orna init`",
        ),
        (
            orna_repository::RepositoryInitError::GitOperationFailed,
            "ORNA-REPO-INIT-002",
            "check that Git can initialize the target directory, then retry `orna init`",
        ),
        (
            orna_repository::RepositoryInitError::LocalStateUnavailable,
            "ORNA-REPO-INIT-003",
            "check local filesystem access for the target directory, then retry `orna init`",
        ),
        (
            orna_repository::RepositoryInitError::UnsafeMetadataPath,
            "ORNA-REPO-INIT-004",
            "replace unsafe metadata links or non-regular entries before retrying `orna init`",
        ),
        (
            orna_repository::RepositoryInitError::MetadataIncomplete,
            "ORNA-REPO-INIT-005",
            "restore complete metadata from a valid snapshot or initialize a new repository",
        ),
        (
            orna_repository::RepositoryInitError::MetadataMalformed,
            "ORNA-REPO-INIT-006",
            "restore valid metadata from a compatible repository snapshot before retrying",
        ),
        (
            orna_repository::RepositoryInitError::MetadataUnsupported,
            "ORNA-REPO-INIT-007",
            "use an Orna version that supports this repository format",
        ),
        (
            orna_repository::RepositoryInitError::MetadataChanged,
            "ORNA-REPO-INIT-008",
            "ensure no other process changes repository metadata, then retry `orna init`",
        ),
        (
            orna_repository::RepositoryInitError::RepositoryBusy,
            "ORNA-REPO-INIT-009",
            "wait for the other initialization to finish, then retry `orna init`",
        ),
        (
            orna_repository::RepositoryInitError::PlatformUnsupported,
            "ORNA-REPO-INIT-010",
            "use a supported platform to initialize this repository",
        ),
    ] {
        let diagnostic = repository_init_diagnostic(&error);
        assert_eq!(diagnostic.code, code);
        assert_eq!(diagnostic.help, help);
    }
}
#[test]
fn fetch_command_consumes_git_transport_without_mutating_head_or_worktree() {
    let root = tempfile::tempdir().expect("fetch fixture");
    let remote = root.path().join("remote.git");
    let local = root.path().join("local");
    let updater = root.path().join("updater");
    let git = |directory: &std::path::Path, arguments: &[&str]| -> String {
        let output = std::process::Command::new("git")
            .current_dir(directory)
            .args(arguments)
            .output()
            .expect("git starts");
        assert!(
            output.status.success(),
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("git output is UTF-8")
            .trim()
            .to_owned()
    };
    std::fs::create_dir_all(&local).expect("local directory");
    git(root.path(), &["init", "--bare", "remote.git"]);
    git(&local, &["init", "-b", "main"]);
    git(&local, &["config", "user.email", "kieran@drewett.dev"]);
    git(&local, &["config", "user.name", "kierandrewett"]);
    std::fs::write(local.join("tracked.txt"), "base\n").expect("base file");
    git(&local, &["add", "tracked.txt"]);
    git(&local, &["commit", "-m", "base"]);
    let initial = git(&local, &["rev-parse", "HEAD"]);
    let remote_path = remote.to_str().expect("remote path");
    git(&local, &["remote", "add", "origin", remote_path]);
    git(&local, &["push", "origin", "HEAD:refs/heads/main"]);
    git(
        root.path(),
        &[
            "--git-dir",
            "remote.git",
            "symbolic-ref",
            "HEAD",
            "refs/heads/main",
        ],
    );
    git(
        root.path(),
        &["clone", "--branch", "main", remote_path, "updater"],
    );
    assert_eq!(git(&updater, &["rev-parse", "HEAD"]), initial);
    git(&updater, &["config", "user.email", "kieran@drewett.dev"]);
    git(&updater, &["config", "user.name", "kierandrewett"]);
    std::fs::write(updater.join("tracked.txt"), "advanced\n").expect("advanced file");
    git(&updater, &["add", "tracked.txt"]);
    git(&updater, &["commit", "-m", "advance"]);
    let advanced = git(&updater, &["rev-parse", "HEAD"]);
    git(&updater, &["push", "origin", "HEAD:refs/heads/main"]);

    let endpoint = Endpoint::Path(local.display().to_string());
    execute(&Parsed {
        endpoint,
        command: Command::Fetch {
            remote: "origin".into(),
            branch: "main".into(),
        },
        color: ColorMode::Auto,
    })
    .expect("fetch command");
    assert_eq!(git(&local, &["rev-parse", "HEAD"]), initial);
    assert_eq!(
        git(&local, &["rev-parse", "refs/remotes/origin/main"]),
        advanced
    );
    assert!(git(&local, &["status", "--porcelain"]).is_empty());
}
