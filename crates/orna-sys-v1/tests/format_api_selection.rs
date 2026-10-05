use std::collections::BTreeSet;

use orna_sys_v1::{
    PersistedRepositoryContext, PersistedRepositoryMetadata, RepositoryFormat,
    SystemCallableAvailability, SystemDispatchError, SystemFormatContext, system_api_json,
    system_api_selection, system_api_selection_json, system_callable_for, system_function_descriptor,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const FINAL_API_SHA256: &str = "10ef7dab9665de4e065ee2b96797b2751c7c3f98f9886241011aee0cc8c0d40e";

struct StoredRepositoryMetadata {
    format: u16,
    context: PersistedRepositoryContext,
}

impl PersistedRepositoryMetadata for StoredRepositoryMetadata {
    fn recorded_format_number(&self) -> u16 {
        self.format
    }

    fn recorded_context(&self) -> PersistedRepositoryContext {
        self.context
    }
}

fn recorded_contexts() -> (SystemFormatContext, SystemFormatContext, SystemFormatContext) {
    let final_context = SystemFormatContext::from_persisted(&StoredRepositoryMetadata {
        format: 3,
        context: PersistedRepositoryContext::Final20261005,
    })
    .expect("persisted final metadata is a valid system context");
    let original_context = SystemFormatContext::from_persisted(&StoredRepositoryMetadata {
        format: 1,
        context: PersistedRepositoryContext::Original100,
    })
    .expect("persisted original metadata is a valid system context");
    let draft_context = SystemFormatContext::from_persisted(&StoredRepositoryMetadata {
        format: 2,
        context: PersistedRepositoryContext::Previous110Draft,
    })
    .expect("persisted draft metadata is a valid system context");
    (final_context, original_context, draft_context)
}

#[test]
fn generated_final_api_matches_authority_release_bytes() {
    assert_eq!(
        format!("{:x}", Sha256::digest(system_api_json().as_bytes())),
        FINAL_API_SHA256
    );
}

#[test]
fn final_blob_inventory_is_admitted_only_to_format_three() {
    let (final_context, original_context, draft_context) = recorded_contexts();
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
        let callable =
            system_callable_for(final_context, name).expect("format 3 admits final Blob API");
        assert_eq!(
            callable.availability(),
            SystemCallableAvailability::FinalFormat3
        );
        assert!(matches!(
            system_callable_for(original_context, name),
            Err(SystemDispatchError::Unavailable { .. })
        ));
        assert!(matches!(
            system_callable_for(draft_context, name),
            Err(SystemDispatchError::Unavailable { .. })
        ));
    }
}

#[test]
fn retired_placement_apis_remain_historical_reader_only() {
    let (final_context, original_context, draft_context) = recorded_contexts();
    for name in [
        "sys.admin.set_storage_preference",
        "sys.admin.rewrite_storage",
    ] {
        assert_eq!(
            system_callable_for(original_context, name)
                .expect("original format retains historical placement API")
                .availability(),
            SystemCallableAvailability::HistoricalReadersOnly
        );
        assert_eq!(
            system_callable_for(draft_context, name)
                .expect("draft format retains historical placement API")
                .availability(),
            SystemCallableAvailability::HistoricalReadersOnly
        );
        assert!(matches!(
            system_callable_for(final_context, name),
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
    let (final_context, _, _) = recorded_contexts();
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
    assert_eq!(system_api_selection().available(final_context).count(), 82);
}

#[test]
fn persisted_context_is_typed_and_fail_closed() {
    let (final_context, _, _) = recorded_contexts();
    assert_eq!(final_context.format(), RepositoryFormat::Final3);
    assert_eq!(final_context.context_label(), "final-2026-10-05");
    assert!(SystemFormatContext::from_persisted(&StoredRepositoryMetadata {
        format: 3,
        context: PersistedRepositoryContext::Original100,
    })
    .is_err());
    assert!(SystemFormatContext::from_persisted(&StoredRepositoryMetadata {
        format: 99,
        context: PersistedRepositoryContext::Final20261005,
    })
    .is_err());
}
