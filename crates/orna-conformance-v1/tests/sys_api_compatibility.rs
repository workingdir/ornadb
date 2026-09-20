use orna_core::revision::ArtifactCompatibilityCoordinates;
use serde_json::Value;

const SYS_API: &str = include_str!("../../../api/sys.json");

#[test]
fn sys_compatibility_info_schema_matches_the_published_contract() {
    let document: Value = serde_json::from_str(SYS_API).expect("portable sys API JSON");
    let compatibility = document["value_types"]
        .as_array()
        .expect("value types")
        .iter()
        .find(|value| value["name"] == "sys.CompatibilityInfo")
        .expect("sys.CompatibilityInfo");

    assert_eq!(compatibility["kind"], "record");
    assert_eq!(compatibility["type_parameters"], serde_json::json!([]));
    assert_eq!(
        compatibility["fields"],
        serde_json::json!([
            {"name": "language_version", "type": "Str"},
            {"name": "sys_version", "type": "Str"},
            {"name": "canonical_orna_codec_version", "type": "Str"},
            {"name": "repository_layout_version", "type": "Str"},
            {"name": "storage_manifest_version", "type": "Str"},
            {"name": "presentation_protocol_version", "type": "Str"},
            {"name": "supported_profiles", "type": "[Str]"}
        ])
    );
    assert_eq!(compatibility["invariants"], serde_json::json!([]));

    let coordinates = ArtifactCompatibilityCoordinates::new(
        "1.0.0",
        "1.0",
        "OVB-1",
        "1",
        "1",
        "1",
        vec!["orna.present.v1".into()],
    )
    .expect("valid compatibility coordinates");
    assert_eq!(coordinates.language_version(), "1.0.0");
    assert_eq!(coordinates.sys_version(), "1.0");
    assert_eq!(coordinates.canonical_orna_codec_version(), "OVB-1");
    assert_eq!(coordinates.repository_layout_version(), "1");
    assert_eq!(coordinates.storage_manifest_version(), "1");
    assert_eq!(coordinates.presentation_protocol_version(), "1");
    assert_eq!(coordinates.supported_profiles(), ["orna.present.v1"]);
}
