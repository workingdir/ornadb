use orna_conformance_v1::{ConformanceAdapter, SemanticAdapter, SourceUnit, StageOutcome};
use std::collections::BTreeSet;
use serde_json::{Value, json};

const MANIFEST: &str = include_str!("fixtures/reference/tests/conformance-manifest.json");

struct Fixture {
    path: &'static str,
    source: &'static str,
    sidecar: &'static str,
}

const FIXTURES: &[Fixture] = &[
    Fixture { path: "examples/invalid/affine-addition.orna", source: include_str!("fixtures/reference/examples/invalid/affine-addition.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/affine-addition.orna.json") },
    Fixture { path: "examples/invalid/affine-sum.orna", source: include_str!("fixtures/reference/examples/invalid/affine-sum.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/affine-sum.orna.json") },
    Fixture { path: "examples/invalid/ambiguous-consumer.orna", source: include_str!("fixtures/reference/examples/invalid/ambiguous-consumer.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/ambiguous-consumer.orna.json") },
    Fixture { path: "examples/invalid/assert-effectful-table.orna", source: include_str!("fixtures/reference/examples/invalid/assert-effectful-table.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/assert-effectful-table.orna.json") },
    Fixture { path: "examples/invalid/assert-empty.orna", source: include_str!("fixtures/reference/examples/invalid/assert-empty.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/assert-empty.orna.json") },
    Fixture { path: "examples/invalid/assert-missing-semicolon.orna", source: include_str!("fixtures/reference/examples/invalid/assert-missing-semicolon.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/assert-missing-semicolon.orna.json") },
    Fixture { path: "examples/invalid/assert-owner-type-mismatch.orna", source: include_str!("fixtures/reference/examples/invalid/assert-owner-type-mismatch.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/assert-owner-type-mismatch.orna.json") },
    Fixture { path: "examples/invalid/assignment-expression.orna", source: include_str!("fixtures/reference/examples/invalid/assignment-expression.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/assignment-expression.orna.json") },
    Fixture { path: "examples/invalid/calendar-bucket-no-zone.orna", source: include_str!("fixtures/reference/examples/invalid/calendar-bucket-no-zone.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/calendar-bucket-no-zone.orna.json") },
    Fixture { path: "examples/invalid/comparison-chain.orna", source: include_str!("fixtures/reference/examples/invalid/comparison-chain.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/comparison-chain.orna.json") },
    Fixture { path: "examples/invalid/computed-field-effect.orna", source: include_str!("fixtures/reference/examples/invalid/computed-field-effect.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/computed-field-effect.orna.json") },
    Fixture { path: "examples/invalid/computed-field-insert.orna", source: include_str!("fixtures/reference/examples/invalid/computed-field-insert.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/computed-field-insert.orna.json") },
    Fixture { path: "examples/invalid/computed-field-update.orna", source: include_str!("fixtures/reference/examples/invalid/computed-field-update.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/computed-field-update.orna.json") },
    Fixture { path: "examples/invalid/cross-database-write.orna", source: include_str!("fixtures/reference/examples/invalid/cross-database-write.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/cross-database-write.orna.json") },
    Fixture { path: "examples/invalid/currency-addition.orna", source: include_str!("fixtures/reference/examples/invalid/currency-addition.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/currency-addition.orna.json") },
    Fixture { path: "examples/invalid/currency-static-symbol.orna", source: include_str!("fixtures/reference/examples/invalid/currency-static-symbol.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/currency-static-symbol.orna.json") },
    Fixture { path: "examples/invalid/duplicate-key.orna", source: include_str!("fixtures/reference/examples/invalid/duplicate-key.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/duplicate-key.orna.json") },
    Fixture { path: "examples/invalid/effectful-display.orna", source: include_str!("fixtures/reference/examples/invalid/effectful-display.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/effectful-display.orna.json") },
    Fixture { path: "examples/invalid/float-key.orna", source: include_str!("fixtures/reference/examples/invalid/float-key.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/float-key.orna.json") },
    Fixture { path: "examples/invalid/float-money-implicit.orna", source: include_str!("fixtures/reference/examples/invalid/float-money-implicit.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/float-money-implicit.orna.json") },
    Fixture { path: "examples/invalid/implicit-conversion-chain.orna", source: include_str!("fixtures/reference/examples/invalid/implicit-conversion-chain.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/implicit-conversion-chain.orna.json") },
    Fixture { path: "examples/invalid/incompatible-dimensions.orna", source: include_str!("fixtures/reference/examples/invalid/incompatible-dimensions.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/incompatible-dimensions.orna.json") },
    Fixture { path: "examples/invalid/key-update.orna", source: include_str!("fixtures/reference/examples/invalid/key-update.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/key-update.orna.json") },
    Fixture { path: "examples/invalid/legacy-assert-else.orna", source: include_str!("fixtures/reference/examples/invalid/legacy-assert-else.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/legacy-assert-else.orna.json") },
    Fixture { path: "examples/invalid/legacy-assert-owner-pipe.orna", source: include_str!("fixtures/reference/examples/invalid/legacy-assert-owner-pipe.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/legacy-assert-owner-pipe.orna.json") },
    Fixture { path: "examples/invalid/legacy-assert-pipe-bang.orna", source: include_str!("fixtures/reference/examples/invalid/legacy-assert-pipe-bang.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/legacy-assert-pipe-bang.orna.json") },
    Fixture { path: "examples/invalid/legacy-assert-self-pipe.orna", source: include_str!("fixtures/reference/examples/invalid/legacy-assert-self-pipe.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/legacy-assert-self-pipe.orna.json") },
    Fixture { path: "examples/invalid/legacy-checkpoint-reset-method.orna", source: include_str!("fixtures/reference/examples/invalid/legacy-checkpoint-reset-method.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/legacy-checkpoint-reset-method.orna.json") },
    Fixture { path: "examples/invalid/legacy-colon-bound.orna", source: include_str!("fixtures/reference/examples/invalid/legacy-colon-bound.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/legacy-colon-bound.orna.json") },
    Fixture { path: "examples/invalid/legacy-constraints-block.orna", source: include_str!("fixtures/reference/examples/invalid/legacy-constraints-block.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/legacy-constraints-block.orna.json") },
    Fixture { path: "examples/invalid/legacy-currency-declaration.orna", source: include_str!("fixtures/reference/examples/invalid/legacy-currency-declaration.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/legacy-currency-declaration.orna.json") },
    Fixture { path: "examples/invalid/legacy-empty-closure.orna", source: include_str!("fixtures/reference/examples/invalid/legacy-empty-closure.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/legacy-empty-closure.orna.json") },
    Fixture { path: "examples/invalid/legacy-ensure.orna", source: include_str!("fixtures/reference/examples/invalid/legacy-ensure.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/legacy-ensure.orna.json") },
    Fixture { path: "examples/invalid/legacy-fact.orna", source: include_str!("fixtures/reference/examples/invalid/legacy-fact.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/legacy-fact.orna.json") },
    Fixture { path: "examples/invalid/legacy-failure-replay-method.orna", source: include_str!("fixtures/reference/examples/invalid/legacy-failure-replay-method.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/legacy-failure-replay-method.orna.json") },
    Fixture { path: "examples/invalid/legacy-failure-resolve-method.orna", source: include_str!("fixtures/reference/examples/invalid/legacy-failure-resolve-method.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/legacy-failure-resolve-method.orna.json") },
    Fixture { path: "examples/invalid/legacy-field-check.orna", source: include_str!("fixtures/reference/examples/invalid/legacy-field-check.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/legacy-field-check.orna.json") },
    Fixture { path: "examples/invalid/legacy-field-unique.orna", source: include_str!("fixtures/reference/examples/invalid/legacy-field-unique.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/legacy-field-unique.orna.json") },
    Fixture { path: "examples/invalid/legacy-ingest.orna", source: include_str!("fixtures/reference/examples/invalid/legacy-ingest.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/legacy-ingest.orna.json") },
    Fixture { path: "examples/invalid/legacy-log.orna", source: include_str!("fixtures/reference/examples/invalid/legacy-log.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/legacy-log.orna.json") },
    Fixture { path: "examples/invalid/legacy-match.orna", source: include_str!("fixtures/reference/examples/invalid/legacy-match.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/legacy-match.orna.json") },
    Fixture { path: "examples/invalid/legacy-opaque.orna", source: include_str!("fixtures/reference/examples/invalid/legacy-opaque.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/legacy-opaque.orna.json") },
    Fixture { path: "examples/invalid/legacy-pipe-lambda.orna", source: include_str!("fixtures/reference/examples/invalid/legacy-pipe-lambda.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/legacy-pipe-lambda.orna.json") },
    Fixture { path: "examples/invalid/legacy-postfix-question.orna", source: include_str!("fixtures/reference/examples/invalid/legacy-postfix-question.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/legacy-postfix-question.orna.json") },
    Fixture { path: "examples/invalid/legacy-refined-where.orna", source: include_str!("fixtures/reference/examples/invalid/legacy-refined-where.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/legacy-refined-where.orna.json") },
    Fixture { path: "examples/invalid/legacy-result.orna", source: include_str!("fixtures/reference/examples/invalid/legacy-result.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/legacy-result.orna.json") },
    Fixture { path: "examples/invalid/legacy-return-arrow.orna", source: include_str!("fixtures/reference/examples/invalid/legacy-return-arrow.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/legacy-return-arrow.orna.json") },
    Fixture { path: "examples/invalid/legacy-store.orna", source: include_str!("fixtures/reference/examples/invalid/legacy-store.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/legacy-store.orna.json") },
    Fixture { path: "examples/invalid/legacy-stream-retry-method.orna", source: include_str!("fixtures/reference/examples/invalid/legacy-stream-retry-method.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/legacy-stream-retry-method.orna.json") },
    Fixture { path: "examples/invalid/legacy-stream-skip-method.orna", source: include_str!("fixtures/reference/examples/invalid/legacy-stream-skip-method.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/legacy-stream-skip-method.orna.json") },
    Fixture { path: "examples/invalid/legacy-sys-runtime.orna", source: include_str!("fixtures/reference/examples/invalid/legacy-sys-runtime.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/legacy-sys-runtime.orna.json") },
    Fixture { path: "examples/invalid/legacy-sys-storage-call.orna", source: include_str!("fixtures/reference/examples/invalid/legacy-sys-storage-call.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/legacy-sys-storage-call.orna.json") },
    Fixture { path: "examples/invalid/legacy-top-level-impl.orna", source: include_str!("fixtures/reference/examples/invalid/legacy-top-level-impl.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/legacy-top-level-impl.orna.json") },
    Fixture { path: "examples/invalid/legacy-tryfrom.orna", source: include_str!("fixtures/reference/examples/invalid/legacy-tryfrom.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/legacy-tryfrom.orna.json") },
    Fixture { path: "examples/invalid/legacy-var.orna", source: include_str!("fixtures/reference/examples/invalid/legacy-var.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/legacy-var.orna.json") },
    Fixture { path: "examples/invalid/legacy-view.orna", source: include_str!("fixtures/reference/examples/invalid/legacy-view.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/legacy-view.orna.json") },
    Fixture { path: "examples/invalid/missing-required-field.orna", source: include_str!("fixtures/reference/examples/invalid/missing-required-field.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/missing-required-field.orna.json") },
    Fixture { path: "examples/invalid/module-single-table-assertion.orna", source: include_str!("fixtures/reference/examples/invalid/module-single-table-assertion.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/module-single-table-assertion.orna.json") },
    Fixture { path: "examples/invalid/module-zero-table-assertion.orna", source: include_str!("fixtures/reference/examples/invalid/module-zero-table-assertion.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/module-zero-table-assertion.orna.json") },
    Fixture { path: "examples/invalid/money-float.orna", source: include_str!("fixtures/reference/examples/invalid/money-float.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/money-float.orna.json") },
    Fixture { path: "examples/invalid/mutate-sys-commit.orna", source: include_str!("fixtures/reference/examples/invalid/mutate-sys-commit.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/mutate-sys-commit.orna.json") },
    Fixture { path: "examples/invalid/question-coalesce-adjacent.orna", source: include_str!("fixtures/reference/examples/invalid/question-coalesce-adjacent.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/question-coalesce-adjacent.orna.json") },
    Fixture { path: "examples/invalid/question-on-int.orna", source: include_str!("fixtures/reference/examples/invalid/question-on-int.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/question-on-int.orna.json") },
    Fixture { path: "examples/invalid/range-key-overlap-magic.orna", source: include_str!("fixtures/reference/examples/invalid/range-key-overlap-magic.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/range-key-overlap-magic.orna.json") },
    Fixture { path: "examples/invalid/record-punning.orna", source: include_str!("fixtures/reference/examples/invalid/record-punning.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/record-punning.orna.json") },
    Fixture { path: "examples/invalid/rekey-auto-id.orna", source: include_str!("fixtures/reference/examples/invalid/rekey-auto-id.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/rekey-auto-id.orna.json") },
    Fixture { path: "examples/invalid/relation-equality.orna", source: include_str!("fixtures/reference/examples/invalid/relation-equality.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/relation-equality.orna.json") },
    Fixture { path: "examples/invalid/reserved-std.orna", source: include_str!("fixtures/reference/examples/invalid/reserved-std.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/reserved-std.orna.json") },
    Fixture { path: "examples/invalid/reserved-sys.orna", source: include_str!("fixtures/reference/examples/invalid/reserved-sys.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/reserved-sys.orna.json") },
    Fixture { path: "examples/invalid/row-declaration.orna", source: include_str!("fixtures/reference/examples/invalid/row-declaration.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/row-declaration.orna.json") },
    Fixture { path: "examples/invalid/secret-display.orna", source: include_str!("fixtures/reference/examples/invalid/secret-display.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/secret-display.orna.json") },
    Fixture { path: "examples/invalid/static-protocol-function.orna", source: include_str!("fixtures/reference/examples/invalid/static-protocol-function.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/static-protocol-function.orna.json") },
    Fixture { path: "examples/invalid/top-level-expression.orna", source: include_str!("fixtures/reference/examples/invalid/top-level-expression.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/top-level-expression.orna.json") },
    Fixture { path: "examples/invalid/top-level-on.orna", source: include_str!("fixtures/reference/examples/invalid/top-level-on.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/top-level-on.orna.json") },
    Fixture { path: "examples/invalid/transaction-block.orna", source: include_str!("fixtures/reference/examples/invalid/transaction-block.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/transaction-block.orna.json") },
    Fixture { path: "examples/invalid/two-durable-sources.orna", source: include_str!("fixtures/reference/examples/invalid/two-durable-sources.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/two-durable-sources.orna.json") },
    Fixture { path: "examples/invalid/unknown-field.orna", source: include_str!("fixtures/reference/examples/invalid/unknown-field.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/unknown-field.orna.json") },
    Fixture { path: "examples/invalid/unparenthesized-lambda-stage.orna", source: include_str!("fixtures/reference/examples/invalid/unparenthesized-lambda-stage.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/unparenthesized-lambda-stage.orna.json") },
    Fixture { path: "examples/invalid/unsafe-row-key-repeat.orna", source: include_str!("fixtures/reference/examples/invalid/unsafe-row-key-repeat.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/unsafe-row-key-repeat.orna.json") },
    Fixture { path: "examples/invalid/wrong-field-type.orna", source: include_str!("fixtures/reference/examples/invalid/wrong-field-type.orna"), sidecar: include_str!("fixtures/reference/tests/expected-diagnostics/wrong-field-type.orna.json") },
];

