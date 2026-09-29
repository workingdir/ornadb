use std::{
    collections::BTreeMap,
    process::Command,
};

use futures::executor::block_on;
use orna_core::{CatalogueRevisionId, SchemaId, catalogue::CatalogueSnapshot, catalogue::SchemaDefinition, catalogue::QualifiedSemanticName, catalogue_diff::{SemanticChange, catalogue_diff}};
use orna_foundation_v1::{CanonicalValue, OvbRaw, SchemaDescriptor};
use orna_live_v1::{Error as LiveError, Limits as LiveLimits, LiveCredentialIssuer, LiveHost, LiveSessionAuthority, LiveTransport, SessionMetadata, TransportLimits, WireRequest};
use orna_repository_v1::CompactCommittedSegmentProjection;
use orna_runtime_v1::{PublicationRowEncoding, PublicationValueEncoding};
use orna_security_v1::{CredentialIssuer, Origin, OriginPolicy, SecretMetadata, SecretRef, SecretResolver, SessionBoundary, SessionDeletionAdapter};
use orna_serving_v1::{Limits as ServingLimits, Serving};
use orna_storage_v1::{CompactBaseState, CompactKeyIdentity, CompactOvbProfile, CompactWriterInput, CompactWriterMutation, CompactWriterMutationState, fold_compact_committed_base};
use orna_sys_v1::{Diagnostic, DiagnosticField, TypeId, TypedValue};
use orna_syntax_v1::parse_module;
use tempfile::tempdir;

const SOURCE: &str = include_str!("fixtures/traceability-diff-trust-secret.orna");
const TEST_SECRET: &[u8] = b"fixture-secret-material-must-not-render";

fn require_fixture() {
    assert!(parse_module(SOURCE).diagnostics.is_empty());
}

#[test]
fn semantic_catalogue_diff_reports_a_schema_addition() {
    require_fixture();
    let base = CatalogueSnapshot::new(CatalogueRevisionId::from_bytes([1; 16]), vec![], vec![])
        .unwrap();
    let candidate = CatalogueSnapshot::new(
        CatalogueRevisionId::from_bytes([2; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([3; 16]),
            QualifiedSemanticName::new(["audit"]).unwrap(),
        )],
        vec![],
    )
    .unwrap();

    assert!(matches!(
        catalogue_diff(&base, &candidate).changes(),
        [SemanticChange::SchemaAdded { name, .. }] if name == "audit"
    ));
}

const TABLE: [u8; 16] = [0x11; 16];
const KEY_FIELD: [u8; 16] = [0x22; 16];
const VALUE_FIELD: [u8; 16] = [0x33; 16];

fn uuid_raw(value: [u8; 16]) -> OvbRaw {
    OvbRaw::Tag(37, Box::new(OvbRaw::Bytes(value.to_vec())))
}

fn primitive(name: &str) -> OvbRaw {
    OvbRaw::Array(vec![OvbRaw::Int(0.into()), OvbRaw::Text(name.into())])
}

fn field(id: [u8; 16], name: &str, ty: OvbRaw, role: u64) -> OvbRaw {
    OvbRaw::Array(vec![
        uuid_raw(id),
        OvbRaw::Text(name.into()),
        ty,
        OvbRaw::Int(role.into()),
        OvbRaw::Array(vec![OvbRaw::Int(0.into())]),
    ])
}

fn compact_profile() -> CompactOvbProfile {
    let raw = OvbRaw::Map(vec![
        (OvbRaw::Int(0.into()), OvbRaw::Int(1.into())),
        (OvbRaw::Int(1.into()), uuid_raw(TABLE)),
        (
            OvbRaw::Int(2.into()),
            OvbRaw::Array(vec![uuid_raw(KEY_FIELD)]),
        ),
        (
            OvbRaw::Int(3.into()),
            OvbRaw::Array(vec![
                field(KEY_FIELD, "id", primitive("Int"), 0),
                field(VALUE_FIELD, "name", primitive("Str"), 1),
            ]),
        ),
        (OvbRaw::Int(4.into()), OvbRaw::Array(vec![])),
    ]);
    CompactOvbProfile::new(SchemaDescriptor::new(raw).unwrap()).unwrap()
}

