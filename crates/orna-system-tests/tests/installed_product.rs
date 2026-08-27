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

/// The canonical `ORV1` envelope for one object reference.
///
/// The layout is `ORV1`, the REFERENCE tag, the 16-byte target type identity,
/// the 4-byte big-endian payload length, and the 16-byte object identity.
fn reference_orv1_envelope(target: [u8; 16], object: [u8; 16]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(41);
    bytes.extend_from_slice(b"ORV1");
    bytes.push(0x08);
    bytes.extend_from_slice(&target);
    bytes.extend_from_slice(&16_u32.to_be_bytes());
    bytes.extend_from_slice(&object);
    bytes
}

/// The exact projected raw result for one identity-selected person read.
///
/// The result contains the stored Reference, Text, and Boolean cells in the
/// declared projection order. It has no row wrapper or column metadata.
fn identity_selected_person_envelopes(
    reference: &OrvReference,
    name: &str,
    active: bool,
) -> Vec<u8> {
    let mut bytes = reference_orv1_envelope(reference.type_id, reference.object);
    bytes.extend(text_orv1_envelope(name));
    bytes.extend(boolean_orv1_envelope(Some(active)));
    bytes
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

/// The parsed public source-diff document. The report exposes source-revision
/// halves and semantic changes; it does not expose catalogue-revision halves.
#[derive(Clone, Debug, Eq, PartialEq)]
struct SourceDiffDocument {
    active_source_revision: String,
    candidate_source_revision: String,
    changes: Vec<SourceDiffChange>,
}

/// One exact semantic change rendered by the installed source-diff command.
#[derive(Clone, Debug, Eq, PartialEq)]
enum SourceDiffChange {
    FieldRename {
        field_id: String,
    },
    FunctionRevision {
        name: String,
        old_revision: String,
        new_revision: String,
        old_hash: String,
        new_hash: String,
        function_id: String,
    },
    NoSemanticChanges,
}

/// Parse the complete public source-diff document.
///
/// The parser deliberately rejects unknown lines, malformed identities, and
/// extra trailing line feeds. The installed proof therefore checks the
/// command's framing and exit-side streams rather than passing on a substring
/// that merely resembles a diff.
fn parse_source_diff_document(bytes: &[u8]) -> Result<SourceDiffDocument, Error> {
    let text = std::str::from_utf8(bytes).map_err(|_| Error::Unexpected {
        message: "source diff output is not UTF-8".to_string(),
    })?;
    if !text.ends_with('\n') {
        return Err(Error::Unexpected {
            message: "source diff output must end with one line feed".to_string(),
        });
    }
    let body = &text[..text.len() - 1];
    if body.ends_with('\n') {
        return Err(Error::Unexpected {
            message: "source diff output must end with exactly one line feed".to_string(),
        });
    }
    if body.contains('\r') {
        return Err(Error::Unexpected {
            message: "source diff output must not contain carriage returns".to_string(),
        });
    }
    let mut lines = body.lines();
    let header = lines.next().ok_or_else(|| Error::Unexpected {
        message: "source diff output must render a header".to_string(),
    })?;
    let revisions = header
        .strip_prefix("semantic diff ")
        .and_then(|rest| rest.split_once(" -> "))
        .ok_or_else(|| Error::Unexpected {
            message: "source diff output must start with its exact revision header".to_string(),
        })?;
    assert_source_diff_revision(revisions.0, "active")?;
    assert_source_diff_revision(revisions.1, "candidate")?;

    let mut changes = Vec::new();
    for line in lines {
        if line == "no semantic changes" {
            changes.push(SourceDiffChange::NoSemanticChanges);
            continue;
        }

        const FIELD_PREFIX: &str =
            "~ field product_test.probe.stored -> product_test.probe.retained [";
        if let Some(field_id) = line
            .strip_prefix(FIELD_PREFIX)
            .and_then(|line| line.strip_suffix(']'))
        {
            assert_canonical_identity(field_id, "field:", "field rename")?;
            changes.push(SourceDiffChange::FieldRename {
                field_id: field_id.to_owned(),
            });
            continue;
        }

        const FUNCTION_PREFIX: &str = "! function ";
        let Some(rest) = line.strip_prefix(FUNCTION_PREFIX) else {
            return Err(Error::Unexpected {
                message: format!("source diff rendered an unknown line: {line:?}"),
            });
        };
        let (name, rest) =
            rest.split_once(" executable revision ")
                .ok_or_else(|| Error::Unexpected {
                    message: format!("source diff rendered a malformed function line: {line:?}"),
                })?;
        if !matches!(
            name,
            "product_test.create_probe" | "product_test.read_probes"
        ) {
            return Err(Error::Unexpected {
                message: format!("source diff rendered an unexpected function: {name:?}"),
            });
        }
        let (old_revision, rest) = rest.split_once(" -> ").ok_or_else(|| Error::Unexpected {
            message: format!("source diff function line lacks its revision transition: {line:?}"),
        })?;
        assert_canonical_identity(old_revision, "function-revision:", "old function revision")?;
        let (new_revision, rest) =
            rest.split_once(" semantic hash ")
                .ok_or_else(|| Error::Unexpected {
                    message: format!(
                        "source diff function line lacks its semantic hashes: {line:?}"
                    ),
                })?;
        assert_canonical_identity(new_revision, "function-revision:", "new function revision")?;
        let (old_hash, rest) = rest.split_once(" -> ").ok_or_else(|| Error::Unexpected {
            message: format!("source diff function line lacks its hash transition: {line:?}"),
        })?;
        assert_digest_hex(old_hash, "old semantic hash")?;
        let (new_hash, function_id) = rest
            .rsplit_once(" [")
            .and_then(|(hash, id)| id.strip_suffix(']').map(|id| (hash, id)))
            .ok_or_else(|| Error::Unexpected {
                message: format!("source diff function line lacks its function identity: {line:?}"),
            })?;
        assert_digest_hex(new_hash, "new semantic hash")?;
        assert_canonical_identity(function_id, "function:", "function revision")?;
        changes.push(SourceDiffChange::FunctionRevision {
            name: name.to_owned(),
            old_revision: old_revision.to_owned(),
            new_revision: new_revision.to_owned(),
            old_hash: old_hash.to_owned(),
            new_hash: new_hash.to_owned(),
            function_id: function_id.to_owned(),
        });
    }

    if changes.is_empty() {
        return Err(Error::Unexpected {
            message: "source diff output must render one semantic result line".to_string(),
        });
    }
    Ok(SourceDiffDocument {
        active_source_revision: revisions.0.to_owned(),
        candidate_source_revision: revisions.1.to_owned(),
        changes,
    })
}

/// Require one canonical revision identity in the public source-diff report.
fn assert_source_diff_revision(value: &str, side: &str) -> Result<(), Error> {
    assert_canonical_identity(
        value,
        "source-revision:",
        &format!("{side} source revision"),
    )
}

/// Require one canonical 16-byte identity rendered with its exact prefix.
fn assert_canonical_identity(value: &str, prefix: &str, label: &str) -> Result<(), Error> {
    const ID_ALPHABET: &[u8] = b"0123456789abcdefghjkmnpqrstvwxyz";
    let encoded = value
        .strip_prefix(prefix)
        .ok_or_else(|| Error::Unexpected {
            message: format!("{label} must use the {prefix} prefix: {value:?}"),
        })?;
    if encoded.len() != 26 || !encoded.bytes().all(|byte| ID_ALPHABET.contains(&byte)) {
        return Err(Error::Unexpected {
            message: format!("{label} must use one canonical 26-character identity: {value:?}"),
        });
    }
    let Some(final_value) = ID_ALPHABET
        .iter()
        .position(|candidate| *candidate == encoded.as_bytes()[25])
    else {
        return Err(Error::Unexpected {
            message: format!("{label} must use one canonical 26-character identity: {value:?}"),
        });
    };
    if final_value & 0b11 != 0 {
        return Err(Error::Unexpected {
            message: format!("{label} final character is not canonical: {value:?}"),
        });
    }
    Ok(())
}

/// Require one lowercase SHA-256 digest in a public function-revision line.
fn assert_digest_hex(value: &str, label: &str) -> Result<(), Error> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(Error::Unexpected {
            message: format!("{label} must use one lowercase SHA-256 digest: {value:?}"),
        });
    }
    Ok(())
}

/// Parse the exact compact JSON success document of `orna source apply`.
///
/// The document is one line ending in one line feed:
///
/// ```json
/// {"source_revision":"source-revision:<id>","catalogue_revision":"catalogue-revision:<id>","functions":[{"qualified_name":["schema","function"],"function_id":"function:<id>"}]}
/// ```
///
/// The required object key order and compact framing are exact. Parameter-free
/// entries use the two-key form shown above. Parameterised entries may include
/// the ordered optional `parameters` array defined by work ADR 0040; deviations
/// from those accepted forms are rejected with a closed message.
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

#[test]
fn source_diff_parser_rejects_an_extra_final_line_feed() {
    let document = concat!(
        "semantic diff source-revision:00000000000000000000000000 -> ",
        "source-revision:00000000000000000000000004\n",
        "no semantic changes\n\n",
    );
    let result = parse_source_diff_document(document.as_bytes());
    assert!(
        matches!(result, Err(Error::Unexpected { .. })),
        "an extra final line feed must be rejected"
    );
}

#[test]
fn source_diff_parser_rejects_a_noncanonical_final_base32_value() {
    let document = concat!(
        "semantic diff source-revision:00000000000000000000000000 -> ",
        "source-revision:00000000000000000000000001\n",
        "no semantic changes\n",
    );
    let result = parse_source_diff_document(document.as_bytes());
    assert!(
        matches!(result, Err(Error::Unexpected { .. })),
        "a non-canonical final base32 value must be rejected"
    );
}

