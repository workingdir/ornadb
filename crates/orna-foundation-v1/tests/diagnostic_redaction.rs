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
fn clone_from_replaces_diagnostic_trust_with_source_state() {
    let fixture = include_str!("fixtures/secret-surface.orna").trim();
    let fixture_credential = fixture.split('"').nth(1).unwrap();
    let admitted = Diagnostic::new(
        SafeText::new("ORNA-E-CLONE-FROM").unwrap(),
        DiagnosticSeverity::Error,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .with_note(SafeText::new(fixture).unwrap())
    .redacted_with_message(SafeText::new(fixture).unwrap());
    let revoked = admitted.clone().redacted();

    let mut admitted_destination = admitted.clone();
    admitted_destination.clone_from(&revoked);
    let mut revoked_destination = revoked.clone();
    revoked_destination.clone_from(&admitted);

    let envelope = serde_json::json!({
        "diagnostics": [admitted_destination.clone(), revoked_destination.clone()]
    });
    let json = serde_json::to_vec(&envelope).unwrap();
    assert_eq!(
        envelope["diagnostics"][0]["message"],
        "<redacted>"
    );
    assert_eq!(
        envelope["diagnostics"][1]["message"],
        fixture
    );
    assert_eq!(
        json.windows(fixture_credential.len())
            .filter(|window| *window == fixture_credential.as_bytes())
            .count(),
        1
    );

    let revoked_wire = admitted_destination.encode_ovb().unwrap();
    let admitted_wire = revoked_destination.encode_ovb().unwrap();
    assert!(
        !revoked_wire
            .windows(fixture.len())
            .any(|window| window == fixture.as_bytes())
    );
    assert!(
        admitted_wire
            .windows(fixture.len())
            .any(|window| window == fixture.as_bytes())
    );
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&admitted_wire).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
}

