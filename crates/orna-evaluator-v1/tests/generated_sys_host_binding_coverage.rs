use std::collections::BTreeSet;

use orna_sys_v1::system_host_operation_registry;

struct BindingProof {
    operation: &'static str,
    provider: &'static str,
    fixture: &'static str,
    call: &'static str,
    behavior_test: &'static str,
    behavior_source: &'static str,
    observable: &'static str,
}

const PROCESS_ENVIRONMENT_PROOFS: &str = include_str!("standard_library_process_environment.rs");
const FILESYSTEM_NETWORK_PROOFS: &str = include_str!("standard_library_filesystem_network.rs");

#[test]
fn every_generated_host_binding_has_a_fixture_and_native_value_proof() {
    let proofs = [
        BindingProof {
            operation: "std.io.environment.get",
            provider: "orna.sys.host.environment.v1",
            fixture: include_str!("fixtures/stdlib-io-environment-get-host-call-xbf3n.orna"),
            call: "io.environment.get(",
            behavior_test: "typed_sys_environment_bindings_execute_native_allowlisted_provider",
            behavior_source: PROCESS_ENVIRONMENT_PROOFS,
            observable: "fixture-home",
        },
        BindingProof {
            operation: "std.io.environment.require",
            provider: "orna.sys.host.environment.v1",
            fixture: include_str!("fixtures/stdlib-io-environment-require-host-call-xbf3n.orna"),
            call: "io.environment.require(",
            behavior_test: "typed_sys_environment_bindings_execute_native_allowlisted_provider",
            behavior_source: PROCESS_ENVIRONMENT_PROOFS,
            observable: "fixture-path",
        },
        BindingProof {
            operation: "std.io.process.run",
            provider: "orna.sys.host.process.v1",
            fixture: include_str!("fixtures/stdlib-io-process-run-real-provider-522pj.orna"),
            call: "io.process.run(",
            behavior_test: "typed_sys_process_and_clock_bindings_execute_allowlisted_native_providers",
            behavior_source: PROCESS_ENVIRONMENT_PROOFS,
            observable: "host-bound",
        },
        BindingProof {
            operation: "std.concurrent.sleep",
            provider: "orna.sys.host.clock.v1",
            fixture: include_str!("fixtures/stdlib-concurrent-sleep-real-provider-522pj.orna"),
            call: "concurrent.sleep(",
            behavior_test: "typed_sys_process_and_clock_bindings_execute_allowlisted_native_providers",
            behavior_source: PROCESS_ENVIRONMENT_PROOFS,
            observable: "sleep_started.elapsed() >= Duration::from_secs(1)",
        },
        BindingProof {
            operation: "std.io.fs.read_text",
            provider: "orna.sys.host.filesystem.v1",
            fixture: include_str!("fixtures/stdlib-io-fs-read-mpk0d.orna"),
            call: "std.io.fs.read_text(",
            behavior_test: "sys_filesystem_registry_dispatches_real_reads_writes_lists_and_denials",
            behavior_source: FILESYSTEM_NETWORK_PROOFS,
            observable: "native-file-value",
        },
        BindingProof {
            operation: "std.io.fs.write_text",
            provider: "orna.sys.host.filesystem.v1",
            fixture: include_str!("fixtures/stdlib-io-fs-write-mpk0d.orna"),
            call: "std.io.fs.write_text(",
            behavior_test: "sys_filesystem_registry_dispatches_real_reads_writes_lists_and_denials",
            behavior_source: FILESYSTEM_NETWORK_PROOFS,
            observable: "native-write",
        },
        BindingProof {
            operation: "std.io.fs.append_text",
            provider: "orna.sys.host.filesystem.v1",
            fixture: include_str!("fixtures/stdlib-io-fs-append-mpk0d.orna"),
            call: "std.io.fs.append_text(",
            behavior_test: "all_registered_filesystem_dispatch_arms_execute_with_native_results",
            behavior_source: FILESYSTEM_NETWORK_PROOFS,
            observable: "source-appended",
        },
        BindingProof {
            operation: "std.io.fs.exists",
            provider: "orna.sys.host.filesystem.v1",
            fixture: include_str!("fixtures/stdlib-io-fs-exists-mpk0d.orna"),
            call: "std.io.fs.exists(",
            behavior_test: "all_registered_filesystem_dispatch_arms_execute_with_native_results",
            behavior_source: FILESYSTEM_NETWORK_PROOFS,
            observable: "Raw::Bool(true)",
        },
        BindingProof {
            operation: "std.io.fs.is_directory",
            provider: "orna.sys.host.filesystem.v1",
            fixture: include_str!("fixtures/stdlib-io-fs-is-directory-mpk0d.orna"),
            call: "std.io.fs.is_directory(",
            behavior_test: "all_registered_filesystem_dispatch_arms_execute_with_native_results",
            behavior_source: FILESYSTEM_NETWORK_PROOFS,
            observable: "Raw::Bool(true)",
        },
        BindingProof {
            operation: "std.io.fs.list",
            provider: "orna.sys.host.filesystem.v1",
            fixture: include_str!("fixtures/stdlib-io-fs-list-mpk0d.orna"),
            call: "std.io.fs.list(",
            behavior_test: "sys_filesystem_registry_dispatches_real_reads_writes_lists_and_denials",
            behavior_source: FILESYSTEM_NETWORK_PROOFS,
            observable: "input.txt",
        },
        BindingProof {
            operation: "std.io.fs.metadata",
            provider: "orna.sys.host.filesystem.v1",
            fixture: include_str!("fixtures/stdlib-io-fs-metadata-mpk0d.orna"),
            call: "std.io.fs.metadata(",
            behavior_test: "sys_filesystem_registry_dispatches_real_reads_writes_lists_and_denials",
            behavior_source: FILESYSTEM_NETWORK_PROOFS,
            observable: "OvbRaw::Int(17.into())",
        },
        BindingProof {
            operation: "std.io.fs.symlink_metadata",
            provider: "orna.sys.host.filesystem.v1",
            fixture: include_str!("fixtures/stdlib-io-fs-symlink-metadata-mpk0d.orna"),
            call: "std.io.fs.symlink_metadata(",
            behavior_test: "sys_filesystem_symlink_metadata_is_non_following_and_reads_cannot_escape_root",
            behavior_source: FILESYSTEM_NETWORK_PROOFS,
            observable: "OvbRaw::Text(\"symlink\".into())",
        },
        BindingProof {
            operation: "std.io.fs.create_dir",
            provider: "orna.sys.host.filesystem.v1",
            fixture: include_str!("fixtures/stdlib-io-fs-create-dir-mpk0d.orna"),
            call: "std.io.fs.create_dir(",
            behavior_test: "all_registered_filesystem_dispatch_arms_execute_with_native_results",
            behavior_source: FILESYSTEM_NETWORK_PROOFS,
            observable: "join(\"created\").is_dir()",
        },
        BindingProof {
            operation: "std.io.fs.copy_file",
            provider: "orna.sys.host.filesystem.v1",
            fixture: include_str!("fixtures/stdlib-io-fs-copy-mpk0d.orna"),
            call: "std.io.fs.copy_file(",
            behavior_test: "all_registered_filesystem_dispatch_arms_execute_with_native_results",
            behavior_source: FILESYSTEM_NETWORK_PROOFS,
            observable: "join(\"copy.txt\").exists()",
        },
        BindingProof {
            operation: "std.io.fs.move_file",
            provider: "orna.sys.host.filesystem.v1",
            fixture: include_str!("fixtures/stdlib-io-fs-move-mpk0d.orna"),
            call: "std.io.fs.move_file(",
            behavior_test: "all_registered_filesystem_dispatch_arms_execute_with_native_results",
            behavior_source: FILESYSTEM_NETWORK_PROOFS,
            observable: "join(\"moved.txt\").exists()",
        },
        BindingProof {
            operation: "std.io.fs.remove_file",
            provider: "orna.sys.host.filesystem.v1",
            fixture: include_str!("fixtures/stdlib-io-fs-remove-mpk0d.orna"),
            call: "std.io.fs.remove_file(",
            behavior_test: "all_registered_filesystem_dispatch_arms_execute_with_native_results",
            behavior_source: FILESYSTEM_NETWORK_PROOFS,
            observable: "!root.path().join(\"moved.txt\").exists()",
        },
        BindingProof {
            operation: "std.net.http.send",
            provider: "orna.sys.host.http.v1",
            fixture: include_str!("fixtures/stdlib-net-http-send-mpk0d.orna"),
            call: "std.net.http.send(",
            behavior_test: "sys_http_registry_dispatches_real_bounded_loopback_response",
            behavior_source: FILESYSTEM_NETWORK_PROOFS,
            observable: "loopback-response",
        },
        BindingProof {
            operation: "std.net.http.start",
            provider: "orna.sys.host.http.v1",
            fixture: include_str!("fixtures/stdlib-net-http-start-wait-wxip1.orna"),
            call: "std.net.http.start(",
            behavior_test: "sys_http_start_wait_and_cancel_dispatch_real_native_behavior",
            behavior_source: FILESYSTEM_NETWORK_PROOFS,
            observable: "async-response",
        },
        BindingProof {
            operation: "std.net.http.wait",
            provider: "orna.sys.host.http.v1",
            fixture: include_str!("fixtures/stdlib-net-http-start-wait-wxip1.orna"),
            call: "std.net.http.wait(",
            behavior_test: "sys_http_start_wait_and_cancel_dispatch_real_native_behavior",
            behavior_source: FILESYSTEM_NETWORK_PROOFS,
            observable: "async-response",
        },
        BindingProof {
            operation: "std.net.http.cancel",
            provider: "orna.sys.host.http.v1",
            fixture: include_str!("fixtures/stdlib-net-http-start-cancel-wxip1.orna"),
            call: "std.net.http.cancel(",
            behavior_test: "sys_http_start_wait_and_cancel_dispatch_real_native_behavior",
            behavior_source: FILESYSTEM_NETWORK_PROOFS,
            observable: "cancellation reports that it stopped the in-flight request",
        },
    ];

    let audited_names = proofs
        .iter()
        .map(|proof| proof.operation)
        .collect::<BTreeSet<_>>();
    let registry_names = system_host_operation_registry()
        .operations()
        .map(|operation| operation.name.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(audited_names, registry_names);

    for proof in proofs {
        let operation = system_host_operation_registry()
            .operation(proof.operation)
            .expect("every generated operation has a coverage row");
        assert_eq!(operation.provider, proof.provider, "{}", proof.operation);
        assert!(proof.fixture.contains(proof.call), "{}", proof.operation);
        assert!(
            proof
                .behavior_source
                .contains(&format!("fn {}(", proof.behavior_test)),
            "{} has its native behavior proof",
            proof.operation
        );
        assert!(
            proof.behavior_source.contains(proof.observable),
            "{} has a concrete native result assertion",
            proof.operation
        );
    }
}
