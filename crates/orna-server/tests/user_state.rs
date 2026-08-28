//! Active `orna state` access to the durable USER state service.
//!
//! This module runs one closed `orna state get|set` command against the
//! selected local or packaged instance. The host derives the principal from
//! the authenticated session: the local peer UID authenticated through
//! [`PostgresKernel::authenticate_local_peer`]. A request never carries a
//! principal identity.
//!
//! `orna state get` plans one `load_user_state` call: the root function and
//! state profile scope the load, optional instance requests filter the
//! returned cells, and optional expected-type entries arm the load-time
//! ORNA0901 check. `orna state set` plans one typed `write_user_state` change
//! carrying its expected revision; a conflict is a per-change closed result
//! (ORNA0902), never a transport failure. Every cell and write result is
//! rendered to `stdout` as one JSON record per line, with typed values in
//! their canonical ORV5 hex form.
mod support;

use std::{
    ffi::OsString,
    fs,
    io::{self, Read},
    os::unix::ffi::OsStringExt,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

use orna_client::{ClientStateContext, ClientStateKey, ClientStateStore, ClientUserStateError};
use orna_core::{
    FunctionId, PrincipalId, StateSlotId,
    state::{UserStateCell, UserStateKey},
    value::RuntimeValue,
};
use orna_server::AuthenticatedClientStateAdapter;

const PROCESS_TIMEOUT: Duration = Duration::from_secs(5);
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

/// A caller-owned scratch directory below the repository `target/` directory.
struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> io::Result<Self> {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("server crate remains below crates");
        let path = repository.join("target").join(format!(
            "user-state-test-{}-{}-{label}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path)?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn spawn_orna(
    directory: &Path,
    arguments: impl IntoIterator<Item = OsString>,
    stdin: Stdio,
) -> io::Result<Child> {
    Command::new(env!("CARGO_BIN_EXE_orna"))
        .args(arguments)
        .env_clear()
        .env("XDG_STATE_HOME", directory.join("state"))
        .env("XDG_RUNTIME_DIR", directory.join("runtime"))
        .current_dir(directory)
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
}

fn run_orna(directory: &Path, arguments: &[OsString]) -> io::Result<Output> {
    wait_bounded(spawn_orna(
        directory,
        arguments.iter().cloned(),
        Stdio::null(),
    )?)
}

fn wait_bounded(mut child: Child) -> io::Result<Output> {
    let deadline = Instant::now() + PROCESS_TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "orna state did not exit",
            ));
        }
        thread::sleep(Duration::from_millis(10));
    };
    let stdout = read_pipe(child.stdout.take().expect("captured stdout"))?;
    let stderr = read_pipe(child.stderr.take().expect("captured stderr"))?;
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

fn read_pipe(mut pipe: impl Read) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    pipe.read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Whether this test process runs with the exact installed `orna` account.
///
/// The guard exists so the valid-identity boundary test can never reach a
/// real default database when the suite itself runs as the service account.
/// If the account is absent or the lookup fails, the boundary may proceed.
fn runs_as_the_orna_account() -> bool {
    let Ok(Some(orna)) = nix::unistd::User::from_name("orna") else {
        return false;
    };
    nix::unistd::getuid() == orna.uid
        && nix::unistd::geteuid() == orna.uid
        && nix::unistd::getgid() == orna.gid
        && nix::unistd::getegid() == orna.gid
}

