use orna_evaluator_v1::{AdmittedReplSession, Limits, SysHostBindingRegistry};
use orna_foundation_v1::CanonicalValue;
use orna_sys_v1::{ClockProvider, EnvironmentProvider, ProcessProvider};
use orna_value_v1::Raw;
use std::{
    env,
    time::{Duration, Instant},
};

fn bool_value(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).unwrap()
}

fn text_value(value: &str) -> CanonicalValue {
    CanonicalValue::new(Raw::Text(value.to_owned())).unwrap()
}

#[test]
fn process_and_environment_host_effects_fail_closed_without_bindings() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default())
        .unwrap_or_else(|error| panic!("reference std failed to load: {}", error.code()));
    session
        .submit(include_str!("fixtures/stdlib-use-io-t7auz.orna"))
        .unwrap_or_else(|error| panic!("std.io import failed: {}", error.code()));

    for source in [
        include_str!("fixtures/stdlib-io-process-run-host-call-xbf3n.orna"),
        include_str!("fixtures/stdlib-io-environment-get-host-call-xbf3n.orna"),
        include_str!("fixtures/stdlib-io-environment-require-host-call-xbf3n.orna"),
    ] {
        assert_eq!(
            session.submit(source).unwrap_err().code(),
            "ORNA-EVAL-ERROR"
        );
    }
}

#[test]
fn core_remains_available_when_process_and_environment_modules_are_absent() {
    let mut session = AdmittedReplSession::new(Limits::default());
    let core = include_str!("fixtures/stdlib-core-without-process-environment-xbf3n.orna");
    assert_eq!(
        session
            .submit(core)
            .unwrap_or_else(|error| panic!("core source failed without std: {}", error.code())),
        Some(bool_value(true))
    );
    for source in [
        include_str!("fixtures/stdlib-io-process-without-snapshot-xbf3n.orna"),
        include_str!("fixtures/stdlib-io-environment-without-snapshot-xbf3n.orna"),
    ] {
        assert_eq!(
            session.submit(source).unwrap_err().code(),
            "ORNA-S010-IMPORT"
        );
    }
    assert_eq!(
        session.submit(core).unwrap_or_else(|error| panic!(
            "core source failed after missing import: {}",
            error.code()
        )),
        Some(bool_value(true))
    );
}

#[test]
fn typed_sys_environment_bindings_execute_native_allowlisted_provider() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default())
        .unwrap_or_else(|error| panic!("reference std failed to load: {}", error.code()));
    session
        .submit(include_str!("fixtures/stdlib-use-io-t7auz.orna"))
        .unwrap_or_else(|error| panic!("std.io import failed: {}", error.code()));
    let provider = EnvironmentProvider::from_snapshot([
        ("HOME".into(), Some("fixture-home".into())),
        ("PATH".into(), Some("fixture-path".into())),
        ("OPTIONAL".into(), None),
    ])
    .expect("valid explicit host environment snapshot");
    let mut bindings = SysHostBindingRegistry::new(provider);

    assert_eq!(
        session.submit_with_sys_host_bindings(
            include_str!("fixtures/stdlib-io-environment-get-host-call-xbf3n.orna"),
            &mut bindings,
        ),
        Ok(Some(text_value("fixture-home")))
    );
    assert_eq!(
        session.submit_with_sys_host_bindings(
            include_str!("fixtures/stdlib-io-environment-require-host-call-xbf3n.orna"),
            &mut bindings,
        ),
        Ok(Some(text_value("fixture-path")))
    );
    assert_eq!(
        session.submit_with_sys_host_bindings(
            include_str!("fixtures/stdlib-io-environment-get-unset-host-call-xufpz.orna"),
            &mut bindings,
        ),
        Ok(Some(CanonicalValue::new(Raw::Null).unwrap()))
    );
    assert_eq!(
        session
            .submit_with_sys_host_bindings(
                include_str!("fixtures/stdlib-io-environment-require-unset-host-call-xufpz.orna"),
                &mut bindings,
            )
            .unwrap_err()
            .code(),
        "ORNA-EVAL-ERROR"
    );
    assert_eq!(
        session
            .submit_with_sys_host_bindings(
                include_str!("fixtures/stdlib-io-environment-get-denied-host-call-xufpz.orna"),
                &mut bindings,
            )
            .unwrap_err()
            .code(),
        "ORNA-EVAL-ERROR"
    );
}

