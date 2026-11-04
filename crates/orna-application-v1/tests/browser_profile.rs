use orna_application_v1::execute_browser_profile;
use orna_foundation_v1::{CanonicalValue, OvbRaw};

const HOST_BINDINGS: &str = include_str!("../src/browser-profile-host.mjs");

const SOURCE: &str = include_str!("fixtures/browser-profile-renderer.orna");

#[test]
fn selected_ordinary_orna_client_and_renderer_execute_through_shared_evaluator() {
    let rendered = execute_browser_profile(SOURCE, "render")
        .expect("selected Orna renderer is admitted and evaluated");
    let client = execute_browser_profile(SOURCE, "client")
        .expect("selected Orna client is admitted and evaluated");

    assert_eq!(rendered.raw(), &OvbRaw::Text("rendered by Orna".to_owned()));
    assert_eq!(client, rendered);
}

#[test]
fn browser_boundary_transports_canonical_typed_values_without_server_authority() {
    let result = execute_browser_profile(SOURCE, "client")
        .expect("selected Orna client is admitted and evaluated");
    let encoded = result.encode().expect("result is canonical OVB");
    let decoded = CanonicalValue::decode(&encoded).expect("browser payload is canonical OVB");

    assert_eq!(decoded, result);
}

#[test]
fn browser_host_javascript_is_byte_marshalling_only() {
    assert!(HOST_BINDINGS.contains("WebAssembly.Memory"));
    assert!(HOST_BINDINGS.contains("TextEncoder"));
    assert!(!HOST_BINDINGS.contains("git"));
    assert!(!HOST_BINDINGS.contains("table"));
    assert!(!HOST_BINDINGS.contains("closure"));
}
#[test]
fn selected_profile_rejects_unknown_entry_instead_of_falling_back() {
    let error = execute_browser_profile(SOURCE, "missing")
        .expect_err("unknown browser app entry must be rejected");

    assert!(!error.to_string().is_empty());
}