#[test]
fn source_diff_parser_retains_function_revision_and_hash_transitions() {
    let old_hash = "0".repeat(64);
    let new_hash = format!("{}1", "0".repeat(63));
    let document = format!(
        "semantic diff source-revision:00000000000000000000000000 -> \
         source-revision:00000000000000000000000004\n\
         ! function product_test.create_probe executable revision \
         function-revision:00000000000000000000000000 -> \
         function-revision:00000000000000000000000004 semantic hash {old_hash} -> \
         {new_hash} [function:00000000000000000000000000]\n"
    );
    let parsed = parse_source_diff_document(document.as_bytes())
        .expect("a canonical function revision transition must parse");
    match parsed.changes.as_slice() {
        [
            SourceDiffChange::FunctionRevision {
                name,
                old_revision,
                new_revision,
                old_hash: parsed_old_hash,
                new_hash: parsed_new_hash,
                function_id,
            },
        ] => {
            assert_eq!(name, "product_test.create_probe");
            assert_eq!(old_revision, "function-revision:00000000000000000000000000");
            assert_eq!(new_revision, "function-revision:00000000000000000000000004");
            assert_eq!(parsed_old_hash, &old_hash);
            assert_eq!(parsed_new_hash, &new_hash);
            assert_eq!(function_id, "function:00000000000000000000000000");
        }
        changes => panic!("unexpected parsed source diff changes: {changes:?}"),
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

/// Prove the installed public `orna source diff` entrypoint without apply.
///
/// The test applies the accepted original fixture, diffs the evidence-bearing
/// renamed fixture through `/usr/bin/orna`, parses the complete public
/// output and exit-0 boundary, and then diffs the original fixture again. The
/// final exact no-change report keeps the active revision pair unchanged while
/// the changed report retains the field identity and executable transition values.
#[test]
#[ignore = "requires Docker, ORNA_SYSTEM_TEST_DEBIAN_PACKAGE, and the installed public source-diff entrypoint"]
fn installed_public_source_diff_preserves_identity_without_apply() {
    let package = std::env::var("ORNA_SYSTEM_TEST_DEBIAN_PACKAGE")
        .expect("ORNA_SYSTEM_TEST_DEBIAN_PACKAGE must point at the reproduced .deb package");
    let artifact = FrozenPackageArtifact::new(PackageFormat::Debian, &package)
        .expect("freeze the reproduced Debian package");
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let original = fs::read(fixtures.join("product_test.orna")).expect("read the product fixture");
    let renamed = fs::read(fixtures.join("product_test_renamed.orna"))
        .expect("read the renamed product fixture");

    let machine = InstalledMachine::start(&artifact, &original)
        .expect("start the installed Debian test machine");

    // The initial apply is the only public source of the active revision pair
    // exposed by this product journey. Keep both IDs and use the source side
    // in each diff header to prove that diff never activated its candidate.
    let apply = machine
        .run_as_orna(&["source", "apply", FIXTURE_PATH])
        .expect("run installed source apply");
    let apply = require_success("orna source apply", apply).expect("source apply must succeed");
    assert_eq!(
        apply.status.code(),
        Some(0),
        "source apply must exit with the exact public success code"
    );
    assert!(
        apply.stderr.is_empty(),
        "source apply must keep standard error empty"
    );
    let apply_document = parse_apply_document(&apply.stdout).expect("source apply JSON must parse");
    assert_eq!(
        apply_document.functions.len(),
        2,
        "source apply must report exactly the accepted fixture functions"
    );
    let initial_source_revision = apply_document.source_revision.clone();
    let initial_catalogue_revision = apply_document.catalogue_revision.clone();
    let create_probe_id = apply_document
        .function_id(&["product_test", "create_probe"])
        .expect("source apply must report create_probe")
        .to_owned();
    let read_probes_id = apply_document
        .function_id(&["product_test", "read_probes"])
        .expect("source apply must report read_probes")
        .to_owned();
    assert_canonical_identity(&create_probe_id, "function:", "create_probe")
        .expect("create_probe identity must be canonical");
    assert_canonical_identity(&read_probes_id, "function:", "read_probes")
        .expect("read_probes identity must be canonical");

    machine
        .write_fixture(&renamed)
        .expect("replace the fixture with the renamed source");
    let diff = machine
        .run_as_orna(&["source", "diff", FIXTURE_PATH])
        .expect("run installed source diff");
    let diff = require_success("orna source diff renamed", diff)
        .expect("source diff must succeed for the renamed source");
    assert_eq!(
        diff.status.code(),
        Some(0),
        "renamed source diff must exit with the exact public success code"
    );
    assert!(
        diff.stderr.is_empty(),
        "source diff must keep standard error empty"
    );
    let diff_document =
        parse_source_diff_document(&diff.stdout).expect("renamed source diff must parse exactly");
    assert_eq!(
        diff_document.active_source_revision, initial_source_revision,
        "source diff must pin its active revision to the applied pair"
    );
    assert_ne!(
        diff_document.active_source_revision, diff_document.candidate_source_revision,
        "source diff must render a distinct prepared candidate revision"
    );
    // The packaged source-diff header exposes only source-revision halves;
    // catalogue-revision halves are a contract blocker for this public proof.

    let mut field_ids = Vec::new();
    let mut function_changes = Vec::new();
    for change in diff_document.changes {
        match change {
            SourceDiffChange::FieldRename { field_id } => field_ids.push(field_id),
            SourceDiffChange::FunctionRevision {
                name,
                old_revision,
                new_revision,
                old_hash,
                new_hash,
                function_id,
            } => function_changes.push((
                name,
                old_revision,
                new_revision,
                old_hash,
                new_hash,
                function_id,
            )),
            SourceDiffChange::NoSemanticChanges => {
                panic!("renamed source diff must not report no semantic changes")
            }
        }
    }
    assert_eq!(
        field_ids.len(),
        1,
        "renamed source diff must render exactly one identity-keyed field rename"
    );
    // The packaged apply response does not expose a field identity, so this
    // proof can require only the canonical field token rendered by source diff;
    // baseline field-identity equality is an explicit output-contract blocker.

    // The renamed fixture also changes create_probe from TRUE to FALSE. ADR
    // 0015 requires a new executable revision and semantic hash for that
    // resolved Boolean change, while ADR 0006 permits the rename-only
    // read_probes dependent revision to remain unchanged and therefore absent
    // from the rendered change list.
    assert_eq!(
        function_changes.len(),
        1,
        "the TRUE-to-FALSE create_probe change must be the only executable revision change"
    );
    let (name, old_revision, new_revision, old_hash, new_hash, function_id) = function_changes
        .pop()
        .expect("the source diff must retain create_probe's transition values");
    assert_eq!(name, "product_test.create_probe");
    assert_eq!(function_id, create_probe_id);
    assert_ne!(
        old_revision, new_revision,
        "TRUE-to-FALSE must allocate a distinct executable revision identity"
    );
    assert_ne!(
        old_hash, new_hash,
        "TRUE-to-FALSE must change the executable semantic hash"
    );

    machine
        .write_fixture(&original)
        .expect("restore the original source fixture");
    let unchanged = machine
        .run_as_orna(&["source", "diff", FIXTURE_PATH])
        .expect("run installed source diff against the original source");
    let unchanged = require_success("orna source diff unchanged", unchanged)
        .expect("source diff must succeed for the unchanged source");
    assert_eq!(
        unchanged.status.code(),
        Some(0),
        "unchanged source diff must exit with the exact public success code"
    );
    assert!(
        unchanged.stderr.is_empty(),
        "unchanged source diff must keep standard error empty"
    );
    let unchanged_document = parse_source_diff_document(&unchanged.stdout)
        .expect("unchanged source diff must parse exactly");
    assert_eq!(
        unchanged_document.active_source_revision, initial_source_revision,
        "unchanged source diff must retain the complete active revision pair"
    );
    assert_ne!(
        unchanged_document.active_source_revision, unchanged_document.candidate_source_revision,
        "unchanged source diff must still be a prepared, unapplied candidate"
    );
    assert!(
        matches!(
            unchanged_document.changes.as_slice(),
            [SourceDiffChange::NoSemanticChanges]
        ),
        "unchanged source diff must contain exactly the no-change result"
    );
    assert_canonical_identity(
        &initial_catalogue_revision,
        "catalogue-revision:",
        "active catalogue revision",
    )
    .expect("source apply must expose a canonical catalogue half of the active pair");
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

/// Prove the public raw Reference journey for work ADR 0048.
///
/// The installed product applies one object with Text and Boolean fields, a
/// scalar-argument creator, and one identity-selected reader. It proves that
/// the reader is denied before its grant, selects only its supplied Reference,
/// accepts a same-type absent Reference as an empty result, and preserves the
/// discovered identities, grants, References, and rows across replay and
/// restart.
#[test]
#[ignore = "requires Docker, ORNA_SYSTEM_TEST_DEBIAN_PACKAGE, and the installed orna executable"]
fn installed_identity_selected_read_binds_reference_and_survives_replay_and_restart() {
    let package = std::env::var("ORNA_SYSTEM_TEST_DEBIAN_PACKAGE")
        .expect("ORNA_SYSTEM_TEST_DEBIAN_PACKAGE must point at the reproduced .deb package");
    let artifact = FrozenPackageArtifact::new(PackageFormat::Debian, &package)
        .expect("freeze the reproduced Debian package");
    let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("product_test_identity_select.orna");
    let fixture = fs::read(&fixture_path).expect("read the checked-in identity select fixture");

    let machine = InstalledMachine::start(&artifact, &fixture)
        .expect("start the installed Debian test machine");

    // Apply the exact fixture and discover the stable function and parameter
    // identities used by the public raw-call command.
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
            "identity_select_test".to_string(),
            "create_person".to_string(),
        ],
        vec![
            "identity_select_test".to_string(),
            "read_person".to_string(),
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
    let create_person = document
        .function_id(&["identity_select_test", "create_person"])
        .expect("apply must report create_person");
    let read_person = document
        .function_id(&["identity_select_test", "read_person"])
        .expect("apply must report read_person");
    assert_ne!(
        create_person, read_person,
        "the two function identities must be distinct"
    );
    let p_name = document
        .parameter_id(&["identity_select_test", "create_person"], "p_name")
        .expect("apply must report create_person.p_name");
    let p_person = document
        .parameter_id(&["identity_select_test", "read_person"], "p_person")
        .expect("apply must report read_person.p_person");
    assert_ne!(
        p_name, p_person,
        "the two parameter identities must be distinct"
    );

    // Both public calls deny before any explicit grant. The reader receives a
    // well-formed same-type-looking Reference only after the creator returns
    // one, so the reader denial below is proved with the created reference.
    let denied_create = machine
        .run_as_orna_with_stdin(
            &["raw-call", create_person, p_name],
            &text_orv1_envelope("Ada"),
        )
        .expect("run denied creator raw call");
    assert_denied("creator raw call before grant", denied_create)
        .expect("creator raw call must be denied before grant");

    let granted_creator = machine
        .run_as_orna(&["security", "grant-execute", create_person])
        .expect("run installed creator grant command");
    require_silent_success("orna security grant-execute create_person", granted_creator)
        .expect("creator grant must succeed silently");
    let ada_call = machine
        .run_as_orna_with_stdin(
            &["raw-call", create_person, p_name],
            &text_orv1_envelope("Ada"),
        )
        .expect("run Ada creator raw call");
    let ada_call = require_value_success("orna raw-call create_person Ada", ada_call)
        .expect("Ada creator must succeed");
    let ada = parse_reference_envelope(&ada_call.stdout)
        .expect("Ada creator must return one ORV reference");
    assert!(
        ada.type_id != [0; 16] && !ada.object_is_zero(),
        "Ada creator must return a real object reference"
    );

    let denied_reader = machine
        .run_as_orna_with_stdin(
            &["raw-call", read_person, p_person],
            &reference_orv1_envelope(ada.type_id, ada.object),
        )
        .expect("run denied reader raw call");
    assert_denied("identity reader raw call before grant", denied_reader)
        .expect("identity reader raw call must be denied before grant");

    let granted_reader = machine
        .run_as_orna(&["security", "grant-execute", read_person])
        .expect("run installed reader grant command");
    require_silent_success("orna security grant-execute read_person", granted_reader)
        .expect("reader grant must succeed silently");

    // The Reference binds only Ada and flattens the one returned row in its
    // declared Reference, Text, Boolean projection order.
    let read_ada = machine
        .run_as_orna_with_stdin(
            &["raw-call", read_person, p_person],
            &reference_orv1_envelope(ada.type_id, ada.object),
        )
        .expect("run Ada identity reader");
    let read_ada = require_value_success("orna raw-call read_person Ada", read_ada)
        .expect("Ada identity reader must succeed");
    assert_eq!(
        read_ada.stdout,
        identity_selected_person_envelopes(&ada, "Ada", true),
        "Ada identity reader must return its exact ordered projected cells"
    );

    // An absent object of the same type is a successful empty selected read.
    // Choose one fixed identity that cannot equal Ada's generated identity.
    let absent_object = if ada.object == [0xa5; 16] {
        [0x5a; 16]
    } else {
        [0xa5; 16]
    };
    let absent = machine
        .run_as_orna_with_stdin(
            &["raw-call", read_person, p_person],
            &reference_orv1_envelope(ada.type_id, absent_object),
        )
        .expect("run absent identity reader");
    require_silent_success("orna raw-call read_person absent", absent)
        .expect("an absent same-type Reference must select no values");

    // A second object proves the selector does not expose another object's
    // projected cells.
    let grace_call = machine
        .run_as_orna_with_stdin(
            &["raw-call", create_person, p_name],
            &text_orv1_envelope("Grace"),
        )
        .expect("run Grace creator raw call");
    let grace_call = require_value_success("orna raw-call create_person Grace", grace_call)
        .expect("Grace creator must succeed");
    let grace = parse_reference_envelope(&grace_call.stdout)
        .expect("Grace creator must return one ORV reference");
    assert_eq!(
        grace.type_id, ada.type_id,
        "both creators must return the same object type"
    );
    assert_ne!(
        grace.object, ada.object,
        "each creator call must return a distinct object identity"
    );
    let read_grace = machine
        .run_as_orna_with_stdin(
            &["raw-call", read_person, p_person],
            &reference_orv1_envelope(grace.type_id, grace.object),
        )
        .expect("run Grace identity reader");
    let read_grace = require_value_success("orna raw-call read_person Grace", read_grace)
        .expect("Grace identity reader must succeed");
    assert_eq!(
        read_grace.stdout,
        identity_selected_person_envelopes(&grace, "Grace", true),
        "Grace identity reader must return only Grace's exact ordered projected cells"
    );

    // Exact replay preserves the discovery identities, explicit grants, and
    // already-created References without a regrant.
    let replay = machine
        .run_as_orna(&["source", "apply", FIXTURE_PATH])
        .expect("run installed source apply replay");
    let replay = require_success("orna source apply replay", replay)
        .expect("identity select fixture replay must succeed");
    assert!(
        replay.stderr.is_empty(),
        "identity select fixture replay must keep standard error empty"
    );
    let replay_document =
        parse_apply_document(&replay.stdout).expect("identity select replay JSON must parse");
    assert_eq!(
        replay_document.functions, document.functions,
        "replay must retain both function and selector identities"
    );
    for (reference, name) in [(&ada, "Ada"), (&grace, "Grace")] {
        let read = machine
            .run_as_orna_with_stdin(
                &["raw-call", read_person, p_person],
                &reference_orv1_envelope(reference.type_id, reference.object),
            )
            .expect("run identity reader after replay");
        let read = require_value_success("orna raw-call read_person after replay", read)
            .expect("identity reader grant must survive replay");
        assert_eq!(
            read.stdout,
            identity_selected_person_envelopes(reference, name, true),
            "identity reader must retain the exact selected cells after replay"
        );
    }

    // Restart retains the original identities, grants, References, and rows.
    machine
        .restart_server()
        .expect("installed server must restart cleanly");
    for (reference, name) in [(&ada, "Ada"), (&grace, "Grace")] {
        let read = machine
            .run_as_orna_with_stdin(
                &["raw-call", read_person, p_person],
                &reference_orv1_envelope(reference.type_id, reference.object),
            )
            .expect("run identity reader after restart");
        let read = require_value_success("orna raw-call read_person after restart", read)
            .expect("identity reader grant must survive restart");
        assert_eq!(
            read.stdout,
            identity_selected_person_envelopes(reference, name, true),
            "identity reader must retain the exact selected cells after restart"
        );
    }
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

/// Prove that the installed product's canonical raw reference mutations
/// update and delete rows through the packaged `/usr/bin/orna` public
/// commands, and that the exact fixture reapplies and restarts without
/// changing identities, parameter bindings, grants, or the stored Boolean
/// multiset.
///
/// The test installs the exact checked-in `product_test_reference_mutations.orna`
/// fixture, applies it, and requires the four sorted qualified-name mappings
/// with pairwise distinct function identities and exact selector parameter
/// identities. It then proves:
///
/// * every create, read, update, and delete call is denied before any grant;
/// * after granting the four functions, two distinct rows are created, and
///   the parameterised constant-FALSE UPDATE of the first row returns a
///   reference byte-identical to the supplied selector;
/// * the reader returns exactly one FALSE and one TRUE value;
/// * the UPDATE of the deleted reference succeeds with no value event and
///   preserves the surviving row;
/// * the DELETE of the first row returns the exact ORV1 TRUE envelope, a
///   repeated DELETE completes empty, and the reader keeps only the second
///   row;
/// * an exact source replay keeps the complete function vector including
///   parameters, and the grants and data survive;
/// * a restart keeps the grants and rows, and the second row can still be
///   updated and deleted with the reader ending empty.
///
/// All observations go through the packaged `/usr/bin/orna` public commands
/// and raw-call ORV envelopes. The test makes no claim about physical storage
/// or private rows.
#[test]
#[ignore = "requires Docker, ORNA_SYSTEM_TEST_DEBIAN_PACKAGE, and the installed orna executable"]
fn installed_reference_mutations_update_delete_and_survive_replay_and_restart() {
    let package = std::env::var("ORNA_SYSTEM_TEST_DEBIAN_PACKAGE")
        .expect("ORNA_SYSTEM_TEST_DEBIAN_PACKAGE must point at the reproduced .deb package");
    let artifact = FrozenPackageArtifact::new(PackageFormat::Debian, &package)
        .expect("freeze the reproduced Debian package");
    let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("product_test_reference_mutations.orna");
    let fixture = fs::read(&fixture_path).expect("read the checked-in reference mutations fixture");

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
        vec![
            "reference_mutation_test".to_string(),
            "create_true".to_string(),
        ],
        vec![
            "reference_mutation_test".to_string(),
            "delete_entry".to_string(),
        ],
        vec![
            "reference_mutation_test".to_string(),
            "read_entries".to_string(),
        ],
        vec![
            "reference_mutation_test".to_string(),
            "update_false".to_string(),
        ],
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
        .function_id(&["reference_mutation_test", "create_true"])
        .expect("apply must report create_true");
    let update_false = document
        .function_id(&["reference_mutation_test", "update_false"])
        .expect("apply must report update_false");
    let delete_entry = document
        .function_id(&["reference_mutation_test", "delete_entry"])
        .expect("apply must report delete_entry");
    let read_entries = document
        .function_id(&["reference_mutation_test", "read_entries"])
        .expect("apply must report read_entries");
    for (left, right) in [
        (create_true, update_false),
        (create_true, delete_entry),
        (create_true, read_entries),
        (update_false, delete_entry),
        (update_false, read_entries),
        (delete_entry, read_entries),
    ] {
        assert_ne!(
            left, right,
            "the four function identities must be pairwise distinct"
        );
    }
    let update_parameter = document
        .parameter_id(&["reference_mutation_test", "update_false"], "p_entry")
        .expect("apply must report update_false.p_entry");
    let delete_parameter = document
        .parameter_id(&["reference_mutation_test", "delete_entry"], "p_entry")
        .expect("apply must report delete_entry.p_entry");
    assert_ne!(
        update_parameter, delete_parameter,
        "the two selector parameter identities must be pairwise distinct"
    );
    for name in ["create_true", "read_entries"] {
        let entry = document
            .functions
            .iter()
            .find(|entry| {
                entry
                    .names()
                    .iter()
                    .map(String::as_str)
                    .eq(["reference_mutation_test", name].iter().copied())
            })
            .expect("apply must report the function entry");
        assert!(
            entry.parameters().is_empty(),
            "{name} must declare no parameters"
        );
    }
    for (name, parameter) in [
        ("update_false", update_parameter),
        ("delete_entry", delete_parameter),
    ] {
        let entry = document
            .functions
            .iter()
            .find(|entry| {
                entry
                    .names()
                    .iter()
                    .map(String::as_str)
                    .eq(["reference_mutation_test", name].iter().copied())
            })
            .expect("apply must report the function entry");
        assert_eq!(
            entry.parameters().len(),
            1,
            "{name} must declare exactly one parameter"
        );
        let declared = &entry.parameters()[0];
        assert_eq!(
            declared.name(),
            "p_entry",
            "{name} must declare exactly the p_entry parameter"
        );
        assert_eq!(
            declared.parameter_id(),
            parameter,
            "the declared parameter must equal the discovered identity"
        );
    }

    // Every create, read, update, and delete call is denied before any grant.
    let denied_create = machine
        .run_as_orna(&["raw-call", create_true])
        .expect("run denied create call");
    assert_denied("create before grant", denied_create).expect("create must be denied");
    let denied_read = machine
        .run_as_orna(&["raw-call", read_entries])
        .expect("run denied read call");
    assert_denied("read before grant", denied_read).expect("read must be denied");
    let pre_grant_selector = reference_orv1_envelope([0x11; 16], [0x22; 16]);
    let denied_update = machine
        .run_as_orna_with_stdin(
            &["raw-call", update_false, update_parameter],
            &pre_grant_selector,
        )
        .expect("run denied update call");
    assert_denied("update before grant", denied_update).expect("update must be denied");
    let denied_delete = machine
        .run_as_orna_with_stdin(
            &["raw-call", delete_entry, delete_parameter],
            &pre_grant_selector,
        )
        .expect("run denied delete call");
    assert_denied("delete before grant", denied_delete).expect("delete must be denied");

    // Grant the four exact functions.
    for function in [create_true, update_false, delete_entry, read_entries] {
        let granted = machine
            .run_as_orna(&["security", "grant-execute", function])
            .expect("run installed grant command");
        require_silent_success("orna security grant-execute", granted)
            .expect("grant must succeed silently");
    }

    // Create two distinct rows.
    let first_created = machine
        .run_as_orna(&["raw-call", create_true])
        .expect("run first create call");
    let first_created = require_value_success("orna raw-call create_true first", first_created)
        .expect("first create must succeed");
    let first = parse_reference_envelope(&first_created.stdout)
        .expect("first create must return one ORV reference");
    let second_created = machine
        .run_as_orna(&["raw-call", create_true])
        .expect("run second create call");
    let second_created = require_value_success("orna raw-call create_true second", second_created)
        .expect("second create must succeed");
    let second = parse_reference_envelope(&second_created.stdout)
        .expect("second create must return one ORV reference");
    assert!(
        !first.object_is_zero() && !second.object_is_zero(),
        "both created references must name real rows"
    );
    assert_eq!(
        first.type_id, second.type_id,
        "both created references must target the same object type"
    );
    assert_ne!(
        first.object, second.object,
        "the two created references must be distinct"
    );

    // UPDATE the first row and require the returned reference to be
    // byte-identical to the supplied selector.
    let first_selector = reference_orv1_envelope(first.type_id, first.object);
    let updated = machine
        .run_as_orna_with_stdin(
            &["raw-call", update_false, update_parameter],
            &first_selector,
        )
        .expect("run first UPDATE call");
    let updated = require_value_success("orna raw-call update_false first", updated)
        .expect("first UPDATE must succeed");
    assert_eq!(
        updated.stdout, first_selector,
        "the UPDATE must return a reference byte-identical to the selector"
    );

    // The reader returns exactly one FALSE and one TRUE value.
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
        "the UPDATE must move the first row to FALSE and keep the second TRUE"
    );

    // DELETE the first row returns the exact ORV1 TRUE envelope.
    let deleted = machine
        .run_as_orna_with_stdin(
            &["raw-call", delete_entry, delete_parameter],
            &first_selector,
        )
        .expect("run first DELETE call");
    let deleted = require_value_success("orna raw-call delete_entry first", deleted)
        .expect("first DELETE must succeed");
    assert_eq!(
        deleted.stdout.as_slice(),
        boolean_orv1_envelope(Some(true)).as_slice(),
        "the DELETE must return the exact canonical Boolean TRUE envelope"
    );

    // A repeated DELETE completes empty.
    let repeated = machine
        .run_as_orna_with_stdin(
            &["raw-call", delete_entry, delete_parameter],
            &first_selector,
        )
        .expect("run repeated DELETE call");
    require_silent_success("orna raw-call delete_entry repeated", repeated)
        .expect("repeated DELETE must complete empty");

    // An UPDATE of the deleted reference completes empty and preserves the
    // surviving row.
    let deleted_update = machine
        .run_as_orna_with_stdin(
            &["raw-call", update_false, update_parameter],
            &first_selector,
        )
        .expect("run UPDATE of the deleted reference");
    require_silent_success("orna raw-call update_false deleted", deleted_update)
        .expect("UPDATE of the deleted reference must complete empty");

    // The reader keeps only the second row.
    let mut one_value = decode_reader_values(
        &machine,
        read_entries,
        "orna raw-call read_entries one value",
    )
    .expect("one-value read must decode");
    one_value.sort();
    assert_eq!(
        one_value,
        vec![true],
        "the UPDATE of the deleted reference must preserve the surviving TRUE row"
    );

    // Exact source replay keeps the complete function vector including
    // parameters, and the grants and data survive without re-granting.
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
        "the replay must keep the complete function vector including parameters"
    );
    let replay_update_parameter = replay_document
        .parameter_id(&["reference_mutation_test", "update_false"], "p_entry")
        .expect("the replay must keep update_false.p_entry");
    let replay_delete_parameter = replay_document
        .parameter_id(&["reference_mutation_test", "delete_entry"], "p_entry")
        .expect("the replay must keep delete_entry.p_entry");
    assert_eq!(
        replay_update_parameter, update_parameter,
        "the replay must keep the exact update selector parameter identity"
    );
    assert_eq!(
        replay_delete_parameter, delete_parameter,
        "the replay must keep the exact delete selector parameter identity"
    );
    let surviving = decode_reader_values(
        &machine,
        read_entries,
        "orna raw-call read_entries after replay",
    )
    .expect("post-replay read must decode");
    assert_eq!(
        surviving,
        vec![true],
        "the replay must preserve the surviving TRUE row"
    );

    // Restart keeps grants and rows; update then delete the second row using
    // the surviving grants, and the final readers are empty.
    machine
        .restart_server()
        .expect("installed server must restart cleanly");
    let second_selector = reference_orv1_envelope(second.type_id, second.object);
    let updated_second = machine
        .run_as_orna_with_stdin(
            &["raw-call", update_false, update_parameter],
            &second_selector,
        )
        .expect("run second UPDATE call after restart");
    let updated_second = require_value_success("orna raw-call update_false second", updated_second)
        .expect("second UPDATE must succeed after restart");
    assert_eq!(
        updated_second.stdout, second_selector,
        "the second UPDATE must return a reference byte-identical to the selector"
    );

    // The reader causally proves the second row moved to FALSE before it is
    // deleted.
    let mut updated_second_values = decode_reader_values(
        &machine,
        read_entries,
        "orna raw-call read_entries after second UPDATE",
    )
    .expect("post-update read must decode");
    updated_second_values.sort();
    assert_eq!(
        updated_second_values,
        vec![false],
        "the second UPDATE must leave exactly one FALSE row"
    );
    let deleted_second = machine
        .run_as_orna_with_stdin(
            &["raw-call", delete_entry, delete_parameter],
            &second_selector,
        )
        .expect("run second DELETE call after restart");
    let deleted_second = require_value_success("orna raw-call delete_entry second", deleted_second)
        .expect("second DELETE must succeed after restart");
    assert_eq!(
        deleted_second.stdout.as_slice(),
        boolean_orv1_envelope(Some(true)).as_slice(),
        "the second DELETE must return the exact canonical Boolean TRUE envelope"
    );
    let final_read = machine
        .run_as_orna(&["raw-call", read_entries])
        .expect("run final empty raw select");
    require_silent_success("orna raw-call read_entries final", final_read)
        .expect("the final reader must be empty");
}

/// Installed public-boundary journey for a nullable Boolean UPDATE through
/// exact reference selectors in the installed product.
///
/// The test installs the exact checked-in
/// `product_test_nullable_update.orna` fixture and applies it. It requires
/// exactly four sorted qualified-name mappings with pairwise distinct
/// function identities, and proves that only `update_null` declares the
/// `p_entry` reference parameter. All four raw calls are denied before any
/// grant, including `update_null` with a syntactically valid synthetic
/// reference selector. After granting the four exact functions, `create_true`
/// and `create_false` each return one real object reference with the same
/// nonzero target type and distinct nonzero object ids. The unordered reader
/// first returns `[Some(false), Some(true)]`. Calling `update_null` with the
/// TRUE reference and its canonical parameter id returns status 0, empty
/// standard error, and stdout byte-identical to the supplied selector
/// envelope. The unordered reader then returns `[None, Some(false)]`,
/// causally proving the UPDATE moved only the selected row to a typed NULL.
///
/// Reapplying the exact same fixture returns a second public JSON document
/// whose complete function discovery vector, including every function and
/// parameter identity, equals the first, and no grant is repeated. A third
/// `create_true` call through the original function id returns a third
/// distinct reference with the same target type, and `update_null` on that
/// selector returns the selector envelope exactly. The unordered reader now
/// returns `[None, None, Some(false)]`. A restart of the installed service
/// through the machine API keeps that exact multiset. After restart,
/// `update_null` on the original FALSE reference returns its selector
/// envelope exactly, and the final unordered reader returns
/// `[None, None, None]`.
///
/// All observations go through the packaged `/usr/bin/orna` public commands
/// and raw-call ORV envelopes. The test makes no claim about physical
/// storage, private rows, or row ordering.
#[test]
#[ignore = "requires Docker, ORNA_SYSTEM_TEST_DEBIAN_PACKAGE, and the installed orna executable"]
fn installed_nullable_update_sets_stored_null_via_reference_selector() {
    let package = std::env::var("ORNA_SYSTEM_TEST_DEBIAN_PACKAGE")
        .expect("ORNA_SYSTEM_TEST_DEBIAN_PACKAGE must point at the reproduced .deb package");
    let artifact = FrozenPackageArtifact::new(PackageFormat::Debian, &package)
        .expect("freeze the reproduced Debian package");
    let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("product_test_nullable_update.orna");
    let fixture = fs::read(&fixture_path).expect("read the checked-in nullable update fixture");

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
        vec![
            "nullable_update_test".to_string(),
            "create_false".to_string(),
        ],
        vec![
            "nullable_update_test".to_string(),
            "create_true".to_string(),
        ],
        vec![
            "nullable_update_test".to_string(),
            "read_entries".to_string(),
        ],
        vec![
            "nullable_update_test".to_string(),
            "update_null".to_string(),
        ],
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
        .function_id(&["nullable_update_test", "create_true"])
        .expect("apply must report create_true");
    let create_false = document
        .function_id(&["nullable_update_test", "create_false"])
        .expect("apply must report create_false");
    let read_entries = document
        .function_id(&["nullable_update_test", "read_entries"])
        .expect("apply must report read_entries");
    let update_null = document
        .function_id(&["nullable_update_test", "update_null"])
        .expect("apply must report update_null");
    for (left, right) in [
        (create_true, create_false),
        (create_true, read_entries),
        (create_true, update_null),
        (create_false, read_entries),
        (create_false, update_null),
        (read_entries, update_null),
    ] {
        assert_ne!(
            left, right,
            "the four function identities must be pairwise distinct"
        );
    }
    let update_parameter = document
        .parameter_id(&["nullable_update_test", "update_null"], "p_entry")
        .expect("apply must report update_null.p_entry");
    for name in ["create_false", "create_true", "read_entries"] {
        let entry = document
            .functions
            .iter()
            .find(|entry| {
                entry
                    .names()
                    .iter()
                    .map(String::as_str)
                    .eq(["nullable_update_test", name].iter().copied())
            })
            .expect("apply must report the function entry");
        assert!(
            entry.parameters().is_empty(),
            "{name} must declare no parameters"
        );
    }
    let update_entry = document
        .functions
        .iter()
        .find(|entry| {
            entry
                .names()
                .iter()
                .map(String::as_str)
                .eq(["nullable_update_test", "update_null"].iter().copied())
        })
        .expect("apply must report update_null");
    assert_eq!(
        update_entry.parameters().len(),
        1,
        "update_null must declare exactly one parameter"
    );
    let declared = &update_entry.parameters()[0];
    assert_eq!(
        declared.name(),
        "p_entry",
        "update_null must declare exactly the p_entry parameter"
    );
    assert_eq!(
        declared.parameter_id(),
        update_parameter,
        "the declared parameter must equal the discovered identity"
    );

    // Every create, read, and update call is denied before any grant. The
    // update selector is a syntactically valid synthetic reference because
    // no row exists yet.
    let denied_create_true = machine
        .run_as_orna(&["raw-call", create_true])
        .expect("run denied create_true call");
    assert_denied("create_true before grant", denied_create_true)
        .expect("create_true must be denied");
    let denied_create_false = machine
        .run_as_orna(&["raw-call", create_false])
        .expect("run denied create_false call");
    assert_denied("create_false before grant", denied_create_false)
        .expect("create_false must be denied");
    let denied_read = machine
        .run_as_orna(&["raw-call", read_entries])
        .expect("run denied read call");
    assert_denied("read before grant", denied_read).expect("read must be denied");
    let pre_grant_selector = reference_orv1_envelope([0x11; 16], [0x22; 16]);
    let denied_update = machine
        .run_as_orna_with_stdin(
            &["raw-call", update_null, update_parameter],
            &pre_grant_selector,
        )
        .expect("run denied update call");
    assert_denied("update before grant", denied_update).expect("update must be denied");

    // Grant the four exact functions.
    for function in [create_true, create_false, read_entries, update_null] {
        let granted = machine
            .run_as_orna(&["security", "grant-execute", function])
            .expect("run installed grant command");
        require_silent_success("orna security grant-execute", granted)
            .expect("grant must succeed silently");
    }

    // Create one TRUE and one FALSE row.
    let true_created = machine
        .run_as_orna(&["raw-call", create_true])
        .expect("run TRUE create call");
    let true_created = require_value_success("orna raw-call create_true", true_created)
        .expect("TRUE create must succeed");
    let true_reference = parse_reference_envelope(&true_created.stdout)
        .expect("TRUE create must return one ORV reference");
    let false_created = machine
        .run_as_orna(&["raw-call", create_false])
        .expect("run FALSE create call");
    let false_created = require_value_success("orna raw-call create_false", false_created)
        .expect("FALSE create must succeed");
    let false_reference = parse_reference_envelope(&false_created.stdout)
        .expect("FALSE create must return one ORV reference");
    assert!(
        !true_reference.object_is_zero() && !false_reference.object_is_zero(),
        "both created references must name real rows"
    );
    assert_ne!(
        true_reference.type_id, [0; 16],
        "the created references must target a real nonzero object type"
    );
    assert_eq!(
        true_reference.type_id, false_reference.type_id,
        "both created references must target the same object type"
    );
    assert_ne!(
        true_reference.object, false_reference.object,
        "the two created references must be distinct"
    );

    // The reader returns exactly one FALSE and one TRUE value.
    let mut two_values = decode_mixed_reader_values(
        &machine,
        read_entries,
        "orna raw-call read_entries two values",
    )
    .expect("two-value read must decode");
    two_values.sort();
    assert_eq!(
        two_values,
        vec![Some(false), Some(true)],
        "the two created rows must store FALSE and TRUE"
    );

    // UPDATE the TRUE row through its canonical parameter and require the
    // returned reference to be byte-identical to the supplied selector.
    let true_selector = reference_orv1_envelope(true_reference.type_id, true_reference.object);
    let updated = machine
        .run_as_orna_with_stdin(&["raw-call", update_null, update_parameter], &true_selector)
        .expect("run NULL UPDATE call");
    let updated = require_value_success("orna raw-call update_null TRUE", updated)
        .expect("NULL UPDATE must succeed");
    assert_eq!(
        updated.stdout, true_selector,
        "the UPDATE must return a reference byte-identical to the selector"
    );

    // The reader causally proves the TRUE row moved to a typed NULL while
    // the FALSE row survives.
    let mut after_update = decode_mixed_reader_values(
        &machine,
        read_entries,
        "orna raw-call read_entries after update",
    )
    .expect("post-update read must decode");
    after_update.sort();
    assert_eq!(
        after_update,
        vec![None, Some(false)],
        "the UPDATE must move only the selected row to NULL"
    );

    // Reapplying the exact fixture returns the identical complete discovery
    // vector, and the grants survive without re-granting.
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
        "the replay must keep the complete function discovery vector including all identities"
    );

    // A third create through the original function id returns a third
    // distinct reference, and update_null returns its selector exactly.
    let third_created = machine
        .run_as_orna(&["raw-call", create_true])
        .expect("run third create call");
    let third_created = require_value_success("orna raw-call create_true third", third_created)
        .expect("third create must succeed");
    let third_reference = parse_reference_envelope(&third_created.stdout)
        .expect("third create must return one ORV reference");
    assert!(
        !third_reference.object_is_zero(),
        "the third created reference must name a real row"
    );
    assert_eq!(
        third_reference.type_id, true_reference.type_id,
        "the third created reference must target the same object type"
    );
    assert_ne!(
        third_reference.object, true_reference.object,
        "the third created reference must be distinct from the TRUE reference"
    );
    assert_ne!(
        third_reference.object, false_reference.object,
        "the third created reference must be distinct from the FALSE reference"
    );
    let third_selector = reference_orv1_envelope(third_reference.type_id, third_reference.object);
    let updated_third = machine
        .run_as_orna_with_stdin(
            &["raw-call", update_null, update_parameter],
            &third_selector,
        )
        .expect("run NULL UPDATE on the third row");
    let updated_third = require_value_success("orna raw-call update_null third", updated_third)
        .expect("third NULL UPDATE must succeed");
    assert_eq!(
        updated_third.stdout, third_selector,
        "the third UPDATE must return a reference byte-identical to the selector"
    );

    // The reader now returns one FALSE and two NULL rows.
    let mut three_values = decode_mixed_reader_values(
        &machine,
        read_entries,
        "orna raw-call read_entries three values",
    )
    .expect("three-value read must decode");
    three_values.sort();
    assert_eq!(
        three_values,
        vec![None, None, Some(false)],
        "the replay and third UPDATE must leave exactly one FALSE and two NULL rows"
    );

    // A restart keeps the grants, rows, and the exact mixed multiset.
    machine
        .restart_server()
        .expect("installed server must restart cleanly");
    let mut after_restart = decode_mixed_reader_values(
        &machine,
        read_entries,
        "orna raw-call read_entries after restart",
    )
    .expect("post-restart read must decode");
    after_restart.sort();
    assert_eq!(
        after_restart,
        vec![None, None, Some(false)],
        "the restart must preserve the exact mixed multiset"
    );

    // UPDATE the original FALSE row after restart and read all NULLs.
    let false_selector = reference_orv1_envelope(false_reference.type_id, false_reference.object);
    let updated_false = machine
        .run_as_orna_with_stdin(
            &["raw-call", update_null, update_parameter],
            &false_selector,
        )
        .expect("run NULL UPDATE on the original FALSE row");
    let updated_false = require_value_success("orna raw-call update_null FALSE", updated_false)
        .expect("FALSE NULL UPDATE must succeed after restart");
    assert_eq!(
        updated_false.stdout, false_selector,
        "the FALSE UPDATE must return a reference byte-identical to the selector"
    );
    let mut final_values =
        decode_mixed_reader_values(&machine, read_entries, "orna raw-call read_entries final")
            .expect("final read must decode");
    final_values.sort();
    assert_eq!(
        final_values,
        vec![None, None, None],
        "the final UPDATE must leave exactly three NULL rows"
    );
}

/// Installed public-boundary tracer for additive source activation.
///
/// The test installs the exact checked-in `product_test.orna` fixture and
/// applies it. It requires exactly two sorted qualified-name mappings with
/// pairwise distinct identities, proves both raw calls are denied before any
/// grant, grants both, creates one real TRUE probe row, and requires the
/// reader to return the exact canonical Boolean TRUE envelope.
///
/// The fixture is then replaced through the machine API with the checked-in
/// `product_test_additive.orna` source and applied again. The second public
/// JSON document must report exactly four sorted qualified-name mappings
/// (`added_test.create_entry`, `added_test.read_entries`,
/// `product_test.create_probe`, `product_test.read_probes`). The two
/// `product_test` function identities must equal the original identities, and
/// the two `added_test` identities must be pairwise distinct and distinct
/// from both original identities.
///
/// Without any repeated grant, the original reader still returns the exact
/// TRUE envelope, the original create still succeeds through the surviving
/// grant and returns a second same-type distinct-object reference, and the
/// original reader then returns exactly two canonical Boolean TRUE envelopes.
/// Before any new grant, both `added_test` raw calls are denied. After
/// granting only the two `added_test` functions, the added create returns a
/// real nonzero reference whose target type differs from the `product_test`
/// target, the added reader returns the exact canonical Boolean FALSE
/// envelope, and the original reader still returns exactly two TRUE
/// envelopes.
///
/// Reapplying the exact same additive fixture returns a replay public JSON
/// document whose complete function discovery vector equals the first
/// additive document, and no grant is repeated. A second added create through
/// the original identity returns a same-target distinct-object reference, the
/// added reader returns exactly two canonical Boolean FALSE envelopes, and
/// the product_test reader still returns exactly two TRUE envelopes. A
/// restart of the installed service through the machine API keeps both exact
/// two-envelope results. After restart, one create through each original
/// identity, again without any repeated grant, returns references with each
/// schema's original target type, real nonzero identities, and object ids
/// distinct from every prior reference in that schema; the target types still
/// differ across schemas. The final readers return exactly three canonical
/// Boolean TRUE envelopes for product_test and exactly three canonical
/// Boolean FALSE envelopes for added_test.
///
/// The test claims only public packaged `/usr/bin/orna` commands, public ORV1
/// outputs, additive source activation, stable old identities, grants, and
/// rows, isolated new behaviour, and replay/restart preservation of those
/// observations. It makes no claim about private storage, physical DDL, or
/// row ordering.
#[test]
#[ignore = "requires Docker, ORNA_SYSTEM_TEST_DEBIAN_PACKAGE, and the installed orna executable"]
fn installed_additive_source_activates_isolated_new_schema_with_stable_old_identities_grants_and_rows()
 {
    let package = std::env::var("ORNA_SYSTEM_TEST_DEBIAN_PACKAGE")
        .expect("ORNA_SYSTEM_TEST_DEBIAN_PACKAGE must point at the reproduced .deb package");
    let artifact = FrozenPackageArtifact::new(PackageFormat::Debian, &package)
        .expect("freeze the reproduced Debian package");
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let original_fixture =
        fs::read(manifest.join("product_test.orna")).expect("read the checked-in product fixture");
    let additive_fixture = fs::read(manifest.join("product_test_additive.orna"))
        .expect("read the checked-in additive fixture");

    let machine = InstalledMachine::start(&artifact, &original_fixture)
        .expect("start the installed Debian test machine");

    // Apply the original one-file fixture and require the two sorted mappings.
    let apply = machine
        .run_as_orna(&["source", "apply", FIXTURE_PATH])
        .expect("run installed source apply");
    let apply = require_success("orna source apply", apply).expect("source apply must succeed");
    assert!(
        apply.stderr.is_empty(),
        "original source apply must keep standard error empty"
    );
    let document = parse_apply_document(&apply.stdout).expect("source apply JSON must parse");
    let original_order = [
        vec!["product_test".to_string(), "create_probe".to_string()],
        vec!["product_test".to_string(), "read_probes".to_string()],
    ];
    let actual_order = document
        .functions
        .iter()
        .map(|function| function.names().to_vec())
        .collect::<Vec<_>>();
    assert_eq!(
        actual_order, original_order,
        "apply must report the two function entries sorted by qualified name"
    );
    let create_probe = document
        .function_id(&["product_test", "create_probe"])
        .expect("apply must report create_probe");
    let read_probes = document
        .function_id(&["product_test", "read_probes"])
        .expect("apply must report read_probes");
    assert_ne!(
        create_probe, read_probes,
        "the two original function identities must be pairwise distinct"
    );

    // Both raw calls are denied before any grant, then both are granted.
    for function in [create_probe, read_probes] {
        let denied = machine
            .run_as_orna(&["raw-call", function])
            .expect("run denied raw call");
        assert_denied("raw call before grant", denied).expect("raw call must be denied");
    }
    for function in [create_probe, read_probes] {
        let granted = machine
            .run_as_orna(&["security", "grant-execute", function])
            .expect("run installed grant command");
        require_silent_success("orna security grant-execute", granted)
            .expect("grant must succeed silently");
    }

    // Create one TRUE probe row and require the exact TRUE reader envelope.
    let first_created = machine
        .run_as_orna(&["raw-call", create_probe])
        .expect("run first create call");
    let first_created = require_value_success("orna raw-call create_probe first", first_created)
        .expect("first create must succeed");
    let first_reference = parse_reference_envelope(&first_created.stdout)
        .expect("first create must return one ORV reference");
    assert!(
        first_reference.type_id != [0; 16] && !first_reference.object_is_zero(),
        "the first created reference must name a real target type and row"
    );
    let first_read = machine
        .run_as_orna(&["raw-call", read_probes])
        .expect("run first read call");
    assert_exact_boolean_true("orna raw-call read_probes first", first_read)
        .expect("the first reader must return the exact Boolean TRUE value");

    // Replace the fixture with the additive source and apply it.
    machine
        .write_fixture(&additive_fixture)
        .expect("replace the fixture with the additive source");
    let applied = machine
        .run_as_orna(&["source", "apply", FIXTURE_PATH])
        .expect("run installed source apply on the additive fixture");
    let applied = require_success("orna source apply additive", applied)
        .expect("additive source apply must succeed");
    assert!(
        applied.stderr.is_empty(),
        "additive source apply must keep standard error empty"
    );
    let additive_document =
        parse_apply_document(&applied.stdout).expect("additive source apply JSON must parse");
    let additive_order = [
        vec!["added_test".to_string(), "create_entry".to_string()],
        vec!["added_test".to_string(), "read_entries".to_string()],
        vec!["product_test".to_string(), "create_probe".to_string()],
        vec!["product_test".to_string(), "read_probes".to_string()],
    ];
    let actual_additive_order = additive_document
        .functions
        .iter()
        .map(|function| function.names().to_vec())
        .collect::<Vec<_>>();
    assert_eq!(
        actual_additive_order, additive_order,
        "the additive apply must report the four function entries sorted by qualified name"
    );
    let additive_create_probe = additive_document
        .function_id(&["product_test", "create_probe"])
        .expect("additive apply must report create_probe");
    let additive_read_probes = additive_document
        .function_id(&["product_test", "read_probes"])
        .expect("additive apply must report read_probes");
    assert_eq!(
        additive_create_probe, create_probe,
        "create_probe identity must be stable across the additive apply"
    );
    assert_eq!(
        additive_read_probes, read_probes,
        "read_probes identity must be stable across the additive apply"
    );
    let added_create_entry = additive_document
        .function_id(&["added_test", "create_entry"])
        .expect("additive apply must report create_entry");
    let added_read_entries = additive_document
        .function_id(&["added_test", "read_entries"])
        .expect("additive apply must report read_entries");
    for (left, right) in [
        (added_create_entry, added_read_entries),
        (added_create_entry, create_probe),
        (added_create_entry, read_probes),
        (added_read_entries, create_probe),
        (added_read_entries, read_probes),
    ] {
        assert_ne!(
            left, right,
            "the added identities must be pairwise distinct and distinct from both originals"
        );
    }

    // Without any repeated grant the old row, reader, create, and grant stay.
    let surviving_read = machine
        .run_as_orna(&["raw-call", read_probes])
        .expect("run read call after additive apply");
    assert_exact_boolean_true(
        "orna raw-call read_probes after additive apply",
        surviving_read,
    )
    .expect("the surviving row must still return the exact Boolean TRUE value");
    let second_created = machine
        .run_as_orna(&["raw-call", create_probe])
        .expect("run second create call after additive apply");
    let second_created = require_value_success("orna raw-call create_probe second", second_created)
        .expect("second create must succeed through the surviving grant");
    let second_reference = parse_reference_envelope(&second_created.stdout)
        .expect("second create must return one ORV reference");
    assert_ne!(
        second_reference.type_id, [0; 16],
        "the second created reference must name a real target type"
    );
    assert!(
        !second_reference.object_is_zero(),
        "the second created reference must name a real row"
    );
    assert_eq!(
        second_reference.type_id, first_reference.type_id,
        "both product_test creates must target the same object type"
    );
    assert_ne!(
        second_reference.object, first_reference.object,
        "each product_test create must allocate a distinct object identity"
    );
    let two_probes = machine
        .run_as_orna(&["raw-call", read_probes])
        .expect("run read call after second create");
    let two_probes = require_value_success("orna raw-call read_probes two rows", two_probes)
        .expect("two-row read must succeed");
    assert_eq!(
        two_probes.stdout.as_slice(),
        two_boolean_true_envelopes().as_slice(),
        "the product_test reader must emit exactly two canonical Boolean TRUE envelopes"
    );

    // The added functions are denied before their grant.
    for function in [added_create_entry, added_read_entries] {
        let denied = machine
            .run_as_orna(&["raw-call", function])
            .expect("run denied added call");
        assert_denied("added call before grant", denied).expect("added call must be denied");
    }

    // Grant only the two added functions and exercise the isolated schema.
    for function in [added_create_entry, added_read_entries] {
        let granted = machine
            .run_as_orna(&["security", "grant-execute", function])
            .expect("run installed grant command for the added function");
        require_silent_success("orna security grant-execute added", granted)
            .expect("added grant must succeed silently");
    }
    let added_created = machine
        .run_as_orna(&["raw-call", added_create_entry])
        .expect("run added create call");
    let added_created = require_value_success("orna raw-call create_entry", added_created)
        .expect("added create must succeed");
    let added_reference = parse_reference_envelope(&added_created.stdout)
        .expect("added create must return one ORV reference");
    assert!(
        added_reference.type_id != [0; 16] && !added_reference.object_is_zero(),
        "the added created reference must name a real target type and row"
    );
    assert_ne!(
        added_reference.type_id, first_reference.type_id,
        "the added object type must differ from the product_test object type"
    );
    let added_read = machine
        .run_as_orna(&["raw-call", added_read_entries])
        .expect("run added read call");
    let added_read = require_value_success("orna raw-call read_entries", added_read)
        .expect("added read must succeed");
    assert_eq!(
        added_read.stdout.as_slice(),
        boolean_orv1_envelope(Some(false)).as_slice(),
        "the added reader must return the exact canonical Boolean FALSE value"
    );
    let probes_after_added = machine
        .run_as_orna(&["raw-call", read_probes])
        .expect("run product_test read after added create");
    let probes_after_added =
        require_value_success("orna raw-call read_probes after added", probes_after_added)
            .expect("product_test read must succeed after the added create");
    assert_eq!(
        probes_after_added.stdout.as_slice(),
        two_boolean_true_envelopes().as_slice(),
        "the added create must not change the two product_test TRUE rows"
    );

    // Reapply the exact additive fixture; the discovery vector must be
    // identical and no grant is repeated.
    let replay = machine
        .run_as_orna(&["source", "apply", FIXTURE_PATH])
        .expect("run installed source apply on the additive fixture");
    let replay = require_success("orna source apply replay", replay)
        .expect("additive fixture replay must succeed");
    assert!(
        replay.stderr.is_empty(),
        "fixture replay must keep standard error empty"
    );
    let replay_document =
        parse_apply_document(&replay.stdout).expect("fixture replay JSON must parse");
    assert_eq!(
        replay_document.functions, additive_document.functions,
        "the replay must keep the complete function discovery vector including all identities"
    );

    // A second added create returns a same-target distinct-object reference.
    let second_added_created = machine
        .run_as_orna(&["raw-call", added_create_entry])
        .expect("run second added create call");
    let second_added_created =
        require_value_success("orna raw-call create_entry second", second_added_created)
            .expect("second added create must succeed through the surviving grant");
    let second_added_reference = parse_reference_envelope(&second_added_created.stdout)
        .expect("second added create must return one ORV reference");
    assert!(
        second_added_reference.type_id != [0; 16] && !second_added_reference.object_is_zero(),
        "the second added reference must name a real target type and row"
    );
    assert_eq!(
        second_added_reference.type_id, added_reference.type_id,
        "both added creates must target the same object type"
    );
    assert_ne!(
        second_added_reference.object, added_reference.object,
        "each added create must allocate a distinct object identity"
    );

    // Both readers return exactly two envelopes after the second added row.
    let mut two_false_envelopes = boolean_orv1_envelope(Some(false));
    two_false_envelopes.extend(boolean_orv1_envelope(Some(false)));
    let added_two = machine
        .run_as_orna(&["raw-call", added_read_entries])
        .expect("run added read call after second create");
    let added_two = require_value_success("orna raw-call read_entries two rows", added_two)
        .expect("two-row added read must succeed");
    assert_eq!(
        added_two.stdout.as_slice(),
        two_false_envelopes.as_slice(),
        "the added reader must emit exactly two canonical Boolean FALSE envelopes"
    );
    let probes_two = machine
        .run_as_orna(&["raw-call", read_probes])
        .expect("run product_test read after second added create");
    let probes_two = require_value_success("orna raw-call read_probes two rows", probes_two)
        .expect("two-row product_test read must succeed");
    assert_eq!(
        probes_two.stdout.as_slice(),
        two_boolean_true_envelopes().as_slice(),
        "the product_test reader must still emit exactly two Boolean TRUE envelopes"
    );

    // A restart keeps both exact two-envelope results.
    machine
        .restart_server()
        .expect("installed server must restart cleanly");
    let after_restart_added = machine
        .run_as_orna(&["raw-call", added_read_entries])
        .expect("run added read call after restart");
    let after_restart_added = require_value_success(
        "orna raw-call read_entries after restart",
        after_restart_added,
    )
    .expect("added read must succeed after restart");
    assert_eq!(
        after_restart_added.stdout.as_slice(),
        two_false_envelopes.as_slice(),
        "the restart must preserve the two added FALSE envelopes"
    );
    let after_restart_probes = machine
        .run_as_orna(&["raw-call", read_probes])
        .expect("run product_test read after restart");
    let after_restart_probes = require_value_success(
        "orna raw-call read_probes after restart",
        after_restart_probes,
    )
    .expect("product_test read must succeed after restart");
    assert_eq!(
        after_restart_probes.stdout.as_slice(),
        two_boolean_true_envelopes().as_slice(),
        "the restart must preserve the two product_test TRUE envelopes"
    );

    // One create through each original identity without any repeated grant.
    let third_probe_created = machine
        .run_as_orna(&["raw-call", create_probe])
        .expect("run third product_test create call");
    let third_probe_created =
        require_value_success("orna raw-call create_probe third", third_probe_created)
            .expect("third product_test create must succeed after restart");
    let third_probe_reference = parse_reference_envelope(&third_probe_created.stdout)
        .expect("third product_test create must return one ORV reference");
    assert!(
        third_probe_reference.type_id != [0; 16] && !third_probe_reference.object_is_zero(),
        "the third product_test reference must name a real target type and row"
    );
    assert_eq!(
        third_probe_reference.type_id, first_reference.type_id,
        "the third product_test reference must keep the original target type"
    );
    assert_ne!(
        third_probe_reference.object, first_reference.object,
        "the third product_test reference must be distinct from the first"
    );
    assert_ne!(
        third_probe_reference.object, second_reference.object,
        "the third product_test reference must be distinct from the second"
    );
    let third_added_created = machine
        .run_as_orna(&["raw-call", added_create_entry])
        .expect("run third added create call");
    let third_added_created =
        require_value_success("orna raw-call create_entry third", third_added_created)
            .expect("third added create must succeed after restart");
    let third_added_reference = parse_reference_envelope(&third_added_created.stdout)
        .expect("third added create must return one ORV reference");
    assert!(
        third_added_reference.type_id != [0; 16] && !third_added_reference.object_is_zero(),
        "the third added reference must name a real target type and row"
    );
    assert_eq!(
        third_added_reference.type_id, added_reference.type_id,
        "the third added reference must keep the original added target type"
    );
    assert_ne!(
        third_added_reference.object, added_reference.object,
        "the third added reference must be distinct from the first"
    );
    assert_ne!(
        third_added_reference.object, second_added_reference.object,
        "the third added reference must be distinct from the second"
    );
    assert_ne!(
        third_probe_reference.type_id, third_added_reference.type_id,
        "the target types must still differ across schemas"
    );

    // Final reads: exactly three TRUE and three FALSE envelopes.
    let mut three_true_envelopes = boolean_orv1_envelope(Some(true));
    three_true_envelopes.extend(boolean_orv1_envelope(Some(true)));
    three_true_envelopes.extend(boolean_orv1_envelope(Some(true)));
    let final_probes = machine
        .run_as_orna(&["raw-call", read_probes])
        .expect("run final product_test read");
    let final_probes = require_value_success("orna raw-call read_probes final", final_probes)
        .expect("final product_test read must succeed");
    assert_eq!(
        final_probes.stdout.as_slice(),
        three_true_envelopes.as_slice(),
        "the product_test reader must emit exactly three Boolean TRUE envelopes"
    );
    let mut three_false_envelopes = boolean_orv1_envelope(Some(false));
    three_false_envelopes.extend(boolean_orv1_envelope(Some(false)));
    three_false_envelopes.extend(boolean_orv1_envelope(Some(false)));
    let final_added = machine
        .run_as_orna(&["raw-call", added_read_entries])
        .expect("run final added read");
    let final_added = require_value_success("orna raw-call read_entries final", final_added)
        .expect("final added read must succeed");
    assert_eq!(
        final_added.stdout.as_slice(),
        three_false_envelopes.as_slice(),
        "the added reader must emit exactly three Boolean FALSE envelopes"
    );
}

/// Installed public-boundary journey for one required unique reference field.
///
/// The test installs the exact checked-in `product_test_unique_reference.orna`
/// fixture and applies it. It requires exactly three sorted qualified-name
/// mappings with pairwise distinct function identities, proves that only
/// `create_assignment` declares the `p_owner` reference parameter, and proves
/// every raw call is denied before any grant, including `create_assignment`
/// with a syntactically valid synthetic owner reference.
///
/// After granting the three exact functions, `create_owner` returns two
/// distinct real owner references with one nonzero target type. A
/// syntactically valid owner reference to a deterministically missing nonzero
/// object identity closes as the exact public `INTERNAL_FAILURE` line before
/// any assignment exists, and the public reader proves zero assignment rows.
/// Inserting an assignment for owner A returns a real assignment reference
/// with a different target type; retrying owner A fails with the exact public
/// `INTERNAL_FAILURE` line, and the public reference reader returns exactly
/// owner A. Inserting owner B succeeds, and the reader returns the unordered
/// multiset {A, B}. Reapplying the exact same source keeps the complete
/// function and parameter discovery vector and both rows without any repeated
/// grant. A restart preserves both rows. Retrying owner A through the
/// original identities and the retained grant fails again with the exact
/// `INTERNAL_FAILURE` line, and the reader still returns exactly {A, B}.
/// Repeating the same missing-object call after exact replay and again
/// after restart fails identically through the retained grant, and the
/// reader still returns exactly {A, B}.
///
/// The test claims only public uniqueness, rollback, missing-target
/// rejection across replay and restart, identity and grant retention,
/// replay, restart, and persistence through the packaged
/// `/usr/bin/orna` commands and raw-call ORV envelopes. It makes no claim
/// about private SQLSTATEs, constraint names, private audit records, the
/// private conflict type, physical storage, or row ordering.
#[test]
#[ignore = "requires Docker, ORNA_SYSTEM_TEST_DEBIAN_PACKAGE, and the installed orna executable"]
fn installed_unique_reference_insert_rolls_back_and_persists_across_replay_and_restart() {
    let package = std::env::var("ORNA_SYSTEM_TEST_DEBIAN_PACKAGE")
        .expect("ORNA_SYSTEM_TEST_DEBIAN_PACKAGE must point at the reproduced .deb package");
    let artifact = FrozenPackageArtifact::new(PackageFormat::Debian, &package)
        .expect("freeze the reproduced Debian package");
    let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("product_test_unique_reference.orna");
    let fixture = fs::read(&fixture_path).expect("read the checked-in unique reference fixture");

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
        vec![
            "unique_reference_test".to_string(),
            "create_assignment".to_string(),
        ],
        vec![
            "unique_reference_test".to_string(),
            "create_owner".to_string(),
        ],
        vec![
            "unique_reference_test".to_string(),
            "read_assignment_owners".to_string(),
        ],
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
    let create_owner = document
        .function_id(&["unique_reference_test", "create_owner"])
        .expect("apply must report create_owner");
    let create_assignment = document
        .function_id(&["unique_reference_test", "create_assignment"])
        .expect("apply must report create_assignment");
    let read_assignment_owners = document
        .function_id(&["unique_reference_test", "read_assignment_owners"])
        .expect("apply must report read_assignment_owners");
    let identities = [create_owner, create_assignment, read_assignment_owners];
    for (index, left) in identities.iter().enumerate() {
        for right in &identities[index + 1..] {
            assert_ne!(
                left, right,
                "the three function identities must be pairwise distinct"
            );
        }
    }
    let owner_parameter = document
        .parameter_id(&["unique_reference_test", "create_assignment"], "p_owner")
        .expect("apply must report create_assignment.p_owner");
    for name in ["create_owner", "read_assignment_owners"] {
        let entry = document
            .functions
            .iter()
            .find(|entry| {
                entry
                    .names()
                    .iter()
                    .map(String::as_str)
                    .eq(["unique_reference_test", name].iter().copied())
            })
            .expect("apply must report the function entry");
        assert!(
            entry.parameters().is_empty(),
            "{name} must declare no parameters"
        );
    }
    let assignment_entry = document
        .functions
        .iter()
        .find(|entry| {
            entry.names().iter().map(String::as_str).eq([
                "unique_reference_test",
                "create_assignment",
            ]
            .iter()
            .copied())
        })
        .expect("apply must report create_assignment");
    assert_eq!(
        assignment_entry.parameters().len(),
        1,
        "create_assignment must declare exactly one parameter"
    );
    let declared = &assignment_entry.parameters()[0];
    assert_eq!(
        declared.name(),
        "p_owner",
        "create_assignment must declare exactly the p_owner parameter"
    );
    assert_eq!(
        declared.parameter_id(),
        owner_parameter,
        "the declared parameter must equal the discovered identity"
    );

    // Source apply grants nothing: every raw call is denied before any grant.
    // The assignment denial carries a syntactically valid synthetic owner
    // reference, proving authorisation precedes argument and row validation.
    let pre_grant_owner = reference_orv1_envelope([0x11; 16], [0x22; 16]);
    let denied_assignment = machine
        .run_as_orna_with_stdin(
            &["raw-call", create_assignment, owner_parameter],
            &pre_grant_owner,
        )
        .expect("run denied assignment call");
    assert_denied("assignment before grant", denied_assignment)
        .expect("create_assignment must be denied before its grant");
    for function in [create_owner, read_assignment_owners] {
        let denied = machine
            .run_as_orna(&["raw-call", function])
            .expect("run denied raw call");
        assert_denied("raw call before grant", denied).expect("raw call must be denied");
    }

    // Grant the three exact functions.
    for function in identities {
        let granted = machine
            .run_as_orna(&["security", "grant-execute", function])
            .expect("run installed grant command");
        require_silent_success("orna security grant-execute", granted)
            .expect("grant must succeed silently");
    }

    // Create two distinct owners and retain their exact ORV1 references.
    let owner_a_call = machine
        .run_as_orna(&["raw-call", create_owner])
        .expect("run owner A create call");
    let owner_a_call = require_value_success("orna raw-call create_owner A", owner_a_call)
        .expect("owner A create must succeed");
    let owner_a = parse_reference_envelope(&owner_a_call.stdout)
        .expect("owner A create must return one ORV reference");
    let owner_b_call = machine
        .run_as_orna(&["raw-call", create_owner])
        .expect("run owner B create call");
    let owner_b_call = require_value_success("orna raw-call create_owner B", owner_b_call)
        .expect("owner B create must succeed");
    let owner_b = parse_reference_envelope(&owner_b_call.stdout)
        .expect("owner B create must return one ORV reference");
    assert!(
        owner_a.type_id != [0; 16] && owner_b.type_id != [0; 16],
        "both owners must name a real nonzero target type"
    );
    assert!(
        !owner_a.object_is_zero() && !owner_b.object_is_zero(),
        "both owners must name real nonzero rows"
    );
    assert_eq!(
        owner_a.type_id, owner_b.type_id,
        "both owners must share one target type"
    );
    assert_ne!(
        owner_a.object, owner_b.object,
        "the two owners must be distinct objects"
    );

    // A missing-referenced-object proof: one syntactically valid owner
    // reference to a deterministically missing nonzero object identity must
    // close as a public INTERNAL_FAILURE and roll back before any assignment
    // exists. Only the two observed owner objects exist in this isolated
    // machine, so a fixed candidate that differs from both is guaranteed to
    // name no row.
    let missing_object = [[0xaa; 16], [0xbb; 16], [0xcc; 16], [0xdd; 16]]
        .into_iter()
        .find(|candidate| *candidate != owner_a.object && *candidate != owner_b.object)
        .expect("a fixed candidate must differ from both observed owner object ids");
    let missing_selector = reference_orv1_envelope(owner_a.type_id, missing_object);
    let missing = machine
        .run_as_orna_with_stdin(
            &["raw-call", create_assignment, owner_parameter],
            &missing_selector,
        )
        .expect("run missing-owner assignment call");
    assert_exact_raw_call_failure(
        "orna raw-call create_assignment missing owner",
        missing,
        "raw call failed: INTERNAL_FAILURE\n",
    )
    .expect("the missing-owner assignment must close as a public INTERNAL_FAILURE");
    let empty_after_missing = machine
        .run_as_orna(&["raw-call", read_assignment_owners])
        .expect("run assignment reader after missing owner");
    require_silent_success(
        "orna raw-call read_assignment_owners after missing owner",
        empty_after_missing,
    )
    .expect("the missing-owner assignment must leave zero assignment rows");

    // Insert an assignment for owner A: the created assignment reference has
    // a different target type and a nonzero object identity.
    let owner_a_selector = reference_orv1_envelope(owner_a.type_id, owner_a.object);
    let assignment_a_call = machine
        .run_as_orna_with_stdin(
            &["raw-call", create_assignment, owner_parameter],
            &owner_a_selector,
        )
        .expect("run assignment for owner A");
    let assignment_a_call =
        require_value_success("orna raw-call create_assignment A", assignment_a_call)
            .expect("assignment A must succeed");
    let assignment_a = parse_reference_envelope(&assignment_a_call.stdout)
        .expect("assignment A must return one ORV reference");
    assert!(
        assignment_a.type_id != [0; 16] && !assignment_a.object_is_zero(),
        "assignment A must name a real nonzero assignment row"
    );
    assert_ne!(
        assignment_a.type_id, owner_a.type_id,
        "the assignment reference must use a different target type from the owner"
    );

    // Retrying owner A fails with the exact public INTERNAL_FAILURE line, and
    // the public reference reader returns exactly owner A.
    let duplicate_a = machine
        .run_as_orna_with_stdin(
            &["raw-call", create_assignment, owner_parameter],
            &owner_a_selector,
        )
        .expect("run duplicate assignment for owner A");
    assert_exact_raw_call_failure(
        "orna raw-call create_assignment duplicate A",
        duplicate_a,
        "raw call failed: INTERNAL_FAILURE\n",
    )
    .expect("the duplicate assignment must close as a public INTERNAL_FAILURE");
    assert_reference_reader_returns(
        &machine,
        read_assignment_owners,
        &[&owner_a],
        "orna raw-call read_assignment_owners after duplicate A",
    )
    .expect("the reader must return exactly owner A after the duplicate");

    // Inserting owner B succeeds, and the reader returns {A, B} without any
    // row-order reliance.
    let owner_b_selector = reference_orv1_envelope(owner_b.type_id, owner_b.object);
    let assignment_b_call = machine
        .run_as_orna_with_stdin(
            &["raw-call", create_assignment, owner_parameter],
            &owner_b_selector,
        )
        .expect("run assignment for owner B");
    let assignment_b_call =
        require_value_success("orna raw-call create_assignment B", assignment_b_call)
            .expect("assignment B must succeed");
    let assignment_b = parse_reference_envelope(&assignment_b_call.stdout)
        .expect("assignment B must return one ORV reference");
    assert!(
        assignment_b.type_id != [0; 16] && !assignment_b.object_is_zero(),
        "assignment B must name a real nonzero assignment row"
    );
    assert_eq!(
        assignment_b.type_id, assignment_a.type_id,
        "both assignments must share the assignment target type"
    );
    assert_ne!(
        assignment_b.object, assignment_a.object,
        "the two assignments must be distinct objects"
    );
    assert_reference_reader_returns(
        &machine,
        read_assignment_owners,
        &[&owner_a, &owner_b],
        "orna raw-call read_assignment_owners two owners",
    )
    .expect("the reader must return exactly the unordered owner multiset A and B");

    // Exact source replay keeps the complete function and parameter discovery
    // vector and both rows without any repeated grant.
    let replay = machine
        .run_as_orna(&["source", "apply", FIXTURE_PATH])
        .expect("run installed source apply on the same fixture");
    let replay = require_success("orna source apply replay", replay)
        .expect("unique reference replay must succeed");
    assert!(
        replay.stderr.is_empty(),
        "unique reference replay must keep standard error empty"
    );
    let replay_document =
        parse_apply_document(&replay.stdout).expect("unique reference replay JSON must parse");
    assert_eq!(
        replay_document.functions, document.functions,
        "the replay must keep the complete function and parameter discovery vector"
    );

    // The same missing-object call fails identically immediately after the
    // replay, before any restart, proving the rejection survives exact
    // source replay and its rollback keeps both rows.
    let missing_after_replay = machine
        .run_as_orna_with_stdin(
            &["raw-call", create_assignment, owner_parameter],
            &missing_selector,
        )
        .expect("run missing-owner assignment call after replay");
    assert_exact_raw_call_failure(
        "orna raw-call create_assignment missing owner after replay",
        missing_after_replay,
        "raw call failed: INTERNAL_FAILURE\n",
    )
    .expect("the post-replay missing-owner assignment must close as a public INTERNAL_FAILURE");
    assert_reference_reader_returns(
        &machine,
        read_assignment_owners,
        &[&owner_a, &owner_b],
        "orna raw-call read_assignment_owners after replay and missing owner",
    )
    .expect("the post-replay missing-owner call must preserve exactly the owners A and B");

    // A restart keeps grants and both rows.
    machine
        .restart_server()
        .expect("installed server must restart cleanly");
    assert_reference_reader_returns(
        &machine,
        read_assignment_owners,
        &[&owner_a, &owner_b],
        "orna raw-call read_assignment_owners after restart",
    )
    .expect("the restart must preserve both assignment rows");

    // Retrying owner A through the original identities and the retained grant
    // fails again with the exact public INTERNAL_FAILURE line, and the reader
    // still returns exactly {A, B}.
    let duplicate_a_after_restart = machine
        .run_as_orna_with_stdin(
            &["raw-call", create_assignment, owner_parameter],
            &owner_a_selector,
        )
        .expect("run duplicate assignment for owner A after restart");
    assert_exact_raw_call_failure(
        "orna raw-call create_assignment duplicate A after restart",
        duplicate_a_after_restart,
        "raw call failed: INTERNAL_FAILURE\n",
    )
    .expect("the post-restart duplicate must close as a public INTERNAL_FAILURE");

    // Repeating the same missing-object call after restart fails identically
    // through the original identities and the retained grant.
    let missing_after_restart = machine
        .run_as_orna_with_stdin(
            &["raw-call", create_assignment, owner_parameter],
            &missing_selector,
        )
        .expect("run missing-owner assignment call after restart");
    assert_exact_raw_call_failure(
        "orna raw-call create_assignment missing owner after restart",
        missing_after_restart,
        "raw call failed: INTERNAL_FAILURE\n",
    )
    .expect("the post-restart missing-owner assignment must close as a public INTERNAL_FAILURE");
    assert_reference_reader_returns(
        &machine,
        read_assignment_owners,
        &[&owner_a, &owner_b],
        "orna raw-call read_assignment_owners after restart and missing owner",
    )
    .expect("the post-restart missing-owner call must preserve exactly the owners A and B");
}

/// Prove ADR 0051 through only the installed product's public command path.
///
/// The journey creates nullable and required unique Text values, verifies that
/// byte-distinct values remain separate, and requires duplicate writes to
/// close as the existing public `INTERNAL_FAILURE`. It then proves replay,
/// semantic rename, and restart retain the public function and parameter
/// identities, grants, and rows without inspecting private database state.
#[test]
#[ignore = "requires Docker, ORNA_SYSTEM_TEST_DEBIAN_PACKAGE, and unique Text fields in the installed orna executable"]
fn installed_unique_text_fields_reject_duplicates_and_persist_across_replay_rename_and_restart() {
    let package = std::env::var("ORNA_SYSTEM_TEST_DEBIAN_PACKAGE")
        .expect("ORNA_SYSTEM_TEST_DEBIAN_PACKAGE must point at the reproduced .deb package");
    let artifact = FrozenPackageArtifact::new(PackageFormat::Debian, &package)
        .expect("freeze the reproduced Debian package");
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let original = fs::read(fixtures.join("product_test_unique_text.orna"))
        .expect("read the checked-in unique Text fixture");
    let renamed = fs::read(fixtures.join("product_test_unique_text_renamed.orna"))
        .expect("read the checked-in renamed unique Text fixture");
    let machine = InstalledMachine::start(&artifact, &original)
        .expect("start the installed unique Text test machine");

    let apply = machine
        .run_as_orna(&["source", "apply", FIXTURE_PATH])
        .expect("apply the installed unique Text fixture");
    let apply = require_success("orna source apply", apply).expect("source apply must succeed");
    assert!(
        apply.stderr.is_empty(),
        "source apply must keep standard error empty"
    );
    let document = parse_apply_document(&apply.stdout).expect("source apply JSON must parse");
    let expected_names = [
        vec!["unique_text_test".to_string(), "create_account".to_string()],
        vec!["unique_text_test".to_string(), "read_accounts".to_string()],
        vec![
            "unique_text_test".to_string(),
            "update_account_email".to_string(),
        ],
        vec![
            "unique_text_test".to_string(),
            "update_account_username".to_string(),
        ],
    ];
    assert_eq!(
        document
            .functions
            .iter()
            .map(|entry| entry.names().to_vec())
            .collect::<Vec<_>>(),
        expected_names,
        "apply must report the four unique Text functions in canonical name order"
    );
    let create = document
        .function_id(&["unique_text_test", "create_account"])
        .expect("apply must report create_account");
    let read = document
        .function_id(&["unique_text_test", "read_accounts"])
        .expect("apply must report read_accounts");
    let update_email = document
        .function_id(&["unique_text_test", "update_account_email"])
        .expect("apply must report update_account_email");
    let update_username = document
        .function_id(&["unique_text_test", "update_account_username"])
        .expect("apply must report update_account_username");
    let p_create_email = document
        .parameter_id(&["unique_text_test", "create_account"], "p_email")
        .expect("apply must report create_account.p_email");
    let p_create_username = document
        .parameter_id(&["unique_text_test", "create_account"], "p_username")
        .expect("apply must report create_account.p_username");
    let p_update_email = document
        .parameter_id(&["unique_text_test", "update_account_email"], "p_email")
        .expect("apply must report update_account_email.p_email");
    let p_update_email_account = document
        .parameter_id(&["unique_text_test", "update_account_email"], "p_account")
        .expect("apply must report update_account_email.p_account");
    let p_update_username = document
        .parameter_id(
            &["unique_text_test", "update_account_username"],
            "p_username",
        )
        .expect("apply must report update_account_username.p_username");
    let p_update_username_account = document
        .parameter_id(
            &["unique_text_test", "update_account_username"],
            "p_account",
        )
        .expect("apply must report update_account_username.p_account");
    for (function, expected) in [
        (create, ["p_email", "p_username"].as_slice()),
        (read, [].as_slice()),
        (update_email, ["p_email", "p_account"].as_slice()),
        (update_username, ["p_username", "p_account"].as_slice()),
    ] {
        let entry = document
            .functions
            .iter()
            .find(|entry| entry.function_id() == function)
            .expect("apply must retain discovered function entry");
        assert_eq!(
            entry
                .parameters()
                .iter()
                .map(ParameterEntry::name)
                .collect::<Vec<_>>(),
            expected,
            "function must retain its exact ordered parameter declaration"
        );
    }
    let parameter_ids = [
        p_create_email,
        p_create_username,
        p_update_email,
        p_update_email_account,
        p_update_username,
        p_update_username_account,
    ];
    for (index, left) in parameter_ids.iter().enumerate() {
        for right in &parameter_ids[index + 1..] {
            assert_ne!(left, right, "every discovered ParameterId must be distinct");
        }
    }

    let synthetic_account = reference_orv1_envelope([0x41; 16], [0x42; 16]);
    for (command, input) in [
        (
            vec!["raw-call", create, p_create_email, p_create_username],
            [
                nullable_text_orv1_envelope(None),
                text_orv1_envelope("denied"),
            ]
            .concat(),
        ),
        (vec!["raw-call", read], Vec::new()),
        (
            vec![
                "raw-call",
                update_email,
                p_update_email,
                p_update_email_account,
            ],
            [text_orv1_envelope("denied"), synthetic_account.clone()].concat(),
        ),
        (
            vec![
                "raw-call",
                update_username,
                p_update_username,
                p_update_username_account,
            ],
            [text_orv1_envelope("denied"), synthetic_account.clone()].concat(),
        ),
    ] {
        let denied = if input.is_empty() {
            machine.run_as_orna(&command)
        } else {
            machine.run_as_orna_with_stdin(&command, &input)
        }
        .expect("run denied unique Text raw call");
        assert_denied("unique Text raw call before grant", denied)
            .expect("authorisation must precede target and value inspection");
    }
    for function in [create, read, update_email, update_username] {
        require_silent_success(
            "orna security grant-execute",
            machine
                .run_as_orna(&["security", "grant-execute", function])
                .expect("grant unique Text function"),
        )
        .expect("grant must succeed silently");
    }

    let create_account = |email: Option<&str>, username: &str, label: &'static str| {
        let input = [
            nullable_text_orv1_envelope(email),
            text_orv1_envelope(username),
        ]
        .concat();
        let output = machine
            .run_as_orna_with_stdin(
                &["raw-call", create, p_create_email, p_create_username],
                &input,
            )
            .expect("create unique Text account");
        parse_reference_envelope(
            &require_value_success(label, output)
                .expect("unique Text account creation must succeed")
                .stdout,
        )
        .expect("account creation must return one canonical Reference")
    };
    let update = |function: &str,
                  value_parameter: &str,
                  account_parameter: &str,
                  value: Option<&str>,
                  account: &OrvReference,
                  label: &'static str| {
        let input = [
            nullable_text_orv1_envelope(value),
            reference_orv1_envelope(account.type_id, account.object),
        ]
        .concat();
        let output = machine
            .run_as_orna_with_stdin(
                &["raw-call", function, value_parameter, account_parameter],
                &input,
            )
            .expect("update selected unique Text account");
        let output = require_value_success(label, output).expect("selected update must succeed");
        assert_eq!(
            output.stdout,
            reference_orv1_envelope(account.type_id, account.object),
            "successful selected update must return its exact account reference"
        );
    };
    let read_accounts = |label: &'static str| {
        let output = machine
            .run_as_orna(&["raw-call", read])
            .expect("read unique Text accounts");
        let output = require_value_success(label, output).expect("account reader must succeed");
        decode_unique_text_accounts(&output.stdout).expect(
            "account reader must emit complete canonical Reference, nullable Text, and Text rows",
        )
    };

    let nullable_a = create_account(None, "nullable-a", "create nullable account A");
    let nullable_b = create_account(None, "nullable-b", "create nullable account B");
    let baseline = create_account(
        Some("exact@example.test"),
        "baseline",
        "create exact baseline account",
    );
    let variants = [
        ("EXACT@example.test", "case"),
        (" exact@example.test ", "whitespace"),
        ("line\nending@example.test", "line-feed"),
        ("line\r\nending@example.test", "carriage-return-line-feed"),
        ("caf\u{e9}@example.test", "nfc"),
        ("cafe\u{301}@example.test", "nfd"),
        ("", "empty"),
    ];
    let variant_accounts = variants
        .iter()
        .map(|(email, username)| {
            create_account(Some(email), username, "create byte-distinct account")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        read_accounts("read initial unique Text accounts").len(),
        10,
        "two nullable NULLs and every byte-distinct value must store independently"
    );

    let duplicate_insert = machine
        .run_as_orna_with_stdin(
            &["raw-call", create, p_create_email, p_create_username],
            &[
                text_orv1_envelope("new@example.test"),
                text_orv1_envelope("baseline"),
            ]
            .concat(),
        )
        .expect("attempt duplicate required Text insert");
    assert_exact_raw_call_failure(
        "orna raw-call create_account duplicate required Text",
        duplicate_insert,
        "raw call failed: INTERNAL_FAILURE\n",
    )
    .expect("duplicate required Text insert must remain publicly redacted");
    assert_eq!(
        read_accounts("read after duplicate required insert").len(),
        10,
        "failed insert must roll back"
    );

    update(
        update_email,
        p_update_email,
        p_update_email_account,
        Some("updated@example.test"),
        &nullable_a,
        "update selected nullable account",
    );
    let duplicate_email_update = machine
        .run_as_orna_with_stdin(
            &[
                "raw-call",
                update_email,
                p_update_email,
                p_update_email_account,
            ],
            &[
                text_orv1_envelope("exact@example.test"),
                reference_orv1_envelope(nullable_b.type_id, nullable_b.object),
            ]
            .concat(),
        )
        .expect("attempt duplicate nullable Text update");
    assert_exact_raw_call_failure(
        "orna raw-call update_account_email duplicate nullable Text",
        duplicate_email_update,
        "raw call failed: INTERNAL_FAILURE\n",
    )
    .expect("duplicate nullable Text update must remain publicly redacted");
    let duplicate_username_update = machine
        .run_as_orna_with_stdin(
            &[
                "raw-call",
                update_username,
                p_update_username,
                p_update_username_account,
            ],
            &[
                text_orv1_envelope("baseline"),
                reference_orv1_envelope(variant_accounts[0].type_id, variant_accounts[0].object),
            ]
            .concat(),
        )
        .expect("attempt duplicate required Text update");
    assert_exact_raw_call_failure(
        "orna raw-call update_account_username duplicate required Text",
        duplicate_username_update,
        "raw call failed: INTERNAL_FAILURE\n",
    )
    .expect("duplicate required Text update must remain publicly redacted");
    update(
        update_email,
        p_update_email,
        p_update_email_account,
        Some("exact@example.test"),
        &baseline,
        "self-update exact unique Text",
    );
    assert_unique_text_public_rows(
        &read_accounts("read after unique Text failures"),
        &nullable_a,
        &nullable_b,
        &baseline,
        &variant_accounts,
        "case",
    );

    let replay = require_success(
        "orna source apply exact unique Text replay",
        machine
            .run_as_orna(&["source", "apply", FIXTURE_PATH])
            .expect("replay unique Text source"),
    )
    .expect("exact unique Text replay must succeed");
    assert!(
        replay.stderr.is_empty(),
        "replay must keep standard error empty"
    );
    assert_eq!(
        parse_apply_document(&replay.stdout)
            .expect("replay JSON must parse")
            .functions,
        document.functions,
        "exact replay must retain every function and ParameterId without regrant"
    );

    machine
        .write_fixture(&renamed)
        .expect("replace with renamed unique Text fixture");
    let renamed_apply = require_success(
        "orna source apply renamed unique Text",
        machine
            .run_as_orna(&["source", "apply", FIXTURE_PATH])
            .expect("apply renamed unique Text source"),
    )
    .expect("semantic unique Text rename must succeed");
    let renamed_document =
        parse_apply_document(&renamed_apply.stdout).expect("renamed JSON must parse");
    assert_ne!(
        renamed_document.source_revision, document.source_revision,
        "rename must change source revision"
    );
    assert_ne!(
        renamed_document.catalogue_revision, document.catalogue_revision,
        "rename must change catalogue revision"
    );
    assert_eq!(
        renamed_document.functions, document.functions,
        "rename must retain all public function and ParameterId identities"
    );
    assert_unique_text_public_rows(
        &read_accounts("read rows after semantic rename"),
        &nullable_a,
        &nullable_b,
        &baseline,
        &variant_accounts,
        "case",
    );
    let duplicate_after_rename = machine
        .run_as_orna_with_stdin(
            &[
                "raw-call",
                update_email,
                p_update_email,
                p_update_email_account,
            ],
            &[
                text_orv1_envelope("exact@example.test"),
                reference_orv1_envelope(nullable_b.type_id, nullable_b.object),
            ]
            .concat(),
        )
        .expect("attempt duplicate nullable Text update after rename");
    assert_exact_raw_call_failure(
        "orna raw-call update_account_email duplicate after semantic rename",
        duplicate_after_rename,
        "raw call failed: INTERNAL_FAILURE\n",
    )
    .expect("renamed unique Text field must retain public duplicate redaction");
    assert_unique_text_public_rows(
        &read_accounts("read rows after renamed duplicate"),
        &nullable_a,
        &nullable_b,
        &baseline,
        &variant_accounts,
        "case",
    );

    machine
        .restart_server()
        .expect("restart installed unique Text server");
    update(
        update_username,
        p_update_username,
        p_update_username_account,
        Some("case-after-restart"),
        &variant_accounts[0],
        "update required Text after restart",
    );
    assert_unique_text_public_rows(
        &read_accounts("read rows after restart"),
        &nullable_a,
        &nullable_b,
        &baseline,
        &variant_accounts,
        "case-after-restart",
    );
    let after_restart = create_account(
        Some("after-restart@example.test"),
        "after-restart",
        "create account through original identity after restart",
    );
    let final_rows = read_accounts("read original creator row after restart");
    assert_eq!(
        final_rows.len(),
        11,
        "the retained original create grant must add one account after restart"
    );
    let created = final_rows
        .iter()
        .find(|row| {
            row.account.type_id == after_restart.type_id
                && row.account.object == after_restart.object
        })
        .expect("public reader must return the account created after restart");
    assert_eq!(
        created.email.as_deref(),
        Some("after-restart@example.test"),
        "the original creator must retain its exact nullable Text binding"
    );
    assert_eq!(
        created.username, "after-restart",
        "the original creator must retain its exact required Text binding"
    );
}

