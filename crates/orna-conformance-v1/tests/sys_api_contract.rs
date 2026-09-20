use std::collections::BTreeSet;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use orna_sys_v1::{
    AdmissionError, AdmissionRequest, ArgumentMap, ExecutionBoundary, FunctionDescriptor,
    FunctionId, FunctionIdentity, InvocationContext, InvocationExecutor, InvocationMode,
    InvocationResult, RevisionId, RuntimeId, RuntimeSupervisor, SnapshotId, TransactionMode,
    TypeId, TypeWitness, TypedValue,
};
use serde_json::Value;

const SYS_API: &str = include_str!("../../../api/sys.json");

fn array_len(document: &Value, name: &str) -> usize {
    document
        .get(name)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("{name} must be an array"))
        .len()
}

struct CountingExecutor(Arc<AtomicUsize>);

impl InvocationExecutor for CountingExecutor {
    fn execute(&mut self, _: &ExecutionBoundary) -> InvocationResult<TypedValue> {
        self.0.fetch_add(1, Ordering::SeqCst);
        InvocationResult::Success(TypedValue::public(TypeId::new("Contact"), []))
    }
}

fn incompatible_typed_request(mode: InvocationMode) -> AdmissionRequest<()> {
    AdmissionRequest {
        function: FunctionDescriptor {
            identity: FunctionIdentity {
                function: FunctionId::new("contact"),
                revision: RevisionId::from_bytes([0x61; 32]),
                snapshot: SnapshotId::new("s1"),
            },
            parameters: Vec::new(),
            result_type: TypeId::new("Contact"),
            visible: true,
            callable: true,
            generics_resolved: true,
        },
        arguments: ArgumentMap::default(),
        explicit_snapshot: None,
        mode,
        transaction: match mode {
            InvocationMode::Invoke => TransactionMode::Inherit,
            InvocationMode::Start => TransactionMode::Separate,
        },
        witness: TypeWitness::new(TypeId::new("Invoice")),
        idempotency_key: None,
        context: InvocationContext::default(),
    }
}

#[test]
fn sys_invoke_result_type_rejects_typed_invoke_and_start_before_effects() {
    // SYS-INVOKE-RESULT-TYPE-100 / ORNA-SYS-132: neither overload attempts a
    // conversion or reaches its executor when the explicit witness disagrees.
    let supervisor = RuntimeSupervisor::new(RuntimeId::new("runtime"));
    let invoke_calls = Arc::new(AtomicUsize::new(0));
    let mut invoke = CountingExecutor(Arc::clone(&invoke_calls));
    assert_eq!(
        supervisor.run(
            incompatible_typed_request(InvocationMode::Invoke),
            &mut invoke
        ),
        Err(AdmissionError::ReturnType)
    );
    assert_eq!(invoke_calls.load(Ordering::SeqCst), 0);

    let start_calls = Arc::new(AtomicUsize::new(0));
    assert_eq!(
        supervisor.start(
            incompatible_typed_request(InvocationMode::Start),
            CountingExecutor(Arc::clone(&start_calls)),
        ),
        Err(AdmissionError::ReturnType)
    );
    assert_eq!(start_calls.load(Ordering::SeqCst), 0);
}

fn object_keys(document: &Value, name: &str) -> BTreeSet<String> {
    document
        .get(name)
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("{name} must be an object"))
        .keys()
        .cloned()
        .collect()
}

fn sys_tokens(text: &str) -> impl Iterator<Item = &str> {
    text.split(|character: char| {
        !(character.is_ascii_alphanumeric() || matches!(character, '.' | '_'))
    })
    .filter(|token| {
        token.starts_with("sys.")
            && token
                .chars()
                .nth(4)
                .is_some_and(|character| character.is_ascii_uppercase())
    })
}

