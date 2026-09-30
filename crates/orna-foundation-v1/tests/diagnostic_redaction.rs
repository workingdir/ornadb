use orna_foundation_v1::{
    Diagnostic, DiagnosticSeverity, DiagnosticSpan, GitHash, OvbRaw, SafeText, Snapshot, Value,
};

fn diagnostic_with_secret_text() -> Diagnostic {
    let cause = Diagnostic::new(
        SafeText::new("ORNA-E-NESTED").unwrap(),
        DiagnosticSeverity::Warning,
        SafeText::new("nested connector token: nested-secret-value").unwrap(),
    )
    .unwrap()
    .with_note(SafeText::new("nested authorization: nested-secret-note").unwrap())
    .with_reference([0xcd; 16]);

    Diagnostic::new(
        SafeText::new("ORNA-E-SECRET").unwrap(),
        DiagnosticSeverity::Error,
        SafeText::new("connector secret: root-secret-value").unwrap(),
    )
    .unwrap()
    .with_span(
        DiagnosticSpan::new(
            Snapshot::Commit {
                database: [7; 16],
                algorithm: GitHash::Sha256,
                oid: vec![9; 32],
            },
            "src/main.orna",
            4.into(),
            9.into(),
        )
        .unwrap(),
    )
    .with_note(SafeText::new("authorization: root-secret-note").unwrap())
    .with_cause(cause)
    .with_reference([0xab; 16])
}

#[test]
fn redaction_is_recursive_before_ovb_and_json_boundaries() {
    assert_redacted_boundaries(diagnostic_with_secret_text().redacted(), "<redacted>");
}

#[test]
fn admitted_root_message_does_not_disclose_notes_or_nested_causes() {
    let message = String::from("operation denied");
    let redacted = diagnostic_with_secret_text()
        .redacted_with_message(SafeText::new(message.clone()).unwrap());
    let encoded = redacted.encode_ovb().unwrap();
    assert!(
        encoded
            .windows(message.len())
            .any(|window| window == message.as_bytes())
    );
    assert_eq!(Diagnostic::decode_ovb(&encoded).unwrap().message(), "<redacted>");
    assert_redacted_boundaries(redacted, &message);
}

