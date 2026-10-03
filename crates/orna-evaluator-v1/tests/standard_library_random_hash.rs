use orna_evaluator_v1::{AdmittedReplSession, Environment, Limits, evaluate_expression};
use orna_foundation_v1::CanonicalValue;
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};
use orna_value_v1::Raw;

fn bool_value(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).unwrap()
}

fn text_value(value: &str) -> CanonicalValue {
    CanonicalValue::new(Raw::Text(value.into())).unwrap()
}

fn payload_environment() -> Environment {
    Environment::from([(
        "payload".into(),
        CanonicalValue::new(Raw::Bytes(b"hello".to_vec())).unwrap(),
    )])
}

fn random_hash_snapshot_session() -> AdmittedReplSession {
    let sources = orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| matches!(path.as_str(), "std/random.orna" | "std/hash.orna"))
        .collect::<Vec<_>>();
    let profile =
        StandardDependencyProfile::from_sources("orna.std/random-hash-focused", sources.clone())
            .expect("the selected random and hash modules form a pinned profile");
    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .expect("the selected modules resolve against core");
    AdmittedReplSession::from_catalogue(&[], catalogue, sources, Limits::default())
        .expect("the selected module snapshot loads")
}

#[test]
fn pinned_random_hash_modules_are_optional_and_do_not_replace_core() {
    let mut with_std = random_hash_snapshot_session();
    assert_eq!(
        with_std.submit(include_str!("fixtures/stdlib-use-random-dmkss.orna")),
        Ok(None)
    );
    assert_eq!(
        with_std.submit(include_str!("fixtures/stdlib-use-hash-dmkss.orna")),
        Ok(None)
    );
    assert_eq!(
        with_std
            .submit(include_str!("fixtures/stdlib-call-random-dmkss.orna"))
            .unwrap_err()
            .code(),
        "ORNA-EVAL-UNSUPPORTED"
    );
    assert_eq!(
        with_std.submit(include_str!("fixtures/stdlib-call-hash-dmkss.orna")),
        Ok(Some(text_value(
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
        )))
    );

    let mut without_std = AdmittedReplSession::new(Limits::default());
    let core = include_str!("fixtures/stdlib-core-without-collections-i7bat.orna");
    assert_eq!(without_std.submit(core), Ok(Some(bool_value(true))));
    for module_import in [
        include_str!("fixtures/stdlib-use-random-dmkss.orna"),
        include_str!("fixtures/stdlib-use-hash-dmkss.orna"),
    ] {
        assert_eq!(
            without_std.submit(module_import).unwrap_err().code(),
            "ORNA-S010-IMPORT"
        );
        assert_eq!(without_std.submit(core), Ok(Some(bool_value(true))));
    }
}

#[test]
fn hash_intrinsics_compute_sha256_domain_separation_and_canonical_hex() {
    let environment = payload_environment();
    let limits = Limits::default();
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/stdlib-hash-blob-dmkss.orna"),
            &environment,
            limits,
        ),
        Ok(text_value(
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
        ))
    );
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/stdlib-hash-domain-dmkss.orna"),
            &environment,
            limits,
        ),
        Ok(text_value(
            "1939f315d7987a5f4685275307bbdabc444cdc1119304b7a0b04b5cb213bfdb3",
        ))
    );

    let digest = Raw::Bytes(
        [
            0x2c, 0xf2, 0x4d, 0xba, 0x5f, 0xb0, 0xa3, 0x0e, 0x26, 0xe8, 0x3b, 0x2a, 0xc5, 0xb9,
            0xe2, 0x9e, 0x1b, 0x16, 0x1e, 0x5c, 0x1f, 0xa7, 0x42, 0x5e, 0x73, 0x04, 0x33, 0x62,
            0x93, 0x8b, 0x98, 0x24,
        ]
        .to_vec(),
    );
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/stdlib-hash-from-hex-dmkss.orna"),
            &Environment::new(),
            limits,
        ),
        Ok(CanonicalValue::new(Raw::Tag(
            60013,
            Box::new(Raw::Array(vec![Raw::Int(1.into()), digest])),
        ))
        .unwrap(),)
    );
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/stdlib-hash-from-uppercase-hex-dmkss.orna"),
            &Environment::new(),
            limits,
        ),
        Ok(CanonicalValue::new(Raw::Tag(
            60013,
            Box::new(Raw::Array(vec![Raw::Int(0.into())])),
        ))
        .unwrap(),)
    );
}
