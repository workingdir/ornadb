//! Executes the published vector suites against the production codecs that
//! implement them.
//!
//! [`Corpus::validate`](crate::Corpus::validate) proves only that every suite
//! named in [`VECTOR_FILES`](crate::VECTOR_FILES) is present. This module runs
//! each executable case through its production entry point and records one
//! result per suite. A suite that production code cannot yet produce is
//! recorded as a known gap naming its tracking bead, so a missing
//! implementation is never reported as a pass and never skipped silently.

use orna_protocol_v1::{Envelope, Limits};
use orna_value_v1::{
    Snapshot, Value as OvbValue, path_collision_key, path_decode_component,
    path_decode_key_components, path_encode_component, path_encode_key_components,
};
use serde::Serialize;
use serde_json::Value;

use crate::{Corpus, EvidenceStatus};

/// The result of running one published vector suite.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VectorSuiteEvidence {
    /// Published file name under `tests/`.
    pub suite: String,
    /// `Passed` or `Failed` once the suite ran through production code, and
    /// `Skipped` only for a recorded known gap.
    pub status: EvidenceStatus,
    /// The production entry point the cases ran through or, for a known gap,
    /// the behaviour production code cannot yet provide.
    pub detail: String,
    /// Cases executed through the production implementation.
    pub executed_cases: usize,
    /// One entry per failing case: the case and the disagreement observed.
    pub failures: Vec<String>,
    /// The bead tracking a suite that production code cannot yet produce.
    pub known_gap: Option<String>,
}

/// Runs every vector suite the corpus loaded, in published file-name order.
/// A suite with neither a production executor nor a recorded known gap fails.
pub fn run_vector_suites(corpus: &Corpus) -> Vec<VectorSuiteEvidence> {
    corpus
        .vectors
        .iter()
        .map(|(suite, vectors)| run_vector_suite(suite, vectors))
        .collect()
}

fn run_vector_suite(suite: &str, vectors: &Value) -> VectorSuiteEvidence {
    match suite {
        "value-vectors.json" => executed(
            suite,
            "orna-value-v1 strict OVB-1 decoder and canonical encoder (Value::decode, Value::encode)"
                .into(),
            value_vectors(vectors),
        ),
        "snapshot-vectors.json" => executed(
            suite,
            "orna-value-v1 snapshot pin codec (Snapshot::decode_bytes, Snapshot::encode)".into(),
            snapshot_vectors(vectors),
        ),
        "protocol-vectors.json" => executed(
            suite,
            "orna-protocol-v1 live envelope codec (Envelope::decode, Envelope::encode)".into(),
            protocol_vectors(vectors),
        ),
        "path-vectors.json" => executed(
            suite,
            format!(
                "orna-value-v1 legacy row-key path reader (ORNA-PATH-001..012 are retired in \
                 1.1.0; the reader remains for legacy input); the {} materialisation \
                 statements are prose inspection prompts, not executable cases",
                vectors["materialisation"].as_array().map_or(0, Vec::len)
            ),
            path_vectors(vectors),
        ),
        "float-vectors.json" => known_gap(
            suite,
            "ornadb-5il1.188",
            "Float arithmetic is not IEEE-754 binary64: NaN and infinities cannot be produced \
             from source, so the total-order, equality and aggregate cases cannot run through \
             the language (W01-01)",
        ),
        "numeric-vectors.json" => known_gap(
            suite,
            "ornadb-5il1.190",
            "exact Money and unit arithmetic does not evaluate at runtime: `0.1.GBP + 0.2.GBP` \
             and `1.decimal.mph` fail with ORNA-EVAL-TYPE (A06-06)",
        ),
        _ => VectorSuiteEvidence {
            suite: suite.to_owned(),
            status: EvidenceStatus::Failed,
            detail: "no production executor or known-gap record exists for this suite".into(),
            executed_cases: 0,
            failures: Vec::new(),
            known_gap: None,
        },
    }
}

fn executed(suite: &str, detail: String, run: CaseRun) -> VectorSuiteEvidence {
    VectorSuiteEvidence {
        suite: suite.to_owned(),
        status: if run.failures.is_empty() && run.executed > 0 {
            EvidenceStatus::Passed
        } else {
            EvidenceStatus::Failed
        },
        detail,
        executed_cases: run.executed,
        failures: run.failures,
        known_gap: None,
    }
}

