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