/// Prove ADR 0052 through only the installed product's public command path.
///
/// The journey discovers the exact callable identities, proves authorisation
/// before target inspection, and creates nullable and required unique Text
/// rows. It then proves byte-exact selected reads, empty results, unavailable
/// targets, replay, semantic rename, and restart without private database
/// inspection or a regrant.
#[test]
#[ignore = "requires Docker, ORNA_SYSTEM_TEST_DEBIAN_PACKAGE, and unique Text selected SERVER SELECT in the installed orna executable"]
fn installed_unique_text_select_binds_exact_text_and_survives_replay_rename_and_restart() {
    let package = std::env::var("ORNA_SYSTEM_TEST_DEBIAN_PACKAGE")
        .expect("ORNA_SYSTEM_TEST_DEBIAN_PACKAGE must point at the reproduced .deb package");
    let artifact = FrozenPackageArtifact::new(PackageFormat::Debian, &package)
        .expect("freeze the reproduced Debian package");
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let original = fs::read(fixtures.join("product_test_unique_text_select.orna"))
        .expect("read the checked-in unique Text select fixture");
    let renamed = fs::read(fixtures.join("product_test_unique_text_select_renamed.orna"))
        .expect("read the checked-in renamed unique Text select fixture");
    let machine = InstalledMachine::start(&artifact, &original)
        .expect("start the installed unique Text select test machine");

    let apply = machine
        .run_as_orna(&["source", "apply", FIXTURE_PATH])
        .expect("apply the installed unique Text select fixture");
    let apply = require_success("orna source apply", apply).expect("source apply must succeed");
    assert!(
        apply.stderr.is_empty(),
        "source apply must keep standard error empty"
    );
    let document = parse_apply_document(&apply.stdout).expect("source apply JSON must parse");
    let expected_names = [
        vec![
            "unique_text_select_test".to_string(),
            "create_account".to_string(),
        ],
        vec![
            "unique_text_select_test".to_string(),
            "create_account_without_email".to_string(),
        ],
        vec![
            "unique_text_select_test".to_string(),
            "find_by_email".to_string(),
        ],
        vec![
            "unique_text_select_test".to_string(),
            "find_by_username".to_string(),
        ],
        vec![
            "unique_text_select_test".to_string(),
            "read_accounts".to_string(),
        ],
    ];
    assert_eq!(
        document
            .functions
            .iter()
            .map(|entry| entry.names().to_vec())
            .collect::<Vec<_>>(),
        expected_names,
        "apply must report the five unique Text select functions in canonical name order"
    );
    let create = document
        .function_id(&["unique_text_select_test", "create_account"])
        .expect("apply must report create_account");
    let create_without_email = document
        .function_id(&["unique_text_select_test", "create_account_without_email"])
        .expect("apply must report create_account_without_email");
    let find_by_email = document
        .function_id(&["unique_text_select_test", "find_by_email"])
        .expect("apply must report find_by_email");
    let find_by_username = document
        .function_id(&["unique_text_select_test", "find_by_username"])
        .expect("apply must report find_by_username");
    let read_accounts = document
        .function_id(&["unique_text_select_test", "read_accounts"])
        .expect("apply must report read_accounts");
    for (function, expected_parameters) in [
        (create, ["p_email", "p_username"].as_slice()),
        (create_without_email, ["p_username"].as_slice()),
        (find_by_email, ["p_email"].as_slice()),
        (find_by_username, ["p_username"].as_slice()),
        (read_accounts, ["p_account"].as_slice()),
    ] {
        let entry = document
            .functions
            .iter()
            .find(|entry| entry.function_id() == function)
            .expect("apply must retain every discovered function entry");
        assert_eq!(
            entry
                .parameters()
                .iter()
                .map(ParameterEntry::name)
                .collect::<Vec<_>>(),
            expected_parameters,
            "function must retain its exact ordered parameter declarations"
        );
    }
    let p_create_email = document
        .parameter_id(&["unique_text_select_test", "create_account"], "p_email")
        .expect("apply must report create_account.p_email");
    let p_create_username = document
        .parameter_id(&["unique_text_select_test", "create_account"], "p_username")
        .expect("apply must report create_account.p_username");
    let p_create_without_email_username = document
        .parameter_id(
            &["unique_text_select_test", "create_account_without_email"],
            "p_username",
        )
        .expect("apply must report create_account_without_email.p_username");
    let p_find_email = document
        .parameter_id(&["unique_text_select_test", "find_by_email"], "p_email")
        .expect("apply must report find_by_email.p_email");
    let p_find_username = document
        .parameter_id(
            &["unique_text_select_test", "find_by_username"],
            "p_username",
        )
        .expect("apply must report find_by_username.p_username");
    let p_read_account = document
        .parameter_id(&["unique_text_select_test", "read_accounts"], "p_account")
        .expect("apply must report read_accounts.p_account");
    let parameter_ids = [
        p_create_email,
        p_create_username,
        p_create_without_email_username,
        p_find_email,
        p_find_username,
        p_read_account,
    ];
    for (index, left) in parameter_ids.iter().enumerate() {
        for right in &parameter_ids[index + 1..] {
            assert_ne!(left, right, "every discovered ParameterId must be distinct");
        }
    }

    // Every public function denies before a grant. The finder inputs are
    // well-formed Text envelopes, but the denied results disclose no target
    // or selected-row information.
    for (command, input) in [
        (
            vec!["raw-call", create, p_create_email, p_create_username],
            [
                text_orv1_envelope("denied@example.test"),
                text_orv1_envelope("denied"),
            ]
            .concat(),
        ),
        (
            vec![
                "raw-call",
                create_without_email,
                p_create_without_email_username,
            ],
            text_orv1_envelope("denied-without-email"),
        ),
        (
            vec!["raw-call", find_by_email, p_find_email],
            text_orv1_envelope("denied@example.test"),
        ),
        (
            vec!["raw-call", find_by_username, p_find_username],
            text_orv1_envelope("denied"),
        ),
        (
            vec!["raw-call", read_accounts, p_read_account],
            reference_orv1_envelope([0xa5; 16], [0x5a; 16]),
        ),
    ] {
        let denied = if input.is_empty() {
            machine.run_as_orna(&command)
        } else {
            machine.run_as_orna_with_stdin(&command, &input)
        }
        .expect("run denied unique Text select raw call");
        assert_denied("unique Text select raw call before grant", denied)
            .expect("authorisation must precede target and value inspection");
    }
    for function in [
        create,
        create_without_email,
        find_by_email,
        find_by_username,
        read_accounts,
    ] {
        require_silent_success(
            "orna security grant-execute",
            machine
                .run_as_orna(&["security", "grant-execute", function])
                .expect("grant unique Text select function"),
        )
        .expect("grant must succeed silently");
    }

    let create_account = |email: &str, username: &str, label: &'static str| {
        let output = machine
            .run_as_orna_with_stdin(
                &["raw-call", create, p_create_email, p_create_username],
                &[text_orv1_envelope(email), text_orv1_envelope(username)].concat(),
            )
            .expect("create unique Text select account");
        parse_reference_envelope(
            &require_value_success(label, output)
                .expect("unique Text select account creation must succeed")
                .stdout,
        )
        .expect("account creation must return one canonical Reference")
    };
    let create_without_email_account = |username: &str, label: &'static str| {
        let output = machine
            .run_as_orna_with_stdin(
                &[
                    "raw-call",
                    create_without_email,
                    p_create_without_email_username,
                ],
                &text_orv1_envelope(username),
            )
            .expect("create unique Text select account without email");
        parse_reference_envelope(
            &require_value_success(label, output)
                .expect("nullable unique Text account creation must succeed")
                .stdout,
        )
        .expect("nullable account creation must return one canonical Reference")
    };
    let select_by_email = |email: &str, label: &'static str| {
        let output = machine
            .run_as_orna_with_stdin(
                &["raw-call", find_by_email, p_find_email],
                &text_orv1_envelope(email),
            )
            .expect("run unique Text email selector");
        let output = require_value_success(label, output).expect("email selector must succeed");
        if output.stdout.is_empty() {
            None
        } else {
            Some(
                decode_unique_text_email_selection(&output.stdout)
                    .expect("email selector must emit one strict Reference and Text result"),
            )
        }
    };
    let select_by_username = |username: &str, label: &'static str| {
        let output = machine
            .run_as_orna_with_stdin(
                &["raw-call", find_by_username, p_find_username],
                &text_orv1_envelope(username),
            )
            .expect("run unique Text username selector");
        let output = require_value_success(label, output).expect("username selector must succeed");
        if output.stdout.is_empty() {
            None
        } else {
            Some(
                decode_unique_text_username_selection(&output.stdout).expect(
                    "username selector must emit one strict Reference and nullable Text result",
                ),
            )
        }
    };
    let read_account = |account: &OrvReference, label: &'static str| {
        let output = machine
            .run_as_orna_with_stdin(
                &["raw-call", read_accounts, p_read_account],
                &reference_orv1_envelope(account.type_id, account.object),
            )
            .expect("run unique Text identity reader");
        let output = require_value_success(label, output).expect("identity reader must succeed");
        let rows = decode_unique_text_accounts(&output.stdout)
            .expect("identity reader must emit one complete strict account row");
        assert_eq!(
            rows.len(),
            1,
            "identity reader must return exactly the supplied account row"
        );
        rows.into_iter()
            .next()
            .expect("one identity-selected account row must exist")
    };

    let nullable_a = create_without_email_account("nullable-a", "create nullable account A");
    let nullable_b = create_without_email_account("nullable-b", "create nullable account B");
    assert_ne!(
        nullable_a.object, nullable_b.object,
        "the two nullable rows must have distinct object identities"
    );
    assert_eq!(
        nullable_a.type_id, nullable_b.type_id,
        "the two nullable rows must share one account type"
    );

    // A non-null empty Text selector cannot match either nullable email. It
    // remains empty until a distinct empty Text value is stored.
    assert!(
        select_by_email("", "orna raw-call find_by_email empty before empty value").is_none(),
        "nullable NULL email values must not match an empty Text selector"
    );
    let baseline = create_account(
        "exact@example.test",
        "baseline",
        "create exact baseline account",
    );
    let variants = [
        ("EXACT@example.test", "case"),
        (" exact@example.test ", "whitespace"),
        ("line\nending@example.test", "line-feed"),
        ("line\r\nending@example.test", "carriage-return-line-feed"),
        ("caf\u{e9}@example.test", "nfc"),
        ("cafe\u{301}@example.test", "nfd"),
        ("", "empty"),
    ];
    let variant_accounts = variants
        .iter()
        .map(|(email, username)| create_account(email, username, "create byte-distinct account"))
        .collect::<Vec<_>>();
    assert_eq!(
        baseline.type_id, nullable_a.type_id,
        "required and nullable accounts must share one object type"
    );

    // Each selector must use the exact bytes. Case, whitespace, line ending,
    // and Unicode-normalisation variants are independent stored values.
    let selected_baseline = select_by_email(
        "exact@example.test",
        "orna raw-call find_by_email exact baseline",
    )
    .expect("the exact Text selector must find its one stored row");
    assert_unique_text_email_selection(&selected_baseline, &baseline, "baseline");
    for ((email, username), account) in variants.iter().zip(&variant_accounts) {
        let selected = select_by_email(email, "orna raw-call find_by_email byte-distinct")
            .expect("each exact byte-distinct Text selector must find one row");
        assert_unique_text_email_selection(&selected, account, username);
    }
    assert_ne!(
        variant_accounts[0].object, baseline.object,
        "case-distinct Text must retain a distinct selected object"
    );
    assert_ne!(
        variant_accounts[5].object, variant_accounts[4].object,
        "canonically equivalent but byte-distinct Text must retain distinct selected objects"
    );

    let selected_empty = select_by_email("", "orna raw-call find_by_email stored empty")
        .expect("the stored empty Text must select one row");
    assert_unique_text_email_selection(&selected_empty, &variant_accounts[6], "empty");
    assert_ne!(
        selected_empty.account.object, nullable_a.object,
        "the empty Text selector must not select the first nullable NULL row"
    );
    assert_ne!(
        selected_empty.account.object, nullable_b.object,
        "the empty Text selector must not select the second nullable NULL row"
    );
    assert!(
        select_by_email("absent@example.test", "orna raw-call find_by_email absent").is_none(),
        "an absent Text selector must complete without values"
    );
    assert!(
        select_by_username("absent", "orna raw-call find_by_username absent").is_none(),
        "an absent required Text selector must complete without values"
    );

    let selected_username =
        select_by_username("baseline", "orna raw-call find_by_username baseline")
            .expect("the exact username selector must find its one stored row");
    assert_unique_text_username_selection(
        &selected_username,
        &baseline,
        Some("exact@example.test"),
    );
    let selected_nullable = select_by_username(
        "nullable-a",
        "orna raw-call find_by_username nullable account",
    )
    .expect("the username selector must find a nullable email row");
    assert_unique_text_username_selection(&selected_nullable, &nullable_a, None);

    // The granted finder with no canonical parameter has no usable target.
    let unavailable = machine
        .run_as_orna(&["raw-call", find_by_email])
        .expect("run unavailable unique Text selector");
    assert_target_unavailable("unique Text selector without parameter", unavailable)
        .expect("the malformed allowed target must close as TARGET_UNAVAILABLE");

    for expected in [
        &nullable_a,
        &nullable_b,
        &baseline,
        &variant_accounts[0],
        &variant_accounts[1],
        &variant_accounts[2],
        &variant_accounts[3],
        &variant_accounts[4],
        &variant_accounts[5],
        &variant_accounts[6],
    ] {
        let row = read_account(expected, "orna raw-call read_accounts by identity");
        assert_eq!(
            row.account.type_id, expected.type_id,
            "identity reader must retain the supplied account type identity"
        );
        assert_eq!(
            row.account.object, expected.object,
            "identity reader must retain the supplied account object identity"
        );
    }

    let absent_object = if baseline.object == [0xa5; 16] {
        [0x5a; 16]
    } else {
        [0xa5; 16]
    };
    let absent = machine
        .run_as_orna_with_stdin(
            &["raw-call", read_accounts, p_read_account],
            &reference_orv1_envelope(baseline.type_id, absent_object),
        )
        .expect("run absent unique Text identity reader");
    require_silent_success("orna raw-call read_accounts absent", absent)
        .expect("an absent same-type Reference must select no values");

    // Exact replay keeps all callable identities and grants. Reuse the
    // original selector identities without regranting.
    let replay = require_success(
        "orna source apply exact unique Text select replay",
        machine
            .run_as_orna(&["source", "apply", FIXTURE_PATH])
            .expect("replay unique Text select source"),
    )
    .expect("exact unique Text select replay must succeed");
    assert!(
        replay.stderr.is_empty(),
        "replay must keep standard error empty"
    );
    assert_eq!(
        parse_apply_document(&replay.stdout)
            .expect("replay JSON must parse")
            .functions,
        document.functions,
        "exact replay must retain every function and ParameterId without regrant"
    );
    let replayed = select_by_email(
        "exact@example.test",
        "orna raw-call find_by_email after exact replay",
    )
    .expect("the original finder grant must survive exact replay");
    assert_unique_text_email_selection(&replayed, &baseline, "baseline");
    let replayed_account =
        read_account(&baseline, "orna raw-call read_accounts after exact replay");
    assert_eq!(
        replayed_account.username, "baseline",
        "the original reader identity and grant must survive exact replay"
    );

    // The field rename changes source and catalogue revisions, but leaves the
    // discovered function and parameter identities, grants, and row identity
    // usable through the original raw-call command.
    machine
        .write_fixture(&renamed)
        .expect("replace with renamed unique Text select fixture");
    let renamed_apply = require_success(
        "orna source apply renamed unique Text select",
        machine
            .run_as_orna(&["source", "apply", FIXTURE_PATH])
            .expect("apply renamed unique Text select source"),
    )
    .expect("semantic unique Text select rename must succeed");
    assert!(
        renamed_apply.stderr.is_empty(),
        "renamed source apply must keep standard error empty"
    );
    let renamed_document =
        parse_apply_document(&renamed_apply.stdout).expect("renamed JSON must parse");
    assert_ne!(
        renamed_document.source_revision, document.source_revision,
        "semantic rename must change the source revision"
    );
    assert_ne!(
        renamed_document.catalogue_revision, document.catalogue_revision,
        "semantic rename must change the catalogue revision"
    );
    assert_eq!(
        renamed_document.functions, document.functions,
        "semantic rename must retain every function and ParameterId identity"
    );
    let renamed_selected = select_by_email(
        "exact@example.test",
        "orna raw-call find_by_email after semantic rename",
    )
    .expect("the original finder identity and grant must survive semantic rename");
    assert_unique_text_email_selection(&renamed_selected, &baseline, "baseline");
    let renamed_nullable = select_by_username(
        "nullable-b",
        "orna raw-call find_by_username nullable after semantic rename",
    )
    .expect("the renamed selector must retain the nullable row identity");
    assert_unique_text_username_selection(&renamed_nullable, &nullable_b, None);
    let renamed_account = read_account(
        &nullable_a,
        "orna raw-call read_accounts after semantic rename",
    );
    assert_eq!(
        renamed_account.email.as_deref(),
        None,
        "the renamed identity reader must retain nullable account values"
    );

    machine
        .restart_server()
        .expect("restart installed unique Text select server");
    let restarted_selected = select_by_email(
        "cafe\u{301}@example.test",
        "orna raw-call find_by_email after restart",
    )
    .expect("the original finder identity and grant must survive restart");
    assert_unique_text_email_selection(&restarted_selected, &variant_accounts[5], "nfd");
    let restarted_baseline =
        select_by_username("baseline", "orna raw-call find_by_username after restart")
            .expect("the original username selector identity and grant must survive restart");
    assert_unique_text_username_selection(
        &restarted_baseline,
        &baseline,
        Some("exact@example.test"),
    );
    let restarted_account = read_account(
        &variant_accounts[5],
        "orna raw-call read_accounts after restart",
    );
    assert_eq!(
        restarted_account.username, "nfd",
        "the original reader identity and grant must survive restart"
    );
}

