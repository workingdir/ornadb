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

/// Require the exact closed denied outcome of a raw call.
///
/// A denied raw call exits 1, emits no value, and prints the exact
/// `raw call failed: EXECUTE_DENIED` line on standard error.
fn assert_denied(label: &'static str, output: Output) -> Result<(), Error> {
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
    if stderr != "raw call failed: EXECUTE_DENIED\n" {
        return Err(Error::Unexpected {
            message: format!("{label} must print the exact denied line, got {stderr:?}"),
        });
    }
    Ok(())
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
    if output.stdout != boolean_true_envelope() {
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

/// The canonical `ORV1` envelope for the exact Boolean value TRUE.
///
/// Layout: `ORV1`, the BOOLEAN tag, the 16-byte BOOLEAN type identity, the
/// 4-byte big-endian payload length, and one payload byte for TRUE.
fn boolean_true_envelope() -> Vec<u8> {
    let mut bytes = Vec::with_capacity(26);
    bytes.extend_from_slice(b"ORV1");
    bytes.push(0x02);
    bytes.extend_from_slice(&[0; 15]);
    bytes.push(0x01);
    bytes.extend_from_slice(&1_u32.to_be_bytes());
    bytes.push(0x01);
    bytes
}

/// Two exact canonical Boolean TRUE envelopes, one per stored row.
fn two_boolean_true_envelopes() -> Vec<u8> {
    let mut bytes = boolean_true_envelope();
    bytes.extend(boolean_true_envelope());
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

/// One parsed application function entry.
///
/// The first element is the exact qualified name parts, the second is the
/// canonical `function:<id>` identity.
type FunctionEntry = (Vec<String>, String);

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
            .find(|(parts, _)| parts.iter().map(String::as_str).eq(name.iter().copied()))
            .map(|(_, id)| id.as_str())
            .ok_or_else(|| Error::Unexpected {
                message: format!("source apply functions lack the qualified name {name:?}"),
            })
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
/// Each entry is
/// `{"qualified_name":["schema","function"],"function_id":"function:<id>"}`,
/// separated by commas and closed by `]`.
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
        rest = rest.strip_prefix('}').ok_or_else(|| Error::Unexpected {
            message: "source apply function entry is not closed".to_string(),
        })?;
        functions.push((names, id.to_string()));

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
