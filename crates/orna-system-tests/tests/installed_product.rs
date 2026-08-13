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
//! 8. restart the installed server and invoke the raw SELECT again to prove
//!    the row persists.
//!
//! The test is ignored by default so ordinary gates stay green. It is
//! expected to fail (RED) until the ADR 0038 commands exist in the installed
//! executable: `orna source apply`, `orna security grant-execute`, and the
//! parameter-free raw INSERT dispatch are not implemented at the ADR docs
//! commit. Run it explicitly with `ORNA_SYSTEM_TEST_DEBIAN_PACKAGE` pointing
//! at an absolute current `.deb`.

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

    /// Write the checked-in fixture into the container.
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

    /// The `docker exec` argument vector for one `orna` command as orna.
    fn setpriv_args(&self, command: &[&str]) -> Vec<String> {
        let mut args = vec![
            "--env".to_string(),
            "PATH=/usr/sbin:/usr/bin:/sbin:/bin".to_string(),
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

/// Require the exact canonical Boolean TRUE value envelope.
///
/// Returns the verified output bytes so the caller can compare exact
/// canonical output across a service restart.
fn assert_exact_boolean_true(label: &'static str, output: Output) -> Result<Vec<u8>, Error> {
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
    Ok(output.stdout)
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
/// * restart preserves the exact canonical SELECT output bytes.
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

    // Raw SELECT returns the exact canonical Boolean TRUE value.
    let before = machine
        .run_as_orna(&["raw-call", read_probes])
        .expect("run raw select call");
    let before_bytes = assert_exact_boolean_true("orna raw-call read_probes", before)
        .expect("raw select must return the exact Boolean TRUE value");

    // Restart the installed service and prove the row persists.
    machine
        .restart_server()
        .expect("installed server must restart cleanly");
    let after = machine
        .run_as_orna(&["raw-call", read_probes])
        .expect("run raw select call after restart");
    let after_bytes = assert_exact_boolean_true("orna raw-call read_probes after restart", after)
        .expect("raw select must return the exact Boolean TRUE value after restart");
    assert_eq!(
        after_bytes, before_bytes,
        "restart must preserve the exact canonical output bytes"
    );
}
