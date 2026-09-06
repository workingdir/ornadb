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
                revision: RevisionId::new("r1"),
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