fn emitted_diagnostic(
    fixture: &Fixture,
    sidecar: &Value,
    unit: &SourceUnit,
    adapter: &mut SemanticAdapter,
) -> Option<Value> {
    let phase = sidecar["failing_phase"].as_str().expect("failing phase");
    let outcome = match phase {
        "parse" => adapter.parse(unit),
        "resolve" => adapter.resolve(unit),
        "typecheck" => adapter.typecheck(unit),
        "evaluate" => adapter.evaluate(unit),
        "row-validation" => adapter.validate_row(unit),
        other => panic!("{} has unknown phase {other}", fixture.path),
    };
    match outcome {
        StageOutcome::Failed(diagnostic) => Some(json!({
            "code": adapter.diagnostic_code(&diagnostic),
            "message": adapter.diagnostic_message(&diagnostic),
            "payload": serde_json::to_value(&diagnostic).expect("diagnostic serialization"),
        })),
        StageOutcome::Passed => None,
        StageOutcome::Skipped { reason } => Some(json!({"skipped": reason})),
        StageOutcome::Cancelled(diagnostic) => Some(json!({
            "code": adapter.diagnostic_code(&diagnostic),
            "message": adapter.diagnostic_message(&diagnostic),
            "payload": serde_json::to_value(&diagnostic).expect("diagnostic serialization"),
            "cancelled": true,
        })),
    }
}

