use orna_evaluator_v1::{AdmittedReplSession, Limits, SysHostBindingRegistry};
use orna_foundation_v1::{CanonicalValue, OvbRaw};
use orna_syntax_v1::{Declaration, Pattern, TypeExpr, parse_module};
use orna_sys_v1::{
    ClockProvider, EnvironmentProvider, ProcessProvider, system_host_binding_modules_json,
    system_host_operation_registry,
};
use orna_value_v1::Raw;
use std::{
    collections::BTreeMap,
    env,
    time::{Duration, Instant},
};

fn bool_value(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).unwrap()
}

fn text_value(value: &str) -> CanonicalValue {
    CanonicalValue::new(Raw::Text(value.to_owned())).unwrap()
}

fn optional_text_value(value: Option<&str>) -> CanonicalValue {
    let mut fields = vec![OvbRaw::Int(if value.is_some() {
        1.into()
    } else {
        0.into()
    })];
    if let Some(value) = value {
        fields.push(OvbRaw::Text(value.to_owned()));
    }
    CanonicalValue::new(OvbRaw::Tag(60013, Box::new(OvbRaw::Array(fields)))).unwrap()
}

fn process_value(status: Option<i64>, stdout: Vec<u8>, stderr: Vec<u8>) -> CanonicalValue {
    let status = match status {
        Some(status) => OvbRaw::Tag(
            60013,
            Box::new(OvbRaw::Array(vec![
                OvbRaw::Int(1.into()),
                OvbRaw::Int(status.into()),
            ])),
        ),
        None => OvbRaw::Tag(60013, Box::new(OvbRaw::Array(vec![OvbRaw::Int(0.into())]))),
    };
    CanonicalValue::new(OvbRaw::Tag(
        60015,
        Box::new(OvbRaw::Array(vec![
            status,
            OvbRaw::Bytes(stdout),
            OvbRaw::Bytes(stderr),
        ])),
    ))
    .unwrap()
}

#[test]
fn process_and_environment_host_effects_require_a_host_effect_handler() {
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
            "ORNA-EVAL-UNSUPPORTED"
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
fn emitted_environment_declarations_parse_typecheck_and_dispatch_to_native_provider() {
    let modules: BTreeMap<String, String> =
        serde_json::from_str(system_host_binding_modules_json())
            .expect("native host binding manifest is valid JSON");
    let module_source = modules
        .get("std/io/environment.orna")
        .expect("emitted environment declarations are included in the production artifact");
    let parsed = parse_module(module_source);
    assert!(
        parsed.is_ok(),
        "emitted environment module parses: {:?}",
        parsed.diagnostics
    );
    assert_eq!(parsed.value.items.len(), 2);

    let registry = system_host_operation_registry();
    for (item, (local_name, return_type)) in parsed
        .value
        .items
        .iter()
        .zip([("get", "Str?"), ("require", "Str")])
    {
        let Declaration::Function { signature, .. } = &item.declaration else {
            panic!("emitted environment declaration is a function");
        };
        assert_eq!(signature.name, local_name);
        assert_eq!(signature.parameters.len(), 1);
        assert!(matches!(
            &signature.parameters[0].pattern,
            Pattern::Name(name, _) if name == "name"
        ));
        assert!(matches!(
            &signature.parameters[0].annotation,
            Some(TypeExpr::Name { path, arguments, .. })
                if path == &["Str"] && arguments.is_empty()
        ));
        match (return_type, &signature.result) {
            ("Str?", Some(TypeExpr::Optional { inner, .. }))
                if matches!(
                    inner.as_ref(),
                    TypeExpr::Name { path, arguments, .. }
                        if path == &["Str"] && arguments.is_empty()
                ) => {}
            (
                "Str",
                Some(TypeExpr::Name {
                    path, arguments, ..
                }),
            ) if path == &["Str"] && arguments.is_empty() => {}
            _ => panic!("emitted {local_name} return type differs from {return_type}"),
        }
        let operation_name = format!("std.io.environment.{local_name}");
        let operation = registry
            .operation(&operation_name)
            .expect("emitted declaration maps to a typed native operation");
        assert_eq!(operation.parameters, ["name"]);
        assert_eq!(
            operation.signature,
            format!("fn {operation_name}(name: Str): {return_type}"),
            "the parsed consumer type matches the typed native registry"
        );
    }

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
        Ok(Some(optional_text_value(Some("fixture-home"))))
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
        Ok(Some(optional_text_value(None)))
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
        Ok(Some(process_value(
            Some(0),
            b"host-bound".to_vec(),
            Vec::new(),
        )))
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
fn base64_binary_input_survives_real_process_stdin_and_stdout() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default())
        .unwrap_or_else(|error| panic!("reference std failed to load: {}", error.code()));
    session
        .submit(include_str!("fixtures/stdlib-use-io-t7auz.orna"))
        .unwrap_or_else(|error| panic!("std.io import failed: {}", error.code()));
    session
        .submit(include_str!("fixtures/stdlib-use-encoding-6u13r.orna"))
        .unwrap_or_else(|error| panic!("std.encoding import failed: {}", error.code()));

    let mut process =
        ProcessProvider::new(Duration::from_secs(2), 64).expect("valid process limits");
    process
        .allow_command("/usr/bin/cat", env::current_dir().unwrap(), [])
        .expect("allowlisted cat executable and current directory");
    let mut bindings =
        SysHostBindingRegistry::new(EnvironmentProvider::default()).with_process_provider(process);

    assert_eq!(
        session.submit_with_sys_host_bindings(
            include_str!("fixtures/stdlib-io-process-base64-stdin-i50o4.orna"),
            &mut bindings,
        ),
        Ok(Some(process_value(
            Some(0),
            vec![0, 1, 2, 3, 255],
            Vec::new(),
        )))
    );
}