fn known_gap(suite: &str, bead: &str, reason: &str) -> VectorSuiteEvidence {
    VectorSuiteEvidence {
        suite: suite.to_owned(),
        status: EvidenceStatus::Skipped,
        detail: reason.to_owned(),
        executed_cases: 0,
        failures: Vec::new(),
        known_gap: Some(bead.to_owned()),
    }
}

#[derive(Default)]
struct CaseRun {
    executed: usize,
    failures: Vec<String>,
}

impl CaseRun {
    fn check(&mut self, case: &str, passed: bool, disagreement: impl FnOnce() -> String) {
        self.executed += 1;
        if !passed {
            self.failures.push(format!("{case}: {}", disagreement()));
        }
    }

    /// A published case this runner cannot read is a failure, not a skip.
    fn malformed(&mut self, case: &str) {
        self.failures
            .push(format!("{case}: the published case is malformed"));
    }
}

/// Decodes lowercase hexadecimal, the only spelling the vector files use.
fn hex_bytes(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0
        || !text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).ok())
        .collect()
}

fn strings(value: &Value) -> Option<Vec<String>> {
    value
        .as_array()?
        .iter()
        .map(|item| item.as_str().map(str::to_owned))
        .collect()
}

/// `value-vectors.json`: each case's bytes either survive the strict decoder
/// and canonical re-encoding unchanged or not, exactly as published.
fn value_vectors(vectors: &Value) -> CaseRun {
    let mut run = CaseRun::default();
    let Some(cases) = vectors.as_array() else {
        run.malformed("value-vectors.json");
        return run;
    };
    for (index, case) in cases.iter().enumerate() {
        let name = format!("[{index}] {}", case["name"].as_str().unwrap_or("<unnamed>"));
        let (Some(bytes), Some(expected)) = (
            case["hex"].as_str().and_then(hex_bytes),
            case["canonical_roundtrip"].as_bool(),
        ) else {
            run.malformed(&name);
            continue;
        };
        let round_trip = OvbValue::decode(&bytes)
            .and_then(|value| value.encode())
            .is_ok_and(|encoded| encoded == bytes);
        run.check(&name, round_trip == expected, || {
            format!("the canonical round trip is {round_trip}, published {expected}")
        });
    }
    run
}

/// `snapshot-vectors.json`: published pins decode and re-encode to the same
/// bytes, and every `invalid_` case is refused by the snapshot decoder.
fn snapshot_vectors(vectors: &Value) -> CaseRun {
    let mut run = CaseRun::default();
    let Some(cases) = vectors.as_object() else {
        run.malformed("snapshot-vectors.json");
        return run;
    };
    for (name, hex) in cases {
        let Some(bytes) = hex.as_str().and_then(hex_bytes) else {
            run.malformed(name);
            continue;
        };
        let decoded = Snapshot::decode_bytes(&bytes);
        if name.starts_with("invalid_") {
            run.check(name, decoded.is_err(), || {
                "the snapshot decoder accepted a published rejection".into()
            });
        } else {
            let encoded = decoded.and_then(|snapshot| snapshot.encode());
            run.check(name, encoded.as_ref() == Ok(&bytes), || {
                format!("the snapshot round trip produced {encoded:?}")
            });
        }
    }
    run
}

/// `protocol-vectors.json`: each live envelope decodes and re-encodes to its
/// exact published bytes.
fn protocol_vectors(vectors: &Value) -> CaseRun {
    let mut run = CaseRun::default();
    let Some(cases) = vectors.as_array() else {
        run.malformed("protocol-vectors.json");
        return run;
    };
    for (index, case) in cases.iter().enumerate() {
        let name = format!("[{index}] {}", case["name"].as_str().unwrap_or("<unnamed>"));
        let Some(bytes) = case["hex"].as_str().and_then(hex_bytes) else {
            run.malformed(&name);
            continue;
        };
        let encoded = Envelope::decode(&bytes, Limits::default())
            .and_then(|envelope| envelope.encode(Limits::default()));
        let passed = encoded.as_ref().is_ok_and(|encoded| *encoded == bytes);
        run.check(&name, passed, || match encoded {
            Ok(_) => "the re-encoded envelope differs from the published bytes".into(),
            Err(error) => format!("the envelope codec failed: {error}"),
        });
    }
    run
}

