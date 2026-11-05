use orna_application::ApplicationAuthority;
use orna_evaluator::{Environment, Limits};
use orna_foundation::OvbRaw;
use orna_semantic::Catalogue;

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
