use std::collections::BTreeSet;

use orna_sys_v1::{
    RepositoryFormat, SystemCallableAvailability, SystemDispatchError, SystemFormatContext,
    system_api_json, system_api_selection, system_api_selection_json, system_callable_for,
    system_function_descriptor,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const FINAL: SystemFormatContext = SystemFormatContext::FORMAT_3_FINAL_2026_10_05;
const ORIGINAL: SystemFormatContext = SystemFormatContext::FORMAT_1_ORIGINAL_1_0_0;
const DRAFT: SystemFormatContext = SystemFormatContext::FORMAT_2_PREVIOUS_1_1_0_DRAFT;
const FINAL_API_SHA256: &str = "10ef7dab9665de4e065ee2b96797b2751c7c3f98f9886241011aee0cc8c0d40e";

#[test]
fn generated_final_api_matches_authority_release_bytes() {
    assert_eq!(
        format!("{:x}", Sha256::digest(system_api_json().as_bytes())),
        FINAL_API_SHA256
    );
}

#[test]
fn final_blob_inventory_is_admitted_only_to_format_three() {
    for name in [
        "sys.blob.length",
        "sys.blob.digest",
        "sys.blob.read",
        "sys.blob.verify",
        "sys.blob.from_bytes",
        "sys.blob.to_bytes",
        "sys.blob.begin",
        "sys.blob.append",
        "sys.blob.finish",
        "sys.blob.abort",
        "sys.blob.capture_file",
        "sys.blob.resource",
        "sys.blob.media_type",
        "sys.blob.suffix",
        "sys.blob.annotate",
        "sys.blob.same_content",
    ] {
        let callable = system_callable_for(FINAL, name).expect("format 3 admits final Blob API");
        assert_eq!(
            callable.availability(),
            SystemCallableAvailability::FinalFormat3
        );
        assert!(matches!(
            system_callable_for(ORIGINAL, name),
            Err(SystemDispatchError::Unavailable { .. })
        ));
        assert!(matches!(
            system_callable_for(DRAFT, name),
            Err(SystemDispatchError::Unavailable { .. })
        ));
    }
}

#[test]
fn retired_placement_apis_remain_historical_reader_only() {
    for name in [
        "sys.admin.set_storage_preference",
        "sys.admin.rewrite_storage",
    ] {
        assert_eq!(
            system_callable_for(ORIGINAL, name)
                .expect("original format retains historical placement API")
                .availability(),
            SystemCallableAvailability::HistoricalReadersOnly
        );
        assert_eq!(
            system_callable_for(DRAFT, name)
                .expect("draft format retains historical placement API")
                .availability(),
            SystemCallableAvailability::HistoricalReadersOnly
        );
        assert!(matches!(
            system_callable_for(FINAL, name),
            Err(SystemDispatchError::Unavailable { .. })
        ));
        assert!(
            system_function_descriptor(name).is_none(),
            "global final descriptor lookup must not expose retired availability"
        );
    }
}

#[test]
fn native_api_selection_and_published_inventory_are_distinct_projections() {
    let selection: Value =
        serde_json::from_str(system_api_selection_json()).expect("native API selection JSON");
    let selection_functions = selection["functions"]
        .as_array()
        .expect("native API selection functions");
    assert_eq!(selection_functions.len(), 84);
    assert!(selection_functions.iter().any(|function| {
        function["name"] == "sys.blob.length" && function["contexts"] == json!(["format-3"])
    }));
    assert!(selection_functions.iter().any(|function| {
        function["name"] == "sys.admin.rewrite_storage"
            && function["contexts"] == json!(["format-1", "format-2"])
    }));

    let published: Value = serde_json::from_str(&system_api_json()).expect("published API JSON");
    let published_functions = published["functions"]
        .as_array()
        .expect("published API functions");
    assert_eq!(published_functions.len(), 82);
    assert!(
        published_functions
            .iter()
            .all(|function| function.get("contexts").is_none())
    );
    let published_names = published_functions
        .iter()
        .map(|function| function["name"].as_str().expect("function name"))
        .collect::<BTreeSet<_>>();
    assert!(!published_names.contains("sys.admin.set_storage_preference"));
    assert!(!published_names.contains("sys.admin.rewrite_storage"));
    assert!(published_names.contains("sys.blob.length"));
    assert_eq!(system_api_selection().available(FINAL).count(), 82);
}

#[test]
fn recorded_context_is_typed_and_fail_closed() {
    assert_eq!(FINAL.format(), RepositoryFormat::Final3);
    assert_eq!(FINAL.context_label(), "final-2026-10-05");
    assert_eq!(
        SystemFormatContext::from_recorded(3, "final-2026-10-05").unwrap(),
        FINAL
    );
    assert!(SystemFormatContext::from_recorded(3, "original 1.0.0").is_err());
    assert!(SystemFormatContext::from_recorded(99, "final-2026-10-05").is_err());
    assert!(SystemFormatContext::from_recorded(3, "unrecorded").is_err());
}