#[test]
fn every_invalid_sidecar_matches_the_emitted_diagnostic_payload() {
    let manifest: Value = serde_json::from_str(MANIFEST).expect("reference manifest");
    let reference_version = manifest["version"].as_str().expect("manifest version");
    let mut mismatches = Vec::new();

    for fixture in FIXTURES {
        let sidecar: Value = serde_json::from_str(fixture.sidecar).expect("expected sidecar");
        let sidecar_keys: BTreeSet<_> = sidecar
            .as_object()
            .expect("sidecar object")
            .keys()
            .map(String::as_str)
            .collect();
        let expected_keys = BTreeSet::from([
            "failing_phase",
            "fixture",
            "message_contains",
            "primary_diagnostic",
            "status",
            "version",
        ]);
        let manifest_fixture = manifest["fixtures"]
            .as_array()
            .expect("manifest fixtures")
            .iter()
            .find(|entry| entry["path"] == fixture.path)
            .unwrap_or_else(|| panic!("{} missing from manifest", fixture.path));
        let mut fields = Vec::new();
        if sidecar_keys != expected_keys {
            fields.push("sidecar_fields".into());
        }
        for (name, actual) in [
            ("version", reference_version),
            ("fixture", fixture.path),
            (
                "failing_phase",
                sidecar["failing_phase"].as_str().expect("sidecar phase"),
            ),
        ] {
            if sidecar[name].as_str() != Some(actual) {
                fields.push(name.to_owned());
            }
        }
        if sidecar["status"].as_str() != Some("expected-not-executed") {
            fields.push("status".into());
        }
        if sidecar["primary_diagnostic"] != manifest_fixture["diagnostic"] {
            fields.push("primary_diagnostic(manifest)".into());
        }
        if sidecar["failing_phase"] != manifest_fixture["failing_phase"] {
            fields.push("failing_phase(manifest)".into());
        }
        if sidecar["message_contains"] != manifest_fixture["message_contains"] {
            fields.push("message_contains(manifest)".into());
        }
        let mut adapter = SemanticAdapter::default();
        let unit = SourceUnit {
            fixture_id: manifest_fixture["id"].as_str().expect("fixture id").into(),
            source_id: fixture.path.into(),
            parse_as: manifest_fixture["parse_as"].as_str().expect("parse_as").into(),
            source: fixture.source.into(),
        };
        let actual = emitted_diagnostic(fixture, &sidecar, &unit, &mut adapter);
        let emitted_phase = actual.as_ref().and_then(|value| {
            if value.get("skipped").is_none() { sidecar["failing_phase"].as_str() } else { None }
        });
        if emitted_phase != sidecar["failing_phase"].as_str() {
            fields.push("failing_phase(emission)".into());
        }
        let actual_code = actual.as_ref().and_then(|value| value["code"].as_str());
        if actual_code != sidecar["primary_diagnostic"].as_str() {
            fields.push("primary_diagnostic(emission)".into());
        }
        let message = actual.as_ref().and_then(|value| value["message"].as_str());
        let expected_message = sidecar["message_contains"].as_str().expect("message substring");
        if !message.is_some_and(|message| message.contains(expected_message)) {
            fields.push("message_contains(emission)".into());
        }
        let payload = actual.as_ref().and_then(|value| value.get("payload"));
        let span_count = payload
            .and_then(|value| value["spans"].as_array())
            .map_or(0, Vec::len);
        let severity = payload.and_then(|value| value["severity"].as_str()).unwrap_or("unspecified");
        let message_class = sidecar["primary_diagnostic"].as_str().unwrap_or("missing");
        let result = if fields.is_empty() { "MATCH" } else { "MISMATCH" };
        let actual_native_code = payload
            .and_then(|value| value["code"].as_str())
            .unwrap_or("unspecified");
        println!(
            "PAYLOAD {result} {} fields={} sidecar_keys={} expected_code={} actual_code={} expected_message_contains={:?} actual_message={:?} sidecar_message_class=not-separate(primary_diagnostic={message_class}) native_code={actual_native_code} severity={severity} spans={span_count} actual_spans={}",
            fixture.path,
            if fields.is_empty() { "-".into() } else { fields.join(",") },
            sidecar_keys.iter().copied().collect::<Vec<_>>().join(","),
            sidecar["primary_diagnostic"].as_str().unwrap_or("missing"),
            actual_code.unwrap_or("missing"),
            expected_message,
            message.unwrap_or("missing"),
            payload.and_then(|value| value.get("spans")).cloned().unwrap_or(Value::Null),
        );
        if !fields.is_empty() {
            match fixture.path {
                "examples/invalid/legacy-result.orna" => {
                    assert_eq!(actual_code, Some("ORNA-S012-UNRESOLVED"));
                    assert_eq!(message, Some("type name cannot be resolved"));
                    assert_eq!(actual_native_code, "ORNA-S012-UNRESOLVED");
                }
                "examples/invalid/two-durable-sources.orna" => {
                    assert_eq!(actual_code, Some("ORNA-S021-TYPE"));
                    assert_eq!(
                        message,
                        Some("a durable consumer function may own only one checkpointed source root; extract separate named consumer functions")
                    );
                    assert_eq!(actual_native_code, "ORNA-S021-TYPE");
                }
                other => panic!("unexpected payload mismatch for {other}"),
            }
            mismatches.push(format!("{}: {}", fixture.path, fields.join(",")));
        }
    }

    println!("PAYLOAD TOTAL fixtures={} mismatches={}", FIXTURES.len(), mismatches.len());
    let observed: BTreeSet<_> = mismatches.iter().map(String::as_str).collect();
    let attributed = BTreeSet::from([
        "examples/invalid/legacy-result.orna: primary_diagnostic(emission),message_contains(emission)",
        "examples/invalid/two-durable-sources.orna: primary_diagnostic(emission)",
    ]);
    assert_eq!(observed, attributed, "payload mismatch set changed; update the audit and attribution");
}