#[test]
fn typed_sys_process_and_clock_bindings_execute_allowlisted_native_providers() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default())
        .unwrap_or_else(|error| panic!("reference std failed to load: {}", error.code()));
    session
        .submit(include_str!("fixtures/stdlib-use-io-t7auz.orna"))
        .unwrap_or_else(|error| panic!("std.io import failed: {}", error.code()));
    session
        .submit(include_str!("fixtures/stdlib-use-concurrent-zhw5h.orna"))
        .unwrap_or_else(|error| panic!("std.concurrent import failed: {}", error.code()));
    let mut process =
        ProcessProvider::new(Duration::from_secs(2), 1024).expect("valid process limits");
    process
        .allow_command("/usr/bin/printf", env::current_dir().unwrap(), [])
        .expect("allowlisted printf executable and current directory");
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_process_provider(process)
        .with_clock_provider(ClockProvider::new(Duration::from_secs(1)));

    let process_result = session.submit_with_sys_host_bindings(
        include_str!("fixtures/stdlib-io-process-run-real-provider-522pj.orna"),
        &mut bindings,
    );
    assert!(
        process_result.is_ok(),
        "process binding failed: {}",
        process_result.as_ref().unwrap_err().code()
    );
    assert_eq!(
        process_result,
        Ok(Some(
            CanonicalValue::new(orna_foundation_v1::OvbRaw::Array(vec![
                orna_foundation_v1::OvbRaw::Tag(
                    60013,
                    Box::new(orna_foundation_v1::OvbRaw::Array(vec![
                        orna_foundation_v1::OvbRaw::Int(1.into()),
                        orna_foundation_v1::OvbRaw::Int(0.into()),
                    ])),
                ),
                orna_foundation_v1::OvbRaw::Bytes(b"host-bound".to_vec()),
                orna_foundation_v1::OvbRaw::Bytes(Vec::new()),
            ]))
            .expect("process tuple is canonical")
        ))
    );
    assert_eq!(
        session
            .submit_with_sys_host_bindings(
                include_str!("fixtures/stdlib-io-process-denied-executable-522pj.orna"),
                &mut bindings,
            )
            .unwrap_err()
            .code(),
        "ORNA-EVAL-ERROR"
    );
    assert_eq!(
        session
            .submit_with_sys_host_bindings(
                include_str!("fixtures/stdlib-io-process-output-limit-522pj.orna"),
                &mut bindings,
            )
            .unwrap_err()
            .code(),
        "ORNA-EVAL-ERROR"
    );

    let mut bounded_process =
        ProcessProvider::new(Duration::from_millis(50), 64).expect("valid process limits");
    bounded_process
        .allow_command("/usr/bin/sleep", env::current_dir().unwrap(), [])
        .expect("allowlisted sleep executable and current directory");
    let mut bounded_bindings =
        SysHostBindingRegistry::default().with_process_provider(bounded_process);
    assert_eq!(
        session
            .submit_with_sys_host_bindings(
                include_str!("fixtures/stdlib-io-process-default-timeout-522pj.orna"),
                &mut bounded_bindings,
            )
            .unwrap_err()
            .code(),
        "ORNA-EVAL-ERROR"
    );

    let sleep_started = Instant::now();
    let sleep_result = session.submit_with_sys_host_bindings(
        include_str!("fixtures/stdlib-concurrent-sleep-real-provider-522pj.orna"),
        &mut bindings,
    );
    assert!(
        sleep_result.is_ok(),
        "clock binding failed: {}",
        sleep_result.as_ref().unwrap_err().code()
    );
    assert_eq!(
        sleep_result,
        Ok(Some(
            CanonicalValue::new(orna_foundation_v1::OvbRaw::Null).unwrap()
        ))
    );
    assert!(sleep_started.elapsed() >= Duration::from_secs(1));
}

#[test]
fn process_and_clock_fail_closed_when_registry_provider_is_not_installed() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default())
        .unwrap_or_else(|error| panic!("reference std failed to load: {}", error.code()));
    session
        .submit(include_str!("fixtures/stdlib-use-io-t7auz.orna"))
        .unwrap_or_else(|error| panic!("std.io import failed: {}", error.code()));
    session
        .submit(include_str!("fixtures/stdlib-use-concurrent-zhw5h.orna"))
        .unwrap_or_else(|error| panic!("std.concurrent import failed: {}", error.code()));
    let mut bindings = SysHostBindingRegistry::default();

    assert_eq!(
        session
            .submit_with_sys_host_bindings(
                include_str!("fixtures/stdlib-io-process-run-real-provider-522pj.orna"),
                &mut bindings,
            )
            .unwrap_err()
            .code(),
        "ORNA-EVAL-UNSUPPORTED"
    );
    assert_eq!(
        session
            .submit_with_sys_host_bindings(
                include_str!("fixtures/stdlib-concurrent-sleep-no-provider-522pj.orna"),
                &mut bindings,
            )
            .unwrap_err()
            .code(),
        "ORNA-EVAL-UNSUPPORTED"
    );
}
