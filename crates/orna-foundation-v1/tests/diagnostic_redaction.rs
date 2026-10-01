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
fn sequential_clone_from_replacements_keep_admission_snapshots_local() {
    let fixture = include_str!("fixtures/secret-surface.orna").trim();
    let fixture_credential = fixture.split('"').nth(1).unwrap();
    let diagnostic = |code: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let first_source = diagnostic("ORNA-E-REPLACEMENT-FIRST")
        .redacted_with_message(SafeText::new("first replacement admission").unwrap())
        .with_cause(
            diagnostic("ORNA-E-REPLACEMENT-FIRST-CAUSE")
                .redacted_with_message(SafeText::new("first nested admission").unwrap()),
        );
    let second_source = diagnostic("ORNA-E-REPLACEMENT-SECOND")
        .redacted_with_message(SafeText::new("second replacement admission").unwrap())
        .with_cause(
            diagnostic("ORNA-E-REPLACEMENT-SECOND-CAUSE")
                .redacted_with_message(SafeText::new("second nested admission").unwrap()),
        );
    let untrusted_source = diagnostic("ORNA-E-REPLACEMENT-UNTRUSTED").with_cause(
        diagnostic("ORNA-E-REPLACEMENT-UNTRUSTED-CAUSE"),
    );

    let mut receiver = untrusted_source.clone();
    receiver.clone_from(&first_source);
    let first_snapshot = receiver.clone();
    receiver.clone_from(&second_source);
    let second_snapshot = receiver.clone();
    receiver.clone_from(&untrusted_source);
    assert_eq!(receiver, untrusted_source);

    // Each replacement updates the receiver alone; older snapshots keep their
    // own admissions, and nesting either snapshot drops that local authority.
    let outer = Diagnostic::new(
        SafeText::new("ORNA-E-REPLACEMENT-SEQUENCE-OUTER").unwrap(),
        DiagnosticSeverity::Error,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .redacted_with_message(SafeText::new("sequence outer admission").unwrap())
    .with_cause(first_snapshot.clone())
    .with_cause(second_snapshot.clone())
    .with_cause(receiver.clone());

    let outer_projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(outer_projection["message"], "sequence outer admission");
    let outer_causes = outer_projection["causes"].as_array().unwrap();
    assert_eq!(outer_causes.len(), 3);
    for cause in outer_causes {
        assert_eq!(cause["message"], "<redacted>");
        assert_eq!(cause["notes"][0], "<redacted>");
    }
    assert_eq!(
        outer_causes[0]["causes"][0]["message"],
        "<redacted>"
    );
    let first_projection = serde_json::to_value(&first_snapshot).unwrap();
    assert_eq!(first_projection["message"], "first replacement admission");
    assert_eq!(
        first_projection["causes"][0]["message"],
        "<redacted>"
    );
    let second_projection = serde_json::to_value(&second_snapshot).unwrap();
    assert_eq!(second_projection["message"], "second replacement admission");
    assert_eq!(
        second_projection["causes"][0]["message"],
        "<redacted>"
    );
    assert_eq!(serde_json::to_value(&receiver).unwrap()["message"], "<redacted>");

    let envelope = serde_json::json!({
        "outer": outer,
        "current": receiver,
        "first": first_snapshot,
        "second": second_snapshot,
    });
    let json = serde_json::to_vec(&envelope).unwrap();
    for disclosure in [
        b"first replacement admission".as_slice(),
        b"second replacement admission".as_slice(),
    ] {
        assert_eq!(
            json.windows(disclosure.len())
                .filter(|window| *window == disclosure)
                .count(),
            1
        );
    }
    for disclosure in [
        b"first nested admission".as_slice(),
        b"second nested admission".as_slice(),
    ] {
        assert!(
            !json
                .windows(disclosure.len())
                .any(|window| window == disclosure)
        );
    }
    assert!(
        !json
            .windows(fixture_credential.len())
            .any(|window| window == fixture_credential.as_bytes())
    );

    let first_wire = first_snapshot.encode_ovb().unwrap();
    assert!(
        first_wire
            .windows(b"first replacement admission".len())
            .any(|window| window == b"first replacement admission")
    );
    assert!(
        !first_wire
            .windows(b"first nested admission".len())
            .any(|window| window == b"first nested admission")
    );
    let second_wire = second_snapshot.encode_ovb().unwrap();
    assert!(
        second_wire
            .windows(b"second replacement admission".len())
            .any(|window| window == b"second replacement admission")
    );
    let current_wire = receiver.encode_ovb().unwrap();
    assert!(
        !current_wire
            .windows(b"first replacement admission".len())
            .any(|window| window == b"first replacement admission")
    );
    let outer_wire = outer.encode_ovb().unwrap();
    for disclosure in [
        b"first replacement admission".as_slice(),
        b"second replacement admission".as_slice(),
        b"first nested admission".as_slice(),
        b"second nested admission".as_slice(),
    ] {
        assert!(
            !outer_wire
                .windows(disclosure.len())
                .any(|window| window == disclosure)
        );
    }
    assert!(
        !outer_wire
            .windows(fixture.len())
            .any(|window| window == fixture.as_bytes())
    );
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&outer_wire).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    for cause in decoded["causes"].as_array().unwrap() {
        assert_eq!(cause["message"], "<redacted>");
    }
}

#[test]
fn forked_clone_from_replacements_do_not_mutate_snapshot_siblings() {
    let fixture = include_str!("fixtures/secret-surface.orna").trim();
    let fixture_credential = fixture.split('"').nth(1).unwrap();
    let diagnostic = |code: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let ancestor_source = diagnostic("ORNA-E-SNAPSHOT-ANCESTOR")
        .redacted_with_message(SafeText::new("ancestor snapshot admission").unwrap())
        .with_cause(
            diagnostic("ORNA-E-SNAPSHOT-ANCESTOR-CAUSE")
                .redacted_with_message(SafeText::new("ancestor nested admission").unwrap()),
        );
    let granted_source = diagnostic("ORNA-E-SNAPSHOT-GRANT")
        .redacted_with_message(SafeText::new("branch replacement admission").unwrap())
        .with_cause(
            diagnostic("ORNA-E-SNAPSHOT-GRANT-CAUSE")
                .redacted_with_message(SafeText::new("branch nested admission").unwrap()),
        );
    let untrusted_source = diagnostic("ORNA-E-SNAPSHOT-UNTRUSTED")
        .with_cause(diagnostic("ORNA-E-SNAPSHOT-UNTRUSTED-CAUSE"));

    let mut ancestor = untrusted_source.clone();
    ancestor.clone_from(&ancestor_source);
    assert_eq!(ancestor, ancestor_source);
    let ancestor_snapshot = ancestor.clone();

    // Branches begin from the same admitted snapshot. Replacing one branch
    // must not mutate the ancestor or the other branch's copied trust state.
    let mut granted_branch = ancestor.clone();
    granted_branch.clone_from(&granted_source);
    assert_eq!(granted_branch, granted_source);
    let mut revoked_branch = ancestor.clone();
    revoked_branch.clone_from(&untrusted_source);
    assert_eq!(revoked_branch, untrusted_source);

    let outer = Diagnostic::new(
        SafeText::new("ORNA-E-SNAPSHOT-OUTER").unwrap(),
        DiagnosticSeverity::Error,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .redacted_with_message(SafeText::new("snapshot outer admission").unwrap())
    .with_cause(ancestor_snapshot.clone())
    .with_cause(granted_branch.clone())
    .with_cause(revoked_branch.clone());
    let outer_projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(outer_projection["message"], "snapshot outer admission");
    let causes = outer_projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 3);
    for cause in causes {
        assert_eq!(cause["message"], "<redacted>");
        assert_eq!(cause["notes"][0], "<redacted>");
    }
    let ancestor_projection = serde_json::to_value(&ancestor_snapshot).unwrap();
    assert_eq!(
        ancestor_projection["message"],
        "ancestor snapshot admission"
    );
    assert_eq!(
        ancestor_projection["causes"][0]["message"],
        "<redacted>"
    );
    let granted_projection = serde_json::to_value(&granted_branch).unwrap();
    assert_eq!(
        granted_projection["message"],
        "branch replacement admission"
    );
    assert_eq!(
        granted_projection["causes"][0]["message"],
        "<redacted>"
    );
    assert_eq!(
        serde_json::to_value(&revoked_branch).unwrap()["message"],
        "<redacted>"
    );

    let envelope = serde_json::json!({
        "outer": outer,
        "ancestor": ancestor_snapshot,
        "granted": granted_branch,
        "revoked": revoked_branch,
    });
    let json = serde_json::to_vec(&envelope).unwrap();
    for disclosure in [
        b"ancestor snapshot admission".as_slice(),
        b"branch replacement admission".as_slice(),
    ] {
        assert_eq!(
            json.windows(disclosure.len())
                .filter(|window| *window == disclosure)
                .count(),
            1
        );
    }
    for disclosure in [
        b"ancestor nested admission".as_slice(),
        b"branch nested admission".as_slice(),
    ] {
        assert!(
            !json
                .windows(disclosure.len())
                .any(|window| window == disclosure)
        );
    }
    assert!(
        !json
            .windows(fixture_credential.len())
            .any(|window| window == fixture_credential.as_bytes())
    );

    let ancestor_wire = ancestor_snapshot.encode_ovb().unwrap();
    assert!(
        ancestor_wire
            .windows(b"ancestor snapshot admission".len())
            .any(|window| window == b"ancestor snapshot admission")
    );
    let granted_wire = granted_branch.encode_ovb().unwrap();
    assert!(
        granted_wire
            .windows(b"branch replacement admission".len())
            .any(|window| window == b"branch replacement admission")
    );
    let revoked_wire = revoked_branch.encode_ovb().unwrap();
    assert!(
        !revoked_wire
            .windows(b"ancestor snapshot admission".len())
            .any(|window| window == b"ancestor snapshot admission")
    );
    let outer_wire = outer.encode_ovb().unwrap();
    for disclosure in [
        b"ancestor snapshot admission".as_slice(),
        b"branch replacement admission".as_slice(),
        b"ancestor nested admission".as_slice(),
        b"branch nested admission".as_slice(),
    ] {
        assert!(
            !outer_wire
                .windows(disclosure.len())
                .any(|window| window == disclosure)
        );
    }
    assert!(
        !outer_wire
            .windows(fixture.len())
            .any(|window| window == fixture.as_bytes())
    );
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&outer_wire).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    for cause in decoded["causes"].as_array().unwrap() {
        assert_eq!(cause["message"], "<redacted>");
    }
}

#[test]
fn parent_replacement_preserves_forked_nested_cause_snapshots() {
    let fixture = include_str!("fixtures/secret-surface.orna").trim();
    let fixture_credential = fixture.split('"').nth(1).unwrap();
    let diagnostic = |code: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let old_child = diagnostic("ORNA-E-NESTED-SNAPSHOT-OLD")
        .redacted_with_message(SafeText::new("old child snapshot admission").unwrap())
        .with_cause(
            diagnostic("ORNA-E-NESTED-SNAPSHOT-OLD-LEAF")
                .redacted_with_message(SafeText::new("old nested leaf admission").unwrap()),
        );
    let old_child_snapshot = old_child.clone();
    let old_parent = diagnostic("ORNA-E-NESTED-SNAPSHOT-OLD-PARENT")
        .redacted_with_message(SafeText::new("old parent admission").unwrap())
        .with_cause(old_child);
    let mut parent_destination = old_parent.clone();
    let old_parent_snapshot = parent_destination.clone();

    let new_child = diagnostic("ORNA-E-NESTED-SNAPSHOT-NEW")
        .redacted_with_message(SafeText::new("new child snapshot admission").unwrap())
        .with_cause(
            diagnostic("ORNA-E-NESTED-SNAPSHOT-NEW-LEAF")
                .redacted_with_message(SafeText::new("new nested leaf admission").unwrap()),
        );
    let new_child_snapshot = new_child.clone();
    let new_parent = diagnostic("ORNA-E-NESTED-SNAPSHOT-NEW-PARENT")
        .redacted_with_message(SafeText::new("new parent admission").unwrap())
        .with_cause(new_child);
    parent_destination.clone_from(&new_parent);
    assert_eq!(parent_destination, new_parent);

    // The parent's reused cause slot now holds the replacement subtree. Copies
    // of its former child and former parent keep their independent admissions.
    let outer = Diagnostic::new(
        SafeText::new("ORNA-E-NESTED-SNAPSHOT-OUTER").unwrap(),
        DiagnosticSeverity::Error,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .redacted_with_message(SafeText::new("nested snapshot outer admission").unwrap())
    .with_cause(old_parent_snapshot.clone())
    .with_cause(parent_destination.clone());
    let outer_projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(
        outer_projection["message"],
        "nested snapshot outer admission"
    );
    for parent in outer_projection["causes"].as_array().unwrap() {
        assert_eq!(parent["message"], "<redacted>");
        assert_eq!(parent["causes"][0]["message"], "<redacted>");
        assert_eq!(parent["causes"][0]["causes"][0]["message"], "<redacted>");
    }

    let old_parent_projection = serde_json::to_value(&old_parent_snapshot).unwrap();
    assert_eq!(old_parent_projection["message"], "old parent admission");
    assert_eq!(old_parent_projection["causes"][0]["message"], "<redacted>");
    let new_parent_projection = serde_json::to_value(&parent_destination).unwrap();
    assert_eq!(new_parent_projection["message"], "new parent admission");
    assert_eq!(new_parent_projection["causes"][0]["message"], "<redacted>");
    let old_child_projection = serde_json::to_value(&old_child_snapshot).unwrap();
    assert_eq!(
        old_child_projection["message"],
        "old child snapshot admission"
    );
    assert_eq!(
        old_child_projection["causes"][0]["message"],
        "<redacted>"
    );
    let new_child_projection = serde_json::to_value(&new_child_snapshot).unwrap();
    assert_eq!(
        new_child_projection["message"],
        "new child snapshot admission"
    );
    assert_eq!(
        new_child_projection["causes"][0]["message"],
        "<redacted>"
    );

    let envelope = serde_json::json!({
        "outer": outer,
        "old_parent": old_parent_snapshot,
        "new_parent": parent_destination,
        "old_child": old_child_snapshot,
        "new_child": new_child_snapshot,
    });
    let json = serde_json::to_vec(&envelope).unwrap();
    for disclosure in [
        b"old parent admission".as_slice(),
        b"new parent admission".as_slice(),
        b"old child snapshot admission".as_slice(),
        b"new child snapshot admission".as_slice(),
    ] {
        assert_eq!(
            json.windows(disclosure.len())
                .filter(|window| *window == disclosure)
                .count(),
            1
        );
    }
    for disclosure in [
        b"old nested leaf admission".as_slice(),
        b"new nested leaf admission".as_slice(),
    ] {
        assert!(
            !json
                .windows(disclosure.len())
                .any(|window| window == disclosure)
        );
    }
    assert!(
        !json
            .windows(fixture_credential.len())
            .any(|window| window == fixture_credential.as_bytes())
    );

    let old_child_wire = old_child_snapshot.encode_ovb().unwrap();
    assert!(
        old_child_wire
            .windows(b"old child snapshot admission".len())
            .any(|window| window == b"old child snapshot admission")
    );
    let new_child_wire = new_child_snapshot.encode_ovb().unwrap();
    assert!(
        new_child_wire
            .windows(b"new child snapshot admission".len())
            .any(|window| window == b"new child snapshot admission")
    );
    let parent_wire = parent_destination.encode_ovb().unwrap();
    assert!(
        parent_wire
            .windows(b"new parent admission".len())
            .any(|window| window == b"new parent admission")
    );
    assert!(
        !parent_wire
            .windows(b"new child snapshot admission".len())
            .any(|window| window == b"new child snapshot admission")
    );
    let outer_wire = outer.encode_ovb().unwrap();
    for disclosure in [
        b"old parent admission".as_slice(),
        b"new parent admission".as_slice(),
        b"old child snapshot admission".as_slice(),
        b"new child snapshot admission".as_slice(),
        b"old nested leaf admission".as_slice(),
        b"new nested leaf admission".as_slice(),
    ] {
        assert!(
            !outer_wire
                .windows(disclosure.len())
                .any(|window| window == disclosure)
        );
    }
    assert!(
        !outer_wire
            .windows(fixture.len())
            .any(|window| window == fixture.as_bytes())
    );
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&outer_wire).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    assert_eq!(decoded["causes"][0]["causes"][0]["message"], "<redacted>");
    assert_eq!(decoded["causes"][1]["causes"][0]["message"], "<redacted>");
}

#[test]
fn nested_replacement_preserves_parent_child_and_leaf_snapshots() {
    let fixture = include_str!("fixtures/secret-surface.orna").trim();
    let fixture_credential = fixture.split('"').nth(1).unwrap();
    let diagnostic = |code: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let old_leaf =
        diagnostic("ORNA-E-NESTED-REPLACE-OLD-LEAF").redacted_with_message(
            SafeText::new("old leaf snapshot admission").unwrap(),
        );
    let old_leaf_snapshot = old_leaf.clone();
    let old_child = diagnostic("ORNA-E-NESTED-REPLACE-OLD-CHILD")
        .redacted_with_message(SafeText::new("old child snapshot admission").unwrap())
        .with_cause(old_leaf);
    let old_child_snapshot = old_child.clone();
    let old_parent = diagnostic("ORNA-E-NESTED-REPLACE-OLD-PARENT")
        .redacted_with_message(SafeText::new("old parent snapshot admission").unwrap())
        .with_cause(old_child);
    let mut parent_destination = old_parent.clone();
    let old_parent_snapshot = parent_destination.clone();

    let new_leaf =
        diagnostic("ORNA-E-NESTED-REPLACE-NEW-LEAF").redacted_with_message(
            SafeText::new("new leaf snapshot admission").unwrap(),
        );
    let new_leaf_snapshot = new_leaf.clone();
    let new_child = diagnostic("ORNA-E-NESTED-REPLACE-NEW-CHILD")
        .redacted_with_message(SafeText::new("new child snapshot admission").unwrap())
        .with_cause(new_leaf);
    let new_child_snapshot = new_child.clone();
    let new_parent = diagnostic("ORNA-E-NESTED-REPLACE-NEW-PARENT")
        .redacted_with_message(SafeText::new("new parent snapshot admission").unwrap())
        .with_cause(new_child);

    // Hold each depth before clone_from reuses the parent's cause slots; the
    // replacement must not revoke a separately cloned child or leaf.
    parent_destination.clone_from(&new_parent);
    assert_eq!(parent_destination, new_parent);
    let outer = Diagnostic::new(
        SafeText::new("ORNA-E-NESTED-REPLACE-OUTER").unwrap(),
        DiagnosticSeverity::Error,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .redacted_with_message(SafeText::new("nested replacement outer admission").unwrap())
    .with_cause(old_parent_snapshot.clone())
    .with_cause(parent_destination.clone())
    .with_cause(old_child_snapshot.clone())
    .with_cause(new_child_snapshot.clone())
    .with_cause(old_leaf_snapshot.clone())
    .with_cause(new_leaf_snapshot.clone());

    let outer_projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(
        outer_projection["message"],
        "nested replacement outer admission"
    );
    let causes = outer_projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 6);
    for cause in causes {
        assert_eq!(cause["message"], "<redacted>");
    }
    for parent in &causes[..2] {
        assert_eq!(parent["causes"][0]["message"], "<redacted>");
        assert_eq!(
            parent["causes"][0]["causes"][0]["message"],
            "<redacted>"
        );
    }
    for child in &causes[2..4] {
        assert_eq!(child["causes"][0]["message"], "<redacted>");
    }

    let old_parent_projection = serde_json::to_value(&old_parent_snapshot).unwrap();
    assert_eq!(
        old_parent_projection["message"],
        "old parent snapshot admission"
    );
    assert_eq!(
        old_parent_projection["causes"][0]["message"],
        "<redacted>"
    );
    let new_parent_projection = serde_json::to_value(&parent_destination).unwrap();
    assert_eq!(
        new_parent_projection["message"],
        "new parent snapshot admission"
    );
    assert_eq!(
        new_parent_projection["causes"][0]["message"],
        "<redacted>"
    );
    let old_child_projection = serde_json::to_value(&old_child_snapshot).unwrap();
    assert_eq!(
        old_child_projection["message"],
        "old child snapshot admission"
    );
    assert_eq!(old_child_projection["causes"][0]["message"], "<redacted>");
    let new_child_projection = serde_json::to_value(&new_child_snapshot).unwrap();
    assert_eq!(
        new_child_projection["message"],
        "new child snapshot admission"
    );
    assert_eq!(new_child_projection["causes"][0]["message"], "<redacted>");
    assert_eq!(
        serde_json::to_value(&old_leaf_snapshot).unwrap()["message"],
        "old leaf snapshot admission"
    );
    assert_eq!(
        serde_json::to_value(&new_leaf_snapshot).unwrap()["message"],
        "new leaf snapshot admission"
    );

    let envelope = serde_json::json!({
        "outer": outer,
        "old_parent": old_parent_snapshot,
        "new_parent": parent_destination,
        "old_child": old_child_snapshot,
        "new_child": new_child_snapshot,
        "old_leaf": old_leaf_snapshot,
        "new_leaf": new_leaf_snapshot,
    });
    let json = serde_json::to_vec(&envelope).unwrap();
    for disclosure in [
        b"old parent snapshot admission".as_slice(),
        b"new parent snapshot admission".as_slice(),
        b"old child snapshot admission".as_slice(),
        b"new child snapshot admission".as_slice(),
        b"old leaf snapshot admission".as_slice(),
        b"new leaf snapshot admission".as_slice(),
    ] {
        assert_eq!(
            json.windows(disclosure.len())
                .filter(|window| *window == disclosure)
                .count(),
            1
        );
    }
    assert!(
        !json
            .windows(fixture_credential.len())
            .any(|window| window == fixture_credential.as_bytes())
    );

    let old_child_wire = old_child_snapshot.encode_ovb().unwrap();
    assert!(
        old_child_wire
            .windows(b"old child snapshot admission".len())
            .any(|window| window == b"old child snapshot admission")
    );
    assert!(
        !old_child_wire
            .windows(b"old leaf snapshot admission".len())
            .any(|window| window == b"old leaf snapshot admission")
    );
    let new_child_wire = new_child_snapshot.encode_ovb().unwrap();
    assert!(
        new_child_wire
            .windows(b"new child snapshot admission".len())
            .any(|window| window == b"new child snapshot admission")
    );
    let old_leaf_wire = old_leaf_snapshot.encode_ovb().unwrap();
    assert!(
        old_leaf_wire
            .windows(b"old leaf snapshot admission".len())
            .any(|window| window == b"old leaf snapshot admission")
    );
    let new_leaf_wire = new_leaf_snapshot.encode_ovb().unwrap();
    assert!(
        new_leaf_wire
            .windows(b"new leaf snapshot admission".len())
            .any(|window| window == b"new leaf snapshot admission")
    );
    let outer_wire = outer.encode_ovb().unwrap();
    for disclosure in [
        b"old parent snapshot admission".as_slice(),
        b"new parent snapshot admission".as_slice(),
        b"old child snapshot admission".as_slice(),
        b"new child snapshot admission".as_slice(),
        b"old leaf snapshot admission".as_slice(),
        b"new leaf snapshot admission".as_slice(),
    ] {
        assert!(
            !outer_wire
                .windows(disclosure.len())
                .any(|window| window == disclosure)
        );
    }
    assert!(
        !outer_wire
            .windows(fixture.len())
            .any(|window| window == fixture.as_bytes())
    );
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&outer_wire).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    assert_eq!(decoded["causes"][0]["causes"][0]["message"], "<redacted>");
    assert_eq!(
        decoded["causes"][0]["causes"][0]["causes"][0]["message"],
        "<redacted>"
    );
}

#[test]
fn nested_vector_shrink_and_growth_preserve_held_tail_snapshots() {
    let fixture = include_str!("fixtures/secret-surface.orna").trim();
    let fixture_credential = fixture.split('"').nth(1).unwrap();
    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let branch = |code: &str, message: &str, leaf_code: &str, leaf_message: &str| {
        admitted(code, message).with_cause(admitted(leaf_code, leaf_message))
    };

    let old_a = branch(
        "ORNA-E-TAIL-OLD-A",
        "old branch a admission",
        "ORNA-E-TAIL-OLD-A-LEAF",
        "old leaf a admission",
    );
    let old_b = branch(
        "ORNA-E-TAIL-OLD-B",
        "old branch b admission",
        "ORNA-E-TAIL-OLD-B-LEAF",
        "old leaf b admission",
    );
    let old_c = branch(
        "ORNA-E-TAIL-OLD-C",
        "old branch c admission",
        "ORNA-E-TAIL-OLD-C-LEAF",
        "old leaf c admission",
    );
    let old_b_snapshot = old_b.clone();
    let old_c_snapshot = old_c.clone();
    let old_parent = admitted("ORNA-E-TAIL-OLD-PARENT", "old parent admission")
        .with_cause(old_a)
        .with_cause(old_b)
        .with_cause(old_c);
    let old_parent_snapshot = old_parent.clone();

    let shrink_keep = branch(
        "ORNA-E-TAIL-SHRINK-KEEP",
        "shrink kept branch admission",
        "ORNA-E-TAIL-SHRINK-KEEP-LEAF",
        "shrink kept leaf admission",
    );
    let shrink_parent = admitted(
        "ORNA-E-TAIL-SHRINK-PARENT",
        "shrink parent admission",
    )
    .with_cause(shrink_keep);
    let shrink_parent_snapshot = shrink_parent.clone();
    let mut destination = old_parent;
    destination.clone_from(&shrink_parent);
    assert_eq!(destination, shrink_parent);

    let grown_a = branch(
        "ORNA-E-TAIL-GROWN-A",
        "grown branch a admission",
        "ORNA-E-TAIL-GROWN-A-LEAF",
        "grown leaf a admission",
    );
    let grown_b = branch(
        "ORNA-E-TAIL-GROWN-B",
        "grown branch b admission",
        "ORNA-E-TAIL-GROWN-B-LEAF",
        "grown leaf b admission",
    );
    let grown_c = branch(
        "ORNA-E-TAIL-GROWN-C",
        "grown branch c admission",
        "ORNA-E-TAIL-GROWN-C-LEAF",
        "grown leaf c admission",
    );
    let grown_b_snapshot = grown_b.clone();
    let grown_c_snapshot = grown_c.clone();
    let grown_parent = admitted("ORNA-E-TAIL-GROWN-PARENT", "grown parent admission")
        .with_cause(grown_a)
        .with_cause(grown_b)
        .with_cause(grown_c);
    destination.clone_from(&grown_parent);
    assert_eq!(destination, grown_parent);

    // Keep removed and appended tail values independently: vector resizing
    // replaces this parent's tree but does not revoke those root snapshots.
    let outer = Diagnostic::new(
        SafeText::new("ORNA-E-TAIL-OUTER").unwrap(),
        DiagnosticSeverity::Error,
        SafeText::new(fixture).unwrap(),
    )
    .unwrap()
    .redacted_with_message(SafeText::new("tail outer admission").unwrap())
    .with_cause(destination.clone())
    .with_cause(old_parent_snapshot.clone())
    .with_cause(old_b_snapshot.clone())
    .with_cause(old_c_snapshot.clone())
    .with_cause(shrink_parent_snapshot.clone())
    .with_cause(grown_b_snapshot.clone())
    .with_cause(grown_c_snapshot.clone());
    let outer_projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(outer_projection["message"], "tail outer admission");
    let outer_causes = outer_projection["causes"].as_array().unwrap();
    assert_eq!(outer_causes.len(), 7);
    for cause in outer_causes {
        assert_eq!(cause["message"], "<redacted>");
        for branch in cause["causes"].as_array().into_iter().flatten() {
            assert_eq!(branch["message"], "<redacted>");
            for leaf in branch["causes"].as_array().into_iter().flatten() {
                assert_eq!(leaf["message"], "<redacted>");
            }
        }
    }

    let roots = [
        (&destination, "grown parent admission"),
        (&old_parent_snapshot, "old parent admission"),
        (&old_b_snapshot, "old branch b admission"),
        (&old_c_snapshot, "old branch c admission"),
        (&shrink_parent_snapshot, "shrink parent admission"),
        (&grown_b_snapshot, "grown branch b admission"),
        (&grown_c_snapshot, "grown branch c admission"),
    ];
    for (root, message) in roots {
        let projection = serde_json::to_value(root).unwrap();
        assert_eq!(projection["message"], message);
        assert_eq!(projection["notes"][0], "<redacted>");
        for branch in projection["causes"].as_array().into_iter().flatten() {
            assert_eq!(branch["message"], "<redacted>");
        }
        let wire = root.encode_ovb().unwrap();
        assert!(wire.windows(message.len()).any(|window| window == message.as_bytes()));
        assert!(
            !wire
                .windows(fixture.len())
                .any(|window| window == fixture.as_bytes())
        );
    }

    let envelope = serde_json::json!({
        "outer": outer,
        "destination": destination,
        "old_parent": old_parent_snapshot,
        "old_b": old_b_snapshot,
        "old_c": old_c_snapshot,
        "shrink_parent": shrink_parent_snapshot,
        "grown_b": grown_b_snapshot,
        "grown_c": grown_c_snapshot,
    });
    let json = serde_json::to_vec(&envelope).unwrap();
    for (_, message) in roots {
        assert_eq!(
            json.windows(message.len())
                .filter(|window| *window == message.as_bytes())
                .count(),
            1
        );
    }
    assert!(
        !json
            .windows(fixture_credential.len())
            .any(|window| window == fixture_credential.as_bytes())
    );
    let outer_wire = outer.encode_ovb().unwrap();
    for (_, message) in roots {
        assert!(
            !outer_wire
                .windows(message.len())
                .any(|window| window == message.as_bytes())
        );
    }
    assert!(
        !outer_wire
            .windows(fixture.len())
            .any(|window| window == fixture.as_bytes())
    );
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&outer_wire).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    for cause in decoded["causes"].as_array().unwrap() {
        assert_eq!(cause["message"], "<redacted>");
    }
}

#[test]
fn composed_cause_snapshot_keeps_original_tree_after_source_resizes() {
    let fixture = include_str!("fixtures/secret-surface.orna").trim();
    let fixture_credential = fixture.split('"').nth(1).unwrap();
    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let branch = |code: &str, message: &str, leaf_code: &str, leaf_message: &str| {
        admitted(code, message).with_cause(admitted(leaf_code, leaf_message))
    };

    let old_a = branch(
        "ORNA-E-COMPOSED-SNAPSHOT-OLD-A",
        "old a root admission",
        "ORNA-E-COMPOSED-SNAPSHOT-OLD-A-LEAF",
        "old a leaf admission",
    );
    let old_b = branch(
        "ORNA-E-COMPOSED-SNAPSHOT-OLD-B",
        "old b root admission",
        "ORNA-E-COMPOSED-SNAPSHOT-OLD-B-LEAF",
        "old b leaf admission",
    );
    let old_c = branch(
        "ORNA-E-COMPOSED-SNAPSHOT-OLD-C",
        "old c root admission",
        "ORNA-E-COMPOSED-SNAPSHOT-OLD-C-LEAF",
        "old c leaf admission",
    );
    let old_a_snapshot = old_a.clone();
    let old_b_snapshot = old_b.clone();
    let old_c_snapshot = old_c.clone();
    let original_parent = admitted(
        "ORNA-E-COMPOSED-SNAPSHOT-OLD-PARENT",
        "original parent admission",
    )
    .with_cause(old_a)
    .with_cause(old_b)
    .with_cause(old_c);
    let mut destination = original_parent.clone();

    // Composition captures an owned cause tree. Later resizing the source
    // destination must not reshape or revoke the cause already attached here.
    let outer = admitted(
        "ORNA-E-COMPOSED-SNAPSHOT-OUTER",
        "outer snapshot admission",
    )
    .with_cause(destination.clone());

    let shrink_branch = branch(
        "ORNA-E-COMPOSED-SNAPSHOT-SHRINK",
        "shrink branch admission",
        "ORNA-E-COMPOSED-SNAPSHOT-SHRINK-LEAF",
        "shrink leaf admission",
    );
    let shrink_parent = admitted(
        "ORNA-E-COMPOSED-SNAPSHOT-SHRINK-PARENT",
        "shrink parent admission",
    )
    .with_cause(shrink_branch);
    destination.clone_from(&shrink_parent);
    assert_eq!(destination, shrink_parent);

    let grown_a = branch(
        "ORNA-E-COMPOSED-SNAPSHOT-GROWN-A",
        "grown a root admission",
        "ORNA-E-COMPOSED-SNAPSHOT-GROWN-A-LEAF",
        "grown a leaf admission",
    );
    let grown_b = branch(
        "ORNA-E-COMPOSED-SNAPSHOT-GROWN-B",
        "grown b root admission",
        "ORNA-E-COMPOSED-SNAPSHOT-GROWN-B-LEAF",
        "grown b leaf admission",
    );
    let grown_a_snapshot = grown_a.clone();
    let grown_b_snapshot = grown_b.clone();
    let grown_parent = admitted(
        "ORNA-E-COMPOSED-SNAPSHOT-GROWN-PARENT",
        "grown parent admission",
    )
    .with_cause(grown_a)
    .with_cause(grown_b);
    destination.clone_from(&grown_parent);
    assert_eq!(destination, grown_parent);

    let outer_projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(outer_projection["message"], "outer snapshot admission");
    let original_projection = &outer_projection["causes"][0];
    assert_eq!(original_projection["message"], "<redacted>");
    let original_causes = original_projection["causes"].as_array().unwrap();
    assert_eq!(original_causes.len(), 3);
    for cause in original_causes {
        assert_eq!(cause["message"], "<redacted>");
        assert_eq!(cause["causes"][0]["message"], "<redacted>");
    }
    let destination_projection = serde_json::to_value(&destination).unwrap();
    assert_eq!(destination_projection["message"], "grown parent admission");
    assert_eq!(destination_projection["causes"].as_array().unwrap().len(), 2);
    for cause in destination_projection["causes"].as_array().unwrap() {
        assert_eq!(cause["message"], "<redacted>");
    }

    let roots = [
        (&outer, "outer snapshot admission"),
        (&destination, "grown parent admission"),
        (&original_parent, "original parent admission"),
        (&old_a_snapshot, "old a root admission"),
        (&old_b_snapshot, "old b root admission"),
        (&old_c_snapshot, "old c root admission"),
        (&shrink_parent, "shrink parent admission"),
        (&grown_a_snapshot, "grown a root admission"),
        (&grown_b_snapshot, "grown b root admission"),
    ];
    for (root, message) in roots {
        let projection = serde_json::to_value(root).unwrap();
        assert_eq!(projection["message"], message);
        let wire = root.encode_ovb().unwrap();
        assert!(wire.windows(message.len()).any(|window| window == message.as_bytes()));
        assert!(
            !wire
                .windows(fixture.len())
                .any(|window| window == fixture.as_bytes())
        );
    }

    let envelope = serde_json::json!({
        "outer": outer,
        "destination": destination,
        "original_parent": original_parent,
        "old_a": old_a_snapshot,
        "old_b": old_b_snapshot,
        "old_c": old_c_snapshot,
        "shrink_parent": shrink_parent,
        "grown_a": grown_a_snapshot,
        "grown_b": grown_b_snapshot,
    });
    let json = serde_json::to_vec(&envelope).unwrap();
    for (_, message) in roots {
        assert_eq!(
            json.windows(message.len())
                .filter(|window| *window == message.as_bytes())
                .count(),
            1
        );
    }
    for disclosure in [
        b"old a leaf admission".as_slice(),
        b"old b leaf admission".as_slice(),
        b"old c leaf admission".as_slice(),
        b"shrink leaf admission".as_slice(),
        b"grown a leaf admission".as_slice(),
        b"grown b leaf admission".as_slice(),
    ] {
        assert!(
            !json
                .windows(disclosure.len())
                .any(|window| window == disclosure)
        );
    }
    assert!(
        !json
            .windows(fixture_credential.len())
            .any(|window| window == fixture_credential.as_bytes())
    );

    let outer_wire = outer.encode_ovb().unwrap();
    for (_, message) in roots.iter().skip(1) {
        assert!(
            !outer_wire
                .windows(message.len())
                .any(|window| window == message.as_bytes())
        );
    }
    assert!(
        !outer_wire
            .windows(fixture.len())
            .any(|window| window == fixture.as_bytes())
    );
    let decoded_outer = serde_json::to_value(Diagnostic::decode_ovb(&outer_wire).unwrap()).unwrap();
    assert_eq!(decoded_outer["message"], "<redacted>");
    assert_eq!(decoded_outer["causes"][0]["message"], "<redacted>");
    assert_eq!(
        decoded_outer["causes"][0]["causes"].as_array().unwrap().len(),
        3
    );
    assert_eq!(
        decoded_outer["causes"][0]["causes"][0]["causes"][0]["message"],
        "<redacted>"
    );
}

#[test]
fn pre_resize_composed_projection_is_stable_after_source_mutation() {
    let fixture = include_str!("fixtures/secret-surface.orna").trim();
    let fixture_credential = fixture.split('"').nth(1).unwrap();
    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let original = admitted("ORNA-E-PRESIZE-ORIGINAL", "pre-resize parent admission")
        .with_cause(admitted("ORNA-E-PRESIZE-OLD-A", "pre-resize branch a"))
        .with_cause(admitted("ORNA-E-PRESIZE-OLD-B", "pre-resize branch b"))
        .with_cause(admitted("ORNA-E-PRESIZE-OLD-C", "pre-resize branch c"));
    let mut destination = original.clone();

    // Capture both public projections before mutating their source. The cause
    // edge owns a snapshot and boundary serialization must remain non-mutating.
    let outer = admitted("ORNA-E-PRESIZE-OUTER", "outer root admission")
        .with_cause(destination.clone());
    let before_projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(before_projection["message"], "outer root admission");
    assert_eq!(before_projection["causes"][0]["message"], "<redacted>");
    assert_eq!(
        before_projection["causes"][0]["causes"].as_array().unwrap().len(),
        3
    );
    let before_json = serde_json::to_vec(&outer).unwrap();
    let before_wire = outer.encode_ovb().unwrap();

    let shrink = admitted("ORNA-E-PRESIZE-SHRUNK", "shrink root admission")
        .with_cause(admitted("ORNA-E-PRESIZE-SHRINK-CAUSE", "shrink cause"));
    destination.clone_from(&shrink);
    assert_eq!(destination, shrink);
    let grown = admitted("ORNA-E-PRESIZE-GROWN", "grown root admission")
        .with_cause(admitted("ORNA-E-PRESIZE-GROWN-A", "grown cause a"))
        .with_cause(admitted("ORNA-E-PRESIZE-GROWN-B", "grown cause b"));
    destination.clone_from(&grown);
    assert_eq!(destination, grown);

    let after_projection = serde_json::to_value(&outer).unwrap();
    let after_json = serde_json::to_vec(&outer).unwrap();
    let after_wire = outer.encode_ovb().unwrap();
    assert_eq!(after_projection, before_projection);
    assert_eq!(after_json, before_json);
    assert_eq!(after_wire, before_wire);
    assert!(
        !after_json
            .windows(fixture_credential.len())
            .any(|window| window == fixture_credential.as_bytes())
    );
    assert!(
        !after_wire
            .windows(fixture.len())
            .any(|window| window == fixture.as_bytes())
    );

    let destination_projection = serde_json::to_value(&destination).unwrap();
    assert_eq!(destination_projection["message"], "grown root admission");
    assert_eq!(destination_projection["causes"].as_array().unwrap().len(), 2);
    for cause in destination_projection["causes"].as_array().unwrap() {
        assert_eq!(cause["message"], "<redacted>");
    }
    let destination_wire = destination.encode_ovb().unwrap();
    assert!(
        destination_wire
            .windows(b"grown root admission".len())
            .any(|window| window == b"grown root admission")
    );
    for disclosure in [
        b"pre-resize branch a".as_slice(),
        b"pre-resize branch b".as_slice(),
        b"pre-resize branch c".as_slice(),
        b"grown cause a".as_slice(),
        b"grown cause b".as_slice(),
    ] {
        assert!(
            !destination_wire
                .windows(disclosure.len())
                .any(|window| window == disclosure)
        );
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&after_wire).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    assert_eq!(decoded["causes"][0]["message"], "<redacted>");
    assert_eq!(decoded["causes"][0]["causes"].as_array().unwrap().len(), 3);
}

#[test]
fn pre_resize_snapshot_keeps_its_slot_when_later_sizes_are_composed() {
    let fixture = include_str!("fixtures/secret-surface.orna").trim();
    let fixture_credential = fixture.split('"').nth(1).unwrap();
    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let branch = |code: &str, message: &str| {
        admitted(code, message).with_cause(admitted("ORNA-E-COVER-LEAF", "covered leaf admission"))
    };

    let mut destination = admitted("ORNA-E-COVER-OLD", "before resize admission")
        .with_cause(branch("ORNA-E-COVER-OLD-A", "old branch a admission"))
        .with_cause(branch("ORNA-E-COVER-OLD-B", "old branch b admission"))
        .with_cause(branch("ORNA-E-COVER-OLD-C", "old branch c admission"));
    // Each clone is a value snapshot with root-local display trust. Keep the
    // pre-resize and intermediate trees while replacing the mutable source.
    let before_resize = destination.clone();
    let before_projection = serde_json::to_value(&before_resize).unwrap();
    let before_wire = before_resize.encode_ovb().unwrap();

    let shrink = admitted("ORNA-E-COVER-SHRINK", "shrink admission")
        .with_cause(branch("ORNA-E-COVER-SHRINK-A", "shrink branch admission"));
    destination.clone_from(&shrink);
    let after_shrink = destination.clone();
    let shrink_projection = serde_json::to_value(&after_shrink).unwrap();
    let shrink_wire = after_shrink.encode_ovb().unwrap();

    let grown = admitted("ORNA-E-COVER-GROWN", "grown admission")
        .with_cause(branch("ORNA-E-COVER-GROWN-A", "grown branch a admission"))
        .with_cause(branch("ORNA-E-COVER-GROWN-B", "grown branch b admission"));
    destination.clone_from(&grown);
    assert_eq!(destination, grown);
    assert_eq!(serde_json::to_value(&before_resize).unwrap(), before_projection);
    assert_eq!(before_resize.encode_ovb().unwrap(), before_wire);
    assert_eq!(serde_json::to_value(&after_shrink).unwrap(), shrink_projection);
    assert_eq!(after_shrink.encode_ovb().unwrap(), shrink_wire);

    let outer = admitted("ORNA-E-COVER-OUTER", "cover outer admission")
        .with_cause(before_resize.clone())
        .with_cause(after_shrink.clone())
        .with_cause(destination.clone());
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["message"], "cover outer admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 3);
    for (cause, expected_len) in causes.iter().zip([3, 1, 2]) {
        assert_eq!(cause["message"], "<redacted>");
        let branches = cause["causes"].as_array().unwrap();
        assert_eq!(branches.len(), expected_len);
        for branch in branches {
            assert_eq!(branch["message"], "<redacted>");
            assert_eq!(branch["causes"][0]["message"], "<redacted>");
        }
    }

    let envelope = serde_json::json!({
        "outer": outer,
        "before_resize": before_resize,
        "after_shrink": after_shrink,
        "after_growth": destination,
    });
    let json = serde_json::to_vec(&envelope).unwrap();
    for message in [
        b"cover outer admission".as_slice(),
        b"before resize admission".as_slice(),
        b"shrink admission".as_slice(),
        b"grown admission".as_slice(),
    ] {
        assert_eq!(
            json.windows(message.len())
                .filter(|window| *window == message)
                .count(),
            1
        );
    }
    for disclosure in [
        fixture_credential.as_bytes(),
        b"old branch a admission".as_slice(),
        b"old branch b admission".as_slice(),
        b"old branch c admission".as_slice(),
        b"shrink branch admission".as_slice(),
        b"grown branch a admission".as_slice(),
        b"grown branch b admission".as_slice(),
        b"covered leaf admission".as_slice(),
    ] {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
    }

    let wire = outer.encode_ovb().unwrap();
    for disclosure in [
        fixture.as_bytes(),
        b"before resize admission".as_slice(),
        b"shrink admission".as_slice(),
        b"grown admission".as_slice(),
        b"old branch a admission".as_slice(),
        b"shrink branch admission".as_slice(),
        b"grown branch b admission".as_slice(),
        b"covered leaf admission".as_slice(),
    ] {
        assert!(!wire.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&wire).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    assert_eq!(
        decoded["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|cause| cause["causes"].as_array().unwrap().len())
            .collect::<Vec<_>>(),
        [3, 1, 2]
    );
}

#[test]
fn nested_pre_resize_composition_survives_late_recomposition() {
    let fixture = include_str!("fixtures/secret-surface.orna").trim();
    let fixture_credential = fixture.split('"').nth(1).unwrap();
    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let original = admitted("ORNA-E-NESTED-COVER-OLD", "old source admission")
        .with_cause(admitted("ORNA-E-NESTED-COVER-A", "old cause a admission"))
        .with_cause(admitted("ORNA-E-NESTED-COVER-B", "old cause b admission"))
        .with_cause(admitted("ORNA-E-NESTED-COVER-C", "old cause c admission"));
    let mut source = original.clone();

    // Capture one composed tree before replacement, then compose that held
    // tree again later. Trust is local to each root; nesting redacts it.
    let before_resize = admitted(
        "ORNA-E-NESTED-COVER-EARLY",
        "early composition admission",
    )
    .with_cause(source.clone());
    let before_projection = serde_json::to_value(&before_resize).unwrap();
    let before_wire = before_resize.encode_ovb().unwrap();
    assert_eq!(
        before_projection["causes"][0]["causes"].as_array().unwrap().len(),
        3
    );

    let shrink = admitted("ORNA-E-NESTED-COVER-SHRINK", "shrink source admission")
        .with_cause(admitted("ORNA-E-NESTED-COVER-S", "shrink cause admission"));
    source.clone_from(&shrink);
    let grown = admitted("ORNA-E-NESTED-COVER-GROWN", "grown source admission")
        .with_cause(admitted("ORNA-E-NESTED-COVER-GA", "grown cause a admission"))
        .with_cause(admitted("ORNA-E-NESTED-COVER-GB", "grown cause b admission"));
    source.clone_from(&grown);
    assert_eq!(source, grown);
    assert_eq!(serde_json::to_value(&before_resize).unwrap(), before_projection);
    assert_eq!(before_resize.encode_ovb().unwrap(), before_wire);

    let later = admitted("ORNA-E-NESTED-COVER-LATE", "late composition admission")
        .with_cause(before_resize.clone())
        .with_cause(source.clone());
    let projection = serde_json::to_value(&later).unwrap();
    assert_eq!(projection["message"], "late composition admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 2);
    assert_eq!(causes[0]["message"], "<redacted>");
    assert_eq!(causes[0]["causes"].as_array().unwrap().len(), 1);
    let old_snapshot = &causes[0]["causes"][0];
    assert_eq!(old_snapshot["message"], "<redacted>");
    assert_eq!(old_snapshot["causes"].as_array().unwrap().len(), 3);
    for cause in old_snapshot["causes"].as_array().unwrap() {
        assert_eq!(cause["message"], "<redacted>");
    }
    assert_eq!(causes[1]["message"], "<redacted>");
    assert_eq!(causes[1]["causes"].as_array().unwrap().len(), 2);
    for cause in causes[1]["causes"].as_array().unwrap() {
        assert_eq!(cause["message"], "<redacted>");
    }

    let envelope = serde_json::json!({
        "later": later,
        "before_resize": before_resize,
        "after_growth": source,
    });
    let json = serde_json::to_vec(&envelope).unwrap();
    for message in [
        b"late composition admission".as_slice(),
        b"early composition admission".as_slice(),
        b"grown source admission".as_slice(),
    ] {
        assert_eq!(
            json.windows(message.len())
                .filter(|window| *window == message)
                .count(),
            1
        );
    }
    for disclosure in [
        fixture_credential.as_bytes(),
        b"old source admission".as_slice(),
        b"old cause a admission".as_slice(),
        b"old cause b admission".as_slice(),
        b"old cause c admission".as_slice(),
        b"grown cause a admission".as_slice(),
        b"grown cause b admission".as_slice(),
    ] {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
    }

    let wire = later.encode_ovb().unwrap();
    for disclosure in [
        fixture.as_bytes(),
        b"early composition admission".as_slice(),
        b"old source admission".as_slice(),
        b"old cause c admission".as_slice(),
        b"grown source admission".as_slice(),
        b"grown cause b admission".as_slice(),
    ] {
        assert!(!wire.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&wire).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    assert_eq!(
        decoded["causes"][0]["causes"][0]["causes"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(decoded["causes"][1]["causes"].as_array().unwrap().len(), 2);
}

#[test]
fn pre_resize_composition_snapshot_survives_parent_vector_replacement() {
    let fixture = include_str!("fixtures/secret-surface.orna").trim();
    let fixture_credential = fixture.split('"').nth(1).unwrap();
    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let original = admitted("ORNA-E-PARENT-COVER-OLD", "old nested admission")
        .with_cause(admitted("ORNA-E-PARENT-COVER-A", "old child a admission"))
        .with_cause(admitted("ORNA-E-PARENT-COVER-B", "old child b admission"))
        .with_cause(admitted("ORNA-E-PARENT-COVER-C", "old child c admission"));
    let before_resize = admitted("ORNA-E-PARENT-COVER-EARLY", "early parent admission")
        .with_cause(original);
    let before_projection = serde_json::to_value(&before_resize).unwrap();
    let before_wire = before_resize.encode_ovb().unwrap();
    let mut destination = before_resize.clone();

    let shrink = admitted("ORNA-E-PARENT-COVER-SHRINK", "shrink parent admission")
        .with_cause(admitted("ORNA-E-PARENT-COVER-S", "shrink child admission"));
    destination.clone_from(&shrink);
    let grown = admitted("ORNA-E-PARENT-COVER-GROWN", "grown parent admission")
        .with_cause(admitted("ORNA-E-PARENT-COVER-GA", "grown child a admission"))
        .with_cause(admitted("ORNA-E-PARENT-COVER-GB", "grown child b admission"));
    destination.clone_from(&grown);
    assert_eq!(destination, grown);
    assert_eq!(serde_json::to_value(&before_resize).unwrap(), before_projection);
    assert_eq!(before_resize.encode_ovb().unwrap(), before_wire);

    // Replacing the composed parent must not rebind an earlier snapshot of
    // that composition; nesting either version redacts its local admission.
    let outer = admitted("ORNA-E-PARENT-COVER-OUTER", "outer parent admission")
        .with_cause(before_resize.clone())
        .with_cause(destination.clone());
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["message"], "outer parent admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 2);
    assert_eq!(causes[0]["message"], "<redacted>");
    assert_eq!(causes[0]["causes"].as_array().unwrap().len(), 1);
    let old_parent = &causes[0]["causes"][0];
    assert_eq!(old_parent["message"], "<redacted>");
    assert_eq!(old_parent["causes"].as_array().unwrap().len(), 3);
    for cause in old_parent["causes"].as_array().unwrap() {
        assert_eq!(cause["message"], "<redacted>");
    }
    assert_eq!(causes[1]["message"], "<redacted>");
    assert_eq!(causes[1]["causes"].as_array().unwrap().len(), 2);
    for cause in causes[1]["causes"].as_array().unwrap() {
        assert_eq!(cause["message"], "<redacted>");
    }

    let envelope = serde_json::json!({
        "outer": outer,
        "before_resize": before_resize,
        "after_growth": destination,
    });
    let json = serde_json::to_vec(&envelope).unwrap();
    for message in [
        b"outer parent admission".as_slice(),
        b"early parent admission".as_slice(),
        b"grown parent admission".as_slice(),
    ] {
        assert_eq!(
            json.windows(message.len())
                .filter(|window| *window == message)
                .count(),
            1
        );
    }
    for disclosure in [
        fixture_credential.as_bytes(),
        b"old nested admission".as_slice(),
        b"old child a admission".as_slice(),
        b"old child b admission".as_slice(),
        b"old child c admission".as_slice(),
        b"grown child a admission".as_slice(),
        b"grown child b admission".as_slice(),
    ] {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
    }

    let wire = outer.encode_ovb().unwrap();
    assert!(
        wire.windows(b"outer parent admission".len())
            .any(|window| window == b"outer parent admission")
    );
    for disclosure in [
        fixture.as_bytes(),
        b"early parent admission".as_slice(),
        b"old nested admission".as_slice(),
        b"old child c admission".as_slice(),
        b"grown parent admission".as_slice(),
        b"grown child b admission".as_slice(),
    ] {
        assert!(!wire.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&wire).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    assert_eq!(
        decoded["causes"][0]["causes"][0]["causes"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(decoded["causes"][1]["causes"].as_array().unwrap().len(), 2);
}

#[test]
fn pre_resize_parent_vector_clone_from_keeps_secret_admission_local() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let original = admitted("ORNA-E-VEC-PARENT-OLD", "old parent admission")
        .with_cause(admitted("ORNA-E-VEC-OLD-A", "old child a admission"))
        .with_cause(admitted("ORNA-E-VEC-OLD-B", "old child b admission"))
        .with_cause(admitted("ORNA-E-VEC-OLD-C", "old child c admission"));
    let mut parent = vec![original];
    let before_replacement = parent[0].clone();
    let before_projection = serde_json::to_value(&before_replacement).unwrap();
    let before_wire = before_replacement.encode_ovb().unwrap();

    let shrink = admitted("ORNA-E-VEC-PARENT-SHRINK", "shrink parent admission")
        .with_cause(admitted("ORNA-E-VEC-SHRINK-CHILD", "shrink child admission"));
    parent.clone_from(&vec![shrink]);
    let grown = admitted("ORNA-E-VEC-PARENT-GROWN", "grown parent admission")
        .with_cause(admitted("ORNA-E-VEC-GROWN-A", "grown child a admission"))
        .with_cause(admitted("ORNA-E-VEC-GROWN-B", "grown child b admission"));
    parent.clone_from(&vec![grown.clone()]);

    assert_eq!(parent[0], grown);
    assert_eq!(serde_json::to_value(&before_replacement).unwrap(), before_projection);
    assert_eq!(before_replacement.encode_ovb().unwrap(), before_wire);
    let current_projection = serde_json::to_value(&parent[0]).unwrap();
    assert_eq!(current_projection["message"], "grown parent admission");
    assert_eq!(current_projection["causes"].as_array().unwrap().len(), 2);
    for cause in current_projection["causes"].as_array().unwrap() {
        assert_eq!(cause["message"], "<redacted>");
    }

    let outer = admitted("ORNA-E-VEC-PARENT-OUTER", "outer parent admission")
        .with_cause(before_replacement.clone())
        .with_cause(parent[0].clone());
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["message"], "outer parent admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 2);
    assert_eq!(causes[0]["message"], "<redacted>");
    assert_eq!(causes[0]["causes"].as_array().unwrap().len(), 3);
    assert_eq!(causes[1]["message"], "<redacted>");
    assert_eq!(causes[1]["causes"].as_array().unwrap().len(), 2);

    let envelope = serde_json::json!({
        "outer": outer,
        "before_replacement": before_replacement,
        "after_replacement": parent,
    });
    let json = serde_json::to_vec(&envelope).unwrap();
    for message in [
        b"outer parent admission".as_slice(),
        b"old parent admission".as_slice(),
        b"grown parent admission".as_slice(),
    ] {
        assert_eq!(
            json.windows(message.len())
                .filter(|window| *window == message)
                .count(),
            1
        );
    }
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            b"old child a admission".as_slice(),
            b"old child b admission".as_slice(),
            b"old child c admission".as_slice(),
            b"grown child a admission".as_slice(),
            b"grown child b admission".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
    }

    let wire = outer.encode_ovb().unwrap();
    assert!(
        wire.windows(b"outer parent admission".len())
            .any(|window| window == b"outer parent admission")
    );
    for disclosure in [
        fixture.as_bytes(),
        b"old parent admission".as_slice(),
        b"old child c admission".as_slice(),
        b"grown parent admission".as_slice(),
        b"grown child b admission".as_slice(),
    ]
    .into_iter()
    .chain(fixture_credentials.iter().map(|value| value.as_bytes()))
    {
        assert!(!wire.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&wire).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    assert_eq!(decoded["causes"][0]["causes"].as_array().unwrap().len(), 3);
    assert_eq!(decoded["causes"][1]["causes"].as_array().unwrap().len(), 2);
}

#[test]
fn pre_resize_parent_vector_empty_round_trip_keeps_snapshots_isolated() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let original = vec![
        admitted("ORNA-E-EMPTY-PARENT-OLD-A", "old parent a admission")
            .with_cause(admitted("ORNA-E-EMPTY-OLD-A-CHILD", "old child a admission")),
        admitted("ORNA-E-EMPTY-PARENT-OLD-B", "old parent b admission")
            .with_cause(admitted("ORNA-E-EMPTY-OLD-B-CHILD-A", "old child b a admission"))
            .with_cause(admitted("ORNA-E-EMPTY-OLD-B-CHILD-B", "old child b b admission")),
    ];
    let mut destination = original.clone();
    let before_empty = destination.clone();
    let before_projection = serde_json::to_value(&before_empty).unwrap();
    let before_wires = before_empty
        .iter()
        .map(|diagnostic| diagnostic.encode_ovb().unwrap())
        .collect::<Vec<_>>();

    // Clear the parent vector, then repopulate it. Earlier values are owned
    // snapshots; reusing the vector allocation cannot transfer their local
    // root-message admission to the replacement parents.
    let empty: Vec<Diagnostic> = Vec::new();
    destination.clone_from(&empty);
    assert!(destination.is_empty());
    let replacement = vec![
        admitted("ORNA-E-EMPTY-PARENT-NEW-A", "new parent a admission")
            .with_cause(admitted("ORNA-E-EMPTY-NEW-A-CHILD-A", "new child a a admission"))
            .with_cause(admitted("ORNA-E-EMPTY-NEW-A-CHILD-B", "new child a b admission")),
        admitted("ORNA-E-EMPTY-PARENT-NEW-B", "new parent b admission")
            .with_cause(admitted("ORNA-E-EMPTY-NEW-B-CHILD", "new child b admission")),
    ];
    destination.clone_from(&replacement);

    assert_eq!(destination, replacement);
    assert_eq!(serde_json::to_value(&before_empty).unwrap(), before_projection);
    assert_eq!(
        before_empty
            .iter()
            .map(|diagnostic| diagnostic.encode_ovb().unwrap())
            .collect::<Vec<_>>(),
        before_wires
    );
    let retained_projection = serde_json::to_value(&before_empty).unwrap();
    assert_eq!(retained_projection[0]["message"], "old parent a admission");
    assert_eq!(retained_projection[0]["causes"][0]["message"], "<redacted>");
    assert_eq!(retained_projection[1]["message"], "old parent b admission");
    assert_eq!(retained_projection[1]["causes"].as_array().unwrap().len(), 2);

    let outer = admitted("ORNA-E-EMPTY-PARENT-OUTER", "outer parent admission")
        .with_cause(before_empty[0].clone())
        .with_cause(before_empty[1].clone())
        .with_cause(destination[0].clone())
        .with_cause(destination[1].clone());
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["message"], "outer parent admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 4);
    for (cause, expected_children) in causes.iter().zip([1, 2, 2, 1]) {
        assert_eq!(cause["message"], "<redacted>");
        assert_eq!(cause["causes"].as_array().unwrap().len(), expected_children);
        for child in cause["causes"].as_array().unwrap() {
            assert_eq!(child["message"], "<redacted>");
        }
    }

    let envelope = serde_json::json!({
        "outer": outer,
        "before_empty": before_empty,
        "after_repopulation": destination,
    });
    let json = serde_json::to_vec(&envelope).unwrap();
    for message in [
        b"outer parent admission".as_slice(),
        b"old parent a admission".as_slice(),
        b"old parent b admission".as_slice(),
        b"new parent a admission".as_slice(),
        b"new parent b admission".as_slice(),
    ] {
        assert_eq!(
            json.windows(message.len())
                .filter(|window| *window == message)
                .count(),
            1
        );
    }
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            b"old child a admission".as_slice(),
            b"old child b a admission".as_slice(),
            b"old child b b admission".as_slice(),
            b"new child a a admission".as_slice(),
            b"new child a b admission".as_slice(),
            b"new child b admission".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
    }

    let wire = outer.encode_ovb().unwrap();
    assert!(
        wire.windows(b"outer parent admission".len())
            .any(|window| window == b"outer parent admission")
    );
    for disclosure in [
        fixture.as_bytes(),
        b"old parent a admission".as_slice(),
        b"old child b b admission".as_slice(),
        b"new parent a admission".as_slice(),
        b"new child b admission".as_slice(),
    ]
    .into_iter()
    .chain(fixture_credentials.iter().map(|value| value.as_bytes()))
    {
        assert!(!wire.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&wire).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    assert_eq!(
        decoded["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|cause| cause["causes"].as_array().unwrap().len())
            .collect::<Vec<_>>(),
        [1, 2, 2, 1]
    );
}

#[test]
fn pre_resize_parent_vector_nonempty_resize_keeps_closure_snapshots() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let original = vec![
        admitted("ORNA-E-CLOSURE-OLD-A", "old parent a admission")
            .with_cause(admitted("ORNA-E-CLOSURE-OLD-A-CHILD", "old child a admission")),
        admitted("ORNA-E-CLOSURE-OLD-B", "old parent b admission")
            .with_cause(admitted("ORNA-E-CLOSURE-OLD-B-CHILD-A", "old child b a admission"))
            .with_cause(admitted("ORNA-E-CLOSURE-OLD-B-CHILD-B", "old child b b admission")),
        admitted("ORNA-E-CLOSURE-OLD-TAIL", "old parent tail admission")
            .with_cause(admitted("ORNA-E-CLOSURE-OLD-TAIL-CHILD", "old tail child admission")),
    ];
    let mut destination = original.clone();
    let before_resize = destination.clone();
    let before_projection = serde_json::to_value(&before_resize).unwrap();
    let before_wires = before_resize
        .iter()
        .map(|diagnostic| diagnostic.encode_ovb().unwrap())
        .collect::<Vec<_>>();

    // Shrinking a nonempty vector truncates its old tail. Growing it again
    // reuses the surviving root slot and appends new roots; the held snapshots
    // keep their original disclosure boundary across both transitions.
    let shrink = vec![
        admitted("ORNA-E-CLOSURE-SHRINK", "shrink parent admission")
            .with_cause(admitted("ORNA-E-CLOSURE-SHRINK-CHILD", "shrink child admission")),
    ];
    destination.clone_from(&shrink);
    let after_shrink = destination.clone();
    let shrink_projection = serde_json::to_value(&after_shrink).unwrap();
    let shrink_wires = after_shrink
        .iter()
        .map(|diagnostic| diagnostic.encode_ovb().unwrap())
        .collect::<Vec<_>>();

    let grown = vec![
        admitted("ORNA-E-CLOSURE-GROWN-A", "grown parent a admission")
            .with_cause(admitted("ORNA-E-CLOSURE-GROWN-A-CHILD-A", "grown child a a admission"))
            .with_cause(admitted("ORNA-E-CLOSURE-GROWN-A-CHILD-B", "grown child a b admission")),
        admitted("ORNA-E-CLOSURE-GROWN-B", "grown parent b admission")
            .with_cause(admitted("ORNA-E-CLOSURE-GROWN-B-CHILD", "grown child b admission")),
        admitted("ORNA-E-CLOSURE-GROWN-TAIL", "grown parent tail admission"),
    ];
    destination.clone_from(&grown);

    assert_eq!(destination, grown);
    assert_eq!(serde_json::to_value(&before_resize).unwrap(), before_projection);
    assert_eq!(
        before_resize
            .iter()
            .map(|diagnostic| diagnostic.encode_ovb().unwrap())
            .collect::<Vec<_>>(),
        before_wires
    );
    assert_eq!(serde_json::to_value(&after_shrink).unwrap(), shrink_projection);
    assert_eq!(
        after_shrink
            .iter()
            .map(|diagnostic| diagnostic.encode_ovb().unwrap())
            .collect::<Vec<_>>(),
        shrink_wires
    );

    let outer = admitted("ORNA-E-CLOSURE-OUTER", "outer closure admission")
        .with_cause(before_resize[0].clone())
        .with_cause(before_resize[1].clone())
        .with_cause(before_resize[2].clone())
        .with_cause(after_shrink[0].clone())
        .with_cause(destination[0].clone())
        .with_cause(destination[1].clone())
        .with_cause(destination[2].clone());
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["message"], "outer closure admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 7);
    for (cause, expected_children) in causes.iter().zip([1, 2, 1, 1, 2, 1, 0]) {
        assert_eq!(cause["message"], "<redacted>");
        let children = cause["causes"].as_array().unwrap();
        assert_eq!(children.len(), expected_children);
        for child in children {
            assert_eq!(child["message"], "<redacted>");
        }
    }

    let envelope = serde_json::json!({
        "outer": outer,
        "before_resize": before_resize,
        "after_shrink": after_shrink,
        "after_growth": destination,
    });
    let json = serde_json::to_vec(&envelope).unwrap();
    for message in [
        b"outer closure admission".as_slice(),
        b"old parent a admission".as_slice(),
        b"old parent b admission".as_slice(),
        b"old parent tail admission".as_slice(),
        b"shrink parent admission".as_slice(),
        b"grown parent a admission".as_slice(),
        b"grown parent b admission".as_slice(),
        b"grown parent tail admission".as_slice(),
    ] {
        assert_eq!(
            json.windows(message.len())
                .filter(|window| *window == message)
                .count(),
            1
        );
    }
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            b"old child a admission".as_slice(),
            b"old child b a admission".as_slice(),
            b"old child b b admission".as_slice(),
            b"old tail child admission".as_slice(),
            b"shrink child admission".as_slice(),
            b"grown child a a admission".as_slice(),
            b"grown child a b admission".as_slice(),
            b"grown child b admission".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
    }

    let wire = outer.encode_ovb().unwrap();
    assert!(
        wire.windows(b"outer closure admission".len())
            .any(|window| window == b"outer closure admission")
    );
    for disclosure in [
        fixture.as_bytes(),
        b"old parent tail admission".as_slice(),
        b"old tail child admission".as_slice(),
        b"shrink parent admission".as_slice(),
        b"grown parent a admission".as_slice(),
        b"grown child b admission".as_slice(),
    ]
    .into_iter()
    .chain(fixture_credentials.iter().map(|value| value.as_bytes()))
    {
        assert!(!wire.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&wire).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    assert_eq!(
        decoded["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|cause| cause["causes"].as_array().unwrap().len())
            .collect::<Vec<_>>(),
        [1, 2, 1, 1, 2, 1, 0]
    );
}

#[test]
fn parent_replacement_closures_keep_captured_diagnostic_snapshots_local() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let original = vec![
        admitted(
            "ORNA-E-CLOSURE-SNAPSHOT-OLD",
            "old closure parent admission",
        )
        .with_cause(admitted(
            "ORNA-E-CLOSURE-SNAPSHOT-OLD-CHILD",
            "old closure child admission",
        )),
        admitted("ORNA-E-CLOSURE-SNAPSHOT-TAIL", "old closure tail admission"),
    ];
    let mut destination = original.clone();
    let before_replacement = destination.clone();
    let before_projection = serde_json::to_value(&before_replacement).unwrap();
    let before_wires = before_replacement
        .iter()
        .map(|diagnostic| diagnostic.encode_ovb().unwrap())
        .collect::<Vec<_>>();

    // The reference defines closure captures as immutable lexical values but
    // leaves host clone_from details open. Treat captures as owned snapshots:
    // replacing the live parent must not rewrite a previously captured value.
    let old_snapshot = before_replacement.clone();
    let capture_old = move || old_snapshot[0].clone();
    let mut replace_and_snapshot = |replacement: &[Diagnostic]| {
        destination.clone_from(&replacement.to_vec());
        destination.clone()
    };
    let shrink = vec![
        admitted(
            "ORNA-E-CLOSURE-SNAPSHOT-SHRINK",
            "shrink closure parent admission",
        )
        .with_cause(admitted(
            "ORNA-E-CLOSURE-SNAPSHOT-SHRINK-CHILD",
            "shrink closure child admission",
        )),
    ];
    let after_shrink = replace_and_snapshot(&shrink);
    let shrink_snapshot = after_shrink.clone();
    let capture_shrink = move || shrink_snapshot[0].clone();
    let grown = vec![
        admitted(
            "ORNA-E-CLOSURE-SNAPSHOT-GROWN",
            "grown closure parent admission",
        )
        .with_cause(admitted(
            "ORNA-E-CLOSURE-SNAPSHOT-GROWN-CHILD-A",
            "grown closure child a admission",
        ))
        .with_cause(admitted(
            "ORNA-E-CLOSURE-SNAPSHOT-GROWN-CHILD-B",
            "grown closure child b admission",
        )),
        admitted(
            "ORNA-E-CLOSURE-SNAPSHOT-GROWN-TAIL",
            "grown closure tail admission",
        ),
    ];
    let after_growth = replace_and_snapshot(&grown);
    drop(replace_and_snapshot);

    assert_eq!(destination, grown);
    assert_eq!(
        serde_json::to_value(&before_replacement).unwrap(),
        before_projection
    );
    assert_eq!(
        before_replacement
            .iter()
            .map(|diagnostic| diagnostic.encode_ovb().unwrap())
            .collect::<Vec<_>>(),
        before_wires
    );
    assert_eq!(after_shrink, shrink);
    assert_eq!(after_growth, grown);

    let captured_old = capture_old();
    let captured_shrink = capture_shrink();
    assert_eq!(
        serde_json::to_value(&captured_old).unwrap()["message"],
        "old closure parent admission"
    );
    assert_eq!(
        serde_json::to_value(&captured_shrink).unwrap()["message"],
        "shrink closure parent admission"
    );
    assert_eq!(
        serde_json::to_value(&destination[0]).unwrap()["message"],
        "grown closure parent admission"
    );

    let outer = admitted(
        "ORNA-E-CLOSURE-SNAPSHOT-OUTER",
        "outer closure parent admission",
    )
    .with_cause(captured_old.clone())
    .with_cause(captured_shrink.clone())
    .with_cause(destination[0].clone());
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["message"], "outer closure parent admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 3);
    for (cause, expected_children) in causes.iter().zip([1, 1, 2]) {
        assert_eq!(cause["message"], "<redacted>");
        let children = cause["causes"].as_array().unwrap();
        assert_eq!(children.len(), expected_children);
        for child in children {
            assert_eq!(child["message"], "<redacted>");
        }
    }

    let envelope = serde_json::json!({
        "outer": outer,
        "captured_old": captured_old,
        "captured_shrink": captured_shrink,
        "after_growth": destination,
    });
    let json = serde_json::to_vec(&envelope).unwrap();
    for message in [
        b"outer closure parent admission".as_slice(),
        b"old closure parent admission".as_slice(),
        b"shrink closure parent admission".as_slice(),
        b"grown closure parent admission".as_slice(),
    ] {
        assert_eq!(
            json.windows(message.len())
                .filter(|window| *window == message)
                .count(),
            1
        );
    }
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            b"old closure child admission".as_slice(),
            b"shrink closure child admission".as_slice(),
            b"grown closure child a admission".as_slice(),
            b"grown closure child b admission".as_slice(),
        ])
    {
        assert!(
            !json
                .windows(disclosure.len())
                .any(|window| window == disclosure)
        );
    }

    let wire = outer.encode_ovb().unwrap();
    for disclosure in [
        fixture.as_bytes(),
        b"old closure parent admission".as_slice(),
        b"shrink closure parent admission".as_slice(),
        b"grown closure parent admission".as_slice(),
        b"grown closure child b admission".as_slice(),
    ]
    .into_iter()
    .chain(fixture_credentials.iter().map(|value| value.as_bytes()))
    {
        assert!(
            !wire
                .windows(disclosure.len())
                .any(|window| window == disclosure)
        );
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&wire).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    assert_eq!(
        decoded["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|cause| cause["causes"].as_array().unwrap().len())
            .collect::<Vec<_>>(),
        [1, 1, 2]
    );
}

#[test]
fn closure_replacement_sources_restore_parent_snapshots_after_resize() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let original = vec![
        admitted("ORNA-E-REMAINDER-OLD-A", "remainder old parent a admission")
            .with_cause(admitted(
                "ORNA-E-REMAINDER-OLD-A-CHILD",
                "remainder old child a admission",
            )),
        admitted("ORNA-E-REMAINDER-OLD-B", "remainder old parent b admission")
            .with_cause(admitted(
                "ORNA-E-REMAINDER-OLD-B-CHILD-A",
                "remainder old child b a admission",
            ))
            .with_cause(admitted(
                "ORNA-E-REMAINDER-OLD-B-CHILD-B",
                "remainder old child b b admission",
            )),
    ];
    let mut destination = original.clone();
    let before_replacement = destination.clone();
    let before_projection = serde_json::to_value(&before_replacement).unwrap();
    let before_wires = before_replacement
        .iter()
        .map(|diagnostic| diagnostic.encode_ovb().unwrap())
        .collect::<Vec<_>>();

    // Orna specifies immutable lexical closure captures, while host
    // clone_from details are unspecified. Model closure sources as owned
    // snapshots and check that destination replacement leaves them intact.
    let captured_original = before_replacement.clone();
    let restore_original = move || captured_original.clone();
    let shrink = vec![
        admitted("ORNA-E-REMAINDER-SHRINK", "remainder shrink parent admission")
            .with_cause(admitted(
                "ORNA-E-REMAINDER-SHRINK-CHILD",
                "remainder shrink child admission",
            )),
    ];
    destination.clone_from(&shrink);
    let after_shrink = destination.clone();
    let shrink_projection = serde_json::to_value(&after_shrink).unwrap();
    let shrink_wires = after_shrink
        .iter()
        .map(|diagnostic| diagnostic.encode_ovb().unwrap())
        .collect::<Vec<_>>();

    let grown = vec![
        admitted("ORNA-E-REMAINDER-GROWN-A", "remainder grown parent a admission")
            .with_cause(admitted(
                "ORNA-E-REMAINDER-GROWN-A-CHILD-A",
                "remainder grown child a a admission",
            ))
            .with_cause(admitted(
                "ORNA-E-REMAINDER-GROWN-A-CHILD-B",
                "remainder grown child a b admission",
            )),
        admitted("ORNA-E-REMAINDER-GROWN-B", "remainder grown parent b admission")
            .with_cause(admitted(
                "ORNA-E-REMAINDER-GROWN-B-CHILD",
                "remainder grown child b admission",
            )),
        admitted("ORNA-E-REMAINDER-GROWN-TAIL", "remainder grown tail admission"),
    ];
    let captured_growth = grown.clone();
    let grow_from_closure = move || captured_growth.clone();
    destination.clone_from(&grow_from_closure());
    let after_growth = destination.clone();
    let growth_projection = serde_json::to_value(&after_growth).unwrap();
    let growth_wires = after_growth
        .iter()
        .map(|diagnostic| diagnostic.encode_ovb().unwrap())
        .collect::<Vec<_>>();

    // The old closure is now a replacement source after both a shrink and a
    // growth have reused the destination's parent vector slots.
    destination.clone_from(&restore_original());
    let after_restore = destination.clone();
    assert_eq!(destination, original);
    assert_eq!(
        serde_json::to_value(&before_replacement).unwrap(),
        before_projection
    );
    assert_eq!(
        before_replacement
            .iter()
            .map(|diagnostic| diagnostic.encode_ovb().unwrap())
            .collect::<Vec<_>>(),
        before_wires
    );
    assert_eq!(serde_json::to_value(&after_shrink).unwrap(), shrink_projection);
    assert_eq!(
        after_shrink
            .iter()
            .map(|diagnostic| diagnostic.encode_ovb().unwrap())
            .collect::<Vec<_>>(),
        shrink_wires
    );
    assert_eq!(serde_json::to_value(&after_growth).unwrap(), growth_projection);
    assert_eq!(
        after_growth
            .iter()
            .map(|diagnostic| diagnostic.encode_ovb().unwrap())
            .collect::<Vec<_>>(),
        growth_wires
    );
    assert_eq!(after_restore, before_replacement);

    let compose_captured_replacements = {
        let before = before_replacement.clone();
        let shrunk = after_shrink.clone();
        let grown = after_growth.clone();
        let restored = after_restore.clone();
        move || {
            let mut outer = admitted(
                "ORNA-E-REMAINDER-OUTER",
                "remainder outer closure admission",
            );
            for cause in [
                before[0].clone(),
                before[1].clone(),
                shrunk[0].clone(),
                grown[0].clone(),
                restored[0].clone(),
            ] {
                outer = outer.with_cause(cause);
            }
            outer
        }
    };
    let empty: Vec<Diagnostic> = Vec::new();
    destination.clone_from(&empty);
    assert!(destination.is_empty());

    // Composition still sees the captured roots after the live parent has
    // been cleared, and repeated calls do not consume their admissions.
    let outer = compose_captured_replacements();
    let outer_projection = serde_json::to_value(&outer).unwrap();
    let outer_again = compose_captured_replacements();
    assert_eq!(serde_json::to_value(&outer_again).unwrap(), outer_projection);
    assert_eq!(outer_projection["message"], "remainder outer closure admission");
    let causes = outer_projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 5);
    for (cause, expected_children) in causes.iter().zip([1, 2, 1, 2, 1]) {
        assert_eq!(cause["message"], "<redacted>");
        let children = cause["causes"].as_array().unwrap();
        assert_eq!(children.len(), expected_children);
        for child in children {
            assert_eq!(child["message"], "<redacted>");
        }
    }

    let envelope = serde_json::json!({
        "outer": outer.clone(),
        "before_replacement": before_replacement,
        "after_shrink": after_shrink,
        "after_growth": after_growth,
    });
    let json = serde_json::to_vec(&envelope).unwrap();
    for message in [
        b"remainder outer closure admission".as_slice(),
        b"remainder old parent a admission".as_slice(),
        b"remainder old parent b admission".as_slice(),
        b"remainder shrink parent admission".as_slice(),
        b"remainder grown parent a admission".as_slice(),
        b"remainder grown parent b admission".as_slice(),
        b"remainder grown tail admission".as_slice(),
    ] {
        assert_eq!(
            json.windows(message.len())
                .filter(|window| *window == message)
                .count(),
            1
        );
    }
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            b"remainder old child a admission".as_slice(),
            b"remainder old child b a admission".as_slice(),
            b"remainder old child b b admission".as_slice(),
            b"remainder shrink child admission".as_slice(),
            b"remainder grown child a a admission".as_slice(),
            b"remainder grown child a b admission".as_slice(),
            b"remainder grown child b admission".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
    }

    let wire = outer.encode_ovb().unwrap();
    for disclosure in [
        fixture.as_bytes(),
        b"remainder old parent a admission".as_slice(),
        b"remainder old parent b admission".as_slice(),
        b"remainder shrink parent admission".as_slice(),
        b"remainder grown parent a admission".as_slice(),
        b"remainder grown parent b admission".as_slice(),
        b"remainder grown tail admission".as_slice(),
        b"remainder grown child b admission".as_slice(),
    ]
    .into_iter()
    .chain(fixture_credentials.iter().map(|value| value.as_bytes()))
    {
        assert!(!wire.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&wire).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    assert_eq!(
        decoded["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|cause| cause["causes"].as_array().unwrap().len())
            .collect::<Vec<_>>(),
        [1, 2, 1, 2, 1]
    );
}

#[test]
fn same_length_closure_sources_keep_replaced_parent_snapshots_local() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let original = vec![
        admitted(
            "ORNA-E-REMAINDER-SAME-OLD-A",
            "same length old parent a admission",
        )
        .with_cause(admitted(
            "ORNA-E-REMAINDER-SAME-OLD-A-CHILD",
            "same length old child a admission",
        )),
        admitted(
            "ORNA-E-REMAINDER-SAME-OLD-B",
            "same length old parent b admission",
        )
        .with_cause(admitted(
            "ORNA-E-REMAINDER-SAME-OLD-B-CHILD-A",
            "same length old child b a admission",
        ))
        .with_cause(admitted(
            "ORNA-E-REMAINDER-SAME-OLD-B-CHILD-B",
            "same length old child b b admission",
        )),
    ];
    let replacement = vec![
        admitted(
            "ORNA-E-REMAINDER-SAME-NEW-A",
            "same length replacement a admission",
        )
        .with_cause(admitted(
            "ORNA-E-REMAINDER-SAME-NEW-A-CHILD-A",
            "same length replacement a child a admission",
        ))
        .with_cause(admitted(
            "ORNA-E-REMAINDER-SAME-NEW-A-CHILD-B",
            "same length replacement a child b admission",
        )),
        admitted(
            "ORNA-E-REMAINDER-SAME-NEW-B",
            "same length replacement b admission",
        )
        .with_cause(admitted(
            "ORNA-E-REMAINDER-SAME-NEW-B-CHILD",
            "same length replacement b child admission",
        )),
    ];
    let mut destination = original.clone();
    let before_replacement = destination.clone();
    let before_projection = serde_json::to_value(&before_replacement).unwrap();
    let before_wires = before_replacement
        .iter()
        .map(|diagnostic| diagnostic.encode_ovb().unwrap())
        .collect::<Vec<_>>();

    // The reference defines immutable lexical closure captures, but does not
    // specify host Vec::clone_from slot reuse. Treat captured roots as owned
    // snapshots and prove same-length replacements retain local admission.
    let original_snapshot = before_replacement.clone();
    let replacement_snapshot = replacement.clone();
    let capture_original = move || original_snapshot.clone();
    let capture_replacement = move || replacement_snapshot.clone();
    destination.clone_from(&capture_replacement());
    let after_replacement = destination.clone();
    let replacement_projection = serde_json::to_value(&after_replacement).unwrap();
    let replacement_wires = after_replacement
        .iter()
        .map(|diagnostic| diagnostic.encode_ovb().unwrap())
        .collect::<Vec<_>>();

    destination.clone_from(&capture_original());
    let after_restore = destination.clone();
    destination.clone_from(&capture_replacement());
    let after_second_replacement = destination.clone();
    assert_eq!(after_restore, before_replacement);
    assert_eq!(after_second_replacement, replacement);
    assert_eq!(
        serde_json::to_value(&before_replacement).unwrap(),
        before_projection
    );
    assert_eq!(
        before_replacement
            .iter()
            .map(|diagnostic| diagnostic.encode_ovb().unwrap())
            .collect::<Vec<_>>(),
        before_wires
    );
    assert_eq!(
        serde_json::to_value(&after_replacement).unwrap(),
        replacement_projection
    );
    assert_eq!(
        after_replacement
            .iter()
            .map(|diagnostic| diagnostic.encode_ovb().unwrap())
            .collect::<Vec<_>>(),
        replacement_wires
    );

    let compose_captured_slots = {
        let old = before_replacement.clone();
        let new = after_replacement.clone();
        let restored = after_restore.clone();
        let replaced_again = after_second_replacement.clone();
        move || {
            let mut outer = admitted(
                "ORNA-E-REMAINDER-SAME-OUTER",
                "same length outer closure admission",
            );
            for cause in [
                old[0].clone(),
                old[1].clone(),
                new[0].clone(),
                restored[0].clone(),
                replaced_again[0].clone(),
            ] {
                outer = outer.with_cause(cause);
            }
            outer
        }
    };
    let empty: Vec<Diagnostic> = Vec::new();
    destination.clone_from(&empty);
    assert!(destination.is_empty());

    let outer = compose_captured_slots();
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["message"], "same length outer closure admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 5);
    for (cause, expected_children) in causes.iter().zip([1, 2, 2, 1, 2]) {
        assert_eq!(cause["message"], "<redacted>");
        let children = cause["causes"].as_array().unwrap();
        assert_eq!(children.len(), expected_children);
        for child in children {
            assert_eq!(child["message"], "<redacted>");
        }
    }

    let envelope = serde_json::json!({
        "outer": outer.clone(),
        "before_replacement": before_replacement,
        "after_replacement": after_replacement,
    });
    let json = serde_json::to_vec(&envelope).unwrap();
    for message in [
        b"same length outer closure admission".as_slice(),
        b"same length old parent a admission".as_slice(),
        b"same length old parent b admission".as_slice(),
        b"same length replacement a admission".as_slice(),
        b"same length replacement b admission".as_slice(),
    ] {
        assert_eq!(
            json.windows(message.len())
                .filter(|window| *window == message)
                .count(),
            1
        );
    }
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            b"same length old child a admission".as_slice(),
            b"same length old child b a admission".as_slice(),
            b"same length old child b b admission".as_slice(),
            b"same length replacement a child a admission".as_slice(),
            b"same length replacement a child b admission".as_slice(),
            b"same length replacement b child admission".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
    }

    let wire = outer.encode_ovb().unwrap();
    assert!(
        wire.windows(b"same length outer closure admission".len())
            .any(|window| window == b"same length outer closure admission")
    );
    for disclosure in [
        fixture.as_bytes(),
        b"same length old parent a admission".as_slice(),
        b"same length old parent b admission".as_slice(),
        b"same length replacement a admission".as_slice(),
        b"same length replacement b admission".as_slice(),
        b"same length replacement b child admission".as_slice(),
    ]
    .into_iter()
    .chain(fixture_credentials.iter().map(|value| value.as_bytes()))
    {
        assert!(!wire.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&wire).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    assert_eq!(
        decoded["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|cause| cause["causes"].as_array().unwrap().len())
            .collect::<Vec<_>>(),
        [1, 2, 2, 1, 2]
    );
}

#[test]
fn same_length_closure_replacement_keeps_mixed_slot_admission_local() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let untrusted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(message).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let original = vec![
        admitted(
            "ORNA-E-MIXED-OLD-ADMITTED",
            "mixed old admitted parent admission",
        )
        .with_cause(untrusted(
            "ORNA-E-MIXED-OLD-CHILD",
            "mixed old child admission",
        )),
        untrusted(
            "ORNA-E-MIXED-OLD-UNTRUSTED",
            "mixed old untrusted parent admission",
        )
        .with_cause(admitted(
            "ORNA-E-MIXED-OLD-UNTRUSTED-CHILD",
            "mixed old untrusted child admission",
        )),
    ];
    let replacement = vec![
        untrusted(
            "ORNA-E-MIXED-NEW-UNTRUSTED",
            "mixed new untrusted parent admission",
        )
        .with_cause(admitted(
            "ORNA-E-MIXED-NEW-UNTRUSTED-CHILD",
            "mixed new untrusted child admission",
        )),
        admitted(
            "ORNA-E-MIXED-NEW-ADMITTED",
            "mixed new admitted parent admission",
        )
        .with_cause(untrusted(
            "ORNA-E-MIXED-NEW-CHILD",
            "mixed new child admission",
        )),
    ];
    let mut destination = original.clone();
    let before_replacement = destination.clone();
    let before_projection = serde_json::to_value(&before_replacement).unwrap();
    let before_wires = before_replacement
        .iter()
        .map(|diagnostic| diagnostic.encode_ovb().unwrap())
        .collect::<Vec<_>>();

    // The reference defines immutable closure captures and diagnostic
    // redaction, but leaves host same-length Vec::clone_from reuse open. Keep
    // each source as an owned snapshot so admission stays local to its slot.
    let old_snapshot = before_replacement.clone();
    let replacement_snapshot = replacement.clone();
    let capture_old = move || old_snapshot.clone();
    let capture_replacement = move || replacement_snapshot.clone();
    destination.clone_from(&capture_replacement());
    let after_replacement = destination.clone();
    let replacement_projection = serde_json::to_value(&after_replacement).unwrap();
    let replacement_wires = after_replacement
        .iter()
        .map(|diagnostic| diagnostic.encode_ovb().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(
        before_projection[0]["message"],
        "mixed old admitted parent admission"
    );
    assert_eq!(before_projection[1]["message"], "<redacted>");
    assert_eq!(replacement_projection[0]["message"], "<redacted>");
    assert_eq!(
        replacement_projection[1]["message"],
        "mixed new admitted parent admission"
    );
    destination.clone_from(&capture_old());
    let after_restore = destination.clone();
    destination.clone_from(&capture_replacement());
    let after_second_replacement = destination.clone();
    assert_eq!(after_restore, before_replacement);
    assert_eq!(after_second_replacement, replacement);
    assert_eq!(
        serde_json::to_value(&before_replacement).unwrap(),
        before_projection
    );
    assert_eq!(
        before_replacement
            .iter()
            .map(|diagnostic| diagnostic.encode_ovb().unwrap())
            .collect::<Vec<_>>(),
        before_wires
    );
    assert_eq!(
        serde_json::to_value(&after_replacement).unwrap(),
        replacement_projection
    );
    assert_eq!(
        after_replacement
            .iter()
            .map(|diagnostic| diagnostic.encode_ovb().unwrap())
            .collect::<Vec<_>>(),
        replacement_wires
    );

    let compose_captured_slots = {
        let old = before_replacement.clone();
        let new = after_replacement.clone();
        let restored = after_restore.clone();
        let replaced_again = after_second_replacement.clone();
        move || {
            let mut outer = admitted(
                "ORNA-E-MIXED-OUTER",
                "mixed slot outer closure admission",
            );
            for cause in [
                old[0].clone(),
                old[1].clone(),
                new[0].clone(),
                new[1].clone(),
                restored[0].clone(),
                replaced_again[1].clone(),
            ] {
                outer = outer.with_cause(cause);
            }
            outer
        }
    };
    let empty: Vec<Diagnostic> = Vec::new();
    destination.clone_from(&empty);
    assert!(destination.is_empty());

    let outer = compose_captured_slots();
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["message"], "mixed slot outer closure admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 6);
    for cause in causes {
        assert_eq!(cause["message"], "<redacted>");
        assert_eq!(cause["causes"].as_array().unwrap().len(), 1);
        assert_eq!(cause["causes"][0]["message"], "<redacted>");
    }

    let envelope = serde_json::json!({
        "outer": outer.clone(),
        "before_replacement": before_replacement,
        "after_replacement": after_replacement,
    });
    let json = serde_json::to_vec(&envelope).unwrap();
    for message in [
        b"mixed slot outer closure admission".as_slice(),
        b"mixed old admitted parent admission".as_slice(),
        b"mixed new admitted parent admission".as_slice(),
    ] {
        assert_eq!(
            json.windows(message.len())
                .filter(|window| *window == message)
                .count(),
            1
        );
    }
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            b"mixed old untrusted parent admission".as_slice(),
            b"mixed new untrusted parent admission".as_slice(),
            b"mixed old child admission".as_slice(),
            b"mixed old untrusted child admission".as_slice(),
            b"mixed new untrusted child admission".as_slice(),
            b"mixed new child admission".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
    }

    let wire = outer.encode_ovb().unwrap();
    assert!(
        wire.windows(b"mixed slot outer closure admission".len())
            .any(|window| window == b"mixed slot outer closure admission")
    );
    for disclosure in [
        fixture.as_bytes(),
        b"mixed old admitted parent admission".as_slice(),
        b"mixed new admitted parent admission".as_slice(),
        b"mixed old untrusted parent admission".as_slice(),
        b"mixed new untrusted parent admission".as_slice(),
        b"mixed new child admission".as_slice(),
    ]
    .into_iter()
    .chain(fixture_credentials.iter().map(|value| value.as_bytes()))
    {
        assert!(!wire.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&wire).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    assert_eq!(
        decoded["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|cause| cause["causes"].as_array().unwrap().len())
            .collect::<Vec<_>>(),
        [1, 1, 1, 1, 1, 1]
    );
}

#[test]
fn mixed_trust_closure_sources_keep_sibling_replacements_independent() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let untrusted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(message).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let original = vec![
        admitted("ORNA-E-FORK-OLD-ADMITTED", "fork old admitted parent")
            .with_cause(untrusted("ORNA-E-FORK-OLD-CHILD", "fork old child secret")),
        untrusted("ORNA-E-FORK-OLD-RAW", "fork old raw parent secret")
            .with_cause(admitted("ORNA-E-FORK-OLD-RAW-CHILD", "fork old raw child secret")),
    ];
    let replacement = vec![
        untrusted("ORNA-E-FORK-NEW-RAW", "fork new raw parent secret")
            .with_cause(admitted("ORNA-E-FORK-NEW-RAW-CHILD", "fork new raw child secret")),
        admitted("ORNA-E-FORK-NEW-ADMITTED", "fork new admitted parent")
            .with_cause(untrusted("ORNA-E-FORK-NEW-CHILD", "fork new child secret")),
    ];
    let before_projection = serde_json::to_value(&original).unwrap();
    let before_wires = original
        .iter()
        .map(|diagnostic| diagnostic.encode_ovb().unwrap())
        .collect::<Vec<_>>();
    let replacement_projection = serde_json::to_value(&replacement).unwrap();
    let replacement_wires = replacement
        .iter()
        .map(|diagnostic| diagnostic.encode_ovb().unwrap())
        .collect::<Vec<_>>();

    // Orna specifies immutable closure captures and diagnostic redaction but
    // leaves host clone_from behavior open. Keep source vectors as snapshots;
    // replacements in one sibling destination must not transfer trust marks
    // to the other destination's reused slots.
    let old_source_snapshot = original.clone();
    let new_source_snapshot = replacement.clone();
    let old_source = move || old_source_snapshot.clone();
    let new_source = move || new_source_snapshot.clone();
    let mut left = original.clone();
    let mut right = original.clone();

    left.clone_from(&new_source());
    let left_new = left.clone();
    right.clone_from(&old_source());
    let right_old = right.clone();
    right.clone_from(&new_source());
    let right_new = right.clone();
    left.clone_from(&old_source());
    let left_old = left.clone();

    assert_eq!(left_new, replacement);
    assert_eq!(right_old, original);
    assert_eq!(right_new, replacement);
    assert_eq!(left_old, original);
    assert_eq!(serde_json::to_value(&original).unwrap(), before_projection);
    assert_eq!(
        original
            .iter()
            .map(|diagnostic| diagnostic.encode_ovb().unwrap())
            .collect::<Vec<_>>(),
        before_wires
    );
    assert_eq!(serde_json::to_value(&replacement).unwrap(), replacement_projection);
    assert_eq!(
        replacement
            .iter()
            .map(|diagnostic| diagnostic.encode_ovb().unwrap())
            .collect::<Vec<_>>(),
        replacement_wires
    );

    let compose_sibling_snapshots = {
        let left_replaced = left_new.clone();
        let right_restored = right_old.clone();
        let left_restored = left_old.clone();
        let right_replaced = right_new.clone();
        move || {
            let mut outer = admitted("ORNA-E-FORK-OUTER", "fork outer closure admission");
            for cause in [
                left_replaced[0].clone(),
                right_restored[1].clone(),
                left_restored[0].clone(),
                right_replaced[1].clone(),
            ] {
                outer = outer.with_cause(cause);
            }
            outer
        }
    };
    let empty: Vec<Diagnostic> = Vec::new();
    left.clone_from(&empty);
    right.clone_from(&empty);
    assert!(left.is_empty() && right.is_empty());

    let outer = compose_sibling_snapshots();
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["message"], "fork outer closure admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 4);
    for cause in causes {
        assert_eq!(cause["message"], "<redacted>");
        assert_eq!(cause["causes"].as_array().unwrap().len(), 1);
        assert_eq!(cause["causes"][0]["message"], "<redacted>");
    }

    let envelope = serde_json::json!({
        "outer": outer.clone(),
        "left_restored": left_old,
        "right_replaced": right_new,
    });
    let json = serde_json::to_vec(&envelope).unwrap();
    for message in [
        b"fork outer closure admission".as_slice(),
        b"fork old admitted parent".as_slice(),
        b"fork new admitted parent".as_slice(),
    ] {
        assert_eq!(
            json.windows(message.len())
                .filter(|window| *window == message)
                .count(),
            1
        );
    }
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            b"fork old raw parent secret".as_slice(),
            b"fork new raw parent secret".as_slice(),
            b"fork old child secret".as_slice(),
            b"fork old raw child secret".as_slice(),
            b"fork new raw child secret".as_slice(),
            b"fork new child secret".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
    }

    let wire = outer.encode_ovb().unwrap();
    assert!(
        wire.windows(b"fork outer closure admission".len())
            .any(|window| window == b"fork outer closure admission")
    );
    for disclosure in [
        fixture.as_bytes(),
        b"fork old admitted parent".as_slice(),
        b"fork new admitted parent".as_slice(),
        b"fork old raw parent secret".as_slice(),
        b"fork new raw parent secret".as_slice(),
        b"fork new child secret".as_slice(),
    ]
    .into_iter()
    .chain(fixture_credentials.iter().map(|value| value.as_bytes()))
    {
        assert!(!wire.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&wire).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    assert_eq!(
        decoded["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|cause| cause["causes"].as_array().unwrap().len())
            .collect::<Vec<_>>(),
        [1, 1, 1, 1]
    );
}

#[test]
fn mixed_trust_sibling_closures_cross_restore_captured_generations() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let untrusted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(message).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let original = vec![
        admitted("ORNA-E-CROSS-OLD-ADMITTED", "cross old admitted parent")
            .with_cause(untrusted("ORNA-E-CROSS-OLD-CHILD", "cross old child secret")),
        untrusted("ORNA-E-CROSS-OLD-RAW", "cross old raw parent secret")
            .with_cause(admitted("ORNA-E-CROSS-OLD-RAW-CHILD", "cross old raw child secret")),
    ];
    let replacement = vec![
        untrusted("ORNA-E-CROSS-NEW-RAW", "cross new raw parent secret")
            .with_cause(admitted("ORNA-E-CROSS-NEW-RAW-CHILD", "cross new raw child secret")),
        admitted("ORNA-E-CROSS-NEW-ADMITTED", "cross new admitted parent")
            .with_cause(untrusted("ORNA-E-CROSS-NEW-CHILD", "cross new child secret")),
    ];
    let old_projection = serde_json::to_value(&original).unwrap();
    let old_wires = original
        .iter()
        .map(|diagnostic| diagnostic.encode_ovb().unwrap())
        .collect::<Vec<_>>();
    let new_projection = serde_json::to_value(&replacement).unwrap();
    let new_wires = replacement
        .iter()
        .map(|diagnostic| diagnostic.encode_ovb().unwrap())
        .collect::<Vec<_>>();

    // The Orna reference specifies immutable closure captures and diagnostic
    // redaction, but leaves host clone_from slot reuse unspecified. Captured
    // sibling generations therefore act as owned, immutable replacement sources.
    let mut left = original.clone();
    let mut right = replacement.clone();
    let left_source_snapshot = left.clone();
    let right_source_snapshot = right.clone();
    let capture_left = move || left_source_snapshot.clone();
    let capture_right = move || right_source_snapshot.clone();

    left.clone_from(&capture_right());
    right.clone_from(&capture_left());
    let left_crossed = left.clone();
    let right_crossed = right.clone();
    assert_eq!(left_crossed, replacement);
    assert_eq!(right_crossed, original);

    let left_crossed_source_snapshot = left_crossed.clone();
    let right_crossed_source_snapshot = right_crossed.clone();
    let capture_left_crossed = move || left_crossed_source_snapshot.clone();
    let capture_right_crossed = move || right_crossed_source_snapshot.clone();
    left.clone_from(&capture_right_crossed());
    right.clone_from(&capture_left_crossed());
    let left_restored = left.clone();
    let right_restored = right.clone();
    assert_eq!(left_restored, original);
    assert_eq!(right_restored, replacement);

    assert_eq!(serde_json::to_value(&original).unwrap(), old_projection);
    assert_eq!(
        original
            .iter()
            .map(|diagnostic| diagnostic.encode_ovb().unwrap())
            .collect::<Vec<_>>(),
        old_wires
    );
    assert_eq!(serde_json::to_value(&replacement).unwrap(), new_projection);
    assert_eq!(
        replacement
            .iter()
            .map(|diagnostic| diagnostic.encode_ovb().unwrap())
            .collect::<Vec<_>>(),
        new_wires
    );

    let compose_crossed_generations = {
        let first_left = left_crossed.clone();
        let first_right = right_crossed.clone();
        let restored_left = left_restored.clone();
        let restored_right = right_restored.clone();
        move || {
            let mut outer = admitted("ORNA-E-CROSS-OUTER", "cross outer closure admission");
            for cause in [
                first_left[0].clone(),
                first_right[1].clone(),
                restored_left[0].clone(),
                restored_right[1].clone(),
            ] {
                outer = outer.with_cause(cause);
            }
            outer
        }
    };
    let empty: Vec<Diagnostic> = Vec::new();
    left.clone_from(&empty);
    right.clone_from(&empty);
    assert!(left.is_empty() && right.is_empty());

    let outer = compose_crossed_generations();
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["message"], "cross outer closure admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 4);
    for cause in causes {
        assert_eq!(cause["message"], "<redacted>");
        assert_eq!(cause["causes"].as_array().unwrap().len(), 1);
        assert_eq!(cause["causes"][0]["message"], "<redacted>");
    }

    let envelope = serde_json::json!({
        "outer": outer.clone(),
        "left_restored": left_restored,
        "right_restored": right_restored,
    });
    let json = serde_json::to_vec(&envelope).unwrap();
    for message in [
        b"cross outer closure admission".as_slice(),
        b"cross old admitted parent".as_slice(),
        b"cross new admitted parent".as_slice(),
    ] {
        assert_eq!(
            json.windows(message.len())
                .filter(|window| *window == message)
                .count(),
            1
        );
    }
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            b"cross old raw parent secret".as_slice(),
            b"cross new raw parent secret".as_slice(),
            b"cross old child secret".as_slice(),
            b"cross old raw child secret".as_slice(),
            b"cross new raw child secret".as_slice(),
            b"cross new child secret".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
    }

    let wire = outer.encode_ovb().unwrap();
    assert!(
        wire.windows(b"cross outer closure admission".len())
            .any(|window| window == b"cross outer closure admission")
    );
    for disclosure in [
        fixture.as_bytes(),
        b"cross old admitted parent".as_slice(),
        b"cross new admitted parent".as_slice(),
        b"cross old raw parent secret".as_slice(),
        b"cross new raw parent secret".as_slice(),
        b"cross new child secret".as_slice(),
    ]
    .into_iter()
    .chain(fixture_credentials.iter().map(|value| value.as_bytes()))
    {
        assert!(!wire.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&wire).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    assert_eq!(
        decoded["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|cause| cause["causes"].as_array().unwrap().len())
            .collect::<Vec<_>>(),
        [1, 1, 1, 1]
    );
}

#[test]
fn mixed_trust_sibling_closures_restore_resized_generations() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let untrusted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(message).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let original = vec![
        admitted("ORNA-E-GEN-BASE-ADMITTED", "generation base admission")
            .with_cause(untrusted("ORNA-E-GEN-BASE-CHILD", "generation base child secret")),
        untrusted("ORNA-E-GEN-BASE-RAW", "generation base parent secret")
            .with_cause(admitted("ORNA-E-GEN-BASE-RAW-CHILD", "generation base raw child")),
    ];
    let mut contracted = vec![
        admitted("ORNA-E-GEN-SMALL-ADMITTED", "generation small admission")
            .with_cause(untrusted("ORNA-E-GEN-SMALL-CHILD", "generation small child secret")),
    ];
    let mut expanded = vec![
        untrusted("ORNA-E-GEN-WIDE-RAW-0", "generation wide raw parent secret")
            .with_cause(admitted("ORNA-E-GEN-WIDE-RAW-CHILD-0", "generation wide raw child")),
        admitted("ORNA-E-GEN-WIDE-ADMITTED", "generation wide admission")
            .with_cause(untrusted("ORNA-E-GEN-WIDE-CHILD", "generation wide child secret")),
        untrusted("ORNA-E-GEN-WIDE-RAW-2", "generation wide tail secret")
            .with_cause(admitted("ORNA-E-GEN-WIDE-RAW-CHILD-2", "generation wide tail child")),
    ];
    let original_snapshot = original.clone();
    let contracted_snapshot = contracted.clone();
    let expanded_snapshot = expanded.clone();
    let original_projection = serde_json::to_value(&original_snapshot).unwrap();
    let original_wires = original_snapshot
        .iter()
        .map(|diagnostic| diagnostic.encode_ovb().unwrap())
        .collect::<Vec<_>>();
    let contracted_projection = serde_json::to_value(&contracted_snapshot).unwrap();
    let contracted_wires = contracted_snapshot
        .iter()
        .map(|diagnostic| diagnostic.encode_ovb().unwrap())
        .collect::<Vec<_>>();
    let expanded_projection = serde_json::to_value(&expanded_snapshot).unwrap();
    let expanded_wires = expanded_snapshot
        .iter()
        .map(|diagnostic| diagnostic.encode_ovb().unwrap())
        .collect::<Vec<_>>();

    // The reference specifies immutable closure captures and diagnostic
    // redaction, but leaves host Vec::clone_from resizing and slot reuse open.
    // Retain each sibling's captured generation as an owned snapshot across
    // both shrink and growth so trust state cannot migrate between siblings.
    let capture_contracted = {
        let snapshot = contracted_snapshot.clone();
        move || snapshot.clone()
    };
    let capture_expanded = {
        let snapshot = expanded_snapshot.clone();
        move || snapshot.clone()
    };
    let empty: Vec<Diagnostic> = Vec::new();
    contracted.clone_from(&empty);
    expanded.clone_from(&empty);
    assert!(contracted.is_empty() && expanded.is_empty());

    let mut left = original.clone();
    let mut right = original.clone();
    left.clone_from(&capture_contracted());
    right.clone_from(&capture_expanded());
    let left_contracted = left.clone();
    let right_expanded = right.clone();
    assert_eq!(left_contracted, contracted_snapshot);
    assert_eq!(right_expanded, expanded_snapshot);

    let restore_left_contracted = {
        let snapshot = left_contracted.clone();
        move || snapshot.clone()
    };
    let restore_right_expanded = {
        let snapshot = right_expanded.clone();
        move || snapshot.clone()
    };
    left.clone_from(&capture_expanded());
    right.clone_from(&capture_contracted());
    let left_expanded = left.clone();
    let right_contracted = right.clone();
    assert_eq!(left_expanded, expanded_snapshot);
    assert_eq!(right_contracted, contracted_snapshot);

    left.clone_from(&restore_left_contracted());
    right.clone_from(&restore_right_expanded());
    assert_eq!(left, contracted_snapshot);
    assert_eq!(right, expanded_snapshot);
    assert_eq!(serde_json::to_value(&original_snapshot).unwrap(), original_projection);
    assert_eq!(
        original_snapshot
            .iter()
            .map(|diagnostic| diagnostic.encode_ovb().unwrap())
            .collect::<Vec<_>>(),
        original_wires
    );
    assert_eq!(
        serde_json::to_value(&contracted_snapshot).unwrap(),
        contracted_projection
    );
    assert_eq!(
        contracted_snapshot
            .iter()
            .map(|diagnostic| diagnostic.encode_ovb().unwrap())
            .collect::<Vec<_>>(),
        contracted_wires
    );
    assert_eq!(serde_json::to_value(&expanded_snapshot).unwrap(), expanded_projection);
    assert_eq!(
        expanded_snapshot
            .iter()
            .map(|diagnostic| diagnostic.encode_ovb().unwrap())
            .collect::<Vec<_>>(),
        expanded_wires
    );

    let compose_sibling_generations = {
        let left_after_growth = left_expanded.clone();
        let right_after_shrink = right_contracted.clone();
        let left_after_restore = left.clone();
        let right_after_restore = right.clone();
        move || {
            let mut outer = admitted("ORNA-E-GEN-OUTER", "generation edge outer admission");
            for cause in [
                left_after_growth[0].clone(),
                right_after_shrink[0].clone(),
                left_after_restore[0].clone(),
                right_after_restore[2].clone(),
            ] {
                outer = outer.with_cause(cause);
            }
            outer
        }
    };
    left.clone_from(&empty);
    right.clone_from(&empty);
    assert!(left.is_empty() && right.is_empty());

    let outer = compose_sibling_generations();
    assert_eq!(outer, compose_sibling_generations());
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["message"], "generation edge outer admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 4);
    for cause in causes {
        assert_eq!(cause["message"], "<redacted>");
        assert_eq!(cause["causes"].as_array().unwrap().len(), 1);
        assert_eq!(cause["causes"][0]["message"], "<redacted>");
    }

    let envelope = serde_json::json!({
        "outer": outer.clone(),
        "left_contracted": left_contracted,
        "right_expanded": right_expanded,
    });
    let json = serde_json::to_vec(&envelope).unwrap();
    for message in [
        b"generation edge outer admission".as_slice(),
        b"generation small admission".as_slice(),
        b"generation wide admission".as_slice(),
    ] {
        assert_eq!(
            json.windows(message.len())
                .filter(|window| *window == message)
                .count(),
            1
        );
    }
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"generation base parent secret".as_slice(),
            b"generation base child secret".as_slice(),
            b"generation small child secret".as_slice(),
            b"generation wide raw parent secret".as_slice(),
            b"generation wide raw child".as_slice(),
            b"generation wide child secret".as_slice(),
            b"generation wide tail secret".as_slice(),
            b"generation wide tail child".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
    }

    let wire = outer.encode_ovb().unwrap();
    assert!(
        wire.windows(b"generation edge outer admission".len())
            .any(|window| window == b"generation edge outer admission")
    );
    for disclosure in [
        fixture.as_bytes(),
        b"generation small admission".as_slice(),
        b"generation wide admission".as_slice(),
        b"generation wide raw parent secret".as_slice(),
        b"generation wide child secret".as_slice(),
        b"generation wide tail secret".as_slice(),
    ]
    .into_iter()
    .chain(fixture_credentials.iter().map(|value| value.as_bytes()))
    {
        assert!(!wire.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&wire).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    assert_eq!(
        decoded["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|cause| cause["causes"].as_array().unwrap().len())
            .collect::<Vec<_>>(),
        [1, 1, 1, 1]
    );
}

#[test]
fn mixed_trust_sibling_parent_closures_restore_resized_cause_generations() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let untrusted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(message).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let old_left = admitted("ORNA-E-PARENT-OLD-LEFT", "parent old left admission")
        .with_cause(untrusted("ORNA-E-PARENT-OLD-L0", "parent old left raw child"))
        .with_cause(admitted("ORNA-E-PARENT-OLD-L1", "parent old left nested admission"))
        .with_cause(untrusted("ORNA-E-PARENT-OLD-L2", "parent old left tail secret"));
    let old_right = untrusted("ORNA-E-PARENT-OLD-RIGHT", "parent old right root secret")
        .with_cause(admitted("ORNA-E-PARENT-OLD-R0", "parent old right child admission"));
    let new_left = untrusted("ORNA-E-PARENT-NEW-LEFT", "parent new left root secret")
        .with_cause(admitted("ORNA-E-PARENT-NEW-L0", "parent new left child admission"));
    let new_right = admitted("ORNA-E-PARENT-NEW-RIGHT", "parent new right admission")
        .with_cause(untrusted("ORNA-E-PARENT-NEW-R0", "parent new right raw child"))
        .with_cause(admitted("ORNA-E-PARENT-NEW-R1", "parent new right nested admission"))
        .with_cause(untrusted("ORNA-E-PARENT-NEW-R2", "parent new right tail secret"));
    let source_snapshots = vec![
        old_left.clone(),
        old_right.clone(),
        new_left.clone(),
        new_right.clone(),
    ];
    let source_projections = source_snapshots
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let source_wires = source_snapshots
        .iter()
        .map(Diagnostic::encode_ovb)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    // The reference specifies immutable closure captures and diagnostic
    // redaction, but leaves host Clone::clone_from reuse of nested cause slots
    // unspecified. Keep each parent generation as an owned snapshot as cause
    // vectors shrink and grow across the two siblings.
    let capture_old_left = {
        let snapshot = source_snapshots[0].clone();
        move || snapshot.clone()
    };
    let capture_old_right = {
        let snapshot = source_snapshots[1].clone();
        move || snapshot.clone()
    };
    let capture_new_left = {
        let snapshot = source_snapshots[2].clone();
        move || snapshot.clone()
    };
    let capture_new_right = {
        let snapshot = source_snapshots[3].clone();
        move || snapshot.clone()
    };
    let mut left = capture_old_left();
    let mut right = capture_old_right();
    left.clone_from(&capture_new_left());
    right.clone_from(&capture_new_right());
    let left_new_generation = left.clone();
    let right_new_generation = right.clone();
    assert_eq!(left_new_generation, new_left);
    assert_eq!(right_new_generation, new_right);

    left.clone_from(&capture_old_right());
    right.clone_from(&capture_old_left());
    let left_crossed_old_generation = left.clone();
    let right_crossed_old_generation = right.clone();
    assert_eq!(left_crossed_old_generation, old_right);
    assert_eq!(right_crossed_old_generation, old_left);

    left.clone_from(&capture_new_left());
    right.clone_from(&capture_new_right());
    assert_eq!(left, new_left);
    assert_eq!(right, new_right);
    for (index, snapshot) in source_snapshots.iter().enumerate() {
        assert_eq!(serde_json::to_value(snapshot).unwrap(), source_projections[index]);
        assert_eq!(snapshot.encode_ovb().unwrap(), source_wires[index]);
    }

    let compose_captured_parent_generations = {
        let left_after_shrink = left_new_generation.clone();
        let right_after_growth = right_new_generation.clone();
        let left_after_cross = left_crossed_old_generation.clone();
        let right_after_cross = right_crossed_old_generation.clone();
        move || {
            admitted("ORNA-E-PARENT-OUTER", "parent generation outer admission")
                .with_cause(left_after_shrink.clone())
                .with_cause(right_after_growth.clone())
                .with_cause(left_after_cross.clone())
                .with_cause(right_after_cross.clone())
        }
    };
    let empty_parent = untrusted("ORNA-E-PARENT-EMPTY", "empty replacement parent secret");
    left.clone_from(&empty_parent);
    right.clone_from(&empty_parent);
    assert_eq!(left, empty_parent);
    assert_eq!(right, empty_parent);

    let outer = compose_captured_parent_generations();
    assert_eq!(outer, compose_captured_parent_generations());
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["message"], "parent generation outer admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 4);
    for (cause, expected_nested_count) in causes.iter().zip([1, 3, 1, 3]) {
        assert_eq!(cause["message"], "<redacted>");
        let nested = cause["causes"].as_array().unwrap();
        assert_eq!(nested.len(), expected_nested_count);
        for child in nested {
            assert_eq!(child["message"], "<redacted>");
        }
    }

    let envelope = serde_json::json!({
        "outer": outer.clone(),
        "old_left": old_left,
        "new_right": new_right,
    });
    let json = serde_json::to_vec(&envelope).unwrap();
    for message in [
        b"parent generation outer admission".as_slice(),
        b"parent old left admission".as_slice(),
        b"parent new right admission".as_slice(),
    ] {
        assert_eq!(
            json.windows(message.len())
                .filter(|window| *window == message)
                .count(),
            1
        );
    }
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"parent old left raw child".as_slice(),
            b"parent old left nested admission".as_slice(),
            b"parent old left tail secret".as_slice(),
            b"parent old right root secret".as_slice(),
            b"parent new left root secret".as_slice(),
            b"parent new left child admission".as_slice(),
            b"parent new right raw child".as_slice(),
            b"parent new right nested admission".as_slice(),
            b"parent new right tail secret".as_slice(),
            b"empty replacement parent secret".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
    }

    let wire = outer.encode_ovb().unwrap();
    assert!(
        wire.windows(b"parent generation outer admission".len())
            .any(|window| window == b"parent generation outer admission")
    );
    for disclosure in [
        fixture.as_bytes(),
        b"parent old left admission".as_slice(),
        b"parent new right admission".as_slice(),
        b"parent old right root secret".as_slice(),
        b"parent new left root secret".as_slice(),
        b"parent new right tail secret".as_slice(),
    ]
    .into_iter()
    .chain(fixture_credentials.iter().map(|value| value.as_bytes()))
    {
        assert!(!wire.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&wire).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    assert_eq!(
        decoded["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|cause| cause["causes"].as_array().unwrap().len())
            .collect::<Vec<_>>(),
        [1, 3, 1, 3]
    );
}

#[test]
fn mixed_trust_sibling_closures_restore_nested_cause_generations() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let untrusted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(message).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let old_left = admitted("ORNA-E-NEST-OLD-LEFT", "nested old left admission").with_cause(
        untrusted("ORNA-E-NEST-OLD-LEFT-CAUSE", "nested old left cause secret")
            .with_cause(admitted("ORNA-E-NEST-OLD-LEFT-LEAF", "nested old left leaf")),
    );
    let old_right = untrusted("ORNA-E-NEST-OLD-RIGHT", "nested old right root secret").with_cause(
        admitted("ORNA-E-NEST-OLD-RIGHT-CAUSE", "nested old right cause admission")
            .with_cause(untrusted("ORNA-E-NEST-OLD-R0", "nested old right leaf zero"))
            .with_cause(admitted("ORNA-E-NEST-OLD-R1", "nested old right leaf one"))
            .with_cause(untrusted("ORNA-E-NEST-OLD-R2", "nested old right leaf two secret")),
    );
    let new_left = untrusted("ORNA-E-NEST-NEW-LEFT", "nested new left root secret").with_cause(
        admitted("ORNA-E-NEST-NEW-LEFT-CAUSE", "nested new left cause admission")
            .with_cause(untrusted("ORNA-E-NEST-NEW-L0", "nested new left leaf zero secret"))
            .with_cause(admitted("ORNA-E-NEST-NEW-L1", "nested new left leaf one"))
            .with_cause(untrusted("ORNA-E-NEST-NEW-L2", "nested new left leaf two secret")),
    );
    let new_right = admitted("ORNA-E-NEST-NEW-RIGHT", "nested new right admission").with_cause(
        untrusted("ORNA-E-NEST-NEW-RIGHT-CAUSE", "nested new right cause secret")
            .with_cause(admitted("ORNA-E-NEST-NEW-RIGHT-LEAF", "nested new right leaf")),
    );
    let source_snapshots = vec![
        old_left.clone(),
        old_right.clone(),
        new_left.clone(),
        new_right.clone(),
    ];
    let source_projections = source_snapshots
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let source_wires = source_snapshots
        .iter()
        .map(Diagnostic::encode_ovb)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    // The reference specifies immutable closure captures and diagnostic
    // redaction, but is silent on host Clone::clone_from reuse at nested cause
    // depths. Preserve each sibling's captured tree as an owned generation.
    let capture_old_left = {
        let snapshot = source_snapshots[0].clone();
        move || snapshot.clone()
    };
    let capture_old_right = {
        let snapshot = source_snapshots[1].clone();
        move || snapshot.clone()
    };
    let capture_new_left = {
        let snapshot = source_snapshots[2].clone();
        move || snapshot.clone()
    };
    let capture_new_right = {
        let snapshot = source_snapshots[3].clone();
        move || snapshot.clone()
    };
    let mut left = capture_old_left();
    let mut right = capture_old_right();
    left.clone_from(&capture_new_left());
    right.clone_from(&capture_new_right());
    let left_new_generation = left.clone();
    let right_new_generation = right.clone();
    assert_eq!(left_new_generation, new_left);
    assert_eq!(right_new_generation, new_right);

    left.clone_from(&capture_old_right());
    right.clone_from(&capture_old_left());
    let left_crossed_generation = left.clone();
    let right_crossed_generation = right.clone();
    assert_eq!(left_crossed_generation, old_right);
    assert_eq!(right_crossed_generation, old_left);

    left.clone_from(&capture_new_left());
    right.clone_from(&capture_new_right());
    assert_eq!(left, new_left);
    assert_eq!(right, new_right);
    for (index, snapshot) in source_snapshots.iter().enumerate() {
        assert_eq!(serde_json::to_value(snapshot).unwrap(), source_projections[index]);
        assert_eq!(snapshot.encode_ovb().unwrap(), source_wires[index]);
    }

    let compose_captured_nested_generations = {
        let left_after_growth = left_new_generation.clone();
        let right_after_shrink = right_new_generation.clone();
        let left_after_cross = left_crossed_generation.clone();
        let right_after_cross = right_crossed_generation.clone();
        move || {
            admitted("ORNA-E-NEST-OUTER", "nested generation outer admission")
                .with_cause(left_after_growth.clone())
                .with_cause(right_after_shrink.clone())
                .with_cause(left_after_cross.clone())
                .with_cause(right_after_cross.clone())
        }
    };
    let empty_parent = untrusted("ORNA-E-NEST-EMPTY", "nested empty parent secret");
    left.clone_from(&empty_parent);
    right.clone_from(&empty_parent);
    assert_eq!(left, empty_parent);
    assert_eq!(right, empty_parent);

    let outer = compose_captured_nested_generations();
    assert_eq!(outer, compose_captured_nested_generations());
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["message"], "nested generation outer admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 4);
    for (cause, expected_leaf_count) in causes.iter().zip([3, 1, 3, 1]) {
        assert_eq!(cause["message"], "<redacted>");
        let nested = cause["causes"].as_array().unwrap();
        assert_eq!(nested.len(), 1);
        assert_eq!(nested[0]["message"], "<redacted>");
        let leaves = nested[0]["causes"].as_array().unwrap();
        assert_eq!(leaves.len(), expected_leaf_count);
        for leaf in leaves {
            assert_eq!(leaf["message"], "<redacted>");
        }
    }

    let envelope = serde_json::json!({
        "outer": outer.clone(),
        "old_left": old_left,
        "new_right": new_right,
    });
    let json = serde_json::to_vec(&envelope).unwrap();
    for message in [
        b"nested generation outer admission".as_slice(),
        b"nested old left admission".as_slice(),
        b"nested new right admission".as_slice(),
    ] {
        assert_eq!(
            json.windows(message.len())
                .filter(|window| *window == message)
                .count(),
            1
        );
    }
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"nested old left cause secret".as_slice(),
            b"nested old left leaf".as_slice(),
            b"nested old right root secret".as_slice(),
            b"nested old right cause admission".as_slice(),
            b"nested old right leaf zero".as_slice(),
            b"nested old right leaf one".as_slice(),
            b"nested old right leaf two secret".as_slice(),
            b"nested new left root secret".as_slice(),
            b"nested new left cause admission".as_slice(),
            b"nested new left leaf zero secret".as_slice(),
            b"nested new left leaf one".as_slice(),
            b"nested new left leaf two secret".as_slice(),
            b"nested new right cause secret".as_slice(),
            b"nested new right leaf".as_slice(),
            b"nested empty parent secret".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
    }

    let wire = outer.encode_ovb().unwrap();
    assert!(
        wire.windows(b"nested generation outer admission".len())
            .any(|window| window == b"nested generation outer admission")
    );
    for disclosure in [
        fixture.as_bytes(),
        b"nested old left admission".as_slice(),
        b"nested new right admission".as_slice(),
        b"nested old right root secret".as_slice(),
        b"nested new left root secret".as_slice(),
        b"nested old right leaf two secret".as_slice(),
        b"nested new left leaf two secret".as_slice(),
    ]
    .into_iter()
    .chain(fixture_credentials.iter().map(|value| value.as_bytes()))
    {
        assert!(!wire.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&wire).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    assert_eq!(
        decoded["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|cause| cause["causes"][0]["causes"].as_array().unwrap().len())
            .collect::<Vec<_>>(),
        [3, 1, 3, 1]
    );
}

#[test]
fn mixed_trust_parent_closures_restore_resized_sibling_causes() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let untrusted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(message).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let old_parent = admitted("ORNA-E-CAUSE-OLD-PARENT", "old sibling parent admission")
        .with_cause(
            untrusted("ORNA-E-CAUSE-OLD-LEFT", "old sibling left cause secret")
                .with_cause(admitted("ORNA-E-CAUSE-OLD-L0", "old sibling left leaf")),
        )
        .with_cause(
            admitted("ORNA-E-CAUSE-OLD-RIGHT", "old sibling right admission")
                .with_cause(untrusted("ORNA-E-CAUSE-OLD-R0", "old sibling right leaf zero secret"))
                .with_cause(admitted("ORNA-E-CAUSE-OLD-R1", "old sibling right leaf one"))
                .with_cause(untrusted("ORNA-E-CAUSE-OLD-R2", "old sibling right leaf two secret")),
        );
    let new_parent = untrusted("ORNA-E-CAUSE-NEW-PARENT", "new sibling parent root secret")
        .with_cause(
            admitted("ORNA-E-CAUSE-NEW-LEFT", "new sibling left admission")
                .with_cause(untrusted("ORNA-E-CAUSE-NEW-L0", "new sibling left leaf zero secret"))
                .with_cause(admitted("ORNA-E-CAUSE-NEW-L1", "new sibling left leaf one"))
                .with_cause(untrusted("ORNA-E-CAUSE-NEW-L2", "new sibling left leaf two secret")),
        )
        .with_cause(
            untrusted("ORNA-E-CAUSE-NEW-RIGHT", "new sibling right cause secret")
                .with_cause(admitted("ORNA-E-CAUSE-NEW-R0", "new sibling right leaf admission")),
        );
    let source_snapshots = vec![old_parent.clone(), new_parent.clone()];
    let source_projections = source_snapshots
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let source_wires = source_snapshots
        .iter()
        .map(Diagnostic::encode_ovb)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    // The reference specifies immutable closure captures and diagnostic
    // redaction, but leaves host Clone::clone_from slot reuse open. Pin the
    // two sibling cause generations as owned snapshots when their nested
    // cause vectors grow and shrink in opposite directions.
    let capture_old = {
        let snapshot = source_snapshots[0].clone();
        move || snapshot.clone()
    };
    let capture_new = {
        let snapshot = source_snapshots[1].clone();
        move || snapshot.clone()
    };
    let mut left = capture_old();
    let mut right = capture_old();
    left.clone_from(&capture_new());
    let left_new_generation = left.clone();
    let right_old_generation = right.clone();
    assert_eq!(left_new_generation, new_parent);
    assert_eq!(right_old_generation, old_parent);

    let restore_left_new = {
        let snapshot = left_new_generation.clone();
        move || snapshot.clone()
    };
    let restore_right_old = {
        let snapshot = right_old_generation.clone();
        move || snapshot.clone()
    };
    left.clone_from(&capture_old());
    right.clone_from(&capture_new());
    let left_old_generation = left.clone();
    let right_new_generation = right.clone();
    assert_eq!(left_old_generation, old_parent);
    assert_eq!(right_new_generation, new_parent);

    left.clone_from(&restore_left_new());
    right.clone_from(&restore_right_old());
    assert_eq!(left, new_parent);
    assert_eq!(right, old_parent);
    for (index, snapshot) in source_snapshots.iter().enumerate() {
        assert_eq!(serde_json::to_value(snapshot).unwrap(), source_projections[index]);
        assert_eq!(snapshot.encode_ovb().unwrap(), source_wires[index]);
    }

    let compose_sibling_cause_generations = {
        let left_after_growth = left_new_generation.clone();
        let right_before_growth = right_old_generation.clone();
        let left_after_shrink = left_old_generation.clone();
        let right_after_shrink = right_new_generation.clone();
        move || {
            admitted("ORNA-E-CAUSE-OUTER", "sibling cause outer admission")
                .with_cause(left_after_growth.clone())
                .with_cause(right_before_growth.clone())
                .with_cause(left_after_shrink.clone())
                .with_cause(right_after_shrink.clone())
        }
    };
    let empty_parent = untrusted("ORNA-E-CAUSE-EMPTY", "sibling empty parent secret");
    left.clone_from(&empty_parent);
    right.clone_from(&empty_parent);
    assert_eq!(left, empty_parent);
    assert_eq!(right, empty_parent);

    let outer = compose_sibling_cause_generations();
    assert_eq!(outer, compose_sibling_cause_generations());
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["message"], "sibling cause outer admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 4);
    let expected_leaf_counts = [[3, 1], [1, 3], [1, 3], [3, 1]];
    for (cause, expected_siblings) in causes.iter().zip(expected_leaf_counts) {
        assert_eq!(cause["message"], "<redacted>");
        let siblings = cause["causes"].as_array().unwrap();
        assert_eq!(siblings.len(), 2);
        for (sibling, expected_leaf_count) in siblings.iter().zip(expected_siblings) {
            assert_eq!(sibling["message"], "<redacted>");
            let leaves = sibling["causes"].as_array().unwrap();
            assert_eq!(leaves.len(), expected_leaf_count);
            for leaf in leaves {
                assert_eq!(leaf["message"], "<redacted>");
            }
        }
    }

    let envelope = serde_json::json!({
        "outer": outer.clone(),
        "old_parent": old_parent,
    });
    let json = serde_json::to_vec(&envelope).unwrap();
    for message in [
        b"sibling cause outer admission".as_slice(),
        b"old sibling parent admission".as_slice(),
    ] {
        assert_eq!(
            json.windows(message.len())
                .filter(|window| *window == message)
                .count(),
            1
        );
    }
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"old sibling left cause secret".as_slice(),
            b"old sibling left leaf".as_slice(),
            b"old sibling right admission".as_slice(),
            b"old sibling right leaf zero secret".as_slice(),
            b"old sibling right leaf one".as_slice(),
            b"old sibling right leaf two secret".as_slice(),
            b"new sibling parent root secret".as_slice(),
            b"new sibling left admission".as_slice(),
            b"new sibling left leaf zero secret".as_slice(),
            b"new sibling left leaf one".as_slice(),
            b"new sibling left leaf two secret".as_slice(),
            b"new sibling right cause secret".as_slice(),
            b"new sibling right leaf admission".as_slice(),
            b"sibling empty parent secret".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
    }

    let wire = outer.encode_ovb().unwrap();
    assert!(
        wire.windows(b"sibling cause outer admission".len())
            .any(|window| window == b"sibling cause outer admission")
    );
    for disclosure in [
        fixture.as_bytes(),
        b"old sibling parent admission".as_slice(),
        b"old sibling right admission".as_slice(),
        b"new sibling parent root secret".as_slice(),
        b"new sibling left admission".as_slice(),
        b"old sibling right leaf two secret".as_slice(),
        b"new sibling left leaf two secret".as_slice(),
    ]
    .into_iter()
    .chain(fixture_credentials.iter().map(|value| value.as_bytes()))
    {
        assert!(!wire.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&wire).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    assert_eq!(
        decoded["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|cause| {
                cause["causes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|sibling| sibling["causes"].as_array().unwrap().len())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>(),
        [[3, 1], [1, 3], [1, 3], [3, 1]]
    );
}

#[test]
fn mixed_trust_parent_closures_restore_empty_sibling_cause_edges() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let untrusted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(message).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let old_parent = admitted("ORNA-E-EMPTY-OLD-PARENT", "empty edge old parent admission")
        .with_cause(untrusted("ORNA-E-EMPTY-OLD-A", "empty edge old empty sibling secret"))
        .with_cause(
            admitted("ORNA-E-EMPTY-OLD-B", "empty edge old populated sibling admission")
                .with_cause(untrusted("ORNA-E-EMPTY-OLD-B0", "empty edge old leaf secret")),
        );
    let new_parent = untrusted("ORNA-E-EMPTY-NEW-PARENT", "empty edge new parent secret")
        .with_cause(
            admitted("ORNA-E-EMPTY-NEW-A", "empty edge new expanded sibling admission")
                .with_cause(untrusted("ORNA-E-EMPTY-NEW-A0", "empty edge new leaf zero secret"))
                .with_cause(admitted("ORNA-E-EMPTY-NEW-A1", "empty edge new leaf one admission")),
        )
        .with_cause(untrusted("ORNA-E-EMPTY-NEW-B", "empty edge new empty sibling secret"));
    let source_snapshots = vec![old_parent.clone(), new_parent.clone()];
    let source_projections = source_snapshots
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let source_wires = source_snapshots
        .iter()
        .map(Diagnostic::encode_ovb)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    // The reference specifies immutable closure captures and diagnostic
    // redaction, but is silent on host Clone::clone_from reuse at empty nested
    // cause edges. Keep each parent generation as an owned snapshot while the
    // sibling cause lists cross between empty and populated states.
    let capture_old = {
        let snapshot = source_snapshots[0].clone();
        move || snapshot.clone()
    };
    let capture_new = {
        let snapshot = source_snapshots[1].clone();
        move || snapshot.clone()
    };
    let mut left = capture_old();
    let mut right = capture_old();
    left.clone_from(&capture_new());
    let left_new_generation = left.clone();
    let right_old_generation = right.clone();
    assert_eq!(left_new_generation, new_parent);
    assert_eq!(right_old_generation, old_parent);

    let restore_left_new = {
        let snapshot = left_new_generation.clone();
        move || snapshot.clone()
    };
    let restore_right_old = {
        let snapshot = right_old_generation.clone();
        move || snapshot.clone()
    };
    left.clone_from(&capture_old());
    right.clone_from(&capture_new());
    let left_old_generation = left.clone();
    let right_new_generation = right.clone();
    assert_eq!(left_old_generation, old_parent);
    assert_eq!(right_new_generation, new_parent);

    left.clone_from(&restore_left_new());
    right.clone_from(&restore_right_old());
    assert_eq!(left, new_parent);
    assert_eq!(right, old_parent);
    for (index, snapshot) in source_snapshots.iter().enumerate() {
        assert_eq!(serde_json::to_value(snapshot).unwrap(), source_projections[index]);
        assert_eq!(snapshot.encode_ovb().unwrap(), source_wires[index]);
    }

    let compose_empty_edge_generations = {
        let left_new = left_new_generation.clone();
        let right_old = right_old_generation.clone();
        let left_old = left_old_generation.clone();
        let right_new = right_new_generation.clone();
        move || {
            admitted("ORNA-E-EMPTY-OUTER", "empty edge outer admission")
                .with_cause(left_new.clone())
                .with_cause(right_old.clone())
                .with_cause(left_old.clone())
                .with_cause(right_new.clone())
        }
    };
    let empty_parent = untrusted("ORNA-E-EMPTY-LIVE", "empty edge live parent secret");
    left.clone_from(&empty_parent);
    right.clone_from(&empty_parent);
    assert_eq!(left, empty_parent);
    assert_eq!(right, empty_parent);

    let outer = compose_empty_edge_generations();
    assert_eq!(outer, compose_empty_edge_generations());
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["message"], "empty edge outer admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 4);
    let expected_leaf_counts = [[2, 0], [0, 1], [0, 1], [2, 0]];
    for (cause, expected_siblings) in causes.iter().zip(expected_leaf_counts) {
        assert_eq!(cause["message"], "<redacted>");
        let siblings = cause["causes"].as_array().unwrap();
        assert_eq!(siblings.len(), 2);
        for (sibling, expected_leaf_count) in siblings.iter().zip(expected_siblings) {
            assert_eq!(sibling["message"], "<redacted>");
            let leaves = sibling["causes"].as_array().unwrap();
            assert_eq!(leaves.len(), expected_leaf_count);
            for leaf in leaves {
                assert_eq!(leaf["message"], "<redacted>");
            }
        }
    }

    let envelope = serde_json::json!({
        "outer": outer.clone(),
        "old_parent": old_parent,
    });
    let json = serde_json::to_vec(&envelope).unwrap();
    for message in [
        b"empty edge outer admission".as_slice(),
        b"empty edge old parent admission".as_slice(),
    ] {
        assert_eq!(
            json.windows(message.len())
                .filter(|window| *window == message)
                .count(),
            1
        );
    }
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"empty edge old empty sibling secret".as_slice(),
            b"empty edge old populated sibling admission".as_slice(),
            b"empty edge old leaf secret".as_slice(),
            b"empty edge new parent secret".as_slice(),
            b"empty edge new expanded sibling admission".as_slice(),
            b"empty edge new leaf zero secret".as_slice(),
            b"empty edge new leaf one admission".as_slice(),
            b"empty edge new empty sibling secret".as_slice(),
            b"empty edge live parent secret".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
    }

    let wire = outer.encode_ovb().unwrap();
    assert!(
        wire.windows(b"empty edge outer admission".len())
            .any(|window| window == b"empty edge outer admission")
    );
    for disclosure in [
        fixture.as_bytes(),
        b"empty edge old parent admission".as_slice(),
        b"empty edge old populated sibling admission".as_slice(),
        b"empty edge new parent secret".as_slice(),
        b"empty edge new expanded sibling admission".as_slice(),
        b"empty edge new leaf one admission".as_slice(),
    ]
    .into_iter()
    .chain(fixture_credentials.iter().map(|value| value.as_bytes()))
    {
        assert!(!wire.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&wire).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    assert_eq!(
        decoded["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|cause| {
                cause["causes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|sibling| sibling["causes"].as_array().unwrap().len())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>(),
        [[2, 0], [0, 1], [0, 1], [2, 0]]
    );
}

#[test]
fn mixed_trust_parent_closures_restore_zero_cause_generations() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let untrusted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(message).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let empty_generation = admitted("ORNA-E-ZERO-EMPTY", "zero cause empty admission");
    let populated_generation =
        untrusted("ORNA-E-ZERO-POPULATED", "zero cause populated root secret")
            .with_cause(
                admitted("ORNA-E-ZERO-CHILD-0", "zero cause populated child admission")
                    .with_cause(untrusted("ORNA-E-ZERO-LEAF-0", "zero cause leaf secret")),
            )
            .with_cause(untrusted("ORNA-E-ZERO-CHILD-1", "zero cause sibling secret"));
    let source_snapshots = vec![empty_generation.clone(), populated_generation.clone()];
    let source_projections = source_snapshots
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let source_wires = source_snapshots
        .iter()
        .map(Diagnostic::encode_ovb)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    // The reference specifies immutable closure captures and diagnostic
    // redaction, but leaves host Clone::clone_from handling of an empty cause
    // vector open. Keep both zero-cause and populated generations as owned
    // snapshots while sibling parents cross between them.
    let capture_empty = {
        let snapshot = source_snapshots[0].clone();
        move || snapshot.clone()
    };
    let capture_populated = {
        let snapshot = source_snapshots[1].clone();
        move || snapshot.clone()
    };
    let mut left = capture_empty();
    let mut right = capture_empty();
    left.clone_from(&capture_populated());
    let left_populated_generation = left.clone();
    let right_empty_generation = right.clone();
    assert_eq!(left_populated_generation, populated_generation);
    assert_eq!(right_empty_generation, empty_generation);

    let restore_left_populated = {
        let snapshot = left_populated_generation.clone();
        move || snapshot.clone()
    };
    let restore_right_empty = {
        let snapshot = right_empty_generation.clone();
        move || snapshot.clone()
    };
    left.clone_from(&capture_empty());
    right.clone_from(&capture_populated());
    let left_empty_generation = left.clone();
    let right_populated_generation = right.clone();
    assert_eq!(left_empty_generation, empty_generation);
    assert_eq!(right_populated_generation, populated_generation);

    left.clone_from(&restore_left_populated());
    right.clone_from(&restore_right_empty());
    assert_eq!(left, populated_generation);
    assert_eq!(right, empty_generation);
    for (index, snapshot) in source_snapshots.iter().enumerate() {
        assert_eq!(serde_json::to_value(snapshot).unwrap(), source_projections[index]);
        assert_eq!(snapshot.encode_ovb().unwrap(), source_wires[index]);
    }

    let compose_zero_cause_generations = {
        let left_populated = left_populated_generation.clone();
        let right_empty = right_empty_generation.clone();
        let left_empty = left_empty_generation.clone();
        let right_populated = right_populated_generation.clone();
        move || {
            admitted("ORNA-E-ZERO-OUTER", "zero cause outer admission")
                .with_cause(left_populated.clone())
                .with_cause(right_empty.clone())
                .with_cause(left_empty.clone())
                .with_cause(right_populated.clone())
        }
    };
    let empty_live_parent = untrusted("ORNA-E-ZERO-LIVE", "zero cause live parent secret");
    left.clone_from(&empty_live_parent);
    right.clone_from(&empty_live_parent);
    assert_eq!(left, empty_live_parent);
    assert_eq!(right, empty_live_parent);

    let outer = compose_zero_cause_generations();
    assert_eq!(outer, compose_zero_cause_generations());
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["message"], "zero cause outer admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 4);
    let expected_shapes = [vec![1, 0], vec![], vec![], vec![1, 0]];
    for (cause, expected_nested_counts) in causes.iter().zip(expected_shapes) {
        assert_eq!(cause["message"], "<redacted>");
        let nested = cause["causes"].as_array().unwrap();
        assert_eq!(nested.len(), expected_nested_counts.len());
        let actual_nested_counts = nested
            .iter()
            .map(|sibling| {
                assert_eq!(sibling["message"], "<redacted>");
                sibling["causes"].as_array().unwrap().len()
            })
            .collect::<Vec<_>>();
        assert_eq!(actual_nested_counts, expected_nested_counts);
    }

    let envelope = serde_json::json!({
        "outer": outer.clone(),
        "empty_generation": empty_generation,
    });
    let json = serde_json::to_vec(&envelope).unwrap();
    for message in [
        b"zero cause outer admission".as_slice(),
        b"zero cause empty admission".as_slice(),
    ] {
        assert_eq!(
            json.windows(message.len())
                .filter(|window| *window == message)
                .count(),
            1
        );
    }
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"zero cause populated root secret".as_slice(),
            b"zero cause populated child admission".as_slice(),
            b"zero cause leaf secret".as_slice(),
            b"zero cause sibling secret".as_slice(),
            b"zero cause live parent secret".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
    }

    let wire = outer.encode_ovb().unwrap();
    assert!(
        wire.windows(b"zero cause outer admission".len())
            .any(|window| window == b"zero cause outer admission")
    );
    for disclosure in [
        fixture.as_bytes(),
        b"zero cause empty admission".as_slice(),
        b"zero cause populated root secret".as_slice(),
        b"zero cause populated child admission".as_slice(),
        b"zero cause leaf secret".as_slice(),
        b"zero cause sibling secret".as_slice(),
    ]
    .into_iter()
    .chain(fixture_credentials.iter().map(|value| value.as_bytes()))
    {
        assert!(!wire.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&wire).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    assert_eq!(
        decoded["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|cause| {
                cause["causes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|sibling| sibling["causes"].as_array().unwrap().len())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>(),
        [vec![1, 0], vec![], vec![], vec![1, 0]]
    );
}

#[test]
fn mixed_trust_empty_cause_closures_preserve_sibling_generation_admission() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let untrusted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(message).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let old_left = admitted("ORNA-E-EMPTY-OLD-LEFT", "empty sibling old left admission");
    let old_right = untrusted("ORNA-E-EMPTY-OLD-RIGHT", "empty sibling old right secret");
    let new_left = untrusted("ORNA-E-EMPTY-NEW-LEFT", "empty sibling new left secret");
    let new_right = admitted("ORNA-E-EMPTY-NEW-RIGHT", "empty sibling new right admission");
    let source_snapshots = vec![
        old_left.clone(),
        old_right.clone(),
        new_left.clone(),
        new_right.clone(),
    ];
    let source_projections = source_snapshots
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let source_wires = source_snapshots
        .iter()
        .map(Diagnostic::encode_ovb)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    // The reference specifies immutable closure captures and diagnostic
    // redaction, but is silent on host Clone::clone_from admission updates for
    // same-shape empty cause trees. Preserve each sibling's owned generation
    // so root admission remains local when the siblings cross and restore.
    let capture_old_left = {
        let snapshot = source_snapshots[0].clone();
        move || snapshot.clone()
    };
    let capture_old_right = {
        let snapshot = source_snapshots[1].clone();
        move || snapshot.clone()
    };
    let capture_new_left = {
        let snapshot = source_snapshots[2].clone();
        move || snapshot.clone()
    };
    let capture_new_right = {
        let snapshot = source_snapshots[3].clone();
        move || snapshot.clone()
    };
    let mut left = capture_old_left();
    let mut right = capture_old_right();
    left.clone_from(&capture_new_left());
    right.clone_from(&capture_new_right());
    let left_new_generation = left.clone();
    let right_new_generation = right.clone();
    assert_eq!(left_new_generation, new_left);
    assert_eq!(right_new_generation, new_right);

    left.clone_from(&capture_old_right());
    right.clone_from(&capture_old_left());
    let left_crossed_generation = left.clone();
    let right_crossed_generation = right.clone();
    assert_eq!(left_crossed_generation, old_right);
    assert_eq!(right_crossed_generation, old_left);

    let restore_left_new = {
        let snapshot = left_new_generation.clone();
        move || snapshot.clone()
    };
    let restore_right_new = {
        let snapshot = right_new_generation.clone();
        move || snapshot.clone()
    };
    left.clone_from(&restore_left_new());
    right.clone_from(&restore_right_new());
    assert_eq!(left, new_left);
    assert_eq!(right, new_right);
    for (index, snapshot) in source_snapshots.iter().enumerate() {
        assert_eq!(serde_json::to_value(snapshot).unwrap(), source_projections[index]);
        assert_eq!(snapshot.encode_ovb().unwrap(), source_wires[index]);
    }

    let compose_empty_sibling_generations = {
        let left_new = left_new_generation.clone();
        let right_new = right_new_generation.clone();
        let left_crossed = left_crossed_generation.clone();
        let right_crossed = right_crossed_generation.clone();
        move || {
            admitted("ORNA-E-EMPTY-OUTER", "empty sibling outer admission")
                .with_cause(left_new.clone())
                .with_cause(right_new.clone())
                .with_cause(left_crossed.clone())
                .with_cause(right_crossed.clone())
        }
    };
    let empty_live = untrusted("ORNA-E-EMPTY-LIVE", "empty sibling live replacement secret");
    left.clone_from(&empty_live);
    right.clone_from(&empty_live);
    assert_eq!(left, empty_live);
    assert_eq!(right, empty_live);

    let outer = compose_empty_sibling_generations();
    assert_eq!(outer, compose_empty_sibling_generations());
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["message"], "empty sibling outer admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 4);
    for cause in causes {
        assert_eq!(cause["message"], "<redacted>");
        assert!(cause["causes"].as_array().unwrap().is_empty());
    }

    let envelope = serde_json::json!({
        "outer": outer.clone(),
        "old_left": old_left,
        "new_right": new_right,
    });
    let json = serde_json::to_vec(&envelope).unwrap();
    for message in [
        b"empty sibling outer admission".as_slice(),
        b"empty sibling old left admission".as_slice(),
        b"empty sibling new right admission".as_slice(),
    ] {
        assert_eq!(
            json.windows(message.len())
                .filter(|window| *window == message)
                .count(),
            1
        );
    }
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"empty sibling old right secret".as_slice(),
            b"empty sibling new left secret".as_slice(),
            b"empty sibling live replacement secret".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
    }

    let wire = outer.encode_ovb().unwrap();
    assert!(
        wire.windows(b"empty sibling outer admission".len())
            .any(|window| window == b"empty sibling outer admission")
    );
    for disclosure in [
        fixture.as_bytes(),
        b"empty sibling old left admission".as_slice(),
        b"empty sibling old right secret".as_slice(),
        b"empty sibling new left secret".as_slice(),
        b"empty sibling new right admission".as_slice(),
    ]
    .into_iter()
    .chain(fixture_credentials.iter().map(|value| value.as_bytes()))
    {
        assert!(!wire.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&wire).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    assert_eq!(
        decoded["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|cause| cause["causes"].as_array().unwrap().len())
            .collect::<Vec<_>>(),
        [0, 0, 0, 0]
    );
}

#[test]
fn mixed_trust_parent_closures_preserve_empty_sibling_admission() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let untrusted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(message).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let old_admitted_sibling = admitted("ORNA-E-ADMIT-OLD-TRUSTED", "old sibling admission");
    let old_untrusted_sibling = untrusted("ORNA-E-ADMIT-OLD-RAW", "old sibling raw secret");
    let new_untrusted_sibling = untrusted("ORNA-E-ADMIT-NEW-RAW", "new sibling raw secret");
    let new_admitted_sibling = admitted("ORNA-E-ADMIT-NEW-TRUSTED", "new sibling admission");
    let old_parent = admitted("ORNA-E-ADMIT-OLD-PARENT", "old parent admission")
        .with_cause(old_admitted_sibling.clone())
        .with_cause(old_untrusted_sibling.clone());
    let new_parent = untrusted("ORNA-E-ADMIT-NEW-PARENT", "new parent raw secret")
        .with_cause(new_untrusted_sibling.clone())
        .with_cause(new_admitted_sibling.clone());
    let source_snapshots = vec![old_parent.clone(), new_parent.clone()];
    let source_projections = source_snapshots
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let source_wires = source_snapshots
        .iter()
        .map(Diagnostic::encode_ovb)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    // The reference specifies immutable closure captures and diagnostic
    // redaction, but leaves host Clone::clone_from admission reuse open for
    // same-shape parents with empty sibling causes. Preserve each parent
    // generation as an owned snapshot so child admission stays value-local.
    let capture_old = {
        let snapshot = source_snapshots[0].clone();
        move || snapshot.clone()
    };
    let capture_new = {
        let snapshot = source_snapshots[1].clone();
        move || snapshot.clone()
    };
    let mut left = capture_old();
    let mut right = capture_old();
    left.clone_from(&capture_new());
    right.clone_from(&capture_old());
    let left_new_generation = left.clone();
    let right_old_generation = right.clone();
    assert_eq!(left_new_generation, new_parent);
    assert_eq!(right_old_generation, old_parent);

    left.clone_from(&capture_old());
    right.clone_from(&capture_new());
    let left_crossed_generation = left.clone();
    let right_crossed_generation = right.clone();
    assert_eq!(left_crossed_generation, old_parent);
    assert_eq!(right_crossed_generation, new_parent);

    let restore_left_new = {
        let snapshot = left_new_generation.clone();
        move || snapshot.clone()
    };
    let restore_right_old = {
        let snapshot = right_old_generation.clone();
        move || snapshot.clone()
    };
    left.clone_from(&restore_left_new());
    right.clone_from(&restore_right_old());
    assert_eq!(left, new_parent);
    assert_eq!(right, old_parent);
    for (index, snapshot) in source_snapshots.iter().enumerate() {
        assert_eq!(serde_json::to_value(snapshot).unwrap(), source_projections[index]);
        assert_eq!(snapshot.encode_ovb().unwrap(), source_wires[index]);
    }

    let compose_empty_sibling_parents = {
        let left_new = left_new_generation.clone();
        let right_old = right_old_generation.clone();
        let left_crossed = left_crossed_generation.clone();
        let right_crossed = right_crossed_generation.clone();
        move || {
            admitted("ORNA-E-ADMIT-OUTER", "empty sibling admission outer")
                .with_cause(left_new.clone())
                .with_cause(right_old.clone())
                .with_cause(left_crossed.clone())
                .with_cause(right_crossed.clone())
        }
    };
    let empty_live = untrusted("ORNA-E-ADMIT-LIVE", "empty sibling live parent secret");
    left.clone_from(&empty_live);
    right.clone_from(&empty_live);
    assert_eq!(left, empty_live);
    assert_eq!(right, empty_live);

    let outer = compose_empty_sibling_parents();
    assert_eq!(outer, compose_empty_sibling_parents());
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["message"], "empty sibling admission outer");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 4);
    for cause in causes {
        assert_eq!(cause["message"], "<redacted>");
        let siblings = cause["causes"].as_array().unwrap();
        assert_eq!(siblings.len(), 2);
        for sibling in siblings {
            assert_eq!(sibling["message"], "<redacted>");
            assert!(sibling["causes"].as_array().unwrap().is_empty());
        }
    }

    let envelope = serde_json::json!({
        "outer": outer.clone(),
        "old_parent": old_parent,
        "old_admitted_sibling": old_admitted_sibling,
        "new_admitted_sibling": new_admitted_sibling,
    });
    let json = serde_json::to_vec(&envelope).unwrap();
    for message in [
        b"empty sibling admission outer".as_slice(),
        b"old parent admission".as_slice(),
        b"old sibling admission".as_slice(),
        b"new sibling admission".as_slice(),
    ] {
        assert_eq!(
            json.windows(message.len())
                .filter(|window| *window == message)
                .count(),
            1
        );
    }
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"old sibling raw secret".as_slice(),
            b"new sibling raw secret".as_slice(),
            b"new parent raw secret".as_slice(),
            b"empty sibling live parent secret".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
    }

    let wire = outer.encode_ovb().unwrap();
    assert!(
        wire.windows(b"empty sibling admission outer".len())
            .any(|window| window == b"empty sibling admission outer")
    );
    for disclosure in [
        fixture.as_bytes(),
        b"old parent admission".as_slice(),
        b"old sibling admission".as_slice(),
        b"new sibling admission".as_slice(),
        b"old sibling raw secret".as_slice(),
        b"new sibling raw secret".as_slice(),
    ]
    .into_iter()
    .chain(fixture_credentials.iter().map(|value| value.as_bytes()))
    {
        assert!(!wire.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&wire).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    assert_eq!(
        decoded["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|cause| {
                cause["causes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|sibling| sibling["causes"].as_array().unwrap().len())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>(),
        [[0, 0], [0, 0], [0, 0], [0, 0]]
    );
}

#[test]
fn mixed_trust_empty_sibling_cause_closures_preserve_admission() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let untrusted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(message).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let old_admitted_sibling = admitted("ORNA-E-ACLOSE-OLD-ADMITTED", "closure old sibling admission");
    let old_untrusted_sibling = untrusted("ORNA-E-ACLOSE-OLD-RAW", "closure old sibling secret");
    let new_untrusted_sibling = untrusted("ORNA-E-ACLOSE-NEW-RAW", "closure new sibling secret");
    let new_admitted_sibling = admitted("ORNA-E-ACLOSE-NEW-ADMITTED", "closure new sibling admission");
    let old_parent_factory = {
        let root = admitted("ORNA-E-ACLOSE-OLD-PARENT", "closure old parent admission");
        let old_admitted = {
            let snapshot = old_admitted_sibling.clone();
            move || snapshot.clone()
        };
        let old_untrusted = {
            let snapshot = old_untrusted_sibling.clone();
            move || snapshot.clone()
        };
        move || root.clone().with_cause(old_admitted()).with_cause(old_untrusted())
    };
    let new_parent_factory = {
        let root = untrusted("ORNA-E-ACLOSE-NEW-PARENT", "closure new parent secret");
        let new_untrusted = {
            let snapshot = new_untrusted_sibling.clone();
            move || snapshot.clone()
        };
        let new_admitted = {
            let snapshot = new_admitted_sibling.clone();
            move || snapshot.clone()
        };
        move || root.clone().with_cause(new_untrusted()).with_cause(new_admitted())
    };
    let source_snapshots = vec![old_parent_factory(), new_parent_factory()];
    let source_projections = source_snapshots
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let source_wires = source_snapshots
        .iter()
        .map(Diagnostic::encode_ovb)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    // The reference specifies immutable closure captures and diagnostic
    // redaction, but is silent on host Clone::clone_from admission reuse for
    // parents with empty sibling cause nodes. Keep each captured child value
    // owned so admission remains local through parent replacement.
    let mut left = old_parent_factory();
    let mut right = old_parent_factory();
    left.clone_from(&new_parent_factory());
    right.clone_from(&old_parent_factory());
    let left_new_generation = left.clone();
    let right_old_generation = right.clone();
    assert_eq!(left_new_generation, source_snapshots[1]);
    assert_eq!(right_old_generation, source_snapshots[0]);

    left.clone_from(&old_parent_factory());
    right.clone_from(&new_parent_factory());
    let left_crossed_generation = left.clone();
    let right_crossed_generation = right.clone();
    assert_eq!(left_crossed_generation, source_snapshots[0]);
    assert_eq!(right_crossed_generation, source_snapshots[1]);

    let restore_left_new = {
        let snapshot = left_new_generation.clone();
        move || snapshot.clone()
    };
    let restore_right_old = {
        let snapshot = right_old_generation.clone();
        move || snapshot.clone()
    };
    left.clone_from(&restore_left_new());
    right.clone_from(&restore_right_old());
    assert_eq!(left, source_snapshots[1]);
    assert_eq!(right, source_snapshots[0]);
    assert_eq!(serde_json::to_value(old_parent_factory()).unwrap(), source_projections[0]);
    assert_eq!(serde_json::to_value(new_parent_factory()).unwrap(), source_projections[1]);
    for (index, snapshot) in source_snapshots.iter().enumerate() {
        assert_eq!(serde_json::to_value(snapshot).unwrap(), source_projections[index]);
        assert_eq!(snapshot.encode_ovb().unwrap(), source_wires[index]);
    }

    let compose_captured_sibling_admissions = {
        let left_new = left_new_generation.clone();
        let right_old = right_old_generation.clone();
        let left_crossed = left_crossed_generation.clone();
        let right_crossed = right_crossed_generation.clone();
        let old_parent_factory = old_parent_factory;
        let new_parent_factory = new_parent_factory;
        move || {
            admitted("ORNA-E-ACLOSE-OUTER", "closure admission outer")
                .with_cause(left_new.clone())
                .with_cause(right_old.clone())
                .with_cause(left_crossed.clone())
                .with_cause(right_crossed.clone())
                .with_cause(old_parent_factory())
                .with_cause(new_parent_factory())
        }
    };
    let empty_live = untrusted("ORNA-E-ACLOSE-LIVE", "closure admission live secret");
    left.clone_from(&empty_live);
    right.clone_from(&empty_live);
    assert_eq!(left, empty_live);
    assert_eq!(right, empty_live);

    let outer = compose_captured_sibling_admissions();
    assert_eq!(outer, compose_captured_sibling_admissions());
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["message"], "closure admission outer");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 6);
    for cause in causes {
        assert_eq!(cause["message"], "<redacted>");
        let siblings = cause["causes"].as_array().unwrap();
        assert_eq!(siblings.len(), 2);
        for sibling in siblings {
            assert_eq!(sibling["message"], "<redacted>");
            assert!(sibling["causes"].as_array().unwrap().is_empty());
        }
    }

    let envelope = serde_json::json!({
        "outer": outer.clone(),
        "old_parent": source_snapshots[0],
        "old_admitted_sibling": old_admitted_sibling,
        "new_admitted_sibling": new_admitted_sibling,
    });
    let json = serde_json::to_vec(&envelope).unwrap();
    for message in [
        b"closure admission outer".as_slice(),
        b"closure old parent admission".as_slice(),
        b"closure old sibling admission".as_slice(),
        b"closure new sibling admission".as_slice(),
    ] {
        assert_eq!(
            json.windows(message.len())
                .filter(|window| *window == message)
                .count(),
            1
        );
    }
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"closure old sibling secret".as_slice(),
            b"closure new sibling secret".as_slice(),
            b"closure new parent secret".as_slice(),
            b"closure admission live secret".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
    }

    let wire = outer.encode_ovb().unwrap();
    assert!(
        wire.windows(b"closure admission outer".len())
            .any(|window| window == b"closure admission outer")
    );
    for disclosure in [
        fixture.as_bytes(),
        b"closure old parent admission".as_slice(),
        b"closure old sibling admission".as_slice(),
        b"closure new sibling admission".as_slice(),
        b"closure old sibling secret".as_slice(),
        b"closure new sibling secret".as_slice(),
    ]
    .into_iter()
    .chain(fixture_credentials.iter().map(|value| value.as_bytes()))
    {
        assert!(!wire.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&wire).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    assert_eq!(
        decoded["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|cause| cause["causes"].as_array().unwrap().len())
            .collect::<Vec<_>>(),
        [2, 2, 2, 2, 2, 2]
    );
}

#[test]
fn mixed_trust_empty_cause_closures_preserve_pre_revocation_admission() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let untrusted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(message).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let admitted_generation = admitted("ORNA-E-REVOKE-ADMITTED", "pre-revocation admission");
    let capture_admitted = {
        let snapshot = admitted_generation.clone();
        move || snapshot.clone()
    };
    // Orna closures capture immutable values, while explicit redaction revokes
    // only the value it consumes. Host Clone::clone_from behavior for empty
    // cause trees is unspecified, so retain the pre-revocation value as a
    // separate closure generation.
    let revoked_generation = capture_admitted().redacted();
    let capture_revoked = {
        let snapshot = revoked_generation.clone();
        move || snapshot.clone()
    };
    let raw_generation = untrusted("ORNA-E-REVOKE-RAW", "raw sibling generation secret");
    let capture_raw = {
        let snapshot = raw_generation.clone();
        move || snapshot.clone()
    };
    assert_eq!(
        serde_json::to_value(capture_admitted()).unwrap()["message"],
        "pre-revocation admission"
    );
    assert_eq!(
        serde_json::to_value(capture_revoked()).unwrap()["message"],
        "<redacted>"
    );

    let mut left = capture_admitted();
    let mut right = capture_raw();
    left.clone_from(&capture_revoked());
    right.clone_from(&capture_admitted());
    let left_revoked = left.clone();
    let right_admitted = right.clone();
    assert_eq!(left_revoked, revoked_generation);
    assert_eq!(right_admitted, admitted_generation);

    left.clone_from(&capture_admitted());
    right.clone_from(&capture_revoked());
    let left_admitted = left.clone();
    let right_revoked = right.clone();
    assert_eq!(left_admitted, admitted_generation);
    assert_eq!(right_revoked, revoked_generation);

    let restore_left_revoked = {
        let snapshot = left_revoked.clone();
        move || snapshot.clone()
    };
    let restore_right_admitted = {
        let snapshot = right_admitted.clone();
        move || snapshot.clone()
    };
    left.clone_from(&restore_left_revoked());
    right.clone_from(&restore_right_admitted());
    assert_eq!(left, revoked_generation);
    assert_eq!(right, admitted_generation);
    let pre_revocation_value = capture_admitted();
    let revoked_value = capture_revoked();
    let raw_value = capture_raw();

    let compose_captured_admission_generations = {
        let left_revoked = left_revoked.clone();
        let right_admitted = right_admitted.clone();
        let left_admitted = left_admitted.clone();
        let right_revoked = right_revoked.clone();
        let capture_admitted = capture_admitted;
        let capture_revoked = capture_revoked;
        let capture_raw = capture_raw;
        move || {
            admitted("ORNA-E-REVOKE-OUTER", "revocation closure outer admission")
                .with_cause(left_revoked.clone())
                .with_cause(right_admitted.clone())
                .with_cause(left_admitted.clone())
                .with_cause(right_revoked.clone())
                .with_cause(capture_admitted())
                .with_cause(capture_revoked())
                .with_cause(capture_raw())
        }
    };
    let empty_live = untrusted("ORNA-E-REVOKE-LIVE", "revocation live replacement secret");
    left.clone_from(&empty_live);
    right.clone_from(&empty_live);
    assert_eq!(left, empty_live);
    assert_eq!(right, empty_live);

    let outer = compose_captured_admission_generations();
    assert_eq!(outer, compose_captured_admission_generations());
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["message"], "revocation closure outer admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 7);
    for cause in causes {
        assert_eq!(cause["message"], "<redacted>");
        assert!(cause["causes"].as_array().unwrap().is_empty());
    }

    let envelope = serde_json::json!({
        "outer": outer.clone(),
        "pre_revocation": pre_revocation_value,
        "revoked": revoked_value,
        "raw": raw_value,
    });
    let json = serde_json::to_vec(&envelope).unwrap();
    for message in [
        b"revocation closure outer admission".as_slice(),
        b"pre-revocation admission".as_slice(),
    ] {
        assert_eq!(
            json.windows(message.len())
                .filter(|window| *window == message)
                .count(),
            1
        );
    }
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"raw sibling generation secret".as_slice(),
            b"revocation live replacement secret".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
    }

    let wire = outer.encode_ovb().unwrap();
    assert!(
        wire.windows(b"revocation closure outer admission".len())
            .any(|window| window == b"revocation closure outer admission")
    );
    for disclosure in [
        fixture.as_bytes(),
        b"pre-revocation admission".as_slice(),
        b"raw sibling generation secret".as_slice(),
    ]
    .into_iter()
    .chain(fixture_credentials.iter().map(|value| value.as_bytes()))
    {
        assert!(!wire.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&wire).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    assert_eq!(
        decoded["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|cause| cause["causes"].as_array().unwrap().len())
            .collect::<Vec<_>>(),
        [0, 0, 0, 0, 0, 0, 0]
    );
}

#[test]
fn mixed_trust_empty_cause_closures_keep_pre_revocation_admission_after_readmission() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let untrusted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(message).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };

    let pre_revocation_generation = admitted("ORNA-E-PREREVOKE", "captured before revocation");
    let capture_pre_revocation = {
        let snapshot = pre_revocation_generation.clone();
        move || snapshot.clone()
    };
    // The reference defines root admission and cause-boundary redaction, but
    // leaves host closure snapshots combined with a later redacted_with_message
    // and Clone::clone_from sequence unspecified. Keep each generation as its
    // own value so revocation and readmission cannot mutate earlier captures.
    let revoked_generation = capture_pre_revocation().redacted();
    let capture_revoked = {
        let snapshot = revoked_generation.clone();
        move || snapshot.clone()
    };
    let readmitted_generation = capture_revoked()
        .redacted_with_message(SafeText::new("readmitted after revocation").unwrap());
    let capture_readmitted = {
        let snapshot = readmitted_generation.clone();
        move || snapshot.clone()
    };
    assert_eq!(
        serde_json::to_value(capture_pre_revocation()).unwrap()["message"],
        "captured before revocation"
    );
    assert_eq!(
        serde_json::to_value(capture_revoked()).unwrap()["message"],
        "<redacted>"
    );
    assert_eq!(
        serde_json::to_value(capture_readmitted()).unwrap()["message"],
        "readmitted after revocation"
    );

    let mut left = capture_pre_revocation();
    let mut right = capture_revoked();
    left.clone_from(&capture_revoked());
    right.clone_from(&capture_readmitted());
    assert_eq!(left, revoked_generation);
    assert_eq!(right, readmitted_generation);
    left.clone_from(&capture_readmitted());
    right.clone_from(&capture_pre_revocation());
    assert_eq!(left, readmitted_generation);
    assert_eq!(right, pre_revocation_generation);

    let pre_revocation_value = capture_pre_revocation();
    let revoked_value = capture_revoked();
    let readmitted_value = capture_readmitted();
    let compose_captured_generations = {
        let left = left.clone();
        let right = right.clone();
        let capture_pre_revocation = capture_pre_revocation;
        let capture_revoked = capture_revoked;
        let capture_readmitted = capture_readmitted;
        move || {
            admitted("ORNA-E-PREREVOKE-OUTER", "pre-revocation outer admission")
                .with_cause(left.clone())
                .with_cause(right.clone())
                .with_cause(capture_pre_revocation())
                .with_cause(capture_revoked())
                .with_cause(capture_readmitted())
        }
    };

    let empty_live = untrusted("ORNA-E-PREREVOKE-LIVE", "replaced live sibling secret");
    left.clone_from(&empty_live);
    right.clone_from(&empty_live);
    assert_eq!(left, empty_live);
    assert_eq!(right, empty_live);

    let outer = compose_captured_generations();
    assert_eq!(outer, compose_captured_generations());
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["message"], "pre-revocation outer admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 5);
    for cause in causes {
        assert_eq!(cause["message"], "<redacted>");
        assert!(cause["causes"].as_array().unwrap().is_empty());
    }

    let envelope = serde_json::json!({
        "outer": outer.clone(),
        "pre_revocation": pre_revocation_value,
        "revoked": revoked_value,
        "readmitted": readmitted_value,
    });
    let json = serde_json::to_vec(&envelope).unwrap();
    for (message, expected_count) in [
        (b"pre-revocation outer admission".as_slice(), 1),
        (b"captured before revocation".as_slice(), 1),
        (b"readmitted after revocation".as_slice(), 1),
    ] {
        assert_eq!(
            json.windows(message.len())
                .filter(|window| *window == message)
                .count(),
            expected_count
        );
    }
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"replaced live sibling secret".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
    }

    let wire = outer.encode_ovb().unwrap();
    assert!(
        wire.windows(b"pre-revocation outer admission".len())
            .any(|window| window == b"pre-revocation outer admission")
    );
    for disclosure in [
        fixture.as_bytes(),
        b"captured before revocation".as_slice(),
        b"readmitted after revocation".as_slice(),
    ]
    .into_iter()
    .chain(fixture_credentials.iter().map(|value| value.as_bytes()))
    {
        assert!(!wire.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&wire).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    assert_eq!(
        decoded["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|cause| cause["causes"].as_array().unwrap().len())
            .collect::<Vec<_>>(),
        [0, 0, 0, 0, 0]
    );
}

#[test]
fn mixed_trust_empty_cause_closures_preserve_readmission_before_second_revocation() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let untrusted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(message).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };

    let initial_generation = admitted("ORNA-E-READMIT-INITIAL", "initial admission");
    let capture_initial = {
        let snapshot = initial_generation.clone();
        move || snapshot.clone()
    };
    // ORNA-SECRET defines root admission and cause-boundary redaction, but the
    // reference leaves closure snapshots across repeated revocation/readmission
    // and host Clone::clone_from unspecified. Model each transform as a new
    // generation so later revocation cannot rewrite earlier captures.
    let first_revoked_generation = capture_initial().redacted();
    let capture_first_revoked = {
        let snapshot = first_revoked_generation.clone();
        move || snapshot.clone()
    };
    let readmitted_generation = capture_first_revoked()
        .redacted_with_message(SafeText::new("admission restored once").unwrap());
    let capture_readmitted = {
        let snapshot = readmitted_generation.clone();
        move || snapshot.clone()
    };
    let second_revoked_generation = capture_readmitted().redacted();
    let capture_second_revoked = {
        let snapshot = second_revoked_generation.clone();
        move || snapshot.clone()
    };

    assert_eq!(
        serde_json::to_value(capture_initial()).unwrap()["message"],
        "initial admission"
    );
    assert_eq!(
        serde_json::to_value(capture_first_revoked()).unwrap()["message"],
        "<redacted>"
    );
    assert_eq!(
        serde_json::to_value(capture_readmitted()).unwrap()["message"],
        "admission restored once"
    );
    assert_eq!(
        serde_json::to_value(capture_second_revoked()).unwrap()["message"],
        "<redacted>"
    );

    let mut left = capture_initial();
    let mut right = capture_readmitted();
    left.clone_from(&capture_second_revoked());
    right.clone_from(&capture_first_revoked());
    assert_eq!(left, second_revoked_generation);
    assert_eq!(right, first_revoked_generation);
    left.clone_from(&capture_readmitted());
    right.clone_from(&capture_initial());
    assert_eq!(left, readmitted_generation);
    assert_eq!(right, initial_generation);

    let initial_value = capture_initial();
    let first_revoked_value = capture_first_revoked();
    let readmitted_value = capture_readmitted();
    let second_revoked_value = capture_second_revoked();
    let compose_captured_generations = {
        let left = left.clone();
        let right = right.clone();
        let capture_initial = capture_initial;
        let capture_first_revoked = capture_first_revoked;
        let capture_readmitted = capture_readmitted;
        let capture_second_revoked = capture_second_revoked;
        move || {
            admitted("ORNA-E-READMIT-OUTER", "readmission closure outer admission")
                .with_cause(left.clone())
                .with_cause(right.clone())
                .with_cause(capture_initial())
                .with_cause(capture_first_revoked())
                .with_cause(capture_readmitted())
                .with_cause(capture_second_revoked())
        }
    };

    let empty_live = untrusted("ORNA-E-READMIT-LIVE", "readmission replaced sibling secret");
    left.clone_from(&empty_live);
    right.clone_from(&empty_live);
    assert_eq!(left, empty_live);
    assert_eq!(right, empty_live);

    let outer = compose_captured_generations();
    assert_eq!(outer, compose_captured_generations());
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["message"], "readmission closure outer admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 6);
    for cause in causes {
        assert_eq!(cause["message"], "<redacted>");
        assert!(cause["causes"].as_array().unwrap().is_empty());
    }

    let envelope = serde_json::json!({
        "outer": outer.clone(),
        "initial": initial_value,
        "first_revoked": first_revoked_value,
        "readmitted": readmitted_value,
        "second_revoked": second_revoked_value,
    });
    let json = serde_json::to_vec(&envelope).unwrap();
    for message in [
        b"readmission closure outer admission".as_slice(),
        b"initial admission".as_slice(),
        b"admission restored once".as_slice(),
    ] {
        assert_eq!(
            json.windows(message.len())
                .filter(|window| *window == message)
                .count(),
            1
        );
    }
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"readmission replaced sibling secret".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
    }

    let wire = outer.encode_ovb().unwrap();
    assert!(
        wire.windows(b"readmission closure outer admission".len())
            .any(|window| window == b"readmission closure outer admission")
    );
    for disclosure in [
        fixture.as_bytes(),
        b"initial admission".as_slice(),
        b"admission restored once".as_slice(),
    ]
    .into_iter()
    .chain(fixture_credentials.iter().map(|value| value.as_bytes()))
    {
        assert!(!wire.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&wire).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    assert_eq!(
        decoded["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|cause| cause["causes"].as_array().unwrap().len())
            .collect::<Vec<_>>(),
        [0, 0, 0, 0, 0, 0]
    );
}

#[test]
fn readmitted_cause_closure_survives_second_revocation_of_its_parent() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let untrusted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(message).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };

    let initial_generation = admitted("ORNA-E-EDGE-INITIAL", "edge initial admission");
    let capture_initial = {
        let snapshot = initial_generation.clone();
        move || snapshot.clone()
    };
    let first_revoked_generation = capture_initial().redacted();
    let capture_first_revoked = {
        let snapshot = first_revoked_generation.clone();
        move || snapshot.clone()
    };
    let readmitted_generation = capture_first_revoked()
        .redacted_with_message(SafeText::new("edge readmitted cause admission").unwrap());
    let capture_readmitted = {
        let snapshot = readmitted_generation.clone();
        move || snapshot.clone()
    };

    // ORNA-SECRET-002 requires secret values redacted from diagnostics, but
    // leaves closure snapshots through recursive parent revocation unspecified.
    // Revoke the parent as a new generation and retain the child snapshot.
    let parent_with_readmitted_cause = admitted(
        "ORNA-E-EDGE-PARENT",
        "edge parent before second revocation",
    )
    .with_cause(capture_readmitted());
    let capture_parent = {
        let snapshot = parent_with_readmitted_cause.clone();
        move || snapshot.clone()
    };
    let second_revoked_parent = capture_parent().redacted();
    let capture_second_revoked_parent = {
        let snapshot = second_revoked_parent.clone();
        move || snapshot.clone()
    };

    let parent_projection = serde_json::to_value(capture_parent()).unwrap();
    assert_eq!(
        parent_projection["message"],
        "edge parent before second revocation"
    );
    assert_eq!(parent_projection["causes"][0]["message"], "<redacted>");
    let revoked_projection = serde_json::to_value(capture_second_revoked_parent()).unwrap();
    assert_eq!(revoked_projection["message"], "<redacted>");
    assert_eq!(revoked_projection["causes"][0]["message"], "<redacted>");
    assert_eq!(
        serde_json::to_value(capture_readmitted()).unwrap()["message"],
        "edge readmitted cause admission"
    );

    let mut left = capture_parent();
    let mut right = capture_second_revoked_parent();
    left.clone_from(&capture_second_revoked_parent());
    right.clone_from(&capture_parent());
    assert_eq!(left, second_revoked_parent);
    assert_eq!(right, parent_with_readmitted_cause);
    left.clone_from(&capture_parent());
    right.clone_from(&capture_second_revoked_parent());
    assert_eq!(left, parent_with_readmitted_cause);
    assert_eq!(right, second_revoked_parent);

    let initial_value = capture_initial();
    let readmitted_value = capture_readmitted();
    let parent_value = capture_parent();
    let second_revoked_parent_value = capture_second_revoked_parent();
    let compose_captured_parent_generations = {
        let left = left.clone();
        let right = right.clone();
        let capture_initial = capture_initial;
        let capture_readmitted = capture_readmitted;
        move || {
            admitted("ORNA-E-EDGE-OUTER", "edge closure outer admission")
                .with_cause(left.clone())
                .with_cause(right.clone())
                .with_cause(capture_initial())
                .with_cause(capture_readmitted())
        }
    };

    let empty_live = untrusted("ORNA-E-EDGE-LIVE", "edge replaced sibling secret");
    left.clone_from(&empty_live);
    right.clone_from(&empty_live);
    assert_eq!(left, empty_live);
    assert_eq!(right, empty_live);

    let outer = compose_captured_parent_generations();
    assert_eq!(outer, compose_captured_parent_generations());
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["message"], "edge closure outer admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 4);
    for cause in causes {
        assert_eq!(cause["message"], "<redacted>");
    }
    assert_eq!(causes[0]["causes"].as_array().unwrap().len(), 1);
    assert_eq!(causes[0]["causes"][0]["message"], "<redacted>");
    assert_eq!(causes[1]["causes"].as_array().unwrap().len(), 1);
    assert_eq!(causes[1]["causes"][0]["message"], "<redacted>");

    let envelope = serde_json::json!({
        "outer": outer.clone(),
        "initial": initial_value,
        "readmitted": readmitted_value,
        "parent": parent_value,
        "revoked_parent": second_revoked_parent_value,
    });
    let json = serde_json::to_vec(&envelope).unwrap();
    for message in [
        b"edge closure outer admission".as_slice(),
        b"edge initial admission".as_slice(),
        b"edge readmitted cause admission".as_slice(),
        b"edge parent before second revocation".as_slice(),
    ] {
        assert_eq!(
            json.windows(message.len())
                .filter(|window| *window == message)
                .count(),
            1
        );
    }
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"edge replaced sibling secret".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
    }

    let wire = outer.encode_ovb().unwrap();
    assert!(
        wire.windows(b"edge closure outer admission".len())
            .any(|window| window == b"edge closure outer admission")
    );
    for disclosure in [
        fixture.as_bytes(),
        b"edge initial admission".as_slice(),
        b"edge readmitted cause admission".as_slice(),
        b"edge parent before second revocation".as_slice(),
    ]
    .into_iter()
    .chain(fixture_credentials.iter().map(|value| value.as_bytes()))
    {
        assert!(!wire.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&wire).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    let decoded_causes = decoded["causes"].as_array().unwrap();
    assert_eq!(decoded_causes.len(), 4);
    assert_eq!(
        decoded_causes
            .iter()
            .map(|cause| cause["causes"].as_array().unwrap().len())
            .collect::<Vec<_>>(),
        [1, 1, 0, 0]
    );
    for cause in decoded_causes {
        assert_eq!(cause["message"], "<redacted>");
    }
}

#[test]
fn recursive_revocation_closure_preserves_nested_parent_snapshots() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let untrusted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(message).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };

    let initial_generation = admitted("ORNA-E-RECURSIVE-INITIAL", "nested initial admission");
    let capture_initial = {
        let snapshot = initial_generation.clone();
        move || snapshot.clone()
    };
    let revoked_generation = capture_initial().redacted();
    let capture_revoked = {
        let snapshot = revoked_generation.clone();
        move || snapshot.clone()
    };
    let readmitted_generation = capture_revoked()
        .redacted_with_message(SafeText::new("nested readmitted cause admission").unwrap());
    let capture_readmitted = {
        let snapshot = readmitted_generation.clone();
        move || snapshot.clone()
    };

    // ORNA-SECRET-002 requires secret values redacted from diagnostics, but
    // leaves closure snapshots through recursive parent revocation unspecified.
    // Treat recursive redaction as a new tree generation and keep old captures.
    let parent_generation = admitted("ORNA-E-RECURSIVE-PARENT", "nested parent admission")
        .with_cause(capture_readmitted());
    let capture_parent = {
        let snapshot = parent_generation.clone();
        move || snapshot.clone()
    };
    let ancestor_generation = admitted("ORNA-E-RECURSIVE-ANCESTOR", "nested ancestor admission")
        .with_cause(capture_parent());
    let capture_ancestor = {
        let snapshot = ancestor_generation.clone();
        move || snapshot.clone()
    };
    let recursively_revoked_generation = capture_ancestor().redacted();
    let capture_recursively_revoked = {
        let snapshot = recursively_revoked_generation.clone();
        move || snapshot.clone()
    };

    assert_eq!(
        serde_json::to_value(capture_initial()).unwrap()["message"],
        "nested initial admission"
    );
    assert_eq!(
        serde_json::to_value(capture_readmitted()).unwrap()["message"],
        "nested readmitted cause admission"
    );
    assert_eq!(
        serde_json::to_value(capture_parent()).unwrap()["message"],
        "nested parent admission"
    );
    assert_eq!(
        serde_json::to_value(capture_ancestor()).unwrap()["message"],
        "nested ancestor admission"
    );
    assert_redacted_tree(&serde_json::to_value(capture_recursively_revoked()).unwrap());

    let mut left = capture_ancestor();
    let mut right = capture_recursively_revoked();
    left.clone_from(&capture_recursively_revoked());
    right.clone_from(&capture_ancestor());
    assert_eq!(left, recursively_revoked_generation);
    assert_eq!(right, ancestor_generation);
    left.clone_from(&capture_ancestor());
    right.clone_from(&capture_recursively_revoked());
    assert_eq!(left, ancestor_generation);
    assert_eq!(right, recursively_revoked_generation);

    let compose_captured_generations = {
        let left = left.clone();
        let right = right.clone();
        let capture_initial = capture_initial;
        let capture_readmitted = capture_readmitted;
        let capture_parent = capture_parent;
        move || {
            admitted("ORNA-E-RECURSIVE-OUTER", "nested outer admission")
                .with_cause(left.clone())
                .with_cause(right.clone())
                .with_cause(capture_initial())
                .with_cause(capture_readmitted())
                .with_cause(capture_parent())
        }
    };

    let empty_live = untrusted("ORNA-E-RECURSIVE-LIVE", "nested replaced sibling secret");
    left.clone_from(&empty_live);
    right.clone_from(&empty_live);
    assert_eq!(left, empty_live);
    assert_eq!(right, empty_live);

    let outer = compose_captured_generations();
    assert_eq!(outer, compose_captured_generations());
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["message"], "nested outer admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 5);
    for cause in causes {
        assert_redacted_tree(cause);
    }
    assert_eq!(causes[0]["causes"].as_array().unwrap().len(), 1);
    assert_eq!(causes[0]["causes"][0]["causes"].as_array().unwrap().len(), 1);
    assert_eq!(causes[1]["causes"].as_array().unwrap().len(), 1);
    assert_eq!(causes[1]["causes"][0]["causes"].as_array().unwrap().len(), 1);

    let json = serde_json::to_vec(&outer).unwrap();
    assert_eq!(
        json.windows(b"nested outer admission".len())
            .filter(|window| *window == b"nested outer admission")
            .count(),
        1
    );
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"nested initial admission".as_slice(),
            b"nested readmitted cause admission".as_slice(),
            b"nested parent admission".as_slice(),
            b"nested ancestor admission".as_slice(),
            b"nested replaced sibling secret".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
    }

    let wire = outer.encode_ovb().unwrap();
    assert!(
        wire.windows(b"nested outer admission".len())
            .any(|window| window == b"nested outer admission")
    );
    for disclosure in [
        fixture.as_bytes(),
        b"nested initial admission".as_slice(),
        b"nested readmitted cause admission".as_slice(),
        b"nested parent admission".as_slice(),
        b"nested ancestor admission".as_slice(),
    ]
    .into_iter()
    .chain(fixture_credentials.iter().map(|value| value.as_bytes()))
    {
        assert!(!wire.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&wire).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    let decoded_causes = decoded["causes"].as_array().unwrap();
    assert_eq!(decoded_causes.len(), 5);
    for cause in decoded_causes {
        assert_redacted_tree(cause);
    }
    assert_eq!(
        decoded_causes
            .iter()
            .map(|cause| cause["causes"].as_array().unwrap().len())
            .collect::<Vec<_>>(),
        [1, 1, 0, 0, 1]
    );
}

#[test]
fn revoked_nested_snapshots_survive_cause_vector_shrink_and_regrowth() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let untrusted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(message).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };

    let old_branch = admitted("ORNA-E-SNAPSHOT-BRANCH", "old nested branch admission")
        .with_cause(admitted("ORNA-E-SNAPSHOT-TAIL-A", "old nested tail A"))
        .with_cause(admitted("ORNA-E-SNAPSHOT-TAIL-B", "old nested tail B"));
    let old_generation = admitted("ORNA-E-SNAPSHOT-PARENT", "old snapshot parent admission")
        .with_cause(old_branch);
    let capture_old = {
        let snapshot = old_generation.clone();
        move || snapshot.clone()
    };
    let revoked_generation = capture_old().redacted();
    let capture_revoked = {
        let snapshot = revoked_generation.clone();
        move || snapshot.clone()
    };
    let short_replacement = untrusted(
        "ORNA-E-SNAPSHOT-SHORT",
        "short replacement parent secret",
    )
    .with_cause(untrusted(
        "ORNA-E-SNAPSHOT-SHORT-BRANCH",
        "short replacement branch secret",
    ));

    // ORNA-SECRET-002 requires diagnostic secret redaction but leaves host
    // Clone::clone_from behavior for nested vector shrink/regrowth unspecified.
    // Keep captured tree generations as snapshots while replacing live slots.
    let mut left = capture_old();
    let mut right = capture_revoked();
    left.clone_from(&short_replacement);
    right.clone_from(&capture_old());
    assert_eq!(left, short_replacement);
    assert_eq!(right, old_generation);
    left.clone_from(&capture_revoked());
    right.clone_from(&short_replacement);
    assert_eq!(left, revoked_generation);
    assert_eq!(right, short_replacement);

    assert_eq!(
        serde_json::to_value(capture_old()).unwrap()["message"],
        "old snapshot parent admission"
    );
    let revoked_projection = serde_json::to_value(capture_revoked()).unwrap();
    assert_eq!(revoked_projection["message"], "<redacted>");
    assert_eq!(revoked_projection["causes"][0]["causes"].as_array().unwrap().len(), 2);

    let compose_captured_snapshots = {
        let left = left.clone();
        let right = right.clone();
        let capture_old = capture_old;
        let capture_revoked = capture_revoked;
        move || {
            admitted("ORNA-E-SNAPSHOT-OUTER", "snapshot closure outer admission")
                .with_cause(left.clone())
                .with_cause(right.clone())
                .with_cause(capture_old())
                .with_cause(capture_revoked())
        }
    };
    let live_replacement = untrusted(
        "ORNA-E-SNAPSHOT-LIVE",
        "live replacement snapshot secret",
    );
    left.clone_from(&live_replacement);
    right.clone_from(&live_replacement);
    assert_eq!(left, live_replacement);
    assert_eq!(right, live_replacement);

    let outer = compose_captured_snapshots();
    assert_eq!(outer, compose_captured_snapshots());
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["message"], "snapshot closure outer admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 4);
    for cause in causes {
        assert_redacted_tree(cause);
    }
    assert_eq!(causes[0]["causes"][0]["causes"].as_array().unwrap().len(), 2);
    assert!(causes[1]["causes"][0]["causes"].as_array().unwrap().is_empty());
    assert_eq!(causes[2]["causes"][0]["causes"].as_array().unwrap().len(), 2);
    assert_eq!(causes[3]["causes"][0]["causes"].as_array().unwrap().len(), 2);

    let json = serde_json::to_vec(&outer).unwrap();
    assert!(
        json.windows(b"snapshot closure outer admission".len())
            .any(|window| window == b"snapshot closure outer admission")
    );
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"old snapshot parent admission".as_slice(),
            b"old nested branch admission".as_slice(),
            b"old nested tail A".as_slice(),
            b"old nested tail B".as_slice(),
            b"short replacement parent secret".as_slice(),
            b"short replacement branch secret".as_slice(),
            b"live replacement snapshot secret".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
    }

    let wire = outer.encode_ovb().unwrap();
    assert!(
        wire.windows(b"snapshot closure outer admission".len())
            .any(|window| window == b"snapshot closure outer admission")
    );
    for disclosure in [
        fixture.as_bytes(),
        b"old snapshot parent admission".as_slice(),
        b"old nested branch admission".as_slice(),
        b"old nested tail A".as_slice(),
        b"old nested tail B".as_slice(),
    ]
    .into_iter()
    .chain(fixture_credentials.iter().map(|value| value.as_bytes()))
    {
        assert!(!wire.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&wire).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    let decoded_causes = decoded["causes"].as_array().unwrap();
    assert_eq!(decoded_causes.len(), 4);
    for cause in decoded_causes {
        assert_redacted_tree(cause);
    }
    assert_eq!(
        decoded_causes[0]["causes"][0]["causes"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        decoded_causes[1]["causes"][0]["causes"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn closure_snapshots_survive_nested_parent_and_cause_vector_resize() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn nested_cause_lengths(diagnostic: &serde_json::Value) -> Vec<usize> {
        diagnostic["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|cause| cause["causes"].as_array().unwrap().len())
            .collect()
    }

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let untrusted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(message).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };

    let old_branch_a = admitted("ORNA-E-RESIZE-BRANCH-A", "old branch A admission")
        .with_cause(admitted("ORNA-E-RESIZE-TAIL-A1", "old branch A tail one"))
        .with_cause(admitted("ORNA-E-RESIZE-TAIL-A2", "old branch A tail two"));
    let old_branch_b = admitted("ORNA-E-RESIZE-BRANCH-B", "old branch B admission")
        .with_cause(admitted("ORNA-E-RESIZE-TAIL-B1", "old branch B tail one"));
    let old_parent = admitted("ORNA-E-RESIZE-PARENT", "old resized parent admission")
        .with_cause(old_branch_a)
        .with_cause(old_branch_b);
    let capture_old = {
        let snapshot = old_parent.clone();
        move || snapshot.clone()
    };
    let revoked_parent = capture_old().redacted();
    let capture_revoked = {
        let snapshot = revoked_parent.clone();
        move || snapshot.clone()
    };
    let short_replacement = untrusted("ORNA-E-RESIZE-SHORT", "short resized parent secret")
        .with_cause(untrusted(
            "ORNA-E-RESIZE-SHORT-BRANCH",
            "short resized branch secret",
        ));

    // ORNA-SECRET-002 requires diagnostic secret redaction, but is silent on
    // host Clone::clone_from when parent and nested vectors both resize. Treat
    // closure results as immutable generations while live receivers shrink.
    let mut left = capture_old();
    let mut right = capture_revoked();
    left.clone_from(&short_replacement);
    right.clone_from(&capture_old());
    assert_eq!(left, short_replacement);
    assert_eq!(right, old_parent);
    left.clone_from(&capture_revoked());
    right.clone_from(&short_replacement);
    assert_eq!(left, revoked_parent);
    assert_eq!(right, short_replacement);

    let old_projection = serde_json::to_value(capture_old()).unwrap();
    assert_eq!(old_projection["message"], "old resized parent admission");
    assert_eq!(old_projection["causes"].as_array().unwrap().len(), 2);
    assert_eq!(nested_cause_lengths(&old_projection), [2, 1]);
    let revoked_projection = serde_json::to_value(capture_revoked()).unwrap();
    assert_redacted_tree(&revoked_projection);
    assert_eq!(revoked_projection["causes"].as_array().unwrap().len(), 2);
    assert_eq!(nested_cause_lengths(&revoked_projection), [2, 1]);

    let compose_captured_generations = {
        let left = left.clone();
        let right = right.clone();
        let capture_old = capture_old;
        let capture_revoked = capture_revoked;
        move || {
            admitted("ORNA-E-RESIZE-OUTER", "resize closure outer admission")
                .with_cause(left.clone())
                .with_cause(right.clone())
                .with_cause(capture_old())
                .with_cause(capture_revoked())
        }
    };
    let live_replacement = untrusted("ORNA-E-RESIZE-LIVE", "resized live replacement secret");
    left.clone_from(&live_replacement);
    right.clone_from(&live_replacement);
    assert_eq!(left, live_replacement);
    assert_eq!(right, live_replacement);

    let outer = compose_captured_generations();
    assert_eq!(outer, compose_captured_generations());
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["message"], "resize closure outer admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 4);
    for cause in causes {
        assert_redacted_tree(cause);
    }
    assert_eq!(
        causes
            .iter()
            .map(|cause| {
                (
                    cause["causes"].as_array().unwrap().len(),
                    nested_cause_lengths(cause),
                )
            })
            .collect::<Vec<_>>(),
        vec![
            (2, vec![2, 1]),
            (1, vec![0]),
            (2, vec![2, 1]),
            (2, vec![2, 1]),
        ]
    );

    let json = serde_json::to_vec(&outer).unwrap();
    assert!(
        json.windows(b"resize closure outer admission".len())
            .any(|window| window == b"resize closure outer admission")
    );
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"old resized parent admission".as_slice(),
            b"old branch A admission".as_slice(),
            b"old branch A tail one".as_slice(),
            b"old branch A tail two".as_slice(),
            b"old branch B admission".as_slice(),
            b"old branch B tail one".as_slice(),
            b"short resized parent secret".as_slice(),
            b"short resized branch secret".as_slice(),
            b"resized live replacement secret".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
    }

    let wire = outer.encode_ovb().unwrap();
    assert!(
        wire.windows(b"resize closure outer admission".len())
            .any(|window| window == b"resize closure outer admission")
    );
    for disclosure in [
        fixture.as_bytes(),
        b"old resized parent admission".as_slice(),
        b"old branch A tail one".as_slice(),
        b"old branch A tail two".as_slice(),
        b"old branch B tail one".as_slice(),
    ]
    .into_iter()
    .chain(fixture_credentials.iter().map(|value| value.as_bytes()))
    {
        assert!(!wire.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&wire).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    let decoded_causes = decoded["causes"].as_array().unwrap();
    assert_eq!(decoded_causes.len(), 4);
    for cause in decoded_causes {
        assert_redacted_tree(cause);
    }
    assert_eq!(
        decoded_causes
            .iter()
            .map(|cause| {
                (
                    cause["causes"].as_array().unwrap().len(),
                    nested_cause_lengths(cause),
                )
            })
            .collect::<Vec<_>>(),
        vec![
            (2, vec![2, 1]),
            (1, vec![0]),
            (2, vec![2, 1]),
            (2, vec![2, 1]),
        ]
    );
}

#[test]
fn closure_snapshots_survive_empty_nested_vector_growth_and_revocation() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn nested_cause_lengths(diagnostic: &serde_json::Value) -> Vec<usize> {
        diagnostic["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|cause| cause["causes"].as_array().unwrap().len())
            .collect()
    }

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let untrusted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(message).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };

    let empty_generation = admitted("ORNA-E-EMPTY-GROWTH-PARENT", "empty snapshot parent admission")
        .with_cause(admitted(
            "ORNA-E-EMPTY-GROWTH-BRANCH",
            "empty snapshot branch admission",
        ));
    let capture_empty = {
        let snapshot = empty_generation.clone();
        move || snapshot.clone()
    };
    let populated_source = admitted(
        "ORNA-E-EMPTY-GROWTH-PARENT",
        "populated snapshot parent admission",
    )
    .with_cause(
        admitted(
            "ORNA-E-EMPTY-GROWTH-BRANCH",
            "populated snapshot branch admission",
        )
        .with_cause(admitted(
            "ORNA-E-EMPTY-GROWTH-TAIL-A",
            "populated nested tail A",
        ))
        .with_cause(admitted(
            "ORNA-E-EMPTY-GROWTH-TAIL-B",
            "populated nested tail B",
        )),
    );

    // ORNA-SECRET-002 requires diagnostic secret redaction, but is silent on
    // host Clone::clone_from appending nested slots captured by closures. Keep
    // the empty snapshot separate while the live receiver grows and is revoked.
    let mut grown_generation = capture_empty();
    grown_generation.clone_from(&populated_source);
    assert_eq!(grown_generation, populated_source);
    let capture_grown = {
        let snapshot = grown_generation.clone();
        move || snapshot.clone()
    };
    let revoked_generation = capture_grown().redacted();
    let capture_revoked = {
        let snapshot = revoked_generation.clone();
        move || snapshot.clone()
    };

    let empty_projection = serde_json::to_value(capture_empty()).unwrap();
    assert_eq!(empty_projection["message"], "empty snapshot parent admission");
    assert_eq!(empty_projection["causes"].as_array().unwrap().len(), 1);
    assert_eq!(nested_cause_lengths(&empty_projection), [0]);
    let grown_projection = serde_json::to_value(capture_grown()).unwrap();
    assert_eq!(
        grown_projection["message"],
        "populated snapshot parent admission"
    );
    assert_eq!(nested_cause_lengths(&grown_projection), [2]);
    let revoked_projection = serde_json::to_value(capture_revoked()).unwrap();
    assert_redacted_tree(&revoked_projection);
    assert_eq!(nested_cause_lengths(&revoked_projection), [2]);

    let mut left = capture_empty();
    let mut right = capture_revoked();
    left.clone_from(&capture_grown());
    right.clone_from(&capture_empty());
    assert_eq!(left, grown_generation);
    assert_eq!(right, empty_generation);
    left.clone_from(&capture_revoked());
    right.clone_from(&capture_grown());
    assert_eq!(left, revoked_generation);
    assert_eq!(right, grown_generation);

    let compose_captured_generations = {
        let left = left.clone();
        let right = right.clone();
        let capture_empty = capture_empty;
        move || {
            admitted("ORNA-E-EMPTY-GROWTH-OUTER", "empty growth outer admission")
                .with_cause(left.clone())
                .with_cause(right.clone())
                .with_cause(capture_empty())
        }
    };
    let live_replacement = untrusted(
        "ORNA-E-EMPTY-GROWTH-LIVE",
        "empty growth live replacement secret",
    )
    .with_cause(untrusted(
        "ORNA-E-EMPTY-GROWTH-LIVE-BRANCH",
        "empty growth live nested secret",
    ));
    left.clone_from(&live_replacement);
    right.clone_from(&live_replacement);
    assert_eq!(left, live_replacement);
    assert_eq!(right, live_replacement);

    let outer = compose_captured_generations();
    assert_eq!(outer, compose_captured_generations());
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["message"], "empty growth outer admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 3);
    for cause in causes {
        assert_redacted_tree(cause);
    }
    assert_eq!(
        causes
            .iter()
            .map(nested_cause_lengths)
            .collect::<Vec<_>>(),
        vec![vec![2], vec![2], vec![0]]
    );

    let json = serde_json::to_vec(&outer).unwrap();
    assert!(
        json.windows(b"empty growth outer admission".len())
            .any(|window| window == b"empty growth outer admission")
    );
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"empty snapshot parent admission".as_slice(),
            b"populated snapshot parent admission".as_slice(),
            b"empty snapshot branch admission".as_slice(),
            b"populated snapshot branch admission".as_slice(),
            b"populated nested tail A".as_slice(),
            b"populated nested tail B".as_slice(),
            b"empty growth live replacement secret".as_slice(),
            b"empty growth live nested secret".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
    }

    let wire = outer.encode_ovb().unwrap();
    assert!(
        wire.windows(b"empty growth outer admission".len())
            .any(|window| window == b"empty growth outer admission")
    );
    for disclosure in [
        fixture.as_bytes(),
        b"empty snapshot parent admission".as_slice(),
        b"populated snapshot parent admission".as_slice(),
        b"populated nested tail A".as_slice(),
        b"populated nested tail B".as_slice(),
    ]
    .into_iter()
    .chain(fixture_credentials.iter().map(|value| value.as_bytes()))
    {
        assert!(!wire.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&wire).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    let decoded_causes = decoded["causes"].as_array().unwrap();
    assert_eq!(decoded_causes.len(), 3);
    for cause in decoded_causes {
        assert_redacted_tree(cause);
    }
    assert_eq!(
        decoded_causes
            .iter()
            .map(nested_cause_lengths)
            .collect::<Vec<_>>(),
        vec![vec![2], vec![2], vec![0]]
    );
}

#[test]
fn successive_nested_growth_closures_preserve_each_snapshot_after_revocation() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn nested_cause_lengths(diagnostic: &serde_json::Value) -> Vec<usize> {
        diagnostic["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|cause| cause["causes"].as_array().unwrap().len())
            .collect()
    }

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let untrusted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(message).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };

    let empty_generation = admitted("ORNA-E-GROWTH-STAGED-PARENT", "stage zero admission")
        .with_cause(admitted(
            "ORNA-E-GROWTH-STAGED-BRANCH",
            "stage zero branch admission",
        ));
    let capture_empty = {
        let snapshot = empty_generation.clone();
        move || snapshot.clone()
    };
    let partial_source = admitted("ORNA-E-GROWTH-STAGED-PARENT", "stage one admission")
        .with_cause(
            admitted(
                "ORNA-E-GROWTH-STAGED-BRANCH",
                "stage one branch admission",
            )
            .with_cause(admitted(
                "ORNA-E-GROWTH-STAGED-CHILD-A",
                "stage one nested child",
            )),
        );
    let full_source = admitted("ORNA-E-GROWTH-STAGED-PARENT", "stage two admission")
        .with_cause(
            admitted(
                "ORNA-E-GROWTH-STAGED-BRANCH",
                "stage two branch admission",
            )
            .with_cause(admitted(
                "ORNA-E-GROWTH-STAGED-CHILD-A",
                "stage two nested child A",
            ))
            .with_cause(admitted(
                "ORNA-E-GROWTH-STAGED-CHILD-B",
                "stage two nested child B",
            )),
        );

    // ORNA-SECRET-002 requires diagnostic secret redaction, but leaves host
    // Clone::clone_from growth across retained closures unspecified. Capture
    // each generation from the same receiver before appending the next tail.
    let mut receiver = capture_empty();
    receiver.clone_from(&partial_source);
    let capture_partial = {
        let snapshot = receiver.clone();
        move || snapshot.clone()
    };
    receiver.clone_from(&full_source);
    let capture_full = {
        let snapshot = receiver.clone();
        move || snapshot.clone()
    };
    assert_eq!(receiver, full_source);
    let revoked_generation = capture_full().redacted();
    let capture_revoked = {
        let snapshot = revoked_generation.clone();
        move || snapshot.clone()
    };

    assert_eq!(
        serde_json::to_value(capture_empty()).unwrap()["message"],
        "stage zero admission"
    );
    let partial_projection = serde_json::to_value(capture_partial()).unwrap();
    assert_eq!(partial_projection["message"], "stage one admission");
    assert_eq!(nested_cause_lengths(&partial_projection), [1]);
    let full_projection = serde_json::to_value(capture_full()).unwrap();
    assert_eq!(full_projection["message"], "stage two admission");
    assert_eq!(nested_cause_lengths(&full_projection), [2]);
    let revoked_projection = serde_json::to_value(capture_revoked()).unwrap();
    assert_redacted_tree(&revoked_projection);
    assert_eq!(nested_cause_lengths(&revoked_projection), [2]);

    let mut left = capture_empty();
    let mut right = capture_revoked();
    left.clone_from(&capture_full());
    right.clone_from(&capture_partial());
    assert_eq!(left, full_source);
    assert_eq!(right, partial_source);
    left.clone_from(&capture_revoked());
    right.clone_from(&capture_empty());
    assert_eq!(left, revoked_generation);
    assert_eq!(right, empty_generation);

    let compose_captured_stages = {
        let left = left.clone();
        let right = right.clone();
        let capture_empty = capture_empty;
        let capture_partial = capture_partial;
        let capture_full = capture_full;
        let capture_revoked = capture_revoked;
        move || {
            admitted("ORNA-E-GROWTH-STAGED-OUTER", "staged growth outer admission")
                .with_cause(left.clone())
                .with_cause(right.clone())
                .with_cause(capture_empty())
                .with_cause(capture_partial())
                .with_cause(capture_full())
                .with_cause(capture_revoked())
        }
    };
    let live_replacement = untrusted(
        "ORNA-E-GROWTH-STAGED-LIVE",
        "staged growth live replacement secret",
    );
    left.clone_from(&live_replacement);
    right.clone_from(&live_replacement);
    receiver.clone_from(&live_replacement);
    assert_eq!(left, live_replacement);
    assert_eq!(right, live_replacement);
    assert_eq!(receiver, live_replacement);

    let outer = compose_captured_stages();
    assert_eq!(outer, compose_captured_stages());
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["message"], "staged growth outer admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 6);
    for cause in causes {
        assert_redacted_tree(cause);
    }
    assert_eq!(
        causes
            .iter()
            .map(nested_cause_lengths)
            .collect::<Vec<_>>(),
        vec![vec![2], vec![0], vec![0], vec![1], vec![2], vec![2]]
    );

    let json = serde_json::to_vec(&outer).unwrap();
    assert!(
        json.windows(b"staged growth outer admission".len())
            .any(|window| window == b"staged growth outer admission")
    );
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"stage zero admission".as_slice(),
            b"stage one admission".as_slice(),
            b"stage two admission".as_slice(),
            b"stage one nested child".as_slice(),
            b"stage two nested child A".as_slice(),
            b"stage two nested child B".as_slice(),
            b"staged growth live replacement secret".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
    }

    let wire = outer.encode_ovb().unwrap();
    assert!(
        wire.windows(b"staged growth outer admission".len())
            .any(|window| window == b"staged growth outer admission")
    );
    for disclosure in [
        fixture.as_bytes(),
        b"stage zero admission".as_slice(),
        b"stage one admission".as_slice(),
        b"stage two admission".as_slice(),
        b"stage one nested child".as_slice(),
        b"stage two nested child A".as_slice(),
        b"stage two nested child B".as_slice(),
    ]
    .into_iter()
    .chain(fixture_credentials.iter().map(|value| value.as_bytes()))
    {
        assert!(!wire.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&wire).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    let decoded_causes = decoded["causes"].as_array().unwrap();
    assert_eq!(decoded_causes.len(), 6);
    for cause in decoded_causes {
        assert_redacted_tree(cause);
    }
    assert_eq!(
        decoded_causes
            .iter()
            .map(nested_cause_lengths)
            .collect::<Vec<_>>(),
        vec![vec![2], vec![0], vec![0], vec![1], vec![2], vec![2]]
    );
}

#[test]
fn staged_growth_closures_keep_replaced_nested_tail_snapshots() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn nested_cause_lengths(diagnostic: &serde_json::Value) -> Vec<usize> {
        diagnostic["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|cause| cause["causes"].as_array().unwrap().len())
            .collect()
    }

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let untrusted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(message).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let make_parent = |parent_message: &str, branch_message: &str, tails: Vec<Diagnostic>| {
        let branch = tails.into_iter().fold(
            admitted("ORNA-E-GROWTH-EDGE-BRANCH", branch_message),
            |branch, tail| branch.with_cause(tail),
        );
        admitted("ORNA-E-GROWTH-EDGE-PARENT", parent_message).with_cause(branch)
    };

    let old_tail_a = admitted("ORNA-E-GROWTH-EDGE-TAIL-A", "stable first nested tail");
    let old_tail_b = admitted("ORNA-E-GROWTH-EDGE-TAIL-B", "captured old final tail");
    let new_tail_b = admitted("ORNA-E-GROWTH-EDGE-TAIL-B", "appended replacement final tail");
    let capture_old_tail = {
        let snapshot = old_tail_b.clone();
        move || snapshot.clone()
    };
    let capture_new_tail = {
        let snapshot = new_tail_b.clone();
        move || snapshot.clone()
    };

    let empty_source = make_parent("stage zero parent", "empty nested branch", vec![]);
    let stage_one_source = make_parent(
        "stage one parent",
        "one-child nested branch",
        vec![old_tail_a.clone()],
    );
    let old_full_source = make_parent(
        "old full parent",
        "old full nested branch",
        vec![old_tail_a.clone(), old_tail_b.clone()],
    );
    let regrown_source = make_parent(
        "regrown full parent",
        "regrown nested branch",
        vec![old_tail_a.clone(), new_tail_b.clone()],
    );

    // ORNA-SECRET-002 requires diagnostic secret redaction, but leaves host
    // Clone::clone_from tail reuse unspecified after shrink and regrowth. Capture
    // each stage separately, including the replaced final cause generation.
    let mut receiver = empty_source.clone();
    let capture_empty = {
        let snapshot = receiver.clone();
        move || snapshot.clone()
    };
    receiver.clone_from(&stage_one_source);
    let capture_stage_one = {
        let snapshot = receiver.clone();
        move || snapshot.clone()
    };
    receiver.clone_from(&old_full_source);
    let capture_old_full = {
        let snapshot = receiver.clone();
        move || snapshot.clone()
    };
    receiver.clone_from(&stage_one_source);
    assert_eq!(receiver, stage_one_source);
    receiver.clone_from(&regrown_source);
    let capture_regrown = {
        let snapshot = receiver.clone();
        move || snapshot.clone()
    };
    assert_eq!(receiver, regrown_source);
    let revoked_generation = capture_regrown().redacted();
    let capture_revoked = {
        let snapshot = revoked_generation.clone();
        move || snapshot.clone()
    };

    assert_eq!(capture_empty(), empty_source);
    assert_eq!(capture_stage_one(), stage_one_source);
    assert_eq!(capture_old_full(), old_full_source);
    assert_eq!(capture_regrown(), regrown_source);
    assert_eq!(capture_revoked(), revoked_generation);
    assert_eq!(
        serde_json::to_value(capture_old_tail()).unwrap()["message"],
        "captured old final tail"
    );
    assert_eq!(
        serde_json::to_value(capture_new_tail()).unwrap()["message"],
        "appended replacement final tail"
    );

    let mut left = capture_old_full();
    let mut right = capture_revoked();
    left.clone_from(&capture_regrown());
    right.clone_from(&capture_stage_one());
    assert_eq!(left, regrown_source);
    assert_eq!(right, stage_one_source);
    left.clone_from(&capture_revoked());
    right.clone_from(&capture_empty());
    assert_eq!(left, revoked_generation);
    assert_eq!(right, empty_source);

    let compose_captured_stages = {
        let left = left.clone();
        let right = right.clone();
        let capture_empty = capture_empty;
        let capture_stage_one = capture_stage_one;
        let capture_old_full = capture_old_full;
        let capture_regrown = capture_regrown;
        let capture_revoked = capture_revoked;
        let capture_old_tail = capture_old_tail;
        let capture_new_tail = capture_new_tail;
        move || {
            admitted("ORNA-E-GROWTH-EDGE-OUTER", "growth edge outer admission")
                .with_cause(left.clone())
                .with_cause(right.clone())
                .with_cause(capture_empty())
                .with_cause(capture_stage_one())
                .with_cause(capture_old_full())
                .with_cause(capture_regrown())
                .with_cause(capture_revoked())
                .with_cause(capture_old_tail())
                .with_cause(capture_new_tail())
        }
    };

    let live_replacement = untrusted(
        "ORNA-E-GROWTH-EDGE-LIVE",
        "growth edge live replacement secret",
    );
    left.clone_from(&live_replacement);
    right.clone_from(&live_replacement);
    receiver.clone_from(&live_replacement);
    assert_eq!(left, live_replacement);
    assert_eq!(right, live_replacement);
    assert_eq!(receiver, live_replacement);

    let outer = compose_captured_stages();
    assert_eq!(outer, compose_captured_stages());
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["message"], "growth edge outer admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 9);
    for cause in causes {
        assert_redacted_tree(cause);
    }
    assert_eq!(
        causes
            .iter()
            .map(nested_cause_lengths)
            .collect::<Vec<_>>(),
        vec![
            vec![2],
            vec![0],
            vec![0],
            vec![1],
            vec![2],
            vec![2],
            vec![2],
            vec![],
            vec![],
        ]
    );

    let json = serde_json::to_vec(&outer).unwrap();
    assert!(
        json.windows(b"growth edge outer admission".len())
            .any(|window| window == b"growth edge outer admission")
    );
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"stage zero parent".as_slice(),
            b"stage one parent".as_slice(),
            b"old full parent".as_slice(),
            b"regrown full parent".as_slice(),
            b"stable first nested tail".as_slice(),
            b"captured old final tail".as_slice(),
            b"appended replacement final tail".as_slice(),
            b"growth edge live replacement secret".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
    }

    let wire = outer.encode_ovb().unwrap();
    assert!(
        wire.windows(b"growth edge outer admission".len())
            .any(|window| window == b"growth edge outer admission")
    );
    for disclosure in [
        fixture.as_bytes(),
        b"stage zero parent".as_slice(),
        b"stage one parent".as_slice(),
        b"old full parent".as_slice(),
        b"regrown full parent".as_slice(),
        b"captured old final tail".as_slice(),
        b"appended replacement final tail".as_slice(),
    ]
    .into_iter()
    .chain(fixture_credentials.iter().map(|value| value.as_bytes()))
    {
        assert!(!wire.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&wire).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    let decoded_causes = decoded["causes"].as_array().unwrap();
    assert_eq!(decoded_causes.len(), 9);
    for cause in decoded_causes {
        assert_redacted_tree(cause);
    }
    assert_eq!(
        decoded_causes
            .iter()
            .map(nested_cause_lengths)
            .collect::<Vec<_>>(),
        vec![
            vec![2],
            vec![0],
            vec![0],
            vec![1],
            vec![2],
            vec![2],
            vec![2],
            vec![],
            vec![],
        ]
    );
}

#[test]
fn untrusted_nested_tail_replacement_keeps_admitted_closure_snapshot() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn nested_cause_lengths(diagnostic: &serde_json::Value) -> Vec<usize> {
        diagnostic["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|cause| cause["causes"].as_array().unwrap().len())
            .collect()
    }

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let untrusted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(message).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let make_parent = |message: &str, branch_message: &str, tails: Vec<Diagnostic>| {
        let branch = tails.into_iter().fold(
            admitted("ORNA-E-TRUST-TAIL-BRANCH", branch_message),
            |branch, tail| branch.with_cause(tail),
        );
        admitted("ORNA-E-TRUST-TAIL-PARENT", message).with_cause(branch)
    };

    let stable_tail = admitted("ORNA-E-TRUST-TAIL-A", "stable admitted tail");
    let old_tail = admitted("ORNA-E-TRUST-TAIL-B", "old admitted final tail");
    let replacement_tail = untrusted(
        "ORNA-E-TRUST-TAIL-B",
        "untrusted replacement final tail secret",
    );
    let capture_old_tail = {
        let snapshot = old_tail.clone();
        move || snapshot.clone()
    };
    let capture_replacement_tail = {
        let snapshot = replacement_tail.clone();
        move || snapshot.clone()
    };
    let old_full_source = make_parent(
        "admitted parent before replacement",
        "admitted full branch before replacement",
        vec![stable_tail.clone(), old_tail.clone()],
    );
    let short_source = make_parent(
        "admitted short parent snapshot",
        "admitted short branch snapshot",
        vec![stable_tail.clone()],
    );
    let untrusted_regrown_source = untrusted(
        "ORNA-E-TRUST-TAIL-PARENT",
        "untrusted regrown parent secret",
    )
    .with_cause(
        untrusted(
            "ORNA-E-TRUST-TAIL-BRANCH",
            "untrusted regrown branch secret",
        )
        .with_cause(stable_tail.clone())
        .with_cause(replacement_tail.clone()),
    );

    // ORNA-SECRET-002 requires diagnostic secret redaction, but leaves host
    // Clone::clone_from trust replacement at a re-appended nested tail open.
    // Keep the admitted and untrusted tail captures as separate generations.
    let mut receiver = old_full_source.clone();
    let capture_old_full = {
        let snapshot = receiver.clone();
        move || snapshot.clone()
    };
    receiver.clone_from(&short_source);
    let capture_short = {
        let snapshot = receiver.clone();
        move || snapshot.clone()
    };
    receiver.clone_from(&untrusted_regrown_source);
    assert_eq!(receiver, untrusted_regrown_source);
    let capture_replacement = {
        let snapshot = receiver.clone();
        move || snapshot.clone()
    };
    let revoked_generation = capture_replacement().redacted();
    let capture_revoked = {
        let snapshot = revoked_generation.clone();
        move || snapshot.clone()
    };

    assert_eq!(capture_old_full(), old_full_source);
    assert_eq!(capture_short(), short_source);
    assert_eq!(capture_replacement(), untrusted_regrown_source);
    assert_eq!(capture_revoked(), revoked_generation);
    assert_eq!(
        serde_json::to_value(capture_old_tail()).unwrap()["message"],
        "old admitted final tail"
    );
    assert_eq!(
        serde_json::to_value(capture_replacement_tail()).unwrap()["message"],
        "<redacted>"
    );
    let replacement_projection = serde_json::to_value(capture_replacement()).unwrap();
    assert_redacted_tree(&replacement_projection);
    assert_eq!(nested_cause_lengths(&replacement_projection), [2]);
    let revoked_projection = serde_json::to_value(capture_revoked()).unwrap();
    assert_redacted_tree(&revoked_projection);
    assert_eq!(nested_cause_lengths(&revoked_projection), [2]);

    let mut left = capture_old_full();
    let mut right = capture_revoked();
    left.clone_from(&capture_replacement());
    right.clone_from(&capture_short());
    assert_eq!(left, untrusted_regrown_source);
    assert_eq!(right, short_source);
    left.clone_from(&capture_revoked());
    right.clone_from(&capture_old_full());
    assert_eq!(left, revoked_generation);
    assert_eq!(right, old_full_source);

    let compose_captured_trust_generations = {
        let left = left.clone();
        let right = right.clone();
        let capture_short = capture_short;
        let capture_old_full = capture_old_full;
        let capture_replacement = capture_replacement;
        let capture_revoked = capture_revoked;
        let capture_old_tail = capture_old_tail;
        let capture_replacement_tail = capture_replacement_tail;
        move || {
            admitted("ORNA-E-TRUST-TAIL-OUTER", "tail trust outer admission")
                .with_cause(left.clone())
                .with_cause(right.clone())
                .with_cause(capture_short())
                .with_cause(capture_old_full())
                .with_cause(capture_replacement())
                .with_cause(capture_revoked())
                .with_cause(capture_old_tail())
                .with_cause(capture_replacement_tail())
        }
    };
    let live_replacement = untrusted("ORNA-E-TRUST-TAIL-LIVE", "tail live replacement secret");
    left.clone_from(&live_replacement);
    right.clone_from(&live_replacement);
    receiver.clone_from(&live_replacement);
    assert_eq!(left, live_replacement);
    assert_eq!(right, live_replacement);
    assert_eq!(receiver, live_replacement);

    let outer = compose_captured_trust_generations();
    assert_eq!(outer, compose_captured_trust_generations());
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["message"], "tail trust outer admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 8);
    for cause in causes {
        assert_redacted_tree(cause);
    }
    assert_eq!(
        causes
            .iter()
            .map(nested_cause_lengths)
            .collect::<Vec<_>>(),
        vec![
            vec![2],
            vec![2],
            vec![1],
            vec![2],
            vec![2],
            vec![2],
            vec![],
            vec![],
        ]
    );

    let json = serde_json::to_vec(&outer).unwrap();
    assert!(
        json.windows(b"tail trust outer admission".len())
            .any(|window| window == b"tail trust outer admission")
    );
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"admitted parent before replacement".as_slice(),
            b"admitted short parent snapshot".as_slice(),
            b"untrusted regrown parent secret".as_slice(),
            b"untrusted regrown branch secret".as_slice(),
            b"stable admitted tail".as_slice(),
            b"old admitted final tail".as_slice(),
            b"untrusted replacement final tail secret".as_slice(),
            b"tail live replacement secret".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
    }

    let wire = outer.encode_ovb().unwrap();
    assert!(
        wire.windows(b"tail trust outer admission".len())
            .any(|window| window == b"tail trust outer admission")
    );
    for disclosure in [
        fixture.as_bytes(),
        b"admitted parent before replacement".as_slice(),
        b"untrusted regrown parent secret".as_slice(),
        b"stable admitted tail".as_slice(),
        b"old admitted final tail".as_slice(),
        b"untrusted replacement final tail secret".as_slice(),
    ]
    .into_iter()
    .chain(fixture_credentials.iter().map(|value| value.as_bytes()))
    {
        assert!(!wire.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&wire).unwrap()).unwrap();
    assert_eq!(decoded["message"], "<redacted>");
    let decoded_causes = decoded["causes"].as_array().unwrap();
    assert_eq!(decoded_causes.len(), 8);
    for cause in decoded_causes {
        assert_redacted_tree(cause);
    }
    assert_eq!(
        decoded_causes
            .iter()
            .map(nested_cause_lengths)
            .collect::<Vec<_>>(),
        vec![
            vec![2],
            vec![2],
            vec![1],
            vec![2],
            vec![2],
            vec![2],
            vec![],
            vec![],
        ]
    );
}

#[test]
fn decoded_chain_tail_recovers_after_resize_without_inheriting_admission() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn branch_tail_lengths(diagnostic: &serde_json::Value) -> Vec<usize> {
        diagnostic["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|branch| branch["causes"].as_array().unwrap().len())
            .collect()
    }

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let make_parent = |message: &str, branch_message: &str, tails: Vec<Diagnostic>| {
        let branch = tails.into_iter().fold(
            admitted("ORNA-E-RECOVERY-BRANCH", branch_message),
            |branch, tail| branch.with_cause(tail),
        );
        admitted("ORNA-E-RECOVERY-PARENT", message).with_cause(branch)
    };
    let capture = |diagnostic: Diagnostic| move || diagnostic.clone();

    let full_source = make_parent(
        "parent before recovery",
        "branch before recovery",
        vec![
            admitted("ORNA-E-RECOVERY-A", "first recovered tail"),
            admitted("ORNA-E-RECOVERY-B", "second recovered tail"),
        ],
    );
    let full_recovery = Diagnostic::decode_ovb(&full_source.encode_ovb().unwrap()).unwrap();
    let capture_full_recovery = capture(full_recovery.clone());
    let short_source = make_parent(
        "short parent generation",
        "short branch generation",
        vec![admitted("ORNA-E-RECOVERY-A", "short tail")],
    );
    let short_recovery = Diagnostic::decode_ovb(&short_source.encode_ovb().unwrap()).unwrap();

    // ORNA-SECRET-002 requires recursive redaction at the diagnostic boundary,
    // but is silent on closure snapshots around host Clone::clone_from shrink
    // and recovery. Keep each decoded generation owned, and treat readmission
    // as authority for its root only.
    let mut receiver = full_recovery.clone();
    let capture_initial_full = capture(receiver.clone());
    receiver.clone_from(&short_recovery);
    let capture_shrunk = capture(receiver.clone());
    receiver.clone_from(&capture_full_recovery());
    let capture_restored = capture(receiver.clone());
    let readmitted = capture_full_recovery()
        .redacted_with_message(SafeText::new("recovered parent admission").unwrap());
    receiver.clone_from(&readmitted);
    let capture_readmitted = capture(receiver.clone());

    assert_eq!(capture_initial_full(), full_recovery);
    assert_eq!(capture_shrunk(), short_recovery);
    assert_eq!(capture_restored(), full_recovery);
    assert_eq!(capture_readmitted(), readmitted);
    assert_eq!(
        serde_json::to_value(capture_readmitted()).unwrap()["message"],
        "recovered parent admission"
    );

    let compose_recoveries = {
        let capture_initial_full = capture_initial_full;
        let capture_shrunk = capture_shrunk;
        let capture_restored = capture_restored;
        let capture_readmitted = capture_readmitted;
        move || {
            admitted("ORNA-E-RECOVERY-OUTER", "recovery chain outer admission")
                .with_cause(capture_initial_full())
                .with_cause(capture_shrunk())
                .with_cause(capture_restored())
                .with_cause(capture_readmitted())
        }
    };
    let replacement = make_parent(
        "replacement parent secret",
        "replacement branch secret",
        vec![],
    );
    receiver.clone_from(&replacement);
    assert_eq!(receiver, replacement);

    let outer = compose_recoveries();
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["message"], "recovery chain outer admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 4);
    for cause in causes {
        assert_redacted_tree(cause);
    }
    assert_eq!(
        causes.iter().map(branch_tail_lengths).collect::<Vec<_>>(),
        vec![vec![2], vec![1], vec![2], vec![2]],
    );

    let json = serde_json::to_vec(&outer).unwrap();
    let wire = outer.encode_ovb().unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"parent before recovery".as_slice(),
            b"branch before recovery".as_slice(),
            b"first recovered tail".as_slice(),
            b"second recovered tail".as_slice(),
            b"short parent generation".as_slice(),
            b"short branch generation".as_slice(),
            b"recovered parent admission".as_slice(),
            b"replacement parent secret".as_slice(),
            b"replacement branch secret".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
        assert!(!wire.windows(disclosure.len()).any(|window| window == disclosure));
    }

    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&wire).unwrap()).unwrap();
    assert_redacted_tree(&decoded);
    let decoded_causes = decoded["causes"].as_array().unwrap();
    assert_eq!(decoded_causes.len(), 4);
    assert_eq!(
        decoded_causes
            .iter()
            .map(branch_tail_lengths)
            .collect::<Vec<_>>(),
        vec![vec![2], vec![1], vec![2], vec![2]],
    );
}

#[test]
fn decoded_chain_roundtrip_keeps_replaced_tail_closure_generations() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn branch_tail_codes(diagnostic: &serde_json::Value) -> Vec<Vec<String>> {
        diagnostic["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|branch| {
                branch["causes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|tail| tail["code"].as_str().unwrap().to_owned())
                    .collect()
            })
            .collect()
    }

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let make_parent = |message: &str, tail_codes: &[(&str, &str)]| {
        let branch = tail_codes.iter().fold(
            admitted("ORNA-E-ROUNDTRIP-BRANCH", "branch generation"),
            |branch, (code, message)| {
                branch.with_cause(admitted(code, message))
            },
        );
        admitted("ORNA-E-ROUNDTRIP-PARENT", message).with_cause(branch)
    };
    let recover = |diagnostic: &Diagnostic| {
        Diagnostic::decode_ovb(&diagnostic.encode_ovb().unwrap()).unwrap()
    };
    let capture = |snapshot: Diagnostic| move || snapshot.clone();

    let original_source = make_parent(
        "original root generation",
        &[
            ("ORNA-E-ROUNDTRIP-A", "stable first tail secret"),
            ("ORNA-E-ROUNDTRIP-OLD", "original final tail secret"),
        ],
    );
    let original_wire = original_source.encode_ovb().unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([fixture.as_bytes()])
    {
        assert!(!original_wire.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let original_recovery = recover(&original_source);
    let replacement_source = make_parent(
        "replacement root generation",
        &[
            ("ORNA-E-ROUNDTRIP-A", "stable replacement first tail secret"),
            ("ORNA-E-ROUNDTRIP-NEW", "replacement final tail secret"),
        ],
    );
    let replacement_recovery = recover(&replacement_source);
    let short_recovery = recover(&make_parent(
        "short root generation",
        &[("ORNA-E-ROUNDTRIP-A", "short final tail secret")],
    ));

    // ORNA-SECRET-002 requires recursive diagnostic redaction, but does not
    // define host closure behavior when Clone::clone_from shrinks and recovers
    // a decoded chain across another encode/decode admission reset. Preserve
    // each decoded and locally readmitted generation as its own snapshot.
    let capture_original = capture(original_recovery.clone());
    let capture_replacement = capture(replacement_recovery.clone());
    let capture_short = capture(short_recovery.clone());
    let readmitted = replacement_recovery
        .clone()
        .redacted_with_message(SafeText::new("readmitted recovered root").unwrap());
    let roundtrip_recovery = recover(&readmitted);
    let capture_readmitted = capture(readmitted.clone());
    let capture_roundtrip = capture(roundtrip_recovery.clone());

    let mut receiver = original_recovery.clone();
    let capture_initial = capture(receiver.clone());
    receiver.clone_from(&short_recovery);
    let capture_shrunk = capture(receiver.clone());
    receiver.clone_from(&capture_replacement());
    let capture_regrown = capture(receiver.clone());
    receiver.clone_from(&capture_readmitted());
    let capture_admitted = capture(receiver.clone());
    receiver.clone_from(&capture_roundtrip());
    let capture_revoked = capture(receiver.clone());

    assert_eq!(capture_initial(), original_recovery);
    assert_eq!(capture_shrunk(), short_recovery);
    assert_eq!(capture_regrown(), replacement_recovery);
    assert_eq!(capture_admitted(), readmitted);
    assert_eq!(capture_revoked(), roundtrip_recovery);
    let admitted_projection = serde_json::to_value(capture_admitted()).unwrap();
    assert_eq!(admitted_projection["message"], "readmitted recovered root");
    for cause in admitted_projection["causes"].as_array().unwrap() {
        assert_redacted_tree(cause);
    }
    assert_eq!(
        serde_json::to_value(capture_revoked()).unwrap()["message"],
        "<redacted>"
    );

    let compose_generations = {
        let capture_initial = capture_initial;
        let capture_original = capture_original;
        let capture_shrunk = capture_shrunk;
        let capture_replacement = capture_replacement;
        let capture_regrown = capture_regrown;
        let capture_admitted = capture_admitted;
        let capture_revoked = capture_revoked;
        let capture_short = capture_short;
        move || {
            admitted("ORNA-E-ROUNDTRIP-OUTER", "roundtrip closure outer admission")
                .with_cause(capture_initial())
                .with_cause(capture_original())
                .with_cause(capture_shrunk())
                .with_cause(capture_replacement())
                .with_cause(capture_regrown())
                .with_cause(capture_admitted())
                .with_cause(capture_revoked())
                .with_cause(capture_short())
        }
    };
    receiver.clone_from(&make_parent("empty replacement generation", &[]));
    let outer = compose_generations();
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["message"], "roundtrip closure outer admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 8);
    for cause in causes {
        assert_redacted_tree(cause);
    }
    assert_eq!(
        causes.iter().map(branch_tail_codes).collect::<Vec<_>>(),
        vec![
            vec![vec![
                "ORNA-E-ROUNDTRIP-A".to_owned(),
                "ORNA-E-ROUNDTRIP-OLD".to_owned(),
            ]],
            vec![vec![
                "ORNA-E-ROUNDTRIP-A".to_owned(),
                "ORNA-E-ROUNDTRIP-OLD".to_owned(),
            ]],
            vec![vec!["ORNA-E-ROUNDTRIP-A".to_owned()]],
            vec![vec![
                "ORNA-E-ROUNDTRIP-A".to_owned(),
                "ORNA-E-ROUNDTRIP-NEW".to_owned(),
            ]],
            vec![vec![
                "ORNA-E-ROUNDTRIP-A".to_owned(),
                "ORNA-E-ROUNDTRIP-NEW".to_owned(),
            ]],
            vec![vec![
                "ORNA-E-ROUNDTRIP-A".to_owned(),
                "ORNA-E-ROUNDTRIP-NEW".to_owned(),
            ]],
            vec![vec![
                "ORNA-E-ROUNDTRIP-A".to_owned(),
                "ORNA-E-ROUNDTRIP-NEW".to_owned(),
            ]],
            vec![vec!["ORNA-E-ROUNDTRIP-A".to_owned()]],
        ],
    );

    let json = serde_json::to_vec(&outer).unwrap();
    let wire = outer.encode_ovb().unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"original root generation".as_slice(),
            b"original final tail secret".as_slice(),
            b"replacement root generation".as_slice(),
            b"replacement final tail secret".as_slice(),
            b"short root generation".as_slice(),
            b"readmitted recovered root".as_slice(),
            b"empty replacement generation".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
        assert!(!wire.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&wire).unwrap()).unwrap();
    assert_redacted_tree(&decoded);
    assert_eq!(
        decoded["causes"].as_array().unwrap().len(),
        8,
    );
    assert_eq!(
        decoded["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(branch_tail_codes)
            .collect::<Vec<_>>(),
        causes.iter().map(branch_tail_codes).collect::<Vec<_>>(),
    );
}

#[test]
fn decoded_empty_chain_tail_can_be_recovered_without_phantom_causes() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn branch_tail_codes(diagnostic: &serde_json::Value) -> Vec<Vec<String>> {
        diagnostic["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|branch| {
                branch["causes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|tail| tail["code"].as_str().unwrap().to_owned())
                    .collect()
            })
            .collect()
    }
    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let make_parent = |message: &str, tails: Vec<Diagnostic>| {
        let branch = tails.into_iter().fold(
            admitted("ORNA-E-EMPTY-RECOVERY-BRANCH", "branch payload"),
            |branch, tail| branch.with_cause(tail),
        );
        admitted("ORNA-E-EMPTY-RECOVERY-PARENT", message).with_cause(branch)
    };
    let recover = |diagnostic: &Diagnostic| {
        Diagnostic::decode_ovb(&diagnostic.encode_ovb().unwrap()).unwrap()
    };
    let capture = |snapshot: Diagnostic| move || snapshot.clone();

    let full_recovery = recover(&make_parent(
        "full recovered parent payload",
        vec![
            admitted("ORNA-E-EMPTY-RECOVERY-A", "first recovered tail payload"),
            admitted("ORNA-E-EMPTY-RECOVERY-B", "last recovered tail payload"),
        ],
    ));
    let empty_recovery = recover(&make_parent("empty recovered parent payload", vec![]));

    // ORNA-SECRET-002 requires recursive redaction, but does not specify how
    // captured Rust values observe a decoded zero-length nested tail when a
    // parent is later restored. Keep the empty and populated generations as
    // independent snapshots through each replacement.
    let capture_full = capture(full_recovery.clone());
    let capture_empty = capture(empty_recovery.clone());
    let empty_readmission = empty_recovery
        .clone()
        .redacted_with_message(SafeText::new("empty recovered root admission").unwrap());
    let capture_empty_readmission = capture(empty_readmission.clone());

    let mut receiver = full_recovery.clone();
    let capture_initial = capture(receiver.clone());
    receiver.clone_from(&capture_empty());
    let capture_shrunk = capture(receiver.clone());
    receiver.clone_from(&capture_full());
    let capture_restored = capture(receiver.clone());
    receiver.clone_from(&capture_empty_readmission());
    let capture_admitted_empty = capture(receiver.clone());
    receiver.clone_from(&capture_full());
    let capture_final_full = capture(receiver.clone());

    assert_eq!(capture_initial(), full_recovery);
    assert_eq!(capture_shrunk(), empty_recovery);
    assert_eq!(capture_restored(), full_recovery);
    assert_eq!(capture_admitted_empty(), empty_readmission);
    assert_eq!(capture_final_full(), full_recovery);
    let empty_projection = serde_json::to_value(capture_admitted_empty()).unwrap();
    assert_eq!(empty_projection["message"], "empty recovered root admission");
    assert_eq!(
        branch_tail_codes(&empty_projection),
        vec![Vec::<String>::new()],
    );
    for cause in empty_projection["causes"].as_array().unwrap() {
        assert_redacted_tree(cause);
    }

    let compose_generations = {
        let capture_initial = capture_initial;
        let capture_shrunk = capture_shrunk;
        let capture_restored = capture_restored;
        let capture_admitted_empty = capture_admitted_empty;
        let capture_final_full = capture_final_full;
        move || {
            admitted("ORNA-E-EMPTY-RECOVERY-OUTER", "empty recovery outer admission")
                .with_cause(capture_initial())
                .with_cause(capture_shrunk())
                .with_cause(capture_restored())
                .with_cause(capture_admitted_empty())
                .with_cause(capture_final_full())
        }
    };
    receiver.clone_from(&capture_empty());
    let outer = compose_generations();
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["message"], "empty recovery outer admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 5);
    for cause in causes {
        assert_redacted_tree(cause);
    }
    assert_eq!(
        causes.iter().map(branch_tail_codes).collect::<Vec<_>>(),
        vec![
            vec![vec![
                "ORNA-E-EMPTY-RECOVERY-A".to_owned(),
                "ORNA-E-EMPTY-RECOVERY-B".to_owned(),
            ]],
            vec![Vec::<String>::new()],
            vec![vec![
                "ORNA-E-EMPTY-RECOVERY-A".to_owned(),
                "ORNA-E-EMPTY-RECOVERY-B".to_owned(),
            ]],
            vec![Vec::<String>::new()],
            vec![vec![
                "ORNA-E-EMPTY-RECOVERY-A".to_owned(),
                "ORNA-E-EMPTY-RECOVERY-B".to_owned(),
            ]],
        ],
    );

    let json = serde_json::to_vec(&outer).unwrap();
    let wire = outer.encode_ovb().unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"full recovered parent payload".as_slice(),
            b"empty recovered parent payload".as_slice(),
            b"first recovered tail payload".as_slice(),
            b"last recovered tail payload".as_slice(),
            b"empty recovered root admission".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
        assert!(!wire.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&wire).unwrap()).unwrap();
    assert_redacted_tree(&decoded);
    assert_eq!(
        decoded["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(branch_tail_codes)
            .collect::<Vec<_>>(),
        causes.iter().map(branch_tail_codes).collect::<Vec<_>>(),
    );
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

#[test]
fn decoded_empty_parent_replacement_restores_chain_closure_snapshots() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn cause_shape(diagnostic: &serde_json::Value) -> Vec<usize> {
        let causes = diagnostic["causes"].as_array().unwrap();
        let mut shape = vec![causes.len()];
        for cause in causes {
            shape.extend(cause_shape(cause));
        }
        shape
    }
    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let full_source = admitted("ORNA-E-EMPTY-PARENT", "populated parent payload").with_cause(
        admitted("ORNA-E-EMPTY-BRANCH", "populated branch payload")
            .with_cause(admitted("ORNA-E-EMPTY-A", "first cause payload"))
            .with_cause(admitted("ORNA-E-EMPTY-B", "last cause payload")),
    );
    let empty_source = admitted("ORNA-E-EMPTY-PARENT", "empty parent payload");
    let recover = |diagnostic: &Diagnostic| {
        Diagnostic::decode_ovb(&diagnostic.encode_ovb().unwrap()).unwrap()
    };
    let capture = |snapshot: Diagnostic| move || snapshot.clone();
    let full_recovery = recover(&full_source);
    let empty_recovery = recover(&empty_source);

    // ORNA-SECRET-002 requires diagnostic redaction, but is silent on closure
    // snapshots when Clone::clone_from replaces a decoded parent's entire
    // cause vector with an empty generation and later restores its chain.
    let capture_full = capture(full_recovery.clone());
    let capture_empty = capture(empty_recovery.clone());
    let empty_readmission = empty_recovery
        .clone()
        .redacted_with_message(SafeText::new("empty root readmission").unwrap());
    let capture_empty_readmission = capture(empty_readmission.clone());

    let mut receiver = full_recovery.clone();
    let capture_initial_full = capture(receiver.clone());
    receiver.clone_from(&capture_empty());
    let capture_empty_parent = capture(receiver.clone());
    receiver.clone_from(&capture_full());
    let capture_restored_full = capture(receiver.clone());
    receiver.clone_from(&capture_empty_readmission());
    let capture_admitted_empty = capture(receiver.clone());
    receiver.clone_from(&capture_full());
    let capture_final_full = capture(receiver.clone());

    assert_eq!(capture_initial_full(), full_recovery);
    assert_eq!(capture_empty_parent(), empty_recovery);
    assert_eq!(capture_restored_full(), full_recovery);
    assert_eq!(capture_admitted_empty(), empty_readmission);
    assert_eq!(capture_final_full(), full_recovery);
    let empty_projection = serde_json::to_value(capture_admitted_empty()).unwrap();
    assert_eq!(empty_projection["message"], "empty root readmission");
    assert_eq!(cause_shape(&empty_projection), [0]);

    let compose_generations = {
        let capture_initial_full = capture_initial_full;
        let capture_empty_parent = capture_empty_parent;
        let capture_restored_full = capture_restored_full;
        let capture_admitted_empty = capture_admitted_empty;
        let capture_final_full = capture_final_full;
        move || {
            admitted("ORNA-E-EMPTY-OUTER", "empty parent closure admission")
                .with_cause(capture_initial_full())
                .with_cause(capture_empty_parent())
                .with_cause(capture_restored_full())
                .with_cause(capture_admitted_empty())
                .with_cause(capture_final_full())
        }
    };
    receiver.clone_from(&capture_empty());
    let outer = compose_generations();
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["message"], "empty parent closure admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 5);
    for cause in causes {
        assert_redacted_tree(cause);
    }
    assert_eq!(
        causes.iter().map(cause_shape).collect::<Vec<_>>(),
        vec![
            vec![1, 2, 0, 0],
            vec![0],
            vec![1, 2, 0, 0],
            vec![0],
            vec![1, 2, 0, 0],
        ],
    );

    let json = serde_json::to_vec(&outer).unwrap();
    let wire = outer.encode_ovb().unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"populated parent payload".as_slice(),
            b"populated branch payload".as_slice(),
            b"first cause payload".as_slice(),
            b"last cause payload".as_slice(),
            b"empty parent payload".as_slice(),
            b"empty root readmission".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
        assert!(!wire.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&wire).unwrap()).unwrap();
    assert_redacted_tree(&decoded);
    assert_eq!(
        decoded["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(cause_shape)
            .collect::<Vec<_>>(),
        causes.iter().map(cause_shape).collect::<Vec<_>>(),
    );
}

#[test]
fn decoded_deeper_parent_chain_recovery_keeps_parent_and_tail_identity() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn cause_shape(diagnostic: &serde_json::Value) -> Vec<usize> {
        let causes = diagnostic["causes"].as_array().unwrap();
        let mut shape = vec![causes.len()];
        for cause in causes {
            shape.extend(cause_shape(cause));
        }
        shape
    }
    fn nested_parent_code(diagnostic: &serde_json::Value) -> String {
        diagnostic["causes"][0]["causes"][0]["code"]
            .as_str()
            .unwrap()
            .to_owned()
    }
    fn deep_tail_codes(diagnostic: &serde_json::Value) -> Vec<String> {
        diagnostic["causes"][0]["causes"][0]["causes"][0]["causes"][0]["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tail| tail["code"].as_str().unwrap().to_owned())
            .collect()
    }
    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let make_parent = |generation_code: &str,
                       root_message: &str,
                       generation_message: &str,
                       terminal_message: &str,
                       tails: Vec<Diagnostic>| {
        let terminal = tails.into_iter().fold(
            admitted("ORNA-E-DEEPER-TERMINAL", terminal_message),
            |terminal, tail| terminal.with_cause(tail),
        );
        let nested = admitted("ORNA-E-DEEPER-NESTED", "nested parent payload")
            .with_cause(terminal);
        let parent = admitted(generation_code, generation_message).with_cause(nested);
        let branch = admitted("ORNA-E-DEEPER-BRANCH", "outer branch payload").with_cause(parent);
        admitted("ORNA-E-DEEPER-ROOT", root_message).with_cause(branch)
    };
    let recover = |diagnostic: &Diagnostic| {
        Diagnostic::decode_ovb(&diagnostic.encode_ovb().unwrap()).unwrap()
    };
    let capture = |snapshot: Diagnostic| move || snapshot.clone();

    let old_recovery = recover(&make_parent(
        "ORNA-E-DEEPER-OLD-PARENT",
        "old deep root payload",
        "old nested parent payload",
        "old terminal parent payload",
        vec![
            admitted("ORNA-E-DEEPER-OLD-A", "old first deep tail payload"),
            admitted("ORNA-E-DEEPER-OLD-B", "old final deep tail payload"),
        ],
    ));
    let empty_recovery = recover(&make_parent(
        "ORNA-E-DEEPER-EMPTY-PARENT",
        "empty deep root payload",
        "empty nested parent payload",
        "empty terminal parent payload",
        vec![],
    ));
    let new_recovery = recover(&make_parent(
        "ORNA-E-DEEPER-NEW-PARENT",
        "new deep root payload",
        "new nested parent payload",
        "new terminal parent payload",
        vec![admitted("ORNA-E-DEEPER-NEW", "new final deep tail payload")],
    ));

    // ORNA-SECRET-002 requires recursive redaction, but leaves closure
    // snapshots around decoded parent replacement at this nested depth open.
    // Keep the parent-code and tail generations distinct across each restore;
    // root readmission remains local and is revoked when composed as a cause.
    let capture_old = capture(old_recovery.clone());
    let capture_empty = capture(empty_recovery.clone());
    let capture_new = capture(new_recovery.clone());
    let empty_readmission = empty_recovery
        .clone()
        .redacted_with_message(SafeText::new("empty deep root readmission").unwrap());
    let capture_empty_readmission = capture(empty_readmission.clone());

    let mut receiver = old_recovery.clone();
    let capture_initial_old = capture(receiver.clone());
    receiver.clone_from(&capture_empty());
    let capture_deep_empty = capture(receiver.clone());
    receiver.clone_from(&capture_new());
    let capture_deep_new = capture(receiver.clone());
    receiver.clone_from(&capture_old());
    let capture_old_restored = capture(receiver.clone());
    receiver.clone_from(&capture_empty_readmission());
    let capture_readmitted_empty = capture(receiver.clone());
    receiver.clone_from(&capture_new());
    let capture_final_new = capture(receiver.clone());

    assert_eq!(capture_initial_old(), old_recovery);
    assert_eq!(capture_deep_empty(), empty_recovery);
    assert_eq!(capture_deep_new(), new_recovery);
    assert_eq!(capture_old_restored(), old_recovery);
    assert_eq!(capture_readmitted_empty(), empty_readmission);
    assert_eq!(capture_final_new(), new_recovery);
    let readmitted_projection = serde_json::to_value(capture_readmitted_empty()).unwrap();
    assert_eq!(readmitted_projection["message"], "empty deep root readmission");
    assert_eq!(cause_shape(&readmitted_projection), [1, 1, 1, 1, 0]);
    for cause in readmitted_projection["causes"].as_array().unwrap() {
        assert_redacted_tree(cause);
    }

    let compose_generations = {
        let capture_initial_old = capture_initial_old;
        let capture_deep_empty = capture_deep_empty;
        let capture_deep_new = capture_deep_new;
        let capture_old_restored = capture_old_restored;
        let capture_readmitted_empty = capture_readmitted_empty;
        let capture_final_new = capture_final_new;
        move || {
            admitted("ORNA-E-DEEPER-OUTER", "deeper recovery outer admission")
                .with_cause(capture_initial_old())
                .with_cause(capture_deep_empty())
                .with_cause(capture_deep_new())
                .with_cause(capture_old_restored())
                .with_cause(capture_readmitted_empty())
                .with_cause(capture_final_new())
        }
    };
    receiver.clone_from(&capture_empty());
    let outer = compose_generations();
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["message"], "deeper recovery outer admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 6);
    for cause in causes {
        assert_redacted_tree(cause);
    }
    assert_eq!(
        causes.iter().map(cause_shape).collect::<Vec<_>>(),
        vec![
            vec![1, 1, 1, 1, 2, 0, 0],
            vec![1, 1, 1, 1, 0],
            vec![1, 1, 1, 1, 1, 0],
            vec![1, 1, 1, 1, 2, 0, 0],
            vec![1, 1, 1, 1, 0],
            vec![1, 1, 1, 1, 1, 0],
        ],
    );
    assert_eq!(
        causes.iter().map(nested_parent_code).collect::<Vec<_>>(),
        vec![
            "ORNA-E-DEEPER-OLD-PARENT",
            "ORNA-E-DEEPER-EMPTY-PARENT",
            "ORNA-E-DEEPER-NEW-PARENT",
            "ORNA-E-DEEPER-OLD-PARENT",
            "ORNA-E-DEEPER-EMPTY-PARENT",
            "ORNA-E-DEEPER-NEW-PARENT",
        ],
    );
    assert_eq!(
        causes.iter().map(deep_tail_codes).collect::<Vec<_>>(),
        vec![
            vec![
                "ORNA-E-DEEPER-OLD-A".to_owned(),
                "ORNA-E-DEEPER-OLD-B".to_owned(),
            ],
            vec![],
            vec!["ORNA-E-DEEPER-NEW".to_owned()],
            vec![
                "ORNA-E-DEEPER-OLD-A".to_owned(),
                "ORNA-E-DEEPER-OLD-B".to_owned(),
            ],
            vec![],
            vec!["ORNA-E-DEEPER-NEW".to_owned()],
        ],
    );

    let json = serde_json::to_vec(&outer).unwrap();
    let wire = outer.encode_ovb().unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"old deep root payload".as_slice(),
            b"old nested parent payload".as_slice(),
            b"old terminal parent payload".as_slice(),
            b"old final deep tail payload".as_slice(),
            b"empty deep root payload".as_slice(),
            b"empty terminal parent payload".as_slice(),
            b"new deep root payload".as_slice(),
            b"new terminal parent payload".as_slice(),
            b"new final deep tail payload".as_slice(),
            b"empty deep root readmission".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
        assert!(!wire.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&wire).unwrap()).unwrap();
    assert_redacted_tree(&decoded);
    assert_eq!(
        decoded["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(cause_shape)
            .collect::<Vec<_>>(),
        causes.iter().map(cause_shape).collect::<Vec<_>>(),
    );
}

#[test]
fn decoded_deep_nested_parent_tail_recovery_keeps_closure_generations() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn cause_shape(diagnostic: &serde_json::Value) -> Vec<usize> {
        let causes = diagnostic["causes"].as_array().unwrap();
        let mut shape = vec![causes.len()];
        for cause in causes {
            shape.extend(cause_shape(cause));
        }
        shape
    }
    fn deep_tail_codes(diagnostic: &serde_json::Value) -> Vec<String> {
        diagnostic["causes"][0]["causes"][0]["causes"][0]["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tail| tail["code"].as_str().unwrap().to_owned())
            .collect()
    }
    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let make_parent = |root_message: &str,
                       branch_message: &str,
                       nested_message: &str,
                       terminal_message: &str,
                       tails: Vec<Diagnostic>| {
        let terminal = tails.into_iter().fold(
            admitted("ORNA-E-DEEP-TERMINAL", terminal_message),
            |terminal, tail| terminal.with_cause(tail),
        );
        let nested = admitted("ORNA-E-DEEP-NESTED", nested_message).with_cause(terminal);
        let branch = admitted("ORNA-E-DEEP-BRANCH", branch_message).with_cause(nested);
        admitted("ORNA-E-DEEP-ROOT", root_message).with_cause(branch)
    };
    let recover = |diagnostic: &Diagnostic| {
        Diagnostic::decode_ovb(&diagnostic.encode_ovb().unwrap()).unwrap()
    };
    let capture = |snapshot: Diagnostic| move || snapshot.clone();

    let full_recovery = recover(&make_parent(
        "deep full root payload",
        "deep full branch payload",
        "deep full nested payload",
        "deep full terminal payload",
        vec![
            admitted("ORNA-E-DEEP-OLD-A", "deep old first tail payload"),
            admitted("ORNA-E-DEEP-OLD-B", "deep old final tail payload"),
        ],
    ));
    let empty_recovery = recover(&make_parent(
        "deep empty root payload",
        "deep empty branch payload",
        "deep empty nested payload",
        "deep empty terminal payload",
        vec![],
    ));
    let replacement_recovery = recover(&make_parent(
        "deep replacement root payload",
        "deep replacement branch payload",
        "deep replacement nested payload",
        "deep replacement terminal payload",
        vec![admitted(
            "ORNA-E-DEEP-NEW",
            "deep replacement tail payload",
        )],
    ));

    // ORNA-SECRET-002 requires recursive redaction, but is silent on closure
    // snapshots when a decoded tail vector four parent edges deep is emptied,
    // replaced, and recovered. Treat each decoded tree as an owned generation;
    // locally readmitting its root does not admit any nested parent.
    let capture_full = capture(full_recovery.clone());
    let capture_empty = capture(empty_recovery.clone());
    let capture_replacement = capture(replacement_recovery.clone());
    let empty_readmission = empty_recovery
        .clone()
        .redacted_with_message(SafeText::new("deep empty root readmission").unwrap());
    let capture_empty_readmission = capture(empty_readmission.clone());

    let mut receiver = full_recovery.clone();
    let capture_initial_full = capture(receiver.clone());
    receiver.clone_from(&capture_empty());
    let capture_empty_tail = capture(receiver.clone());
    receiver.clone_from(&capture_replacement());
    let capture_new_tail = capture(receiver.clone());
    receiver.clone_from(&capture_full());
    let capture_restored_full = capture(receiver.clone());
    receiver.clone_from(&capture_empty_readmission());
    let capture_readmitted_empty = capture(receiver.clone());
    receiver.clone_from(&capture_replacement());
    let capture_final_new = capture(receiver.clone());

    assert_eq!(capture_initial_full(), full_recovery);
    assert_eq!(capture_empty_tail(), empty_recovery);
    assert_eq!(capture_new_tail(), replacement_recovery);
    assert_eq!(capture_restored_full(), full_recovery);
    assert_eq!(capture_readmitted_empty(), empty_readmission);
    assert_eq!(capture_final_new(), replacement_recovery);
    let readmitted_projection = serde_json::to_value(capture_readmitted_empty()).unwrap();
    assert_eq!(readmitted_projection["message"], "deep empty root readmission");
    assert_eq!(cause_shape(&readmitted_projection), [1, 1, 1, 0]);
    for cause in readmitted_projection["causes"].as_array().unwrap() {
        assert_redacted_tree(cause);
    }

    let compose_generations = {
        let capture_initial_full = capture_initial_full;
        let capture_empty_tail = capture_empty_tail;
        let capture_new_tail = capture_new_tail;
        let capture_restored_full = capture_restored_full;
        let capture_readmitted_empty = capture_readmitted_empty;
        let capture_final_new = capture_final_new;
        move || {
            admitted("ORNA-E-DEEP-OUTER", "deep recovery outer admission")
                .with_cause(capture_initial_full())
                .with_cause(capture_empty_tail())
                .with_cause(capture_new_tail())
                .with_cause(capture_restored_full())
                .with_cause(capture_readmitted_empty())
                .with_cause(capture_final_new())
        }
    };
    receiver.clone_from(&capture_empty());
    let outer = compose_generations();
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["message"], "deep recovery outer admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 6);
    for cause in causes {
        assert_redacted_tree(cause);
    }
    assert_eq!(
        causes.iter().map(cause_shape).collect::<Vec<_>>(),
        vec![
            vec![1, 1, 1, 2, 0, 0],
            vec![1, 1, 1, 0],
            vec![1, 1, 1, 1, 0],
            vec![1, 1, 1, 2, 0, 0],
            vec![1, 1, 1, 0],
            vec![1, 1, 1, 1, 0],
        ],
    );
    assert_eq!(
        causes.iter().map(deep_tail_codes).collect::<Vec<_>>(),
        vec![
            vec![
                "ORNA-E-DEEP-OLD-A".to_owned(),
                "ORNA-E-DEEP-OLD-B".to_owned(),
            ],
            vec![],
            vec!["ORNA-E-DEEP-NEW".to_owned()],
            vec![
                "ORNA-E-DEEP-OLD-A".to_owned(),
                "ORNA-E-DEEP-OLD-B".to_owned(),
            ],
            vec![],
            vec!["ORNA-E-DEEP-NEW".to_owned()],
        ],
    );

    let json = serde_json::to_vec(&outer).unwrap();
    let wire = outer.encode_ovb().unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"deep full root payload".as_slice(),
            b"deep full branch payload".as_slice(),
            b"deep full nested payload".as_slice(),
            b"deep full terminal payload".as_slice(),
            b"deep old final tail payload".as_slice(),
            b"deep empty root payload".as_slice(),
            b"deep empty terminal payload".as_slice(),
            b"deep replacement root payload".as_slice(),
            b"deep replacement terminal payload".as_slice(),
            b"deep replacement tail payload".as_slice(),
            b"deep empty root readmission".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
        assert!(!wire.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&wire).unwrap()).unwrap();
    assert_redacted_tree(&decoded);
    assert_eq!(
        decoded["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(cause_shape)
            .collect::<Vec<_>>(),
        causes.iter().map(cause_shape).collect::<Vec<_>>(),
    );
}

#[test]
fn decoded_parent_chains_cross_empty_sibling_replacements_without_tail_leaks() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn cause_shape(diagnostic: &serde_json::Value) -> Vec<usize> {
        let causes = diagnostic["causes"].as_array().unwrap();
        let mut shape = vec![causes.len()];
        for cause in causes {
            shape.extend(cause_shape(cause));
        }
        shape
    }
    fn tail_codes(diagnostic: &serde_json::Value) -> Vec<String> {
        diagnostic["causes"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|branch| branch["causes"].as_array().unwrap())
            .map(|tail| tail["code"].as_str().unwrap().to_owned())
            .collect()
    }
    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let old_source = admitted("ORNA-E-PARENT-CHAIN", "old parent payload").with_cause(
        admitted("ORNA-E-PARENT-OLD-BRANCH", "old branch payload")
            .with_cause(admitted("ORNA-E-PARENT-OLD-A", "old first tail payload"))
            .with_cause(admitted("ORNA-E-PARENT-OLD-B", "old final tail payload")),
    );
    let new_source = admitted("ORNA-E-PARENT-CHAIN", "new parent payload").with_cause(
        admitted("ORNA-E-PARENT-NEW-BRANCH", "new branch payload")
            .with_cause(admitted("ORNA-E-PARENT-NEW-A", "new first tail payload"))
            .with_cause(admitted("ORNA-E-PARENT-NEW-B", "new final tail payload")),
    );
    let empty_source = admitted("ORNA-E-PARENT-CHAIN", "empty parent payload");
    let recover = |diagnostic: &Diagnostic| {
        Diagnostic::decode_ovb(&diagnostic.encode_ovb().unwrap()).unwrap()
    };
    let capture = |snapshot: Diagnostic| move || snapshot.clone();
    let old_recovery = recover(&old_source);
    let new_recovery = recover(&new_source);
    let empty_recovery = recover(&empty_source);

    // ORNA-SECRET-002 requires recursive redaction, but is silent on captured
    // generations when two decoded parent chains cross through empty siblings
    // and then recover into their original receivers. Preserve each state as
    // an owned snapshot so a later replacement cannot change an older tail.
    let capture_old = capture(old_recovery.clone());
    let capture_new = capture(new_recovery.clone());
    let capture_empty = capture(empty_recovery.clone());
    let mut left = old_recovery.clone();
    let mut right = new_recovery.clone();
    let capture_left_initial = capture(left.clone());
    let capture_right_initial = capture(right.clone());

    left.clone_from(&capture_empty());
    right.clone_from(&capture_empty());
    let capture_left_empty = capture(left.clone());
    let capture_right_empty = capture(right.clone());

    left.clone_from(&capture_new());
    right.clone_from(&capture_old());
    let capture_left_crossed = capture(left.clone());
    let capture_right_crossed = capture(right.clone());

    left.clone_from(&capture_old());
    right.clone_from(&capture_new());
    let capture_left_restored = capture(left.clone());
    let capture_right_restored = capture(right.clone());

    assert_eq!(capture_left_initial(), old_recovery);
    assert_eq!(capture_right_initial(), new_recovery);
    assert_eq!(capture_left_empty(), empty_recovery);
    assert_eq!(capture_right_empty(), empty_recovery);
    assert_eq!(capture_left_crossed(), new_recovery);
    assert_eq!(capture_right_crossed(), old_recovery);
    assert_eq!(capture_left_restored(), old_recovery);
    assert_eq!(capture_right_restored(), new_recovery);

    let compose_sibling_generations = {
        let capture_left_initial = capture_left_initial;
        let capture_right_initial = capture_right_initial;
        let capture_left_empty = capture_left_empty;
        let capture_right_empty = capture_right_empty;
        let capture_left_crossed = capture_left_crossed;
        let capture_right_crossed = capture_right_crossed;
        let capture_left_restored = capture_left_restored;
        let capture_right_restored = capture_right_restored;
        move || {
            admitted("ORNA-E-PARENT-OUTER", "parent chain outer admission")
                .with_cause(capture_left_initial())
                .with_cause(capture_right_initial())
                .with_cause(capture_left_empty())
                .with_cause(capture_right_empty())
                .with_cause(capture_left_crossed())
                .with_cause(capture_right_crossed())
                .with_cause(capture_left_restored())
                .with_cause(capture_right_restored())
        }
    };
    left.clone_from(&capture_empty());
    right.clone_from(&capture_empty());
    let outer = compose_sibling_generations();
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["message"], "parent chain outer admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 8);
    for cause in causes {
        assert_redacted_tree(cause);
    }
    assert_eq!(
        causes.iter().map(cause_shape).collect::<Vec<_>>(),
        vec![
            vec![1, 2, 0, 0],
            vec![1, 2, 0, 0],
            vec![0],
            vec![0],
            vec![1, 2, 0, 0],
            vec![1, 2, 0, 0],
            vec![1, 2, 0, 0],
            vec![1, 2, 0, 0],
        ],
    );
    assert_eq!(
        causes.iter().map(tail_codes).collect::<Vec<_>>(),
        vec![
            vec![
                "ORNA-E-PARENT-OLD-A".to_owned(),
                "ORNA-E-PARENT-OLD-B".to_owned(),
            ],
            vec![
                "ORNA-E-PARENT-NEW-A".to_owned(),
                "ORNA-E-PARENT-NEW-B".to_owned(),
            ],
            vec![],
            vec![],
            vec![
                "ORNA-E-PARENT-NEW-A".to_owned(),
                "ORNA-E-PARENT-NEW-B".to_owned(),
            ],
            vec![
                "ORNA-E-PARENT-OLD-A".to_owned(),
                "ORNA-E-PARENT-OLD-B".to_owned(),
            ],
            vec![
                "ORNA-E-PARENT-OLD-A".to_owned(),
                "ORNA-E-PARENT-OLD-B".to_owned(),
            ],
            vec![
                "ORNA-E-PARENT-NEW-A".to_owned(),
                "ORNA-E-PARENT-NEW-B".to_owned(),
            ],
        ],
    );

    let json = serde_json::to_vec(&outer).unwrap();
    let wire = outer.encode_ovb().unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"old parent payload".as_slice(),
            b"old branch payload".as_slice(),
            b"old first tail payload".as_slice(),
            b"old final tail payload".as_slice(),
            b"new parent payload".as_slice(),
            b"new branch payload".as_slice(),
            b"new first tail payload".as_slice(),
            b"new final tail payload".as_slice(),
            b"empty parent payload".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
        assert!(!wire.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&wire).unwrap()).unwrap();
    assert_redacted_tree(&decoded);
    assert_eq!(
        decoded["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(tail_codes)
            .collect::<Vec<_>>(),
        causes.iter().map(tail_codes).collect::<Vec<_>>(),
    );
}

#[test]
fn decoded_nested_parent_tail_recovery_keeps_empty_and_replaced_generations() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn cause_shape(diagnostic: &serde_json::Value) -> Vec<usize> {
        let causes = diagnostic["causes"].as_array().unwrap();
        let mut shape = vec![causes.len()];
        for cause in causes {
            shape.extend(cause_shape(cause));
        }
        shape
    }
    fn deep_tail_codes(diagnostic: &serde_json::Value) -> Vec<String> {
        diagnostic["causes"][0]["causes"][0]["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tail| tail["code"].as_str().unwrap().to_owned())
            .collect()
    }
    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let make_parent = |root_message: &str,
                       branch_message: &str,
                       inner_message: &str,
                       tails: Vec<Diagnostic>| {
        let inner = tails.into_iter().fold(
            admitted("ORNA-E-NESTED-RECOVERY-PARENT", inner_message),
            |inner, tail| inner.with_cause(tail),
        );
        let branch = admitted("ORNA-E-NESTED-RECOVERY-BRANCH", branch_message).with_cause(inner);
        admitted("ORNA-E-NESTED-RECOVERY-ROOT", root_message).with_cause(branch)
    };
    let recover = |diagnostic: &Diagnostic| {
        Diagnostic::decode_ovb(&diagnostic.encode_ovb().unwrap()).unwrap()
    };
    let capture = |snapshot: Diagnostic| move || snapshot.clone();

    let full_recovery = recover(&make_parent(
        "full chain root payload",
        "full chain branch payload",
        "full nested parent payload",
        vec![
            admitted("ORNA-E-NESTED-OLD-A", "old first tail payload"),
            admitted("ORNA-E-NESTED-OLD-B", "old final tail payload"),
        ],
    ));
    let empty_recovery = recover(&make_parent(
        "empty chain root payload",
        "empty chain branch payload",
        "empty nested parent payload",
        vec![],
    ));
    let replacement_recovery = recover(&make_parent(
        "replacement chain root payload",
        "replacement chain branch payload",
        "replacement nested parent payload",
        vec![admitted(
            "ORNA-E-NESTED-NEW",
            "replacement final tail payload",
        )],
    ));

    // ORNA-SECRET-002 requires recursive redaction, but is silent on closure
    // snapshots when a decoded empty inner parent is replaced by a new tail
    // generation and then the old parent chain is restored. Keep every stage
    // as an owned snapshot and let cause composition revoke root admission.
    let capture_full = capture(full_recovery.clone());
    let capture_empty = capture(empty_recovery.clone());
    let capture_replacement = capture(replacement_recovery.clone());
    let empty_readmission = empty_recovery
        .clone()
        .redacted_with_message(SafeText::new("empty nested root admission").unwrap());
    let capture_empty_readmission = capture(empty_readmission.clone());

    let mut receiver = full_recovery.clone();
    let capture_initial_full = capture(receiver.clone());
    receiver.clone_from(&capture_empty());
    let capture_empty_nested = capture(receiver.clone());
    receiver.clone_from(&capture_replacement());
    let capture_replaced_tail = capture(receiver.clone());
    receiver.clone_from(&capture_full());
    let capture_restored_full = capture(receiver.clone());
    receiver.clone_from(&capture_empty_readmission());
    let capture_admitted_empty = capture(receiver.clone());
    receiver.clone_from(&capture_replacement());
    let capture_final_replacement = capture(receiver.clone());

    assert_eq!(capture_initial_full(), full_recovery);
    assert_eq!(capture_empty_nested(), empty_recovery);
    assert_eq!(capture_replaced_tail(), replacement_recovery);
    assert_eq!(capture_restored_full(), full_recovery);
    assert_eq!(capture_admitted_empty(), empty_readmission);
    assert_eq!(capture_final_replacement(), replacement_recovery);
    let readmitted_projection = serde_json::to_value(capture_admitted_empty()).unwrap();
    assert_eq!(readmitted_projection["message"], "empty nested root admission");
    assert_eq!(cause_shape(&readmitted_projection), [1, 1, 0]);
    assert_redacted_tree(&readmitted_projection["causes"][0]);

    let compose_generations = {
        let capture_initial_full = capture_initial_full;
        let capture_empty_nested = capture_empty_nested;
        let capture_replaced_tail = capture_replaced_tail;
        let capture_restored_full = capture_restored_full;
        let capture_admitted_empty = capture_admitted_empty;
        let capture_final_replacement = capture_final_replacement;
        move || {
            admitted("ORNA-E-NESTED-RECOVERY-OUTER", "nested recovery outer admission")
                .with_cause(capture_initial_full())
                .with_cause(capture_empty_nested())
                .with_cause(capture_replaced_tail())
                .with_cause(capture_restored_full())
                .with_cause(capture_admitted_empty())
                .with_cause(capture_final_replacement())
        }
    };
    receiver.clone_from(&capture_empty());
    let outer = compose_generations();
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["message"], "nested recovery outer admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 6);
    for cause in causes {
        assert_redacted_tree(cause);
    }
    assert_eq!(
        causes.iter().map(cause_shape).collect::<Vec<_>>(),
        vec![
            vec![1, 1, 2, 0, 0],
            vec![1, 1, 0],
            vec![1, 1, 1, 0],
            vec![1, 1, 2, 0, 0],
            vec![1, 1, 0],
            vec![1, 1, 1, 0],
        ],
    );
    assert_eq!(
        causes.iter().map(deep_tail_codes).collect::<Vec<_>>(),
        vec![
            vec![
                "ORNA-E-NESTED-OLD-A".to_owned(),
                "ORNA-E-NESTED-OLD-B".to_owned(),
            ],
            vec![],
            vec!["ORNA-E-NESTED-NEW".to_owned()],
            vec![
                "ORNA-E-NESTED-OLD-A".to_owned(),
                "ORNA-E-NESTED-OLD-B".to_owned(),
            ],
            vec![],
            vec!["ORNA-E-NESTED-NEW".to_owned()],
        ],
    );

    let json = serde_json::to_vec(&outer).unwrap();
    let wire = outer.encode_ovb().unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"full chain root payload".as_slice(),
            b"full chain branch payload".as_slice(),
            b"full nested parent payload".as_slice(),
            b"old final tail payload".as_slice(),
            b"empty chain root payload".as_slice(),
            b"empty nested parent payload".as_slice(),
            b"replacement chain root payload".as_slice(),
            b"replacement nested parent payload".as_slice(),
            b"replacement final tail payload".as_slice(),
            b"empty nested root admission".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
        assert!(!wire.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&wire).unwrap()).unwrap();
    assert_redacted_tree(&decoded);
    assert_eq!(
        decoded["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(cause_shape)
            .collect::<Vec<_>>(),
        causes.iter().map(cause_shape).collect::<Vec<_>>(),
    );
}

#[test]
fn deep_parent_sibling_recovery_preserves_crossed_closure_edges() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn cause_shape(diagnostic: &serde_json::Value) -> Vec<usize> {
        let causes = diagnostic["causes"].as_array().unwrap();
        let mut shape = vec![causes.len()];
        for cause in causes {
            shape.extend(cause_shape(cause));
        }
        shape
    }
    fn deep_tail_codes(diagnostic: &serde_json::Value) -> Vec<String> {
        let mut terminal = diagnostic;
        for _ in 0..4 {
            terminal = &terminal["causes"][0];
        }
        terminal["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tail| tail["code"].as_str().unwrap().to_owned())
            .collect()
    }
    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let make_chain = |prefix: &str, messages: [&str; 5], tails: Vec<Diagnostic>| {
        let terminal = tails.into_iter().fold(
            admitted(&format!("{prefix}-TERMINAL"), messages[4]),
            |terminal, tail| terminal.with_cause(tail),
        );
        let nested = admitted(&format!("{prefix}-NESTED"), messages[3]).with_cause(terminal);
        let parent = admitted(&format!("{prefix}-PARENT"), messages[2]).with_cause(nested);
        let branch = admitted(&format!("{prefix}-BRANCH"), messages[1]).with_cause(parent);
        admitted(prefix, messages[0]).with_cause(branch)
    };
    let recover = |diagnostic: &Diagnostic| {
        Diagnostic::decode_ovb(&diagnostic.encode_ovb().unwrap()).unwrap()
    };
    let capture = |snapshot: Diagnostic| move || snapshot.clone();

    let old_recovery = recover(&make_chain(
        "ORNA-E-DEEP-OLD",
        [
            "deep old root payload",
            "deep old branch payload",
            "deep old parent payload",
            "deep old nested payload",
            "deep old terminal payload",
        ],
        vec![
            admitted("ORNA-E-DEEP-OLD-A", "deep old first tail payload"),
            admitted("ORNA-E-DEEP-OLD-B", "deep old final tail payload"),
        ],
    ));
    let new_recovery = recover(&make_chain(
        "ORNA-E-DEEP-NEW",
        [
            "deep new root payload",
            "deep new branch payload",
            "deep new parent payload",
            "deep new nested payload",
            "deep new terminal payload",
        ],
        vec![
            admitted("ORNA-E-DEEP-NEW-A", "deep new first tail payload"),
            admitted("ORNA-E-DEEP-NEW-B", "deep new final tail payload"),
        ],
    ));
    let empty_recovery = recover(&make_chain(
        "ORNA-E-DEEP-EMPTY",
        [
            "deep empty root payload",
            "deep empty branch payload",
            "deep empty parent payload",
            "deep empty nested payload",
            "deep empty terminal payload",
        ],
        vec![],
    ));

    // ORNA-SECRET-002 requires recursive diagnostic redaction but leaves
    // captured closures across deep sibling clone_from replacements unspecified.
    // Keep each decoded chain as an owned generation for its closure snapshot.
    let capture_old = capture(old_recovery.clone());
    let capture_new = capture(new_recovery.clone());
    let capture_empty = capture(empty_recovery.clone());
    let mut left = old_recovery.clone();
    let mut right = new_recovery.clone();
    let capture_left_old = capture(left.clone());
    let capture_right_new = capture(right.clone());

    left.clone_from(&capture_empty());
    right.clone_from(&capture_empty());
    let capture_left_empty = capture(left.clone());
    let capture_right_empty = capture(right.clone());

    left.clone_from(&capture_new());
    right.clone_from(&capture_old());
    let capture_left_crossed = capture(left.clone());
    let capture_right_crossed = capture(right.clone());

    left.clone_from(&capture_old());
    right.clone_from(&capture_new());
    let capture_left_restored = capture(left.clone());
    let capture_right_restored = capture(right.clone());

    left.clone_from(&capture_empty());
    let capture_left_empty_again = capture(left.clone());
    let capture_right_new_again = capture(right.clone());

    assert_eq!(capture_left_old(), old_recovery);
    assert_eq!(capture_right_new(), new_recovery);
    assert_eq!(capture_left_empty(), empty_recovery);
    assert_eq!(capture_right_empty(), empty_recovery);
    assert_eq!(capture_left_crossed(), new_recovery);
    assert_eq!(capture_right_crossed(), old_recovery);
    assert_eq!(capture_left_restored(), old_recovery);
    assert_eq!(capture_right_restored(), new_recovery);
    assert_eq!(capture_left_empty_again(), empty_recovery);
    assert_eq!(capture_right_new_again(), new_recovery);

    let compose_generations = {
        let capture_left_old = capture_left_old;
        let capture_right_new = capture_right_new;
        let capture_left_empty = capture_left_empty;
        let capture_right_empty = capture_right_empty;
        let capture_left_crossed = capture_left_crossed;
        let capture_right_crossed = capture_right_crossed;
        let capture_left_restored = capture_left_restored;
        let capture_right_restored = capture_right_restored;
        let capture_left_empty_again = capture_left_empty_again;
        let capture_right_new_again = capture_right_new_again;
        move || {
            admitted("ORNA-E-DEEP-OUTER", "deep sibling outer admission")
                .with_cause(capture_left_old())
                .with_cause(capture_right_new())
                .with_cause(capture_left_empty())
                .with_cause(capture_right_empty())
                .with_cause(capture_left_crossed())
                .with_cause(capture_right_crossed())
                .with_cause(capture_left_restored())
                .with_cause(capture_right_restored())
                .with_cause(capture_left_empty_again())
                .with_cause(capture_right_new_again())
        }
    };
    left.clone_from(&capture_empty());
    right.clone_from(&capture_empty());
    let outer = compose_generations();
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["message"], "deep sibling outer admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 10);
    let expected_causes = [
        old_recovery.clone(),
        new_recovery.clone(),
        empty_recovery.clone(),
        empty_recovery.clone(),
        new_recovery.clone(),
        old_recovery.clone(),
        old_recovery.clone(),
        new_recovery.clone(),
        empty_recovery.clone(),
        new_recovery.clone(),
    ]
    .into_iter()
    .map(|generation| serde_json::to_value(generation).unwrap())
    .collect::<Vec<_>>();
    for cause in causes {
        assert_redacted_tree(cause);
    }
    assert_eq!(
        causes.iter().map(cause_shape).collect::<Vec<_>>(),
        vec![
            vec![1, 1, 1, 1, 2, 0, 0],
            vec![1, 1, 1, 1, 2, 0, 0],
            vec![1, 1, 1, 1, 0],
            vec![1, 1, 1, 1, 0],
            vec![1, 1, 1, 1, 2, 0, 0],
            vec![1, 1, 1, 1, 2, 0, 0],
            vec![1, 1, 1, 1, 2, 0, 0],
            vec![1, 1, 1, 1, 2, 0, 0],
            vec![1, 1, 1, 1, 0],
            vec![1, 1, 1, 1, 2, 0, 0],
        ],
    );
    assert_eq!(causes, expected_causes.as_slice());

    assert_eq!(
        causes.iter().map(deep_tail_codes).collect::<Vec<_>>(),
        vec![
            vec![
                "ORNA-E-DEEP-OLD-A".to_owned(),
                "ORNA-E-DEEP-OLD-B".to_owned(),
            ],
            vec![
                "ORNA-E-DEEP-NEW-A".to_owned(),
                "ORNA-E-DEEP-NEW-B".to_owned(),
            ],
            vec![],
            vec![],
            vec![
                "ORNA-E-DEEP-NEW-A".to_owned(),
                "ORNA-E-DEEP-NEW-B".to_owned(),
            ],
            vec![
                "ORNA-E-DEEP-OLD-A".to_owned(),
                "ORNA-E-DEEP-OLD-B".to_owned(),
            ],
            vec![
                "ORNA-E-DEEP-OLD-A".to_owned(),
                "ORNA-E-DEEP-OLD-B".to_owned(),
            ],
            vec![
                "ORNA-E-DEEP-NEW-A".to_owned(),
                "ORNA-E-DEEP-NEW-B".to_owned(),
            ],
            vec![],
            vec![
                "ORNA-E-DEEP-NEW-A".to_owned(),
                "ORNA-E-DEEP-NEW-B".to_owned(),
            ],
        ],
    );

    let json = serde_json::to_vec(&outer).unwrap();
    let wire = outer.encode_ovb().unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"deep old root payload".as_slice(),
            b"deep old branch payload".as_slice(),
            b"deep old parent payload".as_slice(),
            b"deep old nested payload".as_slice(),
            b"deep old terminal payload".as_slice(),
            b"deep old final tail payload".as_slice(),
            b"deep new root payload".as_slice(),
            b"deep new branch payload".as_slice(),
            b"deep new parent payload".as_slice(),
            b"deep new nested payload".as_slice(),
            b"deep new terminal payload".as_slice(),
            b"deep new final tail payload".as_slice(),
            b"deep empty root payload".as_slice(),
            b"deep empty terminal payload".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
        assert!(!wire.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&wire).unwrap()).unwrap();
    assert_redacted_tree(&decoded);
    let decoded_causes = decoded["causes"].as_array().unwrap();
    assert_eq!(
        decoded_causes.iter().map(cause_shape).collect::<Vec<_>>(),
        causes.iter().map(cause_shape).collect::<Vec<_>>(),
    );
    assert_eq!(decoded_causes, causes);
    assert_eq!(
        decoded_causes
            .iter()
            .map(deep_tail_codes)
            .collect::<Vec<_>>(),
        causes.iter().map(deep_tail_codes).collect::<Vec<_>>(),
    );
}

#[test]
fn shared_deep_parent_codes_preserve_unequal_tail_recovery_generations() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn cause_shape(diagnostic: &serde_json::Value) -> Vec<usize> {
        let causes = diagnostic["causes"].as_array().unwrap();
        let mut shape = vec![causes.len()];
        for cause in causes {
            shape.extend(cause_shape(cause));
        }
        shape
    }
    fn deep_parent_codes(diagnostic: &serde_json::Value) -> Vec<String> {
        let mut parent = diagnostic;
        let mut codes = Vec::new();
        for depth in 0..5 {
            codes.push(parent["code"].as_str().unwrap().to_owned());
            if depth < 4 {
                parent = &parent["causes"][0];
            }
        }
        codes
    }
    fn deep_tail_codes(diagnostic: &serde_json::Value) -> Vec<String> {
        let mut terminal = diagnostic;
        for _ in 0..4 {
            terminal = &terminal["causes"][0];
        }
        terminal["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tail| tail["code"].as_str().unwrap().to_owned())
            .collect()
    }
    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let make_chain = |generation: &str, tails: Vec<Diagnostic>| {
        let message = format!("{generation} deep parent payload");
        let terminal = tails.into_iter().fold(
            admitted("ORNA-E-SHARED-DEEP-TERMINAL", &message),
            |terminal, tail| terminal.with_cause(tail),
        );
        let nested = admitted("ORNA-E-SHARED-DEEP-NESTED", &message).with_cause(terminal);
        let parent = admitted("ORNA-E-SHARED-DEEP-PARENT", &message).with_cause(nested);
        let branch = admitted("ORNA-E-SHARED-DEEP-BRANCH", &message).with_cause(parent);
        admitted("ORNA-E-SHARED-DEEP-ROOT", &message).with_cause(branch)
    };
    let recover = |diagnostic: &Diagnostic| {
        Diagnostic::decode_ovb(&diagnostic.encode_ovb().unwrap()).unwrap()
    };
    let capture = |snapshot: Diagnostic| move || snapshot.clone();

    let old_recovery = recover(&make_chain(
        "old generation",
        vec![admitted("ORNA-E-SHARED-OLD-TAIL", "old deep tail payload")],
    ));
    let new_recovery = recover(&make_chain(
        "new generation",
        vec![
            admitted("ORNA-E-SHARED-NEW-A", "new first deep tail payload"),
            admitted("ORNA-E-SHARED-NEW-B", "new middle deep tail payload"),
            admitted("ORNA-E-SHARED-NEW-C", "new final deep tail payload"),
        ],
    ));
    let empty_recovery = recover(&make_chain("empty generation", vec![]));

    // ORNA-SECRET-002 requires redaction but leaves captured closure ownership
    // unspecified when one deep parent-code path carries different tail lengths.
    // Preserve each decoded chain as the generation captured at that point.
    let capture_old = capture(old_recovery.clone());
    let capture_new = capture(new_recovery.clone());
    let capture_empty = capture(empty_recovery.clone());
    let mut left = old_recovery.clone();
    let mut right = new_recovery.clone();
    let capture_left_old = capture(left.clone());
    let capture_right_new = capture(right.clone());

    left.clone_from(&capture_empty());
    right.clone_from(&capture_empty());
    let capture_left_empty = capture(left.clone());
    let capture_right_empty = capture(right.clone());

    left.clone_from(&capture_new());
    right.clone_from(&capture_old());
    let capture_left_crossed = capture(left.clone());
    let capture_right_crossed = capture(right.clone());

    left.clone_from(&capture_old());
    right.clone_from(&capture_new());
    let capture_left_restored = capture(left.clone());
    let capture_right_restored = capture(right.clone());

    left.clone_from(&capture_empty());
    let capture_left_empty_again = capture(left.clone());
    let capture_right_new_again = capture(right.clone());

    assert_eq!(capture_left_old(), old_recovery);
    assert_eq!(capture_right_new(), new_recovery);
    assert_eq!(capture_left_empty(), empty_recovery);
    assert_eq!(capture_right_empty(), empty_recovery);
    assert_eq!(capture_left_crossed(), new_recovery);
    assert_eq!(capture_right_crossed(), old_recovery);
    assert_eq!(capture_left_restored(), old_recovery);
    assert_eq!(capture_right_restored(), new_recovery);
    assert_eq!(capture_left_empty_again(), empty_recovery);
    assert_eq!(capture_right_new_again(), new_recovery);

    let compose_generations = {
        let capture_left_old = capture_left_old;
        let capture_right_new = capture_right_new;
        let capture_left_empty = capture_left_empty;
        let capture_right_empty = capture_right_empty;
        let capture_left_crossed = capture_left_crossed;
        let capture_right_crossed = capture_right_crossed;
        let capture_left_restored = capture_left_restored;
        let capture_right_restored = capture_right_restored;
        let capture_left_empty_again = capture_left_empty_again;
        let capture_right_new_again = capture_right_new_again;
        move || {
            admitted("ORNA-E-SHARED-DEEP-OUTER", "shared deep outer admission")
                .with_cause(capture_left_old())
                .with_cause(capture_right_new())
                .with_cause(capture_left_empty())
                .with_cause(capture_right_empty())
                .with_cause(capture_left_crossed())
                .with_cause(capture_right_crossed())
                .with_cause(capture_left_restored())
                .with_cause(capture_right_restored())
                .with_cause(capture_left_empty_again())
                .with_cause(capture_right_new_again())
        }
    };
    left.clone_from(&capture_empty());
    right.clone_from(&capture_empty());
    let outer = compose_generations();
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["message"], "shared deep outer admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 10);
    for cause in causes {
        assert_redacted_tree(cause);
    }
    let expected_causes = [
        old_recovery.clone(),
        new_recovery.clone(),
        empty_recovery.clone(),
        empty_recovery.clone(),
        new_recovery.clone(),
        old_recovery.clone(),
        old_recovery.clone(),
        new_recovery.clone(),
        empty_recovery.clone(),
        new_recovery.clone(),
    ]
    .into_iter()
    .map(|generation| serde_json::to_value(generation).unwrap())
    .collect::<Vec<_>>();
    assert_eq!(causes, expected_causes.as_slice());
    assert!(causes.iter().all(|cause| {
        deep_parent_codes(cause)
            == vec![
                "ORNA-E-SHARED-DEEP-ROOT",
                "ORNA-E-SHARED-DEEP-BRANCH",
                "ORNA-E-SHARED-DEEP-PARENT",
                "ORNA-E-SHARED-DEEP-NESTED",
                "ORNA-E-SHARED-DEEP-TERMINAL",
            ]
    }));
    assert_eq!(
        causes.iter().map(cause_shape).collect::<Vec<_>>(),
        vec![
            vec![1, 1, 1, 1, 1, 0],
            vec![1, 1, 1, 1, 3, 0, 0, 0],
            vec![1, 1, 1, 1, 0],
            vec![1, 1, 1, 1, 0],
            vec![1, 1, 1, 1, 3, 0, 0, 0],
            vec![1, 1, 1, 1, 1, 0],
            vec![1, 1, 1, 1, 1, 0],
            vec![1, 1, 1, 1, 3, 0, 0, 0],
            vec![1, 1, 1, 1, 0],
            vec![1, 1, 1, 1, 3, 0, 0, 0],
        ],
    );
    assert_eq!(
        causes.iter().map(deep_tail_codes).collect::<Vec<_>>(),
        vec![
            vec!["ORNA-E-SHARED-OLD-TAIL".to_owned()],
            vec![
                "ORNA-E-SHARED-NEW-A".to_owned(),
                "ORNA-E-SHARED-NEW-B".to_owned(),
                "ORNA-E-SHARED-NEW-C".to_owned(),
            ],
            vec![],
            vec![],
            vec![
                "ORNA-E-SHARED-NEW-A".to_owned(),
                "ORNA-E-SHARED-NEW-B".to_owned(),
                "ORNA-E-SHARED-NEW-C".to_owned(),
            ],
            vec!["ORNA-E-SHARED-OLD-TAIL".to_owned()],
            vec!["ORNA-E-SHARED-OLD-TAIL".to_owned()],
            vec![
                "ORNA-E-SHARED-NEW-A".to_owned(),
                "ORNA-E-SHARED-NEW-B".to_owned(),
                "ORNA-E-SHARED-NEW-C".to_owned(),
            ],
            vec![],
            vec![
                "ORNA-E-SHARED-NEW-A".to_owned(),
                "ORNA-E-SHARED-NEW-B".to_owned(),
                "ORNA-E-SHARED-NEW-C".to_owned(),
            ],
        ],
    );

    let json = serde_json::to_vec(&outer).unwrap();
    let wire = outer.encode_ovb().unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"old generation deep parent payload".as_slice(),
            b"old deep tail payload".as_slice(),
            b"new generation deep parent payload".as_slice(),
            b"new first deep tail payload".as_slice(),
            b"new middle deep tail payload".as_slice(),
            b"new final deep tail payload".as_slice(),
            b"empty generation deep parent payload".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
        assert!(!wire.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&wire).unwrap()).unwrap();
    assert_redacted_tree(&decoded);
    let decoded_causes = decoded["causes"].as_array().unwrap();
    assert_eq!(
        decoded_causes.iter().map(cause_shape).collect::<Vec<_>>(),
        causes.iter().map(cause_shape).collect::<Vec<_>>(),
    );
    assert_eq!(decoded_causes, causes);
}

#[test]
fn reused_decoded_deep_recovery_tail_survives_parent_replacements() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn cause_shape(diagnostic: &serde_json::Value) -> Vec<usize> {
        let causes = diagnostic["causes"].as_array().unwrap();
        let mut shape = vec![causes.len()];
        for cause in causes {
            shape.extend(cause_shape(cause));
        }
        shape
    }
    fn terminal_parent(diagnostic: &serde_json::Value) -> &serde_json::Value {
        &diagnostic["causes"][0]["causes"][0]["causes"][0]["causes"][0]
    }
    fn deep_tail_codes(diagnostic: &serde_json::Value) -> Vec<String> {
        terminal_parent(diagnostic)["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tail| tail["code"].as_str().unwrap().to_owned())
            .collect()
    }
    fn reused_recovery(diagnostic: &serde_json::Value) -> Option<&serde_json::Value> {
        terminal_parent(diagnostic)["causes"].as_array().unwrap().first()
    }
    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let recover = |diagnostic: &Diagnostic| {
        Diagnostic::decode_ovb(&diagnostic.encode_ovb().unwrap()).unwrap()
    };
    let shared_source = admitted("ORNA-E-SHARED-RECOVERY", "shared recovery root payload")
        .with_cause(
            admitted("ORNA-E-SHARED-RECOVERY-BRANCH", "shared recovery branch payload")
                .with_cause(admitted(
                    "ORNA-E-SHARED-RECOVERY-A",
                    "shared recovery first tail payload",
                ))
                .with_cause(admitted(
                    "ORNA-E-SHARED-RECOVERY-B",
                    "shared recovery final tail payload",
                )),
        );
    let shared_recovery = recover(&shared_source);
    let shared_projection = serde_json::to_value(&shared_recovery).unwrap();
    let make_parent = |generation: &str, unique_tail: Option<(&str, &str)>| {
        let parent_message = format!("{generation} deep parent payload");
        let terminal = match unique_tail {
            Some((code, message)) => admitted("ORNA-E-REUSED-DEEP-TERMINAL", &parent_message)
                .with_cause(shared_recovery.clone())
                .with_cause(admitted(code, message)),
            None => admitted("ORNA-E-REUSED-DEEP-TERMINAL", &parent_message),
        };
        let nested = admitted("ORNA-E-REUSED-DEEP-NESTED", &parent_message).with_cause(terminal);
        let parent = admitted("ORNA-E-REUSED-DEEP-PARENT", &parent_message).with_cause(nested);
        let branch = admitted("ORNA-E-REUSED-DEEP-BRANCH", &parent_message).with_cause(parent);
        admitted("ORNA-E-REUSED-DEEP-ROOT", &parent_message).with_cause(branch)
    };
    let old_recovery = recover(&make_parent(
        "old shared",
        Some(("ORNA-E-REUSED-OLD-TAIL", "old unique tail payload")),
    ));
    let new_recovery = recover(&make_parent(
        "new shared",
        Some(("ORNA-E-REUSED-NEW-TAIL", "new unique tail payload")),
    ));
    let empty_recovery = recover(&make_parent("empty shared", None));
    let capture = |snapshot: Diagnostic| move || snapshot.clone();

    // ORNA-SECRET-002 requires recursive redaction but leaves reuse of one
    // decoded deep subtree across sibling parent generations unspecified. Each
    // closure retains the recovered subtree with the parent generation captured.
    let capture_old = capture(old_recovery.clone());
    let capture_new = capture(new_recovery.clone());
    let capture_empty = capture(empty_recovery.clone());
    let mut left = old_recovery.clone();
    let mut right = new_recovery.clone();
    let capture_left_old = capture(left.clone());
    let capture_right_new = capture(right.clone());

    left.clone_from(&capture_empty());
    right.clone_from(&capture_empty());
    let capture_left_empty = capture(left.clone());
    let capture_right_empty = capture(right.clone());

    left.clone_from(&capture_new());
    right.clone_from(&capture_old());
    let capture_left_crossed = capture(left.clone());
    let capture_right_crossed = capture(right.clone());

    left.clone_from(&capture_old());
    right.clone_from(&capture_new());
    let capture_left_restored = capture(left.clone());
    let capture_right_restored = capture(right.clone());

    left.clone_from(&capture_empty());
    let capture_left_empty_again = capture(left.clone());
    let capture_right_new_again = capture(right.clone());

    assert_eq!(capture_left_old(), old_recovery);
    assert_eq!(capture_right_new(), new_recovery);
    assert_eq!(capture_left_empty(), empty_recovery);
    assert_eq!(capture_right_empty(), empty_recovery);
    assert_eq!(capture_left_crossed(), new_recovery);
    assert_eq!(capture_right_crossed(), old_recovery);
    assert_eq!(capture_left_restored(), old_recovery);
    assert_eq!(capture_right_restored(), new_recovery);
    assert_eq!(capture_left_empty_again(), empty_recovery);
    assert_eq!(capture_right_new_again(), new_recovery);

    let compose_generations = {
        let capture_left_old = capture_left_old;
        let capture_right_new = capture_right_new;
        let capture_left_empty = capture_left_empty;
        let capture_right_empty = capture_right_empty;
        let capture_left_crossed = capture_left_crossed;
        let capture_right_crossed = capture_right_crossed;
        let capture_left_restored = capture_left_restored;
        let capture_right_restored = capture_right_restored;
        let capture_left_empty_again = capture_left_empty_again;
        let capture_right_new_again = capture_right_new_again;
        move || {
            admitted("ORNA-E-REUSED-DEEP-OUTER", "reused deep outer admission")
                .with_cause(capture_left_old())
                .with_cause(capture_right_new())
                .with_cause(capture_left_empty())
                .with_cause(capture_right_empty())
                .with_cause(capture_left_crossed())
                .with_cause(capture_right_crossed())
                .with_cause(capture_left_restored())
                .with_cause(capture_right_restored())
                .with_cause(capture_left_empty_again())
                .with_cause(capture_right_new_again())
        }
    };
    left.clone_from(&capture_empty());
    right.clone_from(&capture_empty());
    let outer = compose_generations();
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["message"], "reused deep outer admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 10);
    for cause in causes {
        assert_redacted_tree(cause);
    }
    let expected_causes = [
        old_recovery.clone(),
        new_recovery.clone(),
        empty_recovery.clone(),
        empty_recovery.clone(),
        new_recovery.clone(),
        old_recovery.clone(),
        old_recovery.clone(),
        new_recovery.clone(),
        empty_recovery.clone(),
        new_recovery.clone(),
    ]
    .into_iter()
    .map(|generation| serde_json::to_value(generation).unwrap())
    .collect::<Vec<_>>();
    assert_eq!(causes, expected_causes.as_slice());
    for cause in causes.iter().filter(|cause| !cause["causes"].is_null()) {
        if let Some(reused) = reused_recovery(cause) {
            assert_eq!(reused, &shared_projection);
        }
    }
    assert_eq!(
        causes.iter().map(cause_shape).collect::<Vec<_>>(),
        vec![
            vec![1, 1, 1, 1, 2, 1, 2, 0, 0, 0],
            vec![1, 1, 1, 1, 2, 1, 2, 0, 0, 0],
            vec![1, 1, 1, 1, 0],
            vec![1, 1, 1, 1, 0],
            vec![1, 1, 1, 1, 2, 1, 2, 0, 0, 0],
            vec![1, 1, 1, 1, 2, 1, 2, 0, 0, 0],
            vec![1, 1, 1, 1, 2, 1, 2, 0, 0, 0],
            vec![1, 1, 1, 1, 2, 1, 2, 0, 0, 0],
            vec![1, 1, 1, 1, 0],
            vec![1, 1, 1, 1, 2, 1, 2, 0, 0, 0],
        ],
    );
    assert_eq!(
        causes.iter().map(deep_tail_codes).collect::<Vec<_>>(),
        vec![
            vec![
                "ORNA-E-SHARED-RECOVERY".to_owned(),
                "ORNA-E-REUSED-OLD-TAIL".to_owned(),
            ],
            vec![
                "ORNA-E-SHARED-RECOVERY".to_owned(),
                "ORNA-E-REUSED-NEW-TAIL".to_owned(),
            ],
            vec![],
            vec![],
            vec![
                "ORNA-E-SHARED-RECOVERY".to_owned(),
                "ORNA-E-REUSED-NEW-TAIL".to_owned(),
            ],
            vec![
                "ORNA-E-SHARED-RECOVERY".to_owned(),
                "ORNA-E-REUSED-OLD-TAIL".to_owned(),
            ],
            vec![
                "ORNA-E-SHARED-RECOVERY".to_owned(),
                "ORNA-E-REUSED-OLD-TAIL".to_owned(),
            ],
            vec![
                "ORNA-E-SHARED-RECOVERY".to_owned(),
                "ORNA-E-REUSED-NEW-TAIL".to_owned(),
            ],
            vec![],
            vec![
                "ORNA-E-SHARED-RECOVERY".to_owned(),
                "ORNA-E-REUSED-NEW-TAIL".to_owned(),
            ],
        ],
    );

    let json = serde_json::to_vec(&outer).unwrap();
    let wire = outer.encode_ovb().unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"shared recovery root payload".as_slice(),
            b"shared recovery branch payload".as_slice(),
            b"shared recovery first tail payload".as_slice(),
            b"shared recovery final tail payload".as_slice(),
            b"old shared deep parent payload".as_slice(),
            b"old unique tail payload".as_slice(),
            b"new shared deep parent payload".as_slice(),
            b"new unique tail payload".as_slice(),
            b"empty shared deep parent payload".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
        assert!(!wire.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&wire).unwrap()).unwrap();
    assert_redacted_tree(&decoded);
    let decoded_causes = decoded["causes"].as_array().unwrap();
    assert_eq!(decoded_causes, causes);
}

#[test]
fn duplicate_reused_deep_tail_edges_survive_sibling_recovery() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn cause_shape(diagnostic: &serde_json::Value) -> Vec<usize> {
        let causes = diagnostic["causes"].as_array().unwrap();
        let mut shape = vec![causes.len()];
        for cause in causes {
            shape.extend(cause_shape(cause));
        }
        shape
    }
    fn terminal_parent(diagnostic: &serde_json::Value) -> &serde_json::Value {
        &diagnostic["causes"][0]["causes"][0]["causes"][0]["causes"][0]
    }
    fn deep_tail_children(diagnostic: &serde_json::Value) -> Vec<&serde_json::Value> {
        terminal_parent(diagnostic)["causes"]
            .as_array()
            .unwrap()
            .iter()
            .collect()
    }
    fn deep_tail_codes(diagnostic: &serde_json::Value) -> Vec<String> {
        deep_tail_children(diagnostic)
            .into_iter()
            .map(|tail| tail["code"].as_str().unwrap().to_owned())
            .collect()
    }
    fn reused_edges(diagnostic: &serde_json::Value) -> Vec<&serde_json::Value> {
        deep_tail_children(diagnostic)
            .into_iter()
            .filter(|tail| tail["code"] == "ORNA-E-DUPLICATE-SHARED")
            .collect()
    }
    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let recover = |diagnostic: &Diagnostic| {
        Diagnostic::decode_ovb(&diagnostic.encode_ovb().unwrap()).unwrap()
    };
    let shared_source = admitted(
        "ORNA-E-DUPLICATE-SHARED",
        "duplicate shared root payload",
    )
    .with_cause(
        admitted(
            "ORNA-E-DUPLICATE-SHARED-BRANCH",
            "duplicate shared branch payload",
        )
        .with_cause(admitted(
            "ORNA-E-DUPLICATE-SHARED-A",
            "duplicate shared first tail payload",
        ))
        .with_cause(admitted(
            "ORNA-E-DUPLICATE-SHARED-B",
            "duplicate shared final tail payload",
        )),
    );
    let shared_recovery = recover(&shared_source);
    let shared_projection = serde_json::to_value(&shared_recovery).unwrap();
    let make_parent = |generation: &str, middle_code: &str, middle_message: &str| {
        let parent_message = format!("{generation} repeated parent payload");
        let terminal = admitted("ORNA-E-DUPLICATE-DEEP-TERMINAL", &parent_message)
            .with_cause(shared_recovery.clone())
            .with_cause(admitted(middle_code, middle_message))
            .with_cause(shared_recovery.clone());
        let nested = admitted("ORNA-E-DUPLICATE-DEEP-NESTED", &parent_message).with_cause(terminal);
        let parent = admitted("ORNA-E-DUPLICATE-DEEP-PARENT", &parent_message).with_cause(nested);
        let branch = admitted("ORNA-E-DUPLICATE-DEEP-BRANCH", &parent_message).with_cause(parent);
        admitted("ORNA-E-DUPLICATE-DEEP-ROOT", &parent_message).with_cause(branch)
    };
    let old_recovery = recover(&make_parent(
        "old generation",
        "ORNA-E-DUPLICATE-OLD-MIDDLE",
        "old middle tail payload",
    ));
    let new_recovery = recover(&make_parent(
        "new generation",
        "ORNA-E-DUPLICATE-NEW-MIDDLE",
        "new middle tail payload",
    ));
    let empty_message = "empty generation repeated parent payload";
    let empty_terminal = admitted("ORNA-E-DUPLICATE-DEEP-TERMINAL", empty_message);
    let empty_nested = admitted("ORNA-E-DUPLICATE-DEEP-NESTED", empty_message)
        .with_cause(empty_terminal);
    let empty_parent = admitted("ORNA-E-DUPLICATE-DEEP-PARENT", empty_message)
        .with_cause(empty_nested);
    let empty_branch = admitted("ORNA-E-DUPLICATE-DEEP-BRANCH", empty_message)
        .with_cause(empty_parent);
    let empty_source = admitted("ORNA-E-DUPLICATE-DEEP-ROOT", empty_message)
        .with_cause(empty_branch);
    let empty_recovery = recover(&empty_source);
    let capture = |snapshot: Diagnostic| move || snapshot.clone();

    // ORNA-SECRET-002 requires recursive redaction but does not specify how
    // captured generations retain a reused subtree duplicated at both ends of
    // a deep cause vector. Keep each recovered value as the closure snapshot.
    let capture_old = capture(old_recovery.clone());
    let capture_new = capture(new_recovery.clone());
    let capture_empty = capture(empty_recovery.clone());
    let mut left = old_recovery.clone();
    let mut right = new_recovery.clone();
    let capture_left_old = capture(left.clone());
    let capture_right_new = capture(right.clone());

    left.clone_from(&capture_empty());
    right.clone_from(&capture_empty());
    let capture_left_empty = capture(left.clone());
    let capture_right_empty = capture(right.clone());
    left.clone_from(&capture_new());
    right.clone_from(&capture_old());
    let capture_left_crossed = capture(left.clone());
    let capture_right_crossed = capture(right.clone());
    left.clone_from(&capture_old());
    right.clone_from(&capture_new());
    let capture_left_restored = capture(left.clone());
    let capture_right_restored = capture(right.clone());
    left.clone_from(&capture_empty());
    let capture_left_empty_again = capture(left.clone());
    let capture_right_new_again = capture(right.clone());

    assert_eq!(capture_left_old(), old_recovery);
    assert_eq!(capture_right_new(), new_recovery);
    assert_eq!(capture_left_empty(), empty_recovery);
    assert_eq!(capture_right_empty(), empty_recovery);
    assert_eq!(capture_left_crossed(), new_recovery);
    assert_eq!(capture_right_crossed(), old_recovery);
    assert_eq!(capture_left_restored(), old_recovery);
    assert_eq!(capture_right_restored(), new_recovery);
    assert_eq!(capture_left_empty_again(), empty_recovery);
    assert_eq!(capture_right_new_again(), new_recovery);

    let compose_generations = {
        let capture_left_old = capture_left_old;
        let capture_right_new = capture_right_new;
        let capture_left_empty = capture_left_empty;
        let capture_right_empty = capture_right_empty;
        let capture_left_crossed = capture_left_crossed;
        let capture_right_crossed = capture_right_crossed;
        let capture_left_restored = capture_left_restored;
        let capture_right_restored = capture_right_restored;
        let capture_left_empty_again = capture_left_empty_again;
        let capture_right_new_again = capture_right_new_again;
        move || {
            admitted("ORNA-E-DUPLICATE-OUTER", "duplicate recovery outer admission")
                .with_cause(capture_left_old())
                .with_cause(capture_right_new())
                .with_cause(capture_left_empty())
                .with_cause(capture_right_empty())
                .with_cause(capture_left_crossed())
                .with_cause(capture_right_crossed())
                .with_cause(capture_left_restored())
                .with_cause(capture_right_restored())
                .with_cause(capture_left_empty_again())
                .with_cause(capture_right_new_again())
        }
    };
    left.clone_from(&capture_empty());
    right.clone_from(&capture_empty());
    let outer = compose_generations();
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["message"], "duplicate recovery outer admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 10);
    for cause in causes {
        assert_redacted_tree(cause);
    }
    let expected_causes = [
        old_recovery.clone(),
        new_recovery.clone(),
        empty_recovery.clone(),
        empty_recovery.clone(),
        new_recovery.clone(),
        old_recovery.clone(),
        old_recovery.clone(),
        new_recovery.clone(),
        empty_recovery.clone(),
        new_recovery.clone(),
    ]
    .into_iter()
    .map(|generation| serde_json::to_value(generation).unwrap())
    .collect::<Vec<_>>();
    assert_eq!(causes, expected_causes.as_slice());
    for cause in causes {
        let duplicates = reused_edges(cause);
        if deep_tail_codes(cause).is_empty() {
            assert!(duplicates.is_empty());
        } else {
            assert_eq!(duplicates.len(), 2);
            assert_eq!(duplicates[0], &shared_projection);
            assert_eq!(duplicates[1], &shared_projection);
        }
    }
    let old_shape = vec![1, 1, 1, 1, 3, 1, 2, 0, 0, 0, 1, 2, 0, 0];
    let empty_shape = vec![1, 1, 1, 1, 0];
    assert_eq!(cause_shape(&causes[0]), old_shape);
    assert_eq!(cause_shape(&causes[1]), old_shape);
    assert_eq!(cause_shape(&causes[2]), empty_shape);
    assert_eq!(cause_shape(&causes[3]), empty_shape);
    assert_eq!(cause_shape(&causes[4]), old_shape);
    assert_eq!(cause_shape(&causes[5]), old_shape);
    assert_eq!(cause_shape(&causes[6]), old_shape);
    assert_eq!(cause_shape(&causes[7]), old_shape);
    assert_eq!(cause_shape(&causes[8]), empty_shape);
    assert_eq!(cause_shape(&causes[9]), old_shape);
    assert_eq!(
        causes.iter().map(deep_tail_codes).collect::<Vec<_>>(),
        vec![
            vec![
                "ORNA-E-DUPLICATE-SHARED".to_owned(),
                "ORNA-E-DUPLICATE-OLD-MIDDLE".to_owned(),
                "ORNA-E-DUPLICATE-SHARED".to_owned(),
            ],
            vec![
                "ORNA-E-DUPLICATE-SHARED".to_owned(),
                "ORNA-E-DUPLICATE-NEW-MIDDLE".to_owned(),
                "ORNA-E-DUPLICATE-SHARED".to_owned(),
            ],
            vec![],
            vec![],
            vec![
                "ORNA-E-DUPLICATE-SHARED".to_owned(),
                "ORNA-E-DUPLICATE-NEW-MIDDLE".to_owned(),
                "ORNA-E-DUPLICATE-SHARED".to_owned(),
            ],
            vec![
                "ORNA-E-DUPLICATE-SHARED".to_owned(),
                "ORNA-E-DUPLICATE-OLD-MIDDLE".to_owned(),
                "ORNA-E-DUPLICATE-SHARED".to_owned(),
            ],
            vec![
                "ORNA-E-DUPLICATE-SHARED".to_owned(),
                "ORNA-E-DUPLICATE-OLD-MIDDLE".to_owned(),
                "ORNA-E-DUPLICATE-SHARED".to_owned(),
            ],
            vec![
                "ORNA-E-DUPLICATE-SHARED".to_owned(),
                "ORNA-E-DUPLICATE-NEW-MIDDLE".to_owned(),
                "ORNA-E-DUPLICATE-SHARED".to_owned(),
            ],
            vec![],
            vec![
                "ORNA-E-DUPLICATE-SHARED".to_owned(),
                "ORNA-E-DUPLICATE-NEW-MIDDLE".to_owned(),
                "ORNA-E-DUPLICATE-SHARED".to_owned(),
            ],
        ],
    );

    let json = serde_json::to_vec(&outer).unwrap();
    let wire = outer.encode_ovb().unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"duplicate shared root payload".as_slice(),
            b"duplicate shared branch payload".as_slice(),
            b"duplicate shared first tail payload".as_slice(),
            b"duplicate shared final tail payload".as_slice(),
            b"old generation repeated parent payload".as_slice(),
            b"old middle tail payload".as_slice(),
            b"new generation repeated parent payload".as_slice(),
            b"new middle tail payload".as_slice(),
            empty_message.as_bytes(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
        assert!(!wire.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&wire).unwrap()).unwrap();
    assert_redacted_tree(&decoded);
    let decoded_causes = decoded["causes"].as_array().unwrap();
    assert_eq!(decoded_causes, causes);
}

#[test]
fn duplicate_closures_keep_same_deep_recovery_across_parent_replacements() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn cause_shape(diagnostic: &serde_json::Value) -> Vec<usize> {
        let causes = diagnostic["causes"].as_array().unwrap();
        let mut shape = vec![causes.len()];
        for cause in causes {
            shape.extend(cause_shape(cause));
        }
        shape
    }
    fn deep_tail_codes(diagnostic: &serde_json::Value) -> Vec<String> {
        let mut terminal = diagnostic;
        for _ in 0..4 {
            terminal = &terminal["causes"][0];
        }
        terminal["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tail| tail["code"].as_str().unwrap().to_owned())
            .collect()
    }
    fn deep_parent_codes(diagnostic: &serde_json::Value) -> Vec<String> {
        let mut parent = diagnostic;
        let mut codes = Vec::new();
        for depth in 0..5 {
            codes.push(parent["code"].as_str().unwrap().to_owned());
            if depth < 4 {
                parent = &parent["causes"][0];
            }
        }
        codes
    }
    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let make_chain = |generation: &str, tails: Vec<Diagnostic>| {
        let message = format!("{generation} deep recovery payload");
        let terminal = tails.into_iter().fold(
            admitted("ORNA-E-DUPLICATE-DEEP-TERMINAL", &message),
            |terminal, tail| terminal.with_cause(tail),
        );
        let nested = admitted("ORNA-E-DUPLICATE-DEEP-NESTED", &message).with_cause(terminal);
        let parent = admitted("ORNA-E-DUPLICATE-DEEP-PARENT", &message).with_cause(nested);
        let branch = admitted("ORNA-E-DUPLICATE-DEEP-BRANCH", &message).with_cause(parent);
        admitted("ORNA-E-DUPLICATE-DEEP-ROOT", &message).with_cause(branch)
    };
    let recover = |diagnostic: &Diagnostic| {
        Diagnostic::decode_ovb(&diagnostic.encode_ovb().unwrap()).unwrap()
    };
    let capture = |snapshot: Diagnostic| move || snapshot.clone();

    let base_wire = make_chain(
        "base generation",
        vec![
            admitted("ORNA-E-DUPLICATE-BASE-A", "base first deep tail payload"),
            admitted("ORNA-E-DUPLICATE-BASE-B", "base final deep tail payload"),
        ],
    )
    .encode_ovb()
    .unwrap();
    let base_recovery = Diagnostic::decode_ovb(&base_wire).unwrap();
    let duplicate_recovery = Diagnostic::decode_ovb(&base_wire).unwrap();
    assert_eq!(base_recovery, duplicate_recovery);
    let replacement_recovery = recover(&make_chain(
        "replacement generation",
        vec![admitted(
            "ORNA-E-DUPLICATE-REPLACEMENT",
            "replacement deep tail payload",
        )],
    ));
    let empty_recovery = recover(&make_chain("empty generation", vec![]));

    // ORNA-SECRET-002 defines recursive redaction but does not specify whether
    // two independent decodes of the same deep wire snapshot retain duplicate
    // generations independently while their receivers cross replacements.
    let capture_base = capture(base_recovery.clone());
    let capture_duplicate_base = capture(duplicate_recovery.clone());
    let capture_replacement = capture(replacement_recovery.clone());
    let capture_empty = capture(empty_recovery.clone());
    let mut left = base_recovery.clone();
    let mut right = duplicate_recovery.clone();
    let capture_left_initial = capture(left.clone());
    let capture_right_initial = capture(right.clone());

    left.clone_from(&capture_empty());
    right.clone_from(&capture_replacement());
    let capture_left_empty = capture(left.clone());
    let capture_right_replacement = capture(right.clone());

    left.clone_from(&capture_replacement());
    right.clone_from(&capture_duplicate_base());
    let capture_left_replacement = capture(left.clone());
    let capture_right_restored = capture(right.clone());

    left.clone_from(&capture_base());
    right.clone_from(&capture_empty());
    let capture_left_restored = capture(left.clone());
    let capture_right_empty = capture(right.clone());

    assert_eq!(capture_left_initial(), base_recovery);
    assert_eq!(capture_right_initial(), duplicate_recovery);
    assert_eq!(capture_left_empty(), empty_recovery);
    assert_eq!(capture_right_replacement(), replacement_recovery);
    assert_eq!(capture_left_replacement(), replacement_recovery);
    assert_eq!(capture_right_restored(), duplicate_recovery);
    assert_eq!(capture_left_restored(), base_recovery);
    assert_eq!(capture_right_empty(), empty_recovery);

    let compose_generations = {
        let capture_left_initial = capture_left_initial;
        let capture_right_initial = capture_right_initial;
        let capture_left_empty = capture_left_empty;
        let capture_right_replacement = capture_right_replacement;
        let capture_left_replacement = capture_left_replacement;
        let capture_right_restored = capture_right_restored;
        let capture_left_restored = capture_left_restored;
        let capture_right_empty = capture_right_empty;
        move || {
            admitted("ORNA-E-DUPLICATE-DEEP-OUTER", "duplicate closure outer admission")
                .with_cause(capture_left_initial())
                .with_cause(capture_right_initial())
                .with_cause(capture_left_empty())
                .with_cause(capture_right_replacement())
                .with_cause(capture_left_replacement())
                .with_cause(capture_right_restored())
                .with_cause(capture_left_restored())
                .with_cause(capture_right_empty())
        }
    };
    left.clone_from(&capture_empty());
    right.clone_from(&capture_empty());
    let outer = compose_generations();
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["message"], "duplicate closure outer admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 8);
    for cause in causes {
        assert_redacted_tree(cause);
    }
    let expected_causes = [
        base_recovery.clone(),
        base_recovery.clone(),
        empty_recovery.clone(),
        replacement_recovery.clone(),
        replacement_recovery.clone(),
        base_recovery.clone(),
        base_recovery.clone(),
        empty_recovery.clone(),
    ]
    .into_iter()
    .map(|generation| serde_json::to_value(generation).unwrap())
    .collect::<Vec<_>>();
    assert_eq!(causes, expected_causes.as_slice());
    assert_eq!(causes[0], causes[1]);
    assert_eq!(causes[5], causes[6]);
    assert_eq!(
        causes.iter().map(deep_parent_codes).collect::<Vec<_>>(),
        vec![
            vec![
                "ORNA-E-DUPLICATE-DEEP-ROOT".to_owned(),
                "ORNA-E-DUPLICATE-DEEP-BRANCH".to_owned(),
                "ORNA-E-DUPLICATE-DEEP-PARENT".to_owned(),
                "ORNA-E-DUPLICATE-DEEP-NESTED".to_owned(),
                "ORNA-E-DUPLICATE-DEEP-TERMINAL".to_owned(),
            ];
            8
        ],
    );
    assert_eq!(
        causes.iter().map(cause_shape).collect::<Vec<_>>(),
        vec![
            vec![1, 1, 1, 1, 2, 0, 0],
            vec![1, 1, 1, 1, 2, 0, 0],
            vec![1, 1, 1, 1, 0],
            vec![1, 1, 1, 1, 1, 0],
            vec![1, 1, 1, 1, 1, 0],
            vec![1, 1, 1, 1, 2, 0, 0],
            vec![1, 1, 1, 1, 2, 0, 0],
            vec![1, 1, 1, 1, 0],
        ],
    );
    assert_eq!(
        causes.iter().map(deep_tail_codes).collect::<Vec<_>>(),
        vec![
            vec![
                "ORNA-E-DUPLICATE-BASE-A".to_owned(),
                "ORNA-E-DUPLICATE-BASE-B".to_owned(),
            ],
            vec![
                "ORNA-E-DUPLICATE-BASE-A".to_owned(),
                "ORNA-E-DUPLICATE-BASE-B".to_owned(),
            ],
            vec![],
            vec!["ORNA-E-DUPLICATE-REPLACEMENT".to_owned()],
            vec!["ORNA-E-DUPLICATE-REPLACEMENT".to_owned()],
            vec![
                "ORNA-E-DUPLICATE-BASE-A".to_owned(),
                "ORNA-E-DUPLICATE-BASE-B".to_owned(),
            ],
            vec![
                "ORNA-E-DUPLICATE-BASE-A".to_owned(),
                "ORNA-E-DUPLICATE-BASE-B".to_owned(),
            ],
            vec![],
        ],
    );

    let json = serde_json::to_vec(&outer).unwrap();
    let wire = outer.encode_ovb().unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"base generation deep recovery payload".as_slice(),
            b"base first deep tail payload".as_slice(),
            b"base final deep tail payload".as_slice(),
            b"replacement generation deep recovery payload".as_slice(),
            b"replacement deep tail payload".as_slice(),
            b"empty generation deep recovery payload".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
        assert!(!wire.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&wire).unwrap()).unwrap();
    assert_redacted_tree(&decoded);
    let decoded_causes = decoded["causes"].as_array().unwrap();
    assert_eq!(decoded_causes, causes);
}


#[test]
fn duplicate_deep_recoveries_redact_local_snapshot_admission() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn cause_shape(diagnostic: &serde_json::Value) -> Vec<usize> {
        let causes = diagnostic["causes"].as_array().unwrap();
        let mut shape = vec![causes.len()];
        for cause in causes {
            shape.extend(cause_shape(cause));
        }
        shape
    }

    let payload = "duplicate deep snapshot payload";
    let terminal = admitted("ORNA-E-TAKE3-TERMINAL", payload)
        .with_cause(admitted("ORNA-E-TAKE3-TAIL-A", "first deep tail secret"))
        .with_cause(admitted("ORNA-E-TAKE3-TAIL-B", "second deep tail secret"));
    let nested = admitted("ORNA-E-TAKE3-NESTED", payload).with_cause(terminal);
    let parent = admitted("ORNA-E-TAKE3-PARENT", payload).with_cause(nested);
    let branch = admitted("ORNA-E-TAKE3-BRANCH", payload).with_cause(parent);
    let root = admitted("ORNA-E-TAKE3-ROOT", payload).with_cause(branch);
    let wire = root.encode_ovb().unwrap();
    let first_recovery = Diagnostic::decode_ovb(&wire).unwrap();
    let duplicate_recovery = Diagnostic::decode_ovb(&wire).unwrap();
    assert_eq!(first_recovery, duplicate_recovery);

    // ORNA-SECRET-002 requires recursive cause redaction but does not specify
    // how a locally re-admitted recovered snapshot behaves beside its duplicate.
    let local_snapshot = first_recovery
        .redacted_with_message(SafeText::new("local snapshot admission").unwrap());
    assert_eq!(serde_json::to_value(&local_snapshot).unwrap()["message"], "local snapshot admission");
    let outer = admitted("ORNA-E-TAKE3-OUTER", "outer snapshot admission")
        .with_cause(local_snapshot)
        .with_cause(duplicate_recovery);
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["message"], "outer snapshot admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 2);
    assert_eq!(causes[0], causes[1]);
    assert_eq!(cause_shape(&causes[0]), [1, 1, 1, 1, 2, 0, 0]);
    for cause in causes {
        assert_redacted_tree(cause);
        let terminal = &cause["causes"][0]["causes"][0]["causes"][0]["causes"][0];
        assert_eq!(terminal["causes"][0]["code"], "ORNA-E-TAKE3-TAIL-A");
        assert_eq!(terminal["causes"][1]["code"], "ORNA-E-TAKE3-TAIL-B");
    }

    let json = serde_json::to_vec(&outer).unwrap();
    let encoded = outer.encode_ovb().unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            payload.as_bytes(),
            b"first deep tail secret".as_slice(),
            b"second deep tail secret".as_slice(),
            b"local snapshot admission".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
        assert!(!encoded.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&encoded).unwrap()).unwrap();
    assert_redacted_tree(&decoded);
    assert_eq!(decoded["causes"], projection["causes"]);
}

#[test]
fn empty_duplicate_sibling_counts_follow_order_with_duplicate_terminals() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let make_wire = |payload: &str, counts: [usize; 2], reverse: bool| {
        let sibling = |label: &str, count: usize| {
            let terminal = (0..count).fold(
                admitted("ORNA-E-EMPTY-DUP-IDENTICAL-TERMINAL", payload),
                |terminal, index| {
                    terminal.with_cause(admitted(
                        "ORNA-E-EMPTY-DUP-IDENTICAL-TAIL",
                        &format!("{payload} {label} duplicate tail {index} secret"),
                    ))
                },
            );
            admitted(
                "ORNA-E-EMPTY-DUP-IDENTICAL-SIBLING",
                &format!("{payload} {label} sibling secret"),
            )
            .with_cause(terminal)
        };
        let left = sibling("LEFT", counts[0]);
        let right = sibling("RIGHT", counts[1]);
        let (first, second) = if reverse {
            (right, left)
        } else {
            (left, right)
        };
        admitted("ORNA-E-EMPTY-DUP-IDENTICAL-ROOT", payload)
            .with_cause(first)
            .with_cause(second)
            .encode_ovb()
            .unwrap()
    };
    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn sibling_tail_counts(diagnostic: &serde_json::Value) -> Vec<usize> {
        let siblings = diagnostic["causes"].as_array().unwrap();
        assert_eq!(siblings.len(), 2);
        siblings
            .iter()
            .map(|sibling| {
                assert_eq!(sibling["code"], "ORNA-E-EMPTY-DUP-IDENTICAL-SIBLING");
                let terminal = &sibling["causes"][0];
                assert_eq!(terminal["code"], "ORNA-E-EMPTY-DUP-IDENTICAL-TERMINAL");
                let tails = terminal["causes"].as_array().unwrap();
                for tail in tails {
                    assert_eq!(tail["code"], "ORNA-E-EMPTY-DUP-IDENTICAL-TAIL");
                    assert!(tail["causes"].as_array().unwrap().is_empty());
                }
                tails.len()
            })
            .collect()
    }

    let forward_wire = make_wire("forward identical empty duplicate payload", [2, 0], false);
    let reverse_wire = make_wire("reverse identical empty duplicate payload", [2, 0], true);
    let empty_wire = make_wire("empty identical duplicate payload", [0, 0], false);
    let replay_forward = || Diagnostic::decode_ovb(&forward_wire).unwrap();
    let replay_reverse = || Diagnostic::decode_ovb(&reverse_wire).unwrap();
    let replay_empty = || Diagnostic::decode_ovb(&empty_wire).unwrap();
    let mut receiver = replay_forward();
    let forward = receiver.clone();
    receiver.clone_from(&replay_reverse());
    let reverse = receiver.clone();
    receiver.clone_from(&replay_empty());
    let empty_between = receiver.clone();
    receiver.clone_from(&replay_forward());
    let restored_forward = receiver.clone();
    assert_eq!(restored_forward, forward);
    assert_eq!(empty_between, replay_empty());

    // ORNA-SECRET-002 leaves ordering open; retain the insertion positions of
    // same-code siblings even when their nested terminal codes also match.
    let outer = admitted(
        "ORNA-E-EMPTY-DUP-IDENTICAL-OUTER",
        "public identical empty duplicate admission",
    )
    .with_cause(forward)
    .with_cause(reverse)
    .with_cause(empty_between)
    .with_cause(restored_forward);
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["severity"], "error");
    assert_eq!(
        projection["message"],
        "public identical empty duplicate admission"
    );
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 4);
    for cause in causes {
        assert_redacted_tree(cause);
    }
    assert_eq!(
        causes.iter().map(sibling_tail_counts).collect::<Vec<_>>(),
        vec![vec![2, 0], vec![0, 2], vec![0, 0], vec![2, 0]],
    );

    let json = serde_json::to_vec(&outer).unwrap();
    let encoded = outer.encode_ovb().unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"forward identical empty duplicate payload".as_slice(),
            b"reverse identical empty duplicate payload".as_slice(),
            b"empty identical duplicate payload".as_slice(),
            b"LEFT duplicate tail 0 secret".as_slice(),
            b"LEFT duplicate tail 1 secret".as_slice(),
            b"RIGHT duplicate tail 0 secret".as_slice(),
            b"LEFT sibling secret".as_slice(),
            b"RIGHT sibling secret".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
        assert!(!encoded.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&encoded).unwrap()).unwrap();
    assert_redacted_tree(&decoded);
    assert_eq!(decoded["causes"], projection["causes"]);
}

#[test]
fn duplicate_deep_snapshots_preserve_repeated_tail_order() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }

    let terminal = admitted("ORNA-E-TAKE4-TERMINAL", "deep snapshot payload")
        .with_cause(admitted(
            "ORNA-E-TAKE4-REPEATED-TAIL",
            "first repeated tail secret",
        ))
        .with_cause(admitted(
            "ORNA-E-TAKE4-REPEATED-TAIL",
            "second repeated tail secret",
        ));
    let nested = admitted("ORNA-E-TAKE4-NESTED", "deep snapshot payload")
        .with_cause(terminal);
    let parent = admitted("ORNA-E-TAKE4-PARENT", "deep snapshot payload")
        .with_cause(nested);
    let branch = admitted("ORNA-E-TAKE4-BRANCH", "deep snapshot payload")
        .with_cause(parent);
    let root = admitted("ORNA-E-TAKE4-ROOT", "deep snapshot payload")
        .with_cause(branch);
    let wire = root.encode_ovb().unwrap();
    let first = Diagnostic::decode_ovb(&wire).unwrap();
    let duplicate = Diagnostic::decode_ovb(&wire).unwrap();
    assert_eq!(first, duplicate);

    // ORNA-SECRET-002 requires diagnostic secret redaction but is silent on
    // repeated tail order and multiplicity; preserve both in the recovered snapshots.
    let outer = admitted("ORNA-E-TAKE4-OUTER", "outer deep snapshot admission")
        .with_cause(first)
        .with_cause(duplicate);
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["message"], "outer deep snapshot admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 2);
    assert_eq!(causes[0], causes[1]);
    for cause in causes {
        assert_redacted_tree(cause);
        let terminal = &cause["causes"][0]["causes"][0]["causes"][0]["causes"][0];
        let tails = terminal["causes"].as_array().unwrap();
        assert_eq!(tails.len(), 2);
        assert_eq!(tails[0]["code"], "ORNA-E-TAKE4-REPEATED-TAIL");
        assert_eq!(tails[1]["code"], "ORNA-E-TAKE4-REPEATED-TAIL");
        assert_eq!(tails[0]["message"], "<redacted>");
        assert_eq!(tails[1]["message"], "<redacted>");
    }

    let json = serde_json::to_vec(&outer).unwrap();
    let encoded = outer.encode_ovb().unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"deep snapshot payload".as_slice(),
            b"first repeated tail secret".as_slice(),
            b"second repeated tail secret".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
        assert!(!encoded.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&encoded).unwrap()).unwrap();
    assert_redacted_tree(&decoded);
    assert_eq!(decoded["causes"], projection["causes"]);
}

#[test]
fn repeated_deep_recovery_cycles_keep_tail_multiplicity() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }

    let terminal = admitted("ORNA-E-CYCLE-TERMINAL", "deep recovery cycle payload")
        .with_cause(admitted(
            "ORNA-E-CYCLE-TAIL",
            "first repeated cycle tail secret",
        ))
        .with_cause(admitted(
            "ORNA-E-CYCLE-TAIL",
            "second repeated cycle tail secret",
        ));
    let nested = admitted("ORNA-E-CYCLE-NESTED", "deep recovery cycle payload")
        .with_cause(terminal);
    let parent = admitted("ORNA-E-CYCLE-PARENT", "deep recovery cycle payload")
        .with_cause(nested);
    let branch = admitted("ORNA-E-CYCLE-BRANCH", "deep recovery cycle payload")
        .with_cause(parent);
    let root = admitted("ORNA-E-CYCLE-ROOT", "deep recovery cycle payload")
        .with_cause(branch);

    let mut current = Diagnostic::decode_ovb(&root.encode_ovb().unwrap()).unwrap();
    let mut generations = vec![current.clone()];
    for _ in 0..2 {
        current = Diagnostic::decode_ovb(&current.encode_ovb().unwrap()).unwrap();
        generations.push(current.clone());
    }
    assert_eq!(generations[0], generations[1]);
    assert_eq!(generations[1], generations[2]);

    // ORNA-SECRET-002 requires diagnostic secret redaction but is silent on
    // repeated recovery preserving sibling order and duplicate tail occurrences.
    let outer = generations.into_iter().fold(
        admitted("ORNA-E-CYCLE-OUTER", "outer recovery cycle admission"),
        |outer, generation| outer.with_cause(generation),
    );
    let projection = serde_json::to_value(&outer).unwrap();
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 3);
    for cause in causes {
        assert_redacted_tree(cause);
        let terminal = &cause["causes"][0]["causes"][0]["causes"][0]["causes"][0];
        let tails = terminal["causes"].as_array().unwrap();
        assert_eq!(tails.len(), 2);
        assert_eq!(tails[0]["code"], "ORNA-E-CYCLE-TAIL");
        assert_eq!(tails[1]["code"], "ORNA-E-CYCLE-TAIL");
        assert_eq!(tails[0]["message"], "<redacted>");
        assert_eq!(tails[1]["message"], "<redacted>");
    }
    assert_eq!(causes[0], causes[1]);
    assert_eq!(causes[1], causes[2]);

    let json = serde_json::to_vec(&outer).unwrap();
    let encoded = outer.encode_ovb().unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"deep recovery cycle payload".as_slice(),
            b"first repeated cycle tail secret".as_slice(),
            b"second repeated cycle tail secret".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
        assert!(!encoded.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&encoded).unwrap()).unwrap();
    assert_redacted_tree(&decoded);
    assert_eq!(decoded["causes"], projection["causes"]);
}


#[test]
fn repeated_recovery_tails_survive_empty_replacements() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Warning,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let make_chain = |tails: Vec<Diagnostic>| {
        let terminal = tails.into_iter().fold(
            admitted("ORNA-E-REPEAT-TERMINAL", "repeated recovery payload"),
            |terminal, tail| terminal.with_cause(tail),
        );
        let nested = admitted("ORNA-E-REPEAT-NESTED", "repeated recovery payload")
            .with_cause(terminal);
        let parent = admitted("ORNA-E-REPEAT-PARENT", "repeated recovery payload")
            .with_cause(nested);
        let branch = admitted("ORNA-E-REPEAT-BRANCH", "repeated recovery payload")
            .with_cause(parent);
        admitted("ORNA-E-REPEAT-ROOT", "repeated recovery payload").with_cause(branch)
    };
    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn cause_shape(diagnostic: &serde_json::Value) -> Vec<usize> {
        let causes = diagnostic["causes"].as_array().unwrap();
        let mut shape = vec![causes.len()];
        for cause in causes {
            shape.extend(cause_shape(cause));
        }
        shape
    }
    fn tail_codes(diagnostic: &serde_json::Value) -> Vec<String> {
        let mut terminal = diagnostic;
        for _ in 0..4 {
            terminal = &terminal["causes"][0];
        }
        terminal["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tail| tail["code"].as_str().unwrap().to_owned())
            .collect()
    }

    let repeated_wire = make_chain(vec![
        admitted("ORNA-E-REPEAT-TAIL", "first repeated tail secret"),
        admitted("ORNA-E-REPEAT-TAIL", "second repeated tail secret"),
    ])
    .encode_ovb()
    .unwrap();
    let first = Diagnostic::decode_ovb(&repeated_wire).unwrap();
    let duplicate = Diagnostic::decode_ovb(&repeated_wire).unwrap();
    let empty = Diagnostic::decode_ovb(&make_chain(vec![]).encode_ovb().unwrap()).unwrap();
    assert_eq!(first, duplicate);

    let capture = |snapshot: Diagnostic| move || snapshot.clone();
    let capture_first = capture(first.clone());
    let capture_duplicate = capture(duplicate.clone());
    let capture_empty = capture(empty.clone());
    let mut receiver = first.clone();
    let initial = capture(receiver.clone());
    receiver.clone_from(&capture_empty());
    let empty_before_restore = capture(receiver.clone());
    receiver.clone_from(&capture_duplicate());
    let restored_duplicate = capture(receiver.clone());
    receiver.clone_from(&capture_empty());
    let empty_before_final_restore = capture(receiver.clone());
    receiver.clone_from(&capture_first());
    let restored_first = capture(receiver.clone());
    assert_eq!(initial(), first);
    assert_eq!(empty_before_restore(), empty);
    assert_eq!(restored_duplicate(), duplicate);
    assert_eq!(empty_before_final_restore(), empty);
    assert_eq!(restored_first(), first);

    // ORNA-SECRET-002 requires redaction but is silent on preserving repeated
    // tail order and multiplicity across empty replacement generations.
    let snapshots = [
        initial(),
        empty_before_restore(),
        restored_duplicate(),
        empty_before_final_restore(),
        restored_first(),
    ];
    let outer = snapshots.into_iter().fold(
        admitted("ORNA-E-REPEAT-OUTER", "repeated recovery outer admission"),
        |outer, snapshot| outer.with_cause(snapshot),
    );
    let projection = serde_json::to_value(&outer).unwrap();
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 5);
    for cause in causes {
        assert_redacted_tree(cause);
    }
    assert_eq!(
        causes.iter().map(cause_shape).collect::<Vec<_>>(),
        vec![
            vec![1, 1, 1, 1, 2, 0, 0],
            vec![1, 1, 1, 1, 0],
            vec![1, 1, 1, 1, 2, 0, 0],
            vec![1, 1, 1, 1, 0],
            vec![1, 1, 1, 1, 2, 0, 0],
        ],
    );
    assert_eq!(
        causes.iter().map(tail_codes).collect::<Vec<_>>(),
        vec![
            vec!["ORNA-E-REPEAT-TAIL".to_owned(), "ORNA-E-REPEAT-TAIL".to_owned()],
            vec![],
            vec!["ORNA-E-REPEAT-TAIL".to_owned(), "ORNA-E-REPEAT-TAIL".to_owned()],
            vec![],
            vec!["ORNA-E-REPEAT-TAIL".to_owned(), "ORNA-E-REPEAT-TAIL".to_owned()],
        ],
    );

    let json = serde_json::to_vec(&outer).unwrap();
    let encoded = outer.encode_ovb().unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"repeated recovery payload".as_slice(),
            b"first repeated tail secret".as_slice(),
            b"second repeated tail secret".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
        assert!(!encoded.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&encoded).unwrap()).unwrap();
    assert_redacted_tree(&decoded);
    assert_eq!(decoded["causes"], projection["causes"]);
}

#[test]
fn repeated_error_tails_keep_order_and_severity_after_recovery() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let admitted = |code: &str, severity: DiagnosticSeverity, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            severity,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }

    let terminal = admitted(
        "ORNA-E-ERROR-TERMINAL",
        DiagnosticSeverity::Error,
        "error recovery payload",
    )
    .with_cause(admitted(
        "ORNA-E-ERROR-REPEAT-TAIL",
        DiagnosticSeverity::Error,
        "first repeated error secret",
    ))
    .with_cause(admitted(
        "ORNA-E-ERROR-REPEAT-TAIL",
        DiagnosticSeverity::Fatal,
        "second repeated fatal secret",
    ));
    let nested = admitted(
        "ORNA-E-ERROR-NESTED",
        DiagnosticSeverity::Help,
        "error recovery payload",
    )
    .with_cause(terminal);
    let parent = admitted(
        "ORNA-E-ERROR-PARENT",
        DiagnosticSeverity::Error,
        "error recovery payload",
    )
    .with_cause(nested);
    let branch = admitted(
        "ORNA-E-ERROR-BRANCH",
        DiagnosticSeverity::Warning,
        "error recovery payload",
    )
    .with_cause(parent);
    let root = admitted(
        "ORNA-E-ERROR-ROOT",
        DiagnosticSeverity::Error,
        "error recovery payload",
    )
    .with_cause(branch);
    let wire = root.encode_ovb().unwrap();
    let first = Diagnostic::decode_ovb(&wire).unwrap();
    let duplicate = Diagnostic::decode_ovb(&wire).unwrap();
    assert_eq!(first, duplicate);

    // ORNA-SECRET-002 requires diagnostics to redact secrets but is silent on
    // duplicate tail ordering when the same code carries different severities.
    let outer = admitted(
        "ORNA-E-ERROR-OUTER",
        DiagnosticSeverity::Error,
        "outer error admission",
    )
    .with_cause(first)
    .with_cause(duplicate);
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["message"], "outer error admission");
    assert_eq!(projection["severity"], "error");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 2);
    assert_eq!(causes[0], causes[1]);
    for cause in causes {
        assert_redacted_tree(cause);
        assert_eq!(cause["severity"], "error");
        let terminal = &cause["causes"][0]["causes"][0]["causes"][0]["causes"][0];
        assert_eq!(terminal["severity"], "error");
        let tails = terminal["causes"].as_array().unwrap();
        assert_eq!(tails.len(), 2);
        assert_eq!(tails[0]["code"], "ORNA-E-ERROR-REPEAT-TAIL");
        assert_eq!(tails[0]["severity"], "error");
        assert_eq!(tails[1]["code"], "ORNA-E-ERROR-REPEAT-TAIL");
        assert_eq!(tails[1]["severity"], "fatal");
        assert_eq!(tails[0]["message"], "<redacted>");
        assert_eq!(tails[1]["message"], "<redacted>");
    }

    let json = serde_json::to_vec(&outer).unwrap();
    let encoded = outer.encode_ovb().unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"error recovery payload".as_slice(),
            b"first repeated error secret".as_slice(),
            b"second repeated fatal secret".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
        assert!(!encoded.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&encoded).unwrap()).unwrap();
    assert_redacted_tree(&decoded);
    assert_eq!(decoded["causes"], projection["causes"]);
}

#[test]
fn repeated_error_tails_survive_empty_snapshot_replacements() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let admitted = |code: &str, severity: DiagnosticSeverity, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            severity,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let make_chain = |tails: Vec<Diagnostic>| {
        let terminal = tails.into_iter().fold(
            admitted(
                "ORNA-E-RESTORE-TERMINAL",
                DiagnosticSeverity::Error,
                "repeated restore payload",
            ),
            |terminal, tail| terminal.with_cause(tail),
        );
        let nested = admitted(
            "ORNA-E-RESTORE-NESTED",
            DiagnosticSeverity::Help,
            "repeated restore payload",
        )
        .with_cause(terminal);
        let parent = admitted(
            "ORNA-E-RESTORE-PARENT",
            DiagnosticSeverity::Error,
            "repeated restore payload",
        )
        .with_cause(nested);
        let branch = admitted(
            "ORNA-E-RESTORE-BRANCH",
            DiagnosticSeverity::Warning,
            "repeated restore payload",
        )
        .with_cause(parent);
        admitted(
            "ORNA-E-RESTORE-ROOT",
            DiagnosticSeverity::Error,
            "repeated restore payload",
        )
        .with_cause(branch)
    };
    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn cause_shape(diagnostic: &serde_json::Value) -> Vec<usize> {
        let causes = diagnostic["causes"].as_array().unwrap();
        let mut shape = vec![causes.len()];
        for cause in causes {
            shape.extend(cause_shape(cause));
        }
        shape
    }
    fn severity_path(diagnostic: &serde_json::Value) -> Vec<String> {
        let mut node = diagnostic;
        let mut path = Vec::new();
        for depth in 0..5 {
            path.push(node["severity"].as_str().unwrap().to_owned());
            if depth < 4 {
                node = &node["causes"][0];
            }
        }
        path
    }

    let repeated_wire = make_chain(vec![
        admitted(
            "ORNA-E-RESTORE-TAIL",
            DiagnosticSeverity::Error,
            "first restore error secret",
        ),
        admitted(
            "ORNA-E-RESTORE-TAIL",
            DiagnosticSeverity::Fatal,
            "second restore fatal secret",
        ),
    ])
    .encode_ovb()
    .unwrap();
    let first = Diagnostic::decode_ovb(&repeated_wire).unwrap();
    let duplicate = Diagnostic::decode_ovb(&repeated_wire).unwrap();
    let empty = Diagnostic::decode_ovb(&make_chain(vec![]).encode_ovb().unwrap()).unwrap();
    assert_eq!(first, duplicate);

    let capture = |snapshot: Diagnostic| move || snapshot.clone();
    let capture_first = capture(first.clone());
    let capture_duplicate = capture(duplicate.clone());
    let capture_empty = capture(empty.clone());
    let mut receiver = first.clone();
    let initial = capture(receiver.clone());
    receiver.clone_from(&capture_empty());
    let empty_before_restore = capture(receiver.clone());
    receiver.clone_from(&capture_duplicate());
    let restored_duplicate = capture(receiver.clone());
    receiver.clone_from(&capture_empty());
    let empty_before_final_restore = capture(receiver.clone());
    receiver.clone_from(&capture_first());
    let restored_first = capture(receiver.clone());
    assert_eq!(initial(), first);
    assert_eq!(empty_before_restore(), empty);
    assert_eq!(restored_duplicate(), duplicate);
    assert_eq!(empty_before_final_restore(), empty);
    assert_eq!(restored_first(), first);

    // ORNA-SECRET-002 requires redaction but is silent on duplicate Error/Fatal
    // tails surviving empty snapshot replacements; preserve order and severity.
    let snapshots = [
        initial(),
        empty_before_restore(),
        restored_duplicate(),
        empty_before_final_restore(),
        restored_first(),
    ];
    let outer = snapshots.into_iter().fold(
        admitted(
            "ORNA-E-RESTORE-OUTER",
            DiagnosticSeverity::Error,
            "outer restore admission",
        ),
        |outer, snapshot| outer.with_cause(snapshot),
    );
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["severity"], "error");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 5);
    for cause in causes {
        assert_redacted_tree(cause);
    }
    assert_eq!(
        causes.iter().map(cause_shape).collect::<Vec<_>>(),
        vec![
            vec![1, 1, 1, 1, 2, 0, 0],
            vec![1, 1, 1, 1, 0],
            vec![1, 1, 1, 1, 2, 0, 0],
            vec![1, 1, 1, 1, 0],
            vec![1, 1, 1, 1, 2, 0, 0],
        ],
    );
    assert_eq!(causes[0], causes[2]);
    assert_eq!(causes[2], causes[4]);
    assert_eq!(causes[1], causes[3]);
    let expected_severity_path = ["error", "warning", "error", "help", "error"];
    assert_eq!(severity_path(&causes[0]), expected_severity_path);
    assert_eq!(severity_path(&causes[1]), expected_severity_path);
    let mut terminal = &causes[0];
    for _ in 0..4 {
        terminal = &terminal["causes"][0];
    }
    let tails = terminal["causes"].as_array().unwrap();
    assert_eq!(tails.len(), 2);
    assert_eq!(tails[0]["code"], "ORNA-E-RESTORE-TAIL");
    assert_eq!(tails[0]["severity"], "error");
    assert_eq!(tails[1]["code"], "ORNA-E-RESTORE-TAIL");
    assert_eq!(tails[1]["severity"], "fatal");
    assert_eq!(tails[0]["message"], "<redacted>");
    assert_eq!(tails[1]["message"], "<redacted>");

    let json = serde_json::to_vec(&outer).unwrap();
    let encoded = outer.encode_ovb().unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"repeated restore payload".as_slice(),
            b"first restore error secret".as_slice(),
            b"second restore fatal secret".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
        assert!(!encoded.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&encoded).unwrap()).unwrap();
    assert_redacted_tree(&decoded);
    assert_eq!(decoded["causes"], projection["causes"]);
}

#[test]
fn error_tail_replay_preserves_repeated_entries_after_empty_recovery() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let admitted = |code: &str, severity: DiagnosticSeverity, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            severity,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let make_chain = |tails: Vec<Diagnostic>| {
        let terminal = tails.into_iter().fold(
            admitted("ORNA-E-REPLAY-TERMINAL", DiagnosticSeverity::Error, "replay payload"),
            |terminal, tail| terminal.with_cause(tail),
        );
        let nested = admitted("ORNA-E-REPLAY-NESTED", DiagnosticSeverity::Help, "replay payload")
            .with_cause(terminal);
        let parent = admitted("ORNA-E-REPLAY-PARENT", DiagnosticSeverity::Error, "replay payload")
            .with_cause(nested);
        let branch = admitted("ORNA-E-REPLAY-BRANCH", DiagnosticSeverity::Warning, "replay payload")
            .with_cause(parent);
        admitted("ORNA-E-REPLAY-ROOT", DiagnosticSeverity::Error, "replay payload")
            .with_cause(branch)
    };
    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn cause_shape(diagnostic: &serde_json::Value) -> Vec<usize> {
        let causes = diagnostic["causes"].as_array().unwrap();
        let mut shape = vec![causes.len()];
        for cause in causes {
            shape.extend(cause_shape(cause));
        }
        shape
    }
    fn severity_path(diagnostic: &serde_json::Value) -> Vec<String> {
        let mut node = diagnostic;
        let mut path = Vec::new();
        for depth in 0..5 {
            path.push(node["severity"].as_str().unwrap().to_owned());
            if depth < 4 {
                node = &node["causes"][0];
            }
        }
        path
    }
    fn tail_codes(diagnostic: &serde_json::Value) -> Vec<String> {
        let mut terminal = diagnostic;
        for _ in 0..4 {
            terminal = &terminal["causes"][0];
        }
        terminal["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tail| tail["code"].as_str().unwrap().to_owned())
            .collect()
    }

    let repeated_wire = make_chain(vec![
        admitted("ORNA-E-REPLAY-TAIL", DiagnosticSeverity::Error, "first replay error secret"),
        admitted("ORNA-E-REPLAY-TAIL", DiagnosticSeverity::Fatal, "second replay fatal secret"),
    ])
    .encode_ovb()
    .unwrap();
    let empty_wire = make_chain(vec![]).encode_ovb().unwrap();
    let replay_error = || Diagnostic::decode_ovb(&repeated_wire).unwrap();
    let replay_empty = || Diagnostic::decode_ovb(&empty_wire).unwrap();
    let first_replay = replay_error();
    let second_replay = replay_error();
    assert_eq!(first_replay, second_replay);

    let capture = |snapshot: Diagnostic| move || snapshot.clone();
    let mut receiver = first_replay.clone();
    let initial = capture(receiver.clone());
    receiver.clone_from(&replay_empty());
    let empty_before_replay = capture(receiver.clone());
    receiver.clone_from(&replay_error());
    let restored_first_replay = capture(receiver.clone());
    receiver.clone_from(&replay_empty());
    let empty_after_replay = capture(receiver.clone());
    receiver.clone_from(&replay_error());
    let restored_second_replay = capture(receiver.clone());
    assert_eq!(initial(), first_replay);
    assert_eq!(empty_before_replay(), replay_empty());
    assert_eq!(restored_first_replay(), first_replay);
    assert_eq!(empty_after_replay(), replay_empty());
    assert_eq!(restored_second_replay(), second_replay);

    // ORNA-SECRET-002 requires redaction but is silent on repeated Error/Fatal
    // tails remaining ordered when a decoded snapshot is replayed after empties.
    let snapshots = [
        initial(),
        empty_before_replay(),
        restored_first_replay(),
        empty_after_replay(),
        restored_second_replay(),
    ];
    let outer = snapshots.into_iter().fold(
        admitted("ORNA-E-REPLAY-OUTER", DiagnosticSeverity::Error, "outer replay admission"),
        |outer, snapshot| outer.with_cause(snapshot),
    );
    let projection = serde_json::to_value(&outer).unwrap();
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 5);
    for cause in causes {
        assert_redacted_tree(cause);
    }
    assert_eq!(causes[0], causes[2]);
    assert_eq!(causes[2], causes[4]);
    assert_eq!(causes[1], causes[3]);
    assert_eq!(
        causes.iter().map(cause_shape).collect::<Vec<_>>(),
        vec![
            vec![1, 1, 1, 1, 2, 0, 0],
            vec![1, 1, 1, 1, 0],
            vec![1, 1, 1, 1, 2, 0, 0],
            vec![1, 1, 1, 1, 0],
            vec![1, 1, 1, 1, 2, 0, 0],
        ],
    );
    assert_eq!(severity_path(&causes[0]), ["error", "warning", "error", "help", "error"]);
    assert_eq!(severity_path(&causes[1]), ["error", "warning", "error", "help", "error"]);
    assert_eq!(
        causes.iter().map(tail_codes).collect::<Vec<_>>(),
        vec![
            vec!["ORNA-E-REPLAY-TAIL".to_owned(), "ORNA-E-REPLAY-TAIL".to_owned()],
            vec![],
            vec!["ORNA-E-REPLAY-TAIL".to_owned(), "ORNA-E-REPLAY-TAIL".to_owned()],
            vec![],
            vec!["ORNA-E-REPLAY-TAIL".to_owned(), "ORNA-E-REPLAY-TAIL".to_owned()],
        ],
    );
    let mut terminal = &causes[0];
    for _ in 0..4 {
        terminal = &terminal["causes"][0];
    }
    let tails = terminal["causes"].as_array().unwrap();
    assert_eq!(tails[0]["severity"], "error");
    assert_eq!(tails[1]["severity"], "fatal");

    let json = serde_json::to_vec(&outer).unwrap();
    let encoded = outer.encode_ovb().unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"replay payload".as_slice(),
            b"first replay error secret".as_slice(),
            b"second replay fatal secret".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
        assert!(!encoded.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&encoded).unwrap()).unwrap();
    assert_redacted_tree(&decoded);
    assert_eq!(decoded["causes"], projection["causes"]);
}

#[test]
fn replayed_error_snapshots_keep_repeated_tail_order() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let admitted = |code: &str, severity: DiagnosticSeverity, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            severity,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let make_wire = |payload: &str, tail_severities: [DiagnosticSeverity; 2]| {
        let terminal = admitted("ORNA-E-REPLAY2-TERMINAL", DiagnosticSeverity::Error, payload)
            .with_cause(admitted(
                "ORNA-E-REPLAY2-TAIL",
                tail_severities[0],
                &format!("{payload} first tail secret"),
            ))
            .with_cause(admitted(
                "ORNA-E-REPLAY2-TAIL",
                tail_severities[1],
                &format!("{payload} second tail secret"),
            ));
        let nested = admitted("ORNA-E-REPLAY2-NESTED", DiagnosticSeverity::Help, payload)
            .with_cause(terminal);
        let parent = admitted("ORNA-E-REPLAY2-PARENT", DiagnosticSeverity::Error, payload)
            .with_cause(nested);
        let branch = admitted("ORNA-E-REPLAY2-BRANCH", DiagnosticSeverity::Warning, payload)
            .with_cause(parent);
        admitted("ORNA-E-REPLAY2-ROOT", DiagnosticSeverity::Error, payload)
            .with_cause(branch)
            .encode_ovb()
            .unwrap()
    };
    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn cause_shape(diagnostic: &serde_json::Value) -> Vec<usize> {
        let causes = diagnostic["causes"].as_array().unwrap();
        let mut shape = vec![causes.len()];
        for cause in causes {
            shape.extend(cause_shape(cause));
        }
        shape
    }
    fn tail_severities(diagnostic: &serde_json::Value) -> Vec<String> {
        let mut terminal = diagnostic;
        for _ in 0..4 {
            terminal = &terminal["causes"][0];
        }
        terminal["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tail| tail["severity"].as_str().unwrap().to_owned())
            .collect()
    }

    let wire_a = make_wire(
        "replay generation A payload",
        [DiagnosticSeverity::Error, DiagnosticSeverity::Fatal],
    );
    let wire_b = make_wire(
        "replay generation B payload",
        [DiagnosticSeverity::Fatal, DiagnosticSeverity::Error],
    );
    let replay_a = || Diagnostic::decode_ovb(&wire_a).unwrap();
    let replay_b = || Diagnostic::decode_ovb(&wire_b).unwrap();
    let first_a = replay_a();
    let duplicate_a = replay_a();
    let first_b = replay_b();
    let duplicate_b = replay_b();
    assert_eq!(first_a, duplicate_a);
    assert_eq!(first_b, duplicate_b);

    let capture = |snapshot: Diagnostic| move || snapshot.clone();
    let mut receiver = first_a.clone();
    let capture_a = capture(receiver.clone());
    receiver.clone_from(&replay_b());
    let capture_b = capture(receiver.clone());
    receiver.clone_from(&replay_a());
    let capture_replayed_a = capture(receiver.clone());
    receiver.clone_from(&replay_b());
    let capture_replayed_b = capture(receiver.clone());
    assert_eq!(capture_a(), first_a);
    assert_eq!(capture_b(), first_b);
    assert_eq!(capture_replayed_a(), duplicate_a);
    assert_eq!(capture_replayed_b(), duplicate_b);

    // ORNA-SECRET-002 requires diagnostic redaction but is silent on replay
    // order for repeated same-code tails with different error severities.
    let snapshots = [
        capture_a(),
        capture_b(),
        capture_replayed_a(),
        capture_replayed_b(),
    ];
    let outer = snapshots.into_iter().fold(
        admitted("ORNA-E-REPLAY2-OUTER", DiagnosticSeverity::Error, "outer replay admission"),
        |outer, snapshot| outer.with_cause(snapshot),
    );
    let projection = serde_json::to_value(&outer).unwrap();
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 4);
    for cause in causes {
        assert_redacted_tree(cause);
    }
    assert_eq!(causes[0], causes[2]);
    assert_eq!(causes[1], causes[3]);
    assert_eq!(
        causes.iter().map(cause_shape).collect::<Vec<_>>(),
        vec![vec![1, 1, 1, 1, 2, 0, 0]; 4],
    );
    assert_eq!(
        causes.iter().map(tail_severities).collect::<Vec<_>>(),
        vec![
            vec!["error", "fatal"],
            vec!["fatal", "error"],
            vec!["error", "fatal"],
            vec!["fatal", "error"],
        ],
    );

    let json = serde_json::to_vec(&outer).unwrap();
    let encoded = outer.encode_ovb().unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"replay generation A payload".as_slice(),
            b"replay generation B payload".as_slice(),
            b"replay generation A payload first tail secret".as_slice(),
            b"replay generation A payload second tail secret".as_slice(),
            b"replay generation B payload first tail secret".as_slice(),
            b"replay generation B payload second tail secret".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
        assert!(!encoded.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&encoded).unwrap()).unwrap();
    assert_redacted_tree(&decoded);
    assert_eq!(decoded["causes"], projection["causes"]);
}

#[test]
fn error_tail_order_survives_crossed_snapshot_replay() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let make_wire = |payload: &str, tail_codes: [&str; 2]| {
        let terminal = admitted("ORNA-E-ORDER-TERMINAL", payload)
            .with_cause(admitted(tail_codes[0], &format!("{payload} first tail secret")))
            .with_cause(admitted(tail_codes[1], &format!("{payload} second tail secret")));
        let nested = admitted("ORNA-E-ORDER-NESTED", payload).with_cause(terminal);
        let parent = admitted("ORNA-E-ORDER-PARENT", payload).with_cause(nested);
        let branch = admitted("ORNA-E-ORDER-BRANCH", payload).with_cause(parent);
        admitted("ORNA-E-ORDER-ROOT", payload)
            .with_cause(branch)
            .encode_ovb()
            .unwrap()
    };
    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn tail_codes(diagnostic: &serde_json::Value) -> Vec<String> {
        let mut terminal = diagnostic;
        for _ in 0..4 {
            terminal = &terminal["causes"][0];
        }
        terminal["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tail| tail["code"].as_str().unwrap().to_owned())
            .collect()
    }

    let ab_wire = make_wire(
        "forward order recovery payload",
        ["ORNA-E-ORDER-TAIL-A", "ORNA-E-ORDER-TAIL-B"],
    );
    let ba_wire = make_wire(
        "reverse order recovery payload",
        ["ORNA-E-ORDER-TAIL-B", "ORNA-E-ORDER-TAIL-A"],
    );
    let replay_ab = || Diagnostic::decode_ovb(&ab_wire).unwrap();
    let replay_ba = || Diagnostic::decode_ovb(&ba_wire).unwrap();
    let snapshot_ab = replay_ab();
    let snapshot_ba = replay_ba();
    let capture = |snapshot: Diagnostic| move || snapshot.clone();
    let mut receiver = snapshot_ab.clone();
    let capture_ab = capture(receiver.clone());
    receiver.clone_from(&snapshot_ba);
    let capture_ba = capture(receiver.clone());
    receiver.clone_from(&replay_ab());
    let capture_replayed_ab = capture(receiver.clone());
    assert_eq!(capture_ab(), snapshot_ab);
    assert_eq!(capture_ba(), snapshot_ba);
    assert_eq!(capture_replayed_ab(), replay_ab());

    // ORNA-SECRET-002 requires diagnostic redaction but is silent on retaining
    // source order when error-tail snapshots with reversed entries are replayed.
    let outer = admitted("ORNA-E-ORDER-OUTER", "outer error order admission")
        .with_cause(capture_ab())
        .with_cause(capture_ba())
        .with_cause(capture_replayed_ab());
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["severity"], "error");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 3);
    for cause in causes {
        assert_redacted_tree(cause);
        assert_eq!(cause["severity"], "error");
    }
    assert_eq!(causes[0], causes[2]);
    assert_eq!(
        causes.iter().map(tail_codes).collect::<Vec<_>>(),
        vec![
            vec!["ORNA-E-ORDER-TAIL-A".to_owned(), "ORNA-E-ORDER-TAIL-B".to_owned()],
            vec!["ORNA-E-ORDER-TAIL-B".to_owned(), "ORNA-E-ORDER-TAIL-A".to_owned()],
            vec!["ORNA-E-ORDER-TAIL-A".to_owned(), "ORNA-E-ORDER-TAIL-B".to_owned()],
        ],
    );
    let mut terminal = &causes[0];
    for _ in 0..4 {
        terminal = &terminal["causes"][0];
    }
    let tails = terminal["causes"].as_array().unwrap();
    assert_eq!(tails[0]["severity"], "error");
    assert_eq!(tails[1]["severity"], "error");

    let json = serde_json::to_vec(&outer).unwrap();
    let encoded = outer.encode_ovb().unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"forward order recovery payload".as_slice(),
            b"reverse order recovery payload".as_slice(),
            b"forward order recovery payload first tail secret".as_slice(),
            b"forward order recovery payload second tail secret".as_slice(),
            b"reverse order recovery payload first tail secret".as_slice(),
            b"reverse order recovery payload second tail secret".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
        assert!(!encoded.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&encoded).unwrap()).unwrap();
    assert_redacted_tree(&decoded);
    assert_eq!(decoded["causes"], projection["causes"]);
}

#[test]
fn replayed_error_tail_order_is_stable_between_siblings() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let admitted = |code: &str, severity: DiagnosticSeverity, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            severity,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let make_wire = |payload: &str, tail_codes: [&str; 2]| {
        let terminal = admitted("ORNA-E-ORDER-REPLAY-TERMINAL", DiagnosticSeverity::Error, payload)
            .with_cause(admitted(
                tail_codes[0],
                DiagnosticSeverity::Error,
                &format!("{payload} first tail secret"),
            ))
            .with_cause(admitted(
                tail_codes[1],
                DiagnosticSeverity::Error,
                &format!("{payload} second tail secret"),
            ));
        let parent = admitted("ORNA-E-ORDER-REPLAY-PARENT", DiagnosticSeverity::Error, payload)
            .with_cause(terminal);
        admitted("ORNA-E-ORDER-REPLAY-ROOT", DiagnosticSeverity::Error, payload)
            .with_cause(parent)
            .encode_ovb()
            .unwrap()
    };
    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn tail_codes(diagnostic: &serde_json::Value) -> Vec<String> {
        let terminal = &diagnostic["causes"][0]["causes"][0];
        terminal["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tail| tail["code"].as_str().unwrap().to_owned())
            .collect()
    }

    let forward_wire = make_wire(
        "forward replay payload",
        ["ORNA-E-ORDER-REPLAY-TAIL-A", "ORNA-E-ORDER-REPLAY-TAIL-B"],
    );
    let reverse_wire = make_wire(
        "reverse replay payload",
        ["ORNA-E-ORDER-REPLAY-TAIL-B", "ORNA-E-ORDER-REPLAY-TAIL-A"],
    );
    let replay_forward = || Diagnostic::decode_ovb(&forward_wire).unwrap();
    let replay_reverse = || Diagnostic::decode_ovb(&reverse_wire).unwrap();
    let mut receiver = replay_forward();
    let forward_before = receiver.clone();
    receiver.clone_from(&replay_reverse());
    let reverse = receiver.clone();
    receiver.clone_from(&replay_forward());
    let forward_after = receiver.clone();

    // ORNA-SECRET-002 requires redaction but is silent on sibling and nested
    // error-tail order after replay; preserve each source's insertion order.
    let outer = admitted(
        "ORNA-E-ORDER-REPLAY-OUTER",
        DiagnosticSeverity::Error,
        "outer replay public admission",
    )
    .with_cause(admitted(
        "ORNA-E-ORDER-REPLAY-BEFORE",
        DiagnosticSeverity::Warning,
        "outer replay sibling payload",
    ))
    .with_cause(forward_before)
    .with_cause(reverse)
    .with_cause(forward_after)
    .with_cause(admitted(
        "ORNA-E-ORDER-REPLAY-AFTER",
        DiagnosticSeverity::Warning,
        "outer replay sibling payload",
    ));
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["severity"], "error");
    assert_eq!(projection["message"], "outer replay public admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 5);
    assert_eq!(
        causes
            .iter()
            .map(|cause| cause["code"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![
            "ORNA-E-ORDER-REPLAY-BEFORE",
            "ORNA-E-ORDER-REPLAY-ROOT",
            "ORNA-E-ORDER-REPLAY-ROOT",
            "ORNA-E-ORDER-REPLAY-ROOT",
            "ORNA-E-ORDER-REPLAY-AFTER",
        ],
    );
    for cause in causes {
        assert_redacted_tree(cause);
    }
    assert_eq!(
        causes[1..4].iter().map(tail_codes).collect::<Vec<_>>(),
        vec![
            vec![
                "ORNA-E-ORDER-REPLAY-TAIL-A".to_owned(),
                "ORNA-E-ORDER-REPLAY-TAIL-B".to_owned(),
            ],
            vec![
                "ORNA-E-ORDER-REPLAY-TAIL-B".to_owned(),
                "ORNA-E-ORDER-REPLAY-TAIL-A".to_owned(),
            ],
            vec![
                "ORNA-E-ORDER-REPLAY-TAIL-A".to_owned(),
                "ORNA-E-ORDER-REPLAY-TAIL-B".to_owned(),
            ],
        ],
    );

    let json = serde_json::to_vec(&outer).unwrap();
    let encoded = outer.encode_ovb().unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"forward replay payload".as_slice(),
            b"reverse replay payload".as_slice(),
            b"outer replay sibling payload".as_slice(),
            b"forward replay payload first tail secret".as_slice(),
            b"forward replay payload second tail secret".as_slice(),
            b"reverse replay payload first tail secret".as_slice(),
            b"reverse replay payload second tail secret".as_slice(),
        ])
    {
        assert!(
            !json.windows(disclosure.len()).any(|window| window == disclosure),
            "JSON retained test secret {:?}",
            String::from_utf8_lossy(disclosure),
        );
        assert!(
            !encoded.windows(disclosure.len()).any(|window| window == disclosure),
            "OVB retained test secret {:?}",
            String::from_utf8_lossy(disclosure),
        );
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&encoded).unwrap()).unwrap();
    assert_redacted_tree(&decoded);
    assert_eq!(decoded["causes"], projection["causes"]);
}

#[test]
fn replayed_error_tail_order_survives_an_interposed_error_sibling() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let admitted = |code: &str, severity: DiagnosticSeverity, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            severity,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let make_wire = |payload: &str, tail_codes: [&str; 2]| {
        let terminal = admitted(
            "ORNA-E-REPLAY-INTERPOSED-TERMINAL",
            DiagnosticSeverity::Error,
            payload,
        )
        .with_cause(admitted(
            tail_codes[0],
            DiagnosticSeverity::Error,
            &format!("{payload} first tail secret"),
        ))
        .with_cause(admitted(
            tail_codes[1],
            DiagnosticSeverity::Error,
            &format!("{payload} second tail secret"),
        ));
        let parent = admitted(
            "ORNA-E-REPLAY-INTERPOSED-PARENT",
            DiagnosticSeverity::Error,
            payload,
        )
        .with_cause(terminal);
        admitted(
            "ORNA-E-REPLAY-INTERPOSED-ROOT",
            DiagnosticSeverity::Error,
            payload,
        )
        .with_cause(parent)
        .encode_ovb()
        .unwrap()
    };
    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn tail_entries(diagnostic: &serde_json::Value) -> Vec<(String, String)> {
        let terminal = &diagnostic["causes"][0]["causes"][0];
        terminal["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tail| {
                (
                    tail["code"].as_str().unwrap().to_owned(),
                    tail["severity"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }

    let forward_wire = make_wire(
        "forward interposed replay payload",
        [
            "ORNA-E-REPLAY-INTERPOSED-TAIL-A",
            "ORNA-E-REPLAY-INTERPOSED-TAIL-B",
        ],
    );
    let reverse_wire = make_wire(
        "reverse interposed replay payload",
        [
            "ORNA-E-REPLAY-INTERPOSED-TAIL-B",
            "ORNA-E-REPLAY-INTERPOSED-TAIL-A",
        ],
    );
    let replay_forward = || Diagnostic::decode_ovb(&forward_wire).unwrap();
    let replay_reverse = || Diagnostic::decode_ovb(&reverse_wire).unwrap();
    let mut receiver = replay_forward();
    let forward_snapshot = receiver.clone();
    receiver.clone_from(&replay_reverse());
    let reverse_snapshot = receiver.clone();
    receiver.clone_from(&replay_forward());
    assert_eq!(receiver, forward_snapshot);

    // ORNA-SECRET-002 requires redaction but does not specify sibling ordering;
    // retain append order around each replayed error tail.
    let outer = admitted(
        "ORNA-E-REPLAY-INTERPOSED-OUTER",
        DiagnosticSeverity::Error,
        "public interposed replay admission",
    )
    .with_cause(admitted(
        "ORNA-E-REPLAY-INTERPOSED-BEFORE",
        DiagnosticSeverity::Warning,
        "outer sibling payload",
    ))
    .with_cause(forward_snapshot)
    .with_cause(admitted(
        "ORNA-E-REPLAY-INTERPOSED-ERROR-TAIL",
        DiagnosticSeverity::Error,
        "interposed error tail secret",
    ))
    .with_cause(reverse_snapshot)
    .with_cause(admitted(
        "ORNA-E-REPLAY-INTERPOSED-AFTER",
        DiagnosticSeverity::Warning,
        "outer sibling payload",
    ));
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["severity"], "error");
    assert_eq!(projection["message"], "public interposed replay admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 5);
    assert_eq!(
        causes
            .iter()
            .map(|cause| cause["code"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![
            "ORNA-E-REPLAY-INTERPOSED-BEFORE",
            "ORNA-E-REPLAY-INTERPOSED-ROOT",
            "ORNA-E-REPLAY-INTERPOSED-ERROR-TAIL",
            "ORNA-E-REPLAY-INTERPOSED-ROOT",
            "ORNA-E-REPLAY-INTERPOSED-AFTER",
        ],
    );
    for cause in causes {
        assert_redacted_tree(cause);
    }
    let tails = [tail_entries(&causes[1]), tail_entries(&causes[3])];
    assert_eq!(
        tails,
        [
            vec![
                ("ORNA-E-REPLAY-INTERPOSED-TAIL-A".to_owned(), "error".to_owned()),
                ("ORNA-E-REPLAY-INTERPOSED-TAIL-B".to_owned(), "error".to_owned()),
            ],
            vec![
                ("ORNA-E-REPLAY-INTERPOSED-TAIL-B".to_owned(), "error".to_owned()),
                ("ORNA-E-REPLAY-INTERPOSED-TAIL-A".to_owned(), "error".to_owned()),
            ],
        ],
    );

    let json = serde_json::to_vec(&outer).unwrap();
    let encoded = outer.encode_ovb().unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"forward interposed replay payload".as_slice(),
            b"reverse interposed replay payload".as_slice(),
            b"outer sibling payload".as_slice(),
            b"interposed error tail secret".as_slice(),
            b"forward interposed replay payload first tail secret".as_slice(),
            b"forward interposed replay payload second tail secret".as_slice(),
            b"reverse interposed replay payload first tail secret".as_slice(),
            b"reverse interposed replay payload second tail secret".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
        assert!(!encoded.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&encoded).unwrap()).unwrap();
    assert_redacted_tree(&decoded);
    assert_eq!(decoded["causes"], projection["causes"]);
}

#[test]
fn replayed_duplicate_error_tail_order_keeps_nested_shapes() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let make_wire = |payload: &str, reverse: bool| {
        let tail_a = || {
            admitted(
                "ORNA-E-REPLAY-DUPLICATE-TAIL",
                &format!("{payload} first duplicate tail secret"),
            )
            .with_cause(admitted(
                "ORNA-E-REPLAY-DUPLICATE-CHILD-A",
                &format!("{payload} child A secret"),
            ))
        };
        let tail_b = || {
            admitted(
                "ORNA-E-REPLAY-DUPLICATE-TAIL",
                &format!("{payload} second duplicate tail secret"),
            )
            .with_cause(admitted(
                "ORNA-E-REPLAY-DUPLICATE-CHILD-B1",
                &format!("{payload} child B1 secret"),
            ))
            .with_cause(admitted(
                "ORNA-E-REPLAY-DUPLICATE-CHILD-B2",
                &format!("{payload} child B2 secret"),
            ))
        };
        let tails = if reverse {
            vec![tail_b(), tail_a()]
        } else {
            vec![tail_a(), tail_b()]
        };
        let terminal = tails.into_iter().fold(
            admitted("ORNA-E-REPLAY-DUPLICATE-TERMINAL", payload),
            |terminal, tail| terminal.with_cause(tail),
        );
        let nested = admitted("ORNA-E-REPLAY-DUPLICATE-NESTED", payload).with_cause(terminal);
        let parent = admitted("ORNA-E-REPLAY-DUPLICATE-PARENT", payload).with_cause(nested);
        let branch = admitted("ORNA-E-REPLAY-DUPLICATE-BRANCH", payload).with_cause(parent);
        admitted("ORNA-E-REPLAY-DUPLICATE-ROOT", payload)
            .with_cause(branch)
            .encode_ovb()
            .unwrap()
    };
    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn tail_signatures(diagnostic: &serde_json::Value) -> Vec<(String, String, usize)> {
        let mut terminal = diagnostic;
        for _ in 0..4 {
            terminal = &terminal["causes"][0];
        }
        terminal["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tail| {
                (
                    tail["code"].as_str().unwrap().to_owned(),
                    tail["severity"].as_str().unwrap().to_owned(),
                    tail["causes"].as_array().unwrap().len(),
                )
            })
            .collect()
    }

    let forward_wire = make_wire("forward duplicate replay payload", false);
    let reverse_wire = make_wire("reverse duplicate replay payload", true);
    let replay_forward = || Diagnostic::decode_ovb(&forward_wire).unwrap();
    let replay_reverse = || Diagnostic::decode_ovb(&reverse_wire).unwrap();
    let mut receiver = replay_forward();
    let forward = receiver.clone();
    receiver.clone_from(&replay_reverse());
    let reverse = receiver.clone();
    receiver.clone_from(&replay_forward());
    let restored_forward = receiver.clone();
    assert_eq!(restored_forward, forward);

    // ORNA-SECRET-002 requires redaction but is silent on order for duplicate
    // same-code Error tails; preserve their insertion order across replay.
    let outer = admitted("ORNA-E-REPLAY-DUPLICATE-OUTER", "public duplicate replay admission")
        .with_cause(forward)
        .with_cause(reverse)
        .with_cause(restored_forward);
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["severity"], "error");
    assert_eq!(projection["message"], "public duplicate replay admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 3);
    for cause in causes {
        assert_redacted_tree(cause);
    }
    let duplicate_tail = "ORNA-E-REPLAY-DUPLICATE-TAIL".to_owned();
    assert_eq!(
        causes.iter().map(tail_signatures).collect::<Vec<_>>(),
        vec![
            vec![
                (duplicate_tail.clone(), "error".to_owned(), 1),
                (duplicate_tail.clone(), "error".to_owned(), 2),
            ],
            vec![
                (duplicate_tail.clone(), "error".to_owned(), 2),
                (duplicate_tail.clone(), "error".to_owned(), 1),
            ],
            vec![
                (duplicate_tail.clone(), "error".to_owned(), 1),
                (duplicate_tail, "error".to_owned(), 2),
            ],
        ],
    );

    let json = serde_json::to_vec(&outer).unwrap();
    let encoded = outer.encode_ovb().unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"forward duplicate replay payload".as_slice(),
            b"reverse duplicate replay payload".as_slice(),
            b"first duplicate tail secret".as_slice(),
            b"second duplicate tail secret".as_slice(),
            b"child A secret".as_slice(),
            b"child B1 secret".as_slice(),
            b"child B2 secret".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
        assert!(!encoded.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&encoded).unwrap()).unwrap();
    assert_redacted_tree(&decoded);
    assert_eq!(decoded["causes"], projection["causes"]);
}

#[test]
fn replayed_duplicate_error_tails_keep_nested_child_order() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let make_wire = |payload: &str, reverse: bool| {
        let tail = |label: &str, child_codes: [&str; 2]| {
            admitted(
                "ORNA-E-REPLAY-NESTED-DUPLICATE-TAIL",
                &format!("{payload} tail {label} secret"),
            )
            .with_cause(admitted(
                child_codes[0],
                &format!("{payload} {} secret", child_codes[0]),
            ))
            .with_cause(admitted(
                child_codes[1],
                &format!("{payload} {} secret", child_codes[1]),
            ))
        };
        let tails = if reverse {
            vec![
                tail("B", ["ORNA-E-REPLAY-CHILD-B2", "ORNA-E-REPLAY-CHILD-B1"]),
                tail("A", ["ORNA-E-REPLAY-CHILD-A2", "ORNA-E-REPLAY-CHILD-A1"]),
            ]
        } else {
            vec![
                tail("A", ["ORNA-E-REPLAY-CHILD-A1", "ORNA-E-REPLAY-CHILD-A2"]),
                tail("B", ["ORNA-E-REPLAY-CHILD-B1", "ORNA-E-REPLAY-CHILD-B2"]),
            ]
        };
        let terminal = tails.into_iter().fold(
            admitted("ORNA-E-REPLAY-NESTED-DUPLICATE-TERMINAL", payload),
            |terminal, tail| terminal.with_cause(tail),
        );
        let nested = admitted("ORNA-E-REPLAY-NESTED-DUPLICATE-NESTED", payload)
            .with_cause(terminal);
        let parent = admitted("ORNA-E-REPLAY-NESTED-DUPLICATE-PARENT", payload)
            .with_cause(nested);
        let branch = admitted("ORNA-E-REPLAY-NESTED-DUPLICATE-BRANCH", payload)
            .with_cause(parent);
        admitted("ORNA-E-REPLAY-NESTED-DUPLICATE-ROOT", payload)
            .with_cause(branch)
            .encode_ovb()
            .unwrap()
    };
    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn tail_contents(diagnostic: &serde_json::Value) -> Vec<(String, String, Vec<String>)> {
        let mut terminal = diagnostic;
        for _ in 0..4 {
            terminal = &terminal["causes"][0];
        }
        terminal["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tail| {
                (
                    tail["code"].as_str().unwrap().to_owned(),
                    tail["severity"].as_str().unwrap().to_owned(),
                    tail["causes"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|child| child["code"].as_str().unwrap().to_owned())
                        .collect(),
                )
            })
            .collect()
    }

    let forward_wire = make_wire("forward nested duplicate replay payload", false);
    let reverse_wire = make_wire("reverse nested duplicate replay payload", true);
    let replay_forward = || Diagnostic::decode_ovb(&forward_wire).unwrap();
    let replay_reverse = || Diagnostic::decode_ovb(&reverse_wire).unwrap();
    let mut receiver = replay_forward();
    let forward = receiver.clone();
    receiver.clone_from(&replay_reverse());
    let reverse = receiver.clone();
    receiver.clone_from(&replay_forward());
    let restored_forward = receiver.clone();
    assert_eq!(restored_forward, forward);

    // ORNA-SECRET-002 requires redaction but does not specify order for
    // duplicate same-code Error tails; preserve their nested insertion order.
    let outer = admitted(
        "ORNA-E-REPLAY-NESTED-DUPLICATE-OUTER",
        "public duplicate replay order admission",
    )
    .with_cause(forward)
    .with_cause(reverse)
    .with_cause(restored_forward);
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["severity"], "error");
    assert_eq!(projection["message"], "public duplicate replay order admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 3);
    for cause in causes {
        assert_redacted_tree(cause);
    }
    let duplicate_tail = "ORNA-E-REPLAY-NESTED-DUPLICATE-TAIL".to_owned();
    let forward_contents = vec![
        (
            duplicate_tail.clone(),
            "error".to_owned(),
            vec![
                "ORNA-E-REPLAY-CHILD-A1".to_owned(),
                "ORNA-E-REPLAY-CHILD-A2".to_owned(),
            ],
        ),
        (
            duplicate_tail.clone(),
            "error".to_owned(),
            vec![
                "ORNA-E-REPLAY-CHILD-B1".to_owned(),
                "ORNA-E-REPLAY-CHILD-B2".to_owned(),
            ],
        ),
    ];
    let reverse_contents = vec![
        (
            duplicate_tail.clone(),
            "error".to_owned(),
            vec![
                "ORNA-E-REPLAY-CHILD-B2".to_owned(),
                "ORNA-E-REPLAY-CHILD-B1".to_owned(),
            ],
        ),
        (
            duplicate_tail.clone(),
            "error".to_owned(),
            vec![
                "ORNA-E-REPLAY-CHILD-A2".to_owned(),
                "ORNA-E-REPLAY-CHILD-A1".to_owned(),
            ],
        ),
    ];
    assert_eq!(
        causes.iter().map(tail_contents).collect::<Vec<_>>(),
        vec![
            forward_contents.clone(),
            reverse_contents,
            forward_contents,
        ],
    );

    let json = serde_json::to_vec(&outer).unwrap();
    let encoded = outer.encode_ovb().unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"forward nested duplicate replay payload".as_slice(),
            b"reverse nested duplicate replay payload".as_slice(),
            b"tail A secret".as_slice(),
            b"tail B secret".as_slice(),
            b"ORNA-E-REPLAY-CHILD-A1 secret".as_slice(),
            b"ORNA-E-REPLAY-CHILD-A2 secret".as_slice(),
            b"ORNA-E-REPLAY-CHILD-B1 secret".as_slice(),
            b"ORNA-E-REPLAY-CHILD-B2 secret".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
        assert!(!encoded.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&encoded).unwrap()).unwrap();
    assert_redacted_tree(&decoded);
    assert_eq!(decoded["causes"], projection["causes"]);
}

#[test]
fn nested_duplicate_error_tail_cardinality_edges_survive_replay() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let make_wire = |payload: &str, layout: &str| {
        let child = |label: &str| {
            let leaf_code = format!("ORNA-E-NESTED-DUPLICATE-LEAF-{label}");
            admitted(
                "ORNA-E-NESTED-DUPLICATE-CHILD",
                &format!("{payload} child {label} secret"),
            )
            .with_cause(admitted(
                &leaf_code,
                &format!("{payload} leaf {label} secret"),
            ))
        };
        let tail = |label: &str, child_labels: [&str; 2]| {
            admitted(
                "ORNA-E-NESTED-DUPLICATE-TAIL",
                &format!("{payload} tail {label} secret"),
            )
            .with_cause(child(child_labels[0]))
            .with_cause(child(child_labels[1]))
        };
        let tails = match layout {
            "forward" => vec![tail("A", ["A1", "A2"]), tail("B", ["B1", "B2"])],
            "reverse" => vec![tail("B", ["B2", "B1"]), tail("A", ["A2", "A1"])],
            "single" => vec![tail("A", ["A1", "A2"])],
            "empty" => Vec::new(),
            _ => unreachable!("unknown nested duplicate tail layout"),
        };
        let terminal = tails.into_iter().fold(
            admitted("ORNA-E-NESTED-DUPLICATE-TERMINAL", payload),
            |terminal, tail| terminal.with_cause(tail),
        );
        let nested = admitted("ORNA-E-NESTED-DUPLICATE-NESTED", payload).with_cause(terminal);
        let parent = admitted("ORNA-E-NESTED-DUPLICATE-PARENT", payload).with_cause(nested);
        let branch = admitted("ORNA-E-NESTED-DUPLICATE-BRANCH", payload).with_cause(parent);
        admitted("ORNA-E-NESTED-DUPLICATE-ROOT", payload)
            .with_cause(branch)
            .encode_ovb()
            .unwrap()
    };
    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn terminal(diagnostic: &serde_json::Value) -> &serde_json::Value {
        let mut node = diagnostic;
        for _ in 0..4 {
            node = &node["causes"][0];
        }
        node
    }
    fn nested_leaf_orders(diagnostic: &serde_json::Value) -> Vec<Vec<String>> {
        terminal(diagnostic)["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tail| {
                tail["causes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|child| child["causes"][0]["code"].as_str().unwrap().to_owned())
                    .collect()
            })
            .collect()
    }

    let forward_wire = make_wire("forward nested duplicate order payload", "forward");
    let reverse_wire = make_wire("reverse nested duplicate order payload", "reverse");
    let single_wire = make_wire("single nested duplicate order payload", "single");
    let empty_wire = make_wire("empty nested duplicate order payload", "empty");
    let replay_forward = || Diagnostic::decode_ovb(&forward_wire).unwrap();
    let replay_reverse = || Diagnostic::decode_ovb(&reverse_wire).unwrap();
    let replay_single = || Diagnostic::decode_ovb(&single_wire).unwrap();
    let replay_empty = || Diagnostic::decode_ovb(&empty_wire).unwrap();
    let mut receiver = replay_forward();
    let forward = receiver.clone();
    receiver.clone_from(&replay_single());
    let single_after_forward = receiver.clone();
    receiver.clone_from(&replay_empty());
    let empty_between = receiver.clone();
    receiver.clone_from(&replay_reverse());
    let reverse = receiver.clone();
    receiver.clone_from(&replay_single());
    let single_after_reverse = receiver.clone();
    receiver.clone_from(&replay_forward());
    let restored_forward = receiver.clone();
    assert_eq!(restored_forward, forward);
    assert_eq!(single_after_forward, replay_single());
    assert_eq!(empty_between, replay_empty());
    assert_eq!(single_after_reverse, replay_single());

    // ORNA-SECRET-002 requires redaction but leaves ordering open; retain the
    // nested insertion order across two, one, and zero-tail replay generations.
    let outer = admitted("ORNA-E-NESTED-DUPLICATE-OUTER", "public nested replay admission")
        .with_cause(forward)
        .with_cause(single_after_forward)
        .with_cause(empty_between)
        .with_cause(reverse)
        .with_cause(single_after_reverse)
        .with_cause(restored_forward);
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["severity"], "error");
    assert_eq!(projection["message"], "public nested replay admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 6);
    for cause in causes {
        assert_redacted_tree(cause);
        for tail in terminal(cause)["causes"].as_array().unwrap() {
            assert_eq!(tail["code"], "ORNA-E-NESTED-DUPLICATE-TAIL");
            assert_eq!(tail["severity"], "error");
            assert_eq!(tail["causes"].as_array().unwrap().len(), 2);
            for child in tail["causes"].as_array().unwrap() {
                assert_eq!(child["code"], "ORNA-E-NESTED-DUPLICATE-CHILD");
                assert_eq!(child["severity"], "error");
                assert_eq!(child["causes"].as_array().unwrap().len(), 1);
            }
        }
    }
    let a_order = vec![
        "ORNA-E-NESTED-DUPLICATE-LEAF-A1".to_owned(),
        "ORNA-E-NESTED-DUPLICATE-LEAF-A2".to_owned(),
    ];
    let b_order = vec![
        "ORNA-E-NESTED-DUPLICATE-LEAF-B1".to_owned(),
        "ORNA-E-NESTED-DUPLICATE-LEAF-B2".to_owned(),
    ];
    let reverse_a_order = vec![
        "ORNA-E-NESTED-DUPLICATE-LEAF-A2".to_owned(),
        "ORNA-E-NESTED-DUPLICATE-LEAF-A1".to_owned(),
    ];
    let reverse_b_order = vec![
        "ORNA-E-NESTED-DUPLICATE-LEAF-B2".to_owned(),
        "ORNA-E-NESTED-DUPLICATE-LEAF-B1".to_owned(),
    ];
    assert_eq!(
        causes.iter().map(nested_leaf_orders).collect::<Vec<_>>(),
        vec![
            vec![a_order.clone(), b_order.clone()],
            vec![a_order.clone()],
            vec![],
            vec![reverse_b_order, reverse_a_order],
            vec![a_order.clone()],
            vec![a_order, b_order],
        ],
    );

    let json = serde_json::to_vec(&outer).unwrap();
    let encoded = outer.encode_ovb().unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"forward nested duplicate order payload".as_slice(),
            b"reverse nested duplicate order payload".as_slice(),
            b"single nested duplicate order payload".as_slice(),
            b"empty nested duplicate order payload".as_slice(),
            b"tail A secret".as_slice(),
            b"tail B secret".as_slice(),
            b"child A1 secret".as_slice(),
            b"child A2 secret".as_slice(),
            b"child B1 secret".as_slice(),
            b"child B2 secret".as_slice(),
            b"leaf A1 secret".as_slice(),
            b"leaf A2 secret".as_slice(),
            b"leaf B1 secret".as_slice(),
            b"leaf B2 secret".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
        assert!(!encoded.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&encoded).unwrap()).unwrap();
    assert_redacted_tree(&decoded);
    assert_eq!(decoded["causes"], projection["causes"]);
}

#[test]
fn nested_duplicate_tails_keep_interposed_sibling_order_on_replay() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let make_wire = |payload: &str, reverse: bool| {
        let duplicate_tail = |label: &str| {
            admitted(
                "ORNA-E-NESTED-DUPLICATE-EDGE-TAIL",
                &format!("{payload} duplicate tail {label} secret"),
            )
            .with_cause(admitted(
                &format!("ORNA-E-NESTED-DUPLICATE-EDGE-LEAF-{label}"),
                &format!("{payload} duplicate leaf {label} secret"),
            ))
        };
        let interposed = admitted(
            "ORNA-E-NESTED-DUPLICATE-EDGE-INTERPOSED",
            &format!("{payload} interposed sibling secret"),
        );
        let tails = if reverse {
            vec![duplicate_tail("B"), interposed, duplicate_tail("A")]
        } else {
            vec![duplicate_tail("A"), interposed, duplicate_tail("B")]
        };
        let terminal = tails.into_iter().fold(
            admitted("ORNA-E-NESTED-DUPLICATE-EDGE-TERMINAL", payload),
            |terminal, tail| terminal.with_cause(tail),
        );
        let nested = admitted("ORNA-E-NESTED-DUPLICATE-EDGE-NESTED", payload)
            .with_cause(terminal);
        let parent = admitted("ORNA-E-NESTED-DUPLICATE-EDGE-PARENT", payload)
            .with_cause(nested);
        let branch = admitted("ORNA-E-NESTED-DUPLICATE-EDGE-BRANCH", payload)
            .with_cause(parent);
        admitted("ORNA-E-NESTED-DUPLICATE-EDGE-ROOT", payload)
            .with_cause(branch)
            .encode_ovb()
            .unwrap()
    };
    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn terminal(diagnostic: &serde_json::Value) -> &serde_json::Value {
        let mut node = diagnostic;
        for _ in 0..4 {
            node = &node["causes"][0];
        }
        node
    }
    fn tail_codes(diagnostic: &serde_json::Value) -> Vec<String> {
        terminal(diagnostic)["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tail| tail["code"].as_str().unwrap().to_owned())
            .collect()
    }
    fn tail_leaf_codes(diagnostic: &serde_json::Value) -> Vec<Option<String>> {
        terminal(diagnostic)["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tail| tail["causes"][0]["code"].as_str().map(str::to_owned))
            .collect()
    }

    let forward_wire = make_wire("forward nested duplicate edge payload", false);
    let reverse_wire = make_wire("reverse nested duplicate edge payload", true);
    let replay_forward = || Diagnostic::decode_ovb(&forward_wire).unwrap();
    let replay_reverse = || Diagnostic::decode_ovb(&reverse_wire).unwrap();
    let mut receiver = replay_forward();
    let forward = receiver.clone();
    receiver.clone_from(&replay_reverse());
    let reverse = receiver.clone();
    receiver.clone_from(&replay_forward());
    let restored_forward = receiver.clone();
    assert_eq!(restored_forward, forward);

    // ORNA-SECRET-002 requires redaction but leaves order open; keep duplicate
    // nested tails on their original sides of an interposed sibling on replay.
    let outer = admitted(
        "ORNA-E-NESTED-DUPLICATE-EDGE-OUTER",
        "public nested duplicate edge admission",
    )
    .with_cause(forward)
    .with_cause(reverse)
    .with_cause(restored_forward);
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["severity"], "error");
    assert_eq!(
        projection["message"],
        "public nested duplicate edge admission"
    );
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 3);
    for cause in causes {
        assert_redacted_tree(cause);
    }
    let duplicate = "ORNA-E-NESTED-DUPLICATE-EDGE-TAIL";
    let interposed = "ORNA-E-NESTED-DUPLICATE-EDGE-INTERPOSED";
    assert_eq!(
        causes.iter().map(tail_codes).collect::<Vec<_>>(),
        vec![
            vec![duplicate.to_owned(), interposed.to_owned(), duplicate.to_owned()],
            vec![duplicate.to_owned(), interposed.to_owned(), duplicate.to_owned()],
            vec![duplicate.to_owned(), interposed.to_owned(), duplicate.to_owned()],
        ],
    );
    assert_eq!(
        causes.iter().map(tail_leaf_codes).collect::<Vec<_>>(),
        vec![
            vec![
                Some("ORNA-E-NESTED-DUPLICATE-EDGE-LEAF-A".to_owned()),
                None,
                Some("ORNA-E-NESTED-DUPLICATE-EDGE-LEAF-B".to_owned()),
            ],
            vec![
                Some("ORNA-E-NESTED-DUPLICATE-EDGE-LEAF-B".to_owned()),
                None,
                Some("ORNA-E-NESTED-DUPLICATE-EDGE-LEAF-A".to_owned()),
            ],
            vec![
                Some("ORNA-E-NESTED-DUPLICATE-EDGE-LEAF-A".to_owned()),
                None,
                Some("ORNA-E-NESTED-DUPLICATE-EDGE-LEAF-B".to_owned()),
            ],
        ],
    );

    let json = serde_json::to_vec(&outer).unwrap();
    let encoded = outer.encode_ovb().unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"forward nested duplicate edge payload".as_slice(),
            b"reverse nested duplicate edge payload".as_slice(),
            b"duplicate tail A secret".as_slice(),
            b"duplicate tail B secret".as_slice(),
            b"duplicate leaf A secret".as_slice(),
            b"duplicate leaf B secret".as_slice(),
            b"interposed sibling secret".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
        assert!(!encoded.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&encoded).unwrap()).unwrap();
    assert_redacted_tree(&decoded);
    assert_eq!(decoded["causes"], projection["causes"]);
}

#[test]
fn nested_duplicate_tail_empty_child_shape_survives_replay() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let make_wire = |payload: &str, reverse: bool| {
        let tail = |label: &str, with_leaf: bool| {
            let tail = admitted(
                "ORNA-E-NESTED-DUPLICATE-EMPTY-TAIL",
                &format!("{payload} duplicate tail {label} secret"),
            );
            if with_leaf {
                tail.with_cause(admitted(
                    &format!("ORNA-E-NESTED-DUPLICATE-EMPTY-LEAF-{label}"),
                    &format!("{payload} duplicate leaf {label} secret"),
                ))
            } else {
                tail
            }
        };
        let interposed = admitted(
            "ORNA-E-NESTED-DUPLICATE-EMPTY-INTERPOSED",
            &format!("{payload} interposed sibling secret"),
        );
        let tails = if reverse {
            vec![tail("B", true), interposed, tail("A", false)]
        } else {
            vec![tail("A", false), interposed, tail("B", true)]
        };
        let terminal = tails.into_iter().fold(
            admitted("ORNA-E-NESTED-DUPLICATE-EMPTY-TERMINAL", payload),
            |terminal, tail| terminal.with_cause(tail),
        );
        let nested = admitted("ORNA-E-NESTED-DUPLICATE-EMPTY-NESTED", payload)
            .with_cause(terminal);
        let parent = admitted("ORNA-E-NESTED-DUPLICATE-EMPTY-PARENT", payload)
            .with_cause(nested);
        let branch = admitted("ORNA-E-NESTED-DUPLICATE-EMPTY-BRANCH", payload)
            .with_cause(parent);
        admitted("ORNA-E-NESTED-DUPLICATE-EMPTY-ROOT", payload)
            .with_cause(branch)
            .encode_ovb()
            .unwrap()
    };
    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn terminal(diagnostic: &serde_json::Value) -> &serde_json::Value {
        let mut node = diagnostic;
        for _ in 0..4 {
            node = &node["causes"][0];
        }
        node
    }
    fn tail_shapes(diagnostic: &serde_json::Value) -> Vec<(String, Vec<String>)> {
        terminal(diagnostic)["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tail| {
                (
                    tail["code"].as_str().unwrap().to_owned(),
                    tail["causes"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|child| child["code"].as_str().unwrap().to_owned())
                        .collect(),
                )
            })
            .collect()
    }

    let forward_wire = make_wire("forward empty nested duplicate edge payload", false);
    let reverse_wire = make_wire("reverse empty nested duplicate edge payload", true);
    let replay_forward = || Diagnostic::decode_ovb(&forward_wire).unwrap();
    let replay_reverse = || Diagnostic::decode_ovb(&reverse_wire).unwrap();
    let mut receiver = replay_forward();
    let forward = receiver.clone();
    receiver.clone_from(&replay_reverse());
    let reverse = receiver.clone();
    receiver.clone_from(&replay_forward());
    let restored_forward = receiver.clone();
    assert_eq!(restored_forward, forward);

    // ORNA-SECRET-002 leaves ordering open; preserve insertion order and the
    // empty versus populated child shape when duplicate tails are replayed.
    let outer = admitted(
        "ORNA-E-NESTED-DUPLICATE-EMPTY-OUTER",
        "public empty duplicate edge admission",
    )
    .with_cause(forward)
    .with_cause(reverse)
    .with_cause(restored_forward);
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["severity"], "error");
    assert_eq!(
        projection["message"],
        "public empty duplicate edge admission"
    );
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 3);
    for cause in causes {
        assert_redacted_tree(cause);
    }
    let duplicate = "ORNA-E-NESTED-DUPLICATE-EMPTY-TAIL";
    let interposed = "ORNA-E-NESTED-DUPLICATE-EMPTY-INTERPOSED";
    let empty = (duplicate.to_owned(), vec![]);
    let interposed_empty = (interposed.to_owned(), vec![]);
    let populated = (
        duplicate.to_owned(),
        vec!["ORNA-E-NESTED-DUPLICATE-EMPTY-LEAF-B".to_owned()],
    );
    assert_eq!(
        causes.iter().map(tail_shapes).collect::<Vec<_>>(),
        vec![
            vec![empty.clone(), interposed_empty.clone(), populated.clone()],
            vec![populated, interposed_empty.clone(), empty.clone()],
            vec![
                empty,
                interposed_empty,
                (
                    duplicate.to_owned(),
                    vec!["ORNA-E-NESTED-DUPLICATE-EMPTY-LEAF-B".to_owned()],
                ),
            ],
        ],
    );

    let json = serde_json::to_vec(&outer).unwrap();
    let encoded = outer.encode_ovb().unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"forward empty nested duplicate edge payload".as_slice(),
            b"reverse empty nested duplicate edge payload".as_slice(),
            b"duplicate tail A secret".as_slice(),
            b"duplicate tail B secret".as_slice(),
            b"duplicate leaf B secret".as_slice(),
            b"interposed sibling secret".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
        assert!(!encoded.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&encoded).unwrap()).unwrap();
    assert_redacted_tree(&decoded);
    assert_eq!(decoded["causes"], projection["causes"]);
}

#[test]
fn empty_nested_duplicate_tails_keep_multiplicity_across_replay() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let make_wire = |payload: &str, layout: &str| {
        let duplicate = |label: &str| {
            admitted(
                "ORNA-E-EMPTY-NESTED-DUPLICATE-TAIL",
                &format!("{payload} empty duplicate {label} secret"),
            )
        };
        let interposed = admitted(
            "ORNA-E-EMPTY-NESTED-DUPLICATE-INTERPOSED",
            &format!("{payload} interposed secret"),
        );
        let tails = match layout {
            "pair" => vec![duplicate("A"), interposed.clone(), duplicate("B")],
            "reverse" => vec![duplicate("B"), interposed, duplicate("A")],
            "single" => vec![duplicate("A"), interposed],
            "empty" => Vec::new(),
            _ => unreachable!("unknown empty duplicate tail layout"),
        };
        let terminal = tails.into_iter().fold(
            admitted("ORNA-E-EMPTY-NESTED-DUPLICATE-TERMINAL", payload),
            |terminal, tail| terminal.with_cause(tail),
        );
        let nested = admitted("ORNA-E-EMPTY-NESTED-DUPLICATE-NESTED", payload)
            .with_cause(terminal);
        let parent = admitted("ORNA-E-EMPTY-NESTED-DUPLICATE-PARENT", payload)
            .with_cause(nested);
        let branch = admitted("ORNA-E-EMPTY-NESTED-DUPLICATE-BRANCH", payload)
            .with_cause(parent);
        admitted("ORNA-E-EMPTY-NESTED-DUPLICATE-ROOT", payload)
            .with_cause(branch)
            .encode_ovb()
            .unwrap()
    };
    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn terminal(diagnostic: &serde_json::Value) -> &serde_json::Value {
        let mut node = diagnostic;
        for _ in 0..4 {
            node = &node["causes"][0];
        }
        node
    }
    fn tail_codes(diagnostic: &serde_json::Value) -> Vec<String> {
        terminal(diagnostic)["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tail| tail["code"].as_str().unwrap().to_owned())
            .collect()
    }
    fn tail_child_counts(diagnostic: &serde_json::Value) -> Vec<usize> {
        terminal(diagnostic)["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tail| tail["causes"].as_array().unwrap().len())
            .collect()
    }

    let pair_wire = make_wire("paired empty duplicate payload", "pair");
    let reverse_wire = make_wire("reversed empty duplicate payload", "reverse");
    let single_wire = make_wire("single empty duplicate payload", "single");
    let empty_wire = make_wire("empty duplicate payload", "empty");
    let replay_pair = || Diagnostic::decode_ovb(&pair_wire).unwrap();
    let replay_reverse = || Diagnostic::decode_ovb(&reverse_wire).unwrap();
    let replay_single = || Diagnostic::decode_ovb(&single_wire).unwrap();
    let replay_empty = || Diagnostic::decode_ovb(&empty_wire).unwrap();
    let mut receiver = replay_pair();
    let pair = receiver.clone();
    receiver.clone_from(&replay_single());
    let single_after_pair = receiver.clone();
    receiver.clone_from(&replay_empty());
    let empty_between = receiver.clone();
    receiver.clone_from(&replay_reverse());
    let reverse = receiver.clone();
    receiver.clone_from(&replay_single());
    let single_after_reverse = receiver.clone();
    receiver.clone_from(&replay_pair());
    let restored_pair = receiver.clone();
    assert_eq!(restored_pair, pair);
    assert_eq!(single_after_pair, replay_single());
    assert_eq!(empty_between, replay_empty());
    assert_eq!(single_after_reverse, replay_single());

    // ORNA-SECRET-002 leaves tail order open; preserve insertion order and
    // duplicate count for empty nested tails across replacement generations.
    let outer = admitted(
        "ORNA-E-EMPTY-NESTED-DUPLICATE-OUTER",
        "public empty duplicate admission",
    )
    .with_cause(pair)
    .with_cause(single_after_pair)
    .with_cause(empty_between)
    .with_cause(reverse)
    .with_cause(single_after_reverse)
    .with_cause(restored_pair);
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["severity"], "error");
    assert_eq!(projection["message"], "public empty duplicate admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 6);
    for cause in causes {
        assert_redacted_tree(cause);
    }
    let duplicate = "ORNA-E-EMPTY-NESTED-DUPLICATE-TAIL";
    let interposed = "ORNA-E-EMPTY-NESTED-DUPLICATE-INTERPOSED";
    let pair_codes = vec![duplicate.to_owned(), interposed.to_owned(), duplicate.to_owned()];
    let single_codes = vec![duplicate.to_owned(), interposed.to_owned()];
    assert_eq!(
        causes.iter().map(tail_codes).collect::<Vec<_>>(),
        vec![
            pair_codes.clone(),
            single_codes.clone(),
            vec![],
            pair_codes.clone(),
            single_codes,
            pair_codes,
        ],
    );
    assert_eq!(
        causes.iter().map(tail_child_counts).collect::<Vec<_>>(),
        vec![vec![0, 0, 0], vec![0, 0], vec![], vec![0, 0, 0], vec![0, 0], vec![0, 0, 0]],
    );

    let json = serde_json::to_vec(&outer).unwrap();
    let encoded = outer.encode_ovb().unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"paired empty duplicate payload".as_slice(),
            b"reversed empty duplicate payload".as_slice(),
            b"single empty duplicate payload".as_slice(),
            b"empty duplicate payload".as_slice(),
            b"empty duplicate A secret".as_slice(),
            b"empty duplicate B secret".as_slice(),
            b"interposed secret".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
        assert!(!encoded.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&encoded).unwrap()).unwrap();
    assert_redacted_tree(&decoded);
    assert_eq!(decoded["causes"], projection["causes"]);
}

#[test]
fn empty_nested_duplicate_tails_stay_local_to_sibling_on_replay() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let make_wire = |payload: &str, left_count: usize, right_count: usize| {
        let branch = |name: &str, tail_count: usize| {
            let terminal = (0..tail_count).fold(
                admitted("ORNA-E-EMPTY-DUPLICATE-SIBLING-TERMINAL", payload),
                |terminal, index| {
                    terminal.with_cause(admitted(
                        "ORNA-E-EMPTY-DUPLICATE-SIBLING-TAIL",
                        &format!("{payload} {name} empty duplicate tail {index} secret"),
                    ))
                },
            );
            admitted(
                &format!("ORNA-E-EMPTY-DUPLICATE-SIBLING-{name}"),
                &format!("{payload} {name} branch secret"),
            )
            .with_cause(terminal)
        };
        admitted("ORNA-E-EMPTY-DUPLICATE-SIBLING-ROOT", payload)
            .with_cause(branch("LEFT", left_count))
            .with_cause(branch("RIGHT", right_count))
            .encode_ovb()
            .unwrap()
    };
    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn branch_tail_counts(diagnostic: &serde_json::Value) -> Vec<usize> {
        diagnostic["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|branch| branch["causes"][0]["causes"].as_array().unwrap().len())
            .collect()
    }
    fn assert_empty_duplicate_tails(diagnostic: &serde_json::Value) {
        let branches = diagnostic["causes"].as_array().unwrap();
        assert_eq!(branches.len(), 2);
        assert_eq!(branches[0]["code"], "ORNA-E-EMPTY-DUPLICATE-SIBLING-LEFT");
        assert_eq!(branches[1]["code"], "ORNA-E-EMPTY-DUPLICATE-SIBLING-RIGHT");
        for branch in branches {
            let tails = branch["causes"][0]["causes"].as_array().unwrap();
            for tail in tails {
                assert_eq!(tail["code"], "ORNA-E-EMPTY-DUPLICATE-SIBLING-TAIL");
                assert!(tail["causes"].as_array().unwrap().is_empty());
            }
        }
    }

    let both_wire = make_wire("both empty duplicate branches payload", 2, 2);
    let left_empty_wire = make_wire("left empty duplicate branch payload", 0, 2);
    let right_empty_wire = make_wire("right empty duplicate branch payload", 2, 0);
    let single_wire = make_wire("single empty duplicate branches payload", 1, 1);
    let empty_wire = make_wire("empty duplicate branches payload", 0, 0);
    let replay_both = || Diagnostic::decode_ovb(&both_wire).unwrap();
    let replay_left_empty = || Diagnostic::decode_ovb(&left_empty_wire).unwrap();
    let replay_right_empty = || Diagnostic::decode_ovb(&right_empty_wire).unwrap();
    let replay_single = || Diagnostic::decode_ovb(&single_wire).unwrap();
    let replay_empty = || Diagnostic::decode_ovb(&empty_wire).unwrap();
    let mut receiver = replay_both();
    let both = receiver.clone();
    receiver.clone_from(&replay_left_empty());
    let left_empty = receiver.clone();
    receiver.clone_from(&replay_right_empty());
    let right_empty = receiver.clone();
    receiver.clone_from(&replay_single());
    let single = receiver.clone();
    receiver.clone_from(&replay_empty());
    let empty = receiver.clone();
    receiver.clone_from(&replay_both());
    let restored_both = receiver.clone();
    assert_eq!(restored_both, both);
    assert_eq!(left_empty, replay_left_empty());
    assert_eq!(right_empty, replay_right_empty());
    assert_eq!(single, replay_single());
    assert_eq!(empty, replay_empty());

    // ORNA-SECRET-002 leaves tail order open; preserve duplicate empty tails
    // within each sibling while replacing the other sibling's cause vector.
    let outer = admitted(
        "ORNA-E-EMPTY-DUPLICATE-SIBLING-OUTER",
        "public empty duplicate sibling admission",
    )
    .with_cause(both)
    .with_cause(left_empty)
    .with_cause(right_empty)
    .with_cause(single)
    .with_cause(empty)
    .with_cause(restored_both);
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["severity"], "error");
    assert_eq!(
        projection["message"],
        "public empty duplicate sibling admission"
    );
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 6);
    for cause in causes {
        assert_redacted_tree(cause);
        assert_empty_duplicate_tails(cause);
    }
    assert_eq!(
        causes.iter().map(branch_tail_counts).collect::<Vec<_>>(),
        vec![vec![2, 2], vec![0, 2], vec![2, 0], vec![1, 1], vec![0, 0], vec![2, 2]],
    );

    let json = serde_json::to_vec(&outer).unwrap();
    let encoded = outer.encode_ovb().unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"both empty duplicate branches payload".as_slice(),
            b"left empty duplicate branch payload".as_slice(),
            b"right empty duplicate branch payload".as_slice(),
            b"single empty duplicate branches payload".as_slice(),
            b"empty duplicate branches payload".as_slice(),
            b"LEFT empty duplicate tail 0 secret".as_slice(),
            b"LEFT empty duplicate tail 1 secret".as_slice(),
            b"RIGHT empty duplicate tail 0 secret".as_slice(),
            b"RIGHT empty duplicate tail 1 secret".as_slice(),
            b"LEFT branch secret".as_slice(),
            b"RIGHT branch secret".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
        assert!(!encoded.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&encoded).unwrap()).unwrap();
    assert_redacted_tree(&decoded);
    assert_eq!(decoded["causes"], projection["causes"]);
}

#[test]
fn empty_duplicate_sibling_tails_keep_owner_across_replay() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let make_wire = |payload: &str, left_count: usize, right_count: usize| {
        let sibling = |name: &str, count: usize| {
            let terminal = (0..count).fold(
                admitted(
                    &format!("ORNA-E-EMPTY-DUP-SIBLING-{name}-TERMINAL"),
                    payload,
                ),
                |terminal, index| {
                    terminal.with_cause(admitted(
                        "ORNA-E-EMPTY-DUP-SIBLING-TAIL",
                        &format!("{payload} {name} empty duplicate tail {index} secret"),
                    ))
                },
            );
            admitted(
                "ORNA-E-EMPTY-DUP-SIBLING",
                &format!("{payload} {name} sibling secret"),
            )
            .with_cause(terminal)
        };
        admitted("ORNA-E-EMPTY-DUP-SIBLING-ROOT", payload)
            .with_cause(sibling("LEFT", left_count))
            .with_cause(sibling("RIGHT", right_count))
            .encode_ovb()
            .unwrap()
    };
    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn sibling_tail_counts(diagnostic: &serde_json::Value) -> Vec<usize> {
        diagnostic["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|sibling| sibling["causes"][0]["causes"].as_array().unwrap().len())
            .collect()
    }
    fn assert_sibling_ownership(diagnostic: &serde_json::Value) {
        let siblings = diagnostic["causes"].as_array().unwrap();
        assert_eq!(siblings.len(), 2);
        for sibling in siblings {
            assert_eq!(sibling["code"], "ORNA-E-EMPTY-DUP-SIBLING");
        }
        for (sibling, name) in siblings.iter().zip(["LEFT", "RIGHT"]) {
            let terminal = &sibling["causes"][0];
            assert_eq!(
                terminal["code"],
                format!("ORNA-E-EMPTY-DUP-SIBLING-{name}-TERMINAL")
            );
            for tail in terminal["causes"].as_array().unwrap() {
                assert_eq!(tail["code"], "ORNA-E-EMPTY-DUP-SIBLING-TAIL");
                assert!(tail["causes"].as_array().unwrap().is_empty());
            }
        }
    }

    let left_empty_wire = make_wire("left empty duplicate sibling payload", 0, 2);
    let right_empty_wire = make_wire("right empty duplicate sibling payload", 2, 0);
    let balanced_wire = make_wire("balanced empty duplicate sibling payload", 1, 1);
    let empty_wire = make_wire("empty duplicate sibling payload", 0, 0);
    let replay_left_empty = || Diagnostic::decode_ovb(&left_empty_wire).unwrap();
    let replay_right_empty = || Diagnostic::decode_ovb(&right_empty_wire).unwrap();
    let replay_balanced = || Diagnostic::decode_ovb(&balanced_wire).unwrap();
    let replay_empty = || Diagnostic::decode_ovb(&empty_wire).unwrap();
    let mut receiver = replay_left_empty();
    let left_empty = receiver.clone();
    receiver.clone_from(&replay_right_empty());
    let right_empty = receiver.clone();
    receiver.clone_from(&replay_balanced());
    let balanced = receiver.clone();
    receiver.clone_from(&replay_empty());
    let empty = receiver.clone();
    receiver.clone_from(&replay_left_empty());
    let restored_left_empty = receiver.clone();
    assert_eq!(restored_left_empty, left_empty);
    assert_eq!(right_empty, replay_right_empty());
    assert_eq!(balanced, replay_balanced());
    assert_eq!(empty, replay_empty());

    // ORNA-SECRET-002 leaves ordering open; retain left-to-right insertion
    // and keep empty duplicate tails attached to their nested sibling owner.
    let outer = admitted(
        "ORNA-E-EMPTY-DUP-SIBLING-OUTER",
        "public duplicate sibling admission",
    )
    .with_cause(left_empty)
    .with_cause(right_empty)
    .with_cause(balanced)
    .with_cause(empty)
    .with_cause(restored_left_empty);
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["severity"], "error");
    assert_eq!(projection["message"], "public duplicate sibling admission");
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 5);
    for cause in causes {
        assert_redacted_tree(cause);
        assert_sibling_ownership(cause);
    }
    assert_eq!(
        causes.iter().map(sibling_tail_counts).collect::<Vec<_>>(),
        vec![vec![0, 2], vec![2, 0], vec![1, 1], vec![0, 0], vec![0, 2]],
    );

    let json = serde_json::to_vec(&outer).unwrap();
    let encoded = outer.encode_ovb().unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"left empty duplicate sibling payload".as_slice(),
            b"right empty duplicate sibling payload".as_slice(),
            b"balanced empty duplicate sibling payload".as_slice(),
            b"empty duplicate sibling payload".as_slice(),
            b"LEFT empty duplicate tail 0 secret".as_slice(),
            b"LEFT empty duplicate tail 1 secret".as_slice(),
            b"RIGHT empty duplicate tail 0 secret".as_slice(),
            b"RIGHT empty duplicate tail 1 secret".as_slice(),
            b"LEFT sibling secret".as_slice(),
            b"RIGHT sibling secret".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
        assert!(!encoded.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&encoded).unwrap()).unwrap();
    assert_redacted_tree(&decoded);
    assert_eq!(decoded["causes"], projection["causes"]);
}

#[test]
fn empty_duplicate_sibling_order_keeps_nested_tail_owners_on_replay() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let make_wire = |payload: &str, reverse: bool| {
        let sibling = |name: &str, count: usize| {
            let terminal = (0..count).fold(
                admitted(&format!("ORNA-E-EMPTY-DUP-ORDER-{name}-TERMINAL"), payload),
                |terminal, index| {
                    terminal.with_cause(admitted(
                        "ORNA-E-EMPTY-DUP-ORDER-TAIL",
                        &format!("{payload} {name} empty duplicate tail {index} secret"),
                    ))
                },
            );
            admitted(
                "ORNA-E-EMPTY-DUP-ORDER-SIBLING",
                &format!("{payload} {name} sibling secret"),
            )
            .with_cause(terminal)
        };
        let left = sibling("LEFT", 2);
        let right = sibling("RIGHT", 1);
        let (first, second) = if reverse {
            (right, left)
        } else {
            (left, right)
        };
        admitted("ORNA-E-EMPTY-DUP-ORDER-ROOT", payload)
            .with_cause(first)
            .with_cause(second)
            .encode_ovb()
            .unwrap()
    };
    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn sibling_terminal_codes(diagnostic: &serde_json::Value) -> Vec<String> {
        diagnostic["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|sibling| sibling["causes"][0]["code"].as_str().unwrap().to_owned())
            .collect()
    }
    fn sibling_tail_counts(diagnostic: &serde_json::Value) -> Vec<usize> {
        diagnostic["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|sibling| sibling["causes"][0]["causes"].as_array().unwrap().len())
            .collect()
    }
    fn assert_empty_tail_shapes(diagnostic: &serde_json::Value) {
        for sibling in diagnostic["causes"].as_array().unwrap() {
            assert_eq!(sibling["code"], "ORNA-E-EMPTY-DUP-ORDER-SIBLING");
            for tail in sibling["causes"][0]["causes"].as_array().unwrap() {
                assert_eq!(tail["code"], "ORNA-E-EMPTY-DUP-ORDER-TAIL");
                assert!(tail["causes"].as_array().unwrap().is_empty());
            }
        }
    }

    let forward_wire = make_wire("forward empty duplicate sibling order payload", false);
    let reverse_wire = make_wire("reverse empty duplicate sibling order payload", true);
    let replay_forward = || Diagnostic::decode_ovb(&forward_wire).unwrap();
    let replay_reverse = || Diagnostic::decode_ovb(&reverse_wire).unwrap();
    let mut receiver = replay_forward();
    let forward = receiver.clone();
    receiver.clone_from(&replay_reverse());
    let reverse = receiver.clone();
    receiver.clone_from(&replay_forward());
    let restored_forward = receiver.clone();
    assert_eq!(restored_forward, forward);

    // ORNA-SECRET-002 leaves order open; retain left-to-right insertion order
    // and keep each empty duplicate tail group with its nested sibling.
    let outer = admitted(
        "ORNA-E-EMPTY-DUP-ORDER-OUTER",
        "public empty duplicate sibling order admission",
    )
    .with_cause(forward)
    .with_cause(reverse)
    .with_cause(restored_forward);
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["severity"], "error");
    assert_eq!(
        projection["message"],
        "public empty duplicate sibling order admission"
    );
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 3);
    for cause in causes {
        assert_redacted_tree(cause);
        assert_empty_tail_shapes(cause);
    }
    assert_eq!(
        causes.iter().map(sibling_terminal_codes).collect::<Vec<_>>(),
        vec![
            vec![
                "ORNA-E-EMPTY-DUP-ORDER-LEFT-TERMINAL".to_owned(),
                "ORNA-E-EMPTY-DUP-ORDER-RIGHT-TERMINAL".to_owned(),
            ],
            vec![
                "ORNA-E-EMPTY-DUP-ORDER-RIGHT-TERMINAL".to_owned(),
                "ORNA-E-EMPTY-DUP-ORDER-LEFT-TERMINAL".to_owned(),
            ],
            vec![
                "ORNA-E-EMPTY-DUP-ORDER-LEFT-TERMINAL".to_owned(),
                "ORNA-E-EMPTY-DUP-ORDER-RIGHT-TERMINAL".to_owned(),
            ],
        ],
    );
    assert_eq!(
        causes.iter().map(sibling_tail_counts).collect::<Vec<_>>(),
        vec![vec![2, 1], vec![1, 2], vec![2, 1]],
    );

    let json = serde_json::to_vec(&outer).unwrap();
    let encoded = outer.encode_ovb().unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"forward empty duplicate sibling order payload".as_slice(),
            b"reverse empty duplicate sibling order payload".as_slice(),
            b"LEFT empty duplicate tail 0 secret".as_slice(),
            b"LEFT empty duplicate tail 1 secret".as_slice(),
            b"RIGHT empty duplicate tail 0 secret".as_slice(),
            b"LEFT sibling secret".as_slice(),
            b"RIGHT sibling secret".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
        assert!(!encoded.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&encoded).unwrap()).unwrap();
    assert_redacted_tree(&decoded);
    assert_eq!(decoded["causes"], projection["causes"]);
}

#[test]
fn duplicate_sibling_tail_groups_survive_reversed_nested_replay() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let make_wire = |payload: &str, reverse: bool| {
        let sibling = |name: &str| {
            let tail_a = || {
                admitted(
                    "ORNA-E-DUP-SIBLING-TAIL",
                    &format!("{payload} {name} first duplicate tail secret"),
                )
                .with_cause(admitted(
                    &format!("ORNA-E-DUP-SIBLING-{name}-A-CHILD"),
                    &format!("{payload} {name} A child secret"),
                ))
            };
            let tail_b = || {
                admitted(
                    "ORNA-E-DUP-SIBLING-TAIL",
                    &format!("{payload} {name} second duplicate tail secret"),
                )
                .with_cause(admitted(
                    &format!("ORNA-E-DUP-SIBLING-{name}-B1-CHILD"),
                    &format!("{payload} {name} B1 child secret"),
                ))
                .with_cause(admitted(
                    &format!("ORNA-E-DUP-SIBLING-{name}-B2-CHILD"),
                    &format!("{payload} {name} B2 child secret"),
                ))
            };
            let tails = if reverse {
                vec![tail_b(), tail_a()]
            } else {
                vec![tail_a(), tail_b()]
            };
            let terminal = tails.into_iter().fold(
                admitted("ORNA-E-DUP-SIBLING-TERMINAL", payload),
                |terminal, tail| terminal.with_cause(tail),
            );
            admitted(
                "ORNA-E-DUP-SIBLING-BRANCH",
                &format!("{payload} {name} sibling secret"),
            )
            .with_cause(terminal)
        };
        let left = sibling("LEFT");
        let right = sibling("RIGHT");
        let siblings = if reverse {
            vec![right, left]
        } else {
            vec![left, right]
        };
        siblings.into_iter().fold(
            admitted("ORNA-E-DUP-SIBLING-ROOT", payload),
            |root, sibling| root.with_cause(sibling),
        )
        .encode_ovb()
        .unwrap()
    };
    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn sibling_tail_children(diagnostic: &serde_json::Value) -> Vec<Vec<Vec<String>>> {
        let siblings = diagnostic["causes"].as_array().unwrap();
        assert_eq!(siblings.len(), 2);
        siblings
            .iter()
            .map(|sibling| {
                assert_eq!(sibling["code"], "ORNA-E-DUP-SIBLING-BRANCH");
                let terminal = &sibling["causes"][0];
                assert_eq!(terminal["code"], "ORNA-E-DUP-SIBLING-TERMINAL");
                terminal["causes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|tail| {
                        assert_eq!(tail["code"], "ORNA-E-DUP-SIBLING-TAIL");
                        tail["causes"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|child| child["code"].as_str().unwrap().to_owned())
                            .collect()
                    })
                    .collect()
            })
            .collect()
    }

    let forward_wire = make_wire("forward duplicate sibling tail payload", false);
    let reverse_wire = make_wire("reverse duplicate sibling tail payload", true);
    let replay_forward = || Diagnostic::decode_ovb(&forward_wire).unwrap();
    let replay_reverse = || Diagnostic::decode_ovb(&reverse_wire).unwrap();
    let mut receiver = replay_forward();
    let forward = receiver.clone();
    receiver.clone_from(&replay_reverse());
    let reverse = receiver.clone();
    receiver.clone_from(&replay_forward());
    let restored_forward = receiver.clone();
    assert_eq!(restored_forward, forward);

    // ORNA-SECRET-002 leaves same-code sibling and tail ordering open; retain
    // insertion order so each nested duplicate group stays with its sibling.
    let outer = admitted(
        "ORNA-E-DUP-SIBLING-OUTER",
        "public duplicate sibling tail admission",
    )
    .with_cause(forward)
    .with_cause(reverse)
    .with_cause(restored_forward);
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["severity"], "error");
    assert_eq!(
        projection["message"],
        "public duplicate sibling tail admission"
    );
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 3);
    for cause in causes {
        assert_redacted_tree(cause);
    }
    assert_eq!(
        causes.iter().map(sibling_tail_children).collect::<Vec<_>>(),
        vec![
            vec![
                vec![
                    vec!["ORNA-E-DUP-SIBLING-LEFT-A-CHILD".to_owned()],
                    vec![
                        "ORNA-E-DUP-SIBLING-LEFT-B1-CHILD".to_owned(),
                        "ORNA-E-DUP-SIBLING-LEFT-B2-CHILD".to_owned(),
                    ],
                ],
                vec![
                    vec!["ORNA-E-DUP-SIBLING-RIGHT-A-CHILD".to_owned()],
                    vec![
                        "ORNA-E-DUP-SIBLING-RIGHT-B1-CHILD".to_owned(),
                        "ORNA-E-DUP-SIBLING-RIGHT-B2-CHILD".to_owned(),
                    ],
                ],
            ],
            vec![
                vec![
                    vec!["ORNA-E-DUP-SIBLING-RIGHT-B1-CHILD".to_owned(), "ORNA-E-DUP-SIBLING-RIGHT-B2-CHILD".to_owned()],
                    vec!["ORNA-E-DUP-SIBLING-RIGHT-A-CHILD".to_owned()],
                ],
                vec![
                    vec!["ORNA-E-DUP-SIBLING-LEFT-B1-CHILD".to_owned(), "ORNA-E-DUP-SIBLING-LEFT-B2-CHILD".to_owned()],
                    vec!["ORNA-E-DUP-SIBLING-LEFT-A-CHILD".to_owned()],
                ],
            ],
            vec![
                vec![
                    vec!["ORNA-E-DUP-SIBLING-LEFT-A-CHILD".to_owned()],
                    vec![
                        "ORNA-E-DUP-SIBLING-LEFT-B1-CHILD".to_owned(),
                        "ORNA-E-DUP-SIBLING-LEFT-B2-CHILD".to_owned(),
                    ],
                ],
                vec![
                    vec!["ORNA-E-DUP-SIBLING-RIGHT-A-CHILD".to_owned()],
                    vec![
                        "ORNA-E-DUP-SIBLING-RIGHT-B1-CHILD".to_owned(),
                        "ORNA-E-DUP-SIBLING-RIGHT-B2-CHILD".to_owned(),
                    ],
                ],
            ],
        ],
    );

    let json = serde_json::to_vec(&outer).unwrap();
    let encoded = outer.encode_ovb().unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"forward duplicate sibling tail payload".as_slice(),
            b"reverse duplicate sibling tail payload".as_slice(),
            b"LEFT first duplicate tail secret".as_slice(),
            b"LEFT second duplicate tail secret".as_slice(),
            b"RIGHT first duplicate tail secret".as_slice(),
            b"RIGHT second duplicate tail secret".as_slice(),
            b"LEFT A child secret".as_slice(),
            b"LEFT B1 child secret".as_slice(),
            b"LEFT B2 child secret".as_slice(),
            b"RIGHT A child secret".as_slice(),
            b"RIGHT B1 child secret".as_slice(),
            b"RIGHT B2 child secret".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
        assert!(!encoded.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&encoded).unwrap()).unwrap();
    assert_redacted_tree(&decoded);
    assert_eq!(decoded["causes"], projection["causes"]);
}

#[test]
fn duplicate_sibling_tail_child_shapes_follow_order_on_replay() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let admitted = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
        .redacted_with_message(SafeText::new(message).unwrap())
    };
    let make_wire = |payload: &str, reverse_siblings: bool| {
        let sibling = |name: &str| {
            let narrow_tail = || {
                admitted(
                    "ORNA-E-DUP-SIBLING-SHAPE-TAIL",
                    &format!("{payload} {name} narrow duplicate tail secret"),
                )
                .with_cause(admitted(
                    "ORNA-E-DUP-SIBLING-SHAPE-CHILD",
                    &format!("{payload} {name} narrow child secret"),
                ))
            };
            let wide_tail = || {
                admitted(
                    "ORNA-E-DUP-SIBLING-SHAPE-TAIL",
                    &format!("{payload} {name} wide duplicate tail secret"),
                )
                .with_cause(admitted(
                    "ORNA-E-DUP-SIBLING-SHAPE-CHILD",
                    &format!("{payload} {name} wide child one secret"),
                ))
                .with_cause(admitted(
                    "ORNA-E-DUP-SIBLING-SHAPE-CHILD",
                    &format!("{payload} {name} wide child two secret"),
                ))
            };
            let tails = if name == "LEFT" {
                vec![narrow_tail(), wide_tail()]
            } else {
                vec![wide_tail(), narrow_tail()]
            };
            let terminal = tails.into_iter().fold(
                admitted("ORNA-E-DUP-SIBLING-SHAPE-TERMINAL", payload),
                |terminal, tail| terminal.with_cause(tail),
            );
            admitted(
                "ORNA-E-DUP-SIBLING-SHAPE-BRANCH",
                &format!("{payload} {name} sibling secret"),
            )
            .with_cause(terminal)
        };
        let left = sibling("LEFT");
        let right = sibling("RIGHT");
        let siblings = if reverse_siblings {
            vec![right, left]
        } else {
            vec![left, right]
        };
        siblings.into_iter().fold(
            admitted("ORNA-E-DUP-SIBLING-SHAPE-ROOT", payload),
            |root, sibling| root.with_cause(sibling),
        )
        .encode_ovb()
        .unwrap()
    };
    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn sibling_tail_child_counts(diagnostic: &serde_json::Value) -> Vec<Vec<usize>> {
        let siblings = diagnostic["causes"].as_array().unwrap();
        assert_eq!(siblings.len(), 2);
        siblings
            .iter()
            .map(|sibling| {
                assert_eq!(sibling["code"], "ORNA-E-DUP-SIBLING-SHAPE-BRANCH");
                let terminal = &sibling["causes"][0];
                assert_eq!(terminal["code"], "ORNA-E-DUP-SIBLING-SHAPE-TERMINAL");
                terminal["causes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|tail| {
                        assert_eq!(tail["code"], "ORNA-E-DUP-SIBLING-SHAPE-TAIL");
                        let children = tail["causes"].as_array().unwrap();
                        for child in children {
                            assert_eq!(child["code"], "ORNA-E-DUP-SIBLING-SHAPE-CHILD");
                            assert!(child["causes"].as_array().unwrap().is_empty());
                        }
                        children.len()
                    })
                    .collect()
            })
            .collect()
    }

    let forward_wire = make_wire("forward duplicate sibling child-shape payload", false);
    let reverse_wire = make_wire("reverse duplicate sibling child-shape payload", true);
    let replay_forward = || Diagnostic::decode_ovb(&forward_wire).unwrap();
    let replay_reverse = || Diagnostic::decode_ovb(&reverse_wire).unwrap();
    let mut receiver = replay_forward();
    let forward = receiver.clone();
    receiver.clone_from(&replay_reverse());
    let reverse = receiver.clone();
    receiver.clone_from(&replay_forward());
    let restored_forward = receiver.clone();
    assert_eq!(restored_forward, forward);

    // ORNA-SECRET-002 leaves same-code sibling/tail order open; preserve
    // insertion order when only nested duplicate child counts distinguish groups.
    let outer = admitted(
        "ORNA-E-DUP-SIBLING-SHAPE-OUTER",
        "public duplicate sibling shape admission",
    )
    .with_cause(forward)
    .with_cause(reverse)
    .with_cause(restored_forward);
    let projection = serde_json::to_value(&outer).unwrap();
    assert_eq!(projection["severity"], "error");
    assert_eq!(
        projection["message"],
        "public duplicate sibling shape admission"
    );
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 3);
    for cause in causes {
        assert_redacted_tree(cause);
    }
    assert_eq!(
        causes
            .iter()
            .map(sibling_tail_child_counts)
            .collect::<Vec<_>>(),
        vec![vec![vec![1, 2], vec![2, 1]], vec![vec![2, 1], vec![1, 2]], vec![vec![1, 2], vec![2, 1]]],
    );

    let json = serde_json::to_vec(&outer).unwrap();
    let encoded = outer.encode_ovb().unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"forward duplicate sibling child-shape payload".as_slice(),
            b"reverse duplicate sibling child-shape payload".as_slice(),
            b"LEFT narrow duplicate tail secret".as_slice(),
            b"LEFT wide duplicate tail secret".as_slice(),
            b"RIGHT narrow duplicate tail secret".as_slice(),
            b"RIGHT wide duplicate tail secret".as_slice(),
            b"LEFT narrow child secret".as_slice(),
            b"LEFT wide child one secret".as_slice(),
            b"LEFT wide child two secret".as_slice(),
            b"RIGHT narrow child secret".as_slice(),
            b"RIGHT wide child one secret".as_slice(),
            b"RIGHT wide child two secret".as_slice(),
            b"LEFT sibling secret".as_slice(),
            b"RIGHT sibling secret".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
        assert!(!encoded.windows(disclosure.len()).any(|window| window == disclosure));
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&encoded).unwrap()).unwrap();
    assert_redacted_tree(&decoded);
    assert_eq!(decoded["causes"], projection["causes"]);
}

#[test]
fn shorthand_redaction_preserves_duplicate_sibling_tail_shapes_on_replay() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let diagnostic = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new(message).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let make_wire = |payload: &str, reverse_siblings: bool| {
        let sibling = |side: &str| {
            let narrow_tail = || {
                diagnostic(
                    "ORNA-E-SHORT-DUP-SIBLING-TAIL",
                    &format!("{payload} {side} narrow tail secret"),
                )
                .with_cause(diagnostic(
                    &format!("ORNA-E-SHORT-DUP-SIBLING-{side}-NARROW-CHILD"),
                    &format!("{payload} {side} narrow child secret"),
                ))
            };
            let wide_tail = || {
                diagnostic(
                    "ORNA-E-SHORT-DUP-SIBLING-TAIL",
                    &format!("{payload} {side} wide tail secret"),
                )
                .with_cause(diagnostic(
                    &format!("ORNA-E-SHORT-DUP-SIBLING-{side}-WIDE-CHILD-1"),
                    &format!("{payload} {side} wide child one secret"),
                ))
                .with_cause(diagnostic(
                    &format!("ORNA-E-SHORT-DUP-SIBLING-{side}-WIDE-CHILD-2"),
                    &format!("{payload} {side} wide child two secret"),
                ))
            };
            let tails = if side == "LEFT" {
                vec![narrow_tail(), wide_tail()]
            } else {
                vec![wide_tail(), narrow_tail()]
            };
            let terminal = tails.into_iter().fold(
                diagnostic("ORNA-E-SHORT-DUP-SIBLING-TERMINAL", payload),
                |terminal, tail| terminal.with_cause(tail),
            );
            diagnostic(
                "ORNA-E-SHORT-DUP-SIBLING-BRANCH",
                &format!("{payload} {side} sibling secret"),
            )
            .with_cause(terminal)
        };
        let left = sibling("LEFT");
        let right = sibling("RIGHT");
        let siblings = if reverse_siblings {
            vec![right, left]
        } else {
            vec![left, right]
        };
        siblings
            .into_iter()
            .fold(
                diagnostic("ORNA-E-SHORT-DUP-SIBLING-ROOT", payload),
                |root, sibling| root.with_cause(sibling),
            )
            .redacted()
            .encode_ovb()
            .unwrap()
    };
    fn assert_shorthand_redacted(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for note in diagnostic["notes"].as_array().unwrap() {
            assert_eq!(note, "<redacted>");
        }
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_shorthand_redacted(cause);
        }
    }
    fn duplicate_tail_child_codes(diagnostic: &serde_json::Value) -> Vec<Vec<Vec<String>>> {
        let siblings = diagnostic["causes"].as_array().unwrap();
        assert_eq!(siblings.len(), 2);
        siblings
            .iter()
            .map(|sibling| {
                assert_eq!(sibling["code"], "ORNA-E-SHORT-DUP-SIBLING-BRANCH");
                let terminal = &sibling["causes"][0];
                assert_eq!(terminal["code"], "ORNA-E-SHORT-DUP-SIBLING-TERMINAL");
                let tails = terminal["causes"].as_array().unwrap();
                assert_eq!(tails.len(), 2);
                tails
                    .iter()
                    .map(|tail| {
                        assert_eq!(tail["code"], "ORNA-E-SHORT-DUP-SIBLING-TAIL");
                        tail["causes"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|child| child["code"].as_str().unwrap().to_owned())
                            .collect()
                    })
                    .collect()
            })
            .collect()
    }

    let forward_wire = make_wire("forward shorthand duplicate payload", false);
    let reverse_wire = make_wire("reverse shorthand duplicate payload", true);
    let forward = Diagnostic::decode_ovb(&forward_wire).unwrap();
    let reverse = Diagnostic::decode_ovb(&reverse_wire).unwrap();
    let mut receiver = forward.clone();
    receiver.clone_from(&reverse);
    assert_eq!(
        duplicate_tail_child_codes(&serde_json::to_value(&receiver).unwrap()),
        vec![
            vec![
                vec![
                    "ORNA-E-SHORT-DUP-SIBLING-RIGHT-WIDE-CHILD-1".to_owned(),
                    "ORNA-E-SHORT-DUP-SIBLING-RIGHT-WIDE-CHILD-2".to_owned(),
                ],
                vec!["ORNA-E-SHORT-DUP-SIBLING-RIGHT-NARROW-CHILD".to_owned()],
            ],
            vec![
                vec!["ORNA-E-SHORT-DUP-SIBLING-LEFT-NARROW-CHILD".to_owned()],
                vec![
                    "ORNA-E-SHORT-DUP-SIBLING-LEFT-WIDE-CHILD-1".to_owned(),
                    "ORNA-E-SHORT-DUP-SIBLING-LEFT-WIDE-CHILD-2".to_owned(),
                ],
            ],
        ]
    );
    receiver.clone_from(&forward);
    assert_eq!(receiver, forward);

    // ORNA-SECRET-002 leaves same-code sibling and tail order unspecified;
    // retain construction order while shorthand redaction keeps duplicate groups intact.
    let projection = serde_json::to_value(&forward).unwrap();
    assert_shorthand_redacted(&projection);
    assert_eq!(
        duplicate_tail_child_codes(&projection),
        vec![
            vec![
                vec!["ORNA-E-SHORT-DUP-SIBLING-LEFT-NARROW-CHILD".to_owned()],
                vec![
                    "ORNA-E-SHORT-DUP-SIBLING-LEFT-WIDE-CHILD-1".to_owned(),
                    "ORNA-E-SHORT-DUP-SIBLING-LEFT-WIDE-CHILD-2".to_owned(),
                ],
            ],
            vec![
                vec![
                    "ORNA-E-SHORT-DUP-SIBLING-RIGHT-WIDE-CHILD-1".to_owned(),
                    "ORNA-E-SHORT-DUP-SIBLING-RIGHT-WIDE-CHILD-2".to_owned(),
                ],
                vec!["ORNA-E-SHORT-DUP-SIBLING-RIGHT-NARROW-CHILD".to_owned()],
            ],
        ]
    );

    let json = serde_json::to_vec(&forward).unwrap();
    let reverse_projection = serde_json::to_value(&reverse).unwrap();
    assert_shorthand_redacted(&reverse_projection);
    let reverse_json = serde_json::to_vec(&reverse).unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"forward shorthand duplicate payload".as_slice(),
            b"reverse shorthand duplicate payload".as_slice(),
            b"LEFT narrow tail secret".as_slice(),
            b"LEFT wide tail secret".as_slice(),
            b"RIGHT narrow tail secret".as_slice(),
            b"RIGHT wide tail secret".as_slice(),
            b"LEFT narrow child secret".as_slice(),
            b"LEFT wide child one secret".as_slice(),
            b"LEFT wide child two secret".as_slice(),
            b"RIGHT narrow child secret".as_slice(),
            b"RIGHT wide child one secret".as_slice(),
            b"RIGHT wide child two secret".as_slice(),
            b"LEFT sibling secret".as_slice(),
            b"RIGHT sibling secret".as_slice(),
        ])
    {
        assert!(!json.windows(disclosure.len()).any(|window| window == disclosure));
        assert!(!reverse_json
            .windows(disclosure.len())
            .any(|window| window == disclosure));
        for wire in [&forward_wire, &reverse_wire] {
            assert!(!wire
                .windows(disclosure.len())
                .any(|window| window == disclosure));
        }
    }
    let decoded = serde_json::to_value(Diagnostic::decode_ovb(&forward_wire).unwrap()).unwrap();
    assert_shorthand_redacted(&decoded);
    assert_eq!(decoded["causes"], projection["causes"]);
}

#[test]
fn pre_redacted_duplicate_tails_keep_sibling_order_on_replay() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let diagnostic = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new(message).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let make_wire = |payload: &str, reverse: bool| {
        let sibling = |side: &str| {
            let tail = |slot: &str, child_count: usize| {
                let tail = (0..child_count).fold(
                    diagnostic(
                        "ORNA-E-SHORTHAND-DUP-SIBLING-TAIL",
                        &format!("{payload} {side} {slot} duplicate tail secret"),
                    ),
                    |tail, child| {
                        tail.with_cause(diagnostic(
                            &format!("ORNA-E-SHORTHAND-DUP-SIBLING-{side}-{slot}-CHILD-{child}"),
                            &format!("{payload} {side} {slot} child {child} secret"),
                        ))
                    },
                );
                // Apply the shorthand before this duplicate tail is attached to its parent.
                tail.redacted()
            };
            let tail_specs = match side {
                "LEFT" => [("A", 1), ("B", 0)],
                "RIGHT" => [("A", 2), ("B", 1)],
                _ => unreachable!(),
            };
            let mut tails = tail_specs
                .into_iter()
                .map(|(slot, child_count)| tail(slot, child_count))
                .collect::<Vec<_>>();
            if reverse {
                tails.reverse();
            }
            let terminal = tails.into_iter().fold(
                diagnostic("ORNA-E-SHORTHAND-DUP-SIBLING-TERMINAL", payload),
                |terminal, tail| terminal.with_cause(tail),
            );
            diagnostic(
                "ORNA-E-SHORTHAND-DUP-SIBLING-BRANCH",
                &format!("{payload} {side} sibling secret"),
            )
            .with_cause(terminal)
        };
        let left = sibling("LEFT");
        let right = sibling("RIGHT");
        let siblings = if reverse {
            vec![right, left]
        } else {
            vec![left, right]
        };
        siblings
            .into_iter()
            .fold(
                diagnostic("ORNA-E-SHORTHAND-DUP-SIBLING-ROOT", payload),
                |root, sibling| root.with_cause(sibling),
            )
            .redacted()
            .encode_ovb()
            .unwrap()
    };
    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for note in diagnostic["notes"].as_array().unwrap() {
            assert_eq!(note, "<redacted>");
        }
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn tail_children_by_sibling(diagnostic: &serde_json::Value) -> Vec<Vec<Vec<String>>> {
        let siblings = diagnostic["causes"].as_array().unwrap();
        assert_eq!(siblings.len(), 2);
        siblings
            .iter()
            .map(|sibling| {
                assert_eq!(sibling["code"], "ORNA-E-SHORTHAND-DUP-SIBLING-BRANCH");
                let terminal = &sibling["causes"][0];
                assert_eq!(terminal["code"], "ORNA-E-SHORTHAND-DUP-SIBLING-TERMINAL");
                let tails = terminal["causes"].as_array().unwrap();
                assert_eq!(tails.len(), 2);
                tails
                    .iter()
                    .map(|tail| {
                        assert_eq!(tail["code"], "ORNA-E-SHORTHAND-DUP-SIBLING-TAIL");
                        tail["causes"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|child| {
                                assert!(child["causes"].as_array().unwrap().is_empty());
                                child["code"].as_str().unwrap().to_owned()
                            })
                            .collect()
                    })
                    .collect()
            })
            .collect()
    }

    let forward_wire = make_wire("forward shorthand tail payload", false);
    let reverse_wire = make_wire("reverse shorthand tail payload", true);
    let forward = Diagnostic::decode_ovb(&forward_wire).unwrap();
    let reverse = Diagnostic::decode_ovb(&reverse_wire).unwrap();
    let forward_projection = serde_json::to_value(&forward).unwrap();
    let reverse_projection = serde_json::to_value(&reverse).unwrap();
    assert_redacted_tree(&forward_projection);
    assert_redacted_tree(&reverse_projection);

    // ORNA-SECRET-002 leaves same-code sibling/tail order unspecified; retain
    // construction order to track each pre-redacted duplicate tail by its children.
    let left_shapes = vec![
        vec!["ORNA-E-SHORTHAND-DUP-SIBLING-LEFT-A-CHILD-0".to_owned()],
        vec![],
    ];
    let right_shapes = vec![
        vec![
            "ORNA-E-SHORTHAND-DUP-SIBLING-RIGHT-A-CHILD-0".to_owned(),
            "ORNA-E-SHORTHAND-DUP-SIBLING-RIGHT-A-CHILD-1".to_owned(),
        ],
        vec!["ORNA-E-SHORTHAND-DUP-SIBLING-RIGHT-B-CHILD-0".to_owned()],
    ];
    assert_eq!(
        tail_children_by_sibling(&forward_projection),
        vec![left_shapes.clone(), right_shapes.clone()]
    );
    assert_eq!(
        tail_children_by_sibling(&reverse_projection),
        vec![
            vec![right_shapes[1].clone(), right_shapes[0].clone()],
            vec![left_shapes[1].clone(), left_shapes[0].clone()],
        ]
    );

    let mut receiver = forward.clone();
    receiver.clone_from(&reverse);
    assert_eq!(
        tail_children_by_sibling(&serde_json::to_value(&receiver).unwrap()),
        tail_children_by_sibling(&reverse_projection)
    );
    receiver.clone_from(&forward);
    assert_eq!(receiver, forward);

    let forward_json = serde_json::to_vec(&forward).unwrap();
    let reverse_json = serde_json::to_vec(&reverse).unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"forward shorthand tail payload".as_slice(),
            b"reverse shorthand tail payload".as_slice(),
            b"LEFT A duplicate tail secret".as_slice(),
            b"RIGHT A duplicate tail secret".as_slice(),
            b"RIGHT B duplicate tail secret".as_slice(),
            b"LEFT A child 0 secret".as_slice(),
            b"RIGHT A child 0 secret".as_slice(),
            b"RIGHT A child 1 secret".as_slice(),
            b"RIGHT B child 0 secret".as_slice(),
            b"LEFT sibling secret".as_slice(),
            b"RIGHT sibling secret".as_slice(),
        ])
    {
        for bytes in [&forward_json, &reverse_json, &forward_wire, &reverse_wire] {
            assert!(!bytes
                .windows(disclosure.len())
                .any(|window| window == disclosure));
        }
    }
    assert_eq!(
        serde_json::to_value(Diagnostic::decode_ovb(&forward_wire).unwrap()).unwrap()["causes"],
        forward_projection["causes"]
    );
    assert_eq!(
        serde_json::to_value(Diagnostic::decode_ovb(&reverse_wire).unwrap()).unwrap()["causes"],
        reverse_projection["causes"]
    );
}

#[test]
fn shorthand_redacted_siblings_keep_duplicate_tail_shapes_when_composed() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let diagnostic = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new(message).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let make_wire = |payload: &str, reverse: bool| {
        let sibling = |side: &str| {
            let tail = |slot: &str, child_count: usize| {
                (0..child_count).fold(
                    diagnostic(
                        "ORNA-E-SHORTHAND-SIBLING-DUP-TAIL",
                        &format!("{payload} {side} {slot} duplicate tail secret"),
                    ),
                    |tail, child| {
                        tail.with_cause(diagnostic(
                            &format!("ORNA-E-SHORTHAND-SIBLING-{side}-{slot}-CHILD-{child}"),
                            &format!("{payload} {side} {slot} child {child} secret"),
                        ))
                    },
                )
            };
            let tail_specs = match side {
                "LEFT" => [("A", 0), ("B", 2)],
                "RIGHT" => [("A", 1), ("B", 0)],
                _ => unreachable!(),
            };
            let mut tails = tail_specs
                .into_iter()
                .map(|(slot, child_count)| tail(slot, child_count))
                .collect::<Vec<_>>();
            if reverse {
                tails.reverse();
            }
            let terminal = tails.into_iter().fold(
                diagnostic("ORNA-E-SHORTHAND-SIBLING-DUP-TERMINAL", payload),
                |terminal, tail| terminal.with_cause(tail),
            );
            // Redact each sibling after building its duplicate tails and before
            // attaching that sibling beside another equal-code branch.
            diagnostic(
                "ORNA-E-SHORTHAND-SIBLING-DUP-BRANCH",
                &format!("{payload} {side} sibling secret"),
            )
            .with_cause(terminal)
            .redacted()
        };
        let left = sibling("LEFT");
        let right = sibling("RIGHT");
        let siblings = if reverse {
            vec![right, left]
        } else {
            vec![left, right]
        };
        siblings
            .into_iter()
            .fold(
                diagnostic("ORNA-E-SHORTHAND-SIBLING-DUP-ROOT", payload),
                |root, sibling| root.with_cause(sibling),
            )
            .redacted()
            .encode_ovb()
            .unwrap()
    };
    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for note in diagnostic["notes"].as_array().unwrap() {
            assert_eq!(note, "<redacted>");
        }
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn tail_children_by_sibling(diagnostic: &serde_json::Value) -> Vec<Vec<Vec<String>>> {
        let siblings = diagnostic["causes"].as_array().unwrap();
        assert_eq!(siblings.len(), 2);
        siblings
            .iter()
            .map(|sibling| {
                assert_eq!(sibling["code"], "ORNA-E-SHORTHAND-SIBLING-DUP-BRANCH");
                let terminal = &sibling["causes"][0];
                assert_eq!(terminal["code"], "ORNA-E-SHORTHAND-SIBLING-DUP-TERMINAL");
                let tails = terminal["causes"].as_array().unwrap();
                assert_eq!(tails.len(), 2);
                tails
                    .iter()
                    .map(|tail| {
                        assert_eq!(tail["code"], "ORNA-E-SHORTHAND-SIBLING-DUP-TAIL");
                        tail["causes"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|child| {
                                assert!(child["causes"].as_array().unwrap().is_empty());
                                child["code"].as_str().unwrap().to_owned()
                            })
                            .collect()
                    })
                    .collect()
            })
            .collect()
    }

    let forward_wire = make_wire("forward shorthand sibling payload", false);
    let reverse_wire = make_wire("reverse shorthand sibling payload", true);
    let forward = Diagnostic::decode_ovb(&forward_wire).unwrap();
    let reverse = Diagnostic::decode_ovb(&reverse_wire).unwrap();
    let forward_projection = serde_json::to_value(&forward).unwrap();
    let reverse_projection = serde_json::to_value(&reverse).unwrap();
    assert_redacted_tree(&forward_projection);
    assert_redacted_tree(&reverse_projection);

    // ORNA-SECRET-002 leaves equal-code sibling/tail order unspecified; retain
    // construction order to track duplicate groups by their child shapes.
    let left_shapes = vec![
        vec![],
        vec![
            "ORNA-E-SHORTHAND-SIBLING-LEFT-B-CHILD-0".to_owned(),
            "ORNA-E-SHORTHAND-SIBLING-LEFT-B-CHILD-1".to_owned(),
        ],
    ];
    let right_shapes = vec![
        vec!["ORNA-E-SHORTHAND-SIBLING-RIGHT-A-CHILD-0".to_owned()],
        vec![],
    ];
    assert_eq!(
        tail_children_by_sibling(&forward_projection),
        vec![left_shapes.clone(), right_shapes.clone()]
    );
    assert_eq!(
        tail_children_by_sibling(&reverse_projection),
        vec![
            vec![right_shapes[1].clone(), right_shapes[0].clone()],
            vec![left_shapes[1].clone(), left_shapes[0].clone()],
        ]
    );

    let mut receiver = forward.clone();
    receiver.clone_from(&reverse);
    assert_eq!(
        tail_children_by_sibling(&serde_json::to_value(&receiver).unwrap()),
        tail_children_by_sibling(&reverse_projection)
    );
    receiver.clone_from(&forward);
    assert_eq!(receiver, forward);

    let forward_json = serde_json::to_vec(&forward).unwrap();
    let reverse_json = serde_json::to_vec(&reverse).unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"forward shorthand sibling payload".as_slice(),
            b"reverse shorthand sibling payload".as_slice(),
            b"LEFT A duplicate tail secret".as_slice(),
            b"LEFT B duplicate tail secret".as_slice(),
            b"RIGHT A duplicate tail secret".as_slice(),
            b"RIGHT B duplicate tail secret".as_slice(),
            b"LEFT B child 0 secret".as_slice(),
            b"LEFT B child 1 secret".as_slice(),
            b"RIGHT A child 0 secret".as_slice(),
            b"LEFT sibling secret".as_slice(),
            b"RIGHT sibling secret".as_slice(),
        ])
    {
        for bytes in [&forward_json, &reverse_json, &forward_wire, &reverse_wire] {
            assert!(!bytes
                .windows(disclosure.len())
                .any(|window| window == disclosure));
        }
    }
    assert_eq!(
        serde_json::to_value(Diagnostic::decode_ovb(&forward_wire).unwrap()).unwrap()["causes"],
        forward_projection["causes"]
    );
    assert_eq!(
        serde_json::to_value(Diagnostic::decode_ovb(&reverse_wire).unwrap()).unwrap()["causes"],
        reverse_projection["causes"]
    );
}

#[test]
fn shorthand_redacted_terminals_keep_duplicate_sibling_tail_shapes() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let diagnostic = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new(message).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let make_wire = |payload: &str, reverse: bool| {
        let sibling = |side: &str| {
            let tail = |slot: &str, child_count: usize| {
                (0..child_count).fold(
                    diagnostic(
                        "ORNA-E-SHORTHAND-TERMINAL-DUP-TAIL",
                        &format!("{payload} {side} {slot} duplicate tail secret"),
                    ),
                    |tail, child| {
                        tail.with_cause(diagnostic(
                            &format!("ORNA-E-SHORTHAND-TERMINAL-{side}-{slot}-CHILD-{child}"),
                            &format!("{payload} {side} {slot} child {child} secret"),
                        ))
                    },
                )
            };
            let tail_specs = match side {
                "LEFT" => [("A", 0), ("B", 2)],
                "RIGHT" => [("A", 1), ("B", 0)],
                _ => unreachable!(),
            };
            let mut tails = tail_specs
                .into_iter()
                .map(|(slot, child_count)| tail(slot, child_count))
                .collect::<Vec<_>>();
            if reverse {
                tails.reverse();
            }
            let terminal = tails
                .into_iter()
                .fold(
                    diagnostic("ORNA-E-SHORTHAND-TERMINAL-DUP-TERMINAL", payload),
                    |terminal, tail| terminal.with_cause(tail),
                )
                // Redact after assembling the duplicate tails and before this
                // terminal is attached under an equal-code sibling branch.
                .redacted();
            diagnostic(
                "ORNA-E-SHORTHAND-TERMINAL-DUP-BRANCH",
                &format!("{payload} {side} sibling secret"),
            )
            .with_cause(terminal)
        };
        let left = sibling("LEFT");
        let right = sibling("RIGHT");
        let siblings = if reverse {
            vec![right, left]
        } else {
            vec![left, right]
        };
        siblings
            .into_iter()
            .fold(
                diagnostic("ORNA-E-SHORTHAND-TERMINAL-DUP-ROOT", payload),
                |root, sibling| root.with_cause(sibling),
            )
            .redacted()
            .encode_ovb()
            .unwrap()
    };
    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for note in diagnostic["notes"].as_array().unwrap() {
            assert_eq!(note, "<redacted>");
        }
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn duplicate_tail_shapes(diagnostic: &serde_json::Value) -> Vec<Vec<Vec<String>>> {
        let siblings = diagnostic["causes"].as_array().unwrap();
        assert_eq!(siblings.len(), 2);
        siblings
            .iter()
            .map(|sibling| {
                assert_eq!(sibling["code"], "ORNA-E-SHORTHAND-TERMINAL-DUP-BRANCH");
                let terminal = &sibling["causes"][0];
                assert_eq!(
                    terminal["code"],
                    "ORNA-E-SHORTHAND-TERMINAL-DUP-TERMINAL"
                );
                let tails = terminal["causes"].as_array().unwrap();
                assert_eq!(tails.len(), 2);
                tails
                    .iter()
                    .map(|tail| {
                        assert_eq!(tail["code"], "ORNA-E-SHORTHAND-TERMINAL-DUP-TAIL");
                        tail["causes"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|child| {
                                assert!(child["causes"].as_array().unwrap().is_empty());
                                child["code"].as_str().unwrap().to_owned()
                            })
                            .collect()
                    })
                    .collect()
            })
            .collect()
    }

    let forward_wire = make_wire("forward terminal shorthand payload", false);
    let reverse_wire = make_wire("reverse terminal shorthand payload", true);
    let forward = Diagnostic::decode_ovb(&forward_wire).unwrap();
    let reverse = Diagnostic::decode_ovb(&reverse_wire).unwrap();
    let forward_projection = serde_json::to_value(&forward).unwrap();
    let reverse_projection = serde_json::to_value(&reverse).unwrap();
    assert_redacted_tree(&forward_projection);
    assert_redacted_tree(&reverse_projection);

    // ORNA-SECRET-002 leaves equal-code sibling/tail order unspecified; retain
    // construction order while redaction crosses the terminal and sibling levels.
    let left_shapes = vec![
        vec![],
        vec![
            "ORNA-E-SHORTHAND-TERMINAL-LEFT-B-CHILD-0".to_owned(),
            "ORNA-E-SHORTHAND-TERMINAL-LEFT-B-CHILD-1".to_owned(),
        ],
    ];
    let right_shapes = vec![
        vec!["ORNA-E-SHORTHAND-TERMINAL-RIGHT-A-CHILD-0".to_owned()],
        vec![],
    ];
    assert_eq!(
        duplicate_tail_shapes(&forward_projection),
        vec![left_shapes.clone(), right_shapes.clone()]
    );
    assert_eq!(
        duplicate_tail_shapes(&reverse_projection),
        vec![
            vec![right_shapes[1].clone(), right_shapes[0].clone()],
            vec![left_shapes[1].clone(), left_shapes[0].clone()],
        ]
    );

    let mut receiver = forward.clone();
    receiver.clone_from(&reverse);
    assert_eq!(
        duplicate_tail_shapes(&serde_json::to_value(&receiver).unwrap()),
        duplicate_tail_shapes(&reverse_projection)
    );
    receiver.clone_from(&forward);
    assert_eq!(receiver, forward);

    let forward_json = serde_json::to_vec(&forward).unwrap();
    let reverse_json = serde_json::to_vec(&reverse).unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"forward terminal shorthand payload".as_slice(),
            b"reverse terminal shorthand payload".as_slice(),
            b"LEFT A duplicate tail secret".as_slice(),
            b"LEFT B duplicate tail secret".as_slice(),
            b"RIGHT A duplicate tail secret".as_slice(),
            b"RIGHT B duplicate tail secret".as_slice(),
            b"LEFT B child 0 secret".as_slice(),
            b"LEFT B child 1 secret".as_slice(),
            b"RIGHT A child 0 secret".as_slice(),
            b"LEFT sibling secret".as_slice(),
            b"RIGHT sibling secret".as_slice(),
        ])
    {
        for bytes in [&forward_json, &reverse_json, &forward_wire, &reverse_wire] {
            assert!(!bytes
                .windows(disclosure.len())
                .any(|window| window == disclosure));
        }
    }
    assert_eq!(
        serde_json::to_value(Diagnostic::decode_ovb(&forward_wire).unwrap()).unwrap()["causes"],
        forward_projection["causes"]
    );
    assert_eq!(
        serde_json::to_value(Diagnostic::decode_ovb(&reverse_wire).unwrap()).unwrap()["causes"],
        reverse_projection["causes"]
    );
}

#[test]
fn shorthand_after_clone_replacement_keeps_duplicate_sibling_tails() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let diagnostic = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new(message).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let make_tree = |payload: &str, replacement: bool| {
        let layout: Vec<(&str, Vec<(&str, usize)>)> = if replacement {
            vec![
                ("RIGHT", vec![("A", 0), ("B", 2), ("C", 1)]),
                ("LEFT", vec![("D", 1)]),
            ]
        } else {
            vec![("LEFT", vec![("A", 0), ("B", 2)]), ("RIGHT", vec![("C", 1)])]
        };
        layout
            .into_iter()
            .map(|(side, tail_specs)| {
                let terminal = tail_specs.into_iter().fold(
                    diagnostic("ORNA-E-SHORTHAND-CLONE-DUP-TERMINAL", payload),
                    |terminal, (slot, child_count)| {
                        let tail = (0..child_count).fold(
                            diagnostic(
                                "ORNA-E-SHORTHAND-CLONE-DUP-TAIL",
                                &format!("{payload} {side} {slot} duplicate tail secret"),
                            ),
                            |tail, child| {
                                tail.with_cause(diagnostic(
                                    &format!(
                                        "ORNA-E-SHORTHAND-CLONE-{side}-{slot}-CHILD-{child}"
                                    ),
                                    &format!("{payload} {side} {slot} child {child} secret"),
                                ))
                            },
                        );
                        terminal.with_cause(tail)
                    },
                );
                diagnostic(
                    "ORNA-E-SHORTHAND-CLONE-DUP-BRANCH",
                    &format!("{payload} {side} sibling secret"),
                )
                .with_cause(terminal)
            })
            .fold(
                diagnostic("ORNA-E-SHORTHAND-CLONE-DUP-ROOT", payload),
                |root, sibling| root.with_cause(sibling),
            )
    };
    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for note in diagnostic["notes"].as_array().unwrap() {
            assert_eq!(note, "<redacted>");
        }
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn duplicate_tail_shapes(diagnostic: &serde_json::Value) -> Vec<Vec<Vec<String>>> {
        let siblings = diagnostic["causes"].as_array().unwrap();
        assert_eq!(siblings.len(), 2);
        siblings
            .iter()
            .map(|sibling| {
                assert_eq!(sibling["code"], "ORNA-E-SHORTHAND-CLONE-DUP-BRANCH");
                let terminal = &sibling["causes"][0];
                assert_eq!(
                    terminal["code"],
                    "ORNA-E-SHORTHAND-CLONE-DUP-TERMINAL"
                );
                terminal["causes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|tail| {
                        assert_eq!(tail["code"], "ORNA-E-SHORTHAND-CLONE-DUP-TAIL");
                        tail["causes"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|child| {
                                assert!(child["causes"].as_array().unwrap().is_empty());
                                child["code"].as_str().unwrap().to_owned()
                            })
                            .collect()
                    })
                    .collect()
            })
            .collect()
    }

    let forward = make_tree("forward clone shorthand payload", false);
    let replacement = make_tree("replacement clone shorthand payload", true);
    let mut receiver = forward.clone();
    receiver.clone_from(&replacement);
    let replaced = receiver.clone().redacted();
    let replaced_projection = serde_json::to_value(&replaced).unwrap();
    assert_redacted_tree(&replaced_projection);

    // ORNA-SECRET-002 leaves equal-code sibling/tail order unspecified; preserve
    // insertion order while shorthand redaction follows a nested clone replacement.
    let forward_shapes = vec![
        vec![
            vec![],
            vec![
                "ORNA-E-SHORTHAND-CLONE-LEFT-B-CHILD-0".to_owned(),
                "ORNA-E-SHORTHAND-CLONE-LEFT-B-CHILD-1".to_owned(),
            ],
        ],
        vec![vec!["ORNA-E-SHORTHAND-CLONE-RIGHT-C-CHILD-0".to_owned()]],
    ];
    let replacement_shapes = vec![
        vec![
            vec![],
            vec![
                "ORNA-E-SHORTHAND-CLONE-RIGHT-B-CHILD-0".to_owned(),
                "ORNA-E-SHORTHAND-CLONE-RIGHT-B-CHILD-1".to_owned(),
            ],
            vec!["ORNA-E-SHORTHAND-CLONE-RIGHT-C-CHILD-0".to_owned()],
        ],
        vec![vec!["ORNA-E-SHORTHAND-CLONE-LEFT-D-CHILD-0".to_owned()]],
    ];
    assert_eq!(
        duplicate_tail_shapes(&replaced_projection),
        replacement_shapes
    );
    let replaced_wire = replaced.encode_ovb().unwrap();
    let replaced_round_trip =
        serde_json::to_value(Diagnostic::decode_ovb(&replaced_wire).unwrap()).unwrap();
    assert_redacted_tree(&replaced_round_trip);
    assert_eq!(
        duplicate_tail_shapes(&replaced_round_trip),
        replacement_shapes
    );

    receiver.clone_from(&forward);
    assert_eq!(receiver, forward);
    let restored = receiver.redacted();
    let restored_projection = serde_json::to_value(&restored).unwrap();
    assert_redacted_tree(&restored_projection);
    assert_eq!(duplicate_tail_shapes(&restored_projection), forward_shapes);
    let restored_wire = restored.encode_ovb().unwrap();
    assert_eq!(
        duplicate_tail_shapes(
            &serde_json::to_value(Diagnostic::decode_ovb(&restored_wire).unwrap()).unwrap()
        ),
        forward_shapes
    );

    let disclosures = fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"forward clone shorthand payload".as_slice(),
            b"replacement clone shorthand payload".as_slice(),
            b"LEFT A duplicate tail secret".as_slice(),
            b"LEFT B duplicate tail secret".as_slice(),
            b"RIGHT C duplicate tail secret".as_slice(),
            b"RIGHT A duplicate tail secret".as_slice(),
            b"RIGHT B duplicate tail secret".as_slice(),
            b"RIGHT C duplicate tail secret".as_slice(),
            b"LEFT D duplicate tail secret".as_slice(),
            b"LEFT B child 0 secret".as_slice(),
            b"LEFT B child 1 secret".as_slice(),
            b"RIGHT C child 0 secret".as_slice(),
            b"RIGHT B child 0 secret".as_slice(),
            b"RIGHT B child 1 secret".as_slice(),
            b"LEFT D child 0 secret".as_slice(),
            b"LEFT sibling secret".as_slice(),
            b"RIGHT sibling secret".as_slice(),
        ]);
    for bytes in [
        serde_json::to_vec(&replaced).unwrap(),
        replaced_wire,
        serde_json::to_vec(&restored).unwrap(),
        restored_wire,
    ] {
        for disclosure in disclosures.clone() {
            assert!(!bytes
                .windows(disclosure.len())
                .any(|window| window == disclosure));
        }
    }
}

#[test]
fn cloned_alias_siblings_retain_their_duplicate_tail_additions() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let diagnostic = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new(message).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let make_wire = |payload: &str, reverse: bool| {
        let tail = |identity: &str, child_count: usize| {
            (0..child_count).fold(
                diagnostic(
                    "ORNA-E-ALIAS-DUP-SIBLING-TAIL",
                    &format!("{payload} {identity} tail secret"),
                ),
                |tail, child| {
                    tail.with_cause(diagnostic(
                        &format!("ORNA-E-ALIAS-DUP-SIBLING-{identity}-CHILD-{child}"),
                        &format!("{payload} {identity} child {child} secret"),
                    ))
                },
            )
        };
        let mut shared_tails = vec![tail("SHARED-A", 0), tail("SHARED-B", 1)];
        if reverse {
            shared_tails.reverse();
        }
        let shared_terminal = shared_tails.into_iter().fold(
            diagnostic("ORNA-E-ALIAS-DUP-SIBLING-TERMINAL", payload),
            |terminal, tail| terminal.with_cause(tail),
        );
        // These independent clones model two aliased uses of one diagnostic
        // value; each sibling appends a distinct same-code tail afterward.
        let left_terminal = shared_terminal.clone().with_cause(tail("LEFT-ALIAS", 1));
        let right_terminal = shared_terminal
            .clone()
            .with_cause(tail("RIGHT-ALIAS", 2));
        let left = diagnostic(
            "ORNA-E-ALIAS-DUP-SIBLING-BRANCH",
            &format!("{payload} left sibling secret"),
        )
        .with_cause(left_terminal);
        let right = diagnostic(
            "ORNA-E-ALIAS-DUP-SIBLING-BRANCH",
            &format!("{payload} right sibling secret"),
        )
        .with_cause(right_terminal);
        let siblings = if reverse {
            vec![right, left]
        } else {
            vec![left, right]
        };
        siblings
            .into_iter()
            .fold(
                diagnostic("ORNA-E-ALIAS-DUP-SIBLING-ROOT", payload),
                |root, sibling| root.with_cause(sibling),
            )
            .redacted()
            .encode_ovb()
            .unwrap()
    };
    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for note in diagnostic["notes"].as_array().unwrap() {
            assert_eq!(note, "<redacted>");
        }
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn aliased_tail_children(diagnostic: &serde_json::Value) -> Vec<Vec<Vec<String>>> {
        let siblings = diagnostic["causes"].as_array().unwrap();
        assert_eq!(siblings.len(), 2);
        siblings
            .iter()
            .map(|sibling| {
                assert_eq!(sibling["code"], "ORNA-E-ALIAS-DUP-SIBLING-BRANCH");
                let terminal = &sibling["causes"][0];
                assert_eq!(terminal["code"], "ORNA-E-ALIAS-DUP-SIBLING-TERMINAL");
                let tails = terminal["causes"].as_array().unwrap();
                assert_eq!(tails.len(), 3);
                tails
                    .iter()
                    .map(|tail| {
                        assert_eq!(tail["code"], "ORNA-E-ALIAS-DUP-SIBLING-TAIL");
                        tail["causes"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|child| {
                                assert!(child["causes"].as_array().unwrap().is_empty());
                                child["code"].as_str().unwrap().to_owned()
                            })
                            .collect()
                    })
                    .collect()
            })
            .collect()
    }

    let forward_wire = make_wire("forward aliased tail payload", false);
    let reverse_wire = make_wire("reverse aliased tail payload", true);
    let forward = Diagnostic::decode_ovb(&forward_wire).unwrap();
    let reverse = Diagnostic::decode_ovb(&reverse_wire).unwrap();
    let forward_projection = serde_json::to_value(&forward).unwrap();
    let reverse_projection = serde_json::to_value(&reverse).unwrap();
    assert_redacted_tree(&forward_projection);
    assert_redacted_tree(&reverse_projection);

    // ORNA-SECRET-002 leaves equal-code sibling/tail ordering open; preserve
    // insertion order and use child identities to show each clone owns its append.
    let shared_a = vec![];
    let shared_b = vec!["ORNA-E-ALIAS-DUP-SIBLING-SHARED-B-CHILD-0".to_owned()];
    let forward_shapes = vec![
        vec![
            shared_a.clone(),
            shared_b.clone(),
            vec!["ORNA-E-ALIAS-DUP-SIBLING-LEFT-ALIAS-CHILD-0".to_owned()],
        ],
        vec![
            shared_a.clone(),
            shared_b.clone(),
            vec![
                "ORNA-E-ALIAS-DUP-SIBLING-RIGHT-ALIAS-CHILD-0".to_owned(),
                "ORNA-E-ALIAS-DUP-SIBLING-RIGHT-ALIAS-CHILD-1".to_owned(),
            ],
        ],
    ];
    let reverse_shapes = vec![
        vec![
            shared_b.clone(),
            shared_a.clone(),
            vec![
                "ORNA-E-ALIAS-DUP-SIBLING-RIGHT-ALIAS-CHILD-0".to_owned(),
                "ORNA-E-ALIAS-DUP-SIBLING-RIGHT-ALIAS-CHILD-1".to_owned(),
            ],
        ],
        vec![
            shared_b,
            shared_a,
            vec!["ORNA-E-ALIAS-DUP-SIBLING-LEFT-ALIAS-CHILD-0".to_owned()],
        ],
    ];
    assert_eq!(aliased_tail_children(&forward_projection), forward_shapes);
    assert_eq!(aliased_tail_children(&reverse_projection), reverse_shapes);

    let mut receiver = forward.clone();
    receiver.clone_from(&reverse);
    assert_eq!(
        aliased_tail_children(&serde_json::to_value(&receiver).unwrap()),
        reverse_shapes
    );
    receiver.clone_from(&forward);
    assert_eq!(receiver, forward);

    let forward_json = serde_json::to_vec(&forward).unwrap();
    let reverse_json = serde_json::to_vec(&reverse).unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"forward aliased tail payload".as_slice(),
            b"reverse aliased tail payload".as_slice(),
            b"SHARED-A tail secret".as_slice(),
            b"SHARED-B tail secret".as_slice(),
            b"LEFT-ALIAS tail secret".as_slice(),
            b"RIGHT-ALIAS tail secret".as_slice(),
            b"SHARED-B child 0 secret".as_slice(),
            b"LEFT-ALIAS child 0 secret".as_slice(),
            b"RIGHT-ALIAS child 0 secret".as_slice(),
            b"RIGHT-ALIAS child 1 secret".as_slice(),
            b"left sibling secret".as_slice(),
            b"right sibling secret".as_slice(),
        ])
    {
        for bytes in [&forward_json, &reverse_json, &forward_wire, &reverse_wire] {
            assert!(!bytes
                .windows(disclosure.len())
                .any(|window| window == disclosure));
        }
    }
    assert_eq!(
        serde_json::to_value(Diagnostic::decode_ovb(&forward_wire).unwrap()).unwrap()["causes"],
        forward_projection["causes"]
    );
    assert_eq!(
        serde_json::to_value(Diagnostic::decode_ovb(&reverse_wire).unwrap()).unwrap()["causes"],
        reverse_projection["causes"]
    );
}

#[test]
fn cloned_alias_tails_keep_distinct_appends_across_siblings() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let diagnostic = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new(message).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let make_wire = |payload: &str, reverse: bool| {
        let shared_tail = diagnostic(
            "ORNA-E-ALIAS-SIBLING-DUP-TAIL",
            &format!("{payload} shared base tail secret"),
        )
        .with_cause(diagnostic(
            "ORNA-E-ALIAS-SIBLING-SHARED-CHILD",
            &format!("{payload} shared base child secret"),
        ));
        let sibling = |side: &str| {
            let aliased_tail = |slot: &str| {
                shared_tail.clone().with_cause(diagnostic(
                    &format!("ORNA-E-ALIAS-SIBLING-{side}-{slot}-CHILD"),
                    &format!("{payload} {side} {slot} alias child secret"),
                ))
            };
            let mut tails = vec![aliased_tail("A"), aliased_tail("B")];
            if reverse {
                tails.reverse();
            }
            let terminal = tails.into_iter().fold(
                diagnostic("ORNA-E-ALIAS-SIBLING-DUP-TERMINAL", payload),
                |terminal, tail| terminal.with_cause(tail),
            );
            diagnostic(
                "ORNA-E-ALIAS-SIBLING-DUP-BRANCH",
                &format!("{payload} {side} sibling alias secret"),
            )
            .with_cause(terminal)
        };
        let left = sibling("LEFT");
        let right = sibling("RIGHT");
        let siblings = if reverse {
            vec![right, left]
        } else {
            vec![left, right]
        };
        siblings
            .into_iter()
            .fold(
                diagnostic("ORNA-E-ALIAS-SIBLING-DUP-ROOT", payload),
                |root, sibling| root.with_cause(sibling),
            )
            .redacted()
            .encode_ovb()
            .unwrap()
    };
    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for note in diagnostic["notes"].as_array().unwrap() {
            assert_eq!(note, "<redacted>");
        }
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn alias_tail_children(diagnostic: &serde_json::Value) -> Vec<Vec<Vec<String>>> {
        let siblings = diagnostic["causes"].as_array().unwrap();
        assert_eq!(siblings.len(), 2);
        siblings
            .iter()
            .map(|sibling| {
                assert_eq!(sibling["code"], "ORNA-E-ALIAS-SIBLING-DUP-BRANCH");
                let terminal = &sibling["causes"][0];
                assert_eq!(terminal["code"], "ORNA-E-ALIAS-SIBLING-DUP-TERMINAL");
                let tails = terminal["causes"].as_array().unwrap();
                assert_eq!(tails.len(), 2);
                tails
                    .iter()
                    .map(|tail| {
                        assert_eq!(tail["code"], "ORNA-E-ALIAS-SIBLING-DUP-TAIL");
                        let children = tail["causes"].as_array().unwrap();
                        assert_eq!(children.len(), 2);
                        assert_eq!(
                            children[0]["code"],
                            "ORNA-E-ALIAS-SIBLING-SHARED-CHILD"
                        );
                        children
                            .iter()
                            .map(|child| {
                                assert!(child["causes"].as_array().unwrap().is_empty());
                                child["code"].as_str().unwrap().to_owned()
                            })
                            .collect()
                    })
                    .collect()
            })
            .collect()
    }

    let forward_wire = make_wire("forward shared alias payload", false);
    let reverse_wire = make_wire("reverse shared alias payload", true);
    let forward = Diagnostic::decode_ovb(&forward_wire).unwrap();
    let reverse = Diagnostic::decode_ovb(&reverse_wire).unwrap();
    let forward_projection = serde_json::to_value(&forward).unwrap();
    let reverse_projection = serde_json::to_value(&reverse).unwrap();
    assert_redacted_tree(&forward_projection);
    assert_redacted_tree(&reverse_projection);

    // ORNA-SECRET-002 leaves equal-code sibling/tail order unspecified; preserve
    // insertion order and use appended child identities to track cloned tails.
    let forward_shapes = vec![
        vec![
            vec![
                "ORNA-E-ALIAS-SIBLING-SHARED-CHILD".to_owned(),
                "ORNA-E-ALIAS-SIBLING-LEFT-A-CHILD".to_owned(),
            ],
            vec![
                "ORNA-E-ALIAS-SIBLING-SHARED-CHILD".to_owned(),
                "ORNA-E-ALIAS-SIBLING-LEFT-B-CHILD".to_owned(),
            ],
        ],
        vec![
            vec![
                "ORNA-E-ALIAS-SIBLING-SHARED-CHILD".to_owned(),
                "ORNA-E-ALIAS-SIBLING-RIGHT-A-CHILD".to_owned(),
            ],
            vec![
                "ORNA-E-ALIAS-SIBLING-SHARED-CHILD".to_owned(),
                "ORNA-E-ALIAS-SIBLING-RIGHT-B-CHILD".to_owned(),
            ],
        ],
    ];
    let reverse_shapes = vec![
        vec![forward_shapes[1][1].clone(), forward_shapes[1][0].clone()],
        vec![forward_shapes[0][1].clone(), forward_shapes[0][0].clone()],
    ];
    assert_eq!(alias_tail_children(&forward_projection), forward_shapes);
    assert_eq!(alias_tail_children(&reverse_projection), reverse_shapes);

    let mut receiver = forward.clone();
    receiver.clone_from(&reverse);
    assert_eq!(
        alias_tail_children(&serde_json::to_value(&receiver).unwrap()),
        reverse_shapes
    );
    receiver.clone_from(&forward);
    assert_eq!(receiver, forward);

    let forward_json = serde_json::to_vec(&forward).unwrap();
    let reverse_json = serde_json::to_vec(&reverse).unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"forward shared alias payload".as_slice(),
            b"reverse shared alias payload".as_slice(),
            b"shared base tail secret".as_slice(),
            b"shared base child secret".as_slice(),
            b"LEFT A alias child secret".as_slice(),
            b"LEFT B alias child secret".as_slice(),
            b"RIGHT A alias child secret".as_slice(),
            b"RIGHT B alias child secret".as_slice(),
            b"LEFT sibling alias secret".as_slice(),
            b"RIGHT sibling alias secret".as_slice(),
        ])
    {
        for bytes in [&forward_json, &reverse_json, &forward_wire, &reverse_wire] {
            assert!(!bytes
                .windows(disclosure.len())
                .any(|window| window == disclosure));
        }
    }
    assert_eq!(
        serde_json::to_value(Diagnostic::decode_ovb(&forward_wire).unwrap()).unwrap()["causes"],
        forward_projection["causes"]
    );
    assert_eq!(
        serde_json::to_value(Diagnostic::decode_ovb(&reverse_wire).unwrap()).unwrap()["causes"],
        reverse_projection["causes"]
    );
}

#[test]
fn nested_duplicate_children_stay_with_cloned_alias_tails() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let diagnostic = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new(message).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let make_wire = |payload: &str, reverse: bool| {
        let shared_tail = diagnostic(
            "ORNA-E-NESTED-ALIAS-DUP-TAIL",
            &format!("{payload} shared tail secret"),
        )
        .with_cause(
            diagnostic(
                "ORNA-E-NESTED-ALIAS-DUP-CHILD",
                &format!("{payload} shared A child secret"),
            )
            .with_cause(diagnostic(
                "ORNA-E-NESTED-ALIAS-SHARED-A-GRANDCHILD",
                &format!("{payload} shared A grandchild secret"),
            )),
        )
        .with_cause(
            diagnostic(
                "ORNA-E-NESTED-ALIAS-DUP-CHILD",
                &format!("{payload} shared B child secret"),
            )
            .with_cause(diagnostic(
                "ORNA-E-NESTED-ALIAS-SHARED-B-GRANDCHILD-1",
                &format!("{payload} shared B grandchild one secret"),
            ))
            .with_cause(diagnostic(
                "ORNA-E-NESTED-ALIAS-SHARED-B-GRANDCHILD-2",
                &format!("{payload} shared B grandchild two secret"),
            )),
        );
        let sibling = |side: &str| {
            let alias_tail = |slot: &str| {
                shared_tail.clone().with_cause(diagnostic(
                    &format!("ORNA-E-NESTED-ALIAS-{side}-{slot}-APPENDED-CHILD"),
                    &format!("{payload} {side} {slot} appended child secret"),
                ))
            };
            let mut tails = vec![alias_tail("A"), alias_tail("B")];
            if reverse {
                tails.reverse();
            }
            let terminal = tails.into_iter().fold(
                diagnostic("ORNA-E-NESTED-ALIAS-DUP-TERMINAL", payload),
                |terminal, tail| terminal.with_cause(tail),
            );
            diagnostic(
                "ORNA-E-NESTED-ALIAS-DUP-BRANCH",
                &format!("{payload} {side} sibling secret"),
            )
            .with_cause(terminal)
        };
        let left = sibling("LEFT");
        let right = sibling("RIGHT");
        let siblings = if reverse {
            vec![right, left]
        } else {
            vec![left, right]
        };
        siblings
            .into_iter()
            .fold(
                diagnostic("ORNA-E-NESTED-ALIAS-DUP-ROOT", payload),
                |root, sibling| root.with_cause(sibling),
            )
            .redacted()
            .encode_ovb()
            .unwrap()
    };
    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for note in diagnostic["notes"].as_array().unwrap() {
            assert_eq!(note, "<redacted>");
        }
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn nested_alias_shape(
        diagnostic: &serde_json::Value,
    ) -> Vec<Vec<Vec<(String, Vec<String>)>>> {
        let siblings = diagnostic["causes"].as_array().unwrap();
        assert_eq!(siblings.len(), 2);
        siblings
            .iter()
            .map(|sibling| {
                assert_eq!(sibling["code"], "ORNA-E-NESTED-ALIAS-DUP-BRANCH");
                let terminal = &sibling["causes"][0];
                assert_eq!(terminal["code"], "ORNA-E-NESTED-ALIAS-DUP-TERMINAL");
                let tails = terminal["causes"].as_array().unwrap();
                assert_eq!(tails.len(), 2);
                tails
                    .iter()
                    .map(|tail| {
                        assert_eq!(tail["code"], "ORNA-E-NESTED-ALIAS-DUP-TAIL");
                        let children = tail["causes"].as_array().unwrap();
                        assert_eq!(children.len(), 3);
                        assert_eq!(
                            children[0]["code"],
                            "ORNA-E-NESTED-ALIAS-DUP-CHILD"
                        );
                        assert_eq!(
                            children[1]["code"],
                            "ORNA-E-NESTED-ALIAS-DUP-CHILD"
                        );
                        children
                            .iter()
                            .map(|child| {
                                (
                                    child["code"].as_str().unwrap().to_owned(),
                                    child["causes"]
                                        .as_array()
                                        .unwrap()
                                        .iter()
                                        .map(|grandchild| {
                                            assert!(grandchild["causes"]
                                                .as_array()
                                                .unwrap()
                                                .is_empty());
                                            grandchild["code"].as_str().unwrap().to_owned()
                                        })
                                        .collect(),
                                )
                            })
                            .collect()
                    })
                    .collect()
            })
            .collect()
    }

    let forward_wire = make_wire("forward nested alias payload", false);
    let reverse_wire = make_wire("reverse nested alias payload", true);
    let forward = Diagnostic::decode_ovb(&forward_wire).unwrap();
    let reverse = Diagnostic::decode_ovb(&reverse_wire).unwrap();
    let forward_projection = serde_json::to_value(&forward).unwrap();
    let reverse_projection = serde_json::to_value(&reverse).unwrap();
    assert_redacted_tree(&forward_projection);
    assert_redacted_tree(&reverse_projection);

    // ORNA-SECRET-002 leaves equal-code sibling/tail order unspecified; preserve
    // construction order and distinguish aliases by their appended child markers.
    let shared_a = (
        "ORNA-E-NESTED-ALIAS-DUP-CHILD".to_owned(),
        vec!["ORNA-E-NESTED-ALIAS-SHARED-A-GRANDCHILD".to_owned()],
    );
    let shared_b = (
        "ORNA-E-NESTED-ALIAS-DUP-CHILD".to_owned(),
        vec![
            "ORNA-E-NESTED-ALIAS-SHARED-B-GRANDCHILD-1".to_owned(),
            "ORNA-E-NESTED-ALIAS-SHARED-B-GRANDCHILD-2".to_owned(),
        ],
    );
    let forward_shapes = vec![
        vec![
            vec![
                shared_a.clone(),
                shared_b.clone(),
                ("ORNA-E-NESTED-ALIAS-LEFT-A-APPENDED-CHILD".to_owned(), vec![]),
            ],
            vec![
                shared_a.clone(),
                shared_b.clone(),
                ("ORNA-E-NESTED-ALIAS-LEFT-B-APPENDED-CHILD".to_owned(), vec![]),
            ],
        ],
        vec![
            vec![
                shared_a.clone(),
                shared_b.clone(),
                ("ORNA-E-NESTED-ALIAS-RIGHT-A-APPENDED-CHILD".to_owned(), vec![]),
            ],
            vec![
                shared_a.clone(),
                shared_b.clone(),
                ("ORNA-E-NESTED-ALIAS-RIGHT-B-APPENDED-CHILD".to_owned(), vec![]),
            ],
        ],
    ];
    let reverse_shapes = vec![
        vec![forward_shapes[1][1].clone(), forward_shapes[1][0].clone()],
        vec![forward_shapes[0][1].clone(), forward_shapes[0][0].clone()],
    ];
    assert_eq!(nested_alias_shape(&forward_projection), forward_shapes);
    assert_eq!(nested_alias_shape(&reverse_projection), reverse_shapes);

    let mut receiver = forward.clone();
    receiver.clone_from(&reverse);
    assert_eq!(
        nested_alias_shape(&serde_json::to_value(&receiver).unwrap()),
        reverse_shapes
    );
    receiver.clone_from(&forward);
    assert_eq!(receiver, forward);

    let forward_json = serde_json::to_vec(&forward).unwrap();
    let reverse_json = serde_json::to_vec(&reverse).unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"forward nested alias payload".as_slice(),
            b"reverse nested alias payload".as_slice(),
            b"shared tail secret".as_slice(),
            b"shared A child secret".as_slice(),
            b"shared B child secret".as_slice(),
            b"shared A grandchild secret".as_slice(),
            b"shared B grandchild one secret".as_slice(),
            b"shared B grandchild two secret".as_slice(),
            b"LEFT A appended child secret".as_slice(),
            b"LEFT B appended child secret".as_slice(),
            b"RIGHT A appended child secret".as_slice(),
            b"RIGHT B appended child secret".as_slice(),
            b"LEFT sibling secret".as_slice(),
            b"RIGHT sibling secret".as_slice(),
        ])
    {
        for bytes in [&forward_json, &reverse_json, &forward_wire, &reverse_wire] {
            assert!(!bytes
                .windows(disclosure.len())
                .any(|window| window == disclosure));
        }
    }
    assert_eq!(
        serde_json::to_value(Diagnostic::decode_ovb(&forward_wire).unwrap()).unwrap()["causes"],
        forward_projection["causes"]
    );
    assert_eq!(
        serde_json::to_value(Diagnostic::decode_ovb(&reverse_wire).unwrap()).unwrap()["causes"],
        reverse_projection["causes"]
    );
}

#[test]
fn nested_cloned_alias_tails_keep_duplicate_inner_tail_ownership() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let diagnostic = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new(message).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let make_wire = |payload: &str, reverse: bool| {
        let shared_inner_tail = diagnostic(
            "ORNA-E-NESTED-CLONED-ALIAS-TAIL",
            &format!("{payload} shared inner tail secret"),
        )
        .with_cause(diagnostic(
            "ORNA-E-NESTED-CLONED-ALIAS-SHARED-LEAF",
            &format!("{payload} shared inner leaf secret"),
        ));
        let shared_outer_tail = diagnostic(
            "ORNA-E-NESTED-CLONED-ALIAS-TAIL",
            &format!("{payload} shared outer tail secret"),
        )
        .with_cause(shared_inner_tail.clone())
        .with_cause(shared_inner_tail.clone());
        let sibling = |side: &str| {
            let alias_tail = |slot: &str| {
                shared_outer_tail
                    .clone()
                    .with_cause(
                        diagnostic(
                            "ORNA-E-NESTED-CLONED-ALIAS-TAIL",
                            &format!("{payload} {side} {slot} appended inner tail secret"),
                        )
                        .with_cause(diagnostic(
                            &format!(
                                "ORNA-E-NESTED-CLONED-ALIAS-{side}-{slot}-APPENDED-LEAF"
                            ),
                            &format!("{payload} {side} {slot} appended leaf secret"),
                        )),
                    )
            };
            let mut tails = vec![alias_tail("A"), alias_tail("B")];
            if reverse {
                tails.reverse();
            }
            let terminal = tails.into_iter().fold(
                diagnostic("ORNA-E-NESTED-CLONED-ALIAS-TERMINAL", payload),
                |terminal, tail| terminal.with_cause(tail),
            );
            diagnostic(
                "ORNA-E-NESTED-CLONED-ALIAS-BRANCH",
                &format!("{payload} {side} sibling secret"),
            )
            .with_cause(terminal)
        };
        let left = sibling("LEFT");
        let right = sibling("RIGHT");
        let siblings = if reverse {
            vec![right, left]
        } else {
            vec![left, right]
        };
        siblings
            .into_iter()
            .fold(
                diagnostic("ORNA-E-NESTED-CLONED-ALIAS-ROOT", payload),
                |root, sibling| root.with_cause(sibling),
            )
            .redacted()
            .encode_ovb()
            .unwrap()
    };
    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for note in diagnostic["notes"].as_array().unwrap() {
            assert_eq!(note, "<redacted>");
        }
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn nested_tail_leaf_codes(
        diagnostic: &serde_json::Value,
    ) -> Vec<Vec<Vec<Vec<String>>>> {
        let siblings = diagnostic["causes"].as_array().unwrap();
        assert_eq!(siblings.len(), 2);
        siblings
            .iter()
            .map(|sibling| {
                assert_eq!(sibling["code"], "ORNA-E-NESTED-CLONED-ALIAS-BRANCH");
                let terminal = &sibling["causes"][0];
                assert_eq!(terminal["code"], "ORNA-E-NESTED-CLONED-ALIAS-TERMINAL");
                let outer_tails = terminal["causes"].as_array().unwrap();
                assert_eq!(outer_tails.len(), 2);
                outer_tails
                    .iter()
                    .map(|outer_tail| {
                        assert_eq!(outer_tail["code"], "ORNA-E-NESTED-CLONED-ALIAS-TAIL");
                        let inner_tails = outer_tail["causes"].as_array().unwrap();
                        assert_eq!(inner_tails.len(), 3);
                        inner_tails
                            .iter()
                            .map(|inner_tail| {
                                assert_eq!(inner_tail["code"], "ORNA-E-NESTED-CLONED-ALIAS-TAIL");
                                let leaves = inner_tail["causes"].as_array().unwrap();
                                assert_eq!(leaves.len(), 1);
                                assert!(leaves[0]["causes"].as_array().unwrap().is_empty());
                                vec![leaves[0]["code"].as_str().unwrap().to_owned()]
                            })
                            .collect()
                    })
                    .collect()
            })
            .collect()
    }

    let forward_wire = make_wire("forward nested cloned alias payload", false);
    let reverse_wire = make_wire("reverse nested cloned alias payload", true);
    let forward = Diagnostic::decode_ovb(&forward_wire).unwrap();
    let reverse = Diagnostic::decode_ovb(&reverse_wire).unwrap();
    let forward_projection = serde_json::to_value(&forward).unwrap();
    let reverse_projection = serde_json::to_value(&reverse).unwrap();
    assert_redacted_tree(&forward_projection);
    assert_redacted_tree(&reverse_projection);

    // ORNA-SECRET-002 leaves equal-code sibling and tail order unspecified;
    // retain construction order and identify each cloned tail by its appended leaf.
    let shared_inner = vec![vec!["ORNA-E-NESTED-CLONED-ALIAS-SHARED-LEAF".to_owned()]];
    let aliased_shape = |side: &str, slot: &str| {
        vec![
            shared_inner[0].clone(),
            shared_inner[0].clone(),
            vec![format!("ORNA-E-NESTED-CLONED-ALIAS-{side}-{slot}-APPENDED-LEAF")],
        ]
    };
    let forward_shapes = vec![
        vec![aliased_shape("LEFT", "A"), aliased_shape("LEFT", "B")],
        vec![aliased_shape("RIGHT", "A"), aliased_shape("RIGHT", "B")],
    ];
    let reverse_shapes = vec![
        vec![aliased_shape("RIGHT", "B"), aliased_shape("RIGHT", "A")],
        vec![aliased_shape("LEFT", "B"), aliased_shape("LEFT", "A")],
    ];
    assert_eq!(nested_tail_leaf_codes(&forward_projection), forward_shapes);
    assert_eq!(nested_tail_leaf_codes(&reverse_projection), reverse_shapes);

    let mut receiver = forward.clone();
    receiver.clone_from(&reverse);
    assert_eq!(
        nested_tail_leaf_codes(&serde_json::to_value(&receiver).unwrap()),
        reverse_shapes
    );
    receiver.clone_from(&forward);
    assert_eq!(receiver, forward);

    let forward_json = serde_json::to_vec(&forward).unwrap();
    let reverse_json = serde_json::to_vec(&reverse).unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"forward nested cloned alias payload".as_slice(),
            b"reverse nested cloned alias payload".as_slice(),
            b"shared inner tail secret".as_slice(),
            b"shared inner leaf secret".as_slice(),
            b"shared outer tail secret".as_slice(),
            b"LEFT A appended inner tail secret".as_slice(),
            b"LEFT B appended inner tail secret".as_slice(),
            b"RIGHT A appended inner tail secret".as_slice(),
            b"RIGHT B appended inner tail secret".as_slice(),
            b"LEFT A appended leaf secret".as_slice(),
            b"LEFT B appended leaf secret".as_slice(),
            b"RIGHT A appended leaf secret".as_slice(),
            b"RIGHT B appended leaf secret".as_slice(),
            b"LEFT sibling secret".as_slice(),
            b"RIGHT sibling secret".as_slice(),
        ])
    {
        for bytes in [&forward_json, &reverse_json, &forward_wire, &reverse_wire] {
            assert!(!bytes
                .windows(disclosure.len())
                .any(|window| window == disclosure));
        }
    }
    assert_eq!(
        serde_json::to_value(Diagnostic::decode_ovb(&forward_wire).unwrap()).unwrap()["causes"],
        forward_projection["causes"]
    );
    assert_eq!(
        serde_json::to_value(Diagnostic::decode_ovb(&reverse_wire).unwrap()).unwrap()["causes"],
        reverse_projection["causes"]
    );
}

#[test]
fn empty_nested_tails_remain_with_cloned_alias_groups() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let diagnostic = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new(message).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let make_wire = |payload: &str, reverse: bool| {
        let shared_inner_a = diagnostic(
            "ORNA-E-EMPTY-NESTED-ALIAS-TAIL",
            &format!("{payload} shared empty inner tail secret"),
        );
        let shared_inner_b = (0..2).fold(
            diagnostic(
                "ORNA-E-EMPTY-NESTED-ALIAS-TAIL",
                &format!("{payload} shared populated inner tail secret"),
            ),
            |tail, child| {
                tail.with_cause(diagnostic(
                    &format!("ORNA-E-EMPTY-NESTED-ALIAS-SHARED-CHILD-{child}"),
                    &format!("{payload} shared child {child} secret"),
                ))
            },
        );
        let shared_outer_tail = diagnostic(
            "ORNA-E-EMPTY-NESTED-ALIAS-TAIL",
            &format!("{payload} shared outer tail secret"),
        )
        .with_cause(shared_inner_a)
        .with_cause(shared_inner_b);
        let sibling = |side: &str| {
            let alias_tail = |slot: &str, appended_count: usize| {
                let appended_inner = (0..appended_count).fold(
                    diagnostic(
                        "ORNA-E-EMPTY-NESTED-ALIAS-TAIL",
                        &format!("{payload} {side} {slot} appended inner tail secret"),
                    ),
                    |tail, child| {
                        tail.with_cause(diagnostic(
                            &format!(
                                "ORNA-E-EMPTY-NESTED-ALIAS-{side}-{slot}-APPENDED-CHILD-{child}"
                            ),
                            &format!("{payload} {side} {slot} appended child {child} secret"),
                        ))
                    },
                );
                shared_outer_tail.clone().with_cause(appended_inner)
            };
            let specs = match side {
                "LEFT" => [("A", 0), ("B", 1)],
                "RIGHT" => [("A", 2), ("B", 0)],
                _ => unreachable!(),
            };
            let mut tails = specs
                .into_iter()
                .map(|(slot, count)| alias_tail(slot, count))
                .collect::<Vec<_>>();
            if reverse {
                tails.reverse();
            }
            let terminal = tails.into_iter().fold(
                diagnostic("ORNA-E-EMPTY-NESTED-ALIAS-TERMINAL", payload),
                |terminal, tail| terminal.with_cause(tail),
            );
            diagnostic(
                "ORNA-E-EMPTY-NESTED-ALIAS-BRANCH",
                &format!("{payload} {side} sibling secret"),
            )
            .with_cause(terminal)
        };
        let left = sibling("LEFT");
        let right = sibling("RIGHT");
        let siblings = if reverse {
            vec![right, left]
        } else {
            vec![left, right]
        };
        siblings
            .into_iter()
            .fold(
                diagnostic("ORNA-E-EMPTY-NESTED-ALIAS-ROOT", payload),
                |root, sibling| root.with_cause(sibling),
            )
            .redacted()
            .encode_ovb()
            .unwrap()
    };
    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for note in diagnostic["notes"].as_array().unwrap() {
            assert_eq!(note, "<redacted>");
        }
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    fn nested_tail_leaf_codes(
        diagnostic: &serde_json::Value,
    ) -> Vec<Vec<Vec<Vec<String>>>> {
        let siblings = diagnostic["causes"].as_array().unwrap();
        assert_eq!(siblings.len(), 2);
        siblings
            .iter()
            .map(|sibling| {
                assert_eq!(sibling["code"], "ORNA-E-EMPTY-NESTED-ALIAS-BRANCH");
                let terminal = &sibling["causes"][0];
                assert_eq!(terminal["code"], "ORNA-E-EMPTY-NESTED-ALIAS-TERMINAL");
                let outer_tails = terminal["causes"].as_array().unwrap();
                assert_eq!(outer_tails.len(), 2);
                outer_tails
                    .iter()
                    .map(|outer_tail| {
                        assert_eq!(outer_tail["code"], "ORNA-E-EMPTY-NESTED-ALIAS-TAIL");
                        let inner_tails = outer_tail["causes"].as_array().unwrap();
                        assert_eq!(inner_tails.len(), 3);
                        inner_tails
                            .iter()
                            .map(|inner_tail| {
                                assert_eq!(inner_tail["code"], "ORNA-E-EMPTY-NESTED-ALIAS-TAIL");
                                inner_tail["causes"]
                                    .as_array()
                                    .unwrap()
                                    .iter()
                                    .map(|leaf| {
                                        assert!(leaf["causes"].as_array().unwrap().is_empty());
                                        leaf["code"].as_str().unwrap().to_owned()
                                    })
                                    .collect()
                            })
                            .collect()
                    })
                    .collect()
            })
            .collect()
    }

    let forward_wire = make_wire("forward empty nested alias payload", false);
    let reverse_wire = make_wire("reverse empty nested alias payload", true);
    let forward = Diagnostic::decode_ovb(&forward_wire).unwrap();
    let reverse = Diagnostic::decode_ovb(&reverse_wire).unwrap();
    let forward_projection = serde_json::to_value(&forward).unwrap();
    let reverse_projection = serde_json::to_value(&reverse).unwrap();
    assert_redacted_tree(&forward_projection);
    assert_redacted_tree(&reverse_projection);

    // ORNA-SECRET-002 leaves equal-code sibling/tail order unspecified; retain
    // construction order and distinguish empty aliases by their appended leaf counts.
    let shared_empty = vec![];
    let shared_populated = vec![
        "ORNA-E-EMPTY-NESTED-ALIAS-SHARED-CHILD-0".to_owned(),
        "ORNA-E-EMPTY-NESTED-ALIAS-SHARED-CHILD-1".to_owned(),
    ];
    let appended = |side: &str, slot: &str, count: usize| {
        (0..count)
            .map(|child| {
                format!("ORNA-E-EMPTY-NESTED-ALIAS-{side}-{slot}-APPENDED-CHILD-{child}")
            })
            .collect::<Vec<_>>()
    };
    let outer_shape = |side: &str, slot: &str, count: usize| {
        vec![
            shared_empty.clone(),
            shared_populated.clone(),
            appended(side, slot, count),
        ]
    };
    let forward_shapes = vec![
        vec![outer_shape("LEFT", "A", 0), outer_shape("LEFT", "B", 1)],
        vec![outer_shape("RIGHT", "A", 2), outer_shape("RIGHT", "B", 0)],
    ];
    let reverse_shapes = vec![
        vec![outer_shape("RIGHT", "B", 0), outer_shape("RIGHT", "A", 2)],
        vec![outer_shape("LEFT", "B", 1), outer_shape("LEFT", "A", 0)],
    ];
    assert_eq!(nested_tail_leaf_codes(&forward_projection), forward_shapes);
    assert_eq!(nested_tail_leaf_codes(&reverse_projection), reverse_shapes);

    let mut receiver = forward.clone();
    receiver.clone_from(&reverse);
    assert_eq!(
        nested_tail_leaf_codes(&serde_json::to_value(&receiver).unwrap()),
        reverse_shapes
    );
    receiver.clone_from(&forward);
    assert_eq!(receiver, forward);

    let forward_json = serde_json::to_vec(&forward).unwrap();
    let reverse_json = serde_json::to_vec(&reverse).unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"forward empty nested alias payload".as_slice(),
            b"reverse empty nested alias payload".as_slice(),
            b"shared empty inner tail secret".as_slice(),
            b"shared populated inner tail secret".as_slice(),
            b"shared outer tail secret".as_slice(),
            b"shared child 0 secret".as_slice(),
            b"shared child 1 secret".as_slice(),
            b"LEFT A appended inner tail secret".as_slice(),
            b"LEFT B appended inner tail secret".as_slice(),
            b"RIGHT A appended inner tail secret".as_slice(),
            b"RIGHT B appended inner tail secret".as_slice(),
            b"LEFT A appended child 0 secret".as_slice(),
            b"LEFT B appended child 0 secret".as_slice(),
            b"RIGHT A appended child 0 secret".as_slice(),
            b"RIGHT A appended child 1 secret".as_slice(),
            b"LEFT sibling secret".as_slice(),
            b"RIGHT sibling secret".as_slice(),
        ])
    {
        for bytes in [&forward_json, &reverse_json, &forward_wire, &reverse_wire] {
            assert!(!bytes
                .windows(disclosure.len())
                .any(|window| window == disclosure));
        }
    }
    assert_eq!(
        serde_json::to_value(Diagnostic::decode_ovb(&forward_wire).unwrap()).unwrap()["causes"],
        forward_projection["causes"]
    );
    assert_eq!(
        serde_json::to_value(Diagnostic::decode_ovb(&reverse_wire).unwrap()).unwrap()["causes"],
        reverse_projection["causes"]
    );
}

#[test]
fn empty_nested_clones_keep_duplicate_tail_counts_on_replay() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let diagnostic = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new(message).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let empty_tail = || {
        diagnostic(
            "ORNA-E-EMPTY-NESTED-CLONE-TAIL",
            "empty nested clone tail secret",
        )
    };
    let make_wire = |reverse: bool| {
        let shared_alias = diagnostic(
            "ORNA-E-EMPTY-NESTED-CLONE-ALIAS",
            "shared cloned alias secret",
        )
        .with_cause(empty_tail())
        .with_cause(empty_tail());
        let extended_alias = shared_alias.clone().with_cause(empty_tail());
        let aliases = if reverse {
            vec![shared_alias, extended_alias]
        } else {
            vec![extended_alias, shared_alias]
        };
        aliases
            .into_iter()
            .fold(
                diagnostic("ORNA-E-EMPTY-NESTED-CLONE-ROOT", "root secret"),
                |root, alias| root.with_cause(alias),
            )
            .redacted()
            .encode_ovb()
            .unwrap()
    };
    fn nested_empty_alias_shape(diagnostic: &serde_json::Value) -> Vec<usize> {
        let aliases = diagnostic["causes"].as_array().unwrap();
        assert_eq!(aliases.len(), 2);
        aliases
            .iter()
            .map(|alias| {
                assert_eq!(alias["code"], "ORNA-E-EMPTY-NESTED-CLONE-ALIAS");
                let tails = alias["causes"].as_array().unwrap();
                assert!(tails.iter().all(|tail| {
                    tail["code"] == "ORNA-E-EMPTY-NESTED-CLONE-TAIL"
                        && tail["causes"].as_array().unwrap().is_empty()
                }));
                tails.len()
            })
            .collect()
    }
    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for note in diagnostic["notes"].as_array().unwrap() {
            assert_eq!(note, "<redacted>");
        }
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }

    let forward_wire = make_wire(false);
    let reverse_wire = make_wire(true);
    let forward = Diagnostic::decode_ovb(&forward_wire).unwrap();
    let reverse = Diagnostic::decode_ovb(&reverse_wire).unwrap();
    let forward_projection = serde_json::to_value(&forward).unwrap();
    let reverse_projection = serde_json::to_value(&reverse).unwrap();
    assert_redacted_tree(&forward_projection);
    assert_redacted_tree(&reverse_projection);

    // ORNA-SECRET-002 leaves equal-code tail order unspecified; this proof follows insertion order.
    assert_eq!(nested_empty_alias_shape(&forward_projection), vec![3, 2]);
    assert_eq!(nested_empty_alias_shape(&reverse_projection), vec![2, 3]);

    let mut replacement = forward.clone();
    replacement.clone_from(&reverse);
    assert_eq!(
        nested_empty_alias_shape(&serde_json::to_value(&replacement).unwrap()),
        vec![2, 3]
    );
    replacement.clone_from(&forward);
    assert_eq!(replacement, forward);

    let forward_json = serde_json::to_vec(&forward).unwrap();
    let reverse_json = serde_json::to_vec(&reverse).unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"root secret".as_slice(),
            b"shared cloned alias secret".as_slice(),
            b"empty nested clone tail secret".as_slice(),
        ])
    {
        for bytes in [&forward_json, &reverse_json, &forward_wire, &reverse_wire] {
            assert!(!bytes
                .windows(disclosure.len())
                .any(|window| window == disclosure));
        }
    }
    assert_eq!(
        serde_json::to_value(Diagnostic::decode_ovb(&forward_wire).unwrap()).unwrap()["causes"],
        forward_projection["causes"]
    );
    assert_eq!(
        serde_json::to_value(Diagnostic::decode_ovb(&reverse_wire).unwrap()).unwrap()["causes"],
        reverse_projection["causes"]
    );
}

#[test]
fn nested_empty_alias_tails_survive_deep_clone_growth_replay() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let diagnostic = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new(message).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let empty_tail = || {
        diagnostic(
            "ORNA-E-DEEP-EMPTY-CLONE-TAIL",
            "deep empty alias tail secret",
        )
    };
    let nested_alias = || {
        diagnostic(
            "ORNA-E-DEEP-EMPTY-CLONE-ALIAS",
            "nested alias secret",
        )
        .with_cause(empty_tail())
    };
    let make_wire = |reverse: bool| {
        let shared_nested = nested_alias();
        let shared_outer = diagnostic(
            "ORNA-E-DEEP-EMPTY-CLONE-ALIAS",
            "outer alias secret",
        )
        .with_cause(shared_nested.clone())
        .with_cause(shared_nested.clone());
        let extended_nested = shared_nested.clone().with_cause(empty_tail());
        let extended_outer = shared_outer.clone().with_cause(extended_nested);
        let aliases = if reverse {
            vec![shared_outer, extended_outer]
        } else {
            vec![extended_outer, shared_outer]
        };
        aliases
            .into_iter()
            .fold(
                diagnostic("ORNA-E-DEEP-EMPTY-CLONE-ROOT", "root secret"),
                |root, alias| root.with_cause(alias),
            )
            .redacted()
            .encode_ovb()
            .unwrap()
    };
    fn deep_empty_alias_shape(diagnostic: &serde_json::Value) -> Vec<Vec<usize>> {
        let aliases = diagnostic["causes"].as_array().unwrap();
        assert_eq!(aliases.len(), 2);
        aliases
            .iter()
            .map(|alias| {
                assert_eq!(alias["code"], "ORNA-E-DEEP-EMPTY-CLONE-ALIAS");
                alias["causes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|nested| {
                        assert_eq!(nested["code"], "ORNA-E-DEEP-EMPTY-CLONE-ALIAS");
                        let tails = nested["causes"].as_array().unwrap();
                        assert!(tails.iter().all(|tail| {
                            tail["code"] == "ORNA-E-DEEP-EMPTY-CLONE-TAIL"
                                && tail["causes"].as_array().unwrap().is_empty()
                        }));
                        tails.len()
                    })
                    .collect()
            })
            .collect()
    }
    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for note in diagnostic["notes"].as_array().unwrap() {
            assert_eq!(note, "<redacted>");
        }
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }

    let forward_wire = make_wire(false);
    let reverse_wire = make_wire(true);
    let forward = Diagnostic::decode_ovb(&forward_wire).unwrap();
    let reverse = Diagnostic::decode_ovb(&reverse_wire).unwrap();
    let forward_projection = serde_json::to_value(&forward).unwrap();
    let reverse_projection = serde_json::to_value(&reverse).unwrap();
    assert_redacted_tree(&forward_projection);
    assert_redacted_tree(&reverse_projection);

    // ORNA-SECRET-002 leaves equal-code order unspecified; these shapes follow insertion order.
    let forward_shape = vec![vec![1, 1, 2], vec![1, 1]];
    let reverse_shape = vec![vec![1, 1], vec![1, 1, 2]];
    assert_eq!(deep_empty_alias_shape(&forward_projection), forward_shape);
    assert_eq!(deep_empty_alias_shape(&reverse_projection), reverse_shape);

    let mut replacement = forward.clone();
    replacement.clone_from(&reverse);
    assert_eq!(
        deep_empty_alias_shape(&serde_json::to_value(&replacement).unwrap()),
        reverse_shape
    );
    replacement.clone_from(&forward);
    assert_eq!(replacement, forward);

    let forward_json = serde_json::to_vec(&forward).unwrap();
    let reverse_json = serde_json::to_vec(&reverse).unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"root secret".as_slice(),
            b"outer alias secret".as_slice(),
            b"nested alias secret".as_slice(),
            b"deep empty alias tail secret".as_slice(),
        ])
    {
        for bytes in [&forward_json, &reverse_json, &forward_wire, &reverse_wire] {
            assert!(!bytes
                .windows(disclosure.len())
                .any(|window| window == disclosure));
        }
    }
    assert_eq!(
        serde_json::to_value(Diagnostic::decode_ovb(&forward_wire).unwrap()).unwrap()["causes"],
        forward_projection["causes"]
    );
    assert_eq!(
        serde_json::to_value(Diagnostic::decode_ovb(&reverse_wire).unwrap()).unwrap()["causes"],
        reverse_projection["causes"]
    );
}

#[test]
fn deepest_empty_alias_tail_growth_stays_with_its_cloned_chain() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let diagnostic = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new(message).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let empty_tail = || {
        diagnostic(
            "ORNA-E-DEEPEST-EMPTY-ALIAS-TAIL",
            "deepest empty alias tail secret",
        )
    };
    let make_wire = |reverse: bool| {
        let shared_leaf = diagnostic(
            "ORNA-E-DEEPEST-EMPTY-ALIAS",
            "leaf alias secret",
        )
        .with_cause(empty_tail());
        let shared_middle = diagnostic(
            "ORNA-E-DEEPEST-EMPTY-ALIAS",
            "middle alias secret",
        )
        .with_cause(shared_leaf.clone())
        .with_cause(shared_leaf.clone());
        let shared_outer = diagnostic(
            "ORNA-E-DEEPEST-EMPTY-ALIAS",
            "outer alias secret",
        )
        .with_cause(shared_middle.clone())
        .with_cause(shared_middle.clone());

        let grown_leaf = shared_leaf.clone().with_cause(empty_tail());
        let grown_middle = shared_middle.clone().with_cause(grown_leaf);
        let grown_outer = shared_outer.clone().with_cause(grown_middle);
        let aliases = if reverse {
            vec![shared_outer, grown_outer]
        } else {
            vec![grown_outer, shared_outer]
        };
        aliases
            .into_iter()
            .fold(
                diagnostic("ORNA-E-DEEPEST-EMPTY-ALIAS-ROOT", "root secret"),
                |root, alias| root.with_cause(alias),
            )
            .redacted()
            .encode_ovb()
            .unwrap()
    };
    fn deepest_empty_alias_shape(diagnostic: &serde_json::Value) -> Vec<Vec<Vec<usize>>> {
        let aliases = diagnostic["causes"].as_array().unwrap();
        assert_eq!(aliases.len(), 2);
        aliases
            .iter()
            .map(|outer| {
                assert_eq!(outer["code"], "ORNA-E-DEEPEST-EMPTY-ALIAS");
                outer["causes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|middle| {
                        assert_eq!(middle["code"], "ORNA-E-DEEPEST-EMPTY-ALIAS");
                        middle["causes"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|leaf| {
                                assert_eq!(leaf["code"], "ORNA-E-DEEPEST-EMPTY-ALIAS");
                                let tails = leaf["causes"].as_array().unwrap();
                                assert!(tails.iter().all(|tail| {
                                    tail["code"] == "ORNA-E-DEEPEST-EMPTY-ALIAS-TAIL"
                                        && tail["causes"].as_array().unwrap().is_empty()
                                }));
                                tails.len()
                            })
                            .collect()
                    })
                    .collect()
            })
            .collect()
    }
    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for note in diagnostic["notes"].as_array().unwrap() {
            assert_eq!(note, "<redacted>");
        }
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }

    let forward_wire = make_wire(false);
    let reverse_wire = make_wire(true);
    let forward = Diagnostic::decode_ovb(&forward_wire).unwrap();
    let reverse = Diagnostic::decode_ovb(&reverse_wire).unwrap();
    let forward_projection = serde_json::to_value(&forward).unwrap();
    let reverse_projection = serde_json::to_value(&reverse).unwrap();
    assert_redacted_tree(&forward_projection);
    assert_redacted_tree(&reverse_projection);

    // ORNA-SECRET-002 leaves equal-code order unspecified; this proof follows insertion order.
    let grown_shape = vec![vec![1, 1], vec![1, 1], vec![1, 1, 2]];
    let shared_shape = vec![vec![1, 1], vec![1, 1]];
    let forward_shape = vec![grown_shape.clone(), shared_shape.clone()];
    let reverse_shape = vec![shared_shape, grown_shape];
    assert_eq!(deepest_empty_alias_shape(&forward_projection), forward_shape);
    assert_eq!(deepest_empty_alias_shape(&reverse_projection), reverse_shape);

    let mut replacement = forward.clone();
    replacement.clone_from(&reverse);
    assert_eq!(
        deepest_empty_alias_shape(&serde_json::to_value(&replacement).unwrap()),
        reverse_shape
    );
    replacement.clone_from(&forward);
    assert_eq!(replacement, forward);

    let forward_json = serde_json::to_vec(&forward).unwrap();
    let reverse_json = serde_json::to_vec(&reverse).unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"root secret".as_slice(),
            b"outer alias secret".as_slice(),
            b"middle alias secret".as_slice(),
            b"leaf alias secret".as_slice(),
            b"deepest empty alias tail secret".as_slice(),
        ])
    {
        for bytes in [&forward_json, &reverse_json, &forward_wire, &reverse_wire] {
            assert!(!bytes
                .windows(disclosure.len())
                .any(|window| window == disclosure));
        }
    }
    assert_eq!(
        serde_json::to_value(Diagnostic::decode_ovb(&forward_wire).unwrap()).unwrap()["causes"],
        forward_projection["causes"]
    );
    assert_eq!(
        serde_json::to_value(Diagnostic::decode_ovb(&reverse_wire).unwrap()).unwrap()["causes"],
        reverse_projection["causes"]
    );
}

#[test]
fn deepest_empty_alias_tail_duplicates_survive_four_clone_levels() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let diagnostic = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new(message).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let alias = |message: &str| diagnostic("ORNA-E-FOUR-LEVEL-EMPTY-ALIAS", message);
    let empty_tail = || {
        diagnostic(
            "ORNA-E-FOUR-LEVEL-EMPTY-TAIL",
            "four-level empty tail secret",
        )
    };
    let make_wire = |reverse: bool| {
        let shared_leaf = alias("leaf alias secret")
            .with_cause(empty_tail())
            .with_cause(empty_tail());
        let shared_inner = alias("inner alias secret")
            .with_cause(shared_leaf.clone())
            .with_cause(shared_leaf.clone());
        let shared_middle = alias("middle alias secret")
            .with_cause(shared_inner.clone())
            .with_cause(shared_inner.clone());
        let shared_outer = alias("outer alias secret")
            .with_cause(shared_middle.clone())
            .with_cause(shared_middle.clone());

        let grown_leaf = shared_leaf.clone().with_cause(empty_tail());
        let grown_inner = shared_inner.clone().with_cause(grown_leaf);
        let grown_middle = shared_middle.clone().with_cause(grown_inner);
        let grown_outer = shared_outer.clone().with_cause(grown_middle);
        let aliases = if reverse {
            vec![shared_outer, grown_outer]
        } else {
            vec![grown_outer, shared_outer]
        };
        aliases
            .into_iter()
            .fold(
                diagnostic("ORNA-E-FOUR-LEVEL-EMPTY-ROOT", "root secret"),
                |root, alias| root.with_cause(alias),
            )
            .redacted()
            .encode_ovb()
            .unwrap()
    };
    fn four_level_empty_alias_shape(
        diagnostic: &serde_json::Value,
    ) -> Vec<Vec<Vec<Vec<usize>>>> {
        let aliases = diagnostic["causes"].as_array().unwrap();
        assert_eq!(aliases.len(), 2);
        aliases
            .iter()
            .map(|outer| {
                assert_eq!(outer["code"], "ORNA-E-FOUR-LEVEL-EMPTY-ALIAS");
                outer["causes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|middle| {
                        assert_eq!(middle["code"], "ORNA-E-FOUR-LEVEL-EMPTY-ALIAS");
                        middle["causes"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|inner| {
                                assert_eq!(inner["code"], "ORNA-E-FOUR-LEVEL-EMPTY-ALIAS");
                                inner["causes"]
                                    .as_array()
                                    .unwrap()
                                    .iter()
                                    .map(|leaf| {
                                        assert_eq!(leaf["code"], "ORNA-E-FOUR-LEVEL-EMPTY-ALIAS");
                                        let tails = leaf["causes"].as_array().unwrap();
                                        assert!(tails.iter().all(|tail| {
                                            tail["code"] == "ORNA-E-FOUR-LEVEL-EMPTY-TAIL"
                                                && tail["causes"].as_array().unwrap().is_empty()
                                        }));
                                        tails.len()
                                    })
                                    .collect()
                            })
                            .collect()
                    })
                    .collect()
            })
            .collect()
    }
    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for note in diagnostic["notes"].as_array().unwrap() {
            assert_eq!(note, "<redacted>");
        }
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }

    let forward_wire = make_wire(false);
    let reverse_wire = make_wire(true);
    let forward = Diagnostic::decode_ovb(&forward_wire).unwrap();
    let reverse = Diagnostic::decode_ovb(&reverse_wire).unwrap();
    let forward_projection = serde_json::to_value(&forward).unwrap();
    let reverse_projection = serde_json::to_value(&reverse).unwrap();
    assert_redacted_tree(&forward_projection);
    assert_redacted_tree(&reverse_projection);

    // ORNA-SECRET-002 leaves equal-code order unspecified; these shapes follow insertion order.
    let shared_inner_shape = vec![2, 2];
    let shared_middle_shape = vec![
        shared_inner_shape.clone(),
        shared_inner_shape.clone(),
    ];
    let shared_outer_shape = vec![
        shared_middle_shape.clone(),
        shared_middle_shape.clone(),
    ];
    let grown_inner_shape = vec![2, 2, 3];
    let grown_middle_shape = vec![
        shared_inner_shape.clone(),
        shared_inner_shape.clone(),
        grown_inner_shape,
    ];
    let grown_outer_shape = vec![
        shared_middle_shape.clone(),
        shared_middle_shape.clone(),
        grown_middle_shape,
    ];
    let forward_shape = vec![grown_outer_shape.clone(), shared_outer_shape.clone()];
    let reverse_shape = vec![shared_outer_shape, grown_outer_shape];
    assert_eq!(four_level_empty_alias_shape(&forward_projection), forward_shape);
    assert_eq!(four_level_empty_alias_shape(&reverse_projection), reverse_shape);

    let mut replacement = forward.clone();
    replacement.clone_from(&reverse);
    assert_eq!(
        four_level_empty_alias_shape(&serde_json::to_value(&replacement).unwrap()),
        reverse_shape
    );
    replacement.clone_from(&forward);
    assert_eq!(replacement, forward);

    let forward_json = serde_json::to_vec(&forward).unwrap();
    let reverse_json = serde_json::to_vec(&reverse).unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"root secret".as_slice(),
            b"outer alias secret".as_slice(),
            b"middle alias secret".as_slice(),
            b"inner alias secret".as_slice(),
            b"leaf alias secret".as_slice(),
            b"four-level empty tail secret".as_slice(),
        ])
    {
        for bytes in [&forward_json, &reverse_json, &forward_wire, &reverse_wire] {
            assert!(!bytes
                .windows(disclosure.len())
                .any(|window| window == disclosure));
        }
    }
    assert_eq!(
        serde_json::to_value(Diagnostic::decode_ovb(&forward_wire).unwrap()).unwrap()["causes"],
        forward_projection["causes"]
    );
    assert_eq!(
        serde_json::to_value(Diagnostic::decode_ovb(&reverse_wire).unwrap()).unwrap()["causes"],
        reverse_projection["causes"]
    );
}

#[test]
fn deepest_empty_alias_tail_duplicates_survive_five_clone_levels() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let diagnostic = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new(message).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let alias = |message: &str| diagnostic("ORNA-E-FIVE-LEVEL-EMPTY-ALIAS", message);
    let empty_tail = || {
        diagnostic(
            "ORNA-E-FIVE-LEVEL-EMPTY-TAIL",
            "five-level empty tail secret",
        )
    };
    let make_wire = |reverse: bool| {
        let shared_leaf = alias("leaf alias secret")
            .with_cause(empty_tail())
            .with_cause(empty_tail());
        let shared_level_two = alias("level two alias secret")
            .with_cause(shared_leaf.clone())
            .with_cause(shared_leaf.clone());
        let shared_level_three = alias("level three alias secret")
            .with_cause(shared_level_two.clone())
            .with_cause(shared_level_two.clone());
        let shared_level_four = alias("level four alias secret")
            .with_cause(shared_level_three.clone())
            .with_cause(shared_level_three.clone());
        let shared_level_five = alias("level five alias secret")
            .with_cause(shared_level_four.clone())
            .with_cause(shared_level_four.clone());

        let grown_leaf = shared_leaf.clone().with_cause(empty_tail());
        let grown_level_two = shared_level_two.clone().with_cause(grown_leaf);
        let grown_level_three = shared_level_three.clone().with_cause(grown_level_two);
        let grown_level_four = shared_level_four.clone().with_cause(grown_level_three);
        let grown_level_five = shared_level_five.clone().with_cause(grown_level_four);
        let aliases = if reverse {
            vec![shared_level_five, grown_level_five]
        } else {
            vec![grown_level_five, shared_level_five]
        };
        aliases
            .into_iter()
            .fold(
                diagnostic("ORNA-E-FIVE-LEVEL-EMPTY-ROOT", "root secret"),
                |root, alias| root.with_cause(alias),
            )
            .redacted()
            .encode_ovb()
            .unwrap()
    };
    fn five_level_empty_alias_shape(
        diagnostic: &serde_json::Value,
    ) -> Vec<Vec<Vec<Vec<Vec<usize>>>>> {
        let aliases = diagnostic["causes"].as_array().unwrap();
        assert_eq!(aliases.len(), 2);
        aliases
            .iter()
            .map(|level_five| {
                assert_eq!(level_five["code"], "ORNA-E-FIVE-LEVEL-EMPTY-ALIAS");
                level_five["causes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|level_four| {
                        assert_eq!(level_four["code"], "ORNA-E-FIVE-LEVEL-EMPTY-ALIAS");
                        level_four["causes"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|level_three| {
                                assert_eq!(level_three["code"], "ORNA-E-FIVE-LEVEL-EMPTY-ALIAS");
                                level_three["causes"]
                                    .as_array()
                                    .unwrap()
                                    .iter()
                                    .map(|level_two| {
                                        assert_eq!(level_two["code"], "ORNA-E-FIVE-LEVEL-EMPTY-ALIAS");
                                        level_two["causes"]
                                            .as_array()
                                            .unwrap()
                                            .iter()
                                            .map(|leaf| {
                                                assert_eq!(
                                                    leaf["code"],
                                                    "ORNA-E-FIVE-LEVEL-EMPTY-ALIAS"
                                                );
                                                let tails = leaf["causes"].as_array().unwrap();
                                                assert!(tails.iter().all(|tail| {
                                                    tail["code"] == "ORNA-E-FIVE-LEVEL-EMPTY-TAIL"
                                                        && tail["causes"]
                                                            .as_array()
                                                            .unwrap()
                                                            .is_empty()
                                                }));
                                                tails.len()
                                            })
                                            .collect()
                                    })
                                    .collect()
                            })
                            .collect()
                    })
                    .collect()
            })
            .collect()
    }
    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for note in diagnostic["notes"].as_array().unwrap() {
            assert_eq!(note, "<redacted>");
        }
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }

    let forward_wire = make_wire(false);
    let reverse_wire = make_wire(true);
    let forward = Diagnostic::decode_ovb(&forward_wire).unwrap();
    let reverse = Diagnostic::decode_ovb(&reverse_wire).unwrap();
    let forward_projection = serde_json::to_value(&forward).unwrap();
    let reverse_projection = serde_json::to_value(&reverse).unwrap();
    assert_redacted_tree(&forward_projection);
    assert_redacted_tree(&reverse_projection);

    // ORNA-SECRET-002 leaves equal-code order unspecified; these shapes follow insertion order.
    let shared_leaf_shape = vec![2, 2];
    let shared_level_two_shape = vec![shared_leaf_shape.clone(), shared_leaf_shape.clone()];
    let shared_level_three_shape = vec![
        shared_level_two_shape.clone(),
        shared_level_two_shape.clone(),
    ];
    let shared_level_four_shape = vec![
        shared_level_three_shape.clone(),
        shared_level_three_shape.clone(),
    ];
    let grown_level_two_shape = vec![2, 2, 3];
    let grown_level_three_shape = vec![
        shared_leaf_shape.clone(),
        shared_leaf_shape.clone(),
        grown_level_two_shape,
    ];
    let grown_level_four_shape = vec![
        shared_level_two_shape.clone(),
        shared_level_two_shape.clone(),
        grown_level_three_shape,
    ];
    let grown_level_five_shape = vec![
        shared_level_three_shape.clone(),
        shared_level_three_shape.clone(),
        grown_level_four_shape,
    ];
    let grown_shape = grown_level_five_shape;
    let shared_shape = shared_level_four_shape;
    let forward_shape = vec![grown_shape.clone(), shared_shape.clone()];
    let reverse_shape = vec![shared_shape, grown_shape];
    assert_eq!(five_level_empty_alias_shape(&forward_projection), forward_shape);
    assert_eq!(five_level_empty_alias_shape(&reverse_projection), reverse_shape);

    let mut replacement = forward.clone();
    replacement.clone_from(&reverse);
    assert_eq!(
        five_level_empty_alias_shape(&serde_json::to_value(&replacement).unwrap()),
        reverse_shape
    );
    replacement.clone_from(&forward);
    assert_eq!(replacement, forward);

    let forward_json = serde_json::to_vec(&forward).unwrap();
    let reverse_json = serde_json::to_vec(&reverse).unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"root secret".as_slice(),
            b"level five alias secret".as_slice(),
            b"level four alias secret".as_slice(),
            b"level three alias secret".as_slice(),
            b"level two alias secret".as_slice(),
            b"leaf alias secret".as_slice(),
            b"five-level empty tail secret".as_slice(),
        ])
    {
        for bytes in [&forward_json, &reverse_json, &forward_wire, &reverse_wire] {
            assert!(!bytes
                .windows(disclosure.len())
                .any(|window| window == disclosure));
        }
    }
    assert_eq!(
        serde_json::to_value(Diagnostic::decode_ovb(&forward_wire).unwrap()).unwrap()["causes"],
        forward_projection["causes"]
    );
    assert_eq!(
        serde_json::to_value(Diagnostic::decode_ovb(&reverse_wire).unwrap()).unwrap()["causes"],
        reverse_projection["causes"]
    );
}

#[test]
fn zero_cause_leaf_aliases_stay_empty_across_five_clone_levels() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let diagnostic = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new(message).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let alias = |message: &str| diagnostic("ORNA-E-ZERO-CAUSE-ALIAS", message);
    let make_wire = |reverse: bool| {
        let shared_leaf = alias("empty leaf alias secret");
        let shared_level_two = alias("level two alias secret")
            .with_cause(shared_leaf.clone())
            .with_cause(shared_leaf.clone());
        let shared_level_three = alias("level three alias secret")
            .with_cause(shared_level_two.clone())
            .with_cause(shared_level_two.clone());
        let shared_level_four = alias("level four alias secret")
            .with_cause(shared_level_three.clone())
            .with_cause(shared_level_three.clone());
        let shared_level_five = alias("level five alias secret")
            .with_cause(shared_level_four.clone())
            .with_cause(shared_level_four.clone());

        let grown_leaf = shared_leaf.clone().with_cause(diagnostic(
            "ORNA-E-ZERO-CAUSE-TAIL",
            "single empty tail secret",
        ));
        let grown_level_two = shared_level_two.clone().with_cause(grown_leaf);
        let grown_level_three = shared_level_three.clone().with_cause(grown_level_two);
        let grown_level_four = shared_level_four.clone().with_cause(grown_level_three);
        let grown_level_five = shared_level_five.clone().with_cause(grown_level_four);
        let aliases = if reverse {
            vec![shared_level_five, grown_level_five]
        } else {
            vec![grown_level_five, shared_level_five]
        };
        aliases
            .into_iter()
            .fold(
                diagnostic("ORNA-E-ZERO-CAUSE-ROOT", "root secret"),
                |root, alias| root.with_cause(alias),
            )
            .redacted()
            .encode_ovb()
            .unwrap()
    };
    fn collect_leaf_tail_counts(
        alias: &serde_json::Value,
        remaining_levels: usize,
        counts: &mut Vec<usize>,
    ) {
        assert_eq!(alias["code"], "ORNA-E-ZERO-CAUSE-ALIAS");
        let causes = alias["causes"].as_array().unwrap();
        if remaining_levels == 0 {
            assert!(causes.iter().all(|tail| {
                tail["code"] == "ORNA-E-ZERO-CAUSE-TAIL"
                    && tail["causes"].as_array().unwrap().is_empty()
            }));
            counts.push(causes.len());
        } else {
            for cause in causes {
                collect_leaf_tail_counts(cause, remaining_levels - 1, counts);
            }
        }
    }
    fn five_level_leaf_tail_counts(diagnostic: &serde_json::Value) -> Vec<Vec<usize>> {
        let aliases = diagnostic["causes"].as_array().unwrap();
        assert_eq!(aliases.len(), 2);
        aliases
            .iter()
            .map(|alias| {
                let mut counts = Vec::new();
                collect_leaf_tail_counts(alias, 4, &mut counts);
                counts
            })
            .collect()
    }
    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for note in diagnostic["notes"].as_array().unwrap() {
            assert_eq!(note, "<redacted>");
        }
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }

    let forward_wire = make_wire(false);
    let reverse_wire = make_wire(true);
    let forward = Diagnostic::decode_ovb(&forward_wire).unwrap();
    let reverse = Diagnostic::decode_ovb(&reverse_wire).unwrap();
    let forward_projection = serde_json::to_value(&forward).unwrap();
    let reverse_projection = serde_json::to_value(&reverse).unwrap();
    assert_redacted_tree(&forward_projection);
    assert_redacted_tree(&reverse_projection);

    // ORNA-SECRET-002 leaves equal-code order unspecified; this proof follows insertion order.
    let mut grown_counts = vec![0; 31];
    *grown_counts.last_mut().unwrap() = 1;
    let shared_counts = vec![0; 16];
    let forward_shape = vec![grown_counts.clone(), shared_counts.clone()];
    let reverse_shape = vec![shared_counts, grown_counts];
    assert_eq!(five_level_leaf_tail_counts(&forward_projection), forward_shape);
    assert_eq!(five_level_leaf_tail_counts(&reverse_projection), reverse_shape);

    let mut replacement = forward.clone();
    replacement.clone_from(&reverse);
    assert_eq!(
        five_level_leaf_tail_counts(&serde_json::to_value(&replacement).unwrap()),
        reverse_shape
    );
    replacement.clone_from(&forward);
    assert_eq!(replacement, forward);

    let forward_json = serde_json::to_vec(&forward).unwrap();
    let reverse_json = serde_json::to_vec(&reverse).unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"root secret".as_slice(),
            b"level five alias secret".as_slice(),
            b"level four alias secret".as_slice(),
            b"level three alias secret".as_slice(),
            b"level two alias secret".as_slice(),
            b"empty leaf alias secret".as_slice(),
            b"single empty tail secret".as_slice(),
        ])
    {
        for bytes in [&forward_json, &reverse_json, &forward_wire, &reverse_wire] {
            assert!(!bytes
                .windows(disclosure.len())
                .any(|window| window == disclosure));
        }
    }
    assert_eq!(
        serde_json::to_value(Diagnostic::decode_ovb(&forward_wire).unwrap()).unwrap()["causes"],
        forward_projection["causes"]
    );
    assert_eq!(
        serde_json::to_value(Diagnostic::decode_ovb(&reverse_wire).unwrap()).unwrap()["causes"],
        reverse_projection["causes"]
    );
}

#[test]
fn zero_cause_five_level_aliases_isolate_two_independent_growth_tails() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let diagnostic = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new(message).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let alias = |message: &str| diagnostic("ORNA-E-ZERO-ALIAS-GROWTH", message);
    let make_wire = |reverse: bool| {
        let shared_leaf = alias("empty shared leaf secret");
        let shared_level_two = alias("shared level two secret")
            .with_cause(shared_leaf.clone())
            .with_cause(shared_leaf.clone());
        let shared_level_three = alias("shared level three secret")
            .with_cause(shared_level_two.clone())
            .with_cause(shared_level_two.clone());
        let shared_level_four = alias("shared level four secret")
            .with_cause(shared_level_three.clone())
            .with_cause(shared_level_three.clone());
        let shared_level_five = alias("shared level five secret")
            .with_cause(shared_level_four.clone())
            .with_cause(shared_level_four.clone());

        let grow_chain = |tail_messages: &[&str]| {
            let grown_leaf = tail_messages.iter().fold(shared_leaf.clone(), |leaf, message| {
                leaf.with_cause(diagnostic(
                    "ORNA-E-ZERO-ALIAS-GROWTH-TAIL",
                    message,
                ))
            });
            let grown_level_two = shared_level_two.clone().with_cause(grown_leaf);
            let grown_level_three = shared_level_three.clone().with_cause(grown_level_two);
            let grown_level_four = shared_level_four.clone().with_cause(grown_level_three);
            shared_level_five.clone().with_cause(grown_level_four)
        };
        let single_tail = grow_chain(&["single growth tail secret"]);
        let double_tail = grow_chain(&[
            "double growth first tail secret",
            "double growth second tail secret",
        ]);
        let aliases = if reverse {
            vec![shared_level_five, double_tail, single_tail]
        } else {
            vec![single_tail, double_tail, shared_level_five]
        };
        aliases
            .into_iter()
            .fold(
                diagnostic("ORNA-E-ZERO-ALIAS-GROWTH-ROOT", "root secret"),
                |root, alias| root.with_cause(alias),
            )
            .redacted()
            .encode_ovb()
            .unwrap()
    };
    fn collect_leaf_tail_counts(
        alias: &serde_json::Value,
        remaining_levels: usize,
        counts: &mut Vec<usize>,
    ) {
        assert_eq!(alias["code"], "ORNA-E-ZERO-ALIAS-GROWTH");
        let causes = alias["causes"].as_array().unwrap();
        if remaining_levels == 0 {
            assert!(causes.iter().all(|tail| {
                tail["code"] == "ORNA-E-ZERO-ALIAS-GROWTH-TAIL"
                    && tail["causes"].as_array().unwrap().is_empty()
            }));
            counts.push(causes.len());
        } else {
            for cause in causes {
                collect_leaf_tail_counts(cause, remaining_levels - 1, counts);
            }
        }
    }
    fn five_level_leaf_tail_counts(diagnostic: &serde_json::Value) -> Vec<Vec<usize>> {
        let aliases = diagnostic["causes"].as_array().unwrap();
        assert_eq!(aliases.len(), 3);
        aliases
            .iter()
            .map(|alias| {
                let mut counts = Vec::new();
                collect_leaf_tail_counts(alias, 4, &mut counts);
                counts
            })
            .collect()
    }
    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        for note in diagnostic["notes"].as_array().unwrap() {
            assert_eq!(note, "<redacted>");
        }
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }

    let forward_wire = make_wire(false);
    let reverse_wire = make_wire(true);
    let forward = Diagnostic::decode_ovb(&forward_wire).unwrap();
    let reverse = Diagnostic::decode_ovb(&reverse_wire).unwrap();
    let forward_projection = serde_json::to_value(&forward).unwrap();
    let reverse_projection = serde_json::to_value(&reverse).unwrap();
    assert_redacted_tree(&forward_projection);
    assert_redacted_tree(&reverse_projection);

    // ORNA-SECRET-002 leaves equal-code order unspecified; these shapes follow insertion order.
    let mut single_tail_counts = vec![0; 31];
    *single_tail_counts.last_mut().unwrap() = 1;
    let mut double_tail_counts = vec![0; 31];
    *double_tail_counts.last_mut().unwrap() = 2;
    let shared_tail_counts = vec![0; 16];
    let forward_shape = vec![
        single_tail_counts.clone(),
        double_tail_counts.clone(),
        shared_tail_counts.clone(),
    ];
    let reverse_shape = vec![shared_tail_counts, double_tail_counts, single_tail_counts];
    assert_eq!(five_level_leaf_tail_counts(&forward_projection), forward_shape);
    assert_eq!(five_level_leaf_tail_counts(&reverse_projection), reverse_shape);

    let mut replacement = forward.clone();
    replacement.clone_from(&reverse);
    assert_eq!(
        five_level_leaf_tail_counts(&serde_json::to_value(&replacement).unwrap()),
        reverse_shape
    );
    replacement.clone_from(&forward);
    assert_eq!(replacement, forward);

    let forward_json = serde_json::to_vec(&forward).unwrap();
    let reverse_json = serde_json::to_vec(&reverse).unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"root secret".as_slice(),
            b"shared level five secret".as_slice(),
            b"shared level four secret".as_slice(),
            b"shared level three secret".as_slice(),
            b"shared level two secret".as_slice(),
            b"empty shared leaf secret".as_slice(),
            b"single growth tail secret".as_slice(),
            b"double growth first tail secret".as_slice(),
            b"double growth second tail secret".as_slice(),
        ])
    {
        for bytes in [&forward_json, &reverse_json, &forward_wire, &reverse_wire] {
            assert!(!bytes
                .windows(disclosure.len())
                .any(|window| window == disclosure));
        }
    }
    assert_eq!(
        serde_json::to_value(Diagnostic::decode_ovb(&forward_wire).unwrap()).unwrap()["causes"],
        forward_projection["causes"]
    );
    assert_eq!(
        serde_json::to_value(Diagnostic::decode_ovb(&reverse_wire).unwrap()).unwrap()["causes"],
        reverse_projection["causes"]
    );
}

#[test]
fn zero_cause_five_level_alias_growth_isolated_to_one_leaf_path() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let diagnostic = |message: &str| {
        Diagnostic::new(
            SafeText::new("ORNA-E-ZERO-ALIAS-ISOLATION").unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new(message).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let empty_leaf = diagnostic("empty leaf isolation secret");
    let grown_leaf = empty_leaf
        .clone()
        .with_cause(diagnostic("single isolated tail secret"));

    let shared_level_two = diagnostic("shared level two isolation secret")
        .with_cause(empty_leaf.clone())
        .with_cause(empty_leaf.clone());
    let grown_level_two = diagnostic("grown level two isolation secret")
        .with_cause(grown_leaf)
        .with_cause(empty_leaf);
    let shared_level_three = diagnostic("shared level three isolation secret")
        .with_cause(shared_level_two.clone())
        .with_cause(shared_level_two.clone());
    let grown_level_three = diagnostic("grown level three isolation secret")
        .with_cause(shared_level_two)
        .with_cause(grown_level_two);
    let shared_level_four = diagnostic("shared level four isolation secret")
        .with_cause(shared_level_three.clone())
        .with_cause(shared_level_three.clone());
    let grown_level_four = diagnostic("grown level four isolation secret")
        .with_cause(shared_level_three)
        .with_cause(grown_level_three);
    let shared_level_five = diagnostic("shared level five isolation secret")
        .with_cause(shared_level_four.clone())
        .with_cause(shared_level_four.clone());
    let grown_level_five = diagnostic("grown level five isolation secret")
        .with_cause(shared_level_four)
        .with_cause(grown_level_four);
    let wire = diagnostic("root isolation secret")
        .with_cause(shared_level_five)
        .with_cause(grown_level_five)
        .redacted()
        .encode_ovb()
        .unwrap();

    fn collect_leaf_tail_counts(
        alias: &serde_json::Value,
        remaining_levels: usize,
        counts: &mut Vec<usize>,
    ) {
        assert_eq!(alias["code"], "ORNA-E-ZERO-ALIAS-ISOLATION");
        let causes = alias["causes"].as_array().unwrap();
        if remaining_levels == 0 {
            assert!(causes.iter().all(|tail| {
                tail["code"] == "ORNA-E-ZERO-ALIAS-ISOLATION"
                    && tail["causes"].as_array().unwrap().is_empty()
            }));
            counts.push(causes.len());
        } else {
            assert_eq!(causes.len(), 2);
            for cause in causes {
                collect_leaf_tail_counts(cause, remaining_levels - 1, counts);
            }
        }
    }
    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        assert!(diagnostic["notes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|note| note == "<redacted>"));
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }

    let decoded = Diagnostic::decode_ovb(&wire).unwrap();
    let projection = serde_json::to_value(&decoded).unwrap();
    assert_redacted_tree(&projection);
    let aliases = projection["causes"].as_array().unwrap();
    assert_eq!(aliases.len(), 2);
    let mut empty_shape = Vec::new();
    collect_leaf_tail_counts(&aliases[0], 4, &mut empty_shape);
    assert_eq!(empty_shape, vec![0; 16]);

    let mut isolated_shape = Vec::new();
    collect_leaf_tail_counts(&aliases[1], 4, &mut isolated_shape);
    // The reference leaves equal-code cause order open; assert isolation without relying on it.
    isolated_shape.sort_unstable();
    let mut expected_isolated_shape = vec![0; 15];
    expected_isolated_shape.push(1);
    assert_eq!(isolated_shape, expected_isolated_shape);

    let json = serde_json::to_vec(&decoded).unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"root isolation secret".as_slice(),
            b"shared level two isolation secret".as_slice(),
            b"grown level two isolation secret".as_slice(),
            b"shared level three isolation secret".as_slice(),
            b"grown level three isolation secret".as_slice(),
            b"shared level four isolation secret".as_slice(),
            b"grown level four isolation secret".as_slice(),
            b"shared level five isolation secret".as_slice(),
            b"grown level five isolation secret".as_slice(),
            b"empty leaf isolation secret".as_slice(),
            b"single isolated tail secret".as_slice(),
        ])
    {
        for bytes in [&json, &wire] {
            assert!(!bytes
                .windows(disclosure.len())
                .any(|window| window == disclosure));
        }
    }
}

#[test]
fn zero_cause_five_level_aliases_isolate_different_leaf_tail_counts() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let diagnostic = |code: &str, message: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new(message).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let alias = |message: &str| diagnostic("ORNA-E-ZERO-ALIAS-ISOLATION-2", message);
    let tail = |message: &str| diagnostic("ORNA-E-ZERO-ALIAS-ISOLATION-2-TAIL", message);

    let empty_leaf = alias("empty leaf isolation two secret");
    let once_grown_leaf = empty_leaf
        .clone()
        .with_cause(tail("single leaf tail isolation two secret"));
    let twice_grown_leaf = empty_leaf
        .clone()
        .with_cause(tail("double leaf first tail isolation two secret"))
        .with_cause(tail("double leaf second tail isolation two secret"));

    let shared_level_two = alias("shared level two isolation two secret")
        .with_cause(empty_leaf.clone())
        .with_cause(empty_leaf.clone());
    let grown_level_two = alias("grown level two isolation two secret")
        .with_cause(once_grown_leaf)
        .with_cause(twice_grown_leaf);
    let shared_level_three = alias("shared level three isolation two secret")
        .with_cause(shared_level_two.clone())
        .with_cause(shared_level_two.clone());
    let grown_level_three = alias("grown level three isolation two secret")
        .with_cause(shared_level_two)
        .with_cause(grown_level_two);
    let shared_level_four = alias("shared level four isolation two secret")
        .with_cause(shared_level_three.clone())
        .with_cause(shared_level_three.clone());
    let grown_level_four = alias("grown level four isolation two secret")
        .with_cause(shared_level_three)
        .with_cause(grown_level_three);
    let shared_level_five = alias("shared level five isolation two secret")
        .with_cause(shared_level_four.clone())
        .with_cause(shared_level_four.clone());
    let grown_level_five = alias("grown level five isolation two secret")
        .with_cause(shared_level_four)
        .with_cause(grown_level_four);
    let wire = alias("root isolation two secret")
        .with_cause(shared_level_five)
        .with_cause(grown_level_five)
        .redacted()
        .encode_ovb()
        .unwrap();

    fn collect_leaf_tail_counts(
        alias: &serde_json::Value,
        remaining_levels: usize,
        counts: &mut Vec<usize>,
    ) {
        assert_eq!(alias["code"], "ORNA-E-ZERO-ALIAS-ISOLATION-2");
        let causes = alias["causes"].as_array().unwrap();
        if remaining_levels == 0 {
            assert!(causes.iter().all(|tail| {
                tail["code"] == "ORNA-E-ZERO-ALIAS-ISOLATION-2-TAIL"
                    && tail["causes"].as_array().unwrap().is_empty()
            }));
            counts.push(causes.len());
        } else {
            assert_eq!(causes.len(), 2);
            for cause in causes {
                collect_leaf_tail_counts(cause, remaining_levels - 1, counts);
            }
        }
    }
    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        assert!(diagnostic["notes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|note| note == "<redacted>"));
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }

    let decoded = Diagnostic::decode_ovb(&wire).unwrap();
    let projection = serde_json::to_value(&decoded).unwrap();
    assert_redacted_tree(&projection);
    let aliases = projection["causes"].as_array().unwrap();
    assert_eq!(aliases.len(), 2);

    let mut empty_shape = Vec::new();
    collect_leaf_tail_counts(&aliases[0], 4, &mut empty_shape);
    assert_eq!(empty_shape, vec![0; 16]);

    let mut grown_shape = Vec::new();
    collect_leaf_tail_counts(&aliases[1], 4, &mut grown_shape);
    // Equal-code sibling order is unspecified, so compare only the count distribution.
    grown_shape.sort_unstable();
    let mut expected_grown_shape = vec![0; 14];
    expected_grown_shape.extend([1, 2]);
    assert_eq!(grown_shape, expected_grown_shape);

    let json = serde_json::to_vec(&decoded).unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([
            fixture.as_bytes(),
            b"root isolation two secret".as_slice(),
            b"empty leaf isolation two secret".as_slice(),
            b"single leaf tail isolation two secret".as_slice(),
            b"double leaf first tail isolation two secret".as_slice(),
            b"double leaf second tail isolation two secret".as_slice(),
        ])
    {
        for bytes in [&json, &wire] {
            assert!(!bytes
                .windows(disclosure.len())
                .any(|window| window == disclosure));
        }
    }
}

#[test]
fn zero_cause_five_level_aliases_keep_distinct_leaf_tail_identity() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let diagnostic = |code: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let alias = || diagnostic("ORNA-E-ZERO-ALIAS-IDENTITY");
    let empty_leaf = alias();
    let tail_a = empty_leaf
        .clone()
        .with_cause(diagnostic("ORNA-E-ZERO-ALIAS-TAIL-A"));
    let tail_b = empty_leaf
        .clone()
        .with_cause(diagnostic("ORNA-E-ZERO-ALIAS-TAIL-B"));

    let shared_level_two = alias()
        .with_cause(empty_leaf.clone())
        .with_cause(empty_leaf.clone());
    let distinct_level_two = alias().with_cause(tail_a).with_cause(tail_b);
    let shared_level_three = alias()
        .with_cause(shared_level_two.clone())
        .with_cause(shared_level_two.clone());
    let distinct_level_three = alias()
        .with_cause(shared_level_two)
        .with_cause(distinct_level_two);
    let shared_level_four = alias()
        .with_cause(shared_level_three.clone())
        .with_cause(shared_level_three.clone());
    let distinct_level_four = alias()
        .with_cause(shared_level_three)
        .with_cause(distinct_level_three);
    let shared_level_five = alias()
        .with_cause(shared_level_four.clone())
        .with_cause(shared_level_four.clone());
    let distinct_level_five = alias()
        .with_cause(shared_level_four)
        .with_cause(distinct_level_four);
    let wire = alias()
        .with_cause(shared_level_five)
        .with_cause(distinct_level_five)
        .redacted()
        .encode_ovb()
        .unwrap();

    fn collect_leaf_tail_codes(
        alias: &serde_json::Value,
        remaining_levels: usize,
        paths: &mut Vec<Vec<String>>,
    ) {
        assert_eq!(alias["code"], "ORNA-E-ZERO-ALIAS-IDENTITY");
        let causes = alias["causes"].as_array().unwrap();
        if remaining_levels == 0 {
            let mut codes = causes
                .iter()
                .map(|tail| {
                    assert!(tail["causes"].as_array().unwrap().is_empty());
                    tail["code"].as_str().unwrap().to_owned()
                })
                .collect::<Vec<_>>();
            codes.sort();
            paths.push(codes);
        } else {
            assert_eq!(causes.len(), 2);
            for cause in causes {
                collect_leaf_tail_codes(cause, remaining_levels - 1, paths);
            }
        }
    }
    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        assert!(diagnostic["notes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|note| note == "<redacted>"));
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }

    let decoded = Diagnostic::decode_ovb(&wire).unwrap();
    let projection = serde_json::to_value(&decoded).unwrap();
    assert_redacted_tree(&projection);
    let aliases = projection["causes"].as_array().unwrap();
    assert_eq!(aliases.len(), 2);

    let mut empty_shape = Vec::new();
    collect_leaf_tail_codes(&aliases[0], 4, &mut empty_shape);
    assert_eq!(empty_shape, vec![Vec::<String>::new(); 16]);

    let mut distinct_shape = Vec::new();
    collect_leaf_tail_codes(&aliases[1], 4, &mut distinct_shape);
    // Cause sibling order is unspecified; identify the two changed leaves by their safe codes.
    distinct_shape.sort();
    let mut expected_distinct_shape = vec![Vec::<String>::new(); 14];
    expected_distinct_shape.extend([
        vec!["ORNA-E-ZERO-ALIAS-TAIL-A".to_owned()],
        vec!["ORNA-E-ZERO-ALIAS-TAIL-B".to_owned()],
    ]);
    expected_distinct_shape.sort();
    assert_eq!(distinct_shape, expected_distinct_shape);

    let json = serde_json::to_vec(&decoded).unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([fixture.as_bytes()])
    {
        for bytes in [&json, &wire] {
            assert!(!bytes
                .windows(disclosure.len())
                .any(|window| window == disclosure));
        }
    }
}

#[test]
fn zero_cause_five_level_aliases_preserve_distinct_leaf_tail_order() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let diagnostic = |code: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let alias = || diagnostic("ORNA-E-ZERO-ALIAS-ORDER");
    let empty_leaf = alias();
    let tail_a = diagnostic("ORNA-E-ZERO-ALIAS-ORDER-A");
    let tail_b = diagnostic("ORNA-E-ZERO-ALIAS-ORDER-B");
    let leaf_ab = empty_leaf
        .clone()
        .with_cause(tail_a.clone())
        .with_cause(tail_b.clone());
    let leaf_ba = empty_leaf
        .clone()
        .with_cause(tail_b)
        .with_cause(tail_a);

    let shared_level_two = alias()
        .with_cause(empty_leaf.clone())
        .with_cause(empty_leaf.clone());
    let ordered_level_two = alias().with_cause(leaf_ab).with_cause(leaf_ba);
    let shared_level_three = alias()
        .with_cause(shared_level_two.clone())
        .with_cause(shared_level_two.clone());
    let ordered_level_three = alias()
        .with_cause(shared_level_two)
        .with_cause(ordered_level_two);
    let shared_level_four = alias()
        .with_cause(shared_level_three.clone())
        .with_cause(shared_level_three.clone());
    let ordered_level_four = alias()
        .with_cause(shared_level_three)
        .with_cause(ordered_level_three);
    let shared_level_five = alias()
        .with_cause(shared_level_four.clone())
        .with_cause(shared_level_four.clone());
    let ordered_level_five = alias()
        .with_cause(shared_level_four)
        .with_cause(ordered_level_four);
    let wire = alias()
        .with_cause(shared_level_five)
        .with_cause(ordered_level_five)
        .redacted()
        .encode_ovb()
        .unwrap();

    fn collect_leaf_tail_codes(
        alias: &serde_json::Value,
        remaining_levels: usize,
        paths: &mut Vec<Vec<String>>,
    ) {
        assert_eq!(alias["code"], "ORNA-E-ZERO-ALIAS-ORDER");
        let causes = alias["causes"].as_array().unwrap();
        if remaining_levels == 0 {
            paths.push(
                causes
                    .iter()
                    .map(|tail| {
                        assert!(tail["causes"].as_array().unwrap().is_empty());
                        tail["code"].as_str().unwrap().to_owned()
                    })
                    .collect(),
            );
        } else {
            assert_eq!(causes.len(), 2);
            for cause in causes {
                collect_leaf_tail_codes(cause, remaining_levels - 1, paths);
            }
        }
    }
    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        assert!(diagnostic["notes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|note| note == "<redacted>"));
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }

    let decoded = Diagnostic::decode_ovb(&wire).unwrap();
    let projection = serde_json::to_value(&decoded).unwrap();
    assert_redacted_tree(&projection);
    let aliases = projection["causes"].as_array().unwrap();
    assert_eq!(aliases.len(), 2);

    let mut empty_shape = Vec::new();
    collect_leaf_tail_codes(&aliases[0], 4, &mut empty_shape);
    assert_eq!(empty_shape, vec![Vec::<String>::new(); 16]);

    let mut ordered_shape = Vec::new();
    collect_leaf_tail_codes(&aliases[1], 4, &mut ordered_shape);
    // ORNA represents causes as ordered Error values, so retain the construction path order.
    let mut expected_ordered_shape = vec![Vec::<String>::new(); 14];
    expected_ordered_shape.extend([
        vec![
            "ORNA-E-ZERO-ALIAS-ORDER-A".to_owned(),
            "ORNA-E-ZERO-ALIAS-ORDER-B".to_owned(),
        ],
        vec![
            "ORNA-E-ZERO-ALIAS-ORDER-B".to_owned(),
            "ORNA-E-ZERO-ALIAS-ORDER-A".to_owned(),
        ],
    ]);
    assert_eq!(ordered_shape, expected_ordered_shape);

    let json = serde_json::to_vec(&decoded).unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([fixture.as_bytes()])
    {
        for bytes in [&json, &wire] {
            assert!(!bytes
                .windows(disclosure.len())
                .any(|window| window == disclosure));
        }
    }
}

#[test]
fn clone_from_preserves_ordered_distinct_zero_cause_five_level_aliases() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let diagnostic = |code: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let alias = || diagnostic("ORNA-E-ZERO-ALIAS-REPLACEMENT");
    let empty_leaf = alias();
    let tail_a = diagnostic("ORNA-E-ZERO-ALIAS-REPLACEMENT-A");
    let tail_b = diagnostic("ORNA-E-ZERO-ALIAS-REPLACEMENT-B");
    let leaf_ab = empty_leaf
        .clone()
        .with_cause(tail_a.clone())
        .with_cause(tail_b.clone());
    let leaf_ba = empty_leaf
        .clone()
        .with_cause(tail_b)
        .with_cause(tail_a);

    let shared_level_two = alias()
        .with_cause(empty_leaf.clone())
        .with_cause(empty_leaf.clone());
    let shared_level_three = alias()
        .with_cause(shared_level_two.clone())
        .with_cause(shared_level_two.clone());
    let shared_level_four = alias()
        .with_cause(shared_level_three.clone())
        .with_cause(shared_level_three.clone());
    let shared_level_five = alias()
        .with_cause(shared_level_four.clone())
        .with_cause(shared_level_four.clone());
    let make_variant = |first_leaf: Diagnostic, second_leaf: Diagnostic| {
        let distinct_level_two = alias()
            .with_cause(first_leaf)
            .with_cause(second_leaf);
        let distinct_level_three = alias()
            .with_cause(shared_level_two.clone())
            .with_cause(distinct_level_two);
        let distinct_level_four = alias()
            .with_cause(shared_level_three.clone())
            .with_cause(distinct_level_three);
        alias()
            .with_cause(shared_level_four.clone())
            .with_cause(distinct_level_four)
    };
    let ab_ba_variant = make_variant(leaf_ab.clone(), leaf_ba.clone());
    let ba_ab_variant = make_variant(leaf_ba, leaf_ab);
    let make_root = |causes: Vec<Diagnostic>| {
        causes
            .into_iter()
            .fold(alias(), |root, cause| root.with_cause(cause))
            .redacted()
    };

    fn collect_leaf_tail_codes(
        alias: &serde_json::Value,
        remaining_levels: usize,
        paths: &mut Vec<Vec<String>>,
    ) {
        assert_eq!(alias["code"], "ORNA-E-ZERO-ALIAS-REPLACEMENT");
        let causes = alias["causes"].as_array().unwrap();
        if remaining_levels == 0 {
            paths.push(
                causes
                    .iter()
                    .map(|tail| {
                        assert!(tail["causes"].as_array().unwrap().is_empty());
                        tail["code"].as_str().unwrap().to_owned()
                    })
                    .collect(),
            );
        } else {
            assert_eq!(causes.len(), 2);
            for cause in causes {
                collect_leaf_tail_codes(cause, remaining_levels - 1, paths);
            }
        }
    }
    fn root_tail_shapes(diagnostic: &serde_json::Value) -> Vec<Vec<Vec<String>>> {
        diagnostic["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|alias| {
                let mut paths = Vec::new();
                collect_leaf_tail_codes(alias, 4, &mut paths);
                paths
            })
            .collect()
    }
    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        assert!(diagnostic["notes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|note| note == "<redacted>"));
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }

    let mut ab_ba_shape = vec![Vec::<String>::new(); 14];
    ab_ba_shape.extend([
        vec![
            "ORNA-E-ZERO-ALIAS-REPLACEMENT-A".to_owned(),
            "ORNA-E-ZERO-ALIAS-REPLACEMENT-B".to_owned(),
        ],
        vec![
            "ORNA-E-ZERO-ALIAS-REPLACEMENT-B".to_owned(),
            "ORNA-E-ZERO-ALIAS-REPLACEMENT-A".to_owned(),
        ],
    ]);
    let mut ba_ab_shape = vec![Vec::<String>::new(); 14];
    ba_ab_shape.extend([
        vec![
            "ORNA-E-ZERO-ALIAS-REPLACEMENT-B".to_owned(),
            "ORNA-E-ZERO-ALIAS-REPLACEMENT-A".to_owned(),
        ],
        vec![
            "ORNA-E-ZERO-ALIAS-REPLACEMENT-A".to_owned(),
            "ORNA-E-ZERO-ALIAS-REPLACEMENT-B".to_owned(),
        ],
    ]);
    let empty_shape = vec![Vec::<String>::new(); 16];
    let replacement = make_root(vec![
        ab_ba_variant.clone(),
        shared_level_five.clone(),
        ba_ab_variant.clone(),
    ]);
    let mut receiver = make_root(vec![
        ba_ab_variant,
        ab_ba_variant,
        shared_level_five,
    ]);
    assert_eq!(
        root_tail_shapes(&serde_json::to_value(&receiver).unwrap()),
        vec![ba_ab_shape.clone(), ab_ba_shape.clone(), empty_shape.clone()]
    );

    // Error causes are ordered values; same-length replacement must retain the source sequence.
    receiver.clone_from(&replacement);
    let replaced_projection = serde_json::to_value(&receiver).unwrap();
    assert_redacted_tree(&replaced_projection);
    let expected_shapes = vec![ab_ba_shape, empty_shape, ba_ab_shape];
    assert_eq!(root_tail_shapes(&replaced_projection), expected_shapes);
    assert_eq!(
        root_tail_shapes(&serde_json::to_value(&replacement).unwrap()),
        expected_shapes
    );

    let wire = receiver.encode_ovb().unwrap();
    let decoded = Diagnostic::decode_ovb(&wire).unwrap();
    let decoded_projection = serde_json::to_value(&decoded).unwrap();
    assert_redacted_tree(&decoded_projection);
    assert_eq!(root_tail_shapes(&decoded_projection), expected_shapes);
    let json = serde_json::to_vec(&decoded).unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([fixture.as_bytes()])
    {
        for bytes in [&json, &wire] {
            assert!(!bytes
                .windows(disclosure.len())
                .any(|window| window == disclosure));
        }
    }
}

#[test]
fn shrinking_clone_from_keeps_ordered_zero_cause_five_level_tails() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let diagnostic = |code: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let alias = || diagnostic("ORNA-E-ZERO-ALIAS-SHRINK");
    let empty_leaf = alias();
    let tail_a = diagnostic("ORNA-E-ZERO-ALIAS-SHRINK-A");
    let tail_b = diagnostic("ORNA-E-ZERO-ALIAS-SHRINK-B");
    let leaf_ab = empty_leaf
        .clone()
        .with_cause(tail_a.clone())
        .with_cause(tail_b.clone());
    let leaf_ba = empty_leaf
        .clone()
        .with_cause(tail_b)
        .with_cause(tail_a);

    let shared_level_two = || {
        alias()
            .with_cause(empty_leaf.clone())
            .with_cause(empty_leaf.clone())
    };
    let shared_level_three = || {
        let shared = shared_level_two();
        alias().with_cause(shared.clone()).with_cause(shared)
    };
    let shared_level_four = || {
        let shared = shared_level_three();
        alias().with_cause(shared.clone()).with_cause(shared)
    };
    let empty_level_five = {
        let shared = shared_level_four();
        alias().with_cause(shared.clone()).with_cause(shared)
    };
    let make_ordered_level_five = |first_leaf: Diagnostic, second_leaf: Diagnostic| {
        let distinct_level_two = alias().with_cause(first_leaf).with_cause(second_leaf);
        let distinct_level_three = alias()
            .with_cause(shared_level_two())
            .with_cause(distinct_level_two);
        let distinct_level_four = alias()
            .with_cause(shared_level_three())
            .with_cause(distinct_level_three);
        alias()
            .with_cause(shared_level_four())
            .with_cause(distinct_level_four)
    };
    let ab_ba = make_ordered_level_five(leaf_ab.clone(), leaf_ba.clone());
    let ba_ab = make_ordered_level_five(leaf_ba, leaf_ab);
    let make_root = |causes: Vec<Diagnostic>| {
        causes
            .into_iter()
            .fold(alias(), |root, cause| root.with_cause(cause))
            .redacted()
    };

    fn collect_leaf_tail_codes(
        alias: &serde_json::Value,
        remaining_levels: usize,
        codes: &mut Vec<String>,
    ) {
        let causes = alias["causes"].as_array().unwrap();
        if remaining_levels == 0 {
            for tail in causes {
                assert!(tail["causes"].as_array().unwrap().is_empty());
                codes.push(tail["code"].as_str().unwrap().to_owned());
            }
        } else {
            assert_eq!(causes.len(), 2);
            for cause in causes {
                collect_leaf_tail_codes(cause, remaining_levels - 1, codes);
            }
        }
    }

    let replacement = make_root(vec![ba_ab, empty_level_five, ab_ba]);
    let mut receiver = make_root(vec![
        diagnostic("ORNA-E-ZERO-ALIAS-STALE"),
        diagnostic("ORNA-E-ZERO-ALIAS-SHRINK-A"),
        diagnostic("ORNA-E-ZERO-ALIAS-SHRINK-B"),
        diagnostic("ORNA-E-ZERO-ALIAS-SHRINK-EXTRA"),
    ]);

    // The format defines causes as ordered values, while clone_from shrinkage is
    // unspecified there; use ordinary source replacement and discard old excess causes.
    receiver.clone_from(&replacement);
    let projection = serde_json::to_value(&receiver).unwrap();
    assert_eq!(projection, serde_json::to_value(&replacement).unwrap());
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 3);

    let mut first_tail = Vec::new();
    collect_leaf_tail_codes(&causes[0], 4, &mut first_tail);
    assert_eq!(
        first_tail,
        [
            "ORNA-E-ZERO-ALIAS-SHRINK-B",
            "ORNA-E-ZERO-ALIAS-SHRINK-A",
            "ORNA-E-ZERO-ALIAS-SHRINK-A",
            "ORNA-E-ZERO-ALIAS-SHRINK-B",
        ]
    );
    let mut empty_tail = Vec::new();
    collect_leaf_tail_codes(&causes[1], 4, &mut empty_tail);
    assert!(empty_tail.is_empty());
    let mut last_tail = Vec::new();
    collect_leaf_tail_codes(&causes[2], 4, &mut last_tail);
    assert_eq!(
        last_tail,
        [
            "ORNA-E-ZERO-ALIAS-SHRINK-A",
            "ORNA-E-ZERO-ALIAS-SHRINK-B",
            "ORNA-E-ZERO-ALIAS-SHRINK-B",
            "ORNA-E-ZERO-ALIAS-SHRINK-A",
        ]
    );

    let wire = receiver.encode_ovb().unwrap();
    let decoded = Diagnostic::decode_ovb(&wire).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), projection);
    let json = serde_json::to_vec(&projection).unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([fixture.as_bytes()])
    {
        for bytes in [&json, &wire] {
            assert!(!bytes
                .windows(disclosure.len())
                .any(|window| window == disclosure));
        }
    }
}

#[test]
fn growing_clone_from_keeps_ordered_zero_cause_five_level_tails() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    fn alias(fixture: &str, code: &str) -> Diagnostic {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    }
    fn empty_tree(fixture: &str, code: &str, levels: usize) -> Diagnostic {
        if levels == 0 {
            alias(fixture, code)
        } else {
            alias(fixture, code)
                .with_cause(empty_tree(fixture, code, levels - 1))
                .with_cause(empty_tree(fixture, code, levels - 1))
        }
    }
    fn ordered_tree(
        fixture: &str,
        code: &str,
        ordered_leaf: Diagnostic,
        levels: usize,
    ) -> Diagnostic {
        if levels == 0 {
            ordered_leaf
        } else {
            alias(fixture, code)
                .with_cause(empty_tree(fixture, code, levels - 1))
                .with_cause(ordered_tree(fixture, code, ordered_leaf, levels - 1))
        }
    }
    fn collect_leaf_tail_codes(
        alias: &serde_json::Value,
        remaining_levels: usize,
        codes: &mut Vec<String>,
    ) {
        let causes = alias["causes"].as_array().unwrap();
        if remaining_levels == 0 {
            for tail in causes {
                assert!(tail["causes"].as_array().unwrap().is_empty());
                codes.push(tail["code"].as_str().unwrap().to_owned());
            }
        } else {
            assert_eq!(causes.len(), 2);
            for cause in causes {
                collect_leaf_tail_codes(cause, remaining_levels - 1, codes);
            }
        }
    }

    let code = "ORNA-E-ZERO-ALIAS-GROW";
    let leaf_ab = alias(fixture, code)
        .with_cause(alias(fixture, "ORNA-E-ZERO-ALIAS-GROW-A"))
        .with_cause(alias(fixture, "ORNA-E-ZERO-ALIAS-GROW-B"));
    let leaf_ba = alias(fixture, code)
        .with_cause(alias(fixture, "ORNA-E-ZERO-ALIAS-GROW-B"))
        .with_cause(alias(fixture, "ORNA-E-ZERO-ALIAS-GROW-A"));
    let replacement = alias(fixture, code)
        .with_cause(ordered_tree(fixture, code, leaf_ba, 4))
        .with_cause(empty_tree(fixture, code, 4))
        .with_cause(ordered_tree(fixture, code, leaf_ab, 4))
        .redacted();
    let mut receiver = alias(fixture, code).redacted();

    // The format defines causes as ordered values but leaves clone_from growth open;
    // use source replacement so the zero-cause receiver adopts the source sequence.
    receiver.clone_from(&replacement);
    let projection = serde_json::to_value(&receiver).unwrap();
    assert_eq!(projection, serde_json::to_value(&replacement).unwrap());
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 3);

    let mut first_tail = Vec::new();
    collect_leaf_tail_codes(&causes[0], 4, &mut first_tail);
    assert_eq!(first_tail, ["ORNA-E-ZERO-ALIAS-GROW-B", "ORNA-E-ZERO-ALIAS-GROW-A"]);
    let mut empty_tail = Vec::new();
    collect_leaf_tail_codes(&causes[1], 4, &mut empty_tail);
    assert!(empty_tail.is_empty());
    let mut last_tail = Vec::new();
    collect_leaf_tail_codes(&causes[2], 4, &mut last_tail);
    assert_eq!(last_tail, ["ORNA-E-ZERO-ALIAS-GROW-A", "ORNA-E-ZERO-ALIAS-GROW-B"]);

    let wire = receiver.encode_ovb().unwrap();
    let decoded = Diagnostic::decode_ovb(&wire).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), projection);
    let json = serde_json::to_vec(&projection).unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([fixture.as_bytes()])
    {
        for bytes in [&json, &wire] {
            assert!(!bytes
                .windows(disclosure.len())
                .any(|window| window == disclosure));
        }
    }
}

#[test]
fn decoded_clone_from_keeps_ordered_zero_cause_tail_siblings() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let diagnostic = |code: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let empty = diagnostic("ORNA-E-ZERO-ALIAS-DECODED-EMPTY");
    let ordered_ab = diagnostic("ORNA-E-ZERO-ALIAS-DECODED-AB")
        .with_cause(diagnostic("ORNA-E-ZERO-ALIAS-DECODED-A"))
        .with_cause(diagnostic("ORNA-E-ZERO-ALIAS-DECODED-B"));
    let ordered_ba = diagnostic("ORNA-E-ZERO-ALIAS-DECODED-BA")
        .with_cause(diagnostic("ORNA-E-ZERO-ALIAS-DECODED-B"))
        .with_cause(diagnostic("ORNA-E-ZERO-ALIAS-DECODED-A"));
    let source = diagnostic("ORNA-E-ZERO-ALIAS-DECODED-ROOT")
        .with_cause(empty)
        .with_cause(ordered_ab)
        .with_cause(ordered_ba)
        .redacted();
    let decoded_source = Diagnostic::decode_ovb(&source.encode_ovb().unwrap()).unwrap();
    let mut receiver = diagnostic("ORNA-E-ZERO-ALIAS-DECODED-ROOT")
        .with_cause(diagnostic("ORNA-E-ZERO-ALIAS-DECODED-STALE"))
        .redacted();

    // Causes are ordered in the format; for a decoded replacement, retain the source sequence.
    receiver.clone_from(&decoded_source);
    let projection = serde_json::to_value(&receiver).unwrap();
    assert_eq!(projection, serde_json::to_value(&decoded_source).unwrap());
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(
        causes
            .iter()
            .map(|cause| cause["code"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "ORNA-E-ZERO-ALIAS-DECODED-EMPTY",
            "ORNA-E-ZERO-ALIAS-DECODED-AB",
            "ORNA-E-ZERO-ALIAS-DECODED-BA",
        ]
    );
    assert!(causes[0]["causes"].as_array().unwrap().is_empty());
    assert_eq!(
        causes[1]["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|cause| cause["code"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["ORNA-E-ZERO-ALIAS-DECODED-A", "ORNA-E-ZERO-ALIAS-DECODED-B"]
    );
    assert_eq!(
        causes[2]["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|cause| cause["code"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["ORNA-E-ZERO-ALIAS-DECODED-B", "ORNA-E-ZERO-ALIAS-DECODED-A"]
    );

    let wire = receiver.encode_ovb().unwrap();
    let replayed = Diagnostic::decode_ovb(&wire).unwrap();
    let replayed_json = serde_json::to_vec(&replayed).unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([fixture.as_bytes()])
    {
        for bytes in [&replayed_json, &wire] {
            assert!(!bytes
                .windows(disclosure.len())
                .any(|window| window == disclosure));
        }
    }
}

#[test]
fn composed_decoded_aliases_keep_ordered_zero_cause_siblings() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let diagnostic = |code: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let ordered_source = diagnostic("ORNA-E-DECODED-ORDERED-ROOT")
        .with_cause(diagnostic("ORNA-E-DECODED-EMPTY"))
        .with_cause(
            diagnostic("ORNA-E-DECODED-AB")
                .with_cause(diagnostic("ORNA-E-DECODED-A"))
                .with_cause(diagnostic("ORNA-E-DECODED-B")),
        )
        .with_cause(
            diagnostic("ORNA-E-DECODED-BA")
                .with_cause(diagnostic("ORNA-E-DECODED-B"))
                .with_cause(diagnostic("ORNA-E-DECODED-A")),
        )
        .redacted();
    let decoded_alias = Diagnostic::decode_ovb(&ordered_source.encode_ovb().unwrap()).unwrap();
    let composed = diagnostic("ORNA-E-DECODED-COMPOSED-ROOT")
        .with_cause(decoded_alias.clone())
        .with_cause(diagnostic("ORNA-E-DECODED-COMPOSED-EMPTY"))
        .with_cause(decoded_alias)
        .redacted();

    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        assert!(diagnostic["notes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|note| note == "<redacted>"));
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    let projection = serde_json::to_value(&composed).unwrap();
    assert_redacted_tree(&projection);
    let outer_causes = projection["causes"].as_array().unwrap();
    assert_eq!(outer_causes.len(), 3);
    assert_eq!(outer_causes[0], outer_causes[2]);
    assert_eq!(
        outer_causes
            .iter()
            .map(|cause| cause["code"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "ORNA-E-DECODED-ORDERED-ROOT",
            "ORNA-E-DECODED-COMPOSED-EMPTY",
            "ORNA-E-DECODED-ORDERED-ROOT",
        ]
    );
    assert!(outer_causes[1]["causes"].as_array().unwrap().is_empty());
    let nested_causes = outer_causes[0]["causes"].as_array().unwrap();
    assert_eq!(
        nested_causes
            .iter()
            .map(|cause| cause["code"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["ORNA-E-DECODED-EMPTY", "ORNA-E-DECODED-AB", "ORNA-E-DECODED-BA"]
    );
    assert!(nested_causes[0]["causes"].as_array().unwrap().is_empty());
    assert_eq!(
        nested_causes[1]["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|cause| cause["code"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["ORNA-E-DECODED-A", "ORNA-E-DECODED-B"]
    );
    assert_eq!(
        nested_causes[2]["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|cause| cause["code"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["ORNA-E-DECODED-B", "ORNA-E-DECODED-A"]
    );

    let wire = composed.encode_ovb().unwrap();
    let replayed = Diagnostic::decode_ovb(&wire).unwrap();
    let replayed_projection = serde_json::to_value(&replayed).unwrap();
    assert_redacted_tree(&replayed_projection);
    assert_eq!(replayed_projection, projection);
    let json = serde_json::to_vec(&replayed_projection).unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([fixture.as_bytes()])
    {
        for bytes in [&json, &wire] {
            assert!(!bytes
                .windows(disclosure.len())
                .any(|window| window == disclosure));
        }
    }
}

#[test]
fn composed_decoded_empty_roots_keep_ordered_tail_sibling() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let diagnostic = |code: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let decoded_empty = Diagnostic::decode_ovb(
        &diagnostic("ORNA-E-DECODED-EMPTY-ROOT")
            .redacted()
            .encode_ovb()
            .unwrap(),
    )
    .unwrap();
    let decoded_ordered = Diagnostic::decode_ovb(
        &diagnostic("ORNA-E-DECODED-ORDERED-TAIL")
            .with_cause(diagnostic("ORNA-E-DECODED-TAIL-A"))
            .with_cause(diagnostic("ORNA-E-DECODED-TAIL-B"))
            .redacted()
            .encode_ovb()
            .unwrap(),
    )
    .unwrap();
    let composed = diagnostic("ORNA-E-DECODED-EMPTY-COMPOSED")
        .with_cause(decoded_empty.clone())
        .with_cause(decoded_ordered)
        .with_cause(decoded_empty)
        .redacted();

    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        assert!(diagnostic["notes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|note| note == "<redacted>"));
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    let projection = serde_json::to_value(&composed).unwrap();
    assert_redacted_tree(&projection);
    let causes = projection["causes"].as_array().unwrap();
    assert_eq!(causes.len(), 3);
    assert_eq!(
        causes
            .iter()
            .map(|cause| cause["code"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "ORNA-E-DECODED-EMPTY-ROOT",
            "ORNA-E-DECODED-ORDERED-TAIL",
            "ORNA-E-DECODED-EMPTY-ROOT",
        ]
    );
    assert_eq!(causes[0], causes[2]);
    assert!(causes[0]["causes"].as_array().unwrap().is_empty());
    assert_eq!(
        causes[1]["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|cause| cause["code"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["ORNA-E-DECODED-TAIL-A", "ORNA-E-DECODED-TAIL-B"]
    );

    let wire = composed.encode_ovb().unwrap();
    let replayed = Diagnostic::decode_ovb(&wire).unwrap();
    let replayed_projection = serde_json::to_value(&replayed).unwrap();
    assert_redacted_tree(&replayed_projection);
    assert_eq!(replayed_projection, projection);
    let json = serde_json::to_vec(&replayed_projection).unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([fixture.as_bytes()])
    {
        for bytes in [&json, &wire] {
            assert!(!bytes
                .windows(disclosure.len())
                .any(|window| window == disclosure));
        }
    }
}

#[test]
fn composed_decoded_clone_keeps_appended_zero_cause_tail_local() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let diagnostic = |code: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let source = diagnostic("ORNA-E-DECODED-APPEND-ROOT")
        .with_cause(diagnostic("ORNA-E-DECODED-APPEND-EMPTY"))
        .with_cause(
            diagnostic("ORNA-E-DECODED-APPEND-ORDERED")
                .with_cause(diagnostic("ORNA-E-DECODED-APPEND-A"))
                .with_cause(diagnostic("ORNA-E-DECODED-APPEND-B")),
        )
        .redacted();
    let decoded = Diagnostic::decode_ovb(&source.encode_ovb().unwrap()).unwrap();
    // Appending to one decoded clone must leave its sibling copy unchanged.
    let composed = diagnostic("ORNA-E-DECODED-APPEND-COMPOSED")
        .with_cause(
            decoded
                .clone()
                .with_cause(diagnostic("ORNA-E-DECODED-APPEND-TAIL"))
                .redacted(),
        )
        .with_cause(decoded)
        .redacted();

    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        assert!(diagnostic["notes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|note| note == "<redacted>"));
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    let projection = serde_json::to_value(&composed).unwrap();
    assert_redacted_tree(&projection);
    let siblings = projection["causes"].as_array().unwrap();
    assert_eq!(siblings.len(), 2);
    let extended = siblings[0]["causes"].as_array().unwrap();
    let original = siblings[1]["causes"].as_array().unwrap();
    assert_eq!(extended.len(), 3);
    assert_eq!(original.len(), 2);
    assert_eq!(extended[0]["code"], "ORNA-E-DECODED-APPEND-EMPTY");
    assert!(extended[0]["causes"].as_array().unwrap().is_empty());
    assert_eq!(extended[1]["code"], "ORNA-E-DECODED-APPEND-ORDERED");
    assert_eq!(
        extended[1]["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|cause| cause["code"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["ORNA-E-DECODED-APPEND-A", "ORNA-E-DECODED-APPEND-B"]
    );
    assert_eq!(extended[2]["code"], "ORNA-E-DECODED-APPEND-TAIL");
    assert!(extended[2]["causes"].as_array().unwrap().is_empty());
    assert_eq!(original[0], extended[0]);
    assert_eq!(original[1], extended[1]);

    let wire = composed.encode_ovb().unwrap();
    let replayed = Diagnostic::decode_ovb(&wire).unwrap();
    let replayed_projection = serde_json::to_value(&replayed).unwrap();
    assert_redacted_tree(&replayed_projection);
    assert_eq!(replayed_projection, projection);
    let json = serde_json::to_vec(&replayed_projection).unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([fixture.as_bytes()])
    {
        for bytes in [&json, &wire] {
            assert!(!bytes
                .windows(disclosure.len())
                .any(|window| window == disclosure));
        }
    }
}

#[test]
fn composed_decoded_clone_tails_remain_isolated_between_siblings() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let diagnostic = |code: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let source = diagnostic("ORNA-E-DECODED-CLONE-BASE")
        .with_cause(diagnostic("ORNA-E-DECODED-CLONE-EMPTY"))
        .redacted();
    let decoded_base = Diagnostic::decode_ovb(&source.encode_ovb().unwrap()).unwrap();
    let clone_b = decoded_base
        .clone()
        .with_cause(diagnostic("ORNA-E-DECODED-CLONE-TAIL-B"))
        .redacted();
    let clone_a = decoded_base
        .clone()
        .with_cause(diagnostic("ORNA-E-DECODED-CLONE-TAIL-A"))
        .redacted();
    // Causes are ordered values; append independently, then compose in B-before-A order.
    let composed = diagnostic("ORNA-E-DECODED-CLONE-COMPOSED")
        .with_cause(clone_b)
        .with_cause(clone_a)
        .redacted();

    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        assert!(diagnostic["notes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|note| note == "<redacted>"));
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    let projection = serde_json::to_value(&composed).unwrap();
    assert_redacted_tree(&projection);
    assert_eq!(
        serde_json::to_value(&decoded_base).unwrap()["causes"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let siblings = projection["causes"].as_array().unwrap();
    assert_eq!(
        siblings
            .iter()
            .map(|cause| cause["code"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "ORNA-E-DECODED-CLONE-BASE",
            "ORNA-E-DECODED-CLONE-BASE",
        ]
    );
    for (sibling, tail_code) in siblings.iter().zip([
        "ORNA-E-DECODED-CLONE-TAIL-B",
        "ORNA-E-DECODED-CLONE-TAIL-A",
    ]) {
        let causes = sibling["causes"].as_array().unwrap();
        assert_eq!(causes.len(), 2);
        assert_eq!(causes[0]["code"], "ORNA-E-DECODED-CLONE-EMPTY");
        assert!(causes[0]["causes"].as_array().unwrap().is_empty());
        assert_eq!(causes[1]["code"], tail_code);
        assert!(causes[1]["causes"].as_array().unwrap().is_empty());
    }

    let wire = composed.encode_ovb().unwrap();
    let replayed = Diagnostic::decode_ovb(&wire).unwrap();
    let replayed_projection = serde_json::to_value(&replayed).unwrap();
    assert_redacted_tree(&replayed_projection);
    assert_eq!(replayed_projection, projection);
    let json = serde_json::to_vec(&replayed_projection).unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([fixture.as_bytes()])
    {
        for bytes in [&json, &wire] {
            assert!(!bytes
                .windows(disclosure.len())
                .any(|window| window == disclosure));
        }
    }
}

#[test]
fn composed_decoded_duplicate_tail_counts_stay_local() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let diagnostic = |code: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let source = diagnostic("ORNA-E-DECODED-DUPLICATE-BASE")
        .with_cause(diagnostic("ORNA-E-DECODED-DUPLICATE-EMPTY"))
        .redacted();
    let decoded_base = Diagnostic::decode_ovb(&source.encode_ovb().unwrap()).unwrap();
    let one_tail = decoded_base
        .clone()
        .with_cause(diagnostic("ORNA-E-DECODED-DUPLICATE-TAIL"))
        .redacted();
    let two_tails = decoded_base
        .clone()
        .with_cause(diagnostic("ORNA-E-DECODED-DUPLICATE-TAIL"))
        .with_cause(diagnostic("ORNA-E-DECODED-DUPLICATE-TAIL"))
        .redacted();
    // Equal-code causes are still ordered values; keep each clone's multiplicity local.
    let composed = diagnostic("ORNA-E-DECODED-DUPLICATE-COMPOSED")
        .with_cause(one_tail)
        .with_cause(decoded_base.clone())
        .with_cause(two_tails)
        .redacted();

    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        assert!(diagnostic["notes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|note| note == "<redacted>"));
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    let projection = serde_json::to_value(&composed).unwrap();
    assert_redacted_tree(&projection);
    let siblings = projection["causes"].as_array().unwrap();
    assert_eq!(siblings.len(), 3);
    for (sibling, expected_tail_count) in siblings.iter().zip([1, 0, 2]) {
        assert_eq!(sibling["code"], "ORNA-E-DECODED-DUPLICATE-BASE");
        let causes = sibling["causes"].as_array().unwrap();
        assert_eq!(causes.len(), 1 + expected_tail_count);
        assert_eq!(causes[0]["code"], "ORNA-E-DECODED-DUPLICATE-EMPTY");
        assert!(causes[0]["causes"].as_array().unwrap().is_empty());
        for tail in &causes[1..] {
            assert_eq!(tail["code"], "ORNA-E-DECODED-DUPLICATE-TAIL");
            assert!(tail["causes"].as_array().unwrap().is_empty());
        }
    }
    assert_eq!(
        serde_json::to_value(&decoded_base).unwrap()["causes"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let wire = composed.encode_ovb().unwrap();
    let replayed = Diagnostic::decode_ovb(&wire).unwrap();
    let replayed_projection = serde_json::to_value(&replayed).unwrap();
    assert_redacted_tree(&replayed_projection);
    assert_eq!(replayed_projection, projection);
    let json = serde_json::to_vec(&replayed_projection).unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([fixture.as_bytes()])
    {
        for bytes in [&json, &wire] {
            assert!(!bytes
                .windows(disclosure.len())
                .any(|window| window == disclosure));
        }
    }
}

#[test]
fn composed_decoded_ordered_tail_stays_contained_in_its_branch() {
    let fixture = include_str!("fixtures/diagnostic-parent-replacement.orna").trim();
    let fixture_credentials = fixture
        .lines()
        .filter_map(|line| line.split_once('=')?.1.trim().strip_suffix(';'))
        .map(|value| value.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(fixture_credentials.len(), 2);

    let diagnostic = |code: &str| {
        Diagnostic::new(
            SafeText::new(code).unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new(fixture).unwrap(),
        )
        .unwrap()
        .with_note(SafeText::new(fixture).unwrap())
    };
    let decoded_empty = Diagnostic::decode_ovb(
        &diagnostic("ORNA-E-CONTAINED-EMPTY")
            .redacted()
            .encode_ovb()
            .unwrap(),
    )
    .unwrap();
    let decoded_middle = Diagnostic::decode_ovb(
        &diagnostic("ORNA-E-CONTAINED-MIDDLE")
            .with_cause(decoded_empty.clone())
            .with_cause(
                diagnostic("ORNA-E-CONTAINED-ORDERED")
                    .with_cause(diagnostic("ORNA-E-CONTAINED-A"))
                    .with_cause(diagnostic("ORNA-E-CONTAINED-B")),
            )
            .redacted()
            .encode_ovb()
            .unwrap(),
    )
    .unwrap();
    let composed = diagnostic("ORNA-E-CONTAINED-OUTER")
        .with_cause(decoded_middle.clone())
        .with_cause(decoded_empty)
        .with_cause(decoded_middle)
        .redacted();

    fn assert_redacted_tree(diagnostic: &serde_json::Value) {
        assert_eq!(diagnostic["message"], "<redacted>");
        assert!(diagnostic["notes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|note| note == "<redacted>"));
        for cause in diagnostic["causes"].as_array().unwrap() {
            assert_redacted_tree(cause);
        }
    }
    let projection = serde_json::to_value(&composed).unwrap();
    assert_redacted_tree(&projection);
    let outer_causes = projection["causes"].as_array().unwrap();
    assert_eq!(outer_causes.len(), 3);
    assert_eq!(outer_causes[0], outer_causes[2]);
    assert_eq!(outer_causes[0]["code"], "ORNA-E-CONTAINED-MIDDLE");
    assert_eq!(outer_causes[1]["code"], "ORNA-E-CONTAINED-EMPTY");
    assert!(outer_causes[1]["causes"].as_array().unwrap().is_empty());

    let middle_causes = outer_causes[0]["causes"].as_array().unwrap();
    assert_eq!(middle_causes.len(), 2);
    assert_eq!(middle_causes[0]["code"], "ORNA-E-CONTAINED-EMPTY");
    assert!(middle_causes[0]["causes"].as_array().unwrap().is_empty());
    assert_eq!(middle_causes[1]["code"], "ORNA-E-CONTAINED-ORDERED");
    assert_eq!(
        middle_causes[1]["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|cause| cause["code"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["ORNA-E-CONTAINED-A", "ORNA-E-CONTAINED-B"]
    );

    let wire = composed.encode_ovb().unwrap();
    let replayed = Diagnostic::decode_ovb(&wire).unwrap();
    let replayed_projection = serde_json::to_value(&replayed).unwrap();
    assert_redacted_tree(&replayed_projection);
    assert_eq!(replayed_projection, projection);
    let json = serde_json::to_vec(&replayed_projection).unwrap();
    for disclosure in fixture_credentials
        .iter()
        .map(|value| value.as_bytes())
        .chain([fixture.as_bytes()])
    {
        for bytes in [&json, &wire] {
            assert!(!bytes
                .windows(disclosure.len())
                .any(|window| window == disclosure));
        }
    }
}