#[test]
fn base64_binary_input_is_preserved_on_both_process_output_pipes() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default())
        .unwrap_or_else(|error| panic!("reference std failed to load: {}", error.code()));
    session
        .submit(include_str!("fixtures/stdlib-use-io-t7auz.orna"))
        .unwrap_or_else(|error| panic!("std.io import failed: {}", error.code()));
    session
        .submit(include_str!("fixtures/stdlib-use-encoding-6u13r.orna"))
        .unwrap_or_else(|error| panic!("std.encoding import failed: {}", error.code()));

    let mut process =
        ProcessProvider::new(Duration::from_secs(2), 64).expect("valid process limits");
    process
        .allow_command("/usr/bin/tee", env::current_dir().unwrap(), [])
        .expect("allowlisted tee executable and current directory");
    let mut bindings =
        SysHostBindingRegistry::new(EnvironmentProvider::default()).with_process_provider(process);

    let bytes = vec![0, 255, 0, b'A'];
    assert_eq!(
        session.submit_with_sys_host_bindings(
            include_str!("fixtures/stdlib-io-process-codec-both-pipes-bwdq0.orna"),
            &mut bindings,
        ),
        Ok(Some(process_value(Some(0), bytes.clone(), bytes,)))
    );
}

#[test]
fn process_arguments_keep_shell_metacharacters_as_literal_text() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default())
        .unwrap_or_else(|error| panic!("reference std failed to load: {}", error.code()));
    session
        .submit(include_str!("fixtures/stdlib-use-io-t7auz.orna"))
        .unwrap_or_else(|error| panic!("std.io import failed: {}", error.code()));

    let mut process =
        ProcessProvider::new(Duration::from_secs(2), 128).expect("valid process limits");
    process
        .allow_command("/usr/bin/printf", env::current_dir().unwrap(), [])
        .expect("allowlisted printf executable and current directory");
    let mut bindings =
        SysHostBindingRegistry::new(EnvironmentProvider::default()).with_process_provider(process);

    assert_eq!(
        session.submit_with_sys_host_bindings(
            include_str!("fixtures/stdlib-io-process-literal-arguments-i50o4.orna"),
            &mut bindings,
        ),
        Ok(Some(process_value(
            Some(0),
            b"$HOME;$(must-not-run)".to_vec(),
            Vec::new(),
        )))
    );
}

#[test]
fn process_child_environment_and_nonzero_status_are_exact_values() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default())
        .unwrap_or_else(|error| panic!("reference std failed to load: {}", error.code()));
    session
        .submit(include_str!("fixtures/stdlib-use-io-t7auz.orna"))
        .unwrap_or_else(|error| panic!("std.io import failed: {}", error.code()));

    let working_directory = env::current_dir().unwrap();
    let mut process =
        ProcessProvider::new(Duration::from_secs(2), 128).expect("valid process limits");
    process
        .allow_command(
            "/usr/bin/env",
            &working_directory,
            ["ORNA_PROOF".to_owned()],
        )
        .expect("allowlisted env executable and one child variable");
    process
        .allow_command("/usr/bin/false", &working_directory, [])
        .expect("allowlisted false executable");
    let mut bindings =
        SysHostBindingRegistry::new(EnvironmentProvider::default()).with_process_provider(process);

    let environment_result = session.submit_with_sys_host_bindings(
        include_str!("fixtures/stdlib-io-process-explicit-environment-bwdq0.orna"),
        &mut bindings,
    );
    assert!(
        environment_result.is_ok(),
        "explicit process environment failed: {}",
        environment_result.as_ref().unwrap_err().code()
    );
    assert_eq!(
        environment_result,
        Ok(Some(process_value(
            Some(0),
            b"ORNA_PROOF=isolated-child\n".to_vec(),
            Vec::new(),
        )))
    );
    assert_eq!(
        session.submit_with_sys_host_bindings(
            include_str!("fixtures/stdlib-io-process-nonzero-status-bwdq0.orna"),
            &mut bindings,
        ),
        Ok(Some(process_value(Some(1), Vec::new(), Vec::new())))
    );
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
