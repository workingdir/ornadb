use orna_client::BrowserProfileValue;
use orna_application_v1::execute_browser_profile;
use orna_foundation_v1::{CanonicalValue, OvbRaw};

const HTTP_SOURCE: &str = include_str!("fixtures/browser-profile-http.orna");
const SECOND_APP_SOURCE: &str = include_str!("fixtures/browser-profile-second-app.orna");
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
    let client_value = BrowserProfileValue::decode(&encoded)
        .expect("the public client API decodes browser profile output");

    assert_eq!(client_value.value(), &result);
}
/// ORNA-APP-021: an independent HTTP renderer and a second app both use the public profile entry.
#[test]
fn independent_http_renderer_and_second_app_use_the_public_profile_entry() {
    let response = execute_browser_profile(HTTP_SOURCE, "response")
        .expect("independent Orna HTTP response is admitted and evaluated");
    let second_app = execute_browser_profile(SECOND_APP_SOURCE, "home")
        .expect("second ordinary application uses the same public profile entry");

    assert_eq!(response.raw(), &OvbRaw::Text(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n<main><button>Run</button></main>".to_owned(),
    ));
    assert_eq!(second_app.raw(), &OvbRaw::Text("A second ordinary source-hosted app".to_owned()));
}

/// ORNA-APP-016: browser host JavaScript is byte marshalling only, with no git or table authority.
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