#[test]
fn diagnostic_debug_redacts_fixture_payloads_without_explicit_redaction() {
    let fixture = include_str!("fixtures/secret-surface.orna").trim();
    let cause = Diagnostic::new(
        SafeText::new(fixture).unwrap(),
        DiagnosticSeverity::Warning,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .with_note(SafeText::new(fixture).unwrap());
    let diagnostic = Diagnostic::new(
        SafeText::new(fixture).unwrap(),
        DiagnosticSeverity::Error,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .with_span(
        DiagnosticSpan::new(
            Snapshot::Commit {
                database: [7; 16],
                algorithm: GitHash::Sha256,
                oid: vec![9; 32],
            },
            "secrets/credential.orna",
            0.into(),
            1.into(),
        )
        .unwrap(),
    )
    .with_note(SafeText::new(fixture).unwrap())
    .with_cause(cause);

    let debug = format!("{diagnostic:?}");
    assert!(debug.contains("<redacted>"));
    assert!(!debug.contains(fixture));
    assert!(!debug.contains("credential.orna"));
}

#[test]
fn composed_json_and_codec_projections_redact_fixture_tails() {
    let fixture = include_str!("fixtures/secret-surface.orna").trim();
    let cause = Diagnostic::new(
        SafeText::new("ORNA-E-CHILD").unwrap(),
        DiagnosticSeverity::Warning,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .with_note(SafeText::new(fixture).unwrap());
    let diagnostic = Diagnostic::new(
        SafeText::new("ORNA-E-ROOT").unwrap(),
        DiagnosticSeverity::Error,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .with_note(SafeText::new(fixture).unwrap())
    .with_cause(cause);

    let admitted = diagnostic
        .clone()
        .redacted_with_message(SafeText::new("operation denied").unwrap())
        // A later composition must not undo the redaction claim.
        .with_note(SafeText::new(fixture).unwrap());
    let composed_json = serde_json::json!({
        "outer": {
            "diagnostic": diagnostic,
            "admitted": admitted,
        }
    });
    let json = serde_json::to_vec(&composed_json).unwrap();
    assert!(
        !json
            .windows(fixture.len())
            .any(|window| window == fixture.as_bytes())
    );
    assert_eq!(
        composed_json["outer"]["diagnostic"]["message"],
        "<redacted>"
    );
    assert_eq!(composed_json["outer"]["diagnostic"]["redacted"], true);
    assert_eq!(
        composed_json["outer"]["diagnostic"]["causes"][0]["message"],
        "<redacted>"
    );
    assert_eq!(
        composed_json["outer"]["admitted"]["message"],
        "operation denied"
    );
    assert_eq!(
        composed_json["outer"]["admitted"]["notes"][0],
        "<redacted>"
    );

    let codec_diagnostic = Diagnostic::new(
        SafeText::new("ORNA-E-CODEC").unwrap(),
        DiagnosticSeverity::Error,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .with_cause(
        Diagnostic::new(
            SafeText::new("ORNA-E-CODEC-CAUSE").unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap(),
    );
    let encoded = codec_diagnostic.encode_ovb().unwrap();
    assert!(
        !encoded
            .windows(fixture.len())
            .any(|window| window == fixture.as_bytes())
    );
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&encoded).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    assert_eq!(decoded["causes"][0]["message"], "<redacted>");
}

#[test]
fn local_message_admission_does_not_transfer_to_nested_diagnostics() {
    let fixture = include_str!("fixtures/secret-surface.orna").trim();
    let admitted_cause = Diagnostic::new(
        SafeText::new("ORNA-E-ADMITTED-CAUSE").unwrap(),
        DiagnosticSeverity::Warning,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .redacted_with_message(SafeText::new(fixture).unwrap());
    let diagnostic = Diagnostic::new(
        SafeText::new("ORNA-E-ADMITTED-ROOT").unwrap(),
        DiagnosticSeverity::Error,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .redacted_with_message(SafeText::new("operation denied").unwrap())
    // Composition after admission cannot pass the root trust mark to a cause.
    .with_cause(admitted_cause);

    let json = serde_json::to_value(&diagnostic).unwrap();
    assert_eq!(json["message"], "operation denied");
    assert_eq!(json["causes"][0]["message"], "<redacted>");
    let serialized = serde_json::to_vec(&json).unwrap();
    assert!(
        !serialized
            .windows(fixture.len())
            .any(|window| window == fixture.as_bytes())
    );

    let encoded = diagnostic.encode_ovb().unwrap();
    assert!(
        !encoded
            .windows(fixture.len())
            .any(|window| window == fixture.as_bytes())
    );
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&encoded).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    assert_eq!(decoded["causes"][0]["message"], "<redacted>");
}

#[test]
fn local_trust_stays_on_root_across_cause_composition_orders() {
    let fixture = include_str!("fixtures/secret-surface.orna").trim();
    let admitted_grandchild = Diagnostic::new(
        SafeText::new("ORNA-E-ADMITTED-GRANDCHILD").unwrap(),
        DiagnosticSeverity::Warning,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .with_note(SafeText::new(fixture).unwrap())
    .redacted_with_message(SafeText::new(fixture).unwrap());
    let admitted_child = Diagnostic::new(
        SafeText::new("ORNA-E-ADMITTED-CHILD").unwrap(),
        DiagnosticSeverity::Warning,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .with_note(SafeText::new(fixture).unwrap())
    .with_cause(admitted_grandchild)
    .redacted_with_message(SafeText::new(fixture).unwrap());

    let parent_admitted_before_cause = Diagnostic::new(
        SafeText::new("ORNA-E-PARENT-FIRST").unwrap(),
        DiagnosticSeverity::Error,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .redacted_with_message(SafeText::new("admitted parent message").unwrap())
    .with_cause(admitted_child.clone());
    let parent_admitted_after_cause = Diagnostic::new(
        SafeText::new("ORNA-E-CAUSE-FIRST").unwrap(),
        DiagnosticSeverity::Error,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .with_cause(admitted_child)
    .redacted_with_message(SafeText::new("admitted parent message").unwrap());

    for diagnostic in [parent_admitted_before_cause, parent_admitted_after_cause] {
        let json = serde_json::to_vec(&diagnostic).unwrap();
        let projection = serde_json::to_value(&diagnostic).unwrap();
        let encoded = diagnostic.encode_ovb().unwrap();

        assert_eq!(projection["message"], "admitted parent message");
        assert_eq!(projection["redacted"], true);
        assert_eq!(projection["causes"][0]["message"], "<redacted>");
        assert_eq!(projection["causes"][0]["notes"][0], "<redacted>");
        assert_eq!(
            projection["causes"][0]["causes"][0]["message"],
            "<redacted>"
        );
        assert_eq!(
            projection["causes"][0]["causes"][0]["notes"][0],
            "<redacted>"
        );
        for bytes in [&json, &encoded] {
            assert!(
                !bytes
                    .windows(fixture.len())
                    .any(|window| window == fixture.as_bytes())
            );
        }

        let decoded = serde_json::to_value(Diagnostic::decode_ovb(&encoded).unwrap()).unwrap();
        assert_eq!(decoded["message"], "<redacted>");
        assert_eq!(decoded["causes"][0]["message"], "<redacted>");
        assert_eq!(decoded["causes"][0]["causes"][0]["message"], "<redacted>");
    }
}

#[test]
fn explicit_redaction_revokes_admission_before_clone_composition() {
    let fixture = include_str!("fixtures/secret-surface.orna").trim();
    let admitted = Diagnostic::new(
        SafeText::new("ORNA-E-REVOKED-ROOT").unwrap(),
        DiagnosticSeverity::Error,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .with_note(SafeText::new(fixture).unwrap())
    .redacted_with_message(SafeText::new(fixture).unwrap());

    // Revoking one clone must also prevent its still-admitted sibling from
    // disclosing when that sibling is later composed as a cause.
    let revoked = admitted
        .clone()
        .redacted()
        .with_note(SafeText::new(fixture).unwrap())
        .with_cause(admitted);
    let envelope = serde_json::json!({"diagnostics": [revoked.clone()]});
    let json = serde_json::to_vec(&envelope).unwrap();
    let projection = &envelope["diagnostics"][0];

    assert_eq!(projection["message"], "<redacted>");
    assert_eq!(projection["notes"][0], "<redacted>");
    assert_eq!(projection["redacted"], true);
    assert_eq!(projection["causes"][0]["message"], "<redacted>");
    assert_eq!(projection["causes"][0]["notes"][0], "<redacted>");
    assert!(
        !json
            .windows(fixture.len())
            .any(|window| window == fixture.as_bytes())
    );

    let encoded = revoked.encode_ovb().unwrap();
    assert!(
        !encoded
            .windows(fixture.len())
            .any(|window| window == fixture.as_bytes())
    );
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&encoded).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    assert_eq!(decoded["causes"][0]["message"], "<redacted>");
}

#[test]
fn redacting_one_clone_preserves_sibling_local_admission() {
    let fixture = include_str!("fixtures/secret-surface.orna").trim();
    let fixture_credential = fixture.split('"').nth(1).unwrap();
    let admitted = Diagnostic::new(
        SafeText::new("ORNA-E-CLONED-ADMISSION").unwrap(),
        DiagnosticSeverity::Error,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .with_note(SafeText::new(fixture).unwrap())
    .redacted_with_message(SafeText::new(fixture).unwrap());
    let revoked = admitted.clone().redacted();

    let envelope = serde_json::json!({"diagnostics": [admitted.clone(), revoked.clone()]});
    let json = serde_json::to_vec(&envelope).unwrap();
    let admitted_projection = &envelope["diagnostics"][0];
    let revoked_projection = &envelope["diagnostics"][1];
    assert_eq!(admitted_projection["message"], fixture);
    assert_eq!(admitted_projection["notes"][0], "<redacted>");
    assert_eq!(revoked_projection["message"], "<redacted>");
    assert_eq!(revoked_projection["notes"][0], "<redacted>");
    assert_eq!(
        json.windows(fixture_credential.len())
            .filter(|window| *window == fixture_credential.as_bytes())
            .count(),
        1
    );

    let admitted_wire = admitted.encode_ovb().unwrap();
    let revoked_wire = revoked.encode_ovb().unwrap();
    assert!(
        admitted_wire
            .windows(fixture.len())
            .any(|window| window == fixture.as_bytes())
    );
    assert!(
        !revoked_wire
            .windows(fixture.len())
            .any(|window| window == fixture.as_bytes())
    );
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&admitted_wire).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
}

#[test]
fn diagnostic_decode_redacts_untrusted_and_composed_payloads() {
    let fixture = include_str!("fixtures/secret-surface.orna").trim();
    let raw_cause = raw_diagnostic("ORNA-E-CAUSE", fixture, vec![], false);
    let raw_root = raw_diagnostic("ORNA-E-ROOT", fixture, vec![raw_cause], false);
    let decoded = Diagnostic::decode_ovb(&Value::new(raw_root).unwrap().encode().unwrap()).unwrap();

    assert_eq!(decoded.message(), "<redacted>");
    let json = serde_json::to_vec(&decoded).unwrap();
    assert!(
        !json
            .windows(fixture.len())
            .any(|window| window == fixture.as_bytes())
    );
    let projection = serde_json::to_value(decoded).unwrap();
    assert_eq!(projection["redacted"], true);
    assert_eq!(projection["message"], "<redacted>");
    assert_eq!(projection["notes"][0], "<redacted>");
    assert_eq!(projection["causes"][0]["redacted"], true);
    assert_eq!(projection["causes"][0]["message"], "<redacted>");
    assert_eq!(projection["causes"][0]["notes"][0], "<redacted>");

    // Neither a forged root admission bit nor a false nested cause bit grants
    // a generic decoder permission to expose fixture payloads.
    let mixed = raw_diagnostic(
        "ORNA-E-ROOT",
        fixture,
        vec![raw_diagnostic("ORNA-E-CAUSE", fixture, vec![], false)],
        true,
    );
    let decoded = Diagnostic::decode_ovb(&Value::new(mixed).unwrap().encode().unwrap()).unwrap();
    assert_eq!(decoded.message(), "<redacted>");
    let json = serde_json::to_vec(&decoded).unwrap();
    assert!(
        !json
            .windows(fixture.len())
            .any(|window| window == fixture.as_bytes())
    );
    let projection = serde_json::to_value(decoded).unwrap();
    assert_eq!(projection["message"], "<redacted>");
    assert_eq!(projection["redacted"], true);
    assert_eq!(projection["causes"][0]["message"], "<redacted>");
}

fn raw_diagnostic(code: &str, message: &str, causes: Vec<OvbRaw>, redacted: bool) -> OvbRaw {
    let fields = vec![
        (0, OvbRaw::Text(code.to_owned())),
        (1, OvbRaw::Int(3.into())),
        (2, OvbRaw::Text(message.to_owned())),
        (3, OvbRaw::Array(Vec::new())),
        (4, OvbRaw::Array(vec![OvbRaw::Text(message.to_owned())])),
        (5, OvbRaw::Array(causes)),
        (6, OvbRaw::Bool(redacted)),
    ]
    .into_iter()
    .map(|(key, value)| (OvbRaw::Int(key.into()), value))
    .collect();
    OvbRaw::Tag(60011, Box::new(OvbRaw::Map(fields)))
}

fn assert_redacted_boundaries(redacted: Diagnostic, expected_message: &str) {
    let ovb = redacted.encode_ovb().unwrap();
    let decoded = Diagnostic::decode_ovb(&ovb).unwrap();
    let json = serde_json::to_vec(&redacted).unwrap();
    let json_value = serde_json::to_value(&redacted).unwrap();
    assert_eq!(decoded, redacted.clone().redacted());

    for secret in [
        "root-secret-value",
        "root-secret-note",
        "nested-secret-value",
        "nested-secret-note",
    ] {
        assert!(
            !ovb.windows(secret.len())
                .any(|window| window == secret.as_bytes())
        );
        assert!(
            !json
                .windows(secret.len())
                .any(|window| window == secret.as_bytes())
        );
    }

    assert_eq!(json_value["code"], "ORNA-E-SECRET");
    assert_eq!(json_value["severity"], "error");
    assert_eq!(json_value["message"], expected_message);
    assert_eq!(json_value["notes"][0], "<redacted>");
    assert_eq!(json_value["redacted"], true);
    assert_eq!(
        json_value["reference"],
        "abababab-abab-abab-abab-abababababab"
    );
    assert_eq!(json_value["spans"][0]["file-path"], "src/main.orna");
    assert_eq!(json_value["causes"][0]["code"], "ORNA-E-NESTED");
    assert_eq!(json_value["causes"][0]["severity"], "warning");
    assert_eq!(json_value["causes"][0]["message"], "<redacted>");
    assert_eq!(json_value["causes"][0]["notes"][0], "<redacted>");
    assert_eq!(json_value["causes"][0]["redacted"], true);
    assert_eq!(
        json_value["causes"][0]["reference"],
        "cdcdcdcd-cdcd-cdcd-cdcd-cdcdcdcdcdcd"
    );
}

#[test]
fn unredacted_diagnostics_keep_local_text_but_serialize_only_safe_projections() {
    let diagnostic = diagnostic_with_secret_text();
    let json = serde_json::to_value(&diagnostic).unwrap();

    assert_eq!(diagnostic.message(), "connector secret: root-secret-value");
    assert_eq!(json["message"], "<redacted>");
    assert_eq!(json["notes"][0], "<redacted>");
    assert_eq!(json["causes"][0]["message"], "<redacted>");
    assert_eq!(json["redacted"], true);
    assert_eq!(json["causes"][0]["redacted"], true);

    let encoded = diagnostic.encode_ovb().unwrap();
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&encoded).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    assert_eq!(decoded["notes"][0], "<redacted>");
    assert_eq!(decoded["causes"][0]["message"], "<redacted>");
}

#[test]
fn diagnostic_spans_accept_utf8_repository_relative_paths_and_round_trip() {
    let span = DiagnosticSpan::new(
        Snapshot::Commit {
            database: [7; 16],
            algorithm: GitHash::Sha256,
            oid: vec![9; 32],
        },
        "src/naïve/λ.orna",
        4.into(),
        9.into(),
    )
    .unwrap();

    assert_eq!(span.file_path, "src/naïve/λ.orna");
    let json = serde_json::to_value(
        Diagnostic::new(
            SafeText::new("ORNA-E-UTF8").unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new("safe message").unwrap(),
        )
        .unwrap()
        .with_span(span),
    )
    .unwrap();
    assert_eq!(json["spans"][0]["file-path"], "src/naïve/λ.orna");
}

#[test]
fn diagnostic_spans_reject_unsafe_repository_paths() {
    let snapshot = Snapshot::Commit {
        database: [7; 16],
        algorithm: GitHash::Sha256,
        oid: vec![9; 32],
    };
    for path in [
        "",
        "/src/main.orna",
        "src//main.orna",
        "./src/main.orna",
        "src/../main.orna",
        "src\\main.orna",
        "..\\main.orna",
        "C:/src/main.orna",
        "c:src/main.orna",
        "\\\\server\\share\\main.orna",
        "src/\u{0}main.orna",
        "src/\nmain.orna",
    ] {
        assert!(
            DiagnosticSpan::new(snapshot.clone(), path, 0.into(), 0.into()).is_err(),
            "unsafe diagnostic path was admitted: {path:?}"
        );
    }
    assert!(DiagnosticSpan::new(snapshot, "<redacted>", 0.into(), 0.into()).is_ok());
}

#[test]
fn redacted_span_marker_promotes_boundary_projection_to_redacted() {
    let snapshot = Snapshot::Commit {
        database: [7; 16],
        algorithm: GitHash::Sha256,
        oid: vec![9; 32],
    };
    let span = DiagnosticSpan::new(snapshot, "<redacted>", 0.into(), 0.into()).unwrap();
    let diagnostic = Diagnostic::new(
        SafeText::new("ORNA-E-REDACTED-SPAN").unwrap(),
        DiagnosticSeverity::Error,
        SafeText::new("safe message").unwrap(),
    )
    .unwrap()
    .with_span(span);

    let projected = serde_json::to_value(&diagnostic).unwrap();
    assert_eq!(projected["redacted"], true);
    assert_eq!(projected["spans"][0]["file-path"], "<redacted>");

    let encoded = diagnostic.encode_ovb().unwrap();
    let decoded = Diagnostic::decode_ovb(&encoded).unwrap();
    assert!(serde_json::to_value(decoded).is_ok());

    let encoded = diagnostic.redacted().encode_ovb().unwrap();
    assert!(Diagnostic::decode_ovb(&encoded).is_ok());

    let mut contradictory = encoded;
    *contradictory.last_mut().unwrap() = 0xf4; // `redacted: false`
    assert!(Diagnostic::decode_ovb(&contradictory).is_err());
}

#[test]
fn diagnostic_decode_rejects_invalid_utf8_in_a_span_path() {
    let diagnostic = Diagnostic::new(
        SafeText::new("ORNA-E-UTF8").unwrap(),
        DiagnosticSeverity::Error,
        SafeText::new("safe message").unwrap(),
    )
    .unwrap()
    .with_span(
        DiagnosticSpan::new(
            Snapshot::Commit {
                database: [7; 16],
                algorithm: GitHash::Sha256,
                oid: vec![9; 32],
            },
            "src/main.orna",
            0.into(),
            1.into(),
        )
        .unwrap(),
    );
    let mut encoded = diagnostic.encode_ovb().unwrap();
    let offset = encoded
        .windows(b"src/main.orna".len())
        .position(|window| window == b"src/main.orna")
        .unwrap();
    encoded[offset] = 0xff;

    assert!(Diagnostic::decode_ovb(&encoded).is_err());
}