/// One strict public result from `find_by_email`.
struct UniqueTextEmailSelection {
    account: OrvReference,
    username: String,
}

/// One strict public result from `find_by_username`.
struct UniqueTextUsernameSelection {
    account: OrvReference,
    email: Option<String>,
}

/// Decode the exact one-row `find_by_email` protocol result.
///
/// The result is exactly one canonical Reference envelope followed by one
/// canonical non-null Text envelope. It has no row wrapper or trailing bytes.
fn decode_unique_text_email_selection(bytes: &[u8]) -> Option<UniqueTextEmailSelection> {
    let account = parse_reference_envelope(bytes.get(..41)?).ok()?;
    let username = decode_unique_text_select_text(bytes.get(41..)?)?;
    Some(UniqueTextEmailSelection { account, username })
}

/// Decode the exact one-row `find_by_username` protocol result.
///
/// The result is exactly one canonical Reference envelope followed by one
/// canonical nullable Text envelope. It has no row wrapper or trailing bytes.
fn decode_unique_text_username_selection(bytes: &[u8]) -> Option<UniqueTextUsernameSelection> {
    let account = parse_reference_envelope(bytes.get(..41)?).ok()?;
    let email = decode_unique_text_select_nullable_text(bytes.get(41..)?)?;
    Some(UniqueTextUsernameSelection { account, email })
}

/// Decode one complete canonical non-null ORV1 Text envelope.
fn decode_unique_text_select_text(bytes: &[u8]) -> Option<String> {
    if bytes.len() < 25
        || &bytes[..4] != b"ORV1"
        || bytes[4] != 0x06
        || bytes[5..20] != [0; 15]
        || bytes[20] != 0x06
    {
        return None;
    }
    let length = u32::from_be_bytes(bytes[21..25].try_into().ok()?) as usize;
    let end = 25_usize.checked_add(length)?;
    if bytes.len() != end {
        return None;
    }
    String::from_utf8(bytes[25..end].to_vec()).ok()
}

/// Decode one complete canonical nullable ORV1 Text envelope.
fn decode_unique_text_select_nullable_text(bytes: &[u8]) -> Option<Option<String>> {
    if bytes.len() < 25 || &bytes[..4] != b"ORV1" || bytes[5..20] != [0; 15] || bytes[20] != 0x06 {
        return None;
    }
    let length = u32::from_be_bytes(bytes[21..25].try_into().ok()?) as usize;
    match bytes[4] {
        0x00 if length == 0 && bytes.len() == 25 => Some(None),
        0x06 => {
            let end = 25_usize.checked_add(length)?;
            if bytes.len() != end {
                return None;
            }
            String::from_utf8(bytes[25..end].to_vec()).ok().map(Some)
        }
        _ => None,
    }
}

/// Require the one public row selected by an exact email value.
fn assert_unique_text_email_selection(
    actual: &UniqueTextEmailSelection,
    expected_account: &OrvReference,
    expected_username: &str,
) {
    assert_eq!(
        actual.account.type_id, expected_account.type_id,
        "selected account must retain its object type identity"
    );
    assert_eq!(
        actual.account.object, expected_account.object,
        "selected account must retain its object identity"
    );
    assert_eq!(
        actual.username, expected_username,
        "email selector must retain its declared Text projection"
    );
}

/// Require the one public row selected by an exact username value.
fn assert_unique_text_username_selection(
    actual: &UniqueTextUsernameSelection,
    expected_account: &OrvReference,
    expected_email: Option<&str>,
) {
    assert_eq!(
        actual.account.type_id, expected_account.type_id,
        "selected account must retain its object type identity"
    );
    assert_eq!(
        actual.account.object, expected_account.object,
        "selected account must retain its object identity"
    );
    assert_eq!(
        actual.email.as_deref(),
        expected_email,
        "username selector must retain its declared nullable Text projection"
    );
}

/// One public account row from the unique Text installed journey.
struct UniqueTextAccountRow {
    account: OrvReference,
    email: Option<String>,
    username: String,
}

/// Decode rows returned by `unique_text_test.read_accounts`.
fn decode_unique_text_accounts(bytes: &[u8]) -> Option<Vec<UniqueTextAccountRow>> {
    let mut rows = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let account = parse_reference_envelope(bytes.get(offset..offset + 41)?).ok()?;
        offset += 41;
        let email_header = bytes.get(offset..offset + 25)?;
        if &email_header[..4] != b"ORV1"
            || email_header[5..20] != [0; 15]
            || email_header[20] != 0x06
        {
            return None;
        }
        let email_length = u32::from_be_bytes(email_header[21..25].try_into().ok()?) as usize;
        let email_end = offset.checked_add(25 + email_length)?;
        let email = match email_header[4] {
            0x00 if email_length == 0 => None,
            0x06 => Some(String::from_utf8(bytes.get(offset + 25..email_end)?.to_vec()).ok()?),
            _ => return None,
        };
        offset = email_end;
        let username_header = bytes.get(offset..offset + 25)?;
        if &username_header[..4] != b"ORV1"
            || username_header[4] != 0x06
            || username_header[5..20] != [0; 15]
            || username_header[20] != 0x06
        {
            return None;
        }
        let username_length = u32::from_be_bytes(username_header[21..25].try_into().ok()?) as usize;
        let username_end = offset.checked_add(25 + username_length)?;
        let username = String::from_utf8(bytes.get(offset + 25..username_end)?.to_vec()).ok()?;
        offset = username_end;
        rows.push(UniqueTextAccountRow {
            account,
            email,
            username,
        });
    }
    Some(rows)
}

/// Require the observable account set after an allowed or rejected mutation.
fn assert_unique_text_public_rows(
    rows: &[UniqueTextAccountRow],
    nullable_a: &OrvReference,
    nullable_b: &OrvReference,
    baseline: &OrvReference,
    variants: &[OrvReference],
    case_username: &str,
) {
    assert_eq!(
        rows.len(),
        10,
        "the public reader must retain every stored account"
    );
    let row = |account: &OrvReference| {
        rows.iter()
            .find(|row| {
                row.account.object == account.object && row.account.type_id == account.type_id
            })
            .expect("public reader must retain the selected account")
    };
    assert_eq!(
        row(nullable_a).email.as_deref(),
        Some("updated@example.test")
    );
    assert_eq!(row(nullable_a).username, "nullable-a");
    assert_eq!(row(nullable_b).email, None);
    assert_eq!(row(nullable_b).username, "nullable-b");
    assert_eq!(row(baseline).email.as_deref(), Some("exact@example.test"));
    assert_eq!(row(baseline).username, "baseline");
    for (account, (email, username)) in variants.iter().zip([
        ("EXACT@example.test", case_username),
        (" exact@example.test ", "whitespace"),
        ("line\nending@example.test", "line-feed"),
        ("line\r\nending@example.test", "carriage-return-line-feed"),
        ("caf\u{e9}@example.test", "nfc"),
        ("cafe\u{301}@example.test", "nfd"),
        ("", "empty"),
    ]) {
        assert_eq!(row(account).email.as_deref(), Some(email));
        assert_eq!(row(account).username, username);
    }
}

/// One decoded ORV1 reference envelope, or one typed NULL whose nominal type
/// is a reference to the given object type.
struct OrvReferenceOrNull {
    reference: Option<OrvReference>,
    nominal_type_id: [u8; 16],
}

/// Decode one complete stream of ORV1 Reference or typed-NULL envelopes.
///
/// A reference envelope is the canonical 41-byte `ORV1` REFERENCE shape. A
/// typed NULL of a reference nominal type is the canonical 25-byte `ORV1`
/// NULL-REFERENCE shape with the referenced object type identity and an empty
/// payload. Any other tag, shape, or trailing byte is rejected.
fn decode_reference_or_null_envelopes(bytes: &[u8]) -> Option<Vec<OrvReferenceOrNull>> {
    let mut values = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let remaining = &bytes[offset..];
        if remaining.len() < 25 || &remaining[0..4] != b"ORV1" {
            return None;
        }
        match remaining[4] {
            0x01 => {
                if remaining[21..25] != 0_u32.to_be_bytes() {
                    return None;
                }
                let mut nominal_type_id = [0; 16];
                nominal_type_id.copy_from_slice(&remaining[5..21]);
                values.push(OrvReferenceOrNull {
                    reference: None,
                    nominal_type_id,
                });
                offset += 25;
            }
            0x08 => {
                if remaining.len() < 41 {
                    return None;
                }
                let parsed = parse_reference_envelope(&remaining[..41]).ok()?;
                let nominal_type_id = parsed.type_id;
                values.push(OrvReferenceOrNull {
                    reference: Some(parsed),
                    nominal_type_id,
                });
                offset += 41;
            }
            _ => return None,
        }
    }
    Some(values)
}

/// Run one granted raw reader and decode its complete Reference-or-typed-NULL
/// stream in wire order.
fn read_reference_or_null_values(
    machine: &InstalledMachine,
    function: &str,
    label: &'static str,
) -> Result<Vec<OrvReferenceOrNull>, Error> {
    run_reader_and_decode(
        machine,
        function,
        label,
        decode_reference_or_null_envelopes,
        "complete Reference or typed-NULL envelopes",
    )
}

