use num_bigint::BigInt;
use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};
use orna_value_v1::{Raw, Value};

fn canonical(raw: Raw) -> CanonicalValue {
    CanonicalValue::new(raw).expect("expected value is canonical")
}

fn integer(decimal: &str) -> BigInt {
    BigInt::parse_bytes(decimal.as_bytes(), 10).expect("test integer is valid")
}

fn int_value(decimal: &str) -> Raw {
    Raw::Int(integer(decimal))
}

fn option_int(decimal: &str) -> CanonicalValue {
    let value = Value::option(Some(Value::int(integer(decimal)))).expect("integer option is valid");
    canonical(value.raw().clone())
}

fn option_decimal(coefficient: &str, exponent10: i64) -> CanonicalValue {
    let decimal = Value::decimal(integer(coefficient), exponent10.into())
        .expect("decimal option value is valid");
    let value = Value::option(Some(decimal)).expect("decimal option is valid");
    canonical(value.raw().clone())
}

#[test]
fn pinned_numeric_modules_keep_results_exact_past_float_precision() {
    let sources = orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| path == "std/math.orna" || path == "std/stats.orna")
        .collect::<Vec<_>>();
    assert_eq!(sources.len(), 2, "both numeric modules are captured");
    let profile = StandardDependencyProfile::from_sources(
        "orna.std/mcqjg-numeric-precision",
        sources.clone(),
    )
    .expect("selected numeric source bytes form an immutable snapshot");
    for (path, source) in &sources {
        profile
            .verify_source(path, source)
            .expect("numeric source bytes match the captured dependency snapshot");
        let mut changed = source.clone();
        changed.push_str("\n// changed after snapshot capture");
        assert!(
            profile.verify_source(path, &changed).is_err(),
            "edited source cannot replace captured {path}"
        );
    }
    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .expect("numeric modules resolve against the core catalogue");
    let mut session =
        AdmittedReplSession::from_catalogue(&[], catalogue, sources, Limits::default())
            .expect("captured numeric sources are admitted");
    for import in [
        include_str!("fixtures/stdlib-use-math-t7auz.orna"),
        include_str!("fixtures/stdlib-use-stats-z09xc.orna"),
    ] {
        assert_eq!(session.submit(import), Ok(None), "import: {import}");
    }

    let integer_math = session
        .submit(include_str!(
            "fixtures/stdlib-math-large-precision-mcqjg.orna"
        ))
        .unwrap_or_else(|error| panic!("large integer math failed with {}", error.code()));
    assert_eq!(
        integer_math,
        Some(canonical(Raw::Array(vec![
            int_value("9007199254740993"),
            int_value("135107988821114895"),
        ]))),
        "GCD and LCM preserve exact values above binary Float precision"
    );

    assert_eq!(
        session
            .submit(include_str!("fixtures/stdlib-math-zero-power-mcqjg.orna"))
            .unwrap_or_else(|error| panic!("zero exponent failed with {}", error.code())),
        Some(option_int("1")),
        "the exact zero-exponent identity returns one, including 0^0"
    );

    assert_eq!(
        session
            .submit(include_str!("fixtures/stdlib-stats-large-mean-mcqjg.orna"))
            .unwrap_or_else(|error| panic!("large exact mean failed with {}", error.code())),
        Some(option_decimal("900719925474099325", -2)),
        "Decimal mean keeps the quarter-unit tail at a magnitude where Float loses it"
    );

    assert_eq!(
        session
            .submit(include_str!("fixtures/stdlib-stats-large-sum-mcqjg.orna"))
            .unwrap_or_else(|error| panic!("large exact sum failed with {}", error.code())),
        Some(canonical(
            Value::decimal(integer("12345678901234567891"), 0.into())
                .expect("exact decimal sum is valid")
                .raw()
                .clone(),
        )),
        "Decimal sum retains the unit after adding a fractional tail"
    );

    let mut without_std = AdmittedReplSession::new(Limits::default());
    assert_eq!(
        without_std
            .submit(include_str!(
                "fixtures/stdlib-core-large-int-without-mcqjg.orna"
            ))
            .unwrap_or_else(|error| panic!("core integer addition failed with {}", error.code())),
        Some(canonical(int_value("9007199254740994"))),
        "core exact integer arithmetic remains available without std"
    );
}
