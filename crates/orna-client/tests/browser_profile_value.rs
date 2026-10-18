use orna_client::BrowserProfileValue;
use orna_foundation_v1::{CanonicalValue, OvbRaw};

#[test]
fn browser_profile_value_round_trips_typed_ovb_without_authority() {
    let expected = CanonicalValue::new(OvbRaw::Text("rendered by Orna".to_owned()))
        .expect("text is canonical");
    let bytes = expected.encode().expect("typed value is encodable");

    let result = BrowserProfileValue::decode(&bytes).expect("browser output decodes as OVB-1");

    assert_eq!(result.value(), &expected);
}

#[test]
fn browser_profile_value_rejects_malformed_transport_bytes() {
    assert!(BrowserProfileValue::decode(&[0xff, 0x00]).is_err());
}