/// Installed public-boundary journey for the four public DELETE policies.
///
/// The test installs the exact checked-in `product_test_delete_policies.orna`
/// fixture and applies it. It requires the complete thirteen-entry sorted
/// function vector with pairwise distinct identities and proves every exact
/// sole parameter declaration, then proves every raw command is denied before
/// any grant using a syntactically valid synthetic reference input for the
/// parameterised commands.
///
/// Four distinct roots drive one policy each without masking: a NO ACTION
/// child blocks root deletion with the exact public `INTERNAL_FAILURE` line
/// until the child is deleted publicly; a RESTRICT child does the same; a SET
/// NULL child lets the root delete succeed while the child survives as one
/// typed NULL whose nominal type is the root type; a CASCADE child disappears
/// with its root. Exact source replay without any repeated grant keeps the
/// complete function and parameter discovery vector and the surviving rows. A
/// restart preserves the SET NULL survivor as typed NULL and every other
/// relation empty. After restart, one new root plus a RESTRICT child fails
/// deletion and preserves both, the blocker is removed publicly, a CASCADE
/// child on the same root is added, the root delete then succeeds, and the
/// cascade child disappears while the original SET NULL survivor stays typed
/// NULL throughout.
///
/// The test claims only public NO ACTION, RESTRICT, SET NULL, CASCADE,
/// rollback, replay, restart, identity and grant retention, and persistence
/// through the packaged `/usr/bin/orna` commands and raw-call ORV envelopes.
/// It makes no claim about private SQLSTATEs, constraint names, audit
/// records, physical storage, the exact internal error type, or row order.
#[test]
#[ignore = "requires Docker, ORNA_SYSTEM_TEST_DEBIAN_PACKAGE, and the installed orna executable"]
fn installed_delete_policies_enforce_no_action_restrict_set_null_and_cascade() {
    let package = std::env::var("ORNA_SYSTEM_TEST_DEBIAN_PACKAGE")
        .expect("ORNA_SYSTEM_TEST_DEBIAN_PACKAGE must point at the reproduced .deb package");
    let artifact = FrozenPackageArtifact::new(PackageFormat::Debian, &package)
        .expect("freeze the reproduced Debian package");
    let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("product_test_delete_policies.orna");
    let fixture = fs::read(&fixture_path).expect("read the checked-in delete policies fixture");

    let machine = InstalledMachine::start(&artifact, &fixture)
        .expect("start the installed Debian test machine");

    // Apply the exact fixture and require the thirteen sorted mappings.
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
        ["delete_policy_test", "create_cascade"],
        ["delete_policy_test", "create_no_action"],
        ["delete_policy_test", "create_restrict"],
        ["delete_policy_test", "create_root"],
        ["delete_policy_test", "create_set_null"],
        ["delete_policy_test", "delete_no_action_child"],
        ["delete_policy_test", "delete_restrict_child"],
        ["delete_policy_test", "delete_root"],
        ["delete_policy_test", "read_cascade_children"],
        ["delete_policy_test", "read_no_action_children"],
        ["delete_policy_test", "read_restrict_children"],
        ["delete_policy_test", "read_roots"],
        ["delete_policy_test", "read_set_null_children"],
    ];
    let actual_order = document
        .functions
        .iter()
        .map(|function| function.names().to_vec())
        .collect::<Vec<_>>();
    assert_eq!(
        actual_order, expected_order,
        "apply must report the thirteen function entries sorted by qualified name"
    );
    let function_id = |name: &str| {
        document
            .function_id(&["delete_policy_test", name])
            .unwrap_or_else(|_| panic!("apply must report {name}"))
    };
    let create_cascade = function_id("create_cascade");
    let create_no_action = function_id("create_no_action");
    let create_restrict = function_id("create_restrict");
    let create_root = function_id("create_root");
    let create_set_null = function_id("create_set_null");
    let delete_no_action_child = function_id("delete_no_action_child");
    let delete_restrict_child = function_id("delete_restrict_child");
    let delete_root = function_id("delete_root");
    let read_cascade_children = function_id("read_cascade_children");
    let read_no_action_children = function_id("read_no_action_children");
    let read_restrict_children = function_id("read_restrict_children");
    let read_roots = function_id("read_roots");
    let read_set_null_children = function_id("read_set_null_children");
    let identities = [
        create_cascade,
        create_no_action,
        create_restrict,
        create_root,
        create_set_null,
        delete_no_action_child,
        delete_restrict_child,
        delete_root,
        read_cascade_children,
        read_no_action_children,
        read_restrict_children,
        read_roots,
        read_set_null_children,
    ];
    for (index, left) in identities.iter().enumerate() {
        for right in &identities[index + 1..] {
            assert_ne!(
                left, right,
                "the thirteen function identities must be pairwise distinct"
            );
        }
    }
    let parameter_id = |name: &str, parameter: &str| {
        document
            .parameter_id(&["delete_policy_test", name], parameter)
            .unwrap_or_else(|_| panic!("apply must report {name}.{parameter}"))
    };
    let create_root_parameter = parameter_id("create_cascade", "p_root");
    let create_restrict_parameter = parameter_id("create_restrict", "p_root");
    let create_set_null_parameter = parameter_id("create_set_null", "p_root");
    let delete_root_parameter = parameter_id("delete_root", "p_root");
    let delete_no_action_parameter = parameter_id("delete_no_action_child", "p_child");
    let delete_restrict_parameter = parameter_id("delete_restrict_child", "p_child");
    let create_no_action_parameter = parameter_id("create_no_action", "p_root");
    let parameterised = [
        (create_no_action, create_no_action_parameter),
        (create_restrict, create_restrict_parameter),
        (create_set_null, create_set_null_parameter),
        (create_cascade, create_root_parameter),
        (delete_root, delete_root_parameter),
        (delete_no_action_child, delete_no_action_parameter),
        (delete_restrict_child, delete_restrict_parameter),
    ];
    for (function, parameter) in parameterised {
        let entry = document
            .functions
            .iter()
            .find(|entry| entry.function_id() == function)
            .expect("apply must report the parameterised function entry");
        assert_eq!(
            entry.parameters().len(),
            1,
            "the parameterised function must declare exactly one parameter"
        );
        let declared = &entry.parameters()[0];
        assert_eq!(
            declared.parameter_id(),
            parameter,
            "the declared parameter must equal the discovered identity"
        );
    }
    for name in [
        "create_root",
        "read_cascade_children",
        "read_no_action_children",
        "read_restrict_children",
        "read_roots",
        "read_set_null_children",
    ] {
        let entry = document
            .functions
            .iter()
            .find(|entry| {
                entry
                    .names()
                    .iter()
                    .map(String::as_str)
                    .eq(["delete_policy_test", name].iter().copied())
            })
            .expect("apply must report the parameter-free function entry");
        assert!(
            entry.parameters().is_empty(),
            "{name} must declare no parameters"
        );
    }

    // Source apply grants nothing: every raw command is denied before any
    // grant. The parameterised commands carry a syntactically valid synthetic
    // reference, proving authorisation precedes argument validation.
    let synthetic = reference_orv1_envelope([0x11; 16], [0x22; 16]);
    for (function, parameter) in parameterised {
        let denied = machine
            .run_as_orna_with_stdin(&["raw-call", function, parameter], &synthetic)
            .expect("run denied parameterised raw call");
        assert_denied("parameterised call before grant", denied)
            .expect("parameterised call must be denied before its grant");
    }
    for function in [
        create_root,
        read_cascade_children,
        read_no_action_children,
        read_restrict_children,
        read_roots,
        read_set_null_children,
    ] {
        let denied = machine
            .run_as_orna(&["raw-call", function])
            .expect("run denied parameter-free raw call");
        assert_denied("parameter-free call before grant", denied)
            .expect("parameter-free call must be denied before its grant");
    }

    // Grant every exact function explicitly.
    for function in identities {
        let granted = machine
            .run_as_orna(&["security", "grant-execute", function])
            .expect("run installed grant command");
        require_silent_success("orna security grant-execute", granted)
            .expect("grant must succeed silently");
    }

    // Journey 1: root A plus one NO ACTION child blocks root deletion until
    // the blocker is removed publicly.
    let root_a_call = machine
        .run_as_orna(&["raw-call", create_root])
        .expect("run root A create call");
    let root_a_call = require_value_success("orna raw-call create_root A", root_a_call)
        .expect("root A create must succeed");
    let root_a = parse_reference_envelope(&root_a_call.stdout)
        .expect("root A create must return one ORV reference");
    let child_na_call = machine
        .run_as_orna_with_stdin(
            &["raw-call", create_no_action, create_no_action_parameter],
            &reference_orv1_envelope(root_a.type_id, root_a.object),
        )
        .expect("run NO ACTION child create for root A");
    let child_na_call = require_value_success("orna raw-call create_no_action A", child_na_call)
        .expect("NO ACTION child create must succeed");
    let child_na = parse_reference_envelope(&child_na_call.stdout)
        .expect("NO ACTION child create must return one ORV reference");
    let blocked_a = machine
        .run_as_orna_with_stdin(
            &["raw-call", delete_root, delete_root_parameter],
            &reference_orv1_envelope(root_a.type_id, root_a.object),
        )
        .expect("run blocked root A delete");
    assert_exact_raw_call_failure(
        "orna raw-call delete_root A blocked",
        blocked_a,
        "raw call failed: INTERNAL_FAILURE\n",
    )
    .expect("the NO ACTION blocker must close as a public INTERNAL_FAILURE");
    assert_reference_reader_returns(
        &machine,
        read_roots,
        &[&root_a],
        "orna raw-call read_roots after blocked delete A",
    )
    .expect("the blocked delete must preserve root A");
    assert_reference_reader_returns(
        &machine,
        read_no_action_children,
        &[&root_a],
        "orna raw-call read_no_action_children after blocked delete A",
    )
    .expect("the blocked delete must preserve the NO ACTION child");
    let removed_na = machine
        .run_as_orna_with_stdin(
            &[
                "raw-call",
                delete_no_action_child,
                delete_no_action_parameter,
            ],
            &reference_orv1_envelope(child_na.type_id, child_na.object),
        )
        .expect("run NO ACTION child delete");
    let removed_na = require_value_success("orna raw-call delete_no_action_child", removed_na)
        .expect("NO ACTION child delete must succeed");
    assert_eq!(
        removed_na.stdout.as_slice(),
        boolean_orv1_envelope(Some(true)).as_slice(),
        "the NO ACTION child delete must return the exact Boolean TRUE envelope"
    );
    let deleted_a = machine
        .run_as_orna_with_stdin(
            &["raw-call", delete_root, delete_root_parameter],
            &reference_orv1_envelope(root_a.type_id, root_a.object),
        )
        .expect("run root A delete after blocker removal");
    let deleted_a = require_value_success("orna raw-call delete_root A", deleted_a)
        .expect("root A delete must succeed after blocker removal");
    assert_eq!(
        deleted_a.stdout.as_slice(),
        boolean_orv1_envelope(Some(true)).as_slice(),
        "the root A delete must return the exact Boolean TRUE envelope"
    );
    let roots_after_a = machine
        .run_as_orna(&["raw-call", read_roots])
        .expect("run root reader after journey A");
    require_silent_success("orna raw-call read_roots after journey A", roots_after_a)
        .expect("root A must disappear after its delete");
    let no_action_after_a = machine
        .run_as_orna(&["raw-call", read_no_action_children])
        .expect("run NO ACTION reader after journey A");
    require_silent_success(
        "orna raw-call read_no_action_children after journey A",
        no_action_after_a,
    )
    .expect("the NO ACTION child must disappear with its blocker removal");

    // Journey 2: root B plus one RESTRICT child blocks root deletion until
    // the blocker is removed publicly.
    let root_b_call = machine
        .run_as_orna(&["raw-call", create_root])
        .expect("run root B create call");
    let root_b_call = require_value_success("orna raw-call create_root B", root_b_call)
        .expect("root B create must succeed");
    let root_b = parse_reference_envelope(&root_b_call.stdout)
        .expect("root B create must return one ORV reference");
    let child_rc_call = machine
        .run_as_orna_with_stdin(
            &["raw-call", create_restrict, create_restrict_parameter],
            &reference_orv1_envelope(root_b.type_id, root_b.object),
        )
        .expect("run RESTRICT child create for root B");
    let child_rc_call = require_value_success("orna raw-call create_restrict B", child_rc_call)
        .expect("RESTRICT child create must succeed");
    let child_rc = parse_reference_envelope(&child_rc_call.stdout)
        .expect("RESTRICT child create must return one ORV reference");
    let blocked_b = machine
        .run_as_orna_with_stdin(
            &["raw-call", delete_root, delete_root_parameter],
            &reference_orv1_envelope(root_b.type_id, root_b.object),
        )
        .expect("run blocked root B delete");
    assert_exact_raw_call_failure(
        "orna raw-call delete_root B blocked",
        blocked_b,
        "raw call failed: INTERNAL_FAILURE\n",
    )
    .expect("the RESTRICT blocker must close as a public INTERNAL_FAILURE");
    assert_reference_reader_returns(
        &machine,
        read_roots,
        &[&root_b],
        "orna raw-call read_roots after blocked delete B",
    )
    .expect("the blocked delete must preserve root B");
    assert_reference_reader_returns(
        &machine,
        read_restrict_children,
        &[&root_b],
        "orna raw-call read_restrict_children after blocked delete B",
    )
    .expect("the blocked delete must preserve the RESTRICT child");
    let removed_rc = machine
        .run_as_orna_with_stdin(
            &["raw-call", delete_restrict_child, delete_restrict_parameter],
            &reference_orv1_envelope(child_rc.type_id, child_rc.object),
        )
        .expect("run RESTRICT child delete");
    let removed_rc = require_value_success("orna raw-call delete_restrict_child", removed_rc)
        .expect("RESTRICT child delete must succeed");
    assert_eq!(
        removed_rc.stdout.as_slice(),
        boolean_orv1_envelope(Some(true)).as_slice(),
        "the RESTRICT child delete must return the exact Boolean TRUE envelope"
    );
    let deleted_b = machine
        .run_as_orna_with_stdin(
            &["raw-call", delete_root, delete_root_parameter],
            &reference_orv1_envelope(root_b.type_id, root_b.object),
        )
        .expect("run root B delete after blocker removal");
    let deleted_b = require_value_success("orna raw-call delete_root B", deleted_b)
        .expect("root B delete must succeed after blocker removal");
    assert_eq!(
        deleted_b.stdout.as_slice(),
        boolean_orv1_envelope(Some(true)).as_slice(),
        "the root B delete must return the exact Boolean TRUE envelope"
    );
    let roots_after_b = machine
        .run_as_orna(&["raw-call", read_roots])
        .expect("run root reader after journey B");
    require_silent_success("orna raw-call read_roots after journey B", roots_after_b)
        .expect("root B must disappear after its delete");
    let restrict_after_b = machine
        .run_as_orna(&["raw-call", read_restrict_children])
        .expect("run RESTRICT reader after journey B");
    require_silent_success(
        "orna raw-call read_restrict_children after journey B",
        restrict_after_b,
    )
    .expect("the RESTRICT child must disappear with its blocker removal");

    // Journey 3: root C plus one SET NULL child lets the root delete succeed
    // while the child survives as one typed NULL of the root nominal type.
    let root_c_call = machine
        .run_as_orna(&["raw-call", create_root])
        .expect("run root C create call");
    let root_c_call = require_value_success("orna raw-call create_root C", root_c_call)
        .expect("root C create must succeed");
    let root_c = parse_reference_envelope(&root_c_call.stdout)
        .expect("root C create must return one ORV reference");
    let child_sn_call = machine
        .run_as_orna_with_stdin(
            &["raw-call", create_set_null, create_set_null_parameter],
            &reference_orv1_envelope(root_c.type_id, root_c.object),
        )
        .expect("run SET NULL child create for root C");
    let child_sn_call = require_value_success("orna raw-call create_set_null C", child_sn_call)
        .expect("SET NULL child create must succeed");
    let child_sn = parse_reference_envelope(&child_sn_call.stdout)
        .expect("SET NULL child create must return one ORV reference");
    assert!(
        child_sn.type_id != [0; 16] && !child_sn.object_is_zero(),
        "the SET NULL child must name a real nonzero row"
    );
    assert_ne!(
        child_sn.type_id, root_c.type_id,
        "the SET NULL child must use a different target type from the root"
    );
    let deleted_c = machine
        .run_as_orna_with_stdin(
            &["raw-call", delete_root, delete_root_parameter],
            &reference_orv1_envelope(root_c.type_id, root_c.object),
        )
        .expect("run root C delete");
    let deleted_c = require_value_success("orna raw-call delete_root C", deleted_c)
        .expect("root C delete must succeed with a SET NULL child");
    assert_eq!(
        deleted_c.stdout.as_slice(),
        boolean_orv1_envelope(Some(true)).as_slice(),
        "the root C delete must return the exact Boolean TRUE envelope"
    );
    let roots_after_c = machine
        .run_as_orna(&["raw-call", read_roots])
        .expect("run root reader after journey C");
    require_silent_success("orna raw-call read_roots after journey C", roots_after_c)
        .expect("root C must disappear after its delete");
    let set_null_after_c = read_reference_or_null_values(
        &machine,
        read_set_null_children,
        "orna raw-call read_set_null_children after journey C",
    )
    .expect("SET NULL reader must decode after journey C");
    assert_eq!(
        set_null_after_c.len(),
        1,
        "the SET NULL child must survive root C deletion"
    );
    assert!(
        set_null_after_c[0].reference.is_none()
            && set_null_after_c[0].nominal_type_id == root_c.type_id,
        "the surviving SET NULL child must be a typed NULL of the root type"
    );

    // Journey 4: root D plus one CASCADE child disappears with its root.
    let root_d_call = machine
        .run_as_orna(&["raw-call", create_root])
        .expect("run root D create call");
    let root_d_call = require_value_success("orna raw-call create_root D", root_d_call)
        .expect("root D create must succeed");
    let root_d = parse_reference_envelope(&root_d_call.stdout)
        .expect("root D create must return one ORV reference");
    let child_ca_call = machine
        .run_as_orna_with_stdin(
            &["raw-call", create_cascade, create_root_parameter],
            &reference_orv1_envelope(root_d.type_id, root_d.object),
        )
        .expect("run CASCADE child create for root D");
    let child_ca_call = require_value_success("orna raw-call create_cascade D", child_ca_call)
        .expect("CASCADE child create must succeed");
    let child_ca = parse_reference_envelope(&child_ca_call.stdout)
        .expect("CASCADE child create must return one ORV reference");
    assert_ne!(
        child_ca.type_id, root_d.type_id,
        "the cascade child must use a different target type from the root"
    );
    let deleted_d = machine
        .run_as_orna_with_stdin(
            &["raw-call", delete_root, delete_root_parameter],
            &reference_orv1_envelope(root_d.type_id, root_d.object),
        )
        .expect("run root D delete");
    let deleted_d = require_value_success("orna raw-call delete_root D", deleted_d)
        .expect("root D delete must succeed with a CASCADE child");
    assert_eq!(
        deleted_d.stdout.as_slice(),
        boolean_orv1_envelope(Some(true)).as_slice(),
        "the root D delete must return the exact Boolean TRUE envelope"
    );
    let roots_after_d = machine
        .run_as_orna(&["raw-call", read_roots])
        .expect("run root reader after journey D");
    require_silent_success("orna raw-call read_roots after journey D", roots_after_d)
        .expect("root D must disappear after its delete");
    let cascade_after_d = machine
        .run_as_orna(&["raw-call", read_cascade_children])
        .expect("run CASCADE reader after journey D");
    require_silent_success(
        "orna raw-call read_cascade_children after journey D",
        cascade_after_d,
    )
    .expect("the CASCADE child must disappear with its root");

    // Exact source replay keeps the complete discovery vector and the SET
    // NULL survivor without any repeated grant.
    let replay = machine
        .run_as_orna(&["source", "apply", FIXTURE_PATH])
        .expect("run installed source apply on the same fixture");
    let replay = require_success("orna source apply replay", replay)
        .expect("delete policies replay must succeed");
    assert!(
        replay.stderr.is_empty(),
        "delete policies replay must keep standard error empty"
    );
    let replay_document =
        parse_apply_document(&replay.stdout).expect("delete policies replay JSON must parse");
    assert_eq!(
        replay_document.functions, document.functions,
        "the replay must keep the complete function and parameter discovery vector"
    );
    let set_null_after_replay = read_reference_or_null_values(
        &machine,
        read_set_null_children,
        "orna raw-call read_set_null_children after replay",
    )
    .expect("SET NULL reader must decode after replay");
    assert_eq!(
        set_null_after_replay.len(),
        1,
        "the replay must preserve the SET NULL survivor"
    );
    assert!(
        set_null_after_replay[0].reference.is_none()
            && set_null_after_replay[0].nominal_type_id == root_c.type_id,
        "the replayed SET NULL survivor must stay a typed NULL of the root type"
    );

    // A restart preserves the SET NULL survivor and every empty relation.
    machine
        .restart_server()
        .expect("installed server must restart cleanly");
    let set_null_after_restart = read_reference_or_null_values(
        &machine,
        read_set_null_children,
        "orna raw-call read_set_null_children after restart",
    )
    .expect("SET NULL reader must decode after restart");
    assert_eq!(
        set_null_after_restart.len(),
        1,
        "the restart must preserve the SET NULL survivor"
    );
    assert!(
        set_null_after_restart[0].reference.is_none()
            && set_null_after_restart[0].nominal_type_id == root_c.type_id,
        "the restarted SET NULL survivor must stay a typed NULL of the root type"
    );
    for (function, label) in [
        (read_roots, "orna raw-call read_roots after restart"),
        (
            read_no_action_children,
            "orna raw-call read_no_action_children after restart",
        ),
        (
            read_restrict_children,
            "orna raw-call read_restrict_children after restart",
        ),
        (
            read_cascade_children,
            "orna raw-call read_cascade_children after restart",
        ),
    ] {
        let empty = machine
            .run_as_orna(&["raw-call", function])
            .expect("run empty relation reader after restart");
        require_silent_success(label, empty).expect("the relation must stay empty after restart");
    }

    // Post-restart: one new root plus a RESTRICT child blocks deletion and
    // preserves both; removing the blocker and adding a CASCADE child on the
    // same root lets the root delete succeed while the cascade child
    // disappears, and the original SET NULL survivor stays typed NULL.
    let root_e_call = machine
        .run_as_orna(&["raw-call", create_root])
        .expect("run root E create call");
    let root_e_call = require_value_success("orna raw-call create_root E", root_e_call)
        .expect("root E create must succeed");
    let root_e = parse_reference_envelope(&root_e_call.stdout)
        .expect("root E create must return one ORV reference");
    let child_er_call = machine
        .run_as_orna_with_stdin(
            &["raw-call", create_restrict, create_restrict_parameter],
            &reference_orv1_envelope(root_e.type_id, root_e.object),
        )
        .expect("run RESTRICT child create for root E");
    let child_er_call = require_value_success("orna raw-call create_restrict E", child_er_call)
        .expect("RESTRICT child create must succeed");
    let child_er = parse_reference_envelope(&child_er_call.stdout)
        .expect("RESTRICT child create must return one ORV reference");
    let blocked_e = machine
        .run_as_orna_with_stdin(
            &["raw-call", delete_root, delete_root_parameter],
            &reference_orv1_envelope(root_e.type_id, root_e.object),
        )
        .expect("run blocked root E delete");
    assert_exact_raw_call_failure(
        "orna raw-call delete_root E blocked",
        blocked_e,
        "raw call failed: INTERNAL_FAILURE\n",
    )
    .expect("the post-restart RESTRICT blocker must close as a public INTERNAL_FAILURE");
    assert_reference_reader_returns(
        &machine,
        read_roots,
        &[&root_e],
        "orna raw-call read_roots after blocked delete E",
    )
    .expect("the blocked delete must preserve root E");
    assert_reference_reader_returns(
        &machine,
        read_restrict_children,
        &[&root_e],
        "orna raw-call read_restrict_children after blocked delete E",
    )
    .expect("the blocked delete must preserve the RESTRICT child");
    let removed_er = machine
        .run_as_orna_with_stdin(
            &["raw-call", delete_restrict_child, delete_restrict_parameter],
            &reference_orv1_envelope(child_er.type_id, child_er.object),
        )
        .expect("run post-restart RESTRICT child delete");
    let removed_er = require_value_success("orna raw-call delete_restrict_child E", removed_er)
        .expect("RESTRICT child delete must succeed after restart");
    assert_eq!(
        removed_er.stdout.as_slice(),
        boolean_orv1_envelope(Some(true)).as_slice(),
        "the post-restart RESTRICT child delete must return the exact Boolean TRUE envelope"
    );
    let child_ec_call = machine
        .run_as_orna_with_stdin(
            &["raw-call", create_cascade, create_root_parameter],
            &reference_orv1_envelope(root_e.type_id, root_e.object),
        )
        .expect("run CASCADE child create for root E");
    require_value_success("orna raw-call create_cascade E", child_ec_call)
        .expect("CASCADE child create must succeed after restart");
    let deleted_e = machine
        .run_as_orna_with_stdin(
            &["raw-call", delete_root, delete_root_parameter],
            &reference_orv1_envelope(root_e.type_id, root_e.object),
        )
        .expect("run root E delete");
    let deleted_e = require_value_success("orna raw-call delete_root E", deleted_e)
        .expect("root E delete must succeed after blocker removal");
    assert_eq!(
        deleted_e.stdout.as_slice(),
        boolean_orv1_envelope(Some(true)).as_slice(),
        "the root E delete must return the exact Boolean TRUE envelope"
    );
    let roots_after_e = machine
        .run_as_orna(&["raw-call", read_roots])
        .expect("run root reader after journey E");
    require_silent_success("orna raw-call read_roots after journey E", roots_after_e)
        .expect("root E must disappear after its delete");
    let cascade_after_e = machine
        .run_as_orna(&["raw-call", read_cascade_children])
        .expect("run CASCADE reader after journey E");
    require_silent_success(
        "orna raw-call read_cascade_children after journey E",
        cascade_after_e,
    )
    .expect("the CASCADE child must disappear with root E");
    let set_null_final = read_reference_or_null_values(
        &machine,
        read_set_null_children,
        "orna raw-call read_set_null_children final",
    )
    .expect("SET NULL reader must decode finally");
    assert_eq!(
        set_null_final.len(),
        1,
        "the original SET NULL survivor must persist throughout"
    );
    assert!(
        set_null_final[0].reference.is_none()
            && set_null_final[0].nominal_type_id == root_c.type_id,
        "the original SET NULL survivor must stay a typed NULL of the root type"
    );
}

/// Installed public-boundary journey for one appended nullable Boolean field.
///
/// The test installs the exact checked-in `product_test.orna` fixture,
/// applies it, grants its unchanged creator and reader, and creates one live
/// TRUE row. It then replaces the fixture with the checked-in
/// `product_test_added_nullable.orna` source, which retains every old
/// declaration and appends `added BOOLEAN` plus a creator that explicitly
/// stores a Boolean and a one-column reader for the new field.
///
/// The existing create/read FunctionIds and grants survive, the two new
/// functions are denied before their grants, the pre-transition row reads as
/// one typed Boolean NULL, the unchanged old creator after the transition
/// yields another NULL, and the new creator yields its explicit Boolean. All
/// three created references share the stable nonzero probe target type with
/// distinct nonzero object identities. Replaying the exact expanded source
/// without any repeated grant keeps the complete discovery vector and the
/// unordered [NULL, NULL, explicit] multiset. A restart preserves that
/// multiset and every callable grant.
///
/// All observations go through the packaged `/usr/bin/orna` commands and
/// raw-call ORV envelopes. The test claims no revision-pair stability, no
/// private storage facts, no row ordering, and no source-text identity rules.
#[test]
#[ignore = "requires Docker, ORNA_SYSTEM_TEST_DEBIAN_PACKAGE, and the installed orna executable"]
fn installed_appended_nullable_field_survives_live_rows_grants_replay_and_restart() {
    let package = std::env::var("ORNA_SYSTEM_TEST_DEBIAN_PACKAGE")
        .expect("ORNA_SYSTEM_TEST_DEBIAN_PACKAGE must point at the reproduced .deb package");
    let artifact = FrozenPackageArtifact::new(PackageFormat::Debian, &package)
        .expect("freeze the reproduced Debian package");
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let initial_fixture =
        fs::read(manifest.join("product_test.orna")).expect("read the checked-in product fixture");
    let expanded_fixture = fs::read(manifest.join("product_test_added_nullable.orna"))
        .expect("read the checked-in appended nullable fixture");

    let machine = InstalledMachine::start(&artifact, &initial_fixture)
        .expect("start the installed Debian test machine");

    // Apply the original one-field source and require the two sorted mappings.
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
        vec!["product_test".to_string(), "create_probe".to_string()],
        vec!["product_test".to_string(), "read_probes".to_string()],
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
    let create_probe = document
        .function_id(&["product_test", "create_probe"])
        .expect("apply must report create_probe");
    let read_probes = document
        .function_id(&["product_test", "read_probes"])
        .expect("apply must report read_probes");
    assert_ne!(
        create_probe, read_probes,
        "the two original identities must be pairwise distinct"
    );
    for function in [create_probe, read_probes] {
        let denied = machine
            .run_as_orna(&["raw-call", function])
            .expect("run denied raw call");
        assert_denied("raw call before grant", denied).expect("raw call must be denied");
    }
    for function in [create_probe, read_probes] {
        let granted = machine
            .run_as_orna(&["security", "grant-execute", function])
            .expect("run installed grant command");
        require_silent_success("orna security grant-execute", granted)
            .expect("grant must succeed silently");
    }

    // One live TRUE row through the unchanged creator.
    let first_call = machine
        .run_as_orna(&["raw-call", create_probe])
        .expect("run first create call");
    let first_call = require_value_success("orna raw-call create_probe first", first_call)
        .expect("first create must succeed");
    let first_probe = parse_reference_envelope(&first_call.stdout)
        .expect("first create must return one ORV reference");
    let first_stored =
        decode_reader_values(&machine, read_probes, "orna raw-call read_probes first")
            .expect("first read must decode");
    assert_eq!(first_stored, vec![true], "the first row must store TRUE");

    // Replace the fixture with the complete expanded source and apply it.
    machine
        .write_fixture(&expanded_fixture)
        .expect("replace the fixture with the appended nullable source");
    let expanded_apply = machine
        .run_as_orna(&["source", "apply", FIXTURE_PATH])
        .expect("run installed source apply on the expanded fixture");
    let expanded_apply = require_success("orna source apply expanded", expanded_apply)
        .expect("expanded source apply must succeed");
    assert!(
        expanded_apply.stderr.is_empty(),
        "expanded source apply must keep standard error empty"
    );
    let expanded_document = parse_apply_document(&expanded_apply.stdout)
        .expect("expanded source apply JSON must parse");
    let expanded_order = [
        vec!["product_test".to_string(), "create_probe".to_string()],
        vec![
            "product_test".to_string(),
            "create_probe_with_added".to_string(),
        ],
        vec!["product_test".to_string(), "read_added".to_string()],
        vec!["product_test".to_string(), "read_probes".to_string()],
    ];
    let actual_expanded_order = expanded_document
        .functions
        .iter()
        .map(|function| function.names().to_vec())
        .collect::<Vec<_>>();
    assert_eq!(
        actual_expanded_order, expanded_order,
        "the expanded apply must report the four function entries sorted by qualified name"
    );
    let expanded_create_probe = expanded_document
        .function_id(&["product_test", "create_probe"])
        .expect("expanded apply must report create_probe");
    let expanded_read_probes = expanded_document
        .function_id(&["product_test", "read_probes"])
        .expect("expanded apply must report read_probes");
    assert_eq!(
        expanded_create_probe, create_probe,
        "create_probe identity must be stable across the field addition"
    );
    assert_eq!(
        expanded_read_probes, read_probes,
        "read_probes identity must be stable across the field addition"
    );
    let create_probe_with_added = expanded_document
        .function_id(&["product_test", "create_probe_with_added"])
        .expect("expanded apply must report create_probe_with_added");
    let read_added = expanded_document
        .function_id(&["product_test", "read_added"])
        .expect("expanded apply must report read_added");
    for (left, right) in [
        (create_probe_with_added, read_added),
        (create_probe_with_added, create_probe),
        (create_probe_with_added, read_probes),
        (read_added, create_probe),
        (read_added, read_probes),
    ] {
        assert_ne!(
            left, right,
            "the new identities must be pairwise distinct and distinct from both originals"
        );
    }
    for entry in &expanded_document.functions {
        assert!(
            entry.parameters().is_empty(),
            "every expanded function must declare no parameters"
        );
    }

    // The new functions are denied before their explicit grants.
    for function in [create_probe_with_added, read_added] {
        let denied = machine
            .run_as_orna(&["raw-call", function])
            .expect("run denied new raw call");
        assert_denied("new raw call before grant", denied).expect("new raw call must be denied");
    }
    for function in [create_probe_with_added, read_added] {
        let granted = machine
            .run_as_orna(&["security", "grant-execute", function])
            .expect("run installed grant command for the new function");
        require_silent_success("orna security grant-execute new", granted)
            .expect("new grant must succeed silently");
    }

    // The pre-transition row reads as one typed Boolean NULL.
    let added_before =
        decode_mixed_reader_values(&machine, read_added, "orna raw-call read_added first")
            .expect("first added read must decode");
    assert_eq!(
        added_before,
        vec![None],
        "the pre-transition row must read as a typed NULL"
    );

    // The unchanged old creator after the transition yields another NULL.
    let second_call = machine
        .run_as_orna(&["raw-call", create_probe])
        .expect("run unchanged old creator call");
    let second_call = require_value_success("orna raw-call create_probe second", second_call)
        .expect("old creator must succeed after the transition");
    let second_probe = parse_reference_envelope(&second_call.stdout)
        .expect("second create must return one ORV reference");
    let mut added_two =
        decode_mixed_reader_values(&machine, read_added, "orna raw-call read_added two")
            .expect("two-row added read must decode");
    added_two.sort();
    assert_eq!(
        added_two,
        vec![None, None],
        "the old creator's omitted field must read as NULL"
    );

    // The new creator yields its explicit Boolean.
    let third_call = machine
        .run_as_orna(&["raw-call", create_probe_with_added])
        .expect("run new creator call");
    let third_call = require_value_success("orna raw-call create_probe_with_added", third_call)
        .expect("new creator must succeed");
    let third_probe = parse_reference_envelope(&third_call.stdout)
        .expect("new creator must return one ORV reference");
    let mut added_three =
        decode_mixed_reader_values(&machine, read_added, "orna raw-call read_added three")
            .expect("three-row added read must decode");
    added_three.sort();
    assert_eq!(
        added_three,
        vec![None, None, Some(false)],
        "the new creator must store its explicit Boolean"
    );
    let mut stored_three =
        decode_reader_values(&machine, read_probes, "orna raw-call read_probes three")
            .expect("three-row stored read must decode");
    stored_three.sort();
    assert_eq!(
        stored_three,
        vec![true, true, true],
        "all three rows must store TRUE"
    );

    // All three references share one nonzero probe target type and distinct
    // nonzero object identities.
    assert!(
        first_probe.type_id != [0; 16]
            && second_probe.type_id != [0; 16]
            && third_probe.type_id != [0; 16],
        "all three probes must name the real nonzero target type"
    );
    assert!(
        !first_probe.object_is_zero()
            && !second_probe.object_is_zero()
            && !third_probe.object_is_zero(),
        "all three probes must name real nonzero rows"
    );
    assert_eq!(
        first_probe.type_id, second_probe.type_id,
        "the first and second probes must share the target type"
    );
    assert_eq!(
        second_probe.type_id, third_probe.type_id,
        "the second and third probes must share the target type"
    );
    assert_ne!(
        first_probe.object, second_probe.object,
        "the first and second probes must be distinct objects"
    );
    assert_ne!(
        second_probe.object, third_probe.object,
        "the second and third probes must be distinct objects"
    );
    assert_ne!(
        first_probe.object, third_probe.object,
        "the first and third probes must be distinct objects"
    );

    // Exact expanded-source replay without regrant keeps the complete
    // discovery vector and every value.
    let replay = machine
        .run_as_orna(&["source", "apply", FIXTURE_PATH])
        .expect("run installed source apply on the expanded fixture");
    let replay =
        require_success("orna source apply replay", replay).expect("expanded replay must succeed");
    assert!(
        replay.stderr.is_empty(),
        "expanded replay must keep standard error empty"
    );
    let replay_document =
        parse_apply_document(&replay.stdout).expect("expanded replay JSON must parse");
    assert_eq!(
        replay_document.functions, expanded_document.functions,
        "the replay must keep the complete function and parameter discovery vector"
    );
    let mut added_replay = decode_mixed_reader_values(
        &machine,
        read_added,
        "orna raw-call read_added after replay",
    )
    .expect("post-replay added read must decode");
    added_replay.sort();
    assert_eq!(
        added_replay,
        vec![None, None, Some(false)],
        "the replay must preserve the added multiset"
    );
    let mut stored_replay = decode_reader_values(
        &machine,
        read_probes,
        "orna raw-call read_probes after replay",
    )
    .expect("post-replay stored read must decode");
    stored_replay.sort();
    assert_eq!(
        stored_replay,
        vec![true, true, true],
        "the replay must preserve the stored values"
    );

    // A restart preserves the unordered multiset and every callable grant.
    machine
        .restart_server()
        .expect("installed server must restart cleanly");
    let mut added_restart = decode_mixed_reader_values(
        &machine,
        read_added,
        "orna raw-call read_added after restart",
    )
    .expect("post-restart added read must decode");
    added_restart.sort();
    assert_eq!(
        added_restart,
        vec![None, None, Some(false)],
        "the restart must preserve the added multiset"
    );
    let mut stored_restart = decode_reader_values(
        &machine,
        read_probes,
        "orna raw-call read_probes after restart",
    )
    .expect("post-restart stored read must decode");
    stored_restart.sort();
    assert_eq!(
        stored_restart,
        vec![true, true, true],
        "the restart must preserve the stored values"
    );

    // Every callable grant survives the restart: both creators still work
    // and the readers follow the evolving multiset.
    let fourth_call = machine
        .run_as_orna(&["raw-call", create_probe])
        .expect("run old creator after restart");
    require_value_success("orna raw-call create_probe after restart", fourth_call)
        .expect("old creator must succeed after restart");
    let mut added_four =
        decode_mixed_reader_values(&machine, read_added, "orna raw-call read_added four")
            .expect("four-row added read must decode");
    added_four.sort();
    assert_eq!(
        added_four,
        vec![None, None, None, Some(false)],
        "the old creator grant must stay callable"
    );
    let fifth_call = machine
        .run_as_orna(&["raw-call", create_probe_with_added])
        .expect("run new creator after restart");
    require_value_success(
        "orna raw-call create_probe_with_added after restart",
        fifth_call,
    )
    .expect("new creator must succeed after restart");
    let mut added_five =
        decode_mixed_reader_values(&machine, read_added, "orna raw-call read_added five")
            .expect("five-row added read must decode");
    added_five.sort();
    assert_eq!(
        added_five,
        vec![None, None, None, Some(false), Some(false)],
        "the new creator grant must stay callable"
    );
    let mut stored_five =
        decode_reader_values(&machine, read_probes, "orna raw-call read_probes five")
            .expect("five-row stored read must decode");
    stored_five.sort();
    assert_eq!(
        stored_five,
        vec![true, true, true, true, true],
        "all five rows must store TRUE"
    );
}

