use std::collections::BTreeMap;

use orna_evaluator_v1::{
    AdmittedReplSession, Environment, Functions, Limits, PureFunction,
    evaluate_expression_with_functions,
};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

fn bool_value(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).unwrap()
}

fn pinned_functions() -> Functions {
    let mut functions = BTreeMap::new();
    for (path, source) in orna_standard_sources::reference_standard_sources_v1() {
        let parsed = orna_syntax_v1::parse_module(&source);
        assert!(parsed.is_ok(), "{path}: {:#?}", parsed.diagnostics);
        let module = path
            .strip_prefix("std/")
            .and_then(|path| path.strip_suffix(".orna"))
            .expect("pinned std paths have a std/ prefix and .orna suffix")
            .replace('/', ".");
        for item in parsed.value.items {
            if let orna_syntax_v1::Declaration::Function { signature, body } = item.declaration {
                let name = format!("std.{module}.{}", signature.name);
                assert!(
                    functions
                        .insert(
                            name.clone(),
                            PureFunction {
                                parameters: signature.parameters,
                                body,
                                environment: Environment::new(),
                            },
                        )
                        .is_none(),
                    "duplicate pinned standard function {name}"
                );
            }
        }
    }
    functions
}

#[test]
fn networking_url_modules_are_optional_and_host_effects_are_separated() {
    let mut with_std = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    let imports = with_std.submit(include_str!("fixtures/stdlib-use-net-bc963.orna"));
    assert!(
        imports == Ok(None),
        "optional std imports failed: {:?}",
        imports.err().map(|error| error.code().to_owned())
    );
    assert_eq!(
        with_std.submit(include_str!("fixtures/stdlib-use-url-bc963.orna")),
        Ok(None)
    );
    // std.url.parse deliberately fails with canonical ORNA-EVAL-UNSUPPORTED
    // (stdlib/std/url.orna); the admitted REPL boundary presents every
    // deliberate std failure as ORNA-EVAL-ERROR and redacts the canonical code
    // (ORNA-FAILURE-009), as in the sibling std tests.
    assert_eq!(
        with_std
            .submit(include_str!("fixtures/stdlib-call-net-url-bc963.orna"))
            .unwrap_err()
            .code(),
        "ORNA-EVAL-ERROR"
    );
    let functions = pinned_functions();
    let error = evaluate_expression_with_functions(
        include_str!("fixtures/stdlib-call-net-url-bc963.orna"),
        &Environment::new(),
        &functions,
        Limits::default(),
    )
    .expect_err("std.url.parse must fail with its pinned error");
    assert_eq!(
        error
            .canonical_error()
            .expect("std failure keeps its canonical error identity")
            .code(),
        "ORNA-EVAL-UNSUPPORTED"
    );
    for network_call in [
        include_str!("fixtures/stdlib-call-net-http-bc963.orna"),
        include_str!("fixtures/stdlib-call-net-websocket-bc963.orna"),
    ] {
        assert_eq!(
            with_std.submit(network_call).unwrap_err().code(),
            "ORNA-REPL-EFFECT"
        );
    }

    let mut without_std = AdmittedReplSession::new(Limits::default());
    let core = include_str!("fixtures/stdlib-core-without-net-bc963.orna");
    assert_eq!(without_std.submit(core), Ok(Some(bool_value(true))));
    assert_eq!(
        without_std
            .submit(include_str!("fixtures/stdlib-use-net-missing-bc963.orna"))
            .unwrap_err()
            .code(),
        "ORNA-S010-IMPORT"
    );
    assert_eq!(
        without_std
            .submit(include_str!("fixtures/stdlib-use-url-missing-bc963.orna"))
            .unwrap_err()
            .code(),
        "ORNA-S010-IMPORT"
    );
    assert_eq!(without_std.submit(core), Ok(Some(bool_value(true))));
}