#[test]
fn malformed_state_shapes_all_fail_closed_with_exact_usage() {
    let directory = TestDirectory::new("usage").expect("scratch directory");
    let canonical = FunctionId::from_bytes([0x11; 16]).canonical();
    let cases = [
        vec![OsString::from("state")],
        vec![OsString::from("state"), OsString::from("get")],
        vec![
            OsString::from("state"),
            OsString::from("get"),
            OsString::from("not-an-id"),
        ],
        vec![
            OsString::from("state"),
            OsString::from("get"),
            OsString::from(&canonical),
            OsString::from("extra"),
        ],
        vec![OsString::from("state"), OsString::from("set")],
        vec![
            OsString::from("state"),
            OsString::from("set"),
            OsString::from(&canonical),
        ],
        vec![
            OsString::from("state"),
            OsString::from("set"),
            OsString::from(&canonical),
            OsString::from("--function"),
        ],
        vec![
            OsString::from("state"),
            OsString::from("set"),
            OsString::from(&canonical),
            OsString::from("--revision"),
            OsString::from("seven"),
        ],
        vec![
            OsString::from("state"),
            OsString::from("set"),
            OsString::from(&canonical),
            OsString::from("--value-file"),
            OsString::from("missing.bin"),
        ],
        vec![OsString::from("state"), OsString::from("dump")],
        vec![OsString::from_vec(b"state\xff".to_vec())],
    ];
    for arguments in cases {
        let output = run_orna(directory.path(), &arguments).expect("bounded invocation");
        assert_eq!(
            output.status.code(),
            Some(2),
            "arguments {arguments:?} must exit 2, got {output:?}"
        );
        assert!(
            output.stdout.is_empty(),
            "arguments {arguments:?} must emit no standard output"
        );
        assert_eq!(
            output.stderr,
            support::EXPECTED_USAGE,
            "arguments {arguments:?} must print the exact global usage"
        );
    }
}

#[test]
fn valid_state_shapes_reach_the_active_host_boundary() {
    if runs_as_the_orna_account() {
        eprintln!("skipping service-account boundary: suite runs as the orna account");
        return;
    }
    let directory = TestDirectory::new("service-account").expect("scratch directory");
    let canonical = FunctionId::from_bytes([0x44; 16]).canonical();
    let slot = orna_core::StateSlotId::from_bytes([0x45; 16]).canonical();
    let value_type = orna_core::TypeId::from_bytes([0x46; 16]).canonical();
    let value_file = directory.path().join("value.bin");
    fs::write(&value_file, [0x0a, 0x0b]).expect("state value file must write");
    let get = run_orna(
        directory.path(),
        &[
            OsString::from("state"),
            OsString::from("get"),
            OsString::from(&canonical),
        ],
    )
    .expect("bounded invocation");
    assert_eq!(get.status.code(), Some(7), "status: {get:?}");
    assert!(
        get.stdout.is_empty(),
        "service-account failure must emit no standard output or prompt"
    );
    assert_eq!(
        get.stderr,
        b"orna state: the Orna instance is not available: embedded PostgreSQL instance state is invalid\n",
        "service-account failure must print the exact public diagnostic"
    );

    let set = run_orna(
        directory.path(),
        &[
            OsString::from("state"),
            OsString::from("set"),
            OsString::from(&canonical),
            OsString::from("--function"),
            OsString::from(&canonical),
            OsString::from("--slot"),
            OsString::from(&slot),
            OsString::from("--revision"),
            OsString::from("create"),
            OsString::from("--type"),
            OsString::from(&value_type),
            OsString::from("--value-file"),
            value_file.into_os_string(),
        ],
    )
    .expect("bounded invocation");
    assert_eq!(set.status.code(), Some(7), "status: {set:?}");
    assert!(
        set.stdout.is_empty(),
        "service-account failure must emit no standard output or prompt"
    );
    assert_eq!(
        set.stderr,
        b"orna state: the Orna instance is not available: embedded PostgreSQL instance state is invalid\n",
        "service-account failure must print the exact public diagnostic"
    );
}