#[test]
fn portable_sys_api_has_exact_declared_counts_and_surface() {
    let document: Value = serde_json::from_str(SYS_API).expect("portable sys API JSON");
    assert_eq!(document["language_version"], "1.0.0");
    assert_eq!(document["sys_version"], "1.0");
    assert_eq!(document["status"], "specification");

    for (name, expected) in [
        ("singletons", 4),
        ("opaque_identifiers", 21),
        ("reference_aliases", 78),
        ("value_types", 34),
        ("relations", 78),
        ("functions", 66),
        ("failure_codes", 46),
    ] {
        assert_eq!(array_len(&document, name), expected, "{name} count");
    }
    assert_eq!(object_keys(&document, "enums").len(), 44, "enums count");

    let value_type_names = document["value_types"]
        .as_array()
        .expect("value types")
        .iter()
        .map(|value| value["name"].as_str().expect("value type name").to_owned())
        .collect::<BTreeSet<_>>();
    let mut known_sys_names = value_type_names;
    known_sys_names.extend(
        object_keys(&document, "enums")
            .into_iter()
            .chain(
                document["relations"]
                    .as_array()
                    .expect("relations")
                    .iter()
                    .map(|value| value["name"].as_str().expect("relation name").to_owned()),
            )
            .chain(
                document["opaque_identifiers"]
                    .as_array()
                    .expect("opaque identifiers")
                    .iter()
                    .map(|value| value.as_str().expect("opaque identifier").to_owned()),
            )
            .chain(
                document["reference_aliases"]
                    .as_array()
                    .expect("reference aliases")
                    .iter()
                    .map(|value| value["name"].as_str().expect("reference alias").to_owned()),
            ),
    );
    known_sys_names.extend(
        document["functions"]
            .as_array()
            .expect("functions")
            .iter()
            .map(|value| value["name"].as_str().expect("function name"))
            .map(|name| {
                name.split('<')
                    .next()
                    .expect("function base name")
                    .to_owned()
            }),
    );
    let generic_bases = known_sys_names
        .iter()
        .filter_map(|name| name.split('<').next())
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    known_sys_names.extend(generic_bases);

    for value in document["value_types"]
        .as_array()
        .expect("value types")
        .iter()
        .chain(document["relations"].as_array().expect("relations"))
    {
        for field in value["fields"].as_array().expect("fields") {
            for token in sys_tokens(field["type"].as_str().expect("field type")) {
                assert!(
                    known_sys_names.contains(token),
                    "unresolved type token {token}"
                );
            }
        }
    }
    let function_names = document["functions"]
        .as_array()
        .expect("functions")
        .iter()
        .map(|value| value["name"].as_str().expect("function name"))
        .collect::<BTreeSet<_>>();
    for required in [
        "sys.invoke(Value)",
        "sys.invoke<T>",
        "sys.start(Value)",
        "sys.start<T>",
        "sys.await",
        "sys.cancel",
        "sys.rt.info",
    ] {
        assert!(function_names.contains(required), "missing {required}");
    }
    for required in [
        "sys.Value",
        "sys.Argument",
        "sys.ArgumentMap",
        "sys.InvocationHandle<T>",
        "sys.InvocationResult<T>",
        "sys.RuntimeView",
        "sys.ReplView",
        "sys.CurrentContext",
    ] {
        assert!(known_sys_names.contains(required), "missing {required}");
    }

    assert_eq!(
        document["removed_names"]["sys.runtime"]["diagnostic"],
        "ORNA100-E-SYS-RUNTIME"
    );
    assert_eq!(
        document["removed_names"]["sys.runtime_info"]["diagnostic"],
        "ORNA100-E-SYS-RUNTIME"
    );
    assert!(
        document["value_types"]
            .as_array()
            .expect("value types")
            .iter()
            .any(|value| value["name"] == "sys.InvocationHandle<T>"
                && value["invariants"]
                    .as_array()
                    .expect("handle invariants")
                    .iter()
                    .any(|invariant| invariant == "resumable is false in 1.0"))
    );
}