/// Prove that the installed development package reports the exact canonical
/// product version through the top-level `orna --version` command.
///
/// The test installs the exact frozen `.deb` in a clean Debian container with
/// the same machine setup as the other installed-product scenarios, then runs
/// the installed `/usr/bin/orna --version` as the real `orna` service account
/// through the shared `run_as_orna` helper. It requires exit status 0, the
/// exact `orna 0.1.0\n` standard output line, and completely empty standard
/// error. This is the public command-side evidence for work ADR 0047's
/// canonical `orna --version` output contract.
#[test]
#[ignore = "requires Docker, ORNA_SYSTEM_TEST_DEBIAN_PACKAGE, and the installed orna executable"]
fn installed_version_reports_the_exact_canonical_product_version() {
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

    let version = machine
        .run_as_orna(&["--version"])
        .expect("run installed orna --version");
    let version =
        require_success("orna --version", version).expect("orna --version must exit with status 0");
    assert_eq!(
        version.stdout, b"orna 0.1.0\n",
        "orna --version must emit the exact canonical version line"
    );
    assert!(
        version.stderr.is_empty(),
        "orna --version must keep standard error empty, got {} bytes",
        version.stderr.len()
    );
}

/// The exact canonical IEEE-754 binary64 bit pattern of the finite value 0.1.
const EXACT_FLOAT_BITS: u64 = 0x3fb9_9999_9999_999a;

/// The exact UTF-8 text stored and read by the raw TEXT journey.
///
/// The value is `caf\u{e9} e\u{301}\n\t\u{65e5}\u{672c}`: a precomposed
/// `caf\u{e9}`, a space, an `e` with a decomposed combining acute accent, a
/// line feed, a tab, and the Japanese "Nihon" pair. The raw path must not
/// normalise, trim, or rewrite any of those bytes.
const EXACT_TEXT: &str = "caf\u{e9} e\u{301}\n\t\u{65e5}\u{672c}";

/// The exact bytes stored and read by the raw BYTES journey.
const EXACT_BYTES: &[u8] = &[0x00, 0xff, 0x7f, 0x00, 0x01];

/// The exact Text argument that must close as an unavailable raw target.
const U0000_TEXT: &str = "a\u{0}b";

/// The canonical `ORV1` envelope for one signed 32-bit integer value.
///
/// The layout is `ORV1`, the INTEGER tag `0x03`, the 16-byte standard INTEGER
/// type identity, the 4-byte big-endian payload length, and the 4-byte
/// big-endian two's-complement value.
fn integer_orv1_envelope(value: i32) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(29);
    bytes.extend_from_slice(b"ORV1");
    bytes.push(0x03);
    bytes.extend_from_slice(&[0; 15]);
    bytes.push(0x02);
    bytes.extend_from_slice(&4_u32.to_be_bytes());
    bytes.extend_from_slice(&value.to_be_bytes());
    bytes
}

/// The canonical `ORV1` envelope for one signed 64-bit integer value.
///
/// The layout is `ORV1`, the BIGINT tag `0x04`, the 16-byte standard BIGINT
/// type identity, the 4-byte big-endian payload length, and the 8-byte
/// big-endian two's-complement value.
fn bigint_orv1_envelope(value: i64) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(33);
    bytes.extend_from_slice(b"ORV1");
    bytes.push(0x04);
    bytes.extend_from_slice(&[0; 15]);
    bytes.push(0x03);
    bytes.extend_from_slice(&8_u32.to_be_bytes());
    bytes.extend_from_slice(&value.to_be_bytes());
    bytes
}

/// The canonical `ORV1` envelope for one finite IEEE-754 binary64 float.
///
/// The value is passed as its exact bit pattern. The layout is `ORV1`, the
/// FLOAT tag `0x05`, the 16-byte standard FLOAT type identity, the 4-byte
/// big-endian payload length, and the 8-byte big-endian IEEE-754 bits.
fn float_orv1_envelope(bits: u64) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(33);
    bytes.extend_from_slice(b"ORV1");
    bytes.push(0x05);
    bytes.extend_from_slice(&[0; 15]);
    bytes.push(0x04);
    bytes.extend_from_slice(&8_u32.to_be_bytes());
    bytes.extend_from_slice(&bits.to_be_bytes());
    bytes
}

/// The canonical `ORV1` envelope for one UTF-8 text value.
///
/// The layout is `ORV1`, the TEXT tag `0x06`, the 16-byte standard
/// CHARACTER LARGE OBJECT type identity, the 4-byte big-endian payload
/// length, and the exact UTF-8 bytes.
fn text_orv1_envelope(text: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(25 + text.len());
    bytes.extend_from_slice(b"ORV1");
    bytes.push(0x06);
    bytes.extend_from_slice(&[0; 15]);
    bytes.push(0x06);
    bytes.extend_from_slice(&(text.len() as u32).to_be_bytes());
    bytes.extend_from_slice(text.as_bytes());
    bytes
}

/// The canonical `ORV1` envelope for one nullable UTF-8 text value.
///
/// `None` is a typed NULL with the standard CHARACTER LARGE OBJECT identity.
/// `Some` retains the exact canonical Text envelope.
fn nullable_text_orv1_envelope(text: Option<&str>) -> Vec<u8> {
    match text {
        Some(text) => text_orv1_envelope(text),
        None => {
            let mut bytes = Vec::with_capacity(25);
            bytes.extend_from_slice(b"ORV1");
            bytes.push(0x00);
            bytes.extend_from_slice(&[0; 15]);
            bytes.push(0x06);
            bytes.extend_from_slice(&0_u32.to_be_bytes());
            bytes
        }
    }
}

/// The canonical `ORV1` envelope for one arbitrary byte value.
///
/// The layout is `ORV1`, the BYTES tag `0x07`, the 16-byte standard
/// BINARY LARGE OBJECT type identity, the 4-byte big-endian payload length,
/// and the exact payload bytes.
fn bytes_orv1_envelope(bytes: &[u8]) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(25 + bytes.len());
    encoded.extend_from_slice(b"ORV1");
    encoded.push(0x07);
    encoded.extend_from_slice(&[0; 15]);
    encoded.push(0x07);
    encoded.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    encoded.extend_from_slice(bytes);
    encoded
}

/// Decode a stream of one canonical ORV1 scalar envelope shape.
///
/// Every envelope must start with the ORV1 marker, carry the exact tag and
/// standard type identity, and declare a complete payload. When `required` is
/// set, every envelope must declare exactly that payload length. Returns the
/// raw payload bytes in order, or `None` when any envelope is malformed,
/// truncated, wrong-length, or trailing bytes remain.
fn decode_scalar_payloads(
    bytes: &[u8],
    tag: u8,
    type_byte: u8,
    required: Option<usize>,
) -> Option<Vec<Vec<u8>>> {
    let mut payloads = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let remaining = &bytes[offset..];
        if remaining.len() < 25
            || &remaining[0..4] != b"ORV1"
            || remaining[4] != tag
            || remaining[5..20] != [0; 15]
            || remaining[20] != type_byte
        {
            return None;
        }
        let length = u32::from_be_bytes(remaining[21..25].try_into().ok()?) as usize;
        if required.is_some_and(|expected| length != expected) || remaining.len() < 25 + length {
            return None;
        }
        payloads.push(remaining[25..25 + length].to_vec());
        offset += 25 + length;
    }
    Some(payloads)
}

/// Decode a stream of complete canonical ORV1 Integer envelopes in order.
///
/// Returns `None` when any envelope is malformed or trailing bytes remain.
fn decode_integer_envelopes(bytes: &[u8]) -> Option<Vec<i32>> {
    decode_scalar_payloads(bytes, 0x03, 0x02, Some(4))?
        .into_iter()
        .map(|payload| Some(i32::from_be_bytes(payload.try_into().ok()?)))
        .collect()
}

/// Decode a stream of complete canonical ORV1 BigInt envelopes in order.
///
/// Returns `None` when any envelope is malformed or trailing bytes remain.
fn decode_bigint_envelopes(bytes: &[u8]) -> Option<Vec<i64>> {
    decode_scalar_payloads(bytes, 0x04, 0x03, Some(8))?
        .into_iter()
        .map(|payload| Some(i64::from_be_bytes(payload.try_into().ok()?)))
        .collect()
}

/// Decode a stream of complete canonical ORV1 Float envelopes in order.
///
/// Each value is returned as its exact IEEE-754 binary64 bit pattern. Returns
/// `None` when any envelope is malformed or trailing bytes remain.
fn decode_float_envelopes(bytes: &[u8]) -> Option<Vec<u64>> {
    decode_scalar_payloads(bytes, 0x05, 0x04, Some(8))?
        .into_iter()
        .map(|payload| Some(u64::from_be_bytes(payload.try_into().ok()?)))
        .collect()
}

/// Decode a stream of complete canonical ORV1 Text envelopes in order.
///
/// Every payload must be exact UTF-8. Returns `None` when any envelope is
/// malformed or trailing bytes remain.
fn decode_text_envelopes(bytes: &[u8]) -> Option<Vec<String>> {
    decode_scalar_payloads(bytes, 0x06, 0x06, None)?
        .into_iter()
        .map(|payload| String::from_utf8(payload).ok())
        .collect()
}

/// Decode a stream of complete canonical ORV1 Bytes envelopes in order.
///
/// Returns `None` when any envelope is malformed or trailing bytes remain.
fn decode_bytes_envelopes(bytes: &[u8]) -> Option<Vec<Vec<u8>>> {
    decode_scalar_payloads(bytes, 0x07, 0x07, None)
}

/// Run one granted raw scalar reader and require the exact decoded values.
///
/// The reader must exit 0 with empty standard error, and its output must
/// decode as exactly the given expected values in order.
fn require_scalar_reader_returns<T>(
    machine: &InstalledMachine,
    function: &str,
    label: &'static str,
    expected: Vec<T>,
    decode: impl FnOnce(&[u8]) -> Option<Vec<T>>,
) -> Result<(), Error>
where
    T: PartialEq + fmt::Debug,
{
    let actual = run_reader_and_decode(
        machine,
        function,
        label,
        decode,
        "complete scalar envelopes",
    )?;
    if actual != expected {
        return Err(Error::Unexpected {
            message: format!("{label} must return exactly {expected:?}, got {actual:?}"),
        });
    }
    Ok(())
}

/// Prove the installed product's canonical raw scalar INSERT journey of work
/// ADR 0045 through the packaged `/usr/bin/orna` public commands.
///
/// The fixture is the exact checked-in `fixtures/product_test_scalar_insert.orna`
/// source with five one-field NOT NULL objects, five single-parameter
/// INSERT/RETURNING REF functions, and five parameter-free readers. Every
/// product step runs through `/usr/bin/orna` public commands as the `orna`
/// service account. The test asserts:
///
/// * apply reports the ten sorted qualified-name mappings, a canonical
///   `p_value` parameter identity on every writer, and no parameter on any
///   reader;
/// * every writer raw call is `EXECUTE_DENIED` before any grant;
/// * granting only the five readers leaves every reader successful and empty,
///   proving the denied writers created no row;
/// * after granting the five writers, Text U+0000 closes as
///   `TARGET_UNAVAILABLE` and creates no row;
/// * each exact boundary or representative value inserts a distinct nonzero
///   typed reference, and each public reader returns exactly the stored value
///   or byte pattern;
/// * an exact source replay keeps the complete ten-entry function vector
///   including parameters, and the grants and rows survive without regrant;
/// * a restart keeps the grants and rows, and one further call of each new
///   scalar type with the original discovered identities stores a second exact
///   value per type.
///
/// The Boolean and Reference INSERT journeys are proven by the separate
/// installed Boolean and Reference tests and are not duplicated here.
#[test]
#[ignore = "requires Docker, ORNA_SYSTEM_TEST_DEBIAN_PACKAGE, and the ADR 0045 scalar INSERT commands in the installed orna executable"]
fn installed_scalar_insert_binds_exact_values_and_survives_replay_and_restart() {
    let package = std::env::var("ORNA_SYSTEM_TEST_DEBIAN_PACKAGE")
        .expect("ORNA_SYSTEM_TEST_DEBIAN_PACKAGE must point at the reproduced .deb package");
    let artifact = FrozenPackageArtifact::new(PackageFormat::Debian, &package)
        .expect("freeze the reproduced Debian package");
    let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("product_test_scalar_insert.orna");
    let fixture = fs::read(&fixture_path).expect("read the checked-in scalar insert fixture");

    let machine = InstalledMachine::start(&artifact, &fixture)
        .expect("start the installed Debian test machine");

    // Apply the exact fixture and require the ten sorted mappings.
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
            "scalar_insert_test".to_string(),
            "create_bigint".to_string(),
        ],
        vec!["scalar_insert_test".to_string(), "create_bytes".to_string()],
        vec!["scalar_insert_test".to_string(), "create_float".to_string()],
        vec!["scalar_insert_test".to_string(), "create_int".to_string()],
        vec!["scalar_insert_test".to_string(), "create_text".to_string()],
        vec!["scalar_insert_test".to_string(), "read_bigints".to_string()],
        vec!["scalar_insert_test".to_string(), "read_bytes".to_string()],
        vec!["scalar_insert_test".to_string(), "read_floats".to_string()],
        vec!["scalar_insert_test".to_string(), "read_ints".to_string()],
        vec!["scalar_insert_test".to_string(), "read_texts".to_string()],
    ];
    let actual_order = document
        .functions
        .iter()
        .map(|function| function.names().to_vec())
        .collect::<Vec<_>>();
    assert_eq!(
        actual_order, expected_order,
        "apply must report the ten function entries sorted by qualified name"
    );

    // Resolve the ten identities and prove they are pairwise distinct.
    let create_int = document
        .function_id(&["scalar_insert_test", "create_int"])
        .expect("apply must report create_int");
    let create_bigint = document
        .function_id(&["scalar_insert_test", "create_bigint"])
        .expect("apply must report create_bigint");
    let create_float = document
        .function_id(&["scalar_insert_test", "create_float"])
        .expect("apply must report create_float");
    let create_text = document
        .function_id(&["scalar_insert_test", "create_text"])
        .expect("apply must report create_text");
    let create_bytes = document
        .function_id(&["scalar_insert_test", "create_bytes"])
        .expect("apply must report create_bytes");
    let read_ints = document
        .function_id(&["scalar_insert_test", "read_ints"])
        .expect("apply must report read_ints");
    let read_bigints = document
        .function_id(&["scalar_insert_test", "read_bigints"])
        .expect("apply must report read_bigints");
    let read_floats = document
        .function_id(&["scalar_insert_test", "read_floats"])
        .expect("apply must report read_floats");
    let read_texts = document
        .function_id(&["scalar_insert_test", "read_texts"])
        .expect("apply must report read_texts");
    let read_bytes = document
        .function_id(&["scalar_insert_test", "read_bytes"])
        .expect("apply must report read_bytes");
    let identities = [
        create_int,
        create_bigint,
        create_float,
        create_text,
        create_bytes,
        read_ints,
        read_bigints,
        read_floats,
        read_texts,
        read_bytes,
    ];
    for (index, left) in identities.iter().enumerate() {
        for right in &identities[index + 1..] {
            assert_ne!(
                left, right,
                "the ten function identities must be pairwise distinct"
            );
        }
    }

    // Every writer declares exactly one p_value parameter; every reader
    // declares none.
    for name in [
        "create_bigint",
        "create_bytes",
        "create_float",
        "create_int",
        "create_text",
    ] {
        let entry = document
            .functions
            .iter()
            .find(|function| {
                function
                    .names()
                    .iter()
                    .map(String::as_str)
                    .eq(["scalar_insert_test", name].iter().copied())
            })
            .expect("apply must report the writer entry");
        assert_eq!(
            entry.parameters().len(),
            1,
            "each writer must declare exactly one parameter"
        );
        assert_eq!(
            entry.parameters()[0].name(),
            "p_value",
            "each writer must declare exactly the p_value parameter"
        );
    }
    for name in [
        "read_bigints",
        "read_bytes",
        "read_floats",
        "read_ints",
        "read_texts",
    ] {
        let entry = document
            .functions
            .iter()
            .find(|function| {
                function
                    .names()
                    .iter()
                    .map(String::as_str)
                    .eq(["scalar_insert_test", name].iter().copied())
            })
            .expect("apply must report the reader entry");
        assert!(
            entry.parameters().is_empty(),
            "each reader must declare no parameters"
        );
    }

    // Discover the five canonical parameter identities and prove they are
    // pairwise distinct.
    let p_int = document
        .parameter_id(&["scalar_insert_test", "create_int"], "p_value")
        .expect("apply must report create_int.p_value");
    let p_bigint = document
        .parameter_id(&["scalar_insert_test", "create_bigint"], "p_value")
        .expect("apply must report create_bigint.p_value");
    let p_float = document
        .parameter_id(&["scalar_insert_test", "create_float"], "p_value")
        .expect("apply must report create_float.p_value");
    let p_text = document
        .parameter_id(&["scalar_insert_test", "create_text"], "p_value")
        .expect("apply must report create_text.p_value");
    let p_bytes = document
        .parameter_id(&["scalar_insert_test", "create_bytes"], "p_value")
        .expect("apply must report create_bytes.p_value");
    let parameter_ids = [p_int, p_bigint, p_float, p_text, p_bytes];
    for (index, left) in parameter_ids.iter().enumerate() {
        for right in &parameter_ids[index + 1..] {
            assert_ne!(
                left, right,
                "each writer parameter must carry a distinct canonical identity"
            );
        }
    }

    // Before any grant, every writer raw call with its exact argument
    // envelope is denied and nothing is stored.
    assert_eq!(
        EXACT_FLOAT_BITS,
        0.1_f64.to_bits(),
        "the exact FLOAT constant must be the canonical 0.1 bit pattern"
    );
    for (writer, parameter, envelope) in [
        (create_int, p_int, integer_orv1_envelope(i32::MIN)),
        (create_bigint, p_bigint, bigint_orv1_envelope(i64::MAX)),
        (create_float, p_float, float_orv1_envelope(EXACT_FLOAT_BITS)),
        (create_text, p_text, text_orv1_envelope(EXACT_TEXT)),
        (create_bytes, p_bytes, bytes_orv1_envelope(EXACT_BYTES)),
    ] {
        let denied = machine
            .run_as_orna_with_stdin(&["raw-call", writer, parameter], &envelope)
            .expect("run denied writer raw call");
        assert_denied("writer raw call before grant", denied)
            .expect("writer raw call must be denied before grant");
    }

    // Grant only the five readers, then prove the denied writers created no
    // row: every reader succeeds with completely empty output.
    for reader in [read_ints, read_bigints, read_floats, read_texts, read_bytes] {
        let granted = machine
            .run_as_orna(&["security", "grant-execute", reader])
            .expect("run installed reader grant command");
        require_silent_success("orna security grant-execute reader", granted)
            .expect("reader grant must succeed silently");
    }
    for reader in [read_ints, read_bigints, read_floats, read_texts, read_bytes] {
        let empty = machine
            .run_as_orna(&["raw-call", reader])
            .expect("run empty raw reader after denied writers");
        require_silent_success("orna raw-call reader empty", empty)
            .expect("the denied writers must leave the reader empty");
    }

    // Grant the five writers.
    for writer in [
        create_int,
        create_bigint,
        create_float,
        create_text,
        create_bytes,
    ] {
        let granted = machine
            .run_as_orna(&["security", "grant-execute", writer])
            .expect("run installed writer grant command");
        require_silent_success("orna security grant-execute writer", granted)
            .expect("writer grant must succeed silently");
    }

    // Text U+0000 is an authorised target failure after the grant: it closes
    // as TARGET_UNAVAILABLE, creates no row, and never reaches the driver.
    let u0000 = machine
        .run_as_orna_with_stdin(
            &["raw-call", create_text, p_text],
            &text_orv1_envelope(U0000_TEXT),
        )
        .expect("run U+0000 raw Text call");
    assert_target_unavailable("Text U+0000 raw call", u0000)
        .expect("Text U+0000 must close as target unavailable");
    let no_text = machine
        .run_as_orna(&["raw-call", read_texts])
        .expect("run empty raw Text reader after U+0000");
    require_silent_success("orna raw-call read_texts after U+0000", no_text)
        .expect("the U+0000 call must create no row");

    // Each exact scalar binds its discovered ParameterId and stores the exact
    // value; every INSERT returns one nonzero typed reference.
    let inserted_int = machine
        .run_as_orna_with_stdin(
            &["raw-call", create_int, p_int],
            &integer_orv1_envelope(i32::MIN),
        )
        .expect("run exact INT raw call");
    let inserted_int = require_value_success("orna raw-call create_int", inserted_int)
        .expect("INT create must succeed");
    let int_reference = parse_reference_envelope(&inserted_int.stdout)
        .expect("INT create must return one ORV reference");
    assert!(
        int_reference.type_id != [0; 16] && !int_reference.object_is_zero(),
        "the INT create reference must name a real target type and row"
    );
    require_scalar_reader_returns(
        &machine,
        read_ints,
        "orna raw-call read_ints one value",
        vec![i32::MIN],
        decode_integer_envelopes,
    )
    .expect("read_ints must return exactly the stored INT value");

    let inserted_bigint = machine
        .run_as_orna_with_stdin(
            &["raw-call", create_bigint, p_bigint],
            &bigint_orv1_envelope(i64::MAX),
        )
        .expect("run exact BIGINT raw call");
    let inserted_bigint = require_value_success("orna raw-call create_bigint", inserted_bigint)
        .expect("BIGINT create must succeed");
    let bigint_reference = parse_reference_envelope(&inserted_bigint.stdout)
        .expect("BIGINT create must return one ORV reference");
    assert!(
        bigint_reference.type_id != [0; 16] && !bigint_reference.object_is_zero(),
        "the BIGINT create reference must name a real target type and row"
    );
    require_scalar_reader_returns(
        &machine,
        read_bigints,
        "orna raw-call read_bigints one value",
        vec![i64::MAX],
        decode_bigint_envelopes,
    )
    .expect("read_bigints must return exactly the stored BIGINT value");

    let inserted_float = machine
        .run_as_orna_with_stdin(
            &["raw-call", create_float, p_float],
            &float_orv1_envelope(EXACT_FLOAT_BITS),
        )
        .expect("run exact FLOAT raw call");
    let inserted_float = require_value_success("orna raw-call create_float", inserted_float)
        .expect("FLOAT create must succeed");
    let float_reference = parse_reference_envelope(&inserted_float.stdout)
        .expect("FLOAT create must return one ORV reference");
    assert!(
        float_reference.type_id != [0; 16] && !float_reference.object_is_zero(),
        "the FLOAT create reference must name a real target type and row"
    );
    require_scalar_reader_returns(
        &machine,
        read_floats,
        "orna raw-call read_floats one value",
        vec![EXACT_FLOAT_BITS],
        decode_float_envelopes,
    )
    .expect("read_floats must return exactly the stored 0.1 bit pattern");

    let inserted_text = machine
        .run_as_orna_with_stdin(
            &["raw-call", create_text, p_text],
            &text_orv1_envelope(EXACT_TEXT),
        )
        .expect("run exact TEXT raw call");
    let inserted_text = require_value_success("orna raw-call create_text", inserted_text)
        .expect("TEXT create must succeed");
    let text_reference = parse_reference_envelope(&inserted_text.stdout)
        .expect("TEXT create must return one ORV reference");
    assert!(
        text_reference.type_id != [0; 16] && !text_reference.object_is_zero(),
        "the TEXT create reference must name a real target type and row"
    );
    require_scalar_reader_returns(
        &machine,
        read_texts,
        "orna raw-call read_texts one value",
        vec![EXACT_TEXT.to_string()],
        decode_text_envelopes,
    )
    .expect("read_texts must return exactly the stored UTF-8 text");

    let inserted_bytes = machine
        .run_as_orna_with_stdin(
            &["raw-call", create_bytes, p_bytes],
            &bytes_orv1_envelope(EXACT_BYTES),
        )
        .expect("run exact BYTES raw call");
    let inserted_bytes = require_value_success("orna raw-call create_bytes", inserted_bytes)
        .expect("BYTES create must succeed");
    let bytes_reference = parse_reference_envelope(&inserted_bytes.stdout)
        .expect("BYTES create must return one ORV reference");
    assert!(
        bytes_reference.type_id != [0; 16] && !bytes_reference.object_is_zero(),
        "the BYTES create reference must name a real target type and row"
    );
    require_scalar_reader_returns(
        &machine,
        read_bytes,
        "orna raw-call read_bytes one value",
        vec![EXACT_BYTES.to_vec()],
        decode_bytes_envelopes,
    )
    .expect("read_bytes must return exactly the stored byte sequence");

    // Each scalar INSERT allocates a distinct object identity.
    let objects = [
        int_reference.object,
        bigint_reference.object,
        float_reference.object,
        text_reference.object,
        bytes_reference.object,
    ];
    for (index, left) in objects.iter().enumerate() {
        for right in &objects[index + 1..] {
            assert_ne!(
                left, right,
                "each scalar INSERT must allocate a distinct object identity"
            );
        }
    }

    // Exact source replay keeps the complete mapping including parameters,
    // and no re-grant is needed.
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
        "the replay must keep the complete ten-entry function vector including parameters"
    );
    assert_eq!(
        replay_document
            .parameter_id(&["scalar_insert_test", "create_int"], "p_value")
            .expect("the replay must keep create_int.p_value"),
        p_int,
        "the replay must keep the exact canonical parameter identities"
    );
    for reader in [read_ints, read_bigints, read_floats, read_texts, read_bytes] {
        let after_replay = machine
            .run_as_orna(&["raw-call", reader])
            .expect("run raw reader after replay");
        require_success("orna raw-call reader after replay", after_replay)
            .expect("the reader grant must survive the replay");
    }
    require_scalar_reader_returns(
        &machine,
        read_ints,
        "orna raw-call read_ints after replay",
        vec![i32::MIN],
        decode_integer_envelopes,
    )
    .expect("read_ints must stay exact after replay");
    require_scalar_reader_returns(
        &machine,
        read_bigints,
        "orna raw-call read_bigints after replay",
        vec![i64::MAX],
        decode_bigint_envelopes,
    )
    .expect("read_bigints must stay exact after replay");
    require_scalar_reader_returns(
        &machine,
        read_floats,
        "orna raw-call read_floats after replay",
        vec![EXACT_FLOAT_BITS],
        decode_float_envelopes,
    )
    .expect("read_floats must stay exact after replay");
    require_scalar_reader_returns(
        &machine,
        read_texts,
        "orna raw-call read_texts after replay",
        vec![EXACT_TEXT.to_string()],
        decode_text_envelopes,
    )
    .expect("read_texts must stay exact after replay");
    require_scalar_reader_returns(
        &machine,
        read_bytes,
        "orna raw-call read_bytes after replay",
        vec![EXACT_BYTES.to_vec()],
        decode_bytes_envelopes,
    )
    .expect("read_bytes must stay exact after replay");

    // Restart keeps the grants and rows; one further call of each new scalar
    // type uses the original discovered identities and stores a second exact
    // value.
    machine
        .restart_server()
        .expect("installed server must restart cleanly");
    require_scalar_reader_returns(
        &machine,
        read_ints,
        "orna raw-call read_ints after restart",
        vec![i32::MIN],
        decode_integer_envelopes,
    )
    .expect("read_ints must survive the restart");
    require_scalar_reader_returns(
        &machine,
        read_bigints,
        "orna raw-call read_bigints after restart",
        vec![i64::MAX],
        decode_bigint_envelopes,
    )
    .expect("read_bigints must survive the restart");
    require_scalar_reader_returns(
        &machine,
        read_floats,
        "orna raw-call read_floats after restart",
        vec![EXACT_FLOAT_BITS],
        decode_float_envelopes,
    )
    .expect("read_floats must survive the restart");
    require_scalar_reader_returns(
        &machine,
        read_texts,
        "orna raw-call read_texts after restart",
        vec![EXACT_TEXT.to_string()],
        decode_text_envelopes,
    )
    .expect("read_texts must survive the restart");
    require_scalar_reader_returns(
        &machine,
        read_bytes,
        "orna raw-call read_bytes after restart",
        vec![EXACT_BYTES.to_vec()],
        decode_bytes_envelopes,
    )
    .expect("read_bytes must survive the restart");

    let after_int = machine
        .run_as_orna_with_stdin(
            &["raw-call", create_int, p_int],
            &integer_orv1_envelope(i32::MIN),
        )
        .expect("run post-restart INT raw call");
    let after_int = require_value_success("orna raw-call create_int after restart", after_int)
        .expect("INT create must succeed after restart");
    let after_int_reference = parse_reference_envelope(&after_int.stdout)
        .expect("post-restart INT create must return one ORV reference");
    assert_eq!(
        after_int_reference.type_id, int_reference.type_id,
        "the post-restart INT create must target the same object type"
    );
    assert!(
        after_int_reference.object != int_reference.object && !after_int_reference.object_is_zero(),
        "the post-restart INT create must allocate a distinct real row"
    );

    let after_bigint = machine
        .run_as_orna_with_stdin(
            &["raw-call", create_bigint, p_bigint],
            &bigint_orv1_envelope(i64::MAX),
        )
        .expect("run post-restart BIGINT raw call");
    let after_bigint =
        require_value_success("orna raw-call create_bigint after restart", after_bigint)
            .expect("BIGINT create must succeed after restart");
    let after_bigint_reference = parse_reference_envelope(&after_bigint.stdout)
        .expect("post-restart BIGINT create must return one ORV reference");
    assert_eq!(
        after_bigint_reference.type_id, bigint_reference.type_id,
        "the post-restart BIGINT create must target the same object type"
    );
    assert!(
        after_bigint_reference.object != bigint_reference.object
            && !after_bigint_reference.object_is_zero(),
        "the post-restart BIGINT create must allocate a distinct real row"
    );

    let after_float = machine
        .run_as_orna_with_stdin(
            &["raw-call", create_float, p_float],
            &float_orv1_envelope(EXACT_FLOAT_BITS),
        )
        .expect("run post-restart FLOAT raw call");
    let after_float =
        require_value_success("orna raw-call create_float after restart", after_float)
            .expect("FLOAT create must succeed after restart");
    let after_float_reference = parse_reference_envelope(&after_float.stdout)
        .expect("post-restart FLOAT create must return one ORV reference");
    assert_eq!(
        after_float_reference.type_id, float_reference.type_id,
        "the post-restart FLOAT create must target the same object type"
    );
    assert!(
        after_float_reference.object != float_reference.object
            && !after_float_reference.object_is_zero(),
        "the post-restart FLOAT create must allocate a distinct real row"
    );

    let after_text = machine
        .run_as_orna_with_stdin(
            &["raw-call", create_text, p_text],
            &text_orv1_envelope(EXACT_TEXT),
        )
        .expect("run post-restart TEXT raw call");
    let after_text = require_value_success("orna raw-call create_text after restart", after_text)
        .expect("TEXT create must succeed after restart");
    let after_text_reference = parse_reference_envelope(&after_text.stdout)
        .expect("post-restart TEXT create must return one ORV reference");
    assert_eq!(
        after_text_reference.type_id, text_reference.type_id,
        "the post-restart TEXT create must target the same object type"
    );
    assert!(
        after_text_reference.object != text_reference.object
            && !after_text_reference.object_is_zero(),
        "the post-restart TEXT create must allocate a distinct real row"
    );

    let after_bytes = machine
        .run_as_orna_with_stdin(
            &["raw-call", create_bytes, p_bytes],
            &bytes_orv1_envelope(EXACT_BYTES),
        )
        .expect("run post-restart BYTES raw call");
    let after_bytes =
        require_value_success("orna raw-call create_bytes after restart", after_bytes)
            .expect("BYTES create must succeed after restart");
    let after_bytes_reference = parse_reference_envelope(&after_bytes.stdout)
        .expect("post-restart BYTES create must return one ORV reference");
    assert_eq!(
        after_bytes_reference.type_id, bytes_reference.type_id,
        "the post-restart BYTES create must target the same object type"
    );
    assert!(
        after_bytes_reference.object != bytes_reference.object
            && !after_bytes_reference.object_is_zero(),
        "the post-restart BYTES create must allocate a distinct real row"
    );

    // Every reader now returns exactly two copies of its exact stored value.
    require_scalar_reader_returns(
        &machine,
        read_ints,
        "orna raw-call read_ints two values",
        vec![i32::MIN, i32::MIN],
        decode_integer_envelopes,
    )
    .expect("read_ints must return exactly two stored INT values");
    require_scalar_reader_returns(
        &machine,
        read_bigints,
        "orna raw-call read_bigints two values",
        vec![i64::MAX, i64::MAX],
        decode_bigint_envelopes,
    )
    .expect("read_bigints must return exactly two stored BIGINT values");
    require_scalar_reader_returns(
        &machine,
        read_floats,
        "orna raw-call read_floats two values",
        vec![EXACT_FLOAT_BITS, EXACT_FLOAT_BITS],
        decode_float_envelopes,
    )
    .expect("read_floats must return exactly two stored 0.1 bit patterns");
    require_scalar_reader_returns(
        &machine,
        read_texts,
        "orna raw-call read_texts two values",
        vec![EXACT_TEXT.to_string(), EXACT_TEXT.to_string()],
        decode_text_envelopes,
    )
    .expect("read_texts must return exactly two stored UTF-8 texts");
    require_scalar_reader_returns(
        &machine,
        read_bytes,
        "orna raw-call read_bytes two values",
        vec![EXACT_BYTES.to_vec(), EXACT_BYTES.to_vec()],
        decode_bytes_envelopes,
    )
    .expect("read_bytes must return exactly two stored byte sequences");
}

