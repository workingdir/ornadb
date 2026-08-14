//! Installed-product end-to-end test for work ADR 0038.
//!
//! This test drives the packaged `/usr/bin/orna` executable and its public
//! commands only. It never calls an internal Orna Rust or kernel API to
//! apply source, grant execution, or invoke a function.
//!
//! The flow is:
//!
//! 1. install the exact frozen `.deb` in a clean Debian container;
//! 2. start the installed server as the `orna` service account;
//! 3. run `orna source apply` on the checked-in fixture and parse the exact
//!    success JSON for both function identities;
//! 4. prove raw calls to both functions are denied before any grant;
//! 5. grant both functions through `orna security grant-execute`;
//! 6. invoke the parameter-free raw INSERT and validate the returned ORV
//!    object reference;
//! 7. invoke the raw SELECT and require the exact canonical Boolean TRUE
//!    value;
//! 8. repeat both fixed-service grants and prove they stay silent;
//! 9. apply the checked-in invalid fixture and require a compiler
//!    diagnostic failure that changes nothing;
//! 10. invoke the raw INSERT again for a second distinct object and read two
//!     canonical Boolean TRUE values;
//! 11. restart the installed server and prove the two rows persist
//!     byte-identically;
//! 12. apply the checked-in false fixture, prove the active revisions change
//!     while the function identities stay stable, insert a third object, and
//!     prove the three stored rows decode as one FALSE and two TRUE values,
//!     byte-order-independent, across another restart;
//! 13. reapply the original fixture, prove the revisions advance again while
//!     the function identities and grants stay stable, insert a fourth
//!     object, and prove the four stored rows decode as one FALSE and three
//!     TRUE values, byte-order-independent, across another restart.
//!
//! The test is ignored by default so ordinary gates stay green. The Debian
//! package workflow runs it against the reproduced package by setting
//! `ORNA_SYSTEM_TEST_DEBIAN_PACKAGE` and invoking the ignored test exactly,
//! so the workflow fails closed if the installed product path regresses.
//!
//! Every post-install service/data-path invocation made by this scenario
//! through `setpriv_args` (server run and restart, source apply, security
//! grant-execute, and raw-call) runs with deliberately poisoned libpq/
//! PostgreSQL environment values (hostile `PGHOST`, `PGPORT`, `PGDATABASE`,
//! `PGUSER`, `PGSERVICE`, `PGSERVICEFILE`, `PGDATA`, `PGPASSFILE`, and
//! `PGOPTIONS`). Those real service-account operations still complete
//! against the fixed private endpoint, which demonstrates that the packaged
//! binary does not select an external endpoint from those variables. The
//! package postinst invokes `/usr/bin/orna` separately through `env -i` and
//! is excluded from this environment assertion, as is package maintenance.

use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, ExitStatus, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use orna_system_tests::{Error as ArtifactError, FrozenPackageArtifact, PackageFormat};

/// Lowercase prefix shared by every container name and image tag.
const NAME_PREFIX: &str = "orna-product-test";

/// The fixed private readiness file published by the installed server.
const READY_FILE: &str = "/run/orna/default/ready";

/// The fixed public raw-call socket published by the installed server.
const PUBLIC_SOCKET: &str = "/run/orna/default/orna.sock";

/// The path of the transferred package inside the container.
const DEB_PATH: &str = "/proof/orna.deb";

/// The path of the fixture source file inside the container.
const FIXTURE_PATH: &str = "/work/product_test.orna";

/// How long the server may take to publish readiness after a start.
const READY_TIMEOUT: Duration = Duration::from_secs(60);

/// How long the server may take to stop cleanly after SIGINT.
const STOP_TIMEOUT: Duration = Duration::from_secs(30);

/// How often readiness and shutdown checks poll the container.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// A process-local counter that keeps names unique within one test run.
static NAME_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The transfer script executed inside the container with `/bin/sh -ceu`.
///
/// The exact frozen bytes arrive on stdin through `docker exec --interactive
/// --env EXPECTED_SHA256=...`. The script writes them to `/proof/orna.deb`,
/// tightens the mode to 0400, and verifies the digest before returning.
const TRANSFER_SCRIPT: &str = "\
umask 0377\n\
mkdir -p /proof\n\
cat > /proof/orna.deb\n\
chmod 0400 /proof/orna.deb\n\
printf '%s /proof/orna.deb\\n' \"$EXPECTED_SHA256\" | sha256sum -c -";

/// The fixture write script executed inside the container with `/bin/sh -ceu`.
///
/// The checked-in fixture bytes arrive on stdin. The file must stay readable
/// by the `orna` service account, so the mode is 0644.
const FIXTURE_WRITE_SCRIPT: &str = "\
umask 0022\n\
mkdir -p /work\n\
cat > /work/product_test.orna\n\
chmod 0644 /work/product_test.orna";

/// A unique lowercase suffix built from the process id and a counter.
fn unique_suffix() -> String {
    let counter = NAME_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("{}-{counter}", std::process::id())
}

/// The container name for `suffix`.
fn container_name(suffix: &str) -> String {
    format!("{NAME_PREFIX}-{suffix}")
}

/// The full image reference for `suffix`.
fn image_ref(suffix: &str) -> String {
    format!("{NAME_PREFIX}:{suffix}")
}

/// A clean Docker-backed Debian machine with the installed product.
///
/// The machine owns a unique image tag and container name. Drop
/// force-removes the exact container and then the exact image, printing
/// cleanup errors only.
struct InstalledMachine {
    image_ref: String,
    container: String,
    uid: String,
    gid: String,
}

impl InstalledMachine {
    /// Build the image, install the exact frozen package, write the fixture,
    /// and start the installed server.
    ///
    /// The frozen artifact is verified exactly once, when it is streamed into
    /// the container for `dpkg --install`.
    fn start(artifact: &FrozenPackageArtifact, fixture: &[u8]) -> Result<Self, Error> {
        let suffix = unique_suffix();
        let mut machine = Self {
            image_ref: image_ref(&suffix),
            container: container_name(&suffix),
            uid: String::new(),
            gid: String::new(),
        };
        machine.build_image()?;
        machine.create_and_start_container()?;
        machine.transfer_deb(artifact)?;
        machine.install_deb()?;
        let (uid, gid) = machine.orna_identity()?;
        machine.uid = uid;
        machine.gid = gid;
        machine.write_fixture(fixture)?;
        machine.prepare_runtime_root()?;
        machine.start_server()?;
        machine.wait_ready()?;
        Ok(machine)
    }

    /// Build the Debian image from the checked-in Containerfile.
    fn build_image(&self) -> Result<(), Error> {
        let assets = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join("debian");
        let containerfile = assets.join("Containerfile");
        let mut command = Command::new("docker");
        command
            .args([
                "build",
                "--platform",
                "linux/amd64",
                "--provenance=false",
                "--tag",
                self.image_ref.as_str(),
                "--file",
            ])
            .arg(&containerfile)
            .arg(&assets);
        run("build the installed-product image", &mut command)?;
        Ok(())
    }

    /// Create a stopped container with no network, then start it.
    fn create_and_start_container(&self) -> Result<(), Error> {
        let mut create = Command::new("docker");
        create.args([
            "create",
            "--name",
            self.container.as_str(),
            "--network",
            "none",
            "--platform",
            "linux/amd64",
            self.image_ref.as_str(),
            "sleep",
            "infinity",
        ]);
        run("create the installed-product container", &mut create)?;

        let mut start = Command::new("docker");
        start.args(["start", self.container.as_str()]);
        run("start the installed-product container", &mut start)?;
        Ok(())
    }

    /// Stream the verified frozen bytes into the container on stdin.
    ///
    /// This is the only place the artifact is opened and verified. The
    /// container verifies the digest again before the exec returns.
    fn transfer_deb(&self, artifact: &FrozenPackageArtifact) -> Result<(), Error> {
        let verified = artifact.open_verified().map_err(Error::Verify)?;
        let expected = verified.sha256().to_string();
        let mut command = Command::new("docker");
        command
            .args(["exec", "--interactive", "--env"])
            .arg(format!("EXPECTED_SHA256={expected}"))
            .args([self.container.as_str(), "/bin/sh", "-ceu", TRANSFER_SCRIPT])
            .stdin(Stdio::from(verified.into_file()));
        run("transfer the package into the container", &mut command)?;
        Ok(())
    }

    /// Install the transferred package into the clean container.
    fn install_deb(&self) -> Result<(), Error> {
        self.exec_root(
            "install the Debian package",
            &["/usr/bin/dpkg", "--install", DEB_PATH],
        )?;
        Ok(())
    }

    /// Resolve the numeric `orna` service account identity.
    fn orna_identity(&self) -> Result<(String, String), Error> {
        let uid = self.exec_root("resolve the orna uid", &["/usr/bin/id", "-u", "orna"])?;
        let gid = self.exec_root("resolve the orna gid", &["/usr/bin/id", "-g", "orna"])?;
        let uid = String::from_utf8(uid.stdout).map_err(|_| Error::Unexpected {
            message: "id -u orna output is not UTF-8".to_string(),
        })?;
        let gid = String::from_utf8(gid.stdout).map_err(|_| Error::Unexpected {
            message: "id -g orna output is not UTF-8".to_string(),
        })?;
        Ok((uid.trim().to_string(), gid.trim().to_string()))
    }

    /// Write one fixture source file into the container.
    fn write_fixture(&self, fixture: &[u8]) -> Result<(), Error> {
        let mut command = Command::new("docker");
        command
            .args([
                "exec",
                "--interactive",
                self.container.as_str(),
                "/bin/sh",
                "-ceu",
                FIXTURE_WRITE_SCRIPT,
            ])
            .stdin(Stdio::piped());
        let mut child = command.spawn().map_err(|io| Error::Spawn {
            label: "spawn fixture transfer",
            io,
        })?;
        child
            .stdin
            .take()
            .expect("piped stdin must be present")
            .write_all(fixture)
            .map_err(|io| Error::Spawn {
                label: "write fixture into the container",
                io,
            })?;
        let output = child.wait_with_output().map_err(|io| Error::Spawn {
            label: "wait for fixture transfer",
            io,
        })?;
        require_success("write the fixture into the container", output)?;
        Ok(())
    }

    /// Create the fixed runtime root with the exact service ownership.
    fn prepare_runtime_root(&self) -> Result<(), Error> {
        let script = format!(
            "install -d -o {} -g {} -m 0711 /run/orna/default",
            self.uid, self.gid
        );
        self.exec_root_shell("prepare the runtime root", &script)?;
        Ok(())
    }

    /// Start the installed server detached as the `orna` service account.
    fn start_server(&self) -> Result<(), Error> {
        let args = self.setpriv_args(&["server", "run"]);
        let mut command = Command::new("docker");
        command.arg("exec").arg("-d").args(&args);
        run("start the installed orna server", &mut command)?;
        Ok(())
    }

    /// Stop the installed server with SIGINT and require a clean shutdown.
    fn stop_server(&self) -> Result<(), Error> {
        let server_pid = self.read_pid("server_pid")?;
        let script = format!("kill -INT {server_pid}");
        self.exec_root_shell("signal the installed orna server", &script)?;

        let deadline = Instant::now() + STOP_TIMEOUT;
        while Instant::now() < deadline {
            let stopped = self.root_test(&format!("test ! -e {READY_FILE}"))
                && self.root_test(&format!("test ! -e {PUBLIC_SOCKET}"))
                && self.root_test(&format!("! kill -0 {server_pid}"));
            if stopped {
                return Ok(());
            }
            std::thread::sleep(POLL_INTERVAL);
        }
        Err(Error::Timeout {
            what: "the installed orna server did not stop cleanly",
        })
    }

    /// Restart the installed server and require readiness again.
    fn restart_server(&self) -> Result<(), Error> {
        self.stop_server()?;
        self.start_server()?;
        self.wait_ready()
    }

    /// Read one numeric field from the private readiness file.
    fn read_pid(&self, key: &str) -> Result<String, Error> {
        let script = format!("sed -n 's/^{key} = //p' {READY_FILE}");
        let output = self.exec_root_shell("read the readiness file", &script)?;
        let text = String::from_utf8_lossy(&output.stdout);
        let value = text.trim();
        if value.parse::<u32>().is_err() {
            return Err(Error::Unexpected {
                message: format!("readiness field {key} is not a process id: {value:?}"),
            });
        }
        Ok(value.to_string())
    }

    /// Poll until the installed server publishes readiness.
    fn wait_ready(&self) -> Result<(), Error> {
        let deadline = Instant::now() + READY_TIMEOUT;
        while Instant::now() < deadline {
            if self.root_test(&format!("test -f {READY_FILE}")) {
                return Ok(());
            }
            std::thread::sleep(POLL_INTERVAL);
        }
        Err(Error::Timeout {
            what: "the installed orna server did not publish readiness",
        })
    }

    /// Run one installed `orna` command as the service account.
    ///
    /// The exit status is not checked here. Callers assert success, silence,
    /// or a specific denied failure against the returned output.
    fn run_as_orna(&self, command: &[&str]) -> Result<Output, Error> {
        let args = self.setpriv_args(command);
        self.exec_args(&args)
    }

    /// Run one installed `orna` raw-call command with the exact argument
    /// bytes streamed through `docker exec --interactive`.
    ///
    /// The command runs as the real service account with the same poisoned
    /// libpq environment as [`Self::run_as_orna`]. The supplied bytes arrive
    /// on the child's standard input and EOF closes the pipe after the write.
    /// The exit status is not checked here.
    fn run_as_orna_with_stdin(&self, command: &[&str], input: &[u8]) -> Result<Output, Error> {
        let mut args = vec!["--interactive".to_string()];
        args.extend(self.setpriv_args(command));
        let mut child = Command::new("docker")
            .arg("exec")
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|io| Error::Spawn {
                label: "spawn argument raw-call exec",
                io,
            })?;
        child
            .stdin
            .take()
            .expect("piped stdin must be present")
            .write_all(input)
            .map_err(|io| Error::Spawn {
                label: "stream the raw-call argument",
                io,
            })?;
        child.wait_with_output().map_err(|io| Error::Spawn {
            label: "wait for argument raw-call exec",
            io,
        })
    }

    /// The `docker exec` argument vector for one post-install service/data
    /// path `orna` command as the real service account.
    ///
    /// Every invocation made through this helper (server run and restart,
    /// source apply, security grant-execute, and raw-call) receives the
    /// fixed private socket selection plus deliberately poisoned standard
    /// libpq/PostgreSQL environment values as `docker exec --env` pairs.
    /// Those real service-account operations still complete against the
    /// fixed private endpoint, which demonstrates the packaged binary does
    /// not select an external endpoint from them. Package maintenance and
    /// the postinst invoke `/usr/bin/orna` separately and are not covered
    /// by this assertion.
    fn setpriv_args(&self, command: &[&str]) -> Vec<String> {
        let mut args = vec![
            "--env".to_string(),
            "PATH=/usr/sbin:/usr/bin:/sbin:/bin".to_string(),
            "--env".to_string(),
            "PGHOST=127.0.0.1".to_string(),
            "--env".to_string(),
            "PGPORT=1".to_string(),
            "--env".to_string(),
            "PGDATABASE=hostile".to_string(),
            "--env".to_string(),
            "PGUSER=hostile".to_string(),
            "--env".to_string(),
            "PGSERVICE=no_such_service".to_string(),
            "--env".to_string(),
            "PGSERVICEFILE=/nonexistent/orna-pg-service.conf".to_string(),
            "--env".to_string(),
            "PGDATA=/nonexistent/orna-pg-data".to_string(),
            "--env".to_string(),
            "PGPASSFILE=/nonexistent/orna-pg-pass".to_string(),
            "--env".to_string(),
            "PGOPTIONS=-csearch_path=hostile".to_string(),
            self.container.clone(),
            "/usr/bin/setpriv".to_string(),
            format!("--reuid={}", self.uid),
            format!("--regid={}", self.gid),
            "--clear-groups".to_string(),
            "--".to_string(),
            "/usr/bin/orna".to_string(),
        ];
        args.extend(command.iter().map(|part| part.to_string()));
        args
    }

    /// Run one root command in the container and require success.
    fn exec_root(&self, label: &'static str, program: &[&str]) -> Result<Output, Error> {
        let mut args = vec![self.container.clone()];
        args.extend(program.iter().map(|part| part.to_string()));
        require_success(label, self.exec_args(&args)?)
    }

    /// Run one root shell script in the container and require success.
    fn exec_root_shell(&self, label: &'static str, script: &str) -> Result<Output, Error> {
        let mut args = vec![self.container.clone()];
        args.push("/bin/sh".to_string());
        args.push("-ceu".to_string());
        args.push(script.to_string());
        require_success(label, self.exec_args(&args)?)
    }

    /// Run one root shell script in the container without a status check.
    ///
    /// Polling uses `test`-style scripts whose non-zero status is data.
    fn root_test(&self, script: &str) -> bool {
        let mut args = vec![self.container.clone()];
        args.push("/bin/sh".to_string());
        args.push("-ceu".to_string());
        args.push(script.to_string());
        self.exec_args(&args)
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    /// Run `docker exec` with the given arguments, returning any output.
    fn exec_args(&self, args: &[String]) -> Result<Output, Error> {
        let mut command = Command::new("docker");
        command.arg("exec").args(args);
        command.output().map_err(|io| Error::Spawn {
            label: "docker exec",
            io,
        })
    }
}

impl Drop for InstalledMachine {
    fn drop(&mut self) {
        let mut remove_container = Command::new("docker");
        remove_container.args(["rm", "--force", self.container.as_str()]);
        if let Err(error) = run(
            "remove the installed-product container",
            &mut remove_container,
        ) {
            eprintln!("cleanup: {error}");
        }

        let mut remove_image = Command::new("docker");
        remove_image.args(["rmi", "--force", self.image_ref.as_str()]);
        if let Err(error) = run("remove the installed-product image", &mut remove_image) {
            eprintln!("cleanup: {error}");
        }
    }
}

/// Run a command to completion and require success.
///
/// Command output and status collection is unbounded in this first slice.
/// The explicit test run is bounded by the operator or by the workflow job
/// timeout, so no fake timeout is implemented here.
fn run(label: &'static str, command: &mut Command) -> Result<Output, Error> {
    let output = command.output().map_err(|io| Error::Spawn { label, io })?;
    require_success(label, output)
}