#[test]
fn sys_session_schema_binds_the_live_runtime_relation_without_runtime_claims() {
    let document: Value = serde_json::from_str(SYS_API).expect("portable sys API JSON");
    let relation = document["relations"]
        .as_array()
        .expect("relations")
        .iter()
        .find(|value| value["name"] == "sys.Session")
        .expect("sys.Session relation");

    assert_eq!(relation["grouped_handle"], "sys.rt.sessions");
    assert_eq!(relation["availability"], "live");
    assert_eq!(relation["key"], "id");
    assert_eq!(relation["writable"], false);
    assert_eq!(relation["reference_type"], "sys.SessionRef");
    assert_eq!(relation["key_fields"], serde_json::json!(["id"]));

    let fields = relation["fields"]
        .as_array()
        .expect("sys.Session fields")
        .iter()
        .map(|field| {
            (
                field["name"].as_str().expect("session field name"),
                field["type"].as_str().expect("session field type"),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        fields,
        vec![
            ("reference", "sys.SessionRef"),
            ("id", "sys.SessionId"),
            ("started", "Instant"),
            ("last_seen", "Instant"),
            ("client", "sys.ClientRef?"),
            ("locale", "Locale"),
            ("timezone", "TimeZone"),
            ("renderer", "Str?"),
        ]
    );

    let session_ref = document["reference_aliases"]
        .as_array()
        .expect("reference aliases")
        .iter()
        .find(|value| value["name"] == "sys.SessionRef")
        .expect("sys.SessionRef alias");
    assert_eq!(session_ref["target"], "sys.Session");
    assert_eq!(session_ref["definition"], "sys.RowRef<sys.Session>");

    let runtime_view = document["value_types"]
        .as_array()
        .expect("value types")
        .iter()
        .find(|value| value["name"] == "sys.RuntimeView")
        .expect("sys.RuntimeView");
    let session_handle = runtime_view["fields"]
        .as_array()
        .expect("runtime view fields")
        .iter()
        .find(|field| field["name"] == "sessions")
        .expect("runtime session handle");
    assert_eq!(session_handle["type"], "Relation<sys.Session>");
}

#[test]
fn sys_context_view_schemas_match_the_published_contract() {
    let document: Value = serde_json::from_str(SYS_API).expect("portable sys API JSON");

    for (name, expected_fields) in [
        (
            "sys.CurrentContext",
            vec![
                ("snapshot", "sys.SnapshotRef"),
                ("transaction", "sys.TransactionRef?"),
                ("invocation", "sys.InvocationRef?"),
                ("run", "sys.RunRef?"),
                ("session", "sys.SessionRef?"),
                ("client", "sys.ClientRef?"),
                ("logical_cwd", "Path"),
                ("locale", "Locale"),
                ("timezone", "TimeZone"),
                ("trace", "sys.TraceRef?"),
                ("cancellation", "sys.CancellationView"),
                ("present_context", "PresentContext"),
            ],
        ),
        (
            "sys.ReplView",
            vec![
                ("session", "sys.SessionRef"),
                ("renderer", "Str"),
                ("width", "Int?"),
                ("height", "Int?"),
                ("locale", "Locale"),
                ("timezone", "TimeZone"),
                (
                    "presentation_overrides",
                    "Relation<sys.PresentationOverride>",
                ),
                ("watched_expressions", "Relation<sys.Watch>"),
            ],
        ),
    ] {
        let value_type = document["value_types"]
            .as_array()
            .expect("value types")
            .iter()
            .find(|value| value["name"] == name)
            .unwrap_or_else(|| panic!("{name}"));
        assert_eq!(value_type["kind"], "record", "{name} kind");
        assert_eq!(value_type["type_parameters"], serde_json::json!([]));
        let fields = value_type["fields"]
            .as_array()
            .expect("context-view fields")
            .iter()
            .map(|field| {
                (
                    field["name"].as_str().expect("field name"),
                    field["type"].as_str().expect("field type"),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(fields, expected_fields, "{name} fields");
    }

    let singleton = |name: &str| {
        document["singletons"]
            .as_array()
            .expect("singletons")
            .iter()
            .find(|value| value["name"] == name)
            .unwrap_or_else(|| panic!("{name} singleton"))
    };
    assert_eq!(singleton("sys.current")["type"], "sys.CurrentContext");
    assert_eq!(singleton("sys.current")["availability"], "every activation");
    assert_eq!(singleton("sys.repl")["type"], "sys.ReplView");
    assert_eq!(
        singleton("sys.repl")["availability"],
        "REPL only; otherwise sys.context.repl_unavailable"
    );
}

#[test]
fn foundation_typed_reference_aliases_match_published_targets() {
    let document: Value = serde_json::from_str(SYS_API).expect("portable sys API JSON");
    let aliases = document["reference_aliases"]
        .as_array()
        .expect("reference aliases");

    // These are the foundation-v1 TypedRowRef aliases whose published sys
    // aliases have an exact RowRef target.  This pins schema identity only;
    // it deliberately does not assert a physical table ID or row authority.
    for (name, target) in [
        ("sys.FileRef", "sys.File"),
        ("sys.SnapshotRef", "sys.Snapshot"),
        ("sys.DiagnosticRef", "sys.Diagnostic"),
        ("sys.ObjectRef", "sys.Object"),
        ("sys.DefinitionRef", "sys.Definition"),
        ("sys.TypeRef", "sys.Type"),
        ("sys.TraceRef", "sys.Trace"),
        ("sys.AssertionRef", "sys.Assertion"),
        ("sys.FunctionRef", "sys.Function"),
        ("sys.InvocationRef", "sys.Invocation"),
        ("sys.InvocationArgumentRef", "sys.InvocationArgument"),
        ("sys.RunRef", "sys.Run"),
        ("sys.StreamRef", "sys.Stream"),
        ("sys.CheckpointRef", "sys.Checkpoint"),
        ("sys.FailureRef", "sys.Failure"),
    ] {
        let alias = aliases
            .iter()
            .find(|value| value["name"] == name)
            .unwrap_or_else(|| panic!("missing published alias {name}"));
        assert_eq!(alias["target"], target, "target for {name}");
        assert_eq!(alias["definition"], format!("sys.RowRef<{target}>"));
    }
}

#[test]
fn sys_runtime_info_schema_matches_the_published_compatibility_contract() {
    let document: Value = serde_json::from_str(SYS_API).expect("portable sys API JSON");
    let runtime = document["value_types"]
        .as_array()
        .expect("value types")
        .iter()
        .find(|value| value["name"] == "sys.RuntimeInfo")
        .expect("sys.RuntimeInfo");

    assert_eq!(runtime["kind"], "record");
    assert_eq!(runtime["type_parameters"], serde_json::json!([]));
    assert_eq!(
        runtime["fields"],
        serde_json::json!([
            {"name": "language_version", "type": "Str"},
            {"name": "sys_version", "type": "Str"},
            {"name": "canonical_orna_codec_version", "type": "Str"},
            {"name": "repository_layout_version", "type": "Str"},
            {"name": "storage_manifest_version", "type": "Str"},
            {"name": "presentation_protocol_version", "type": "Str"},
            {"name": "implementation_name", "type": "Str"},
            {"name": "implementation_version", "type": "Str"},
            {"name": "build_id", "type": "Str"},
            {"name": "supported_profiles", "type": "[Str]"},
            {"name": "runtime", "type": "sys.RuntimeId"},
            {"name": "mode", "type": "sys.RuntimeMode"},
            {"name": "read_only", "type": "Bool"},
            {"name": "std_snapshot", "type": "sys.SnapshotRef?"},
            {"name": "unicode_version", "type": "Str"},
            {"name": "timezone_database_version", "type": "Str?"}
        ])
    );

    let singleton = document["singletons"]
        .as_array()
        .expect("singletons")
        .iter()
        .find(|value| value["name"] == "sys.rt")
        .expect("sys.rt singleton");
    assert_eq!(singleton["type"], "sys.RuntimeView");

    let info = document["functions"]
        .as_array()
        .expect("functions")
        .iter()
        .find(|value| value["name"] == "sys.rt.info")
        .expect("sys.rt.info function");
    assert_eq!(info["signature"], "fn sys.rt.info(): sys.RuntimeInfo");
    assert_eq!(info["effect"], "read");
}

#[test]
fn sys_client_lease_listener_schemas_match_the_published_contract() {
    let document: Value = serde_json::from_str(SYS_API).expect("portable sys API JSON");

    let assert_relation = |name: &str,
                           grouped_handle: &str,
                           availability: &str,
                           key: &str,
                           reference_type: &str,
                           fields: &[(&str, &str)]| {
        let relation = document["relations"]
            .as_array()
            .expect("relations")
            .iter()
            .find(|value| value["name"] == name)
            .unwrap_or_else(|| panic!("{name} relation"));

        assert_eq!(relation["grouped_handle"], grouped_handle);
        assert_eq!(relation["kind"], "relation");
        assert_eq!(relation["availability"], availability);
        assert_eq!(relation["key"], key);
        assert_eq!(relation["writable"], false);
        assert_eq!(relation["reference_type"], reference_type);
        assert_eq!(relation["key_fields"], serde_json::json!([key]));
        assert_eq!(
            relation["fields"],
            serde_json::Value::Array(
                fields
                    .iter()
                    .map(|(field_name, field_type)| {
                        serde_json::json!({"name": field_name, "type": field_type})
                    })
                    .collect()
            )
        );
    };

    assert_relation(
        "sys.Client",
        "sys.rt.clients",
        "live",
        "id",
        "sys.ClientRef",
        &[
            ("reference", "sys.ClientRef"),
            ("id", "sys.ClientId"),
            ("kind", "sys.ClientKind"),
            ("connected", "Instant"),
            ("last_seen", "Instant"),
            ("protocol_version", "Str?"),
            ("remote", "Str?"),
            ("redacted", "Bool"),
        ],
    );
    assert_relation(
        "sys.Lease",
        "sys.rt.leases",
        "local-durable",
        "name",
        "sys.LeaseRef",
        &[
            ("reference", "sys.LeaseRef"),
            ("name", "Str"),
            ("holder", "sys.RuntimeId"),
            ("status", "sys.LeaseStatus"),
            ("acquired", "Instant"),
            ("expires", "Instant?"),
            ("generation", "Int"),
        ],
    );
    assert_relation(
        "sys.Listener",
        "sys.rt.listeners",
        "live",
        "id",
        "sys.ListenerRef",
        &[
            ("reference", "sys.ListenerRef"),
            ("id", "Str"),
            ("kind", "sys.ListenerKind"),
            ("address", "Str"),
            ("started", "Instant"),
            ("clients", "Int"),
            ("tls", "Bool"),
            ("authenticated", "Bool"),
        ],
    );

    for (name, target, definition) in [
        ("sys.ClientRef", "sys.Client", "sys.RowRef<sys.Client>"),
        ("sys.LeaseRef", "sys.Lease", "sys.RowRef<sys.Lease>"),
        (
            "sys.ListenerRef",
            "sys.Listener",
            "sys.RowRef<sys.Listener>",
        ),
    ] {
        let reference = document["reference_aliases"]
            .as_array()
            .expect("reference aliases")
            .iter()
            .find(|value| value["name"] == name)
            .unwrap_or_else(|| panic!("{name} alias"));
        assert_eq!(reference["target"], target);
        assert_eq!(reference["definition"], definition);
    }

    for (name, expected) in [
        (
            "sys.ClientKind",
            serde_json::json!(["cli", "repl", "server", "renderer", "embedded", "tool"]),
        ),
        (
            "sys.LeaseStatus",
            serde_json::json!(["acquiring", "held", "releasing", "expired", "lost"]),
        ),
        (
            "sys.ListenerKind",
            serde_json::json!([
                "git_http",
                "query_http",
                "websocket",
                "renderer",
                "admin",
                "custom"
            ]),
        ),
    ] {
        assert_eq!(document["enums"][name], expected, "{name} values");
    }

    let runtime_view = document["value_types"]
        .as_array()
        .expect("value types")
        .iter()
        .find(|value| value["name"] == "sys.RuntimeView")
        .expect("sys.RuntimeView");
    for (field_name, field_type) in [
        ("clients", "Relation<sys.Client>"),
        ("leases", "Relation<sys.Lease>"),
        ("listeners", "Relation<sys.Listener>"),
    ] {
        let field = runtime_view["fields"]
            .as_array()
            .expect("runtime view fields")
            .iter()
            .find(|field| field["name"] == field_name)
            .unwrap_or_else(|| panic!("sys.RuntimeView.{field_name}"));
        assert_eq!(field["type"], field_type);
    }
}

#[test]
fn sys_source_span_schema_matches_the_published_contract() {
    let document: Value = serde_json::from_str(SYS_API).expect("portable sys API JSON");
    let source_span = document["value_types"]
        .as_array()
        .expect("value types")
        .iter()
        .find(|value| value["name"] == "sys.SourceSpan")
        .expect("sys.SourceSpan");

    assert_eq!(source_span["kind"], "record");
    assert_eq!(source_span["type_parameters"], serde_json::json!([]));
    assert_eq!(
        source_span["fields"],
        serde_json::json!([
            {"name": "file", "type": "sys.FileRef"},
            {"name": "start_byte", "type": "Int"},
            {"name": "end_byte", "type": "Int"},
            {"name": "start_line", "type": "Int"},
            {"name": "start_column", "type": "Int"},
            {"name": "end_line", "type": "Int"},
            {"name": "end_column", "type": "Int"}
        ])
    );
    assert_eq!(
        source_span["invariants"],
        serde_json::json!([
            "0 <= start_byte <= end_byte",
            "line and column values are one-based"
        ])
    );
}
