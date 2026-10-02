use std::{
    io::{Read, Write},
    net::TcpListener,
    path::Path,
    thread,
    time::Duration,
};

use orna_evaluator_v1::{AdmittedReplSession, Limits, SysHostBindingRegistry};
use orna_foundation_v1::{CanonicalValue, OvbRaw};
use orna_sys_v1::{EnvironmentProvider, FilesystemProvider, HttpProvider};
use orna_value_v1::Raw;

fn text_value(value: &str) -> CanonicalValue {
    CanonicalValue::new(Raw::Text(value.to_owned())).unwrap()
}

fn fixture_root(source: &str, root: &Path) -> String {
    source.replace("ROOT_PATH", &root.to_string_lossy())
}

#[test]
fn sys_filesystem_registry_dispatches_real_reads_writes_lists_and_denials() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("input.txt"), "native-file-value").unwrap();
    let mut filesystem = FilesystemProvider::new();
    filesystem.allow_root(root.path()).unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem);
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    session
        .submit(include_str!("fixtures/stdlib-use-io-fs-mpk0d.orna"))
        .unwrap();

    assert_eq!(
        session.submit_with_sys_host_bindings(
            &fixture_root(
                include_str!("fixtures/stdlib-io-fs-read-mpk0d.orna"),
                root.path()
            ),
            &mut bindings,
        ),
        Ok(Some(text_value("native-file-value")))
    );
    assert_eq!(
        session.submit_with_sys_host_bindings(
            &fixture_root(
                include_str!("fixtures/stdlib-io-fs-list-mpk0d.orna"),
                root.path()
            ),
            &mut bindings,
        ),
        Ok(Some(
            CanonicalValue::new(Raw::Array(vec![Raw::Text("input.txt".into())])).unwrap()
        ))
    );
    let metadata = session
        .submit_with_sys_host_bindings(
            &fixture_root(
                include_str!("fixtures/stdlib-io-fs-metadata-mpk0d.orna"),
                root.path(),
            ),
            &mut bindings,
        )
        .unwrap()
        .unwrap();
    assert!(matches!(
        metadata.raw(),
        OvbRaw::Array(fields)
            if fields.len() == 4
                && fields[0] == OvbRaw::Text("file".into())
                && fields[1] == OvbRaw::Tag(60013, Box::new(OvbRaw::Array(vec![
                    OvbRaw::Int(1.into()), OvbRaw::Int(17.into())
                ])))
    ));
    assert_eq!(
        session.submit_with_sys_host_bindings(
            &fixture_root(
                include_str!("fixtures/stdlib-io-fs-write-mpk0d.orna"),
                root.path()
            ),
            &mut bindings,
        ),
        Ok(Some(CanonicalValue::new(Raw::Null).unwrap()))
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("output.txt")).unwrap(),
        "native-write"
    );
    assert_eq!(
        session
            .submit_with_sys_host_bindings(
                &fixture_root(
                    include_str!("fixtures/stdlib-io-fs-traversal-mpk0d.orna"),
                    root.path()
                ),
                &mut bindings,
            )
            .unwrap_err()
            .code(),
        "ORNA-EVAL-ERROR"
    );
}

#[cfg(unix)]
#[test]
fn sys_filesystem_symlink_metadata_is_non_following_and_reads_cannot_escape_root() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.txt"), "outside-secret").unwrap();
    symlink(
        outside.path().join("secret.txt"),
        root.path().join("outside-link"),
    )
    .unwrap();
    let mut filesystem = FilesystemProvider::new();
    filesystem.allow_root(root.path()).unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem);
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    session
        .submit(include_str!("fixtures/stdlib-use-io-fs-mpk0d.orna"))
        .unwrap();

    let metadata = session
        .submit_with_sys_host_bindings(
            &fixture_root(
                include_str!("fixtures/stdlib-io-fs-symlink-metadata-mpk0d.orna"),
                root.path(),
            ),
            &mut bindings,
        )
        .unwrap()
        .unwrap();
    assert!(matches!(
        metadata.raw(),
        OvbRaw::Array(fields) if fields[0] == OvbRaw::Text("symlink".into())
    ));
    assert_eq!(
        session
            .submit_with_sys_host_bindings(
                &fixture_root(
                    include_str!("fixtures/stdlib-io-fs-symlink-read-mpk0d.orna"),
                    root.path(),
                ),
                &mut bindings,
            )
            .unwrap_err()
            .code(),
        "ORNA-EVAL-ERROR"
    );
}