#[test]
fn clone_from_tree_keeps_admission_at_root_only_at_projection() {
    let fixture = include_str!("fixtures/secret-surface.orna").trim();
    let fixture_credential = fixture.split('"').nth(1).unwrap();
    let admitted_grandchild = Diagnostic::new(
        SafeText::new("ORNA-E-CLONE-GRANDCHILD").unwrap(),
        DiagnosticSeverity::Warning,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .redacted_with_message(SafeText::new(fixture).unwrap())
    .with_note(SafeText::new(fixture).unwrap());
    let admitted_cause = Diagnostic::new(
        SafeText::new("ORNA-E-CLONE-CAUSE").unwrap(),
        DiagnosticSeverity::Warning,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .redacted_with_message(SafeText::new(fixture).unwrap())
    .with_note(SafeText::new(fixture).unwrap())
    .with_cause(admitted_grandchild);
    let source = Diagnostic::new(
        SafeText::new("ORNA-E-CLONE-ROOT").unwrap(),
        DiagnosticSeverity::Error,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .redacted_with_message(SafeText::new("admitted root message").unwrap())
    .with_note(SafeText::new(fixture).unwrap())
    .with_cause(admitted_cause);

    let destination_cause = Diagnostic::new(
        SafeText::new("ORNA-E-OLD-CAUSE").unwrap(),
        DiagnosticSeverity::Warning,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .with_cause(
        Diagnostic::new(
            SafeText::new("ORNA-E-OLD-GRANDCHILD").unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap(),
    );
    let mut destination = Diagnostic::new(
        SafeText::new("ORNA-E-OLD-ROOT").unwrap(),
        DiagnosticSeverity::Error,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .with_cause(destination_cause);

    destination.clone_from(&source);
    let json = serde_json::to_vec(&destination).unwrap();
    let projection = serde_json::to_value(&destination).unwrap();
    assert_eq!(projection["message"], "admitted root message");
    assert_eq!(projection["notes"][0], "<redacted>");
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
    assert!(
        !json
            .windows(fixture_credential.len())
            .any(|window| window == fixture_credential.as_bytes())
    );

    let encoded = destination.encode_ovb().unwrap();
    assert!(
        !encoded
            .windows(fixture.len())
            .any(|window| window == fixture.as_bytes())
    );
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&encoded).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    assert_eq!(decoded["causes"][0]["message"], "<redacted>");
    assert_eq!(decoded["causes"][0]["causes"][0]["message"], "<redacted>");
}

#[test]
fn clone_from_cause_vector_size_changes_redact_or_drop_secret_tails() {
    let fixture = include_str!("fixtures/secret-surface.orna").trim();
    let fixture_credential = fixture.split('"').nth(1).unwrap();
    let admitted_cause = |code: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .redacted_with_message(SafeText::new(fixture).unwrap())
        .with_note(SafeText::new(fixture).unwrap())
    };
    let untrusted_cause = |code: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };

    let growing_source = Diagnostic::new(
        SafeText::new("ORNA-E-GROW-SOURCE").unwrap(),
        DiagnosticSeverity::Error,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .redacted_with_message(SafeText::new("admitted growth root").unwrap())
    .with_cause(admitted_cause("ORNA-E-GROW-CAUSE-A"))
    .with_cause(admitted_cause("ORNA-E-GROW-CAUSE-B"));
    let mut growing_destination = Diagnostic::new(
        SafeText::new("ORNA-E-OLD-ROOT").unwrap(),
        DiagnosticSeverity::Error,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .with_cause(untrusted_cause("ORNA-E-OLD-CAUSE"));

    // clone_from reuses the old child slot, then appends a second admitted
    // child. Neither child's local admission may cross the cause boundary.
    growing_destination.clone_from(&growing_source);
    let growing_json = serde_json::to_vec(&growing_destination).unwrap();
    let growing_projection = serde_json::to_value(&growing_destination).unwrap();
    assert_eq!(growing_projection["message"], "admitted growth root");
    assert_eq!(growing_projection["causes"].as_array().unwrap().len(), 2);
    for cause in growing_projection["causes"].as_array().unwrap() {
        assert_eq!(cause["message"], "<redacted>");
        assert_eq!(cause["notes"][0], "<redacted>");
        assert_eq!(cause["redacted"], true);
    }
    assert!(
        !growing_json
            .windows(fixture_credential.len())
            .any(|window| window == fixture_credential.as_bytes())
    );
    let growing_wire = growing_destination.encode_ovb().unwrap();
    assert!(
        !growing_wire
            .windows(fixture.len())
            .any(|window| window == fixture.as_bytes())
    );
    let growing_decoded =
        serde_json::to_value(Diagnostic::decode_ovb(&growing_wire).unwrap()).unwrap();
    assert_eq!(growing_decoded["message"], "<redacted>");
    assert_eq!(growing_decoded["causes"].as_array().unwrap().len(), 2);

    let shrinking_source = Diagnostic::new(
        SafeText::new("ORNA-E-SHRINK-SOURCE").unwrap(),
        DiagnosticSeverity::Error,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .redacted_with_message(SafeText::new("admitted shrinking root").unwrap());
    let mut shrinking_destination = shrinking_source
        .clone()
        .with_cause(admitted_cause("ORNA-E-STALE-CAUSE-A"))
        .with_cause(admitted_cause("ORNA-E-STALE-CAUSE-B"))
        .with_cause(admitted_cause("ORNA-E-STALE-CAUSE-C"));

    // Replacing a longer cause vector must drop every stale admitted child.
    shrinking_destination.clone_from(&shrinking_source);
    let shrinking_json = serde_json::to_vec(&shrinking_destination).unwrap();
    let shrinking_projection = serde_json::to_value(&shrinking_destination).unwrap();
    assert_eq!(shrinking_projection["message"], "admitted shrinking root");
    assert_eq!(shrinking_projection["causes"].as_array().unwrap().len(), 0);
    assert!(
        !shrinking_json
            .windows(fixture_credential.len())
            .any(|window| window == fixture_credential.as_bytes())
    );
    let shrinking_wire = shrinking_destination.encode_ovb().unwrap();
    assert!(
        !shrinking_wire
            .windows(fixture.len())
            .any(|window| window == fixture.as_bytes())
    );
    let shrinking_decoded =
        serde_json::to_value(Diagnostic::decode_ovb(&shrinking_wire).unwrap()).unwrap();
    assert_eq!(shrinking_decoded["message"], "<redacted>");
    assert_eq!(shrinking_decoded["causes"].as_array().unwrap().len(), 0);
}

#[test]
fn clone_from_nested_cause_vectors_resize_without_secret_tails() {
    let fixture = include_str!("fixtures/secret-surface.orna").trim();
    let fixture_credential = fixture.split('"').nth(1).unwrap();
    let admitted_leaf = |code: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .redacted_with_message(SafeText::new(fixture).unwrap())
        .with_note(SafeText::new(fixture).unwrap())
    };
    let untrusted_leaf = |code: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };

    let admitted_branch = Diagnostic::new(
        SafeText::new("ORNA-E-NESTED-GROW-SOURCE").unwrap(),
        DiagnosticSeverity::Warning,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .redacted_with_message(SafeText::new(fixture).unwrap())
    .with_note(SafeText::new(fixture).unwrap())
    .with_cause(admitted_leaf("ORNA-E-NESTED-GROW-A"))
    .with_cause(admitted_leaf("ORNA-E-NESTED-GROW-B"));
    let growing_source = Diagnostic::new(
        SafeText::new("ORNA-E-NESTED-GROW-ROOT").unwrap(),
        DiagnosticSeverity::Error,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .redacted_with_message(SafeText::new("admitted nested growth root").unwrap())
    .with_cause(admitted_branch);
    let old_branch = Diagnostic::new(
        SafeText::new("ORNA-E-NESTED-OLD-BRANCH").unwrap(),
        DiagnosticSeverity::Warning,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .with_cause(untrusted_leaf("ORNA-E-NESTED-OLD-LEAF"));
    let mut growing_destination = Diagnostic::new(
        SafeText::new("ORNA-E-NESTED-OLD-ROOT").unwrap(),
        DiagnosticSeverity::Error,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .with_cause(old_branch);

    // The root reuses its cause slot while that cause's own vector grows.
    growing_destination.clone_from(&growing_source);
    let growing_json = serde_json::to_vec(&growing_destination).unwrap();
    let growing_projection = serde_json::to_value(&growing_destination).unwrap();
    assert_eq!(growing_projection["message"], "admitted nested growth root");
    assert_eq!(growing_projection["causes"].as_array().unwrap().len(), 1);
    assert_eq!(growing_projection["causes"][0]["message"], "<redacted>");
    assert_eq!(growing_projection["causes"][0]["notes"][0], "<redacted>");
    assert_eq!(
        growing_projection["causes"][0]["causes"].as_array().unwrap().len(),
        2
    );
    for leaf in growing_projection["causes"][0]["causes"].as_array().unwrap() {
        assert_eq!(leaf["message"], "<redacted>");
        assert_eq!(leaf["notes"][0], "<redacted>");
    }
    assert!(
        !growing_json
            .windows(fixture_credential.len())
            .any(|window| window == fixture_credential.as_bytes())
    );
    let growing_wire = growing_destination.encode_ovb().unwrap();
    assert!(
        !growing_wire
            .windows(fixture.len())
            .any(|window| window == fixture.as_bytes())
    );

    let empty_admitted_branch = Diagnostic::new(
        SafeText::new("ORNA-E-NESTED-SHRINK-SOURCE").unwrap(),
        DiagnosticSeverity::Warning,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .redacted_with_message(SafeText::new(fixture).unwrap())
    .with_note(SafeText::new(fixture).unwrap());
    let shrinking_source = Diagnostic::new(
        SafeText::new("ORNA-E-NESTED-SHRINK-ROOT").unwrap(),
        DiagnosticSeverity::Error,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .redacted_with_message(SafeText::new("admitted nested shrink root").unwrap())
    .with_cause(empty_admitted_branch);
    let stale_branch = Diagnostic::new(
        SafeText::new("ORNA-E-NESTED-STALE-BRANCH").unwrap(),
        DiagnosticSeverity::Warning,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .redacted_with_message(SafeText::new(fixture).unwrap())
    .with_note(SafeText::new(fixture).unwrap())
    .with_cause(admitted_leaf("ORNA-E-NESTED-STALE-A"))
    .with_cause(admitted_leaf("ORNA-E-NESTED-STALE-B"));
    let mut shrinking_destination = Diagnostic::new(
        SafeText::new("ORNA-E-NESTED-STALE-ROOT").unwrap(),
        DiagnosticSeverity::Error,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .with_cause(stale_branch);

    // Replacing that cause reuses its slot and removes its stale nested tail.
    shrinking_destination.clone_from(&shrinking_source);
    let shrinking_json = serde_json::to_vec(&shrinking_destination).unwrap();
    let shrinking_projection = serde_json::to_value(&shrinking_destination).unwrap();
    assert_eq!(shrinking_projection["message"], "admitted nested shrink root");
    assert_eq!(shrinking_projection["causes"].as_array().unwrap().len(), 1);
    assert_eq!(shrinking_projection["causes"][0]["message"], "<redacted>");
    assert_eq!(
        shrinking_projection["causes"][0]["causes"].as_array().unwrap().len(),
        0
    );
    assert!(
        !shrinking_json
            .windows(fixture_credential.len())
            .any(|window| window == fixture_credential.as_bytes())
    );
    let shrinking_wire = shrinking_destination.encode_ovb().unwrap();
    assert!(
        !shrinking_wire
            .windows(fixture.len())
            .any(|window| window == fixture.as_bytes())
    );
    let shrinking_decoded =
        serde_json::to_value(Diagnostic::decode_ovb(&shrinking_wire).unwrap()).unwrap();
    assert_eq!(shrinking_decoded["message"], "<redacted>");
    assert_eq!(shrinking_decoded["causes"][0]["message"], "<redacted>");
    assert_eq!(
        shrinking_decoded["causes"][0]["causes"].as_array().unwrap().len(),
        0
    );
}

#[test]
fn clone_from_nested_same_length_mixed_trust_slots_redacts_all_causes() {
    let fixture = include_str!("fixtures/secret-surface.orna").trim();
    let fixture_credential = fixture.split('"').nth(1).unwrap();
    let diagnostic = |code: &str, admitted: bool| {
        let diagnostic = Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap();
        let diagnostic = if admitted {
            diagnostic.redacted_with_message(SafeText::new(fixture).unwrap())
        } else {
            diagnostic
        };
        diagnostic.with_note(SafeText::new(fixture).unwrap())
    };

    let source_branch = diagnostic("ORNA-E-MIXED-SOURCE-BRANCH", true)
        .with_cause(diagnostic("ORNA-E-MIXED-SOURCE-A", true))
        .with_cause(diagnostic("ORNA-E-MIXED-SOURCE-B", false))
        .with_cause(diagnostic("ORNA-E-MIXED-SOURCE-C", true));
    let source = Diagnostic::new(
        SafeText::new("ORNA-E-MIXED-SOURCE-ROOT").unwrap(),
        DiagnosticSeverity::Error,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .redacted_with_message(SafeText::new("admitted mixed-trust root").unwrap())
    .with_cause(source_branch);

    let destination_branch = diagnostic("ORNA-E-MIXED-OLD-BRANCH", false)
        .with_cause(diagnostic("ORNA-E-MIXED-OLD-A", false))
        .with_cause(diagnostic("ORNA-E-MIXED-OLD-B", true))
        .with_cause(diagnostic("ORNA-E-MIXED-OLD-C", false));
    let mut destination = Diagnostic::new(
        SafeText::new("ORNA-E-MIXED-OLD-ROOT").unwrap(),
        DiagnosticSeverity::Error,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .with_cause(destination_branch);

    destination.clone_from(&source);
    // Equality includes the private admission marks: each reused slot now
    // carries precisely the source node's state before boundary projection.
    assert_eq!(destination, source);

    let json = serde_json::to_vec(&destination).unwrap();
    let projection = serde_json::to_value(&destination).unwrap();
    assert_eq!(projection["message"], "admitted mixed-trust root");
    assert_eq!(projection["causes"][0]["message"], "<redacted>");
    assert_eq!(projection["causes"][0]["notes"][0], "<redacted>");
    let nested = projection["causes"][0]["causes"].as_array().unwrap();
    assert_eq!(nested.len(), 3);
    for cause in nested {
        assert_eq!(cause["message"], "<redacted>");
        assert_eq!(cause["notes"][0], "<redacted>");
        assert_eq!(cause["redacted"], true);
    }
    assert!(
        !json
            .windows(fixture_credential.len())
            .any(|window| window == fixture_credential.as_bytes())
    );

    let encoded = destination.encode_ovb().unwrap();
    assert!(
        !encoded
            .windows(fixture.len())
            .any(|window| window == fixture.as_bytes())
    );
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&encoded).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    assert_eq!(decoded["causes"][0]["message"], "<redacted>");
    for cause in decoded["causes"][0]["causes"].as_array().unwrap() {
        assert_eq!(cause["message"], "<redacted>");
    }
}

#[test]
fn root_readmission_after_clone_from_does_not_regrant_nested_slots() {
    let fixture = include_str!("fixtures/secret-surface.orna").trim();
    let fixture_credential = fixture.split('"').nth(1).unwrap();
    let diagnostic = |code: &str, admitted: bool| {
        let diagnostic = Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap();
        let diagnostic = if admitted {
            diagnostic.redacted_with_message(SafeText::new(fixture).unwrap())
        } else {
            diagnostic
        };
        diagnostic.with_note(SafeText::new(fixture).unwrap())
    };

    let source = Diagnostic::new(
        SafeText::new("ORNA-E-READMISSION-SOURCE").unwrap(),
        DiagnosticSeverity::Error,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .with_note(SafeText::new(fixture).unwrap())
    .with_cause(
        diagnostic("ORNA-E-READMISSION-BRANCH", true)
            .with_cause(diagnostic("ORNA-E-READMISSION-ADMITTED", true))
            .with_cause(diagnostic("ORNA-E-READMISSION-UNTRUSTED", false)),
    );
    let mut destination = Diagnostic::new(
        SafeText::new("ORNA-E-READMISSION-OLD-ROOT").unwrap(),
        DiagnosticSeverity::Error,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .redacted_with_message(SafeText::new("old root admission").unwrap())
    .with_cause(diagnostic("ORNA-E-READMISSION-OLD-BRANCH", true));

    destination.clone_from(&source);
    assert_eq!(destination, source);
    let readmitted = destination
        .redacted_with_message(SafeText::new("new root admission").unwrap());

    let json = serde_json::to_vec(&readmitted).unwrap();
    let projection = serde_json::to_value(&readmitted).unwrap();
    assert_eq!(projection["message"], "new root admission");
    assert_eq!(projection["notes"][0], "<redacted>");
    assert_eq!(projection["causes"][0]["message"], "<redacted>");
    assert_eq!(projection["causes"][0]["notes"][0], "<redacted>");
    assert_eq!(projection["causes"][0]["causes"].as_array().unwrap().len(), 2);
    for cause in projection["causes"][0]["causes"].as_array().unwrap() {
        assert_eq!(cause["message"], "<redacted>");
        assert_eq!(cause["notes"][0], "<redacted>");
    }
    assert!(
        !json
            .windows(fixture_credential.len())
            .any(|window| window == fixture_credential.as_bytes())
    );

    let encoded = readmitted.encode_ovb().unwrap();
    assert!(
        !encoded
            .windows(fixture.len())
            .any(|window| window == fixture.as_bytes())
    );
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&encoded).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    assert_eq!(decoded["causes"][0]["message"], "<redacted>");
    for cause in decoded["causes"][0]["causes"].as_array().unwrap() {
        assert_eq!(cause["message"], "<redacted>");
    }
}

#[test]
fn post_clone_admission_stays_local_through_nested_cause_composition() {
    let fixture = include_str!("fixtures/secret-surface.orna").trim();
    let fixture_credential = fixture.split('"').nth(1).unwrap();
    let diagnostic = |code: &str, admitted: bool| {
        let diagnostic = Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap());
        if admitted {
            diagnostic.redacted_with_message(SafeText::new(fixture).unwrap())
        } else {
            diagnostic
        }
    };

    let source = Diagnostic::new(
        SafeText::new("ORNA-E-POST-CLONE-SOURCE").unwrap(),
        DiagnosticSeverity::Error,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .with_cause(
        diagnostic("ORNA-E-POST-CLONE-BRANCH", true)
            .with_cause(diagnostic("ORNA-E-POST-CLONE-LEAF", true)),
    );
    let mut destination = Diagnostic::new(
        SafeText::new("ORNA-E-POST-CLONE-OLD-ROOT").unwrap(),
        DiagnosticSeverity::Error,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .redacted_with_message(SafeText::new("old local admission").unwrap())
    .with_cause(diagnostic("ORNA-E-POST-CLONE-OLD-BRANCH", true));
    destination.clone_from(&source);
    assert_eq!(destination, source);

    // The post-clone root gets a fresh local admission and then receives new
    // independently admitted descendants. Those marks remain local too.
    let post_clone = destination
        .redacted_with_message(SafeText::new("post-clone local admission").unwrap())
        .with_cause(
            diagnostic("ORNA-E-POST-CLONE-NEW-BRANCH", true)
                .with_cause(diagnostic("ORNA-E-POST-CLONE-NEW-LEAF", true)),
        )
        .with_cause(diagnostic("ORNA-E-POST-CLONE-UNTRUSTED", false));

    let standalone_json = serde_json::to_vec(&post_clone).unwrap();
    let standalone = serde_json::to_value(&post_clone).unwrap();
    assert_eq!(standalone["message"], "post-clone local admission");
    assert_eq!(standalone["causes"][0]["message"], "<redacted>");
    assert_eq!(
        standalone["causes"][0]["causes"][0]["message"],
        "<redacted>"
    );
    assert_eq!(standalone["causes"][1]["message"], "<redacted>");
    assert!(
        !standalone_json
            .windows(fixture_credential.len())
            .any(|window| window == fixture_credential.as_bytes())
    );
    let standalone_wire = post_clone.encode_ovb().unwrap();
    assert!(
        !standalone_wire
            .windows(fixture.len())
            .any(|window| window == fixture.as_bytes())
    );
    let standalone_decoded =
        serde_json::to_value(Diagnostic::decode_ovb(&standalone_wire).unwrap()).unwrap();
    assert_eq!(standalone_decoded["message"], "<redacted>");
    assert_eq!(standalone_decoded["causes"][0]["message"], "<redacted>");
    assert_eq!(
        standalone_decoded["causes"][0]["causes"][0]["message"],
        "<redacted>"
    );

    let composed = Diagnostic::new(
        SafeText::new("ORNA-E-POST-CLONE-OUTER").unwrap(),
        DiagnosticSeverity::Error,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .redacted_with_message(SafeText::new("outer local admission").unwrap())
    .with_cause(post_clone);
    let composed_json = serde_json::to_vec(&composed).unwrap();
    let projection = serde_json::to_value(&composed).unwrap();
    assert_eq!(projection["message"], "outer local admission");
    assert_eq!(projection["causes"][0]["message"], "<redacted>");
    assert_eq!(projection["causes"][0]["causes"][0]["message"], "<redacted>");
    assert_eq!(
        projection["causes"][0]["causes"][0]["causes"][0]["message"],
        "<redacted>"
    );
    assert!(
        !composed_json
            .windows(fixture_credential.len())
            .any(|window| window == fixture_credential.as_bytes())
    );

    let composed_wire = composed.encode_ovb().unwrap();
    assert!(
        !composed_wire
            .windows(fixture.len())
            .any(|window| window == fixture.as_bytes())
    );
    assert!(
        !composed_wire
            .windows(b"post-clone local admission".len())
            .any(|window| window == b"post-clone local admission")
    );
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&composed_wire).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    assert_eq!(decoded["causes"][0]["message"], "<redacted>");
    assert_eq!(decoded["causes"][0]["causes"][0]["message"], "<redacted>");
}

#[test]
fn clone_from_readmission_keeps_root_sibling_when_clone_is_a_cause() {
    let fixture = include_str!("fixtures/secret-surface.orna").trim();
    let fixture_credential = fixture.split('"').nth(1).unwrap();
    let source = Diagnostic::new(
        SafeText::new("ORNA-E-LOCALITY-SOURCE").unwrap(),
        DiagnosticSeverity::Error,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .with_note(SafeText::new(fixture).unwrap())
    .with_cause(
        Diagnostic::new(
            SafeText::new("ORNA-E-LOCALITY-SOURCE-CAUSE").unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .redacted_with_message(SafeText::new(fixture).unwrap())
        .with_cause(
            Diagnostic::new(
                SafeText::new("ORNA-E-LOCALITY-SOURCE-LEAF").unwrap(),
                DiagnosticSeverity::Warning,
                SafeText::new(fixture).unwrap(),
            )
            .unwrap()
            .redacted_with_message(SafeText::new(fixture).unwrap()),
        ),
    );
    let mut destination = Diagnostic::new(
        SafeText::new("ORNA-E-LOCALITY-OLD").unwrap(),
        DiagnosticSeverity::Error,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .redacted_with_message(SafeText::new("old destination admission").unwrap());
    destination.clone_from(&source);
    assert_eq!(destination, source);

    let post_clone = destination
        .redacted_with_message(SafeText::new("post-clone sibling disclosure").unwrap());
    let standalone_sibling = post_clone.clone();
    let cause_sibling = post_clone;
    let composed = Diagnostic::new(
        SafeText::new("ORNA-E-LOCALITY-OUTER").unwrap(),
        DiagnosticSeverity::Error,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .redacted_with_message(SafeText::new("outer local disclosure").unwrap())
    .with_cause(cause_sibling);
    let standalone_bytes = standalone_sibling.encode_ovb().unwrap();
    let composed_wire = composed.encode_ovb().unwrap();

    let envelope = serde_json::json!({
        "standalone": standalone_sibling,
        "composed": composed,
    });
    let json = serde_json::to_vec(&envelope).unwrap();
    assert_eq!(
        envelope["standalone"]["message"],
        "post-clone sibling disclosure"
    );
    assert_eq!(
        envelope["composed"]["message"],
        "outer local disclosure"
    );
    assert_eq!(
        envelope["composed"]["causes"][0]["message"],
        "<redacted>"
    );
    assert_eq!(
        json.windows(b"post-clone sibling disclosure".len())
            .filter(|window| *window == b"post-clone sibling disclosure")
            .count(),
        1
    );
    assert!(
        !json
            .windows(fixture_credential.len())
            .any(|window| window == fixture_credential.as_bytes())
    );

    assert!(
        standalone_bytes
            .windows(b"post-clone sibling disclosure".len())
            .any(|window| window == b"post-clone sibling disclosure")
    );
    assert!(
        !standalone_bytes
            .windows(fixture.len())
            .any(|window| window == fixture.as_bytes())
    );
    assert!(
        !composed_wire
            .windows(b"post-clone sibling disclosure".len())
            .any(|window| window == b"post-clone sibling disclosure")
    );
    assert!(
        !composed_wire
            .windows(fixture.len())
            .any(|window| window == fixture.as_bytes())
    );
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&composed_wire).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    assert_eq!(decoded["causes"][0]["message"], "<redacted>");
}

#[test]
fn clone_from_replacement_revokes_only_that_post_clone_sibling() {
    let fixture = include_str!("fixtures/secret-surface.orna").trim();
    let fixture_credential = fixture.split('"').nth(1).unwrap();
    let source = Diagnostic::new(
        SafeText::new("ORNA-E-REPLACEMENT-SOURCE").unwrap(),
        DiagnosticSeverity::Error,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .with_cause(
        Diagnostic::new(
            SafeText::new("ORNA-E-REPLACEMENT-SOURCE-CAUSE").unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .redacted_with_message(SafeText::new(fixture).unwrap()),
    );
    let mut destination = Diagnostic::new(
        SafeText::new("ORNA-E-REPLACEMENT-OLD-ROOT").unwrap(),
        DiagnosticSeverity::Error,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .redacted_with_message(SafeText::new("old root admission").unwrap());
    destination.clone_from(&source);
    assert_eq!(destination, source);

    let survivor = destination
        .redacted_with_message(SafeText::new("post-clone sibling admission").unwrap());
    let mut replaced = survivor.clone();
    let untrusted_replacement = Diagnostic::new(
        SafeText::new("ORNA-E-REPLACEMENT-UNTRUSTED").unwrap(),
        DiagnosticSeverity::Warning,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .with_note(SafeText::new(fixture).unwrap());
    replaced.clone_from(&untrusted_replacement);
    assert_eq!(replaced, untrusted_replacement);

    let outer = Diagnostic::new(
        SafeText::new("ORNA-E-REPLACEMENT-OUTER").unwrap(),
        DiagnosticSeverity::Error,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .redacted_with_message(SafeText::new("outer local admission").unwrap())
    .with_cause(replaced.clone())
    .with_cause(survivor.clone());

    // Project the composed siblings first; boundary projection must not
    // revoke the independent root admission retained by `survivor`.
    let outer_projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(outer_projection["message"], "outer local admission");
    assert_eq!(outer_projection["causes"][0]["message"], "<redacted>");
    assert_eq!(outer_projection["causes"][1]["message"], "<redacted>");
    let replaced_projection = serde_json::to_value(&replaced).unwrap();
    assert_eq!(replaced_projection["message"], "<redacted>");
    let survivor_projection = serde_json::to_value(&survivor).unwrap();
    assert_eq!(
        survivor_projection["message"],
        "post-clone sibling admission"
    );

    let envelope = serde_json::json!({
        "outer": outer,
        "replaced": replaced,
        "survivor": survivor,
    });
    let json = serde_json::to_vec(&envelope).unwrap();
    assert_eq!(
        json.windows(b"post-clone sibling admission".len())
            .filter(|window| *window == b"post-clone sibling admission")
            .count(),
        1
    );
    assert!(
        !json
            .windows(fixture_credential.len())
            .any(|window| window == fixture_credential.as_bytes())
    );

    let survivor_wire = survivor.encode_ovb().unwrap();
    assert!(
        survivor_wire
            .windows(b"post-clone sibling admission".len())
            .any(|window| window == b"post-clone sibling admission")
    );
    let replaced_wire = replaced.encode_ovb().unwrap();
    assert!(
        !replaced_wire
            .windows(b"post-clone sibling admission".len())
            .any(|window| window == b"post-clone sibling admission")
    );
    let outer_wire = outer.encode_ovb().unwrap();
    assert!(
        !outer_wire
            .windows(b"post-clone sibling admission".len())
            .any(|window| window == b"post-clone sibling admission")
    );
    assert!(
        !outer_wire
            .windows(fixture.len())
            .any(|window| window == fixture.as_bytes())
    );
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&outer_wire).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    assert_eq!(decoded["causes"][0]["message"], "<redacted>");
    assert_eq!(decoded["causes"][1]["message"], "<redacted>");
}

#[test]
fn clone_from_grant_and_revoke_stay_local_at_cause_boundaries() {
    let fixture = include_str!("fixtures/secret-surface.orna").trim();
    let fixture_credential = fixture.split('"').nth(1).unwrap();
    let admitted_source = Diagnostic::new(
        SafeText::new("ORNA-E-ADMISSION-REPLACE-SOURCE").unwrap(),
        DiagnosticSeverity::Error,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .redacted_with_message(SafeText::new("source local admission").unwrap())
    .with_cause(
        Diagnostic::new(
            SafeText::new("ORNA-E-ADMISSION-REPLACE-SOURCE-CAUSE").unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .redacted_with_message(SafeText::new("nested source admission").unwrap()),
    );
    let untrusted_source = Diagnostic::new(
        SafeText::new("ORNA-E-ADMISSION-REPLACE-UNTRUSTED").unwrap(),
        DiagnosticSeverity::Warning,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .with_note(SafeText::new(fixture).unwrap())
    .with_cause(
        Diagnostic::new(
            SafeText::new("ORNA-E-ADMISSION-REPLACE-UNTRUSTED-CAUSE").unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap(),
    );

    let mut granted_destination = untrusted_source.clone();
    granted_destination.clone_from(&admitted_source);
    let mut revoked_destination = admitted_source.clone();
    revoked_destination.clone_from(&untrusted_source);
    assert_eq!(granted_destination, admitted_source);
    assert_eq!(revoked_destination, untrusted_source);

    let outer = Diagnostic::new(
        SafeText::new("ORNA-E-ADMISSION-REPLACE-OUTER").unwrap(),
        DiagnosticSeverity::Error,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .redacted_with_message(SafeText::new("outer local admission").unwrap())
    .with_cause(granted_destination.clone())
    .with_cause(revoked_destination.clone());

    let outer_projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(outer_projection["message"], "outer local admission");
    assert_eq!(outer_projection["causes"][0]["message"], "<redacted>");
    assert_eq!(outer_projection["causes"][1]["message"], "<redacted>");
    assert_eq!(
        outer_projection["causes"][0]["causes"][0]["message"],
        "<redacted>"
    );
    let granted_projection = serde_json::to_value(&granted_destination).unwrap();
    assert_eq!(granted_projection["message"], "source local admission");
    assert_eq!(
        granted_projection["causes"][0]["message"],
        "<redacted>"
    );
    let revoked_projection = serde_json::to_value(&revoked_destination).unwrap();
    assert_eq!(revoked_projection["message"], "<redacted>");

    let envelope = serde_json::json!({
        "outer": outer,
        "granted": granted_destination,
        "revoked": revoked_destination,
    });
    let json = serde_json::to_vec(&envelope).unwrap();
    assert_eq!(
        json.windows(b"source local admission".len())
            .filter(|window| *window == b"source local admission")
            .count(),
        1
    );
    assert!(
        !json
            .windows(b"nested source admission".len())
            .any(|window| window == b"nested source admission")
    );
    assert!(
        !json
            .windows(fixture_credential.len())
            .any(|window| window == fixture_credential.as_bytes())
    );

    let granted_wire = granted_destination.encode_ovb().unwrap();
    assert!(
        granted_wire
            .windows(b"source local admission".len())
            .any(|window| window == b"source local admission")
    );
    assert!(
        !granted_wire
            .windows(b"nested source admission".len())
            .any(|window| window == b"nested source admission")
    );
    let revoked_wire = revoked_destination.encode_ovb().unwrap();
    assert!(
        !revoked_wire
            .windows(b"source local admission".len())
            .any(|window| window == b"source local admission")
    );
    let outer_wire = outer.encode_ovb().unwrap();
    assert!(
        !outer_wire
            .windows(b"source local admission".len())
            .any(|window| window == b"source local admission")
    );
    assert!(
        !outer_wire
            .windows(fixture.len())
            .any(|window| window == fixture.as_bytes())
    );
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&outer_wire).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    assert_eq!(decoded["causes"][0]["message"], "<redacted>");
    assert_eq!(decoded["causes"][1]["message"], "<redacted>");
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
