use num_bigint::BigInt;
use orna_evaluator_v1::{AdmittedReplSession, Limits, SysHostBindingRegistry};
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};
use orna_value_v1::Raw;

fn captured_random_sources() -> Vec<(String, String)> {
    orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| matches!(path.as_str(), "std/random.orna" | "std/hash.orna"))
        .collect()
}

fn session_with_random_snapshot() -> AdmittedReplSession {
    let sources = captured_random_sources();
    let profile = StandardDependencyProfile::from_sources("orna.std/27d5k-random", sources.clone())
        .expect("captured random and hash sources form a dependency snapshot");
    let (path, source) = sources
        .iter()
        .find(|(path, _)| path == "std/random.orna")
        .expect("the random module is in the captured std snapshot");
    profile
        .verify_source(path, source)
        .expect("random source bytes are bound to the selected snapshot");
    assert!(
        profile
            .verify_source(path, &format!("{source}\n// changed after capture"))
            .is_err()
    );
    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .expect("the selected modules resolve against the core catalogue");
    let mut session =
        AdmittedReplSession::from_catalogue(&[], catalogue, sources, Limits::default())
            .unwrap_or_else(|error| {
                panic!("captured random profile failed to load: {}", error.code())
            });
    session
        .submit(include_str!("fixtures/stdlib-use-random-27d5k.orna"))
        .expect("the captured random module imports");
    session
}

#[test]
fn random_helpers_return_values_with_the_documented_constraints() {
    let mut session = session_with_random_snapshot();
    let mut bindings = SysHostBindingRegistry::default().with_system_random_provider();

    let bytes = session
        .submit_with_sys_host_bindings(
            include_str!("fixtures/stdlib-random-bytes-27d5k.orna"),
            &mut bindings,
        )
        .expect("the installed operating system CSPRNG supplies bytes")
        .expect("the expression returns a Blob");
    assert!(matches!(bytes.raw(), Raw::Bytes(bytes) if bytes.len() == 8));

    let integer = session
        .submit_with_sys_host_bindings(
            include_str!("fixtures/stdlib-random-integer-27d5k.orna"),
            &mut bindings,
        )
        .expect("the nonempty integer interval is sampled")
        .expect("the expression returns an integer");
    assert!(
        matches!(integer.raw(), Raw::Int(value) if *value >= (-7).into() && *value < 13.into())
    );

    let choice = session
        .submit_with_sys_host_bindings(
            include_str!("fixtures/stdlib-random-choose-27d5k.orna"),
            &mut bindings,
        )
        .expect("a nonempty list yields a choice")
        .expect("the expression returns an optional value");
    let Raw::Tag(60013, payload) = choice.raw() else {
        panic!("choose returns the documented optional value");
    };
    let Raw::Array(payload) = payload.as_ref() else {
        panic!("Some carries its discriminator and selected value");
    };
    assert_eq!(payload[0], Raw::Int(1.into()));
    assert!(
        matches!(&payload[1], Raw::Int(value) if value == &BigInt::from(7) || value == &BigInt::from(11))
    );

    session
        .submit(include_str!("fixtures/stdlib-random-empty-list-27d5k.orna"))
        .expect("an empty integer list can be retained in the session");
    let empty_choice = session
        .submit_with_sys_host_bindings(
            include_str!("fixtures/stdlib-random-choose-empty-27d5k.orna"),
            &mut bindings,
        )
        .expect("an empty list is a valid choice input")
        .expect("the expression returns an optional value");
    assert_eq!(
        empty_choice.raw(),
        &Raw::Tag(60013, Box::new(Raw::Array(vec![Raw::Int(0.into())])))
    );

    let shuffled = session
        .submit_with_sys_host_bindings(
            include_str!("fixtures/stdlib-random-shuffle-27d5k.orna"),
            &mut bindings,
        )
        .expect("shuffle computes a permutation")
        .expect("the expression returns a list");
    let Raw::Array(items) = shuffled.raw() else {
        panic!("shuffle returns a list");
    };
    let mut actual = items
        .iter()
        .map(|item| match item {
            Raw::Int(value) => value.clone(),
            _ => panic!("the shuffled list retains integer items"),
        })
        .collect::<Vec<_>>();
    actual.sort();
    assert_eq!(actual, vec![7.into(), 7.into(), 11.into()]);
}

#[test]
fn random_module_fails_closed_without_an_installed_csprng() {
    let mut session = session_with_random_snapshot();
    let mut bindings = SysHostBindingRegistry::default();
    let error = session
        .submit_with_sys_host_bindings(
            include_str!("fixtures/stdlib-random-without-provider-27d5k.orna"),
            &mut bindings,
        )
        .expect_err("missing randomness capability does not synthesize output");
    assert_eq!(error.code(), "ORNA-EVAL-UNSUPPORTED");
}
