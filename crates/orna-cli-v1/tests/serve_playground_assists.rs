use std::{
    fs,
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::Duration,
};

const BINARY: &str = env!("CARGO_BIN_EXE_orna-cli-v1");
const PROJECT_MAIN: &str = include_str!("fixtures/project-core-main.orna");
const PLAYGROUND_SCHEMA: &str = include_str!("fixtures/playground-schema.orna");

struct RunningServer(Child);

impl Drop for RunningServer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("run git for the served-assists project");
    assert!(
        output.status.success(),
        "git {args:?} exited with {}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).expect("create copied database table directory");
    for entry in fs::read_dir(source).expect("read committed Playground database table") {
        let entry = entry.expect("read Playground database row");
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if entry
            .file_type()
            .expect("inspect Playground database row")
            .is_dir()
        {
            copy_tree(&source_path, &destination_path);
        } else {
            fs::copy(source_path, destination_path).expect("copy committed Playground row");
        }
    }
}

fn wait_until_serving(server: &mut RunningServer, base_url: &str) {
    for _ in 0..100 {
        if let Some(status) = server.0.try_wait().expect("check orna serve process") {
            panic!("orna serve exited before accepting requests: {status}");
        }
        let response = Command::new("curl")
            .args([
                "--silent",
                "--show-error",
                "--output",
                "/dev/null",
                "--write-out",
                "%{http_code}",
                &format!("{base_url}/playground/"),
            ])
            .output()
            .expect("probe the served Playground route");
        if response.status.success() && response.stdout == b"200" {
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
    panic!("orna serve did not publish /playground/ at {base_url}");
}

#[test]
fn served_assist_clients_run_concurrent_inlay_proofs_against_database_assets() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest
        .join("../..")
        .canonicalize()
        .expect("locate repository root");
    let project = tempfile::tempdir().expect("create temporary served-assists database");
    let project_root = project.path();

    let initialized = Command::new(BINARY)
        .arg("init")
        .current_dir(project_root)
        .output()
        .expect("initialize served-assists database");
    assert!(
        initialized.status.success(),
        "orna init exited with {}\n{}",
        initialized.status,
        String::from_utf8_lossy(&initialized.stderr),
    );
    fs::write(project_root.join("main.orna"), PROJECT_MAIN)
        .expect("write crate-local project fixture");
    fs::write(project_root.join("playground.orna"), PLAYGROUND_SCHEMA)
        .expect("write crate-local Playground schema fixture");

    let source_playground = repo_root.join("playground");
    for table in ["Asset", "Entry", "Layout", "Route", "Sample", "Theme"] {
        copy_tree(
            &source_playground.join(table),
            &project_root.join("playground").join(table),
        );
    }
    git(
        project_root,
        &["add", "main.orna", "playground.orna", "playground"],
    );
    git(
        project_root,
        &[
            "-c",
            "user.name=kierandrewett",
            "-c",
            "user.email=kieran@drewett.dev",
            "commit",
            "--quiet",
            "-m",
            "seed database-served Playground assets",
        ],
    );

    let listener = TcpListener::bind("127.0.0.1:0").expect("reserve a free serve port");
    let port = listener.local_addr().expect("read free serve port").port();
    drop(listener);
    let child = Command::new(BINARY)
        .args(["serve", "--port", &port.to_string()])
        .current_dir(project_root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start database-backed orna serve");
    let mut server = RunningServer(child);
    let base_url = format!("http://127.0.0.1:{port}");
    wait_until_serving(&mut server, &base_url);

    let proof = Command::new("node")
        .arg(repo_root.join("playground/web-ui/scripts/prove-served-assists.mjs"))
        .arg(&base_url)
        .current_dir(&repo_root)
        .output()
        .expect("run concurrent served-assists proof");
    print!("{}", String::from_utf8_lossy(&proof.stdout));
    eprint!("{}", String::from_utf8_lossy(&proof.stderr));
    let exit_code = proof.status.code().unwrap_or(-1);
    println!("SERVED_ASSISTS_EXIT_CODE={exit_code}");
    assert!(
        proof.status.success(),
        "served-assists proof exited with {exit_code}"
    );
}