/// Decode a stream of nullable ORV1 scalar envelopes of one exact type.
///
/// Every envelope must start with the ORV1 marker, carry the exact standard
/// type identity, and declare a complete payload. A typed NULL envelope uses
/// tag `0x00` with payload length zero and decodes as `None`. Any other
/// envelope must carry the exact scalar `tag` and decodes as `Some` with its
/// raw payload bytes. A wrong tag, a wrong type identity, a truncated
/// payload, or trailing bytes fail the whole stream.
fn decode_nullable_scalar_payloads(
    bytes: &[u8],
    tag: u8,
    type_byte: u8,
) -> Option<Vec<Option<Vec<u8>>>> {
    let mut values = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let remaining = &bytes[offset..];
        if remaining.len() < 25
            || &remaining[0..4] != b"ORV1"
            || remaining[5..20] != [0; 15]
            || remaining[20] != type_byte
        {
            return None;
        }
        let length = u32::from_be_bytes(remaining[21..25].try_into().ok()?) as usize;
        if remaining.len() < 25 + length {
            return None;
        }
        match remaining[4] {
            0x00 if length == 0 => values.push(None),
            value if value == tag => values.push(Some(remaining[25..25 + length].to_vec())),
            _ => return None,
        }
        offset += 25 + length;
    }
    Some(values)
}

/// Decode a stream of nullable canonical ORV1 Integer envelopes in order.
///
/// A typed NULL is `None`; a value must be exactly four payload bytes and
/// decodes as `Some(i32)`. Returns `None` when any envelope is malformed,
/// wrong-length, or trailing bytes remain.
fn decode_nullable_integer_envelopes(bytes: &[u8]) -> Option<Vec<Option<i32>>> {
    let values = decode_nullable_scalar_payloads(bytes, 0x03, 0x02)?;
    let mut integers = Vec::with_capacity(values.len());
    for payload in values {
        match payload {
            None => integers.push(None),
            Some(bytes) => integers.push(Some(i32::from_be_bytes(bytes.try_into().ok()?))),
        }
    }
    Some(integers)
}

/// Decode a stream of nullable canonical ORV1 Text envelopes in order.
///
/// A typed NULL is `None`; a value must be exact UTF-8 and decodes as
/// `Some(String)`. Returns `None` when any envelope is malformed, not valid
/// UTF-8, or trailing bytes remain.
fn decode_nullable_text_envelopes(bytes: &[u8]) -> Option<Vec<Option<String>>> {
    let values = decode_nullable_scalar_payloads(bytes, 0x06, 0x06)?;
    let mut texts = Vec::with_capacity(values.len());
    for payload in values {
        match payload {
            None => texts.push(None),
            Some(bytes) => texts.push(Some(String::from_utf8(bytes).ok()?)),
        }
    }
    Some(texts)
}

/// Prove one added nullable scalar field journey through the installed
/// product's public apply, grant, raw-call, replay, and restart path.
///
/// The fixture is `fixtures/product_test.orna` applied first, then the exact
/// checked-in expanded fixture named by `fixture_name`, which keeps
/// `stored BOOLEAN NOT NULL` and adds one nullable `added` field of the scalar
/// type. The journey asserts:
///
/// * the original apply reports exactly the two sorted create/read mappings,
///   both raw calls are denied before grant and succeed silently after, one
///   create stores TRUE, and the reader returns exactly one TRUE;
/// * the expanded apply reports exactly four sorted mappings, the two
///   original identities stay stable, exactly `create_probe_with_added`
///   declares the canonical `p_added` parameter while every other function
///   declares none, the two new functions are denied then granted;
/// * the pre-transition row reads as one typed NULL, the unchanged old
///   creator adds a second typed NULL, and the new creator with the exact
///   explicit argument envelope stores `explicit_value`;
/// * the three creates share one nonzero target type and pairwise distinct
///   object identities, `read_added` decodes as the unordered
///   [None, None, Some(explicit)] multiset, and `read_probes` stays all TRUE;
/// * an exact expanded replay keeps the complete function and parameter
///   discovery vector and every value without any re-grant;
/// * a restart preserves the multiset and every callable grant, after which
///   the old creator adds a fourth typed NULL and the new creator adds a
///   second explicit value, leaving [None, None, None, Some, Some] unordered
///   with all five stored rows TRUE.
fn run_added_nullable_scalar_journey<T>(
    fixture_name: &str,
    explicit_envelope: Vec<u8>,
    decode_added: impl Fn(&[u8]) -> Option<Vec<Option<T>>>,
    explicit_value: T,
) -> Result<(), Error>
where
    T: Ord + Clone + fmt::Debug,
{
    let package = std::env::var("ORNA_SYSTEM_TEST_DEBIAN_PACKAGE")
        .expect("ORNA_SYSTEM_TEST_DEBIAN_PACKAGE must point at the reproduced .deb package");
    let artifact = FrozenPackageArtifact::new(PackageFormat::Debian, &package)
        .expect("freeze the reproduced Debian package");
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let original =
        fs::read(manifest.join("product_test.orna")).expect("read the checked-in product fixture");
    let expanded =
        fs::read(manifest.join(fixture_name)).expect("read the checked-in added nullable fixture");

    let machine = InstalledMachine::start(&artifact, &original)
        .expect("start the installed Debian test machine");

    // Apply the original one-field source and require the two sorted mappings.
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
        vec!["product_test".to_string(), "create_probe".to_string()],
        vec!["product_test".to_string(), "read_probes".to_string()],
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
    let create_probe = document
        .function_id(&["product_test", "create_probe"])
        .expect("apply must report create_probe");
    let read_probes = document
        .function_id(&["product_test", "read_probes"])
        .expect("apply must report read_probes");
    assert_ne!(
        create_probe, read_probes,
        "the two original identities must be pairwise distinct"
    );
    for function in [create_probe, read_probes] {
        let denied = machine
            .run_as_orna(&["raw-call", function])
            .expect("run denied raw call");
        assert_denied("raw call before grant", denied).expect("raw call must be denied");
    }
    for function in [create_probe, read_probes] {
        let granted = machine
            .run_as_orna(&["security", "grant-execute", function])
            .expect("run installed grant command");
        require_silent_success("orna security grant-execute", granted)
            .expect("grant must succeed silently");
    }

    // One live TRUE row through the unchanged creator.
    let first_call = machine
        .run_as_orna(&["raw-call", create_probe])
        .expect("run first create call");
    let first_call = require_value_success("orna raw-call create_probe first", first_call)
        .expect("first create must succeed");
    let first_probe = parse_reference_envelope(&first_call.stdout)
        .expect("first create must return one ORV reference");
    let first_stored =
        decode_reader_values(&machine, read_probes, "orna raw-call read_probes first")
            .expect("first read must decode");
    assert_eq!(first_stored, vec![true], "the first row must store TRUE");

    // Replace the fixture with the complete expanded source and apply it.
    machine
        .write_fixture(&expanded)
        .expect("replace the fixture with the expanded source");
    let expanded_apply = machine
        .run_as_orna(&["source", "apply", FIXTURE_PATH])
        .expect("run installed source apply on the expanded fixture");
    let expanded_apply = require_success("orna source apply expanded", expanded_apply)
        .expect("expanded source apply must succeed");
    assert!(
        expanded_apply.stderr.is_empty(),
        "expanded source apply must keep standard error empty"
    );
    let expanded_document = parse_apply_document(&expanded_apply.stdout)
        .expect("expanded source apply JSON must parse");
    let expanded_order = [
        vec!["product_test".to_string(), "create_probe".to_string()],
        vec![
            "product_test".to_string(),
            "create_probe_with_added".to_string(),
        ],
        vec!["product_test".to_string(), "read_added".to_string()],
        vec!["product_test".to_string(), "read_probes".to_string()],
    ];
    let actual_expanded_order = expanded_document
        .functions
        .iter()
        .map(|function| function.names().to_vec())
        .collect::<Vec<_>>();
    assert_eq!(
        actual_expanded_order, expanded_order,
        "the expanded apply must report the four function entries sorted by qualified name"
    );
    let expanded_create_probe = expanded_document
        .function_id(&["product_test", "create_probe"])
        .expect("expanded apply must report create_probe");
    let expanded_read_probes = expanded_document
        .function_id(&["product_test", "read_probes"])
        .expect("expanded apply must report read_probes");
    assert_eq!(
        expanded_create_probe, create_probe,
        "create_probe identity must be stable across the field addition"
    );
    assert_eq!(
        expanded_read_probes, read_probes,
        "read_probes identity must be stable across the field addition"
    );
    let create_probe_with_added = expanded_document
        .function_id(&["product_test", "create_probe_with_added"])
        .expect("expanded apply must report create_probe_with_added");
    let read_added = expanded_document
        .function_id(&["product_test", "read_added"])
        .expect("expanded apply must report read_added");
    for (left, right) in [
        (create_probe_with_added, read_added),
        (create_probe_with_added, create_probe),
        (create_probe_with_added, read_probes),
        (read_added, create_probe),
        (read_added, read_probes),
    ] {
        assert_ne!(
            left, right,
            "the new identities must be pairwise distinct and distinct from both originals"
        );
    }

    // Exactly the new creator declares the canonical p_added parameter; every
    // other function declares none.
    let p_added = expanded_document
        .parameter_id(&["product_test", "create_probe_with_added"], "p_added")
        .expect("expanded apply must report create_probe_with_added.p_added");
    for names in [
        ["product_test", "create_probe"],
        ["product_test", "read_added"],
        ["product_test", "read_probes"],
    ] {
        let entry = expanded_document
            .functions
            .iter()
            .find(|function| {
                function
                    .names()
                    .iter()
                    .map(String::as_str)
                    .eq(names.iter().copied())
            })
            .expect("expanded apply must report the unchanged entry");
        assert!(
            entry.parameters().is_empty(),
            "the unchanged functions must declare no parameters"
        );
    }
    let added_entry = expanded_document
        .functions
        .iter()
        .find(|function| {
            function.names().iter().map(String::as_str).eq([
                "product_test",
                "create_probe_with_added",
            ]
            .iter()
            .copied())
        })
        .expect("expanded apply must report create_probe_with_added");
    assert_eq!(
        added_entry.parameters().len(),
        1,
        "the new creator must declare exactly one parameter"
    );
    assert_eq!(
        added_entry.parameters()[0].name(),
        "p_added",
        "the new creator must declare exactly the p_added parameter"
    );
    assert_eq!(
        added_entry.parameters()[0].parameter_id(),
        p_added,
        "the declared p_added identity must equal the discovered identity"
    );

    // The new functions are denied before their explicit grants.
    for function in [create_probe_with_added, read_added] {
        let denied = machine
            .run_as_orna(&["raw-call", function])
            .expect("run denied new raw call");
        assert_denied("new raw call before grant", denied).expect("new raw call must be denied");
    }
    for function in [create_probe_with_added, read_added] {
        let granted = machine
            .run_as_orna(&["security", "grant-execute", function])
            .expect("run installed grant command for the new function");
        require_silent_success("orna security grant-execute new", granted)
            .expect("new grant must succeed silently");
    }

    // The pre-transition row reads as one typed NULL.
    let mut added_before = run_reader_and_decode(
        &machine,
        read_added,
        "orna raw-call read_added first",
        |bytes| decode_added(bytes),
        "nullable scalar envelopes",
    )
    .expect("first added read must decode");
    added_before.sort();
    assert_eq!(
        added_before,
        vec![None],
        "the pre-transition row must read as a typed NULL"
    );

    // The unchanged old creator after the transition yields another NULL.
    let second_call = machine
        .run_as_orna(&["raw-call", create_probe])
        .expect("run unchanged old creator call");
    let second_call = require_value_success("orna raw-call create_probe second", second_call)
        .expect("old creator must succeed after the transition");
    let second_probe = parse_reference_envelope(&second_call.stdout)
        .expect("second create must return one ORV reference");
    let mut added_two = run_reader_and_decode(
        &machine,
        read_added,
        "orna raw-call read_added two",
        |bytes| decode_added(bytes),
        "nullable scalar envelopes",
    )
    .expect("two-row added read must decode");
    added_two.sort();
    assert_eq!(
        added_two,
        vec![None, None],
        "the old creator's omitted field must read as NULL"
    );

    // The new creator yields its explicit scalar value.
    let third_call = machine
        .run_as_orna_with_stdin(
            &["raw-call", create_probe_with_added, p_added],
            &explicit_envelope,
        )
        .expect("run new creator call");
    let third_call = require_value_success("orna raw-call create_probe_with_added", third_call)
        .expect("new creator must succeed");
    let third_probe = parse_reference_envelope(&third_call.stdout)
        .expect("new creator must return one ORV reference");
    let mut added_three = run_reader_and_decode(
        &machine,
        read_added,
        "orna raw-call read_added three",
        |bytes| decode_added(bytes),
        "nullable scalar envelopes",
    )
    .expect("three-row added read must decode");
    added_three.sort();
    assert_eq!(
        added_three,
        vec![None, None, Some(explicit_value.clone())],
        "the new creator must store its explicit scalar"
    );
    let mut stored_three =
        decode_reader_values(&machine, read_probes, "orna raw-call read_probes three")
            .expect("three-row stored read must decode");
    stored_three.sort();
    assert_eq!(
        stored_three,
        vec![true, true, true],
        "all three rows must store TRUE"
    );

    // All three references share one nonzero probe target type and distinct
    // nonzero object identities.
    assert!(
        first_probe.type_id != [0; 16]
            && second_probe.type_id != [0; 16]
            && third_probe.type_id != [0; 16],
        "all three probes must name the real nonzero target type"
    );
    assert!(
        !first_probe.object_is_zero()
            && !second_probe.object_is_zero()
            && !third_probe.object_is_zero(),
        "all three probes must name real nonzero rows"
    );
    assert_eq!(
        first_probe.type_id, second_probe.type_id,
        "the first and second probes must share the target type"
    );
    assert_eq!(
        second_probe.type_id, third_probe.type_id,
        "the second and third probes must share the target type"
    );
    assert_ne!(
        first_probe.object, second_probe.object,
        "the first and second probes must be distinct objects"
    );
    assert_ne!(
        second_probe.object, third_probe.object,
        "the second and third probes must be distinct objects"
    );
    assert_ne!(
        first_probe.object, third_probe.object,
        "the first and third probes must be distinct objects"
    );

    // Exact expanded-source replay without regrant keeps the complete
    // discovery vector and every value.
    let replay = machine
        .run_as_orna(&["source", "apply", FIXTURE_PATH])
        .expect("run installed source apply on the expanded fixture");
    let replay =
        require_success("orna source apply replay", replay).expect("expanded replay must succeed");
    assert!(
        replay.stderr.is_empty(),
        "expanded replay must keep standard error empty"
    );
    let replay_document =
        parse_apply_document(&replay.stdout).expect("expanded replay JSON must parse");
    assert_eq!(
        replay_document.functions, expanded_document.functions,
        "the replay must keep the complete function and parameter discovery vector"
    );
    assert_eq!(
        replay_document
            .parameter_id(&["product_test", "create_probe_with_added"], "p_added")
            .expect("the replay must keep create_probe_with_added.p_added"),
        p_added,
        "the replay must keep the exact canonical parameter identity"
    );
    let mut added_replay = run_reader_and_decode(
        &machine,
        read_added,
        "orna raw-call read_added after replay",
        |bytes| decode_added(bytes),
        "nullable scalar envelopes",
    )
    .expect("post-replay added read must decode");
    added_replay.sort();
    assert_eq!(
        added_replay,
        vec![None, None, Some(explicit_value.clone())],
        "the replay must preserve the added multiset"
    );
    let mut stored_replay = decode_reader_values(
        &machine,
        read_probes,
        "orna raw-call read_probes after replay",
    )
    .expect("post-replay stored read must decode");
    stored_replay.sort();
    assert_eq!(
        stored_replay,
        vec![true, true, true],
        "the replay must preserve the stored values"
    );

    // A restart preserves the unordered multiset and every callable grant.
    machine
        .restart_server()
        .expect("installed server must restart cleanly");
    let mut added_restart = run_reader_and_decode(
        &machine,
        read_added,
        "orna raw-call read_added after restart",
        |bytes| decode_added(bytes),
        "nullable scalar envelopes",
    )
    .expect("post-restart added read must decode");
    added_restart.sort();
    assert_eq!(
        added_restart,
        vec![None, None, Some(explicit_value.clone())],
        "the restart must preserve the added multiset"
    );
    let mut stored_restart = decode_reader_values(
        &machine,
        read_probes,
        "orna raw-call read_probes after restart",
    )
    .expect("post-restart stored read must decode");
    stored_restart.sort();
    assert_eq!(
        stored_restart,
        vec![true, true, true],
        "the restart must preserve the stored values"
    );

    // Every callable grant survives the restart: both creators still work
    // and the readers follow the evolving multiset.
    let fourth_call = machine
        .run_as_orna(&["raw-call", create_probe])
        .expect("run old creator after restart");
    require_value_success("orna raw-call create_probe after restart", fourth_call)
        .expect("old creator must succeed after restart");
    let mut added_four = run_reader_and_decode(
        &machine,
        read_added,
        "orna raw-call read_added four",
        |bytes| decode_added(bytes),
        "nullable scalar envelopes",
    )
    .expect("four-row added read must decode");
    added_four.sort();
    assert_eq!(
        added_four,
        vec![None, None, None, Some(explicit_value.clone())],
        "the old creator grant must stay callable"
    );
    let fifth_call = machine
        .run_as_orna_with_stdin(
            &["raw-call", create_probe_with_added, p_added],
            &explicit_envelope,
        )
        .expect("run new creator after restart");
    require_value_success(
        "orna raw-call create_probe_with_added after restart",
        fifth_call,
    )
    .expect("new creator must succeed after restart");
    let mut added_five = run_reader_and_decode(
        &machine,
        read_added,
        "orna raw-call read_added five",
        |bytes| decode_added(bytes),
        "nullable scalar envelopes",
    )
    .expect("five-row added read must decode");
    added_five.sort();
    assert_eq!(
        added_five,
        vec![
            None,
            None,
            None,
            Some(explicit_value.clone()),
            Some(explicit_value.clone()),
        ],
        "the new creator grant must stay callable"
    );
    let mut stored_five =
        decode_reader_values(&machine, read_probes, "orna raw-call read_probes five")
            .expect("five-row stored read must decode");
    stored_five.sort();
    assert_eq!(
        stored_five,
        vec![true, true, true, true, true],
        "all five rows must store TRUE"
    );

    Ok(())
}

/// Prove that adding a nullable INTEGER field keeps the pre-transition row
/// as a typed NULL and binds the exact `i32::MIN` argument through the
/// installed product's public apply, grant, raw-call, replay, and restart
/// path.
#[test]
#[ignore = "requires Docker, ORNA_SYSTEM_TEST_DEBIAN_PACKAGE, and the installed orna executable"]
fn installed_added_nullable_integer_field_survives_live_rows_grants_replay_and_restart() {
    run_added_nullable_scalar_journey(
        "product_test_added_nullable_integer.orna",
        integer_orv1_envelope(i32::MIN),
        decode_nullable_integer_envelopes,
        i32::MIN,
    )
    .expect("the added nullable INTEGER journey must pass");
}

/// Prove that adding a nullable TEXT field keeps the pre-transition row as a
/// typed NULL and binds the exact `EXACT_TEXT` argument through the installed
/// product's public apply, grant, raw-call, replay, and restart path.
#[test]
#[ignore = "requires Docker, ORNA_SYSTEM_TEST_DEBIAN_PACKAGE, and the installed orna executable"]
fn installed_added_nullable_text_field_survives_live_rows_grants_replay_and_restart() {
    run_added_nullable_scalar_journey(
        "product_test_added_nullable_text.orna",
        text_orv1_envelope(EXACT_TEXT),
        decode_nullable_text_envelopes,
        EXACT_TEXT.to_string(),
    )
    .expect("the added nullable TEXT journey must pass");
}

/// Decode a complete row-major stream of canonical ORV1 Integer pairs.
///
/// Each row must contain exactly two standard INTEGER envelopes. The pairing
/// remains part of the result, so callers can verify identity binding without
/// assuming a database row order.
fn decode_integer_pair_envelopes(bytes: &[u8]) -> Option<Vec<(i32, i32)>> {
    const INTEGER_ENVELOPE_LENGTH: usize = 29;
    const PAIR_LENGTH: usize = INTEGER_ENVELOPE_LENGTH * 2;
    if !bytes.len().is_multiple_of(PAIR_LENGTH) {
        return None;
    }
    bytes
        .chunks_exact(PAIR_LENGTH)
        .map(|row| {
            let first = decode_integer_envelopes(&row[..INTEGER_ENVELOPE_LENGTH])?
                .into_iter()
                .next()?;
            let second = decode_integer_envelopes(&row[INTEGER_ENVELOPE_LENGTH..])?
                .into_iter()
                .next()?;
            Some((first, second))
        })
        .collect()
}

/// Decode a complete row-major stream of canonical ORV1 Text and Reference
/// pairs, retaining the association between cells from the same row.
fn decode_text_reference_pair_envelopes(bytes: &[u8]) -> Option<Vec<(String, OrvReference)>> {
    let mut pairs = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let text_header = bytes.get(offset..offset + 25)?;
        if &text_header[..4] != b"ORV1"
            || text_header[4] != 0x06
            || text_header[5..20] != [0; 15]
            || text_header[20] != 0x06
        {
            return None;
        }
        let text_length = u32::from_be_bytes(text_header[21..25].try_into().ok()?) as usize;
        let text_end = offset.checked_add(25 + text_length)?;
        let text = String::from_utf8(bytes.get(offset + 25..text_end)?.to_vec()).ok()?;
        let reference_end = text_end.checked_add(41)?;
        let reference = parse_reference_envelope(bytes.get(text_end..reference_end)?).ok()?;
        pairs.push((text, reference));
        offset = reference_end;
    }
    Some(pairs)
}

/// Decode exactly one row from the ADR 0050 public probe reader.
///
/// The row is a Reference selector, one Text cell, and one nullable Reference
/// cell. This rejects a wrong tag, nominal type, length, malformed UTF-8, or
/// any trailing value.
fn decode_reference_value_update_probe(
    bytes: &[u8],
) -> Option<(OrvReference, String, OrvReferenceOrNull)> {
    let probe = parse_reference_envelope(bytes.get(..41)?).ok()?;
    let text_header = bytes.get(41..66)?;
    if &text_header[..4] != b"ORV1"
        || text_header[4] != 0x06
        || text_header[5..20] != [0; 15]
        || text_header[20] != 0x06
    {
        return None;
    }
    let text_length = u32::from_be_bytes(text_header[21..25].try_into().ok()?) as usize;
    let text_end = 66usize.checked_add(text_length)?;
    let text = String::from_utf8(bytes.get(66..text_end)?.to_vec()).ok()?;
    let mut linked_values = decode_reference_or_null_envelopes(bytes.get(text_end..)?)?.into_iter();
    let linked = linked_values.next()?;
    if linked_values.next().is_some() {
        return None;
    }
    Some((probe, text, linked))
}