/// Require a zero exit status and return the output.
fn require_success(label: &'static str, output: Output) -> Result<Output, Error> {
    if output.status.success() {
        Ok(output)
    } else {
        Err(Error::CommandUnexpected {
            label,
            status: output.status,
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// Require a zero exit status and completely empty streams.
fn require_silent_success(label: &'static str, output: Output) -> Result<Output, Error> {
    let output = require_success(label, output)?;
    if !output.stdout.is_empty() || !output.stderr.is_empty() {
        return Err(Error::Unexpected {
            message: format!(
                "{label} must write nothing, got {} stdout bytes and {} stderr bytes",
                output.stdout.len(),
                output.stderr.len()
            ),
        });
    }
    Ok(output)
}

/// Require one successful raw-call that produced a value payload and no
/// standard error, returning the output for exact envelope validation.
fn require_value_success(label: &'static str, output: Output) -> Result<Output, Error> {
    let output = require_success(label, output)?;
    if !output.stderr.is_empty() {
        return Err(Error::Unexpected {
            message: format!(
                "{label} must keep standard error empty, got {} bytes",
                output.stderr.len()
            ),
        });
    }
    Ok(output)
}

/// Require one closed raw-call failure with the exact standard-error line.
///
/// The call must exit 1, emit no value, and print exactly `line` on standard
/// error.
fn assert_exact_raw_call_failure(
    label: &'static str,
    output: Output,
    line: &str,
) -> Result<(), Error> {
    if output.status.code() != Some(1) {
        return Err(Error::Unexpected {
            message: format!("{label} must exit 1, got {}", output.status),
        });
    }
    if !output.stdout.is_empty() {
        return Err(Error::Unexpected {
            message: format!(
                "{label} must emit no value, got {} bytes",
                output.stdout.len()
            ),
        });
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr != line {
        return Err(Error::Unexpected {
            message: format!("{label} must print the exact line, got {stderr:?}"),
        });
    }
    Ok(())
}

/// Require the exact closed denied outcome of a raw call.
///
/// A denied raw call exits 1, emits no value, and prints the exact
/// `raw call failed: EXECUTE_DENIED` line on standard error.
fn assert_denied(label: &'static str, output: Output) -> Result<(), Error> {
    assert_exact_raw_call_failure(label, output, "raw call failed: EXECUTE_DENIED\n")
}

/// Require the exact closed target-unavailable outcome of a raw call.
///
/// A raw call with no usable target exits 1, emits no value, and prints the
/// exact `raw call failed: TARGET_UNAVAILABLE` line on standard error.
fn assert_target_unavailable(label: &'static str, output: Output) -> Result<(), Error> {
    assert_exact_raw_call_failure(label, output, "raw call failed: TARGET_UNAVAILABLE\n")
}

/// Require one rejected installed source apply.
///
/// The apply must exit 1, emit no standard output, and write at least one
/// compiler diagnostic that names the fixture path and an ORNA diagnostic
/// code on standard error.
fn assert_source_apply_rejected(output: Output) -> Result<(), Error> {
    if output.status.code() != Some(1) {
        return Err(Error::Unexpected {
            message: format!("source apply must exit 1, got {}", output.status),
        });
    }
    if !output.stdout.is_empty() {
        return Err(Error::Unexpected {
            message: format!(
                "rejected source apply must emit no standard output, got {} bytes",
                output.stdout.len()
            ),
        });
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let has_orna_code = stderr.as_bytes().windows(8).any(|window| {
        window[0..4] == *b"ORNA" && window[4..8].iter().all(|byte| byte.is_ascii_digit())
    });
    if stderr.is_empty() || !stderr.contains("product_test.orna") || !has_orna_code {
        return Err(Error::Unexpected {
            message: format!(
                "rejected source apply must name the fixture and an ORNA code, got {stderr:?}"
            ),
        });
    }
    Ok(())
}

/// Require the exact canonical Boolean TRUE value envelope.
fn assert_exact_boolean_true(label: &'static str, output: Output) -> Result<(), Error> {
    let output = require_success(label, output)?;
    if output.stdout != boolean_orv1_envelope(Some(true)) {
        return Err(Error::Unexpected {
            message: format!(
                "{label} must emit the exact canonical Boolean TRUE envelope, got {} bytes",
                output.stdout.len()
            ),
        });
    }
    if !output.stderr.is_empty() {
        return Err(Error::Unexpected {
            message: format!(
                "{label} must keep standard error empty, got {} bytes",
                output.stderr.len()
            ),
        });
    }
    Ok(())
}

/// The canonical `ORV1` envelope for one Boolean value.
///
/// `None` emits the typed Boolean NULL envelope: `ORV1`, the NULL-SCALAR
/// tag, the 16-byte BOOLEAN type identity, and a zero payload length with no
/// payload bytes. `Some(value)` emits the exact Boolean envelope: `ORV1`,
/// the BOOLEAN tag, the 16-byte BOOLEAN type identity, the 4-byte big-endian
/// payload length, and one payload byte for FALSE or TRUE.
fn boolean_orv1_envelope(value: Option<bool>) -> Vec<u8> {
    match value {
        Some(value) => {
            let mut bytes = Vec::with_capacity(26);
            bytes.extend_from_slice(b"ORV1");
            bytes.push(0x02);
            bytes.extend_from_slice(&[0; 15]);
            bytes.push(0x01);
            bytes.extend_from_slice(&1_u32.to_be_bytes());
            bytes.push(u8::from(value));
            bytes
        }
        None => {
            let mut bytes = Vec::with_capacity(25);
            bytes.extend_from_slice(b"ORV1");
            bytes.push(0x00);
            bytes.extend_from_slice(&[0; 15]);
            bytes.push(0x01);
            bytes.extend_from_slice(&0_u32.to_be_bytes());
            bytes
        }
    }
}

/// Two exact canonical Boolean TRUE envelopes, one per stored row.
fn two_boolean_true_envelopes() -> Vec<u8> {
    let mut bytes = boolean_orv1_envelope(Some(true));
    bytes.extend(boolean_orv1_envelope(Some(true)));
    bytes
}

/// Decode one complete canonical ORV1 Boolean envelope.
///
/// Returns `Some(value)` only when the bytes form exactly one envelope with
/// the ORV1 marker, the BOOLEAN tag, the BOOLEAN type identity, payload
/// length 1, and a payload byte of exactly 0 or 1.
fn decode_boolean_envelope(bytes: &[u8]) -> Option<bool> {
    if bytes.len() != 26
        || &bytes[0..4] != b"ORV1"
        || bytes[4] != 0x02
        || bytes[5..20] != [0; 15]
        || bytes[20] != 0x01
        || bytes[21..25] != 1_u32.to_be_bytes()
    {
        return None;
    }
    match bytes[25] {
        0x00 => Some(false),
        0x01 => Some(true),
        _ => None,
    }
}

/// Decode a stream of complete canonical ORV1 Boolean envelopes in order.
///
/// Returns `None` when any envelope is malformed or trailing bytes remain.
fn decode_boolean_envelopes(bytes: &[u8]) -> Option<Vec<bool>> {
    if !bytes.len().is_multiple_of(26) {
        return None;
    }
    bytes
        .chunks_exact(26)
        .map(decode_boolean_envelope)
        .collect()
}

/// One parsed `ORV1` object-reference envelope.
struct OrvReference {
    type_id: [u8; 16],
    object: [u8; 16],
}

impl OrvReference {
    /// Whether the referenced object identity is all zero.
    fn object_is_zero(&self) -> bool {
        self.object == [0; 16]
    }
}

/// Parse one complete canonical `ORV1` reference envelope.
///
/// The envelope layout is `ORV1`, the REFERENCE tag, the 16-byte target type
/// identity, the 4-byte big-endian payload length, and the 16-byte object
/// identity. Any other shape, tag, or length is rejected.
fn parse_reference_envelope(bytes: &[u8]) -> Result<OrvReference, Error> {
    const ENVELOPE_LENGTH: usize = 4 + 1 + 16 + 4 + 16;
    if bytes.len() != ENVELOPE_LENGTH {
        return Err(Error::Unexpected {
            message: format!(
                "reference envelope must be {ENVELOPE_LENGTH} bytes, got {}",
                bytes.len()
            ),
        });
    }
    if &bytes[0..4] != b"ORV1" {
        return Err(Error::Unexpected {
            message: "reference envelope must start with the ORV1 marker".to_string(),
        });
    }
    if bytes[4] != 0x08 {
        return Err(Error::Unexpected {
            message: format!(
                "reference envelope must use the REFERENCE tag, got {}",
                bytes[4]
            ),
        });
    }
    let payload_length =
        u32::from_be_bytes(bytes[21..25].try_into().expect("fixed slice")) as usize;
    if payload_length != 16 {
        return Err(Error::Unexpected {
            message: format!("reference object identity must be 16 bytes, got {payload_length}"),
        });
    }
    let mut type_id = [0; 16];
    type_id.copy_from_slice(&bytes[5..21]);
    let mut object = [0; 16];
    object.copy_from_slice(&bytes[25..41]);
    Ok(OrvReference { type_id, object })
}

/// One parsed parameter declaration of an application function entry.
///
/// The name is the exact parameter name, the identity is the canonical
/// `parameter:<id>` value.
#[derive(Clone, Debug, Eq, PartialEq)]
struct ParameterEntry {
    name: String,
    parameter_id: String,
}

impl ParameterEntry {
    /// The exact parameter name.
    fn name(&self) -> &str {
        &self.name
    }

    /// The canonical parameter identity.
    fn parameter_id(&self) -> &str {
        &self.parameter_id
    }
}

/// One parsed application function entry.
///
/// The qualified name parts, the canonical `function:<id>` identity, and the
/// ordered parameter declarations, empty for a parameter-free function.
#[derive(Clone, Debug, Eq, PartialEq)]
struct FunctionEntry {
    names: Vec<String>,
    function_id: String,
    parameters: Vec<ParameterEntry>,
}

impl FunctionEntry {
    /// The exact qualified name parts.
    fn names(&self) -> &[String] {
        &self.names
    }

    /// The canonical function identity.
    fn function_id(&self) -> &str {
        &self.function_id
    }

    /// The ordered parameter declarations.
    fn parameters(&self) -> &[ParameterEntry] {
        &self.parameters
    }

    /// The parameter identity for one exact parameter name.
    fn parameter_id(&self, name: &str) -> Result<&str, Error> {
        self.parameters
            .iter()
            .find(|parameter| parameter.name() == name)
            .map(|parameter| parameter.parameter_id())
            .ok_or_else(|| Error::Unexpected {
                message: format!("function {} lacks the parameter {name:?}", self.function_id),
            })
    }
}

/// The parsed success document of `orna source apply`.
struct ApplyDocument {
    source_revision: String,
    catalogue_revision: String,
    functions: Vec<FunctionEntry>,
}

impl ApplyDocument {
    /// The function identity for one exact qualified name.
    fn function_id(&self, name: &[&str]) -> Result<&str, Error> {
        self.functions
            .iter()
            .find(|function| {
                function
                    .names()
                    .iter()
                    .map(String::as_str)
                    .eq(name.iter().copied())
            })
            .map(|function| function.function_id())
            .ok_or_else(|| Error::Unexpected {
                message: format!("source apply functions lack the qualified name {name:?}"),
            })
    }

    /// The parameter identity for one exact parameter name of one function.
    fn parameter_id(&self, function: &[&str], parameter: &str) -> Result<&str, Error> {
        let entry = self
            .functions
            .iter()
            .find(|candidate| {
                candidate
                    .names()
                    .iter()
                    .map(String::as_str)
                    .eq(function.iter().copied())
            })
            .ok_or_else(|| Error::Unexpected {
                message: format!("source apply functions lack the qualified name {function:?}"),
            })?;
        entry.parameter_id(parameter)
    }
}

/// Parse the exact compact JSON success document of `orna source apply`.
///
/// The document is one line ending in one line feed:
///
/// ```json
/// {"source_revision":"source-revision:<id>","catalogue_revision":"catalogue-revision:<id>","functions":[{"qualified_name":["schema","function"],"function_id":"function:<id>"}]}
/// ```
///
/// The object key order and the entry shape are exact. Every deviation is
/// rejected with a closed message.
fn parse_apply_document(bytes: &[u8]) -> Result<ApplyDocument, Error> {
    let text = std::str::from_utf8(bytes).map_err(|_| Error::Unexpected {
        message: "source apply output is not UTF-8".to_string(),
    })?;
    if !text.ends_with('\n') {
        return Err(Error::Unexpected {
            message: "source apply output must end with one line feed".to_string(),
        });
    }
    let body = &text[..text.len() - 1];
    if body.contains('\n') || body.contains('\r') {
        return Err(Error::Unexpected {
            message: "source apply output must be exactly one line".to_string(),
        });
    }

    let source_marker = "\"source_revision\":\"source-revision:";
    let rest = body
        .strip_prefix('{')
        .and_then(|rest| rest.strip_prefix(source_marker))
        .ok_or_else(|| Error::Unexpected {
            message: "source apply output must start with the source revision field".to_string(),
        })?;
    let source_end = rest.find('"').ok_or_else(|| Error::Unexpected {
        message: "source apply output has no source revision terminator".to_string(),
    })?;
    let source_id = &rest[..source_end];
    if source_id.is_empty() {
        return Err(Error::Unexpected {
            message: "source apply output has an empty source revision".to_string(),
        });
    }
    let rest = &rest[source_end..];

    let catalogue_marker = "\",\"catalogue_revision\":\"catalogue-revision:";
    let rest = rest
        .strip_prefix(catalogue_marker)
        .ok_or_else(|| Error::Unexpected {
            message: "source apply output must continue with the catalogue revision field"
                .to_string(),
        })?;
    let catalogue_end = rest.find('"').ok_or_else(|| Error::Unexpected {
        message: "source apply output has no catalogue revision terminator".to_string(),
    })?;
    let catalogue_id = &rest[..catalogue_end];
    if catalogue_id.is_empty() {
        return Err(Error::Unexpected {
            message: "source apply output has an empty catalogue revision".to_string(),
        });
    }
    let rest = &rest[catalogue_end..];

    let functions_marker = "\",\"functions\":[";
    let rest = rest
        .strip_prefix(functions_marker)
        .ok_or_else(|| Error::Unexpected {
            message: "source apply output must continue with the functions array".to_string(),
        })?;
    let (functions, tail) = parse_functions(rest)?;
    if tail != "}" {
        return Err(Error::Unexpected {
            message: "source apply output must close after the functions array".to_string(),
        });
    }

    Ok(ApplyDocument {
        source_revision: format!("source-revision:{source_id}"),
        catalogue_revision: format!("catalogue-revision:{catalogue_id}"),
        functions,
    })
}

/// Parse the exact `functions` array entries of the success document.
///
/// A parameter-free entry is
/// `{"qualified_name":["schema","function"],"function_id":"function:<id>"}`.
/// A parameterised entry appends exactly
/// `,"parameters":[{"name":"<name>","parameter_id":"parameter:<id>"},...]`
/// after the function identity. Entries are separated by commas and closed
/// by `]`. The parameters array is optional, non-empty, and preserves order.
fn parse_functions(text: &str) -> Result<(Vec<FunctionEntry>, &str), Error> {
    let mut rest = text;
    let mut functions = Vec::new();
    loop {
        rest = rest.strip_prefix('{').ok_or_else(|| Error::Unexpected {
            message: "source apply functions must be objects".to_string(),
        })?;
        let name_marker = "\"qualified_name\":[";
        rest = rest
            .strip_prefix(name_marker)
            .ok_or_else(|| Error::Unexpected {
                message: "source apply functions must carry a qualified name".to_string(),
            })?;
        let close = rest.find(']').ok_or_else(|| Error::Unexpected {
            message: "source apply qualified name is not closed".to_string(),
        })?;
        let parts = &rest[..close];
        let names: Vec<String> = parts
            .split("\",\"")
            .map(|part| part.trim_matches('"').to_string())
            .collect();
        if names.is_empty() || names.iter().any(|part| part.is_empty()) {
            return Err(Error::Unexpected {
                message: format!("source apply qualified name parts are invalid: {parts:?}"),
            });
        }
        rest = &rest[close + 1..];

        let id_marker = ",\"function_id\":\"";
        rest = rest
            .strip_prefix(id_marker)
            .ok_or_else(|| Error::Unexpected {
                message: "source apply functions must carry a function identity".to_string(),
            })?;
        let id_end = rest.find('"').ok_or_else(|| Error::Unexpected {
            message: "source apply function identity is not closed".to_string(),
        })?;
        let id = &rest[..id_end];
        if !id.starts_with("function:") || id.len() <= "function:".len() {
            return Err(Error::Unexpected {
                message: format!("source apply function identity is invalid: {id:?}"),
            });
        }
        rest = &rest[id_end + 1..];
        let (parameters, next) = match rest.strip_prefix(",\"parameters\":[") {
            Some(after_marker) => {
                if after_marker.starts_with(']') {
                    return Err(Error::Unexpected {
                        message: "source apply parameters must be a non-empty array".to_string(),
                    });
                }
                let mut parameters = Vec::new();
                let mut params_rest = after_marker;
                loop {
                    let (parameter, after) = parse_parameter(params_rest)?;
                    parameters.push(parameter);
                    params_rest = after;
                    match params_rest.strip_prefix(']') {
                        Some(tail) => {
                            rest = tail;
                            break;
                        }
                        None => {
                            params_rest =
                                params_rest
                                    .strip_prefix(',')
                                    .ok_or_else(|| Error::Unexpected {
                                        message: "source apply parameters must be comma separated"
                                            .to_string(),
                                    })?;
                        }
                    }
                }
                let rest = rest.strip_prefix('}').ok_or_else(|| Error::Unexpected {
                    message: "source apply function entry is not closed".to_string(),
                })?;
                (parameters, rest)
            }
            None => {
                let rest = rest.strip_prefix('}').ok_or_else(|| Error::Unexpected {
                    message: "source apply function entry is not closed".to_string(),
                })?;
                (Vec::new(), rest)
            }
        };
        rest = next;
        functions.push(FunctionEntry {
            names,
            function_id: id.to_string(),
            parameters,
        });

        match rest.strip_prefix(']') {
            Some(tail) => return Ok((functions, tail)),
            None => {
                rest = rest.strip_prefix(',').ok_or_else(|| Error::Unexpected {
                    message: "source apply functions must be comma separated".to_string(),
                })?;
            }
        }
    }
}

/// Parse one exact parameter object of a function entry.
///
/// The object is `{"name":"<name>","parameter_id":"parameter:<id>"}` with
/// exact key order and a canonical 26-character base32 parameter identity.
/// Every deviation is rejected with a closed message.
fn parse_parameter(text: &str) -> Result<(ParameterEntry, &str), Error> {
    const PARAMETER_ID_ALPHABET: &[u8] = b"0123456789abcdefghjkmnpqrstvwxyz";
    let rest = text.strip_prefix('{').ok_or_else(|| Error::Unexpected {
        message: "source apply parameters must be objects".to_string(),
    })?;
    let name_marker = "\"name\":\"";
    let rest = rest
        .strip_prefix(name_marker)
        .ok_or_else(|| Error::Unexpected {
            message: "source apply parameter must start with its name".to_string(),
        })?;
    let name_end = rest.find('"').ok_or_else(|| Error::Unexpected {
        message: "source apply parameter name is not closed".to_string(),
    })?;
    let name = &rest[..name_end];
    if name.is_empty() {
        return Err(Error::Unexpected {
            message: "source apply parameter name must be non-empty".to_string(),
        });
    }
    let rest = &rest[name_end..];
    let id_marker = "\",\"parameter_id\":\"parameter:";
    let rest = rest
        .strip_prefix(id_marker)
        .ok_or_else(|| Error::Unexpected {
            message: "source apply parameter must continue with its parameter identity".to_string(),
        })?;
    let id_end = rest.find('"').ok_or_else(|| Error::Unexpected {
        message: "source apply parameter identity is not closed".to_string(),
    })?;
    let id = &rest[..id_end];
    if id.len() != 26 {
        return Err(Error::Unexpected {
            message: "source apply parameter identity must be 26 canonical characters".to_string(),
        });
    }
    let mut final_value = 0_usize;
    for (index, character) in id.bytes().enumerate() {
        let Some(value) = PARAMETER_ID_ALPHABET
            .iter()
            .position(|candidate| *candidate == character)
        else {
            return Err(Error::Unexpected {
                message: format!(
                    "source apply parameter identity contains an invalid character: {character:?}"
                ),
            });
        };
        if index == 25 {
            final_value = value;
        }
    }
    if final_value & 0b11 != 0 {
        return Err(Error::Unexpected {
            message: "source apply parameter identity final character is not canonical".to_string(),
        });
    }
    let rest = &rest[id_end + 1..];
    let rest = rest.strip_prefix('}').ok_or_else(|| Error::Unexpected {
        message: "source apply parameter object is not closed".to_string(),
    })?;
    Ok((
        ParameterEntry {
            name: name.to_string(),
            parameter_id: format!("parameter:{id}"),
        },
        rest,
    ))
}

/// One exact apply document line with the given functions-array body.
fn apply_document_line(functions: &str) -> String {
    format!(
        "{{\"source_revision\":\"source-revision:s\",\"catalogue_revision\":\"catalogue-revision:c\",\"functions\":[{functions}]}}\n"
    )
}

#[test]
fn apply_document_parser_accepts_the_exact_zero_parameter_form() {
    let document = parse_apply_document(
        apply_document_line(
            "{\"qualified_name\":[\"schema\",\"fn\"],\"function_id\":\"function:abc\"}",
        )
        .as_bytes(),
    )
    .expect("the zero-parameter document must parse");
    assert_eq!(document.functions.len(), 1);
    let function = &document.functions[0];
    assert_eq!(function.names(), &["schema".to_string(), "fn".to_string()]);
    assert_eq!(function.function_id(), "function:abc");
    assert!(function.parameters().is_empty());
    assert_eq!(
        document
            .function_id(&["schema", "fn"])
            .expect("function identity"),
        "function:abc"
    );
}

#[test]
fn apply_document_parser_accepts_exact_ordered_parameters() {
    let document = parse_apply_document(
        apply_document_line(
            "{\"qualified_name\":[\"schema\",\"fn\"],\"function_id\":\"function:abc\",\"parameters\":[{\"name\":\"first\",\"parameter_id\":\"parameter:00000000000000000000000000\"},{\"name\":\"second\",\"parameter_id\":\"parameter:00000000000000000000000004\"}]}",
        )
        .as_bytes(),
    )
    .expect("the parameterised document must parse");
    assert_eq!(document.functions.len(), 1);
    let function = &document.functions[0];
    assert_eq!(function.function_id(), "function:abc");
    assert_eq!(function.parameters().len(), 2);
    assert_eq!(function.parameters()[0].name(), "first");
    assert_eq!(
        function.parameters()[0].parameter_id(),
        "parameter:00000000000000000000000000"
    );
    assert_eq!(function.parameters()[1].name(), "second");
    assert_eq!(
        function.parameters()[1].parameter_id(),
        "parameter:00000000000000000000000004"
    );
    assert_eq!(
        function
            .parameter_id("first")
            .expect("first parameter identity"),
        "parameter:00000000000000000000000000"
    );
    assert_eq!(
        function
            .parameter_id("second")
            .expect("second parameter identity"),
        "parameter:00000000000000000000000004"
    );
    assert_eq!(
        document
            .parameter_id(&["schema", "fn"], "first")
            .expect("first parameter identity by function"),
        "parameter:00000000000000000000000000"
    );
    assert_eq!(
        document
            .parameter_id(&["schema", "fn"], "second")
            .expect("second parameter identity by function"),
        "parameter:00000000000000000000000004"
    );
    assert!(
        document.parameter_id(&["schema", "fn"], "missing").is_err(),
        "an unknown parameter name must be rejected by the function accessor"
    );
}

#[test]
fn apply_document_parser_omits_parameters_only_when_absent() {
    let without = parse_apply_document(
        apply_document_line(
            "{\"qualified_name\":[\"schema\",\"fn\"],\"function_id\":\"function:abc\"}",
        )
        .as_bytes(),
    )
    .expect("the zero-parameter document must parse");
    assert!(without.functions[0].parameters().is_empty());

    let with_one = parse_apply_document(
        apply_document_line(
            "{\"qualified_name\":[\"schema\",\"fn\"],\"function_id\":\"function:abc\",\"parameters\":[{\"name\":\"p\",\"parameter_id\":\"parameter:00000000000000000000000000\"}]}",
        )
        .as_bytes(),
    )
    .expect("the one-parameter document must parse");
    assert_eq!(with_one.functions[0].parameters().len(), 1);
    assert_eq!(
        with_one.functions[0]
            .parameter_id("p")
            .expect("parameter identity"),
        "parameter:00000000000000000000000000"
    );
}

#[test]
fn apply_document_parser_rejects_invalid_parameter_shapes() {
    let cases = [
        "{\"qualified_name\":[\"schema\",\"fn\"],\"function_id\":\"function:abc\",\"parameters\":[]}",
        "{\"qualified_name\":[\"schema\",\"fn\"],\"function_id\":\"function:abc\",\"parameters\":[{\"parameter_id\":\"parameter:00000000000000000000000000\",\"name\":\"p\"}]}",
        "{\"qualified_name\":[\"schema\",\"fn\"],\"function_id\":\"function:abc\",\"parameters\":[{\"name\":\"p\",\"parameter_id\":\"parameter:00000000000000000000000000\",\"extra\":1}]}",
        "{\"qualified_name\":[\"schema\",\"fn\"],\"function_id\":\"function:abc\",\"parameters\":[{\"name\":\"p\",\"parameter_id\":\"id:x\"}]}",
        "{\"qualified_name\":[\"schema\",\"fn\"],\"function_id\":\"function:abc\",\"parameters\":[{\"name\":\",\"parameter_id\":\"parameter:00000000000000000000000000\"}]}",
        "{\"qualified_name\":[\"schema\",\"fn\"],\"function_id\":\"function:abc\",\"parameters\":[{\"name\":\"p\",\"parameter_id\":\"parameter:\"}]}",
        "{\"qualified_name\":[\"schema\",\"fn\"],\"function_id\":\"function:abc\",\"parameters\":[{\"name\":\"a\",\"parameter_id\":\"parameter:00000000000000000000000000\"}{\"name\":\"b\",\"parameter_id\":\"parameter:00000000000000000000000004\"}]}",
    ];
    for case in cases {
        let result = parse_apply_document(apply_document_line(case).as_bytes());
        assert!(
            matches!(result, Err(Error::Unexpected { .. })),
            "invalid parameter shape must be rejected: {case}"
        );
    }
}

#[test]
fn apply_document_parser_rejects_invalid_canonical_parameter_ids() {
    let cases = [
        "parameter:0000000000000000000000000",
        "parameter:000000000000000000000000000",
        "parameter:iiiiiiiiiiiiiiiiiiiiiiiiii",
        "parameter:llllllllllllllllllllllllll",
        "parameter:oooooooooooooooooooooooooo",
        "parameter:uuuuuuuuuuuuuuuuuuuuuuuuuu",
        "parameter:AAAAAAAAAAAAAAAAAAAAAAAAAA",
        "parameter:00000000000000000000000001",
    ];
    for parameter_id in cases {
        let document = format!(
            "{{\"source_revision\":\"source-revision:s\",\"catalogue_revision\":\"catalogue-revision:c\",\"functions\":[{{\"qualified_name\":[\"schema\",\"fn\"],\"function_id\":\"function:abc\",\"parameters\":[{{\"name\":\"p\",\"parameter_id\":\"{parameter_id}\"}}]}}]}}\n"
        );
        let result = parse_apply_document(document.as_bytes());
        assert!(
            matches!(result, Err(Error::Unexpected { .. })),
            "non-canonical parameter identity must be rejected: {parameter_id}"
        );
    }
}

#[test]
fn apply_document_parser_rejects_truncated_parameter_shapes() {
    let cases = [
        "{\"qualified_name\":[\"schema\",\"fn\"],\"function_id\":\"function:abc\",\"parameters\":",
        "{\"qualified_name\":[\"schema\",\"fn\"],\"function_id\":\"function:abc\",\"parameters\":[{\"name\":\"p",
        "{\"qualified_name\":[\"schema\",\"fn\"],\"function_id\":\"function:abc\",\"parameters\":[{\"name\":\"p\",\"parameter_id\":\"parameter:",
        "{\"qualified_name\":[\"schema\",\"fn\"],\"function_id\":\"function:abc\",\"parameters\":[{\"name\":\"p\",\"parameter_id\":\"parameter:00000000000000000000000000\"}",
        "{\"qualified_name\":[\"schema\",\"fn\"],\"function_id\":\"function:abc\",\"parameters\":[{\"name\":\"p\",\"parameter_id\":\"parameter:00000000000000000000000000\"}]junk}",
        "{\"qualified_name\":[\"schema\",\"fn\"],\"parameters\":[{\"name\":\"p\",\"parameter_id\":\"parameter:00000000000000000000000000\"}],\"function_id\":\"function:abc\"}",
    ];
    for case in cases {
        let result = parse_apply_document(apply_document_line(case).as_bytes());
        assert!(
            matches!(result, Err(Error::Unexpected { .. })),
            "truncated parameter shape must be rejected: {case}"
        );
    }
}

/// Errors from the installed-product machine and its assertions.
#[derive(Debug)]
enum Error {
    /// A docker command could not be spawned or fed input.
    Spawn {
        /// The label of the failed operation.
        label: &'static str,
        /// The spawn or write failure.
        io: io::Error,
    },
    /// A command exited with a non-zero status.
    CommandUnexpected {
        /// The label of the failed command.
        label: &'static str,
        /// The exit status.
        status: ExitStatus,
        /// The captured standard output.
        stdout: String,
        /// The captured standard error.
        stderr: String,
    },
    /// The frozen artifact could not be verified before transfer.
    Verify(ArtifactError),
    /// A bounded poll reached its deadline.
    Timeout {
        /// What failed to happen in time.
        what: &'static str,
    },
    /// An output or document did not match the exact expected shape.
    Unexpected {
        /// The closed failure detail.
        message: String,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Spawn { label, io } => write!(f, "{label} failed: {io}"),
            Error::CommandUnexpected {
                label,
                status,
                stdout,
                stderr,
            } => write!(
                f,
                "{label} failed with {status}\nstdout:\n{stdout}\nstderr:\n{stderr}"
            ),
            Error::Verify(source) => write!(f, "frozen package verification failed: {source}"),
            Error::Timeout { what } => write!(f, "{what}"),
            Error::Unexpected { message } => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Spawn { io, .. } => Some(io),
            Error::Verify(source) => Some(source),
            _ => None,
        }
    }
}

/// Prove the full installed data path of work ADR 0038.
///
/// The fixture is the exact checked-in `fixtures/product_test.orna` source.
/// Every product step runs through `/usr/bin/orna` public commands as the
/// `orna` service account. The test asserts:
///
/// * apply reports one committed pair and both function identities;
/// * raw calls are denied before the two explicit grants;
/// * the grants succeed and write nothing;
/// * raw INSERT returns one well-formed ORV object reference;
/// * raw SELECT returns the exact canonical Boolean TRUE value;
/// * repeated grants stay silent and idempotent;
/// * a failed apply preserves the active functions, grants, and rows;
/// * a second raw INSERT allocates a distinct object identity and the raw
///   SELECT returns two exact canonical Boolean TRUE values;
/// * restart preserves the exact two-row canonical SELECT output bytes;
/// * semantic apply activates new revisions while the function identities
///   and grants stay stable and existing rows are preserved;
/// * a third row with FALSE decodes with the two existing TRUE rows as one
///   unordered FALSE and two TRUE values, across another restart;
/// * reversion to the original fixture reactivates the original function
///   behaviour with stable identities and grants, retains all rows, and the
///   four rows decode as one unordered FALSE and three TRUE values across
///   another restart.
#[test]
#[ignore = "requires Docker, ORNA_SYSTEM_TEST_DEBIAN_PACKAGE, and the ADR 0038 commands in the installed orna executable"]
fn installed_source_apply_grants_raw_insert_and_persists_across_restart() {
    let package = std::env::var("ORNA_SYSTEM_TEST_DEBIAN_PACKAGE")
        .expect("ORNA_SYSTEM_TEST_DEBIAN_PACKAGE must point at the reproduced .deb package");
    let artifact = FrozenPackageArtifact::new(PackageFormat::Debian, &package)
        .expect("freeze the reproduced Debian package");
    let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("product_test.orna");
    let fixture = fs::read(&fixture_path).expect("read the checked-in product fixture");

    let machine = InstalledMachine::start(&artifact, &fixture)
        .expect("start the installed Debian test machine");

    // Apply the exact one-file fixture through the packaged executable.
    let apply = machine
        .run_as_orna(&["source", "apply", FIXTURE_PATH])
        .expect("run installed source apply");
    let apply = require_success("orna source apply", apply).expect("source apply must succeed");
    let document = parse_apply_document(&apply.stdout).expect("source apply JSON must parse");
    assert!(
        document.source_revision.starts_with("source-revision:"),
        "apply must report the committed source revision"
    );
    assert!(
        document
            .catalogue_revision
            .starts_with("catalogue-revision:"),
        "apply must report the committed catalogue revision"
    );
    let create_probe = document
        .function_id(&["product_test", "create_probe"])
        .expect("apply must report create_probe");
    let read_probes = document
        .function_id(&["product_test", "read_probes"])
        .expect("apply must report read_probes");

    // No application grant exists after apply: both raw calls are denied.
    for function in [create_probe, read_probes] {
        let denied = machine
            .run_as_orna(&["raw-call", function])
            .expect("run denied raw call");
        assert_denied("raw call before grant", denied).expect("raw call must be denied");
    }

    // The fixed-service administration command grants exactly these functions.
    for function in [create_probe, read_probes] {
        let granted = machine
            .run_as_orna(&["security", "grant-execute", function])
            .expect("run installed grant command");
        require_silent_success("orna security grant-execute", granted)
            .expect("grant must succeed silently");
    }

    // Parameter-free raw INSERT creates one object and returns its reference.
    let inserted = machine
        .run_as_orna(&["raw-call", create_probe])
        .expect("run raw insert call");
    let inserted =
        require_success("orna raw-call create_probe", inserted).expect("raw insert must succeed");
    let reference = parse_reference_envelope(&inserted.stdout)
        .expect("raw insert must return one ORV reference");
    assert!(
        reference.type_id != [0; 16],
        "the inserted object reference must name a real target type"
    );
    assert!(
        !reference.object_is_zero(),
        "the inserted object reference must name a real row"
    );

    // The first raw SELECT returns the exact canonical Boolean TRUE value.
    let before = machine
        .run_as_orna(&["raw-call", read_probes])
        .expect("run raw select call");
    assert_exact_boolean_true("orna raw-call read_probes", before)
        .expect("raw select must return the exact Boolean TRUE value");

    // Repeated fixed-service grants stay silent and idempotent.
    for function in [create_probe, read_probes] {
        let repeated = machine
            .run_as_orna(&["security", "grant-execute", function])
            .expect("run repeated installed grant command");
        require_silent_success("orna security grant-execute repeated", repeated)
            .expect("repeated grant must succeed silently");
    }

    // A failed apply must preserve the active revision, grants, and rows.
    let invalid_fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("invalid_product_test.orna");
    let invalid_fixture =
        fs::read(&invalid_fixture_path).expect("read the checked-in invalid fixture");
    machine
        .write_fixture(&invalid_fixture)
        .expect("replace the fixture with the invalid source");
    let failed = machine
        .run_as_orna(&["source", "apply", FIXTURE_PATH])
        .expect("run installed source apply on the invalid source");
    assert_source_apply_rejected(failed).expect("invalid source apply must be rejected");

    // The original function identities and grants survive the failed apply.
    let second = machine
        .run_as_orna(&["raw-call", create_probe])
        .expect("run raw insert call after failed apply");
    let second = require_success("orna raw-call create_probe after failed apply", second)
        .expect("raw insert must still succeed");
    let second_reference = parse_reference_envelope(&second.stdout)
        .expect("raw insert after failed apply must return one ORV reference");
    assert!(
        second_reference.type_id != [0; 16] && !second_reference.object_is_zero(),
        "the second inserted object reference must name a real row"
    );
    assert_ne!(
        second_reference.object, reference.object,
        "each raw INSERT must allocate a distinct object identity"
    );

    // Two stored rows emit exactly two canonical Boolean TRUE envelopes.
    let two_rows = machine
        .run_as_orna(&["raw-call", read_probes])
        .expect("run raw select call after second insert");
    let two_rows = require_success("orna raw-call read_probes two rows", two_rows)
        .expect("raw select must succeed after the second insert");
    assert!(
        two_rows.stderr.is_empty(),
        "raw select must keep standard error empty"
    );
    assert_eq!(
        two_rows.stdout.as_slice(),
        two_boolean_true_envelopes().as_slice(),
        "two rows must emit exactly two concatenated Boolean TRUE envelopes"
    );

    // Restart the installed service and prove both rows persist byte-identically.
    machine
        .restart_server()
        .expect("installed server must restart cleanly");
    let after = machine
        .run_as_orna(&["raw-call", read_probes])
        .expect("run raw select call after restart");
    let after = require_success("orna raw-call read_probes after restart", after)
        .expect("raw select must succeed after restart");
    assert!(
        after.stderr.is_empty(),
        "raw select after restart must keep standard error empty"
    );
    assert_eq!(
        after.stdout.as_slice(),
        two_rows.stdout.as_slice(),
        "restart must preserve the exact two-row canonical output bytes"
    );

    // The false fixture activates a new revision with stable identities.
    let false_fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("product_test_false.orna");
    let false_fixture = fs::read(&false_fixture_path).expect("read the checked-in false fixture");
    machine
        .write_fixture(&false_fixture)
        .expect("replace the fixture with the false source");
    let reapplied = machine
        .run_as_orna(&["source", "apply", FIXTURE_PATH])
        .expect("run installed source apply on the false fixture");
    let reapplied = require_success("orna source apply false fixture", reapplied)
        .expect("false source apply must succeed");
    let false_document =
        parse_apply_document(&reapplied.stdout).expect("false source apply JSON must parse");
    assert_ne!(
        false_document.source_revision, document.source_revision,
        "semantic apply must activate a new source revision"
    );
    assert_ne!(
        false_document.catalogue_revision, document.catalogue_revision,
        "semantic apply must activate a new catalogue revision"
    );
    let false_create_probe = false_document
        .function_id(&["product_test", "create_probe"])
        .expect("false apply must report create_probe");
    let false_read_probes = false_document
        .function_id(&["product_test", "read_probes"])
        .expect("false apply must report read_probes");
    assert_eq!(
        false_create_probe, create_probe,
        "create_probe identity must be stable across semantic apply"
    );
    assert_eq!(
        false_read_probes, read_probes,
        "read_probes identity must be stable across semantic apply"
    );

    // The existing grant still covers the stable identity: no re-grant needed.
    let third = machine
        .run_as_orna(&["raw-call", create_probe])
        .expect("run raw insert call after semantic apply");
    let third = require_success("orna raw-call create_probe after semantic apply", third)
        .expect("raw insert after semantic apply must succeed");
    let third_reference = parse_reference_envelope(&third.stdout)
        .expect("raw insert after semantic apply must return one ORV reference");
    assert!(
        third_reference.type_id != [0; 16] && !third_reference.object_is_zero(),
        "the third object reference must name a real row"
    );
    assert_ne!(
        third_reference.object, reference.object,
        "the third object must differ from the first object"
    );
    assert_ne!(
        third_reference.object, second_reference.object,
        "the third object must differ from the second object"
    );

    // Three rows decode as one FALSE and two TRUE values.
    let three_rows = machine
        .run_as_orna(&["raw-call", read_probes])
        .expect("run raw select call after semantic apply");
    let three_rows = require_success("orna raw-call read_probes three rows", three_rows)
        .expect("raw select after semantic apply must succeed");
    assert!(
        three_rows.stderr.is_empty(),
        "raw select after semantic apply must keep standard error empty"
    );
    let mut three_values = decode_boolean_envelopes(&three_rows.stdout)
        .expect("three rows must decode as three Boolean envelopes");
    assert_eq!(
        three_values.len(),
        3,
        "three rows must emit exactly three Boolean envelopes"
    );
    three_values.sort_unstable();
    assert_eq!(
        three_values,
        [false, true, true],
        "the three stored rows must hold one FALSE and two TRUE values"
    );

    // Restart preserves the same unordered Boolean multiset.
    machine
        .restart_server()
        .expect("installed server must restart cleanly after semantic apply");
    let three_rows_after = machine
        .run_as_orna(&["raw-call", read_probes])
        .expect("run raw select call after semantic apply restart");
    let three_rows_after = require_success(
        "orna raw-call read_probes after semantic apply restart",
        three_rows_after,
    )
    .expect("raw select after semantic apply restart must succeed");
    assert!(
        three_rows_after.stderr.is_empty(),
        "raw select after semantic apply restart must keep standard error empty"
    );
    let mut three_after = decode_boolean_envelopes(&three_rows_after.stdout)
        .expect("restart rows must decode as three Boolean envelopes");
    assert_eq!(
        three_after.len(),
        3,
        "restart must emit exactly three Boolean envelopes"
    );
    three_after.sort_unstable();
    assert_eq!(
        three_after,
        [false, true, true],
        "restart must preserve the same unordered Boolean multiset"
    );

    // Reapply the original fixture: reversion reactivates TRUE behaviour.
    machine
        .write_fixture(&fixture)
        .expect("replace the fixture with the original source");
    let reverted = machine
        .run_as_orna(&["source", "apply", FIXTURE_PATH])
        .expect("run installed source apply on the original fixture");
    let reverted = require_success("orna source apply reverted", reverted)
        .expect("reverted source apply must succeed");
    let reverted_document =
        parse_apply_document(&reverted.stdout).expect("reverted source apply JSON must parse");
    assert_ne!(
        reverted_document.source_revision, false_document.source_revision,
        "reversion must advance the source revision again"
    );
    assert_ne!(
        reverted_document.catalogue_revision, false_document.catalogue_revision,
        "reversion must advance the catalogue revision again"
    );
    assert_eq!(
        reverted_document
            .function_id(&["product_test", "create_probe"])
            .expect("reverted apply must report create_probe"),
        create_probe,
        "reversion must keep the create_probe identity"
    );
    assert_eq!(
        reverted_document
            .function_id(&["product_test", "read_probes"])
            .expect("reverted apply must report read_probes"),
        read_probes,
        "reversion must keep the read_probes identity"
    );

    // No new grant: the original grants survive the reversion.
    let fourth = machine
        .run_as_orna(&["raw-call", create_probe])
        .expect("run raw insert call after the reversion");
    let fourth = require_success("orna raw-call create_probe after the reversion", fourth)
        .expect("raw insert after the reversion must succeed");
    let fourth_reference = parse_reference_envelope(&fourth.stdout)
        .expect("raw insert after the reversion must return one ORV reference");
    assert!(
        fourth_reference.type_id != [0; 16] && !fourth_reference.object_is_zero(),
        "the fourth inserted object reference must name a real row"
    );
    assert_ne!(
        fourth_reference.object, reference.object,
        "the fourth raw INSERT must create a distinct object"
    );
    assert_ne!(
        fourth_reference.object, second_reference.object,
        "the fourth raw INSERT must create a distinct object"
    );
    assert_ne!(
        fourth_reference.object, third_reference.object,
        "the fourth raw INSERT must create a distinct object"
    );

    // Four stored rows emit the unordered Boolean multiset TRUE, TRUE, TRUE, FALSE.
    let four_rows = machine
        .run_as_orna(&["raw-call", read_probes])
        .expect("run raw select call after the fourth insert");
    let four_rows = require_success("orna raw-call read_probes four rows", four_rows)
        .expect("raw select after the fourth insert must succeed");
    assert!(
        four_rows.stderr.is_empty(),
        "raw select after the fourth insert must keep standard error empty"
    );
    let mut four_values = decode_boolean_envelopes(&four_rows.stdout)
        .expect("four rows must decode as complete Boolean envelopes");
    let mut expected_four = vec![true, true, true, false];
    four_values.sort();
    expected_four.sort();
    assert_eq!(
        four_values, expected_four,
        "four rows must hold the unordered Boolean multiset TRUE, TRUE, TRUE, FALSE"
    );

    // Restart again and require the same unordered four-value multiset.
    machine
        .restart_server()
        .expect("installed server must restart cleanly after the reversion");
    let four_rows_after = machine
        .run_as_orna(&["raw-call", read_probes])
        .expect("run raw select call after the reversion restart");
    let four_rows_after = require_success(
        "orna raw-call read_probes after the reversion restart",
        four_rows_after,
    )
    .expect("raw select after the reversion restart must succeed");
    assert!(
        four_rows_after.stderr.is_empty(),
        "raw select after the reversion restart must keep standard error empty"
    );
    let mut four_values_after = decode_boolean_envelopes(&four_rows_after.stdout)
        .expect("reversion restart rows must decode as complete Boolean envelopes");
    four_values_after.sort();
    assert_eq!(
        four_values_after, expected_four,
        "restart must preserve the unordered four-value Boolean multiset"
    );
}

/// Prove that an ADR 0006 replay-safe field rename keeps the installed
/// product's public data path observable through the original function
/// identities, and that the rename requires its transition evidence.
///
/// The test installs the original `product_test.orna` fixture, applies it,
/// grants both functions, inserts one TRUE row, and then:
///
/// * submits the renamed shape without `ALTER TYPE ... RENAME FIELD`
///   evidence and requires the installed product to reject it with the exact
///   apply-commit failure line, proving the shape change cannot commit;
/// * proves the rejection changes nothing: the original create identity
///   inserts a second distinct TRUE object of the same reference type, and
///   the original read identity returns exactly two TRUE values;
/// * applies the evidence-bearing renamed fixture: source and catalogue
///   revisions advance while the complete two-entry function vector stays
///   exactly equal to the original apply, and a replay of the same renamed
///   source keeps that complete mapping exact;
/// * without any re-grant, reads the two pre-rename TRUE rows, inserts one
///   FALSE object through the original create identity, and decodes the
///   unordered multiset [false, true, true];
/// * restarts the installed server and reads the same unordered multiset,
///   then proves the create grant also survived by inserting a second FALSE
///   object and decoding the unordered multiset [false, false, true, true].
///
/// All observations go through the packaged `/usr/bin/orna` public commands
/// and raw-call ORV envelopes. The test proves stored values and grants
/// remain observable across the rename; it does not inspect physical table
/// columns, storage, or field identities.
#[test]
#[ignore = "requires Docker, ORNA_SYSTEM_TEST_DEBIAN_PACKAGE, and the ADR 0006 rename support in the installed orna executable"]
fn installed_field_rename_preserves_function_identities_and_values_across_restart() {
    let package = std::env::var("ORNA_SYSTEM_TEST_DEBIAN_PACKAGE")
        .expect("ORNA_SYSTEM_TEST_DEBIAN_PACKAGE must point at the reproduced .deb package");
    let artifact = FrozenPackageArtifact::new(PackageFormat::Debian, &package)
        .expect("freeze the reproduced Debian package");
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let original = fs::read(fixtures.join("product_test.orna")).expect("read the product fixture");
    let renamed = fs::read(fixtures.join("product_test_renamed.orna"))
        .expect("read the renamed product fixture");
    let without_evidence = fs::read(fixtures.join("product_test_renamed_without_evidence.orna"))
        .expect("read the no-evidence renamed product fixture");

    let machine = InstalledMachine::start(&artifact, &original)
        .expect("start the installed Debian test machine");

    // Apply the original fixture and capture the complete function vector.
    let apply = machine
        .run_as_orna(&["source", "apply", FIXTURE_PATH])
        .expect("run installed source apply");
    let apply = require_success("orna source apply", apply).expect("source apply must succeed");
    let document = parse_apply_document(&apply.stdout).expect("source apply JSON must parse");
    assert_eq!(
        document.functions.len(),
        2,
        "the original apply must report exactly two function entries"
    );
    let original_functions = document.functions.clone();
    let create_probe = document
        .function_id(&["product_test", "create_probe"])
        .expect("apply must report create_probe");
    let read_probes = document
        .function_id(&["product_test", "read_probes"])
        .expect("apply must report read_probes");

    // Grant both functions through the fixed-service command.
    for function in [create_probe, read_probes] {
        let granted = machine
            .run_as_orna(&["security", "grant-execute", function])
            .expect("run installed grant command");
        require_silent_success("orna security grant-execute", granted)
            .expect("grant must succeed silently");
    }

    // One pre-rename row with the exact canonical Boolean TRUE value.
    let inserted = machine
        .run_as_orna(&["raw-call", create_probe])
        .expect("run raw insert call");
    let inserted =
        require_success("orna raw-call create_probe", inserted).expect("raw insert must succeed");
    let reference = parse_reference_envelope(&inserted.stdout)
        .expect("raw insert must return one ORV reference");
    assert!(
        reference.type_id != [0; 16] && !reference.object_is_zero(),
        "the inserted object reference must name a real row"
    );
    assert_exact_boolean_true(
        "orna raw-call read_probes before rename",
        machine
            .run_as_orna(&["raw-call", read_probes])
            .expect("run raw select call before rename"),
    )
    .expect("raw select must return the exact Boolean TRUE value");

    // The no-evidence fixture keeps the renamed shape but omits the ALTER
    // evidence. The installed product must reject it at the apply-commit
    // boundary rather than silently changing the object field set.
    machine
        .write_fixture(&without_evidence)
        .expect("replace the fixture with the no-evidence source");
    let rejected = machine
        .run_as_orna(&["source", "apply", FIXTURE_PATH])
        .expect("run installed source apply on the no-evidence source");
    assert!(
        rejected.status.code() == Some(1),
        "no-evidence apply must exit 1, got {}",
        rejected.status
    );
    assert!(
        rejected.stdout.is_empty(),
        "no-evidence apply must emit no standard output"
    );
    assert_eq!(
        rejected.stderr,
        b"orna: source apply did not commit\n".as_slice(),
        "no-evidence apply must fail with the exact apply-commit rejection line"
    );

    // The rejection changes nothing: the original create identity inserts a
    // second distinct TRUE object of the same reference type.
    let second = machine
        .run_as_orna(&["raw-call", create_probe])
        .expect("run raw insert call after the rejected apply");
    let second = require_success(
        "orna raw-call create_probe after the rejected apply",
        second,
    )
    .expect("raw insert after the rejected apply must succeed");
    let second_reference = parse_reference_envelope(&second.stdout)
        .expect("raw insert after the rejected apply must return one ORV reference");
    assert!(
        second_reference.type_id != [0; 16] && !second_reference.object_is_zero(),
        "the post-rejection object reference must name a real row"
    );
    assert_ne!(
        second_reference.object, reference.object,
        "the post-rejection raw INSERT must allocate a distinct object identity"
    );
    assert_eq!(
        second_reference.type_id, reference.type_id,
        "the post-rejection raw INSERT must reference the same stable object type"
    );

    // The original read identity still returns exactly two TRUE values.
    let two_after_rejection = machine
        .run_as_orna(&["raw-call", read_probes])
        .expect("run raw select call after the rejected apply");
    let two_after_rejection = require_success(
        "orna raw-call read_probes after the rejected apply",
        two_after_rejection,
    )
    .expect("raw select after the rejected apply must succeed");
    assert!(
        two_after_rejection.stderr.is_empty(),
        "raw select after the rejected apply must keep standard error empty"
    );
    let mut two_values = decode_boolean_envelopes(&two_after_rejection.stdout)
        .expect("post-rejection rows must decode as two Boolean envelopes");
    assert_eq!(
        two_values.len(),
        2,
        "post-rejection rows must emit exactly two Boolean envelopes"
    );
    two_values.sort_unstable();
    assert_eq!(
        two_values,
        [true, true],
        "the two pre-rename rows must both hold TRUE"
    );

    // The evidence-bearing renamed fixture applies: revisions advance and
    // the complete two-entry function vector stays exactly the original.
    machine
        .write_fixture(&renamed)
        .expect("replace the fixture with the renamed source");
    let renamed_apply = machine
        .run_as_orna(&["source", "apply", FIXTURE_PATH])
        .expect("run installed source apply on the renamed source");
    let renamed_apply = require_success("orna source apply renamed", renamed_apply)
        .expect("renamed source apply must succeed");
    let renamed_document =
        parse_apply_document(&renamed_apply.stdout).expect("renamed source apply JSON must parse");
    assert_ne!(
        renamed_document.source_revision, document.source_revision,
        "the rename must advance the source revision"
    );
    assert_ne!(
        renamed_document.catalogue_revision, document.catalogue_revision,
        "the rename must advance the catalogue revision"
    );
    assert_eq!(
        renamed_document.functions, original_functions,
        "the rename apply must report the complete original function vector"
    );

    // Replaying the exact same renamed source keeps the complete mapping
    // exact. Revision advance on replay is not assumed.
    machine
        .write_fixture(&renamed)
        .expect("replace the fixture with the same renamed source");
    let replay = machine
        .run_as_orna(&["source", "apply", FIXTURE_PATH])
        .expect("run installed source apply on the replay source");
    let replay =
        require_success("orna source apply replay", replay).expect("renamed replay must succeed");
    let replay_document =
        parse_apply_document(&replay.stdout).expect("renamed replay JSON must parse");
    assert_eq!(
        replay_document.functions, original_functions,
        "the renamed replay must keep the complete original function vector"
    );

    // Without any re-grant, the two pre-rename TRUE rows remain observable.
    let two_before_false = machine
        .run_as_orna(&["raw-call", read_probes])
        .expect("run raw select call after the rename");
    let two_before_false = require_success(
        "orna raw-call read_probes after the rename",
        two_before_false,
    )
    .expect("raw select after the rename must succeed");
    assert!(
        two_before_false.stderr.is_empty(),
        "raw select after the rename must keep standard error empty"
    );
    let mut two_before_values = decode_boolean_envelopes(&two_before_false.stdout)
        .expect("pre-rename rows must decode as two Boolean envelopes");
    assert_eq!(
        two_before_values.len(),
        2,
        "the two pre-rename rows must emit exactly two Boolean envelopes"
    );
    two_before_values.sort_unstable();
    assert_eq!(
        two_before_values,
        [true, true],
        "the two pre-rename rows must both hold TRUE"
    );

    // The original create identity now inserts one FALSE object with the
    // same reference type and a distinct object identity.
    let third = machine
        .run_as_orna(&["raw-call", create_probe])
        .expect("run raw insert call after the rename");
    let third = require_success("orna raw-call create_probe after the rename", third)
        .expect("raw insert after the rename must succeed");
    let third_reference = parse_reference_envelope(&third.stdout)
        .expect("raw insert after the rename must return one ORV reference");
    assert!(
        third_reference.type_id != [0; 16] && !third_reference.object_is_zero(),
        "the post-rename object reference must name a real row"
    );
    assert_ne!(
        third_reference.object, reference.object,
        "the post-rename raw INSERT must allocate a distinct object identity"
    );
    assert_ne!(
        third_reference.object, second_reference.object,
        "the post-rename raw INSERT must allocate a distinct object identity"
    );
    assert_eq!(
        third_reference.type_id, reference.type_id,
        "the post-rename raw INSERT must reference the same stable object type"
    );

    // Three rows decode as the unordered multiset [false, true, true].
    let three = machine
        .run_as_orna(&["raw-call", read_probes])
        .expect("run raw select call after the post-rename insert");
    let three = require_success("orna raw-call read_probes three rows", three)
        .expect("raw select must succeed after the post-rename insert");
    assert!(
        three.stderr.is_empty(),
        "raw select must keep standard error empty"
    );
    let mut three_values = decode_boolean_envelopes(&three.stdout)
        .expect("three rows must decode as three Boolean envelopes");
    assert_eq!(
        three_values.len(),
        3,
        "three rows must emit exactly three Boolean envelopes"
    );
    three_values.sort_unstable();
    assert_eq!(
        three_values,
        [false, true, true],
        "the three rows must hold one FALSE and two TRUE values"
    );

    // Restart preserves the same unordered multiset.
    machine
        .restart_server()
        .expect("installed server must restart cleanly");
    let three_after = machine
        .run_as_orna(&["raw-call", read_probes])
        .expect("run raw select call after restart");
    let three_after = require_success("orna raw-call read_probes after restart", three_after)
        .expect("raw select after restart must succeed");
    assert!(
        three_after.stderr.is_empty(),
        "raw select after restart must keep standard error empty"
    );
    let mut three_after_values = decode_boolean_envelopes(&three_after.stdout)
        .expect("restart rows must decode as three Boolean envelopes");
    assert_eq!(
        three_after_values.len(),
        3,
        "restart must emit exactly three Boolean envelopes"
    );
    three_after_values.sort_unstable();
    assert_eq!(
        three_after_values,
        [false, true, true],
        "restart must preserve the unordered three-value multiset"
    );

    // After restart the original create grant survived too: a second FALSE
    // object with the same reference type and a distinct object identity.
    let fourth = machine
        .run_as_orna(&["raw-call", create_probe])
        .expect("run raw insert call after restart");
    let fourth = require_success("orna raw-call create_probe after restart", fourth)
        .expect("raw insert after restart must succeed");
    let fourth_reference = parse_reference_envelope(&fourth.stdout)
        .expect("raw insert after restart must return one ORV reference");
    assert!(
        fourth_reference.type_id != [0; 16] && !fourth_reference.object_is_zero(),
        "the post-restart object reference must name a real row"
    );
    assert_ne!(
        fourth_reference.object, reference.object,
        "the post-restart raw INSERT must allocate a distinct object identity"
    );
    assert_ne!(
        fourth_reference.object, second_reference.object,
        "the post-restart raw INSERT must allocate a distinct object identity"
    );
    assert_ne!(
        fourth_reference.object, third_reference.object,
        "the post-restart raw INSERT must allocate a distinct object identity"
    );
    assert_eq!(
        fourth_reference.type_id, reference.type_id,
        "the post-restart raw INSERT must reference the same stable object type"
    );

    // Four rows decode as the unordered multiset [false, false, true, true].
    let four = machine
        .run_as_orna(&["raw-call", read_probes])
        .expect("run raw select call after the post-restart insert");
    let four = require_success("orna raw-call read_probes four rows", four)
        .expect("raw select must succeed after the post-restart insert");
    assert!(
        four.stderr.is_empty(),
        "raw select must keep standard error empty"
    );
    let mut four_values = decode_boolean_envelopes(&four.stdout)
        .expect("four rows must decode as four Boolean envelopes");
    assert_eq!(
        four_values.len(),
        4,
        "four rows must emit exactly four Boolean envelopes"
    );
    four_values.sort_unstable();
    assert_eq!(
        four_values,
        [false, false, true, true],
        "the four rows must hold two FALSE and two TRUE values"
    );
}

/// Prove that an omitted nullable object field persists as a typed Boolean
/// NULL while a present nullable field persists its exact FALSE value.
///
/// The test installs the exact checked-in `product_test_nullable.orna`
/// fixture, applies it, and requires exactly four qualified-name/function-ID
/// mappings. Both readers are denied before any grant. After granting all
/// four functions it proves through the public raw-call path only:
///
/// * both readers initially exit 0 with empty standard output and error;
/// * `create_omitted` returns one valid ORV1 reference and `read_omitted`
///   returns exactly one typed Boolean NULL envelope while `read_present`
///   stays empty;
/// * `create_present` returns a second valid reference with the same target
///   type and a distinct object identity, `read_omitted` stays byte-identical,
///   and `read_present` returns exactly Boolean FALSE;
/// * a restart keeps both reader outputs byte-identical.
///
/// All observations go through the packaged `/usr/bin/orna` public commands
/// and raw-call ORV envelopes. The test makes no claim about private rows,
/// SQL columns, field identities, or physical storage.
#[test]
#[ignore = "requires Docker, ORNA_SYSTEM_TEST_DEBIAN_PACKAGE, and the installed orna executable"]
fn installed_nullable_field_persists_omitted_and_present_values_across_restart() {
    let package = std::env::var("ORNA_SYSTEM_TEST_DEBIAN_PACKAGE")
        .expect("ORNA_SYSTEM_TEST_DEBIAN_PACKAGE must point at the reproduced .deb package");
    let artifact = FrozenPackageArtifact::new(PackageFormat::Debian, &package)
        .expect("freeze the reproduced Debian package");
    let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("product_test_nullable.orna");
    let fixture = fs::read(&fixture_path).expect("read the checked-in nullable fixture");

    let machine = InstalledMachine::start(&artifact, &fixture)
        .expect("start the installed Debian test machine");

    // Apply the exact fixture and require all four function mappings.
    let apply = machine
        .run_as_orna(&["source", "apply", FIXTURE_PATH])
        .expect("run installed source apply");
    let apply = require_success("orna source apply", apply).expect("source apply must succeed");
    assert!(
        apply.stderr.is_empty(),
        "source apply must keep standard error empty"
    );
    let document = parse_apply_document(&apply.stdout).expect("source apply JSON must parse");
    assert_eq!(
        document.functions.len(),
        4,
        "apply must report exactly four function entries"
    );
    let expected_order = [
        vec!["nullable_test".to_string(), "create_omitted".to_string()],
        vec!["nullable_test".to_string(), "create_present".to_string()],
        vec!["nullable_test".to_string(), "read_omitted".to_string()],
        vec!["nullable_test".to_string(), "read_present".to_string()],
    ];
    let actual_order = document
        .functions
        .iter()
        .map(|function| function.names().to_vec())
        .collect::<Vec<_>>();
    assert_eq!(
        actual_order, expected_order,
        "apply must report the four function entries sorted by qualified name"
    );
    let create_omitted = document
        .function_id(&["nullable_test", "create_omitted"])
        .expect("apply must report create_omitted");
    let create_present = document
        .function_id(&["nullable_test", "create_present"])
        .expect("apply must report create_present");
    let read_omitted = document
        .function_id(&["nullable_test", "read_omitted"])
        .expect("apply must report read_omitted");
    let read_present = document
        .function_id(&["nullable_test", "read_present"])
        .expect("apply must report read_present");

    // Both readers are denied before any grant.
    for function in [read_omitted, read_present] {
        let denied = machine
            .run_as_orna(&["raw-call", function])
            .expect("run denied raw call");
        assert_denied("raw call before grant", denied).expect("raw call must be denied");
    }

    // Grant all four functions through the fixed-service command.
    for function in [create_omitted, create_present, read_omitted, read_present] {
        let granted = machine
            .run_as_orna(&["security", "grant-execute", function])
            .expect("run installed grant command");
        require_silent_success("orna security grant-execute", granted)
            .expect("grant must succeed silently");
    }

    // Both readers initially exit 0 with empty streams.
    for function in [read_omitted, read_present] {
        let empty = machine
            .run_as_orna(&["raw-call", function])
            .expect("run empty raw select");
        require_silent_success("orna raw-call empty select", empty)
            .expect("empty select must exit 0 with empty streams");
    }

    // create_omitted inserts marker FALSE and omits the optional field.
    let first = machine
        .run_as_orna(&["raw-call", create_omitted])
        .expect("run raw omitted insert");
    let first = require_success("orna raw-call create_omitted", first)
        .expect("omitted insert must succeed");
    assert!(
        first.stderr.is_empty(),
        "omitted insert must keep standard error empty"
    );
    let reference = parse_reference_envelope(&first.stdout)
        .expect("omitted insert must return one ORV reference");
    assert!(
        reference.type_id != [0; 16] && !reference.object_is_zero(),
        "the omitted insert must return a real object reference"
    );

    // read_omitted returns exactly one typed Boolean NULL envelope.
    let null_output = machine
        .run_as_orna(&["raw-call", read_omitted])
        .expect("run raw null select");
    let null_output = require_success("orna raw-call read_omitted", null_output)
        .expect("null select must succeed");
    assert!(
        null_output.stderr.is_empty(),
        "null select must keep standard error empty"
    );
    assert_eq!(
        null_output.stdout.as_slice(),
        boolean_orv1_envelope(None).as_slice(),
        "read_omitted must emit exactly one Boolean NULL envelope"
    );

    // read_present still returns nothing.
    let present_empty = machine
        .run_as_orna(&["raw-call", read_present])
        .expect("run raw present empty select");
    require_silent_success("orna raw-call read_present empty", present_empty)
        .expect("present select must stay empty");

    // create_present inserts marker TRUE with optional FALSE.
    let second = machine
        .run_as_orna(&["raw-call", create_present])
        .expect("run raw present insert");
    let second = require_success("orna raw-call create_present", second)
        .expect("present insert must succeed");
    assert!(
        second.stderr.is_empty(),
        "present insert must keep standard error empty"
    );
    let second_reference = parse_reference_envelope(&second.stdout)
        .expect("present insert must return one ORV reference");
    assert!(
        second_reference.type_id != [0; 16] && !second_reference.object_is_zero(),
        "the present insert must return a real object reference"
    );
    assert_eq!(
        second_reference.type_id, reference.type_id,
        "both creates must reference the same object type"
    );
    assert_ne!(
        second_reference.object, reference.object,
        "each create must allocate a distinct object identity"
    );

    // read_omitted stays byte-identical.
    let null_again = machine
        .run_as_orna(&["raw-call", read_omitted])
        .expect("run raw null select again");
    let null_again = require_success("orna raw-call read_omitted again", null_again)
        .expect("null select must succeed");
    assert!(
        null_again.stderr.is_empty(),
        "null select must keep standard error empty"
    );
    assert_eq!(
        null_again.stdout.as_slice(),
        null_output.stdout.as_slice(),
        "read_omitted output must be byte-identical"
    );

    // read_present returns exactly Boolean FALSE.
    let false_output = machine
        .run_as_orna(&["raw-call", read_present])
        .expect("run raw present select");
    let false_output = require_success("orna raw-call read_present", false_output)
        .expect("present select must succeed");
    assert!(
        false_output.stderr.is_empty(),
        "present select must keep standard error empty"
    );
    assert_eq!(
        false_output.stdout.as_slice(),
        boolean_orv1_envelope(Some(false)).as_slice(),
        "read_present must emit exactly one Boolean FALSE envelope"
    );

    // Restart preserves both reader outputs byte-identically.
    machine
        .restart_server()
        .expect("installed server must restart cleanly");
    let null_after = machine
        .run_as_orna(&["raw-call", read_omitted])
        .expect("run raw null select after restart");
    let null_after = require_success("orna raw-call read_omitted after restart", null_after)
        .expect("null select after restart must succeed");
    assert!(
        null_after.stderr.is_empty(),
        "null select after restart must keep standard error empty"
    );
    assert_eq!(
        null_after.stdout.as_slice(),
        null_output.stdout.as_slice(),
        "read_omitted must stay byte-identical after restart"
    );
    let false_after = machine
        .run_as_orna(&["raw-call", read_present])
        .expect("run raw present select after restart");
    let false_after = require_success("orna raw-call read_present after restart", false_after)
        .expect("present select after restart must succeed");
    assert!(
        false_after.stderr.is_empty(),
        "present select after restart must keep standard error empty"
    );
    assert_eq!(
        false_after.stdout.as_slice(),
        false_output.stdout.as_slice(),
        "read_present must stay byte-identical after restart"
    );
}

/// Prove that two schemas with identically named object types and functions
/// stay isolated through the installed product's public grant and raw-call
/// path, and that both relations persist across a restart.
///
/// The test installs the exact checked-in `product_test_schemas.orna`
/// fixture, applies it, and requires exactly four sorted qualified-name
/// mappings with pairwise distinct function identities. It then proves:
///
/// * all four raw calls are denied before any grant;
/// * granting only the north create/read leaves both south calls denied;
/// * north creates one TRUE row and reads exactly TRUE while south stays
///   empty and denied;
/// * granting only the south reader lets south read empty even though north
///   already holds a row, while south create stays denied;
/// * granting the south create inserts one FALSE row with a different target
///   type from north, and each relation reads its own exact value;
/// * a second north create returns the same target type with a distinct
///   object identity, north reads exactly two TRUE envelopes, and south stays
///   exactly one FALSE;
/// * after a restart both reader outputs stay byte-identical, the south
///   create grant survives, a second south create returns the same target
///   type with a distinct object identity, and south reads exactly two FALSE
///   envelopes while north stays exactly two TRUE.
///
/// All observations go through the packaged `/usr/bin/orna` public commands
/// and raw-call ORV envelopes. The test makes no claim about private rows,
/// SQL columns, field identities, physical storage, or row ordering.
#[test]
#[ignore = "requires Docker, ORNA_SYSTEM_TEST_DEBIAN_PACKAGE, and the installed orna executable"]
fn installed_schema_isolation_persists_separate_relations_across_restart() {
    let package = std::env::var("ORNA_SYSTEM_TEST_DEBIAN_PACKAGE")
        .expect("ORNA_SYSTEM_TEST_DEBIAN_PACKAGE must point at the reproduced .deb package");
    let artifact = FrozenPackageArtifact::new(PackageFormat::Debian, &package)
        .expect("freeze the reproduced Debian package");
    let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("product_test_schemas.orna");
    let fixture = fs::read(&fixture_path).expect("read the checked-in schemas fixture");

    let machine = InstalledMachine::start(&artifact, &fixture)
        .expect("start the installed Debian test machine");

    // Apply the exact fixture and require the four sorted mappings.
    let apply = machine
        .run_as_orna(&["source", "apply", FIXTURE_PATH])
        .expect("run installed source apply");
    let apply = require_success("orna source apply", apply).expect("source apply must succeed");
    assert!(
        apply.stderr.is_empty(),
        "source apply must keep standard error empty"
    );
    let document = parse_apply_document(&apply.stdout).expect("source apply JSON must parse");
    let expected_order = [
        vec!["north".to_string(), "create_entry".to_string()],
        vec!["north".to_string(), "read_entries".to_string()],
        vec!["south".to_string(), "create_entry".to_string()],
        vec!["south".to_string(), "read_entries".to_string()],
    ];
    let actual_order = document
        .functions
        .iter()
        .map(|function| function.names().to_vec())
        .collect::<Vec<_>>();
    assert_eq!(
        actual_order, expected_order,
        "apply must report the four function entries sorted by qualified name"
    );
    let north_create = document
        .function_id(&["north", "create_entry"])
        .expect("apply must report north.create_entry");
    let north_read = document
        .function_id(&["north", "read_entries"])
        .expect("apply must report north.read_entries");
    let south_create = document
        .function_id(&["south", "create_entry"])
        .expect("apply must report south.create_entry");
    let south_read = document
        .function_id(&["south", "read_entries"])
        .expect("apply must report south.read_entries");
    let identities = [north_create, north_read, south_create, south_read];
    for (index, left) in identities.iter().enumerate() {
        for right in &identities[index + 1..] {
            assert_ne!(
                left, right,
                "the four function identities must be pairwise distinct"
            );
        }
    }

    // All four raw calls are denied before any grant.
    for function in identities {
        let denied = machine
            .run_as_orna(&["raw-call", function])
            .expect("run denied raw call");
        assert_denied("raw call before grant", denied).expect("raw call must be denied");
    }

    // Grant only the north create and read.
    for function in [north_create, north_read] {
        let granted = machine
            .run_as_orna(&["security", "grant-execute", function])
            .expect("run installed grant command");
        require_silent_success("orna security grant-execute north", granted)
            .expect("north grant must succeed silently");
    }

    // Both south calls remain denied while north read succeeds empty.
    for function in [south_create, south_read] {
        let denied = machine
            .run_as_orna(&["raw-call", function])
            .expect("run denied south raw call");
        assert_denied("south raw call after north grant", denied)
            .expect("south raw call must remain denied");
    }
    let north_empty = machine
        .run_as_orna(&["raw-call", north_read])
        .expect("run empty north raw select");
    require_silent_success("orna raw-call north read empty", north_empty)
        .expect("north read must succeed empty");

    // north.create_entry returns a real reference and north reads TRUE.
    let n1_call = machine
        .run_as_orna(&["raw-call", north_create])
        .expect("run north raw insert");
    let n1_call = require_success("orna raw-call north create_entry", n1_call)
        .expect("north insert must succeed");
    assert!(
        n1_call.stderr.is_empty(),
        "north insert must keep standard error empty"
    );
    let n1 = parse_reference_envelope(&n1_call.stdout)
        .expect("north insert must return one ORV reference");
    assert!(
        n1.type_id != [0; 16] && !n1.object_is_zero(),
        "the north insert must return a real object reference"
    );
    assert_exact_boolean_true(
        "orna raw-call north read_entries",
        machine
            .run_as_orna(&["raw-call", north_read])
            .expect("run north raw select"),
    )
    .expect("north read must return the exact Boolean TRUE value");

    // Grant only the south reader: south reads empty, south create stays denied.
    let granted = machine
        .run_as_orna(&["security", "grant-execute", south_read])
        .expect("run installed grant command");
    require_silent_success("orna security grant-execute south read", granted)
        .expect("south read grant must succeed silently");
    let denied = machine
        .run_as_orna(&["raw-call", south_create])
        .expect("run denied south create raw call");
    assert_denied("south create after read grant", denied)
        .expect("south create must remain denied");
    let south_empty = machine
        .run_as_orna(&["raw-call", south_read])
        .expect("run empty south raw select");
    require_silent_success("orna raw-call south read empty", south_empty)
        .expect("south read must succeed empty even though north holds a row");

    // Grant the south create and insert one FALSE row.
    let granted = machine
        .run_as_orna(&["security", "grant-execute", south_create])
        .expect("run installed grant command");
    require_silent_success("orna security grant-execute south create", granted)
        .expect("south create grant must succeed silently");
    let s1_call = machine
        .run_as_orna(&["raw-call", south_create])
        .expect("run south raw insert");
    let s1_call = require_success("orna raw-call south create_entry", s1_call)
        .expect("south insert must succeed");
    assert!(
        s1_call.stderr.is_empty(),
        "south insert must keep standard error empty"
    );
    let s1 = parse_reference_envelope(&s1_call.stdout)
        .expect("south insert must return one ORV reference");
    assert!(
        s1.type_id != [0; 16] && !s1.object_is_zero(),
        "the south insert must return a real object reference"
    );
    assert_ne!(
        n1.type_id, s1.type_id,
        "north and south objects must use different target types"
    );
    let south_false = machine
        .run_as_orna(&["raw-call", south_read])
        .expect("run south raw select");
    let south_false = require_success("orna raw-call south read_entries", south_false)
        .expect("south read must succeed");
    assert!(
        south_false.stderr.is_empty(),
        "south read must keep standard error empty"
    );
    assert_eq!(
        south_false.stdout.as_slice(),
        boolean_orv1_envelope(Some(false)).as_slice(),
        "south read must emit exactly one Boolean FALSE envelope"
    );
    assert_exact_boolean_true(
        "orna raw-call north read_entries after south insert",
        machine
            .run_as_orna(&["raw-call", north_read])
            .expect("run north raw select after south insert"),
    )
    .expect("north read must remain the exact Boolean TRUE value");

    // A second north create keeps the target type and a distinct object.
    let n2_call = machine
        .run_as_orna(&["raw-call", north_create])
        .expect("run second north raw insert");
    let n2_call = require_success("orna raw-call north create_entry again", n2_call)
        .expect("second north insert must succeed");
    assert!(
        n2_call.stderr.is_empty(),
        "second north insert must keep standard error empty"
    );
    let n2 = parse_reference_envelope(&n2_call.stdout)
        .expect("second north insert must return one ORV reference");
    assert!(
        n2.type_id != [0; 16] && !n2.object_is_zero(),
        "the second north insert must return a real object reference"
    );
    assert_eq!(
        n2.type_id, n1.type_id,
        "north inserts must reference the same target type"
    );
    assert_ne!(
        n2.object, n1.object,
        "north inserts must allocate distinct object identities"
    );
    let north_two = machine
        .run_as_orna(&["raw-call", north_read])
        .expect("run north raw select after second insert");
    let north_two = require_success("orna raw-call north read_entries two rows", north_two)
        .expect("north read must succeed");
    assert!(
        north_two.stderr.is_empty(),
        "north read must keep standard error empty"
    );
    assert_eq!(
        north_two.stdout.as_slice(),
        two_boolean_true_envelopes().as_slice(),
        "north read must emit exactly two Boolean TRUE envelopes"
    );
    let south_one = machine
        .run_as_orna(&["raw-call", south_read])
        .expect("run south raw select after north insert");
    let south_one = require_success("orna raw-call south read_entries one row", south_one)
        .expect("south read must succeed");
    assert!(
        south_one.stderr.is_empty(),
        "south read must keep standard error empty"
    );
    assert_eq!(
        south_one.stdout.as_slice(),
        boolean_orv1_envelope(Some(false)).as_slice(),
        "south read must stay exactly one Boolean FALSE envelope"
    );

    // Restart preserves both reader outputs byte-identically.
    machine
        .restart_server()
        .expect("installed server must restart cleanly");
    let north_after = machine
        .run_as_orna(&["raw-call", north_read])
        .expect("run north raw select after restart");
    let north_after = require_success(
        "orna raw-call north read_entries after restart",
        north_after,
    )
    .expect("north read after restart must succeed");
    assert!(
        north_after.stderr.is_empty(),
        "north read after restart must keep standard error empty"
    );
    assert_eq!(
        north_after.stdout.as_slice(),
        north_two.stdout.as_slice(),
        "north read must stay byte-identical after restart"
    );
    let south_after = machine
        .run_as_orna(&["raw-call", south_read])
        .expect("run south raw select after restart");
    let south_after = require_success(
        "orna raw-call south read_entries after restart",
        south_after,
    )
    .expect("south read after restart must succeed");
    assert!(
        south_after.stderr.is_empty(),
        "south read after restart must keep standard error empty"
    );
    assert_eq!(
        south_after.stdout.as_slice(),
        south_one.stdout.as_slice(),
        "south read must stay byte-identical after restart"
    );

    // The south create grant survived: a second south FALSE object.
    let s2_call = machine
        .run_as_orna(&["raw-call", south_create])
        .expect("run second south raw insert");
    let s2_call = require_success("orna raw-call south create_entry again", s2_call)
        .expect("second south insert must succeed");
    assert!(
        s2_call.stderr.is_empty(),
        "second south insert must keep standard error empty"
    );
    let s2 = parse_reference_envelope(&s2_call.stdout)
        .expect("second south insert must return one ORV reference");
    assert!(
        s2.type_id != [0; 16] && !s2.object_is_zero(),
        "the second south insert must return a real object reference"
    );
    assert_eq!(
        s2.type_id, s1.type_id,
        "south inserts must reference the same target type"
    );
    assert_ne!(
        s2.object, s1.object,
        "south inserts must allocate distinct object identities"
    );

    // South reads exactly two FALSE envelopes; north stays exactly two TRUE.
    let mut south_two_expected = boolean_orv1_envelope(Some(false));
    south_two_expected.extend(boolean_orv1_envelope(Some(false)));
    let south_two = machine
        .run_as_orna(&["raw-call", south_read])
        .expect("run south raw select after second insert");
    let south_two = require_success("orna raw-call south read_entries two rows", south_two)
        .expect("south read must succeed");
    assert!(
        south_two.stderr.is_empty(),
        "south read must keep standard error empty"
    );
    assert_eq!(
        south_two.stdout.as_slice(),
        south_two_expected.as_slice(),
        "south read must emit exactly two Boolean FALSE envelopes"
    );
    let north_final = machine
        .run_as_orna(&["raw-call", north_read])
        .expect("run north raw select after south insert");
    let north_final = require_success("orna raw-call north read_entries final", north_final)
        .expect("north read must succeed");
    assert!(
        north_final.stderr.is_empty(),
        "north read must keep standard error empty"
    );
    assert_eq!(
        north_final.stdout.as_slice(),
        north_after.stdout.as_slice(),
        "north read must stay exactly two TRUE envelopes"
    );
}

/// Run one granted raw reader and decode its complete Boolean envelopes.
///
/// The reader must exit 0 with empty standard error. The decoded values are
/// returned in wire order; callers sort before comparing multisets.
fn decode_reader_values(
    machine: &InstalledMachine,
    function: &str,
    label: &'static str,
) -> Result<Vec<bool>, Error> {
    run_reader_and_decode(
        machine,
        function,
        label,
        decode_boolean_envelopes,
        "complete Boolean envelopes",
    )
}

/// Run one granted raw reader and decode its complete envelope stream.
///
/// The reader must exit 0 with empty standard error. The decoder receives the
/// exact standard-output bytes and must return `Some` only for one complete
/// valid stream.
fn run_reader_and_decode<T>(
    machine: &InstalledMachine,
    function: &str,
    label: &'static str,
    decode: impl FnOnce(&[u8]) -> Option<T>,
    description: &'static str,
) -> Result<T, Error> {
    let output = machine
        .run_as_orna(&["raw-call", function])
        .map_err(|error| Error::Spawn {
            label: "spawn raw reader call",
            io: match error {
                Error::Spawn { io, .. } => io,
                _ => unreachable!("run_as_orna only returns spawn errors"),
            },
        })?;
    let output = require_success(label, output)?;
    if !output.stderr.is_empty() {
        return Err(Error::Unexpected {
            message: format!(
                "{label} must keep standard error empty, got {} bytes",
                output.stderr.len()
            ),
        });
    }
    decode(&output.stdout).ok_or_else(|| Error::Unexpected {
        message: format!("{label} output must decode as {description}"),
    })
}

/// Prove that `SELECT DISTINCT` eliminates duplicate stored values through
/// the installed product's public raw-call path, and that the exact fixture
/// reapplies cleanly without changing the observable rows.
///
/// The test installs the exact checked-in `product_test_distinct.orna`
/// fixture, applies it, and requires exactly four sorted qualified-name
/// mappings with pairwise distinct function identities. After granting all
/// four functions it proves through public raw-call output only:
///
/// * both readers initially succeed with empty streams;
/// * two TRUE inserts decode as [true, true] through `read_all` and [true]
///   through `read_distinct`, with distinct object identities;
/// * one FALSE insert makes `read_all` decode as [false, true, true] while
///   `read_distinct` stays [false, true];
/// * a second FALSE insert keeps `read_all` at [false, false, true, true]
///   while `read_distinct` remains [false, true], proving causal duplicate
///   elimination;
/// * reapplying the exact same fixture succeeds with empty stderr and the
///   complete four-entry function vector stays exactly equal, without any
///   re-grant, and the rows stay observable;
/// * after a restart the same sorted multisets remain, and one more TRUE
///   insert through the surviving grant makes `read_all` decode as
///   [false, false, true, true, true] while `read_distinct` stays
///   [false, true].
///
/// All observations go through the packaged `/usr/bin/orna` public commands
/// and raw-call ORV envelopes. The test makes no claim about physical
/// storage, row ordering, or private rows.
#[test]
#[ignore = "requires Docker, ORNA_SYSTEM_TEST_DEBIAN_PACKAGE, and the installed orna executable"]
fn installed_distinct_eliminates_duplicate_stored_values_across_reapply_and_restart() {
    let package = std::env::var("ORNA_SYSTEM_TEST_DEBIAN_PACKAGE")
        .expect("ORNA_SYSTEM_TEST_DEBIAN_PACKAGE must point at the reproduced .deb package");
    let artifact = FrozenPackageArtifact::new(PackageFormat::Debian, &package)
        .expect("freeze the reproduced Debian package");
    let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("product_test_distinct.orna");
    let fixture = fs::read(&fixture_path).expect("read the checked-in distinct fixture");

    let machine = InstalledMachine::start(&artifact, &fixture)
        .expect("start the installed Debian test machine");

    // Apply the exact fixture and require the four sorted mappings.
    let apply = machine
        .run_as_orna(&["source", "apply", FIXTURE_PATH])
        .expect("run installed source apply");
    let apply = require_success("orna source apply", apply).expect("source apply must succeed");
    assert!(
        apply.stderr.is_empty(),
        "source apply must keep standard error empty"
    );
    let document = parse_apply_document(&apply.stdout).expect("source apply JSON must parse");
    let expected_order = [
        vec!["distinct_test".to_string(), "create_false".to_string()],
        vec!["distinct_test".to_string(), "create_true".to_string()],
        vec!["distinct_test".to_string(), "read_all".to_string()],
        vec!["distinct_test".to_string(), "read_distinct".to_string()],
    ];
    let actual_order = document
        .functions
        .iter()
        .map(|function| function.names().to_vec())
        .collect::<Vec<_>>();
    assert_eq!(
        actual_order, expected_order,
        "apply must report the four function entries sorted by qualified name"
    );
    let create_true = document
        .function_id(&["distinct_test", "create_true"])
        .expect("apply must report create_true");
    let create_false = document
        .function_id(&["distinct_test", "create_false"])
        .expect("apply must report create_false");
    let read_all = document
        .function_id(&["distinct_test", "read_all"])
        .expect("apply must report read_all");
    let read_distinct = document
        .function_id(&["distinct_test", "read_distinct"])
        .expect("apply must report read_distinct");
    let identities = [create_true, create_false, read_all, read_distinct];
    for (index, left) in identities.iter().enumerate() {
        for right in &identities[index + 1..] {
            assert_ne!(
                left, right,
                "the four function identities must be pairwise distinct"
            );
        }
    }

    // Grant all four functions through the fixed-service command.
    for function in identities {
        let granted = machine
            .run_as_orna(&["security", "grant-execute", function])
            .expect("run installed grant command");
        require_silent_success("orna security grant-execute", granted)
            .expect("grant must succeed silently");
    }

    // Both readers initially succeed with empty streams.
    for function in [read_all, read_distinct] {
        let empty = machine
            .run_as_orna(&["raw-call", function])
            .expect("run empty raw select");
        require_silent_success("orna raw-call empty select", empty)
            .expect("empty select must exit 0 with empty streams");
    }

    // Two TRUE inserts with distinct object identities.
    let mut true_references: Vec<OrvReference> = Vec::new();
    for _ in 0..2 {
        let inserted = machine
            .run_as_orna(&["raw-call", create_true])
            .expect("run true raw insert");
        let inserted = require_success("orna raw-call create_true", inserted)
            .expect("true insert must succeed");
        assert!(
            inserted.stderr.is_empty(),
            "true insert must keep standard error empty"
        );
        let reference = parse_reference_envelope(&inserted.stdout)
            .expect("true insert must return one ORV reference");
        assert!(
            reference.type_id != [0; 16] && !reference.object_is_zero(),
            "the true insert must return a real object reference"
        );
        for earlier in &true_references {
            assert_ne!(
                reference.object, earlier.object,
                "each true insert must allocate a distinct object identity"
            );
        }
        true_references.push(reference);
    }
    let t1 = &true_references[0];
    let t2 = &true_references[1];
    assert_eq!(
        t2.type_id, t1.type_id,
        "true inserts must reference the same target type"
    );

    let mut all_two = decode_reader_values(&machine, read_all, "orna raw-call read_all")
        .expect("read_all must decode");
    all_two.sort_unstable();
    assert_eq!(
        all_two,
        [true, true],
        "read_all must decode as exactly two TRUE values"
    );
    let distinct_two = decode_reader_values(&machine, read_distinct, "orna raw-call read_distinct")
        .expect("read_distinct must decode");
    assert_eq!(
        distinct_two,
        [true],
        "read_distinct must decode as exactly one TRUE value"
    );

    // One FALSE insert with the same target type and a new object identity.
    let inserted = machine
        .run_as_orna(&["raw-call", create_false])
        .expect("run false raw insert");
    let inserted =
        require_success("orna raw-call create_false", inserted).expect("false insert must succeed");
    assert!(
        inserted.stderr.is_empty(),
        "false insert must keep standard error empty"
    );
    let f1 = parse_reference_envelope(&inserted.stdout)
        .expect("false insert must return one ORV reference");
    assert!(
        f1.type_id != [0; 16] && !f1.object_is_zero(),
        "the false insert must return a real object reference"
    );
    assert_eq!(
        f1.type_id, t1.type_id,
        "false inserts must reference the same target type"
    );
    assert_ne!(
        f1.object, t1.object,
        "the false insert must allocate a distinct object identity"
    );
    assert_ne!(
        f1.object, t2.object,
        "the false insert must allocate a distinct object identity"
    );

    let mut all_three = decode_reader_values(&machine, read_all, "orna raw-call read_all")
        .expect("read_all must decode");
    all_three.sort_unstable();
    assert_eq!(
        all_three,
        [false, true, true],
        "read_all must decode as one FALSE and two TRUE values"
    );
    let mut distinct_three =
        decode_reader_values(&machine, read_distinct, "orna raw-call read_distinct")
            .expect("read_distinct must decode");
    distinct_three.sort_unstable();
    assert_eq!(
        distinct_three,
        [false, true],
        "read_distinct must decode as exactly FALSE and TRUE"
    );

    // A second FALSE insert: duplicate elimination stays causal.
    let inserted = machine
        .run_as_orna(&["raw-call", create_false])
        .expect("run second false raw insert");
    let inserted = require_success("orna raw-call create_false again", inserted)
        .expect("second false insert must succeed");
    assert!(
        inserted.stderr.is_empty(),
        "second false insert must keep standard error empty"
    );
    let f2 = parse_reference_envelope(&inserted.stdout)
        .expect("second false insert must return one ORV reference");
    assert!(
        f2.type_id != [0; 16] && !f2.object_is_zero(),
        "the second false insert must return a real object reference"
    );
    assert_eq!(
        f2.type_id, t1.type_id,
        "false inserts must reference the same target type"
    );
    for reference in [t1, t2, &f1] {
        assert_ne!(
            f2.object, reference.object,
            "the second false insert must allocate a distinct object identity"
        );
    }

    let mut all_four = decode_reader_values(&machine, read_all, "orna raw-call read_all")
        .expect("read_all must decode");
    all_four.sort_unstable();
    assert_eq!(
        all_four,
        [false, false, true, true],
        "read_all must decode as two FALSE and two TRUE values"
    );
    let mut distinct_four =
        decode_reader_values(&machine, read_distinct, "orna raw-call read_distinct")
            .expect("read_distinct must decode");
    distinct_four.sort_unstable();
    assert_eq!(
        distinct_four,
        [false, true],
        "read_distinct must still decode as exactly FALSE and TRUE"
    );

    // Reapply the exact same fixture: success, empty stderr, exact vector.
    let replay = machine
        .run_as_orna(&["source", "apply", FIXTURE_PATH])
        .expect("run installed source apply on the same fixture");
    let replay =
        require_success("orna source apply replay", replay).expect("distinct replay must succeed");
    assert!(
        replay.stderr.is_empty(),
        "distinct replay must keep standard error empty"
    );
    let replay_document =
        parse_apply_document(&replay.stdout).expect("distinct replay JSON must parse");
    assert_eq!(
        replay_document.functions, document.functions,
        "the replay must keep the complete four-entry function vector"
    );

    // No re-grant: the rows stay observable through the same identities.
    let mut all_after_replay = decode_reader_values(&machine, read_all, "orna raw-call read_all")
        .expect("read_all must decode after replay");
    all_after_replay.sort_unstable();
    assert_eq!(
        all_after_replay,
        [false, false, true, true],
        "read_all must stay two FALSE and two TRUE after replay"
    );
    let mut distinct_after_replay = decode_reader_values(
        &machine,
        read_distinct,
        "orna raw-call read_distinct after replay",
    )
    .expect("read_distinct must decode after replay");
    distinct_after_replay.sort_unstable();
    assert_eq!(
        distinct_after_replay,
        [false, true],
        "read_distinct must stay FALSE and TRUE after replay"
    );

    // Restart preserves the same sorted multisets.
    machine
        .restart_server()
        .expect("installed server must restart cleanly");
    let mut all_after_restart =
        decode_reader_values(&machine, read_all, "orna raw-call read_all after restart")
            .expect("read_all must decode after restart");
    all_after_restart.sort_unstable();
    assert_eq!(
        all_after_restart,
        [false, false, true, true],
        "read_all must stay two FALSE and two TRUE after restart"
    );
    let mut distinct_after_restart = decode_reader_values(
        &machine,
        read_distinct,
        "orna raw-call read_distinct after restart",
    )
    .expect("read_distinct must decode after restart");
    distinct_after_restart.sort_unstable();
    assert_eq!(
        distinct_after_restart,
        [false, true],
        "read_distinct must stay FALSE and TRUE after restart"
    );

    // The create_true grant survived: one more TRUE object.
    let inserted = machine
        .run_as_orna(&["raw-call", create_true])
        .expect("run true raw insert after restart");
    let inserted = require_success("orna raw-call create_true after restart", inserted)
        .expect("true insert after restart must succeed");
    assert!(
        inserted.stderr.is_empty(),
        "true insert after restart must keep standard error empty"
    );
    let t3 = parse_reference_envelope(&inserted.stdout)
        .expect("true insert after restart must return one ORV reference");
    assert!(
        t3.type_id != [0; 16] && !t3.object_is_zero(),
        "the post-restart true insert must return a real object reference"
    );
    assert_eq!(
        t3.type_id, t1.type_id,
        "the post-restart true insert must reference the same target type"
    );
    for reference in [t1, t2, &f1, &f2] {
        assert_ne!(
            t3.object, reference.object,
            "the post-restart true insert must allocate a distinct object identity"
        );
    }

    let mut all_final = decode_reader_values(&machine, read_all, "orna raw-call read_all")
        .expect("read_all must decode");
    all_final.sort_unstable();
    assert_eq!(
        all_final,
        [false, false, true, true, true],
        "read_all must decode as two FALSE and three TRUE values"
    );
    let mut distinct_final =
        decode_reader_values(&machine, read_distinct, "orna raw-call read_distinct")
            .expect("read_distinct must decode");
    distinct_final.sort_unstable();
    assert_eq!(
        distinct_final,
        [false, true],
        "read_distinct must stay FALSE and TRUE"
    );
}

/// Decode a stream of complete canonical ORV1 reference envelopes in order.
///
/// Returns `None` when any envelope is malformed or trailing bytes remain.
fn decode_reference_envelopes(bytes: &[u8]) -> Option<Vec<OrvReference>> {
    if !bytes.len().is_multiple_of(41) {
        return None;
    }
    bytes
        .chunks_exact(41)
        .map(parse_reference_envelope)
        .collect::<Result<Vec<_>, _>>()
        .ok()
}

/// The sorted object identities of the given references.
///
/// Object identities are the unique public part of an ORV reference, so the
/// sorted vector is an order-independent multiset.
fn sorted_reference_objects(references: &[&OrvReference]) -> Vec<[u8; 16]> {
    let mut objects = references
        .iter()
        .map(|reference| reference.object)
        .collect::<Vec<_>>();
    objects.sort_unstable();
    objects
}

/// Require one granted raw reference reader to return exactly the given
/// reference multiset.
///
/// The reader must exit 0 with empty standard error, its output must decode
/// as complete ORV1 reference envelopes, the envelope count must equal the
/// expected count, every envelope must use the same target type as the first
/// expected reference, and the decoded object identities must equal the
/// expected object identities as an unordered multiset.
fn assert_reference_reader_returns(
    machine: &InstalledMachine,
    function: &str,
    expected: &[&OrvReference],
    label: &'static str,
) -> Result<(), Error> {
    let output = machine
        .run_as_orna(&["raw-call", function])
        .map_err(|error| Error::Spawn {
            label: "spawn raw reference reader call",
            io: match error {
                Error::Spawn { io, .. } => io,
                _ => unreachable!("run_as_orna only returns spawn errors"),
            },
        })?;
    let output = require_success(label, output)?;
    if !output.stderr.is_empty() {
        return Err(Error::Unexpected {
            message: format!(
                "{label} must keep standard error empty, got {} bytes",
                output.stderr.len()
            ),
        });
    }
    let decoded = decode_reference_envelopes(&output.stdout).ok_or_else(|| Error::Unexpected {
        message: format!("{label} output must decode as complete reference envelopes"),
    })?;
    if decoded.len() != expected.len() {
        return Err(Error::Unexpected {
            message: format!(
                "{label} must return exactly {} reference envelopes, got {}",
                expected.len(),
                decoded.len()
            ),
        });
    }
    let Some(first) = expected.first() else {
        return Err(Error::Unexpected {
            message: format!("{label} requires at least one expected reference"),
        });
    };
    if decoded
        .iter()
        .any(|reference| reference.type_id != first.type_id)
    {
        return Err(Error::Unexpected {
            message: format!("{label} must use one uniform target type"),
        });
    }
    if sorted_reference_objects(&decoded.iter().collect::<Vec<_>>())
        != sorted_reference_objects(expected)
    {
        return Err(Error::Unexpected {
            message: format!("{label} must return exactly the expected references"),
        });
    }
    Ok(())
}

/// Prove that `SELECT REF(entry)` returns the stored object references
/// through the installed product's public raw-call path, without any row
/// ordering assumption, and that the exact fixture reapplies cleanly.
///
/// The test installs the exact checked-in `product_test_references.orna`
/// fixture, applies it, and requires exactly three sorted qualified-name
/// mappings with pairwise distinct function identities. It then proves:
///
/// * the raw reader is denied before any grant;
/// * after granting all three functions the reader succeeds empty;
/// * `create_true` returns one reference A and the reader returns exactly A;
/// * `create_false` returns a reference B with the same target type and a
///   distinct nonzero object identity, and the reader returns the unordered
///   multiset {A, B};
/// * reapplying the exact same fixture keeps the complete function mapping,
///   grants, and rows;
/// * a restart keeps the unordered multiset {A, B};
/// * after the restart both surviving grants stay usable without any re-grant:
///   `create_true` returns a distinct reference C, then `create_false`
///   returns a distinct reference D, and the reader returns the unordered
///   multiset {A, B, C, D}.
///
/// All observations go through the packaged `/usr/bin/orna` public commands
/// and raw-call ORV envelopes. The test makes no claim about physical
/// storage, private rows, or row ordering.
#[test]
#[ignore = "requires Docker, ORNA_SYSTEM_TEST_DEBIAN_PACKAGE, and the installed orna executable"]
fn installed_reference_reader_returns_stored_object_references_across_replay_and_restart() {
    let package = std::env::var("ORNA_SYSTEM_TEST_DEBIAN_PACKAGE")
        .expect("ORNA_SYSTEM_TEST_DEBIAN_PACKAGE must point at the reproduced .deb package");
    let artifact = FrozenPackageArtifact::new(PackageFormat::Debian, &package)
        .expect("freeze the reproduced Debian package");
    let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("product_test_references.orna");
    let fixture = fs::read(&fixture_path).expect("read the checked-in references fixture");

    let machine = InstalledMachine::start(&artifact, &fixture)
        .expect("start the installed Debian test machine");

    // Apply the exact fixture and require the three sorted mappings.
    let apply = machine
        .run_as_orna(&["source", "apply", FIXTURE_PATH])
        .expect("run installed source apply");
    let apply = require_success("orna source apply", apply).expect("source apply must succeed");
    assert!(
        apply.stderr.is_empty(),
        "source apply must keep standard error empty"
    );
    let document = parse_apply_document(&apply.stdout).expect("source apply JSON must parse");
    let expected_order = [
        vec!["reference_test".to_string(), "create_false".to_string()],
        vec!["reference_test".to_string(), "create_true".to_string()],
        vec!["reference_test".to_string(), "read_entries".to_string()],
    ];
    let actual_order = document
        .functions
        .iter()
        .map(|function| function.names().to_vec())
        .collect::<Vec<_>>();
    assert_eq!(
        actual_order, expected_order,
        "apply must report the three function entries sorted by qualified name"
    );
    let create_true = document
        .function_id(&["reference_test", "create_true"])
        .expect("apply must report create_true");
    let create_false = document
        .function_id(&["reference_test", "create_false"])
        .expect("apply must report create_false");
    let read_entries = document
        .function_id(&["reference_test", "read_entries"])
        .expect("apply must report read_entries");
    let identities = [create_true, create_false, read_entries];
    for (index, left) in identities.iter().enumerate() {
        for right in &identities[index + 1..] {
            assert_ne!(
                left, right,
                "the three function identities must be pairwise distinct"
            );
        }
    }

    // Source apply grants nothing: every raw call is denied before any grant.
    for function in identities {
        let denied = machine
            .run_as_orna(&["raw-call", function])
            .expect("run denied raw call");
        assert_denied("raw call before grant", denied).expect("raw call must be denied");
    }

    // Grant all three functions through the fixed-service command.
    for function in identities {
        let granted = machine
            .run_as_orna(&["security", "grant-execute", function])
            .expect("run installed grant command");
        require_silent_success("orna security grant-execute", granted)
            .expect("grant must succeed silently");
    }

    // The reader initially succeeds with empty output.
    let empty = machine
        .run_as_orna(&["raw-call", read_entries])
        .expect("run empty raw select");
    require_silent_success("orna raw-call read_entries empty", empty)
        .expect("empty read must exit 0 with empty streams");

    // create_true returns one reference A and the reader returns exactly A.
    let create_true_call = machine
        .run_as_orna(&["raw-call", create_true])
        .expect("run true raw insert");
    let create_true_call = require_success("orna raw-call create_true", create_true_call)
        .expect("true insert must succeed");
    assert!(
        create_true_call.stderr.is_empty(),
        "true insert must keep standard error empty"
    );
    let a = parse_reference_envelope(&create_true_call.stdout)
        .expect("true insert must return one ORV reference");
    assert!(
        a.type_id != [0; 16] && !a.object_is_zero(),
        "the true insert must return a real object reference"
    );
    let read_a = machine
        .run_as_orna(&["raw-call", read_entries])
        .expect("run raw select for one reference");
    let read_a = require_success("orna raw-call read_entries one reference", read_a)
        .expect("read must succeed");
    assert!(
        read_a.stderr.is_empty(),
        "read must keep standard error empty"
    );
    assert_eq!(
        read_a.stdout.as_slice(),
        create_true_call.stdout.as_slice(),
        "read must return exactly the reference A envelope"
    );

    // create_false returns B with the same target type and a distinct object.
    let create_false_call = machine
        .run_as_orna(&["raw-call", create_false])
        .expect("run false raw insert");
    let create_false_call = require_success("orna raw-call create_false", create_false_call)
        .expect("false insert must succeed");
    assert!(
        create_false_call.stderr.is_empty(),
        "false insert must keep standard error empty"
    );
    let b = parse_reference_envelope(&create_false_call.stdout)
        .expect("false insert must return one ORV reference");
    assert!(
        b.type_id != [0; 16] && !b.object_is_zero(),
        "the false insert must return a real object reference"
    );
    assert_eq!(
        b.type_id, a.type_id,
        "both creates must reference the same target type"
    );
    assert_ne!(
        b.object, a.object,
        "the false insert must allocate a distinct object identity"
    );

    // The reader returns the unordered multiset {A, B}.
    assert_reference_reader_returns(
        &machine,
        read_entries,
        &[&a, &b],
        "orna raw-call read_entries two references",
    )
    .expect("read must return exactly the references A and B");

    // Exact source replay keeps the complete mapping, grants, and rows.
    let replay = machine
        .run_as_orna(&["source", "apply", FIXTURE_PATH])
        .expect("run installed source apply on the same fixture");
    let replay = require_success("orna source apply replay", replay)
        .expect("references replay must succeed");
    assert!(
        replay.stderr.is_empty(),
        "references replay must keep standard error empty"
    );
    let replay_document =
        parse_apply_document(&replay.stdout).expect("references replay JSON must parse");
    assert_eq!(
        replay_document.functions, document.functions,
        "the replay must keep the complete three-entry function vector"
    );
    assert_reference_reader_returns(
        &machine,
        read_entries,
        &[&a, &b],
        "orna raw-call read_entries after replay",
    )
    .expect("read after replay must return exactly the references A and B");

    // Restart keeps the unordered multiset {A, B}.
    machine
        .restart_server()
        .expect("installed server must restart cleanly");
    assert_reference_reader_returns(
        &machine,
        read_entries,
        &[&a, &b],
        "orna raw-call read_entries after restart",
    )
    .expect("read after restart must return exactly the references A and B");

    // A post-restart create_true returns distinct C; read returns {A, B, C}.
    let create_true_after = machine
        .run_as_orna(&["raw-call", create_true])
        .expect("run true raw insert after restart");
    let create_true_after =
        require_success("orna raw-call create_true after restart", create_true_after)
            .expect("true insert after restart must succeed");
    assert!(
        create_true_after.stderr.is_empty(),
        "true insert after restart must keep standard error empty"
    );
    let c = parse_reference_envelope(&create_true_after.stdout)
        .expect("true insert after restart must return one ORV reference");
    assert!(
        c.type_id != [0; 16] && !c.object_is_zero(),
        "the post-restart true insert must return a real object reference"
    );
    assert_eq!(
        c.type_id, a.type_id,
        "the post-restart true insert must reference the same target type"
    );
    assert_ne!(
        c.object, a.object,
        "the post-restart true insert must allocate a distinct object identity"
    );
    assert_ne!(
        c.object, b.object,
        "the post-restart true insert must allocate a distinct object identity"
    );

    assert_reference_reader_returns(
        &machine,
        read_entries,
        &[&a, &b, &c],
        "orna raw-call read_entries three references",
    )
    .expect("read must return exactly the references A, B, and C");

    // The post-restart create_false grant survived too: distinct D.
    let create_false_after = machine
        .run_as_orna(&["raw-call", create_false])
        .expect("run false raw insert after restart");
    let create_false_after = require_success(
        "orna raw-call create_false after restart",
        create_false_after,
    )
    .expect("false insert after restart must succeed");
    assert!(
        create_false_after.stderr.is_empty(),
        "false insert after restart must keep standard error empty"
    );
    let d = parse_reference_envelope(&create_false_after.stdout)
        .expect("false insert after restart must return one ORV reference");
    assert!(
        d.type_id != [0; 16] && !d.object_is_zero(),
        "the post-restart false insert must return a real object reference"
    );
    assert_eq!(
        d.type_id, a.type_id,
        "the post-restart false insert must reference the same target type"
    );
    for reference in [&a, &b, &c] {
        assert_ne!(
            d.object, reference.object,
            "the post-restart false insert must allocate a distinct object identity"
        );
    }

    assert_reference_reader_returns(
        &machine,
        read_entries,
        &[&a, &b, &c, &d],
        "orna raw-call read_entries four references",
    )
    .expect("read must return exactly the references A, B, C, and D");
}

/// Decode a stream of complete canonical ORV1 Boolean or Boolean-NULL
/// envelopes in order.
///
/// Each envelope must start with the ORV1 marker, carry the BOOLEAN type
/// identity, and be either the NULL-SCALAR tag with a zero payload length or
/// the BOOLEAN tag with payload length 1 and a payload byte of exactly 0 or
/// 1. Returns `None` when any envelope is malformed or trailing bytes remain.
fn decode_boolean_or_null_envelopes(bytes: &[u8]) -> Option<Vec<Option<bool>>> {
    let mut values = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let remaining = &bytes[offset..];
        if remaining.len() < 25 || &remaining[0..4] != b"ORV1" {
            return None;
        }
        if remaining[5..20] != [0; 15] || remaining[20] != 0x01 {
            return None;
        }
        match remaining[4] {
            0x00 => {
                if remaining[21..25] != 0_u32.to_be_bytes() {
                    return None;
                }
                values.push(None);
                offset += 25;
            }
            0x02 => {
                if remaining.len() < 26 || remaining[21..25] != 1_u32.to_be_bytes() {
                    return None;
                }
                match remaining[25] {
                    0x00 => values.push(Some(false)),
                    0x01 => values.push(Some(true)),
                    _ => return None,
                }
                offset += 26;
            }
            _ => return None,
        }
    }
    Some(values)
}

/// Run one granted raw reader and decode its complete mixed Boolean stream.
///
/// The reader must exit 0 with empty standard error. The decoded values are
/// returned in wire order; callers sort before comparing multisets.
fn decode_mixed_reader_values(
    machine: &InstalledMachine,
    function: &str,
    label: &'static str,
) -> Result<Vec<Option<bool>>, Error> {
    run_reader_and_decode(
        machine,
        function,
        label,
        decode_boolean_or_null_envelopes,
        "complete mixed Boolean envelopes",
    )
}

/// Prove that a nullable Boolean field reads back as a typed NULL envelope,
/// that a bare Boolean predicate filters rows and SELECT DISTINCT deduplicates
/// through the installed product's public raw-call path, and that the exact
/// fixture reapplies and restarts without changing the observable rows.
///
/// The test installs the exact checked-in `product_test_predicates.orna`
/// fixture, applies it, and requires exactly seven sorted qualified-name
/// mappings with pairwise distinct function identities. It then proves:
///
/// * all seven raw calls are denied before any grant;
/// * after granting all seven functions the four readers succeed empty;
/// * rows where marker and visible carry opposite truth (TRUE rows have
///   marker FALSE and visible TRUE, the FALSE row has marker TRUE and
///   visible FALSE, and the omitted-nullable row has marker TRUE) decode
///   through `read_all` as the sorted multiset
///   [None, Some(false), Some(true), Some(true)], while the bare-predicate
///   readers project marker and return [false, false] and [false], causally
///   separating the visible predicate from the marker projection, and
///   `read_visible_distinct` returns the sorted multiset
///   [None, Some(false), Some(true)];
/// * reapplying the exact same fixture keeps the complete seven-entry function
///   vector, grants, and rows, and `read_visible_distinct` stays
///   [None, Some(false), Some(true)];
/// * a restart keeps every reader result, including `read_visible_distinct`
///   at [None, Some(false), Some(true)];
/// * post-restart creates through the surviving grants add one TRUE, one
///   FALSE, and one omitted-nullable row, and the final sorted multisets are
///   `read_all` [None, None, false, false, true, true, true] with duplicate
///   NULL, FALSE, and TRUE values, `read_matching` [false, false, false],
///   `read_matching_distinct` [false], and `read_visible_distinct`
///   [None, Some(false), Some(true)].
///
/// All observations go through the packaged `/usr/bin/orna` public commands
/// and raw-call ORV envelopes. The test makes no claim about physical
/// storage, private rows, or row ordering.
#[test]
#[ignore = "requires Docker, ORNA_SYSTEM_TEST_DEBIAN_PACKAGE, and the installed orna executable"]
fn installed_nullable_boolean_predicates_filter_and_distinct_across_replay_and_restart() {
    let package = std::env::var("ORNA_SYSTEM_TEST_DEBIAN_PACKAGE")
        .expect("ORNA_SYSTEM_TEST_DEBIAN_PACKAGE must point at the reproduced .deb package");
    let artifact = FrozenPackageArtifact::new(PackageFormat::Debian, &package)
        .expect("freeze the reproduced Debian package");
    let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("product_test_predicates.orna");
    let fixture = fs::read(&fixture_path).expect("read the checked-in predicates fixture");

    let machine = InstalledMachine::start(&artifact, &fixture)
        .expect("start the installed Debian test machine");

    // Apply the exact fixture and require the seven sorted mappings.
    let apply = machine
        .run_as_orna(&["source", "apply", FIXTURE_PATH])
        .expect("run installed source apply");
    let apply = require_success("orna source apply", apply).expect("source apply must succeed");
    assert!(
        apply.stderr.is_empty(),
        "source apply must keep standard error empty"
    );
    let document = parse_apply_document(&apply.stdout).expect("source apply JSON must parse");
    let expected_order = [
        vec!["predicate_test".to_string(), "create_false".to_string()],
        vec!["predicate_test".to_string(), "create_null".to_string()],
        vec!["predicate_test".to_string(), "create_true".to_string()],
        vec!["predicate_test".to_string(), "read_all".to_string()],
        vec!["predicate_test".to_string(), "read_matching".to_string()],
        vec![
            "predicate_test".to_string(),
            "read_matching_distinct".to_string(),
        ],
        vec![
            "predicate_test".to_string(),
            "read_visible_distinct".to_string(),
        ],
    ];
    let actual_order = document
        .functions
        .iter()
        .map(|function| function.names().to_vec())
        .collect::<Vec<_>>();
    assert_eq!(
        actual_order, expected_order,
        "apply must report the seven function entries sorted by qualified name"
    );
    let create_true = document
        .function_id(&["predicate_test", "create_true"])
        .expect("apply must report create_true");
    let create_false = document
        .function_id(&["predicate_test", "create_false"])
        .expect("apply must report create_false");
    let create_null = document
        .function_id(&["predicate_test", "create_null"])
        .expect("apply must report create_null");
    let read_all = document
        .function_id(&["predicate_test", "read_all"])
        .expect("apply must report read_all");
    let read_matching = document
        .function_id(&["predicate_test", "read_matching"])
        .expect("apply must report read_matching");
    let read_matching_distinct = document
        .function_id(&["predicate_test", "read_matching_distinct"])
        .expect("apply must report read_matching_distinct");
    let read_visible_distinct = document
        .function_id(&["predicate_test", "read_visible_distinct"])
        .expect("apply must report read_visible_distinct");
    let identities = [
        create_true,
        create_false,
        create_null,
        read_all,
        read_matching,
        read_matching_distinct,
        read_visible_distinct,
    ];
    for (index, left) in identities.iter().enumerate() {
        for right in &identities[index + 1..] {
            assert_ne!(
                left, right,
                "the seven function identities must be pairwise distinct"
            );
        }
    }

    // Source apply grants nothing: every raw call is denied before any grant.
    for function in identities {
        let denied = machine
            .run_as_orna(&["raw-call", function])
            .expect("run denied raw call");
        assert_denied("raw call before grant", denied).expect("raw call must be denied");
    }

    // Grant all seven functions through the fixed-service command.
    for function in identities {
        let granted = machine
            .run_as_orna(&["security", "grant-execute", function])
            .expect("run installed grant command");
        require_silent_success("orna security grant-execute", granted)
            .expect("grant must succeed silently");
    }

    // All four readers initially succeed with empty streams.
    for function in [
        read_all,
        read_matching,
        read_matching_distinct,
        read_visible_distinct,
    ] {
        let empty = machine
            .run_as_orna(&["raw-call", function])
            .expect("run empty raw select");
        require_silent_success("orna raw-call empty select", empty)
            .expect("empty select must exit 0 with empty streams");
    }

    // Two TRUE rows, one FALSE row, and one omitted-nullable row.
    let mut references: Vec<&OrvReference> = Vec::new();
    let insert_and_check = |machine: &InstalledMachine, function: &str| {
        let inserted = machine
            .run_as_orna(&["raw-call", function])
            .expect("run raw insert");
        let inserted =
            require_success("orna raw-call insert", inserted).expect("insert must succeed");
        assert!(
            inserted.stderr.is_empty(),
            "insert must keep standard error empty"
        );
        let reference = parse_reference_envelope(&inserted.stdout)
            .expect("insert must return one ORV reference");
        assert!(
            reference.type_id != [0; 16] && !reference.object_is_zero(),
            "the insert must return a real object reference"
        );
        reference
    };
    let a = insert_and_check(&machine, create_true);
    let b = insert_and_check(&machine, create_true);
    let c = insert_and_check(&machine, create_false);
    let d = insert_and_check(&machine, create_null);
    references.extend([&a, &b, &c, &d]);
    for (index, left) in references.iter().enumerate() {
        assert_eq!(
            left.type_id, a.type_id,
            "every insert must reference the same target type"
        );
        for right in &references[index + 1..] {
            assert_ne!(
                left.object, right.object,
                "every insert must allocate a distinct object identity"
            );
        }
    }

    // read_all decodes the mixed multiset; the predicate readers filter.
    let mut all_values = decode_mixed_reader_values(&machine, read_all, "orna raw-call read_all")
        .expect("read_all must decode");
    all_values.sort_unstable();
    assert_eq!(
        all_values,
        [None, Some(false), Some(true), Some(true)],
        "read_all must decode as one NULL, one FALSE, and two TRUE values"
    );
    let mut matching = decode_reader_values(&machine, read_matching, "orna raw-call read_matching")
        .expect("read_matching must decode");
    matching.sort_unstable();
    assert_eq!(
        matching,
        [false, false],
        "read_matching must decode as exactly two FALSE values"
    );
    let matching_distinct = decode_reader_values(
        &machine,
        read_matching_distinct,
        "orna raw-call read_matching_distinct",
    )
    .expect("read_matching_distinct must decode");
    assert_eq!(
        matching_distinct,
        [false],
        "read_matching_distinct must decode as exactly one FALSE value"
    );
    let mut visible_distinct = decode_mixed_reader_values(
        &machine,
        read_visible_distinct,
        "orna raw-call read_visible_distinct",
    )
    .expect("read_visible_distinct must decode");
    visible_distinct.sort_unstable();
    assert_eq!(
        visible_distinct,
        [None, Some(false), Some(true)],
        "read_visible_distinct must decode as one NULL, one FALSE, and one TRUE value"
    );

    // Exact source replay keeps the complete mapping, grants, and rows.
    let replay = machine
        .run_as_orna(&["source", "apply", FIXTURE_PATH])
        .expect("run installed source apply on the same fixture");
    let replay = require_success("orna source apply replay", replay)
        .expect("predicates replay must succeed");
    assert!(
        replay.stderr.is_empty(),
        "predicates replay must keep standard error empty"
    );
    let replay_document =
        parse_apply_document(&replay.stdout).expect("predicates replay JSON must parse");
    assert_eq!(
        replay_document.functions, document.functions,
        "the replay must keep the complete seven-entry function vector"
    );
    let mut visible_after_replay = decode_mixed_reader_values(
        &machine,
        read_visible_distinct,
        "orna raw-call read_visible_distinct after replay",
    )
    .expect("read_visible_distinct must decode after replay");
    visible_after_replay.sort_unstable();
    assert_eq!(
        visible_after_replay, visible_distinct,
        "read_visible_distinct must stay unchanged after replay"
    );

    // Restart preserves every reader result.
    machine
        .restart_server()
        .expect("installed server must restart cleanly");
    let mut all_after_restart =
        decode_mixed_reader_values(&machine, read_all, "orna raw-call read_all after restart")
            .expect("read_all must decode after restart");
    all_after_restart.sort_unstable();
    assert_eq!(
        all_after_restart, all_values,
        "read_all must stay unchanged after restart"
    );
    let mut matching_after_restart = decode_reader_values(
        &machine,
        read_matching,
        "orna raw-call read_matching after restart",
    )
    .expect("read_matching must decode after restart");
    matching_after_restart.sort_unstable();
    assert_eq!(
        matching_after_restart, matching,
        "read_matching must stay unchanged after restart"
    );
    let distinct_after_restart = decode_reader_values(
        &machine,
        read_matching_distinct,
        "orna raw-call read_matching_distinct after restart",
    )
    .expect("read_matching_distinct must decode after restart");
    assert_eq!(
        distinct_after_restart, matching_distinct,
        "read_matching_distinct must stay unchanged after restart"
    );
    let mut visible_after_restart = decode_mixed_reader_values(
        &machine,
        read_visible_distinct,
        "orna raw-call read_visible_distinct after restart",
    )
    .expect("read_visible_distinct must decode after restart");
    visible_after_restart.sort_unstable();
    assert_eq!(
        visible_after_restart, visible_distinct,
        "read_visible_distinct must stay unchanged after restart"
    );

    // Post-restart creates through the surviving grants add three objects.
    let e = insert_and_check(&machine, create_true);
    let f = insert_and_check(&machine, create_false);
    let g = insert_and_check(&machine, create_null);
    references.extend([&e, &f, &g]);
    for (index, left) in references.iter().enumerate() {
        assert_eq!(
            left.type_id, a.type_id,
            "every insert must reference the same target type"
        );
        for right in &references[index + 1..] {
            assert_ne!(
                left.object, right.object,
                "every insert must allocate a distinct object identity"
            );
        }
    }

    // Final multisets after the three post-restart inserts.
    let mut all_final = decode_mixed_reader_values(&machine, read_all, "orna raw-call read_all")
        .expect("read_all must decode");
    all_final.sort_unstable();
    assert_eq!(
        all_final,
        [
            None,
            None,
            Some(false),
            Some(false),
            Some(true),
            Some(true),
            Some(true)
        ],
        "read_all must decode as two NULL, two FALSE, and three TRUE values"
    );
    let mut matching_final =
        decode_reader_values(&machine, read_matching, "orna raw-call read_matching")
            .expect("read_matching must decode");
    matching_final.sort_unstable();
    assert_eq!(
        matching_final,
        [false, false, false],
        "read_matching must decode as exactly three FALSE values"
    );
    let distinct_final = decode_reader_values(
        &machine,
        read_matching_distinct,
        "orna raw-call read_matching_distinct",
    )
    .expect("read_matching_distinct must decode");
    assert_eq!(
        distinct_final,
        [false],
        "read_matching_distinct must decode as exactly one FALSE value"
    );
    let mut visible_final = decode_mixed_reader_values(
        &machine,
        read_visible_distinct,
        "orna raw-call read_visible_distinct",
    )
    .expect("read_visible_distinct must decode");
    visible_final.sort_unstable();
    assert_eq!(
        visible_final,
        [None, Some(false), Some(true)],
        "read_visible_distinct must stay one NULL, one FALSE, and one TRUE value"
    );
}

/// Prove that the installed product's parameterised raw-call argument path
/// inserts persisted rows through the packaged `/usr/bin/orna` public
/// commands, and that the exact fixture reapplies and restarts without
/// changing identities, grants, or the stored Boolean multiset.
///
/// The test installs the exact checked-in `product_test_unavailable_insert.orna`
/// fixture, applies it, and requires exactly two sorted qualified-name
/// mappings with pairwise distinct function identities. It then proves:
///
/// * `create_entry` reports the canonical `p_stored` parameter identity while
///   `read_entries` has no parameters;
/// * an argument-bearing create call with the exact ORV1 TRUE argument on
///   standard input is denied before create's own explicit grant; after
///   granting the reader alone, the reader stays empty, which proves the
///   denied writer created no row;
/// * a zero-argument create call is TARGET_UNAVAILABLE and adds no row;
/// * an argument-bearing TRUE create returns a canonical nonzero reference,
///   and a second FALSE create returns the same target type with a distinct
///   nonzero object identity;
/// * the reader returns exactly one TRUE and one FALSE value;
/// * reapplying the exact same fixture keeps the complete two-entry function
///   vector including ordered parameters, without re-granting, and a third
///   TRUE create adds a third distinct object;
/// * a restart keeps grants and rows, and a fourth FALSE create adds a
///   fourth distinct object;
/// * the reader then returns exactly two TRUE and two FALSE values.
///
/// All observations go through the packaged `/usr/bin/orna` public commands
/// and raw-call ORV envelopes, with the argument streamed through
/// `docker exec --interactive`. The test makes no claim about physical
/// storage, private rows, or row ordering.
#[test]
#[ignore = "requires Docker, ORNA_SYSTEM_TEST_DEBIAN_PACKAGE, and the installed orna executable"]
fn installed_parameterised_argument_insert_persists_across_replay_and_restart() {
    let package = std::env::var("ORNA_SYSTEM_TEST_DEBIAN_PACKAGE")
        .expect("ORNA_SYSTEM_TEST_DEBIAN_PACKAGE must point at the reproduced .deb package");
    let artifact = FrozenPackageArtifact::new(PackageFormat::Debian, &package)
        .expect("freeze the reproduced Debian package");
    let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("product_test_unavailable_insert.orna");
    let fixture = fs::read(&fixture_path).expect("read the checked-in unavailable insert fixture");

    let machine = InstalledMachine::start(&artifact, &fixture)
        .expect("start the installed Debian test machine");

    // Apply the exact fixture and require the two sorted mappings.
    let apply = machine
        .run_as_orna(&["source", "apply", FIXTURE_PATH])
        .expect("run installed source apply");
    let apply = require_success("orna source apply", apply).expect("source apply must succeed");
    assert!(
        apply.stderr.is_empty(),
        "source apply must keep standard error empty"
    );
    let document = parse_apply_document(&apply.stdout).expect("source apply JSON must parse");
    let expected_order = [
        vec![
            "unavailable_insert_test".to_string(),
            "create_entry".to_string(),
        ],
        vec![
            "unavailable_insert_test".to_string(),
            "read_entries".to_string(),
        ],
    ];
    let actual_order = document
        .functions
        .iter()
        .map(|function| function.names().to_vec())
        .collect::<Vec<_>>();
    assert_eq!(
        actual_order, expected_order,
        "apply must report the two function entries sorted by qualified name"
    );
    let create_entry = document
        .function_id(&["unavailable_insert_test", "create_entry"])
        .expect("apply must report create_entry");
    let read_entries = document
        .function_id(&["unavailable_insert_test", "read_entries"])
        .expect("apply must report read_entries");
    assert_ne!(
        create_entry, read_entries,
        "the two function identities must be pairwise distinct"
    );
    let p_stored = document
        .parameter_id(&["unavailable_insert_test", "create_entry"], "p_stored")
        .expect("apply must report the canonical create_entry.p_stored identity");
    let create_entry_entry = document
        .functions
        .iter()
        .find(|function| {
            function.names().iter().map(String::as_str).eq([
                "unavailable_insert_test",
                "create_entry",
            ]
            .iter()
            .copied())
        })
        .expect("apply must report the create_entry entry");
    assert_eq!(
        create_entry_entry.parameters().len(),
        1,
        "create_entry must declare exactly one parameter"
    );
    let stored_parameter = &create_entry_entry.parameters()[0];
    assert_eq!(
        stored_parameter.name(),
        "p_stored",
        "create_entry must declare exactly the p_stored parameter"
    );
    assert_eq!(
        stored_parameter.parameter_id(),
        p_stored,
        "the declared parameter identity must equal the discovered p_stored identity"
    );
    let read_entry = document
        .functions
        .iter()
        .find(|function| {
            function.names().iter().map(String::as_str).eq([
                "unavailable_insert_test",
                "read_entries",
            ]
            .iter()
            .copied())
        })
        .expect("apply must report the read_entries entry");
    assert!(
        read_entry.parameters().is_empty(),
        "read_entries must have no parameters"
    );

    // Before create's explicit grant, the argument-bearing create is denied
    // and nothing is stored.
    let denied = machine
        .run_as_orna_with_stdin(
            &["raw-call", create_entry, p_stored],
            &boolean_orv1_envelope(Some(true)),
        )
        .expect("run denied argument raw call");
    assert_denied("argument raw call before grant", denied)
        .expect("argument raw call must be denied before grant");

    // Grant the reader only, then prove the denied writer created no row.
    let granted_reader = machine
        .run_as_orna(&["security", "grant-execute", read_entries])
        .expect("run installed reader grant command");
    require_silent_success("orna security grant-execute read_entries", granted_reader)
        .expect("reader grant must succeed silently");
    let empty_before = machine
        .run_as_orna(&["raw-call", read_entries])
        .expect("run empty raw select after reader grant");
    require_silent_success(
        "orna raw-call read_entries after reader grant",
        empty_before,
    )
    .expect("the denied writer must leave the reader empty");

    // Grant the create only, then prove the zero-argument create is
    // unavailable and adds no row.
    let granted_create = machine
        .run_as_orna(&["security", "grant-execute", create_entry])
        .expect("run installed create grant command");
    require_silent_success("orna security grant-execute create_entry", granted_create)
        .expect("create grant must succeed silently");
    let unavailable = machine
        .run_as_orna(&["raw-call", create_entry])
        .expect("run unavailable zero-argument raw call");
    assert_target_unavailable("zero-argument raw call", unavailable)
        .expect("zero-argument create must be target unavailable");
    let empty_after_unavailable = machine
        .run_as_orna(&["raw-call", read_entries])
        .expect("run empty raw select after unavailable call");
    require_silent_success(
        "orna raw-call read_entries after unavailable call",
        empty_after_unavailable,
    )
    .expect("the reader must stay successful and empty");

    // A TRUE argument create returns a canonical nonzero reference.
    let inserted_true = machine
        .run_as_orna_with_stdin(
            &["raw-call", create_entry, p_stored],
            &boolean_orv1_envelope(Some(true)),
        )
        .expect("run TRUE argument raw call");
    let inserted_true = require_value_success("orna raw-call create_entry TRUE", inserted_true)
        .expect("TRUE argument create must succeed");
    let true_reference = parse_reference_envelope(&inserted_true.stdout)
        .expect("TRUE argument create must return one ORV reference");
    assert!(
        true_reference.type_id != [0; 16] && !true_reference.object_is_zero(),
        "the TRUE create reference must name a real target type and row"
    );

    // A FALSE argument create returns the same target type with a distinct
    // nonzero object identity.
    let inserted_false = machine
        .run_as_orna_with_stdin(
            &["raw-call", create_entry, p_stored],
            &boolean_orv1_envelope(Some(false)),
        )
        .expect("run FALSE argument raw call");
    let inserted_false = require_value_success("orna raw-call create_entry FALSE", inserted_false)
        .expect("FALSE argument create must succeed");
    let false_reference = parse_reference_envelope(&inserted_false.stdout)
        .expect("FALSE argument create must return one ORV reference");
    assert!(
        !false_reference.object_is_zero(),
        "the FALSE create reference must name a real row"
    );
    assert_eq!(
        false_reference.type_id, true_reference.type_id,
        "both argument creates must target the same object type"
    );
    assert_ne!(
        false_reference.object, true_reference.object,
        "each argument create must allocate a distinct object identity"
    );

    // The reader returns exactly one TRUE and one FALSE value.
    let mut two_values = decode_reader_values(
        &machine,
        read_entries,
        "orna raw-call read_entries two values",
    )
    .expect("two-value read must decode");
    two_values.sort();
    assert_eq!(
        two_values,
        vec![false, true],
        "the reader must return exactly one TRUE and one FALSE value"
    );

    // Exact source replay keeps the complete mapping including ordered
    // parameters, and no re-grant is needed.
    let replay = machine
        .run_as_orna(&["source", "apply", FIXTURE_PATH])
        .expect("run installed source apply on the same fixture");
    let replay =
        require_success("orna source apply replay", replay).expect("fixture replay must succeed");
    assert!(
        replay.stderr.is_empty(),
        "fixture replay must keep standard error empty"
    );
    let replay_document =
        parse_apply_document(&replay.stdout).expect("fixture replay JSON must parse");
    assert_eq!(
        replay_document.functions, document.functions,
        "the replay must keep the complete two-entry function vector including parameters"
    );
    let replay_p_stored = replay_document
        .parameter_id(&["unavailable_insert_test", "create_entry"], "p_stored")
        .expect("the replay must keep create_entry.p_stored");
    assert_eq!(
        replay_p_stored, p_stored,
        "the replay must keep the exact canonical parameter identity"
    );

    // A third TRUE create with the original identities adds a third object.
    let third = machine
        .run_as_orna_with_stdin(
            &["raw-call", create_entry, p_stored],
            &boolean_orv1_envelope(Some(true)),
        )
        .expect("run third TRUE argument raw call");
    let third = require_value_success("orna raw-call create_entry third", third)
        .expect("third TRUE argument create must succeed");
    let third_reference = parse_reference_envelope(&third.stdout)
        .expect("third create must return one ORV reference");
    assert!(
        third_reference.type_id == true_reference.type_id && !third_reference.object_is_zero(),
        "the third create must target the same real object type"
    );
    assert!(
        third_reference.object != true_reference.object
            && third_reference.object != false_reference.object,
        "each argument create must allocate a distinct object identity"
    );

    // The reader returns exactly one FALSE and two TRUE values.
    let mut three_values = decode_reader_values(
        &machine,
        read_entries,
        "orna raw-call read_entries three values",
    )
    .expect("three-value read must decode");
    three_values.sort();
    assert_eq!(
        three_values,
        vec![false, true, true],
        "the reader must return exactly one FALSE and two TRUE values"
    );

    // Restart keeps grants and rows; a fourth FALSE create adds an object.
    machine
        .restart_server()
        .expect("installed server must restart cleanly");
    let fourth = machine
        .run_as_orna_with_stdin(
            &["raw-call", create_entry, p_stored],
            &boolean_orv1_envelope(Some(false)),
        )
        .expect("run fourth FALSE argument raw call");
    let fourth = require_value_success("orna raw-call create_entry fourth", fourth)
        .expect("fourth FALSE argument create must succeed");
    let fourth_reference = parse_reference_envelope(&fourth.stdout)
        .expect("fourth create must return one ORV reference");
    assert!(
        fourth_reference.type_id == true_reference.type_id && !fourth_reference.object_is_zero(),
        "the fourth create must target the same real object type"
    );
    assert!(
        fourth_reference.object != true_reference.object
            && fourth_reference.object != false_reference.object
            && fourth_reference.object != third_reference.object,
        "each argument create must allocate a distinct object identity"
    );

    // The reader returns exactly two FALSE and two TRUE values.
    let mut four_values = decode_reader_values(
        &machine,
        read_entries,
        "orna raw-call read_entries four values",
    )
    .expect("four-value read must decode");
    four_values.sort();
    assert_eq!(
        four_values,
        vec![false, false, true, true],
        "the reader must return exactly two FALSE and two TRUE values"
    );
}
