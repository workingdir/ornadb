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
