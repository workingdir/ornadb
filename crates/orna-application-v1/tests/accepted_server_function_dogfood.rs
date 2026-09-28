use orna_application_v1::ApplicationAuthority;
use orna_evaluator_v1::{Environment, Limits};
use orna_foundation_v1::OvbRaw;
use orna_semantic_v1::Catalogue;

const SOURCE: &str = include_str!("fixtures/server-function-dogfood.orna");

#[test]
fn accepted_dogfood_application_is_admitted_and_invoked() {
    let authority = ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
    let application = authority
        .admit_module("server-function-dogfood.orna", SOURCE, "read")
        .expect("dogfood source must pass parser, resolution, and type/effect checks");

    let result = authority
        .evaluate(&application, &Environment::new())
        .expect("the admitted read entry must execute through the application authority");

    assert_eq!(result.raw(), &OvbRaw::Int(42.into()));
}