/// `path-vectors.json`: forward and reverse spellings, composite keys,
/// rejected spellings, portable collision keys and the component and
/// table-relative path limits of the legacy row-key path reader.
fn path_vectors(vectors: &Value) -> CaseRun {
    let mut run = CaseRun::default();
    let sections = [
        "round_trip",
        "composite",
        "reject_encoded",
        "portable_collisions",
    ];
    for section in sections {
        let Some(cases) = vectors[section].as_array() else {
            run.malformed(section);
            continue;
        };
        for (index, case) in cases.iter().enumerate() {
            let name = format!("{section}[{index}]");
            match section {
                "round_trip" => {
                    let (Some(value), Some(encoded)) =
                        (case["value"].as_str(), case["encoded"].as_str())
                    else {
                        run.malformed(&name);
                        continue;
                    };
                    let passed = path_encode_component(value).is_ok_and(|text| text == encoded)
                        && path_decode_component(encoded).is_ok_and(|text| text == value)
                        && path_decode_key_components(&[format!("{encoded}.orna")])
                            .is_ok_and(|values| values == [value]);
                    run.check(&name, passed, || {
                        format!("{value:?} does not round trip through {encoded:?}")
                    });
                }
                "composite" => {
                    let (Some(values), Some(components)) =
                        (strings(&case["values"]), strings(&case["components"]))
                    else {
                        run.malformed(&name);
                        continue;
                    };
                    let passed = path_encode_key_components(&values)
                        .is_ok_and(|encoded| encoded == components)
                        && path_decode_key_components(&components)
                            .is_ok_and(|decoded| decoded == values);
                    run.check(&name, passed, || {
                        format!("{values:?} does not round trip through {components:?}")
                    });
                }
                "reject_encoded" => {
                    let Some(encoded) = case["encoded"].as_str() else {
                        run.malformed(&name);
                        continue;
                    };
                    let passed = path_decode_component(encoded).is_err()
                        && path_decode_key_components(&[format!("{encoded}.orna")]).is_err();
                    run.check(&name, passed, || format!("{encoded:?} was accepted"));
                }
                _ => {
                    let (Some(left), Some(right), Some(collision_key)) = (
                        case["left"].as_str(),
                        case["right"].as_str(),
                        case["collision_key"].as_str(),
                    ) else {
                        run.malformed(&name);
                        continue;
                    };
                    let keys = path_encode_component(left).and_then(|left| {
                        path_encode_component(right)
                            .map(|right| (path_collision_key(&left), path_collision_key(&right)))
                    });
                    let passed = keys.as_ref().is_ok_and(|(left, right)| {
                        left == collision_key && right == collision_key
                    });
                    run.check(&name, passed, || {
                        format!("{left:?} and {right:?} do not share collision key {collision_key:?}")
                    });
                }
            }
        }
    }
    for limit in ["component", "table_relative_path"] {
        let Some(cases) = vectors["limits"][limit].as_array() else {
            run.malformed(&format!("limits.{limit}"));
            continue;
        };
        for (index, case) in cases.iter().enumerate() {
            let name = format!("limits.{limit}[{index}]");
            let (Some(length), Some(expect)) = (
                case["encoded_length"]
                    .as_u64()
                    .and_then(|length| usize::try_from(length).ok()),
                case["expect"].as_str(),
            ) else {
                run.malformed(&name);
                continue;
            };
            let accepted = if limit == "component" {
                path_encode_component(&"a".repeat(length)).is_ok()
            } else {
                // A table-relative row path is its key components joined by
                // `/` plus the `.orna` document suffix.
                let Some(mut remaining) = length.checked_sub(".orna".len()) else {
                    run.malformed(&name);
                    continue;
                };
                let mut components = Vec::new();
                while remaining > 200 {
                    components.push("a".repeat(200));
                    remaining -= 201;
                }
                components.push("a".repeat(remaining));
                path_encode_key_components(&components).is_ok()
            };
            run.check(&name, accepted == (expect == "pass"), || {
                format!("an encoded length of {length} was accepted: {accepted}, published {expect}")
            });
        }
    }
    run
}