/// The authenticated adapter stages a complete multi-instance response before
/// replacing the caller-owned store: valid cells load together, while a
/// foreign context or duplicate identity leaves the prior context and values
/// untouched.
#[test]
fn authenticated_state_load_preserves_context_until_response_is_valid() {
    let principal = PrincipalId::from_bytes([0x71; 16]);
    let original_context = ClientStateContext::new(
        FunctionId::from_bytes([0x72; 16]),
        "original-profile".to_owned(),
        "original-instance".to_owned(),
    )
    .expect("original context must validate");
    let requested_context = ClientStateContext::new(
        FunctionId::from_bytes([0x73; 16]),
        "requested-profile".to_owned(),
        "root-instance".to_owned(),
    )
    .expect("requested context must validate");
    let function_a = FunctionId::from_bytes([0x74; 16]);
    let function_b = FunctionId::from_bytes([0x75; 16]);
    let slot_a = StateSlotId::from_bytes([0x76; 16]);
    let slot_b = StateSlotId::from_bytes([0x77; 16]);
    let value_type = orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID;
    let requested = vec![
        (function_a, "row-a".to_owned()),
        (function_b, "row-b".to_owned()),
    ];
    let cell = |function: FunctionId, instance_key: &str, slot: StateSlotId, value: &str| {
        UserStateCell::new(
            UserStateKey::new(
                principal,
                requested_context.root_function(),
                requested_context.state_profile().to_owned(),
                function,
                instance_key.to_owned(),
                slot,
            )
            .expect("state cell key must validate"),
            RuntimeValue::Text(value.to_owned()),
            value_type,
            1,
            std::time::SystemTime::UNIX_EPOCH,
        )
    };
    let valid_cells = vec![
        cell(function_a, "row-a", slot_a, "a"),
        cell(function_b, "row-b", slot_b, "b"),
    ];
    let mut store = ClientStateStore::new();
    store.set_context(original_context.clone());
    let existing_key = ClientStateKey::from_context(&original_context, function_a, slot_a);
    store
        .set_user_state(
            existing_key.clone(),
            RuntimeValue::Text("caller-owned".to_owned()),
            value_type,
        )
        .expect("caller-owned state must validate");
    let before = store.clone();

    let staged = AuthenticatedClientStateAdapter::<'static>::stage_authenticated_user_state_load(
        &store,
        &requested_context,
        &valid_cells,
        &requested,
    )
    .expect("a complete multi-instance response must stage");
    assert_eq!(staged.context(), &requested_context);
    assert_eq!(staged.user().len(), 3);
    assert_eq!(
        staged.user().get(&existing_key),
        before.user().get(&existing_key)
    );
    assert_eq!(
        store, before,
        "staging must not mutate the caller-owned store"
    );

    let foreign_cell = UserStateCell::new(
        UserStateKey::new(
            principal,
            FunctionId::from_bytes([0x78; 16]),
            "foreign-profile".to_owned(),
            function_a,
            "row-a".to_owned(),
            slot_a,
        )
        .expect("foreign state cell key must validate"),
        RuntimeValue::Text("foreign".to_owned()),
        value_type,
        2,
        std::time::SystemTime::UNIX_EPOCH,
    );
    let foreign_error =
        AuthenticatedClientStateAdapter::<'static>::stage_authenticated_user_state_load(
            &store,
            &requested_context,
            &[valid_cells[0].clone(), foreign_cell],
            &requested,
        )
        .expect_err("a foreign root/profile must be rejected");
    assert!(matches!(
        foreign_error,
        ClientUserStateError::ContextMismatch(_)
    ));
    assert_eq!(
        store, before,
        "foreign response must be rejected atomically"
    );

    let duplicate_error =
        AuthenticatedClientStateAdapter::<'static>::stage_authenticated_user_state_load(
            &store,
            &requested_context,
            &[valid_cells[0].clone(), valid_cells[0].clone()],
            &requested,
        )
        .expect_err("duplicate identities must be rejected");
    assert!(matches!(
        duplicate_error,
        ClientUserStateError::DuplicateKey(_)
    ));
    assert_eq!(
        store, before,
        "duplicate response must be rejected atomically"
    );

    let loaded_key = ClientStateKey::from_user_cell(&valid_cells[0]);
    assert_eq!(
        staged.user().get(&loaded_key).map(|value| value.value()),
        Some(&RuntimeValue::Text("a".to_owned()))
    );
}