fn compact_row(name: &str) -> Vec<u8> {
    CanonicalValue::new(OvbRaw::Tag(
        60009,
        Box::new(OvbRaw::Array(vec![
            OvbRaw::Null,
            OvbRaw::Array(vec![
                OvbRaw::Array(vec![uuid_raw(KEY_FIELD), OvbRaw::Int(7.into())]),
                OvbRaw::Array(vec![uuid_raw(VALUE_FIELD), OvbRaw::Text(name.into())]),
            ]),
        ])),
    ))
    .unwrap()
    .encode()
    .unwrap()
}

fn state_with_row(profile: &CompactOvbProfile, generation: u64, name: &str) -> CompactBaseState {
    let base = fold_compact_committed_base(
        profile,
        std::iter::empty::<&CompactCommittedSegmentProjection>(),
        generation,
    )
    .unwrap();
    let key_bytes = CanonicalValue::new(OvbRaw::Int(7.into()))
        .unwrap()
        .encode()
        .unwrap();
    let key: CompactKeyIdentity = profile.decode_key(&key_bytes).unwrap();
    let input = CompactWriterInput {
        table_id: profile.table_id(),
        schema_fingerprint: profile.schema_fingerprint(),
        candidate_generation: generation,
        row_encoding_identity: PublicationRowEncoding::CompactOvb1,
        value_encoding_identity: PublicationValueEncoding::Ovb1,
        mutations: vec![CompactWriterMutation {
            sequence: 1,
            mutation_id: [u8::try_from(generation).unwrap(); 16],
            key,
            state: CompactWriterMutationState::Replacement {
                value: compact_row(name),
            },
        }],
        candidate_digest: [0x44; 32],
    };
    base.apply_writer_input(&input).unwrap()
}

#[test]
fn compact_generations_are_distinct_from_logical_row_changes() {
    require_fixture();
    let profile = compact_profile();
    let compacted = state_with_row(&profile, 1, "Ada");
    let rewritten = state_with_row(&profile, 9, "Ada");
    let changed = state_with_row(&profile, 10, "Grace");

    assert_ne!(
        compacted.rows().next().unwrap().generation(),
        rewritten.rows().next().unwrap().generation()
    );
    assert!(compacted.has_same_logical_rows(&rewritten));
    assert!(!compacted.has_same_logical_rows(&changed));
}

struct Authority;
impl LiveSessionAuthority for Authority {
    fn create_session(
        &mut self,
        database: [u8; 16],
        now: u64,
    ) -> Result<SessionMetadata, LiveError> {
        Ok(SessionMetadata {
            session: [1; 16],
            database,
            runtime: [2; 16],
            expires_at: now + 100,
            subscribe: vec![],
        })
    }
}

struct Issuer(Option<[u8; 32]>);
impl CredentialIssuer for Issuer {
    fn issue_credential(&mut self) -> Result<[u8; 32], orna_security_v1::BoundaryError> {
        let value = [5; 32];
        self.0 = Some(value);
        Ok(value)
    }
}
impl LiveCredentialIssuer for Issuer {
    fn last_issued(&self) -> Option<[u8; 32]> {
        self.0
    }
}

struct Delete;
impl SessionDeletionAdapter for Delete {
    type Error = ();
    fn delete(&mut self, _: orna_security_v1::SessionId) -> Result<(), Self::Error> {
        Ok(())
    }
}
fn live_transport() -> LiveTransport {
    let origin = Origin::parse("https://app.example").unwrap();
    let host = LiveHost::new(
        LiveLimits::default(),
        SessionBoundary::new(OriginPolicy::new([origin], []), 10),
        Serving::new(ServingLimits::default()).unwrap(),
    )
    .unwrap();
    LiveTransport::new(host, TransportLimits::default()).unwrap()
}

#[test]
fn default_live_listener_reports_loopback_exposure() {
    require_fixture();
    let listener = LiveTransport::bind_default_listener(0).unwrap();
    assert!(listener.status().address.ip().is_loopback());
    assert_eq!(listener.status().exposure, orna_live_v1::ListenerExposure::Loopback);
}