#[test]
fn sys_http_registry_dispatches_real_bounded_loopback_response() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 4096];
        let mut received = 0;
        loop {
            let count = stream.read(&mut request[received..]).unwrap();
            received += count;
            if count == 0
                || request[..received]
                    .windows(4)
                    .any(|window| window == b"\r\n\r\n")
            {
                break;
            }
        }
        assert!(request[..received].starts_with(b"GET /probe HTTP/1.1\r\n"));
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 17\r\nX-Proof: native\r\nConnection: close\r\n\r\nloopback-response",
            )
            .unwrap();
    });

    let origin = format!("http://{address}/");
    let url = format!("http://{address}/probe");
    let mut http = HttpProvider::new(Duration::from_secs(2), 4096, 1024).unwrap();
    http.allow_origin(&origin).unwrap();
    let mut bindings =
        SysHostBindingRegistry::new(EnvironmentProvider::default()).with_http_provider(http);
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    session
        .submit(include_str!("fixtures/stdlib-use-net-http-mpk0d.orna"))
        .unwrap();
    let source =
        include_str!("fixtures/stdlib-net-http-send-mpk0d.orna").replace("URL_VALUE", &url);
    let result = session
        .submit_with_sys_host_bindings(&source, &mut bindings)
        .unwrap()
        .unwrap();
    server.join().unwrap();

    let OvbRaw::Array(parts) = result.raw() else {
        panic!("HTTP result must be a tuple");
    };
    assert_eq!(parts.len(), 3);
    assert_eq!(parts[0], OvbRaw::Int(200.into()));
    assert!(matches!(
        &parts[1],
        OvbRaw::Array(headers)
            if headers.iter().any(|header| matches!(header, OvbRaw::Array(pair)
                if pair.as_slice() == [OvbRaw::Text("x-proof".into()), OvbRaw::Text("native".into())]))
    ));
    assert_eq!(parts[2], OvbRaw::Bytes(b"loopback-response".to_vec()));
}

#[test]
fn sys_filesystem_host_operation_fails_closed_without_provider() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("input.txt"), "native-file-value").unwrap();
    let mut bindings = SysHostBindingRegistry::default();
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    session
        .submit(include_str!("fixtures/stdlib-use-io-fs-mpk0d.orna"))
        .unwrap();
    assert_eq!(
        session
            .submit_with_sys_host_bindings(
                &fixture_root(
                    include_str!("fixtures/stdlib-io-fs-read-mpk0d.orna"),
                    root.path()
                ),
                &mut bindings,
            )
            .unwrap_err()
            .code(),
        "ORNA-EVAL-UNSUPPORTED"
    );
}

#[test]
fn sys_filesystem_provider_enforces_host_selected_text_and_entry_limits() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.txt"), "123456789").unwrap();
    std::fs::write(root.path().join("b.txt"), "b").unwrap();
    let mut filesystem = FilesystemProvider::with_limits(8, 1).unwrap();
    filesystem.allow_root(root.path()).unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem);
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    session
        .submit(include_str!("fixtures/stdlib-use-io-fs-mpk0d.orna"))
        .unwrap();

    for source in [
        fixture_root(
            include_str!("fixtures/stdlib-io-fs-read-mpk0d.orna"),
            root.path(),
        ),
        fixture_root(
            include_str!("fixtures/stdlib-io-fs-list-mpk0d.orna"),
            root.path(),
        ),
    ] {
        assert_eq!(
            session
                .submit_with_sys_host_bindings(&source, &mut bindings)
                .unwrap_err()
                .code(),
            "ORNA-EVAL-ERROR"
        );
    }
}