/// Prove ADR 0049 through the installed public raw-call product surface.
///
/// The journey discovers exact function and ParameterId tokens from source
/// apply, proves both two-argument creators are denied before grants, then
/// proves identity binding for same-type Integer and Text/Reference pairs with
/// public identity-selected multi-column readers. It replays the exact fixture
/// without regranting and restarts before reusing the original identities and
/// grants.
#[test]
#[ignore = "requires Docker, ORNA_SYSTEM_TEST_DEBIAN_PACKAGE, and the ADR 0049 argument-pair commands in the installed orna executable"]
fn installed_argument_pairs_bind_by_identity_across_replay_and_restart() {
    let package = std::env::var("ORNA_SYSTEM_TEST_DEBIAN_PACKAGE")
        .expect("ORNA_SYSTEM_TEST_DEBIAN_PACKAGE must point at the reproduced .deb package");
    let artifact = FrozenPackageArtifact::new(PackageFormat::Debian, &package)
        .expect("freeze the reproduced Debian package");
    let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("product_test_argument_pairs.orna");
    let fixture = fs::read(&fixture_path).expect("read the checked-in argument-pair fixture");
    let machine = InstalledMachine::start(&artifact, &fixture)
        .expect("start the installed Debian test machine");

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
            "argument_pairs_test".to_string(),
            "create_anchor".to_string(),
        ],
        vec![
            "argument_pairs_test".to_string(),
            "create_int_pair".to_string(),
        ],
        vec![
            "argument_pairs_test".to_string(),
            "create_text_anchor_pair".to_string(),
        ],
        vec![
            "argument_pairs_test".to_string(),
            "read_int_first_values".to_string(),
        ],
        vec![
            "argument_pairs_test".to_string(),
            "read_int_pair".to_string(),
        ],
        vec![
            "argument_pairs_test".to_string(),
            "read_text_anchor_pair".to_string(),
        ],
        vec![
            "argument_pairs_test".to_string(),
            "read_text_messages".to_string(),
        ],
    ];
    assert_eq!(
        document
            .functions
            .iter()
            .map(|entry| entry.names().to_vec())
            .collect::<Vec<_>>(),
        expected_order,
        "apply must report all argument-pair functions in canonical name order"
    );

    let create_anchor = document
        .function_id(&["argument_pairs_test", "create_anchor"])
        .expect("apply must report create_anchor");
    let create_int_pair = document
        .function_id(&["argument_pairs_test", "create_int_pair"])
        .expect("apply must report create_int_pair");
    let create_text_anchor_pair = document
        .function_id(&["argument_pairs_test", "create_text_anchor_pair"])
        .expect("apply must report create_text_anchor_pair");
    let read_int_pair = document
        .function_id(&["argument_pairs_test", "read_int_pair"])
        .expect("apply must report read_int_pair");
    let read_int_first_values = document
        .function_id(&["argument_pairs_test", "read_int_first_values"])
        .expect("apply must report read_int_first_values");
    let read_text_anchor_pair = document
        .function_id(&["argument_pairs_test", "read_text_anchor_pair"])
        .expect("apply must report read_text_anchor_pair");
    let read_text_messages = document
        .function_id(&["argument_pairs_test", "read_text_messages"])
        .expect("apply must report read_text_messages");
    let assert_parameter_names = |function: &[&str], expected: &[&str]| {
        let entry = document
            .functions
            .iter()
            .find(|entry| {
                entry
                    .names()
                    .iter()
                    .map(String::as_str)
                    .eq(function.iter().copied())
            })
            .expect("apply must report the function entry");
        let actual = entry
            .parameters()
            .iter()
            .map(|parameter| parameter.name())
            .collect::<Vec<_>>();
        assert_eq!(
            actual, expected,
            "each function must report its complete ordered parameter declaration"
        );
    };
    for (function, parameters) in [
        (&["argument_pairs_test", "create_anchor"][..], &[][..]),
        (
            &["argument_pairs_test", "create_int_pair"][..],
            &["p_first", "p_second"][..],
        ),
        (
            &["argument_pairs_test", "create_text_anchor_pair"][..],
            &["p_message", "p_anchor"][..],
        ),
        (
            &["argument_pairs_test", "read_int_first_values"][..],
            &[][..],
        ),
        (
            &["argument_pairs_test", "read_int_pair"][..],
            &["p_pair"][..],
        ),
        (
            &["argument_pairs_test", "read_text_anchor_pair"][..],
            &["p_pair"][..],
        ),
        (&["argument_pairs_test", "read_text_messages"][..], &[][..]),
    ] {
        assert_parameter_names(function, parameters);
    }
    let p_first = document
        .parameter_id(&["argument_pairs_test", "create_int_pair"], "p_first")
        .expect("apply must report create_int_pair.p_first");
    let p_second = document
        .parameter_id(&["argument_pairs_test", "create_int_pair"], "p_second")
        .expect("apply must report create_int_pair.p_second");
    let p_message = document
        .parameter_id(
            &["argument_pairs_test", "create_text_anchor_pair"],
            "p_message",
        )
        .expect("apply must report create_text_anchor_pair.p_message");
    let p_anchor = document
        .parameter_id(
            &["argument_pairs_test", "create_text_anchor_pair"],
            "p_anchor",
        )
        .expect("apply must report create_text_anchor_pair.p_anchor");
    let p_int_pair = document
        .parameter_id(&["argument_pairs_test", "read_int_pair"], "p_pair")
        .expect("apply must report read_int_pair.p_pair");
    let p_text_anchor_pair = document
        .parameter_id(&["argument_pairs_test", "read_text_anchor_pair"], "p_pair")
        .expect("apply must report read_text_anchor_pair.p_pair");
    let parameter_ids = [
        p_first,
        p_second,
        p_message,
        p_anchor,
        p_int_pair,
        p_text_anchor_pair,
    ];
    for (index, left) in parameter_ids.iter().enumerate() {
        for right in &parameter_ids[index + 1..] {
            assert_ne!(left, right, "every discovered ParameterId must be distinct");
        }
    }

    let denied_int_input = [integer_orv1_envelope(-19), integer_orv1_envelope(73)].concat();
    let denied_int = machine
        .run_as_orna_with_stdin(
            &["raw-call", create_int_pair, p_first, p_second],
            &denied_int_input,
        )
        .expect("run denied Integer-pair creator");
    assert_denied("Integer-pair creator before grant", denied_int)
        .expect("Integer-pair creator must be denied before grant");
    let denied_text_input = [
        text_orv1_envelope("denied"),
        reference_orv1_envelope([0x11; 16], [0x22; 16]),
    ]
    .concat();
    let denied_text = machine
        .run_as_orna_with_stdin(
            &["raw-call", create_text_anchor_pair, p_message, p_anchor],
            &denied_text_input,
        )
        .expect("run denied Text/Reference-pair creator");
    assert_denied("Text/Reference-pair creator before grant", denied_text)
        .expect("Text/Reference-pair creator must be denied before grant");

    for reader in [
        read_int_pair,
        read_int_first_values,
        read_text_anchor_pair,
        read_text_messages,
    ] {
        let granted = machine
            .run_as_orna(&["security", "grant-execute", reader])
            .expect("run installed reader grant command");
        require_silent_success("orna security grant-execute reader", granted)
            .expect("reader grant must succeed silently");
    }
    for reader in [read_int_first_values, read_text_messages] {
        let empty = machine
            .run_as_orna(&["raw-call", reader])
            .expect("run empty public reader after denied creators");
        require_silent_success("orna raw-call empty reader after denied creators", empty)
            .expect("denied creators must leave the public reader empty");
    }
    for writer in [create_anchor, create_int_pair, create_text_anchor_pair] {
        let granted = machine
            .run_as_orna(&["security", "grant-execute", writer])
            .expect("run installed writer grant command");
        require_silent_success("orna security grant-execute writer", granted)
            .expect("writer grant must succeed silently");
    }

    let first_int = machine
        .run_as_orna_with_stdin(
            &["raw-call", create_int_pair, p_first, p_second],
            &[integer_orv1_envelope(-19), integer_orv1_envelope(73)].concat(),
        )
        .expect("run first Integer-pair creator");
    let first_int = require_value_success("orna raw-call create_int_pair first", first_int)
        .expect("first Integer-pair create must succeed");
    let first_int_reference = parse_reference_envelope(&first_int.stdout)
        .expect("Integer-pair create must return one reference");
    assert!(
        !first_int_reference.object_is_zero(),
        "Integer-pair create must return a real row"
    );
    let reversed_int = machine
        .run_as_orna_with_stdin(
            &["raw-call", create_int_pair, p_second, p_first],
            &[integer_orv1_envelope(-404), integer_orv1_envelope(909)].concat(),
        )
        .expect("run reversed Integer-pair creator");
    let reversed_int =
        require_value_success("orna raw-call create_int_pair reversed", reversed_int)
            .expect("reversed Integer-pair create must succeed");
    let reversed_int_reference = parse_reference_envelope(&reversed_int.stdout)
        .expect("reversed Integer-pair create must return one reference");

    let anchor_a = machine
        .run_as_orna(&["raw-call", create_anchor])
        .expect("create anchor A");
    let anchor_a = parse_reference_envelope(
        &require_value_success("orna raw-call create_anchor A", anchor_a)
            .expect("anchor A create must succeed")
            .stdout,
    )
    .expect("anchor A create must return one reference");
    let anchor_b = machine
        .run_as_orna(&["raw-call", create_anchor])
        .expect("create anchor B");
    let anchor_b = parse_reference_envelope(
        &require_value_success("orna raw-call create_anchor B", anchor_b)
            .expect("anchor B create must succeed")
            .stdout,
    )
    .expect("anchor B create must return one reference");
    assert_eq!(
        anchor_a.type_id, anchor_b.type_id,
        "anchors must have one target type"
    );
    assert_ne!(
        anchor_a.object, anchor_b.object,
        "anchors must be distinct rows"
    );

    let first_text = machine
        .run_as_orna_with_stdin(
            &["raw-call", create_text_anchor_pair, p_message, p_anchor],
            &[
                text_orv1_envelope("first anchor"),
                reference_orv1_envelope(anchor_a.type_id, anchor_a.object),
            ]
            .concat(),
        )
        .expect("run first Text/Reference-pair creator");
    let first_text =
        require_value_success("orna raw-call create_text_anchor_pair first", first_text)
            .expect("first Text/Reference-pair create must succeed");
    let first_text_reference = parse_reference_envelope(&first_text.stdout)
        .expect("Text/Reference-pair create must return one reference");
    let reversed_text = machine
        .run_as_orna_with_stdin(
            &["raw-call", create_text_anchor_pair, p_anchor, p_message],
            &[
                reference_orv1_envelope(anchor_b.type_id, anchor_b.object),
                text_orv1_envelope("second anchor"),
            ]
            .concat(),
        )
        .expect("run reversed Text/Reference-pair creator");
    let reversed_text = require_value_success(
        "orna raw-call create_text_anchor_pair reversed",
        reversed_text,
    )
    .expect("reversed Text/Reference-pair create must succeed");
    let reversed_text_reference = parse_reference_envelope(&reversed_text.stdout)
        .expect("reversed Text/Reference-pair create must return one reference");

    let assert_int_pair = |pair: &OrvReference, expected: (i32, i32), label: &'static str| {
        let output = machine
            .run_as_orna_with_stdin(
                &["raw-call", read_int_pair, p_int_pair],
                &reference_orv1_envelope(pair.type_id, pair.object),
            )
            .expect("run identity-selected Integer-pair reader");
        let output = require_value_success(label, output)
            .expect("identity-selected Integer-pair reader must succeed");
        assert_eq!(
            decode_integer_pair_envelopes(&output.stdout),
            Some(vec![expected]),
            "Integer reader must return one strict ORV1 pair for its selected row"
        );
    };
    let assert_text_pair = |pair: &OrvReference,
                            expected_message: &str,
                            expected_anchor: &OrvReference,
                            label: &'static str| {
        let output = machine
            .run_as_orna_with_stdin(
                &["raw-call", read_text_anchor_pair, p_text_anchor_pair],
                &reference_orv1_envelope(pair.type_id, pair.object),
            )
            .expect("run identity-selected Text/Reference-pair reader");
        let output = require_value_success(label, output)
            .expect("identity-selected Text/Reference-pair reader must succeed");
        let actual = decode_text_reference_pair_envelopes(&output.stdout)
            .expect("Text/Reference reader output must be one strict ORV1 pair");
        assert_eq!(
            actual.len(),
            1,
            "selected Text/Reference reader must return one row"
        );
        let (message, anchor) = &actual[0];
        assert_eq!(
            message, expected_message,
            "selected row must retain its Text value"
        );
        assert_eq!(
            anchor.type_id, expected_anchor.type_id,
            "selected row must retain anchor type"
        );
        assert_eq!(
            anchor.object, expected_anchor.object,
            "selected row must retain its associated anchor"
        );
    };
    assert_int_pair(&first_int_reference, (-19, 73), "read first Integer pair");
    assert_int_pair(
        &reversed_int_reference,
        (909, -404),
        "read reversed Integer pair",
    );
    assert_text_pair(
        &first_text_reference,
        "first anchor",
        &anchor_a,
        "read first Text/Reference pair",
    );
    assert_text_pair(
        &reversed_text_reference,
        "second anchor",
        &anchor_b,
        "read reversed Text/Reference pair",
    );

    let replay = machine
        .run_as_orna(&["source", "apply", FIXTURE_PATH])
        .expect("replay the installed argument-pair fixture");
    let replay =
        require_success("orna source apply replay", replay).expect("fixture replay must succeed");
    assert!(
        replay.stderr.is_empty(),
        "fixture replay must keep standard error empty"
    );
    let replay_document = parse_apply_document(&replay.stdout).expect("replay JSON must parse");
    assert_eq!(
        replay_document.functions, document.functions,
        "replay must preserve all function and ParameterId identities"
    );
    assert_int_pair(
        &first_int_reference,
        (-19, 73),
        "read first Integer pair after replay",
    );
    assert_int_pair(
        &reversed_int_reference,
        (909, -404),
        "read reversed Integer pair after replay",
    );
    assert_text_pair(
        &first_text_reference,
        "first anchor",
        &anchor_a,
        "read first Text/Reference pair after replay",
    );
    assert_text_pair(
        &reversed_text_reference,
        "second anchor",
        &anchor_b,
        "read reversed Text/Reference pair after replay",
    );

    machine
        .restart_server()
        .expect("installed server must restart cleanly");
    assert_int_pair(
        &first_int_reference,
        (-19, 73),
        "read first Integer pair after restart",
    );
    assert_int_pair(
        &reversed_int_reference,
        (909, -404),
        "read reversed Integer pair after restart",
    );
    assert_text_pair(
        &first_text_reference,
        "first anchor",
        &anchor_a,
        "read first Text/Reference pair after restart",
    );
    assert_text_pair(
        &reversed_text_reference,
        "second anchor",
        &anchor_b,
        "read reversed Text/Reference pair after restart",
    );
    let after_restart_int = machine
        .run_as_orna_with_stdin(
            &["raw-call", create_int_pair, p_first, p_second],
            &[
                integer_orv1_envelope(i32::MIN),
                integer_orv1_envelope(i32::MAX),
            ]
            .concat(),
        )
        .expect("reuse original Integer-pair identities after restart");
    let after_restart_int = require_value_success(
        "orna raw-call create_int_pair after restart",
        after_restart_int,
    )
    .expect("original Integer-pair grant must survive restart");
    let after_restart_int_reference = parse_reference_envelope(&after_restart_int.stdout)
        .expect("post-restart Integer-pair create must return one reference");
    let after_restart_text = machine
        .run_as_orna_with_stdin(
            &["raw-call", create_text_anchor_pair, p_message, p_anchor],
            &[
                text_orv1_envelope("after restart"),
                reference_orv1_envelope(anchor_a.type_id, anchor_a.object),
            ]
            .concat(),
        )
        .expect("reuse original Text/Reference-pair identities after restart");
    let after_restart_text = require_value_success(
        "orna raw-call create_text_anchor_pair after restart",
        after_restart_text,
    )
    .expect("original Text/Reference-pair grant must survive restart");
    let after_restart_text_reference = parse_reference_envelope(&after_restart_text.stdout)
        .expect("post-restart Text/Reference-pair create must return one reference");
    assert_int_pair(
        &after_restart_int_reference,
        (i32::MIN, i32::MAX),
        "read post-restart Integer pair",
    );
    assert_text_pair(
        &after_restart_text_reference,
        "after restart",
        &anchor_a,
        "read post-restart Text/Reference pair",
    );
    let int_values = machine
        .run_as_orna(&["raw-call", read_int_first_values])
        .expect("run final Integer first-value reader");
    let int_values = require_value_success("orna raw-call read_int_first_values", int_values)
        .expect("Integer first-value reader must succeed");
    let mut int_values = decode_integer_envelopes(&int_values.stdout)
        .expect("Integer first-value reader output must be strict ORV1 Integers");
    int_values.sort_unstable();
    assert_eq!(
        int_values,
        vec![i32::MIN, -19, 909],
        "Integer first-value reader must return the exact unordered stored multiset"
    );
    let text_messages = machine
        .run_as_orna(&["raw-call", read_text_messages])
        .expect("run final Text message reader");
    let text_messages = require_value_success("orna raw-call read_text_messages", text_messages)
        .expect("Text message reader must succeed");
    let mut text_messages = decode_text_envelopes(&text_messages.stdout)
        .expect("Text message reader output must be strict ORV1 Text values");
    text_messages.sort_unstable();
    assert_eq!(
        text_messages,
        vec![
            "after restart".to_string(),
            "first anchor".to_string(),
            "second anchor".to_string(),
        ],
        "Text message reader must return the exact unordered stored multiset"
    );
}

/// Prove ADR 0050 through the installed public raw-call product surface.
///
/// The journey discovers both parameter identities of each value UPDATE,
/// denies every call before a grant, then uses the original identities to
/// update only selected rows. It uses reverse token and envelope order for a
/// Text value and a Reference value. Exact source replay and restart retain
/// the discovery, grants, references, and stored fields without a regrant.
#[test]
#[ignore = "requires Docker, ORNA_SYSTEM_TEST_DEBIAN_PACKAGE, and the ADR 0050 raw reference value update commands in the installed orna executable"]
fn installed_raw_reference_value_update_binds_by_identity_across_replay_and_restart() {
    let package = std::env::var("ORNA_SYSTEM_TEST_DEBIAN_PACKAGE")
        .expect("ORNA_SYSTEM_TEST_DEBIAN_PACKAGE must point at the reproduced .deb package");
    let artifact = FrozenPackageArtifact::new(PackageFormat::Debian, &package)
        .expect("freeze the reproduced Debian package");
    let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("product_test_reference_value_update.orna");
    let fixture = fs::read(&fixture_path).expect("read the checked-in value update fixture");
    let machine = InstalledMachine::start(&artifact, &fixture)
        .expect("start the installed Debian test machine");

    let apply = machine
        .run_as_orna(&["source", "apply", FIXTURE_PATH])
        .expect("apply the installed value update fixture");
    let apply = require_success("orna source apply", apply).expect("source apply must succeed");
    assert!(
        apply.stderr.is_empty(),
        "source apply must keep standard error empty"
    );
    let document = parse_apply_document(&apply.stdout).expect("source apply JSON must parse");
    let expected_order = [
        vec![
            "reference_value_update_test".to_string(),
            "create_anchor".to_string(),
        ],
        vec![
            "reference_value_update_test".to_string(),
            "create_probe".to_string(),
        ],
        vec![
            "reference_value_update_test".to_string(),
            "read_anchor".to_string(),
        ],
        vec![
            "reference_value_update_test".to_string(),
            "read_probe".to_string(),
        ],
        vec![
            "reference_value_update_test".to_string(),
            "update_probe_link".to_string(),
        ],
        vec![
            "reference_value_update_test".to_string(),
            "update_probe_stored".to_string(),
        ],
    ];
    assert_eq!(
        document
            .functions
            .iter()
            .map(|entry| entry.names().to_vec())
            .collect::<Vec<_>>(),
        expected_order,
        "apply must report every value-update function in canonical name order"
    );
    let create_anchor = document
        .function_id(&["reference_value_update_test", "create_anchor"])
        .expect("apply must report create_anchor");
    let create_probe = document
        .function_id(&["reference_value_update_test", "create_probe"])
        .expect("apply must report create_probe");
    let read_anchor = document
        .function_id(&["reference_value_update_test", "read_anchor"])
        .expect("apply must report read_anchor");
    let read_probe = document
        .function_id(&["reference_value_update_test", "read_probe"])
        .expect("apply must report read_probe");
    let update_probe_link = document
        .function_id(&["reference_value_update_test", "update_probe_link"])
        .expect("apply must report update_probe_link");
    let update_probe_stored = document
        .function_id(&["reference_value_update_test", "update_probe_stored"])
        .expect("apply must report update_probe_stored");
    let assert_parameter_names = |function: &[&str], expected: &[&str]| {
        let entry = document
            .functions
            .iter()
            .find(|entry| {
                entry
                    .names()
                    .iter()
                    .map(String::as_str)
                    .eq(function.iter().copied())
            })
            .expect("apply must report the function entry");
        let actual = entry
            .parameters()
            .iter()
            .map(|parameter| parameter.name())
            .collect::<Vec<_>>();
        assert_eq!(
            actual, expected,
            "each function must report its complete ordered parameter declaration"
        );
    };
    for (function, parameters) in [
        (
            &["reference_value_update_test", "create_anchor"][..],
            &[][..],
        ),
        (
            &["reference_value_update_test", "create_probe"][..],
            &["p_stored"][..],
        ),
        (
            &["reference_value_update_test", "read_anchor"][..],
            &["p_anchor"][..],
        ),
        (
            &["reference_value_update_test", "read_probe"][..],
            &["p_probe"][..],
        ),
        (
            &["reference_value_update_test", "update_probe_link"][..],
            &["p_anchor", "p_probe"][..],
        ),
        (
            &["reference_value_update_test", "update_probe_stored"][..],
            &["p_stored", "p_probe"][..],
        ),
    ] {
        assert_parameter_names(function, parameters);
    }
    let p_create_stored = document
        .parameter_id(&["reference_value_update_test", "create_probe"], "p_stored")
        .expect("apply must report create_probe.p_stored");
    let p_read_anchor = document
        .parameter_id(&["reference_value_update_test", "read_anchor"], "p_anchor")
        .expect("apply must report read_anchor.p_anchor");
    let p_read_probe = document
        .parameter_id(&["reference_value_update_test", "read_probe"], "p_probe")
        .expect("apply must report read_probe.p_probe");
    let p_link_anchor = document
        .parameter_id(
            &["reference_value_update_test", "update_probe_link"],
            "p_anchor",
        )
        .expect("apply must report update_probe_link.p_anchor");
    let p_link_probe = document
        .parameter_id(
            &["reference_value_update_test", "update_probe_link"],
            "p_probe",
        )
        .expect("apply must report update_probe_link.p_probe");
    let p_stored_value = document
        .parameter_id(
            &["reference_value_update_test", "update_probe_stored"],
            "p_stored",
        )
        .expect("apply must report update_probe_stored.p_stored");
    let p_stored_probe = document
        .parameter_id(
            &["reference_value_update_test", "update_probe_stored"],
            "p_probe",
        )
        .expect("apply must report update_probe_stored.p_probe");
    let parameter_ids = [
        p_create_stored,
        p_read_anchor,
        p_read_probe,
        p_link_anchor,
        p_link_probe,
        p_stored_value,
        p_stored_probe,
    ];
    for (index, left) in parameter_ids.iter().enumerate() {
        for right in &parameter_ids[index + 1..] {
            assert_ne!(left, right, "every discovered ParameterId must be distinct");
        }
    }

    let denied_calls = [
        (create_anchor, Vec::new()),
        (create_probe, text_orv1_envelope("denied")),
        (read_anchor, reference_orv1_envelope([0x10; 16], [0x11; 16])),
        (read_probe, reference_orv1_envelope([0x12; 16], [0x13; 16])),
        (
            update_probe_link,
            [
                reference_orv1_envelope([0x14; 16], [0x15; 16]),
                reference_orv1_envelope([0x16; 16], [0x17; 16]),
            ]
            .concat(),
        ),
        (
            update_probe_stored,
            [
                text_orv1_envelope("denied"),
                reference_orv1_envelope([0x18; 16], [0x19; 16]),
            ]
            .concat(),
        ),
    ];
    for (function, input) in denied_calls {
        let denied = if input.is_empty() {
            machine.run_as_orna(&["raw-call", function])
        } else if function == create_probe {
            machine.run_as_orna_with_stdin(&["raw-call", function, p_create_stored], &input)
        } else if function == read_anchor {
            machine.run_as_orna_with_stdin(&["raw-call", function, p_read_anchor], &input)
        } else if function == read_probe {
            machine.run_as_orna_with_stdin(&["raw-call", function, p_read_probe], &input)
        } else if function == update_probe_link {
            machine.run_as_orna_with_stdin(
                &["raw-call", function, p_link_anchor, p_link_probe],
                &input,
            )
        } else {
            machine.run_as_orna_with_stdin(
                &["raw-call", function, p_stored_value, p_stored_probe],
                &input,
            )
        }
        .expect("run denied raw call");
        assert_denied("raw call before grant", denied).expect("raw call must be denied");
    }

    for function in [create_anchor, create_probe, read_anchor, read_probe] {
        let granted = machine
            .run_as_orna(&["security", "grant-execute", function])
            .expect("grant installed value-update function");
        require_silent_success("orna security grant-execute", granted)
            .expect("grant must succeed silently");
    }

    let create_probe_with_text = |text: &str, label: &'static str| {
        let output = machine
            .run_as_orna_with_stdin(
                &["raw-call", create_probe, p_create_stored],
                &text_orv1_envelope(text),
            )
            .expect("create probe with caller Text");
        parse_reference_envelope(
            &require_value_success(label, output)
                .expect("probe creation must succeed")
                .stdout,
        )
        .expect("probe creation must return one canonical Reference")
    };
    let first_probe = create_probe_with_text("first", "orna raw-call create_probe first");
    let second_probe = create_probe_with_text("second", "orna raw-call create_probe second");
    assert_eq!(
        first_probe.type_id, second_probe.type_id,
        "probes must have one target type"
    );
    assert_ne!(
        first_probe.object, second_probe.object,
        "probes must be distinct rows"
    );
    let create_anchor_reference = |label: &'static str| {
        let output = machine
            .run_as_orna(&["raw-call", create_anchor])
            .expect("create anchor");
        parse_reference_envelope(
            &require_value_success(label, output)
                .expect("anchor creation must succeed")
                .stdout,
        )
        .expect("anchor creation must return one canonical Reference")
    };
    let first_anchor = create_anchor_reference("orna raw-call create_anchor first");
    let second_anchor = create_anchor_reference("orna raw-call create_anchor second");
    assert_eq!(
        first_anchor.type_id, second_anchor.type_id,
        "anchors must have one target type"
    );
    assert_ne!(
        first_anchor.object, second_anchor.object,
        "anchors must be distinct rows"
    );

    let denied_update = machine
        .run_as_orna_with_stdin(
            &[
                "raw-call",
                update_probe_stored,
                p_stored_value,
                p_stored_probe,
            ],
            &[
                text_orv1_envelope("must not store"),
                reference_orv1_envelope(first_probe.type_id, first_probe.object),
            ]
            .concat(),
        )
        .expect("run denied scalar update after creating probes");
    assert_denied("scalar update before grant", denied_update)
        .expect("ungranted scalar update must be denied");
    for (probe, expected_stored) in [(&first_probe, "first"), (&second_probe, "second")] {
        let output = machine
            .run_as_orna_with_stdin(
                &["raw-call", read_probe, p_read_probe],
                &reference_orv1_envelope(probe.type_id, probe.object),
            )
            .expect("read probe after denied update");
        let output = require_value_success("orna raw-call read_probe after denied update", output)
            .expect("reader grant must remain usable");
        let (selected, stored, linked) = decode_reference_value_update_probe(&output.stdout)
            .expect("denied-update reader output must be one strict ORV1 row");
        assert_eq!(
            selected.object, probe.object,
            "reader must retain the selected probe"
        );
        assert_eq!(
            stored, expected_stored,
            "denied update must leave the stored Text unchanged"
        );
        assert!(
            linked.reference.is_none(),
            "denied update must leave the nullable link unchanged"
        );
    }
    for function in [update_probe_link, update_probe_stored] {
        let granted = machine
            .run_as_orna(&["security", "grant-execute", function])
            .expect("grant installed value-update writer");
        require_silent_success("orna security grant-execute writer", granted)
            .expect("writer grant must succeed silently");
    }

    let updated_text = machine
        .run_as_orna_with_stdin(
            &[
                "raw-call",
                update_probe_stored,
                p_stored_probe,
                p_stored_value,
            ],
            &[
                reference_orv1_envelope(first_probe.type_id, first_probe.object),
                text_orv1_envelope("first updated"),
            ]
            .concat(),
        )
        .expect("update first probe with reverse Text parameter order");
    let updated_text = require_value_success("orna raw-call update_probe_stored", updated_text)
        .expect("Text update must succeed");
    assert_eq!(
        updated_text.stdout,
        reference_orv1_envelope(first_probe.type_id, first_probe.object),
        "Text update must return the exact selected probe Reference"
    );
    let updated_link = machine
        .run_as_orna_with_stdin(
            &["raw-call", update_probe_link, p_link_probe, p_link_anchor],
            &[
                reference_orv1_envelope(second_probe.type_id, second_probe.object),
                reference_orv1_envelope(first_anchor.type_id, first_anchor.object),
            ]
            .concat(),
        )
        .expect("update second probe with reverse Reference parameter order");
    let updated_link = require_value_success("orna raw-call update_probe_link", updated_link)
        .expect("Reference update must succeed");
    assert_eq!(
        updated_link.stdout,
        reference_orv1_envelope(second_probe.type_id, second_probe.object),
        "Reference update must return the exact selected probe Reference"
    );

    let assert_probe = |probe: &OrvReference,
                        expected_stored: &str,
                        expected_link: Option<&OrvReference>,
                        label: &'static str| {
        let output = machine
            .run_as_orna_with_stdin(
                &["raw-call", read_probe, p_read_probe],
                &reference_orv1_envelope(probe.type_id, probe.object),
            )
            .expect("read selected probe");
        let output = require_value_success(label, output).expect("probe reader must succeed");
        let (reference, stored, linked) = decode_reference_value_update_probe(&output.stdout)
            .expect("probe reader must emit one complete strict ORV1 row");
        assert_eq!(
            reference.type_id, probe.type_id,
            "reader must return its selected probe type"
        );
        assert_eq!(
            reference.object, probe.object,
            "reader must return its selected probe identity"
        );
        assert_eq!(
            stored, expected_stored,
            "reader must return the exact stored Text"
        );
        match expected_link {
            Some(expected) => {
                let actual = linked
                    .reference
                    .expect("reader must return the assigned Reference");
                assert_eq!(
                    actual.type_id, expected.type_id,
                    "reader must retain the assigned anchor type"
                );
                assert_eq!(
                    actual.object, expected.object,
                    "reader must retain the assigned anchor identity"
                );
            }
            None => {
                assert!(
                    linked.reference.is_none(),
                    "unassigned link must remain a typed NULL"
                );
                assert_eq!(
                    linked.nominal_type_id, first_anchor.type_id,
                    "typed NULL link must retain its anchor nominal type"
                );
            }
        }
    };
    assert_probe(
        &first_probe,
        "first updated",
        None,
        "read first probe after Text update",
    );
    assert_probe(
        &second_probe,
        "second",
        Some(&first_anchor),
        "read second probe after Reference update",
    );

    let absent_object = [[0xa5; 16], [0x5a; 16], [0x3c; 16]]
        .into_iter()
        .find(|candidate| *candidate != first_probe.object && *candidate != second_probe.object)
        .expect("one fixed absent selector candidate must differ from both observed probes");
    let absent = machine
        .run_as_orna_with_stdin(
            &[
                "raw-call",
                update_probe_stored,
                p_stored_value,
                p_stored_probe,
            ],
            &[
                text_orv1_envelope("absent must not create"),
                reference_orv1_envelope(first_probe.type_id, absent_object),
            ]
            .concat(),
        )
        .expect("update an absent probe");
    require_silent_success("orna raw-call update_probe_stored absent", absent)
        .expect("an absent selector must complete with no value");
    assert_probe(
        &first_probe,
        "first updated",
        None,
        "read first probe after absent update",
    );
    assert_probe(
        &second_probe,
        "second",
        Some(&first_anchor),
        "read second probe after absent update",
    );

    let replay = machine
        .run_as_orna(&["source", "apply", FIXTURE_PATH])
        .expect("replay installed value-update fixture");
    let replay =
        require_success("orna source apply replay", replay).expect("fixture replay must succeed");
    assert!(
        replay.stderr.is_empty(),
        "fixture replay must keep standard error empty"
    );
    let replay_document = parse_apply_document(&replay.stdout).expect("replay JSON must parse");
    assert_eq!(
        replay_document.functions, document.functions,
        "replay must preserve every function and ParameterId without regrant"
    );
    assert_probe(
        &first_probe,
        "first updated",
        None,
        "read first probe after replay",
    );
    assert_probe(
        &second_probe,
        "second",
        Some(&first_anchor),
        "read second probe after replay",
    );

    machine
        .restart_server()
        .expect("installed server must restart cleanly");
    assert_probe(
        &first_probe,
        "first updated",
        None,
        "read first probe after restart",
    );
    assert_probe(
        &second_probe,
        "second",
        Some(&first_anchor),
        "read second probe after restart",
    );
    let after_restart = machine
        .run_as_orna_with_stdin(
            &[
                "raw-call",
                update_probe_stored,
                p_stored_value,
                p_stored_probe,
            ],
            &[
                text_orv1_envelope("first after restart"),
                reference_orv1_envelope(first_probe.type_id, first_probe.object),
            ]
            .concat(),
        )
        .expect("reuse original value-update identities after restart");
    let after_restart = require_value_success(
        "orna raw-call update_probe_stored after restart",
        after_restart,
    )
    .expect("original value-update grant must survive restart");
    assert_eq!(
        after_restart.stdout,
        reference_orv1_envelope(first_probe.type_id, first_probe.object),
        "post-restart update must return the exact selector"
    );
    assert_probe(
        &first_probe,
        "first after restart",
        None,
        "read first probe after post-restart update",
    );
    assert_probe(
        &second_probe,
        "second",
        Some(&first_anchor),
        "read second probe after post-restart update",
    );
    let anchor = machine
        .run_as_orna_with_stdin(
            &["raw-call", read_anchor, p_read_anchor],
            &reference_orv1_envelope(second_anchor.type_id, second_anchor.object),
        )
        .expect("read unused anchor after restart");
    let anchor = require_value_success("orna raw-call read_anchor", anchor)
        .expect("original reader grant must survive restart");
    assert_eq!(
        anchor.stdout,
        [
            reference_orv1_envelope(second_anchor.type_id, second_anchor.object),
            boolean_orv1_envelope(Some(true))
        ]
        .concat(),
        "unselected anchor must remain readable with its exact stored value"
    );
}

/// Prove the installed public source-check/apply/grant/invoke journey for one
/// parameterised scalar SERVER function.
///
/// The checked-in fixture is applied through `/usr/bin/orna`, the exact
/// canonical function identity returned by apply is granted, and the function
/// is invoked by qualified name with a named INTEGER argument. Explicit JSON
/// output and `--no-progress` keep the streams deterministic: stdout must be
/// exactly the JSON scalar and stderr must remain empty.
#[test]
#[ignore = "requires Docker, ORNA_SYSTEM_TEST_DEBIAN_PACKAGE, and the installed orna executable"]
fn installed_scalar_server_invoke_returns_named_integer_as_json() {
    let package = std::env::var("ORNA_SYSTEM_TEST_DEBIAN_PACKAGE")
        .expect("ORNA_SYSTEM_TEST_DEBIAN_PACKAGE must point at the reproduced .deb package");
    let artifact = FrozenPackageArtifact::new(PackageFormat::Debian, &package)
        .expect("freeze the reproduced Debian package");
    let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("scalar_server_dogfood.orna");
    let fixture = fs::read(&fixture_path).expect("read the checked-in scalar SERVER fixture");

    let machine = InstalledMachine::start(&artifact, &fixture)
        .expect("start the installed Debian test machine");

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
        1,
        "source apply must report exactly the scalar SERVER function"
    );
    let function_id = document
        .function_id(&["scalar_server_dogfood", "echo"])
        .expect("source apply must report scalar_server_dogfood.echo")
        .to_owned();
    assert_canonical_identity(
        &function_id,
        "function:",
        "scalar_server_dogfood.echo function identity",
    )
    .expect("source apply must return a canonical function identity");
    let parameter_id = document
        .parameter_id(&["scalar_server_dogfood", "echo"], "p_value")
        .expect("source apply must report the p_value parameter");
    assert_canonical_identity(parameter_id, "parameter:", "scalar_server_dogfood.echo.p_value")
        .expect("source apply must return a canonical parameter identity");

    let granted = machine
        .run_as_orna(&["security", "grant-execute", function_id.as_str()])
        .expect("run installed grant command");
    require_silent_success("orna security grant-execute", granted)
        .expect("grant must succeed silently");

    let invoked = machine
        .run_as_orna(&[
            "invoke",
            "scalar_server_dogfood.echo",
            "--arg",
            "p_value=41",
            "--output",
            "json",
            "--no-progress",
        ])
        .expect("run installed scalar SERVER invoke");
    let invoked =
        require_success("orna invoke scalar_server_dogfood.echo", invoked)
            .expect("scalar SERVER invoke must succeed");
    assert_eq!(
        invoked.stdout, b"41",
        "JSON output must be exactly the invoked INTEGER scalar"
    );
    assert!(
        invoked.stderr.is_empty(),
        "no-progress JSON invoke must keep standard error empty"
    );
}