#[test]
fn anonymous_delete_is_denied_before_session_mutation() {
    require_fixture();
    let mut transport = live_transport();
    let mut authority = Authority;
    let mut issuer = Issuer(None);
    let mut deletion = Delete;
    let response = block_on(transport.handle(
        WireRequest {
            method: "DELETE".into(),
            path: "/orna/session/00000000-0000-0000-0000-000000000001".into(),
            headers: vec![("origin".into(), "https://app.example".into())],
            body: Vec::new(),
        },
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    assert_eq!(response.status, 401);
}

#[test]
fn secret_reference_fixture_contains_a_stable_name_not_secret_material() {
    require_fixture();
    assert!(SOURCE.contains("std.secret.ref(\"google.personal\")"));
    assert!(!SOURCE
        .as_bytes()
        .windows(TEST_SECRET.len())
        .any(|window| window == TEST_SECRET));

    let directory = tempdir().unwrap();
    let repository = directory.path();
    assert!(Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(repository)
        .status()
        .unwrap()
        .success());
    std::fs::write(repository.join("safe-reference.orna"), SOURCE).unwrap();
    assert!(Command::new("git")
        .args(["add", "--", "safe-reference.orna"])
        .current_dir(repository)
        .status()
        .unwrap()
        .success());
    let staged = Command::new("git")
        .args(["show", ":safe-reference.orna"])
        .current_dir(repository)
        .output()
        .unwrap();
    assert!(staged.status.success());
    assert!(String::from_utf8_lossy(&staged.stdout).contains("google.personal"));
    assert!(!staged.stdout.windows(TEST_SECRET.len()).any(|window| window == TEST_SECRET));
}

#[test]
fn protected_sys_secret_values_are_redacted_from_retained_values() {
    require_fixture();
    let value = TypedValue::protected(TypeId::new("sys.Secret"), TEST_SECRET);
    assert!(value.is_redacted());
    assert_eq!(value.canonical(), None);
    assert!(!format!("{value:?}").contains(std::str::from_utf8(TEST_SECRET).unwrap()));

    let diagnostic = Diagnostic {
        code: "sys.secret.denied",
        message: "secret resolution denied",
        fields: BTreeMap::from([(
            "secret".into(),
            DiagnosticField::Redacted {
                static_type: TypeId::new("sys.Secret"),
            },
        )]),
        causes: Vec::new(),
    };
    let rendered = serde_json::to_string(&diagnostic).unwrap();
    assert!(!rendered.contains(std::str::from_utf8(TEST_SECRET).unwrap()));
    assert!(rendered.contains("redacted"));
}

struct Resolver;
impl SecretResolver for Resolver {
    fn metadata(
        &self,
        reference: &SecretRef,
    ) -> Result<SecretMetadata, orna_security_v1::SecretBoundaryError> {
        Ok(SecretMetadata::new(reference.clone(), "test-provider", true))
    }

    fn with_secret<T>(
        &self,
        _: &SecretRef,
        operation: &mut dyn FnMut(&[u8]) -> T,
    ) -> Result<T, orna_security_v1::SecretBoundaryError> {
        Ok(operation(TEST_SECRET))
    }
}

#[test]
fn secret_ref_displays_by_stable_name_and_not_resolved_value() {
    require_fixture();
    let reference = SecretRef::new("google.personal").unwrap();
    let metadata = Resolver.metadata(&reference).unwrap();
    let display = format!("{reference} {metadata:?}");
    assert!(display.contains("google.personal"));
    assert!(display.contains("test-provider"));
    assert!(!display.contains(std::str::from_utf8(TEST_SECRET).unwrap()));
    let mut consume = |value: &[u8]| value.len();
    assert_eq!(Resolver.with_secret(&reference, &mut consume), Ok(TEST_SECRET.len()));
}

#[test]
fn secret_metadata_exposes_name_provider_and_availability_without_contents() {
    require_fixture();
    let reference = SecretRef::new("google.personal").unwrap();
    let metadata = Resolver.metadata(&reference).unwrap();
    assert_eq!(metadata.reference(), &reference);
    assert_eq!(metadata.provider(), "test-provider");
    assert!(metadata.available());
    let rendered = format!("{metadata:?}");
    assert!(rendered.contains("google.personal"));
    assert!(!rendered.contains(std::str::from_utf8(TEST_SECRET).unwrap()));
}
