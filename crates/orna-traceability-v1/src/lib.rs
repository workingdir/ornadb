//! Bounded, logical traceability for the published Orna 1.0.0 reference bundle.
//! The report intentionally contains identifiers and statuses, never source bodies or host paths.

use orna_conformance_v1::{EngineWitnesses, Stage};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path},
};

const VERSION: &str = "1.0.0";
const NORMATIVE_PAYLOAD_COUNT: usize = 46;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceError(String);
impl std::fmt::Display for TraceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
impl std::error::Error for TraceError {}
type Result<T> = std::result::Result<T, TraceError>;
fn err(message: impl Into<String>) -> TraceError {
    TraceError(message.into())
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Report {
    pub format: String,
    pub specification_version: String,
    pub publication_digests: BTreeMap<String, String>,
    pub normative_payloads: Vec<NormativePayloadTrace>,
    pub requirements: Vec<RequirementTrace>,
    pub named_algorithms: Vec<InventoryEntry>,
    pub schema_profile_members: Vec<InventoryEntry>,
    pub fixture_classes: Vec<FixtureClass>,
    pub behavioral_scenarios: Vec<ScenarioTrace>,
}
/// A release-payload disposition. This inventory does not establish execution;
/// missing implementation and test references remain explicit coverage gaps.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct NormativePayloadTrace {
    pub logical_id: String,
    pub requirement_ids: Vec<String>,
    pub implementation_refs: Vec<String>,
    pub test_refs: Vec<String>,
    pub status: Status,
}
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct RequirementTrace {
    pub requirement_id: String,
    pub chapter: String,
    pub status: Status,
    pub boundaries: Vec<Boundary>,
}
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Boundary {
    pub kind: String,
    pub logical_id: String,
    pub implementation_ref: Option<String>,
    pub test_ref: Option<String>,
    pub status: Status,
}
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct InventoryEntry {
    pub logical_id: String,
    pub status: Status,
}
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct FixtureClass {
    pub kind: String,
    pub count: usize,
    pub status: Status,
}
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ScenarioTrace {
    pub scenario_id: String,
    pub requirements: Vec<String>,
    pub status: Status,
}
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    Executed,
    PartiallyExecuted,
    SpecifiedModelOnly,
    JustifiedGap,
}

#[derive(Deserialize)]
struct Release {
    version: String,
    normative_payload_sha256: BTreeMap<String, String>,
}
#[derive(Deserialize)]
struct Requirement {
    id: String,
    chapter: String,
    source: String,
}
#[derive(Deserialize)]
struct EvidenceFile {
    requirements: Vec<Evidence>,
}
#[derive(Deserialize)]
struct Evidence {
    requirement: String,
    implementation_result: String,
    tests: Vec<Test>,
}
#[derive(Deserialize)]
struct Test {
    kind: String,
    status: String,
    #[serde(default)]
    fixture: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    subject: Option<String>,
}
#[derive(Deserialize)]
struct Manifest {
    version: String,
    counts: Counts,
    fixtures: Vec<Fixture>,
}
#[derive(Deserialize)]
struct Counts {
    total: usize,
}
#[derive(Deserialize)]
struct Fixture {
    id: String,
    kind: String,
    path: String,
}
#[derive(Deserialize)]
struct InvalidMetadata {
    version: String,
    count: usize,
    fixtures: Vec<InvalidFixture>,
}
#[derive(Deserialize)]
struct InvalidFixture {
    path: String,
    failing_phase: String,
    diagnostic: String,
    message_contains: String,
}
#[allow(clippy::struct_field_names)]
#[derive(Deserialize)]
struct Scenarios {
    version: String,
    count: usize,
    scenarios: Vec<Scenario>,
}
#[derive(Deserialize)]
struct Scenario {
    id: String,
    requirements: Vec<String>,
    evidence_level: String,
}
#[derive(Deserialize)]
struct Models {
    results: Vec<Model>,
}
#[derive(Deserialize)]
struct Model {
    name: String,
}

/// Generate the frozen traceability report without implementation execution
/// witnesses.
///
/// # Errors
///
/// Returns an error when the reference bundle or its publication digests are
/// invalid.
pub fn generate(root: impl AsRef<Path>) -> Result<Report> {
    generate_inner(root.as_ref(), None)
}

/// Generate the frozen report with an execution register produced by the
/// conformance harness. The register is accepted only when it carries the
/// exact publication digest inventory loaded from the same reference root.
///
/// # Errors
///
/// Returns an error when the reference bundle, its digests, or the witness
/// register is invalid.
pub fn generate_with_engine_witnesses(
    root: impl AsRef<Path>,
    witnesses: &EngineWitnesses,
) -> Result<Report> {
    generate_inner(root.as_ref(), Some(witnesses))
}

fn generate_inner(root: &Path, engine_witnesses: Option<&EngineWitnesses>) -> Result<Report> {
    let release: Release = read_json(root, "release.json")?;
    if release.version != VERSION
        || release.normative_payload_sha256.len() != NORMATIVE_PAYLOAD_COUNT
    {
        return Err(err("release version or digest inventory is invalid"));
    }
    verify_digests(root, &release.normative_payload_sha256)?;
    let requirements: Vec<Requirement> = read_json(root, "tests/requirements.json")?;
    let evidence: EvidenceFile = read_json(root, "tests/requirement-evidence.json")?;
    let manifest: Manifest = read_json(root, "tests/conformance-manifest.json")?;
    let invalid: InvalidMetadata = read_json(root, "tests/invalid-metadata.json")?;
    let scenarios: Scenarios = read_json(root, "tests/scenarios.json")?;
    let models: Models = read_json(root, "evidence/contract-models.json")?;
    validate(
        &requirements,
        &evidence,
        &manifest,
        &invalid,
        &scenarios,
        &models,
    )?;
    let evidence_by_id = evidence
        .requirements
        .iter()
        .map(|item| (item.requirement.as_str(), item))
        .collect::<BTreeMap<_, _>>();
    let requirement_traces = requirements
        .iter()
        .map(|requirement| {
            let entry = evidence_by_id[requirement.id.as_str()];
            let boundaries = entry
                .tests
                .iter()
                .map(|test| Boundary {
                    kind: test.kind.clone(),
                    logical_id: logical_test_id(test, &requirement.id),
                    implementation_ref: None,
                    test_ref: None,
                    status: test_status(test, &entry.implementation_result),
                })
                .collect::<Vec<_>>();
            RequirementTrace {
                requirement_id: requirement.id.clone(),
                chapter: requirement.chapter.clone(),
                status: aggregate(&boundaries.iter().map(|b| b.status).collect::<Vec<_>>()),
                boundaries,
            }
        })
        .collect();
    let mut report = Report {
        format: "orna.traceability.v1".into(),
        specification_version: VERSION.into(),
        normative_payloads: normative_payloads(&release.normative_payload_sha256, &requirements)?,
        publication_digests: release.normative_payload_sha256,
        requirements: requirement_traces,
        named_algorithms: models
            .results
            .into_iter()
            .map(|model| InventoryEntry {
                logical_id: model.name,
                status: Status::SpecifiedModelOnly,
            })
            .collect(),
        schema_profile_members: schema_profile_members(root)?,
        fixture_classes: fixture_classes(&manifest),
        behavioral_scenarios: scenarios
            .scenarios
            .into_iter()
            .map(|scenario| {
                let status = if scenario
                    .requirements
                    .iter()
                    .any(|id| !evidence_by_id.contains_key(id.as_str()))
                {
                    Status::JustifiedGap
                } else {
                    status_from_level(&scenario.evidence_level)
                };
                ScenarioTrace {
                    scenario_id: scenario.id,
                    requirements: scenario.requirements,
                    status,
                }
            })
            .collect(),
    };
    if let Some(witnesses) = engine_witnesses {
        apply_engine_witnesses(&mut report, witnesses, &manifest)?;
    }
    Ok(report)
}

fn apply_engine_witnesses(
    report: &mut Report,
    witnesses: &EngineWitnesses,
    manifest: &Manifest,
) -> Result<()> {
    if report.publication_digests != witnesses.publication_digests {
        return Err(err(
            "engine witness publication digests do not match report",
        ));
    }
    let fixtures = manifest
        .fixtures
        .iter()
        .map(|fixture| (fixture.id.as_str(), fixture.path.as_str()))
        .collect::<BTreeMap<_, _>>();
    let mut requirements = report
        .requirements
        .iter_mut()
        .map(|requirement| (requirement.requirement_id.clone(), requirement))
        .collect::<BTreeMap<_, _>>();
    let mut seen = BTreeSet::new();
    for witness in &witnesses.witnesses {
        let expected_path = fixtures.get(witness.fixture_id.as_str()).ok_or_else(|| {
            err(format!(
                "engine witness names unknown fixture: {}",
                witness.fixture_id
            ))
        })?;
        if *expected_path != witness.fixture_path {
            return Err(err(format!(
                "engine witness fixture path mismatch: {}",
                witness.fixture_id
            )));
        }
        if witness.implementation_ref.is_empty() || witness.test_ref.is_empty() {
            return Err(err("engine witness lacks implementation or test reference"));
        }
        if matches!(
            witness.observed_status,
            orna_conformance_v1::EvidenceStatus::Skipped
                | orna_conformance_v1::EvidenceStatus::Specified
        ) {
            return Err(err("engine witness has non-executed status"));
        }
        let stage = stage_name(&witness.stage);
        let key = (
            witness.requirement_id.as_str(),
            witness.fixture_id.as_str(),
            stage,
        );
        if !seen.insert(key) {
            return Err(err("duplicate engine witness binding"));
        }
        let requirement = requirements
            .get_mut(&witness.requirement_id)
            .ok_or_else(|| {
                err(format!(
                    "engine witness names unknown requirement: {}",
                    witness.requirement_id
                ))
            })?;
        requirement.boundaries.push(Boundary {
            kind: "engine-witness".into(),
            logical_id: format!("{}:{stage}", witness.fixture_id),
            implementation_ref: Some(witness.implementation_ref.clone()),
            test_ref: Some(witness.test_ref.clone()),
            status: Status::Executed,
        });
        requirement.status = aggregate(
            &requirement
                .boundaries
                .iter()
                .map(|boundary| boundary.status)
                .collect::<Vec<_>>(),
        );
    }
    Ok(())
}

fn stage_name(stage: &Stage) -> &'static str {
    match stage {
        Stage::Parse => "parse",
        Stage::Resolve => "resolve",
        Stage::Typecheck => "typecheck",
        Stage::Evaluate => "evaluate",
        Stage::RowValidation => "row-validation",
    }
}

fn validate(
    requirements: &[Requirement],
    evidence: &EvidenceFile,
    manifest: &Manifest,
    invalid: &InvalidMetadata,
    scenarios: &Scenarios,
    models: &Models,
) -> Result<()> {
    if requirements.len() != 870 || !unique(requirements.iter().map(|r| r.id.as_str())) {
        return Err(err(
            "requirements must contain exactly 870 unique identifiers",
        ));
    }
    if requirements
        .iter()
        .any(|r| r.chapter.is_empty() || r.source.is_empty())
    {
        return Err(err(
            "requirements contain an empty chapter or source identifier",
        ));
    }
    if evidence.requirements.len() != 870
        || !unique(evidence.requirements.iter().map(|e| e.requirement.as_str()))
    {
        return Err(err(
            "requirement evidence must contain exactly 870 unique identifiers",
        ));
    }
    let ids = requirements
        .iter()
        .map(|r| r.id.as_str())
        .collect::<BTreeSet<_>>();
    for item in &evidence.requirements {
        if !ids.contains(item.requirement.as_str()) || item.tests.is_empty() {
            return Err(err(
                "requirement evidence has a broken link or empty test list",
            ));
        }
        for test in &item.tests {
            if test.kind.is_empty()
                || test.status.is_empty()
                || test
                    .fixture
                    .as_deref()
                    .or(test.path.as_deref())
                    .or(test.subject.as_deref())
                    .is_none()
            {
                return Err(err(
                    "requirement evidence contains an incomplete test entry",
                ));
            }
        }
    }
    if manifest.version != VERSION
        || manifest.counts.total != 167
        || manifest.fixtures.len() != 167
        || !unique(manifest.fixtures.iter().map(|f| f.id.as_str()))
    {
        return Err(err(
            "conformance manifest count or fixture identifiers are invalid",
        ));
    }
    let fixture_paths = manifest
        .fixtures
        .iter()
        .map(|f| f.path.as_str())
        .collect::<BTreeSet<_>>();
    if invalid.version != VERSION || invalid.count != 80 || invalid.fixtures.len() != 80 {
        return Err(err("invalid fixture metadata count is invalid"));
    }
    for item in &invalid.fixtures {
        if !fixture_paths.contains(item.path.as_str())
            || item.failing_phase.is_empty()
            || item.diagnostic.is_empty()
            || item.message_contains.is_empty()
        {
            return Err(err(
                "invalid fixture metadata has a broken link or empty field",
            ));
        }
    }
    if scenarios.version != VERSION
        || scenarios.count != 144
        || scenarios.scenarios.len() != 144
        || !unique(scenarios.scenarios.iter().map(|s| s.id.as_str()))
    {
        return Err(err("scenario count or identifiers are invalid"));
    }
    for scenario in &scenarios.scenarios {
        if scenario.requirements.is_empty() || scenario.evidence_level.is_empty() {
            return Err(err(
                "scenario has an empty requirement link or evidence field",
            ));
        }
    }
    if models.results.is_empty() || !unique(models.results.iter().map(|m| m.name.as_str())) {
        return Err(err("named algorithm inventory is invalid"));
    }
    Ok(())
}
fn normative_payloads(
    digests: &BTreeMap<String, String>,
    requirements: &[Requirement],
) -> Result<Vec<NormativePayloadTrace>> {
    let requirements_by_source = requirements.iter().fold(
        BTreeMap::<&str, Vec<&Requirement>>::new(),
        |mut grouped, requirement| {
            grouped
                .entry(requirement.source.as_str())
                .or_default()
                .push(requirement);
            grouped
        },
    );
    let mut payloads = Vec::with_capacity(digests.len());
    for logical_id in digests.keys() {
        let linked = requirements_by_source
            .get(logical_id.as_str())
            .cloned()
            .unwrap_or_default();
        let requirement_ids = linked
            .iter()
            .map(|requirement| requirement.id.clone())
            .collect::<Vec<_>>();
        let payload = NormativePayloadTrace {
            logical_id: logical_id.clone(),
            requirement_ids,
            implementation_refs: Vec::new(),
            test_refs: Vec::new(),
            status: Status::JustifiedGap,
        };
        validate_payload(&payload)?;
        payloads.push(payload);
    }
    if payloads.len() != NORMATIVE_PAYLOAD_COUNT {
        return Err(err("normative payload report is incomplete"));
    }
    Ok(payloads)
}
fn validate_payload(payload: &NormativePayloadTrace) -> Result<()> {
    if payload.logical_id.is_empty() {
        return Err(err("normative payload lacks a logical identifier"));
    }
    Ok(())
}
fn schema_profile_members(root: &Path) -> Result<Vec<InventoryEntry>> {
    let sys: Value = read_json(root, "api/sys.json")?;
    let live: Value = read_json(root, "profiles/live-messages.json")?;
    let session: Value = read_json(root, "profiles/session.schema.json")?;
    let mut ids = BTreeSet::new();
    for key in [
        "functions",
        "relations",
        "value_types",
        "enums",
        "singletons",
        "opaque_identifiers",
    ] {
        named_object_members(&sys, key, "sys", &mut ids)?;
    }
    for key in ["messages", "definitions"] {
        named_object_members(&live, key, "live", &mut ids)?;
    }
    named_object_members(&session, "$defs", "session", &mut ids)?;
    if ids.is_empty() {
        return Err(err("schema and profile member inventory is empty"));
    }
    Ok(ids
        .into_iter()
        .map(|logical_id| InventoryEntry {
            logical_id,
            status: Status::SpecifiedModelOnly,
        })
        .collect())
}
fn named_object_members(
    value: &Value,
    key: &str,
    prefix: &str,
    output: &mut BTreeSet<String>,
) -> Result<()> {
    let group = value.get(key).ok_or_else(|| {
        err(format!(
            "schema/profile member group is missing: {prefix}.{key}"
        ))
    })?;
    match group {
        Value::Object(object) if !object.is_empty() => {
            output.extend(object.keys().map(|name| format!("{prefix}.{key}.{name}")));
        }
        Value::Array(items) if !items.is_empty() => {
            for item in items {
                let name = item
                    .get("name")
                    .and_then(Value::as_str)
                    .or_else(|| item.as_str())
                    .filter(|name| !name.is_empty())
                    .ok_or_else(|| {
                        err(format!(
                            "schema/profile member lacks a name: {prefix}.{key}"
                        ))
                    })?;
                output.insert(format!("{prefix}.{key}.{name}"));
            }
        }
        _ => {
            return Err(err(format!(
                "schema/profile member group is empty: {prefix}.{key}"
            )));
        }
    }
    Ok(())
}
fn fixture_classes(manifest: &Manifest) -> Vec<FixtureClass> {
    let mut counts = BTreeMap::new();
    for fixture in &manifest.fixtures {
        *counts.entry(fixture.kind.clone()).or_insert(0) += 1;
    }
    counts
        .into_iter()
        .map(|(kind, count)| FixtureClass {
            kind,
            count,
            status: Status::JustifiedGap,
        })
        .collect()
}
fn test_status(test: &Test, implementation_result: &str) -> Status {
    let value = format!("{} {}", test.status, implementation_result).to_ascii_lowercase();
    if value.contains("model") || value.contains("specified") {
        Status::SpecifiedModelOnly
    } else {
        Status::JustifiedGap
    }
}
fn status_from_level(level: &str) -> Status {
    if level.to_ascii_lowercase().contains("not executed")
        || level.to_ascii_lowercase().contains("planned")
    {
        Status::JustifiedGap
    } else {
        Status::SpecifiedModelOnly
    }
}
fn aggregate(statuses: &[Status]) -> Status {
    if statuses.is_empty() {
        Status::JustifiedGap
    } else if statuses.iter().all(|status| *status == Status::Executed) {
        Status::Executed
    } else if statuses
        .iter()
        .all(|status| *status == Status::SpecifiedModelOnly)
    {
        Status::SpecifiedModelOnly
    } else if statuses
        .iter()
        .all(|status| *status == Status::JustifiedGap)
    {
        Status::JustifiedGap
    } else {
        Status::PartiallyExecuted
    }
}
fn logical_test_id(test: &Test, fallback: &str) -> String {
    test.fixture
        .clone()
        .or_else(|| test.path.clone())
        .or_else(|| test.subject.clone())
        .unwrap_or_else(|| fallback.into())
}
fn unique<'a>(mut items: impl Iterator<Item = &'a str>) -> bool {
    let mut set = BTreeSet::new();
    items.all(|item| !item.is_empty() && set.insert(item))
}
fn read_json<T: for<'de> Deserialize<'de>>(root: &Path, logical: &str) -> Result<T> {
    safe_logical(logical)?;
    let body = fs::read_to_string(root.join(logical))
        .map_err(|_| err(format!("cannot read required logical member: {logical}")))?;
    serde_json::from_str(&body)
        .map_err(|_| err(format!("invalid JSON in logical member: {logical}")))
}
fn safe_logical(logical: &str) -> Result<()> {
    let path = Path::new(logical);
    if path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        Err(err("unsafe logical member identifier"))
    } else {
        Ok(())
    }
}
fn verify_digests(root: &Path, digests: &BTreeMap<String, String>) -> Result<()> {
    for (logical, expected) in digests {
        safe_logical(logical)?;
        if expected.len() != 64 || !expected.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(err(format!("invalid publication digest: {logical}")));
        }
        let bytes = fs::read(root.join(logical))
            .map_err(|_| err(format!("missing published member: {logical}")))?;
        if format!("{:x}", Sha256::digest(bytes)) != *expected {
            return Err(err(format!("publication digest mismatch: {logical}")));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };
    fn corpus() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../reference/Orna-1.0.0")
    }
    fn copy_corpus() -> std::path::PathBuf {
        let target = std::env::temp_dir().join(format!(
            "orna-traceability-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        copy_dir(&corpus(), &target);
        target
    }
    fn copy_dir(from: &Path, to: &Path) {
        fs::create_dir_all(to).expect("mkdir");
        for entry in fs::read_dir(from).expect("read") {
            let entry = entry.expect("entry");
            let out = to.join(entry.file_name());
            if entry.file_type().expect("type").is_dir() {
                copy_dir(&entry.path(), &out);
            } else {
                fs::copy(entry.path(), out).expect("copy");
            }
        }
    }
    #[test]
    fn covers_all_870_requirements() {
        let report = generate(corpus()).expect("valid corpus");
        assert_eq!(report.requirements.len(), 870);
        assert!(
            report
                .requirements
                .iter()
                .all(|item| !item.boundaries.is_empty())
        );
    }
    #[test]
    fn covers_every_normative_release_payload() {
        let report = generate(corpus()).expect("valid corpus");
        assert_eq!(report.normative_payloads.len(), NORMATIVE_PAYLOAD_COUNT);
        assert_eq!(
            report
                .normative_payloads
                .iter()
                .map(|payload| payload.logical_id.as_str())
                .collect::<BTreeSet<_>>(),
            report
                .publication_digests
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>()
        );
        assert!(report.normative_payloads.iter().all(|payload| {
            payload.status == Status::JustifiedGap
                && payload.implementation_refs.is_empty()
                && payload.test_refs.is_empty()
        }));
    }
    #[test]
    fn model_and_skipped_entries_never_pass() {
        let report = generate(corpus()).expect("valid corpus");
        assert!(
            report
                .requirements
                .iter()
                .all(|item| item.status != Status::Executed)
        );
        assert!(
            report
                .named_algorithms
                .iter()
                .all(|item| item.status == Status::SpecifiedModelOnly)
        );
        assert!(
            report
                .fixture_classes
                .iter()
                .all(|item| item.status == Status::JustifiedGap)
        );
    }
    #[test]
    fn repl_and_transaction_implementation_scenarios_remain_gaps_without_engine_evidence() {
        let report = generate(corpus()).expect("valid corpus");
        for (scenario_id, requirements) in [
            ("REPL-001", &["ORNA-REPL-003"][..]),
            ("REPL-002", &["ORNA-REPL-003"][..]),
            (
                "TXN-001",
                &["ORNA-TXN-001", "ORNA-TXN-002", "ORNA-TXN-003"][..],
            ),
            ("TXN-002", &["ORNA-TXN-001"][..]),
        ] {
            let scenario = report
                .behavioral_scenarios
                .iter()
                .find(|scenario| scenario.scenario_id == scenario_id)
                .expect("published scenario");
            assert_eq!(scenario.requirements, requirements, "{scenario_id}");
            assert_eq!(scenario.status, Status::JustifiedGap, "{scenario_id}");
        }
    }
    #[test]
    fn aggregate_requires_all_applicable_boundaries_to_execute() {
        let cases = [
            (
                "all executed",
                vec![Status::Executed, Status::Executed],
                Status::Executed,
            ),
            (
                "all gaps",
                vec![Status::JustifiedGap, Status::JustifiedGap],
                Status::JustifiedGap,
            ),
            (
                "executed and model",
                vec![Status::Executed, Status::SpecifiedModelOnly],
                Status::PartiallyExecuted,
            ),
            (
                "executed and gap",
                vec![Status::Executed, Status::JustifiedGap],
                Status::PartiallyExecuted,
            ),
        ];
        for (name, statuses, expected) in cases {
            assert_eq!(aggregate(&statuses), expected, "{name}");
        }
    }
    #[test]
    fn output_is_logical_only() {
        let report = generate(corpus()).expect("valid corpus");
        let json = serde_json::to_string(&report).expect("serialize");
        assert!(!json.contains(&corpus().display().to_string()));
        assert!(!json.contains("\"text\""));
        assert!(!json.contains("\"statement\""));
    }
    #[test]
    fn rejects_broken_links_and_counts() {
        let root = copy_corpus();
        let evidence = root.join("tests/requirement-evidence.json");
        let changed = fs::read_to_string(&evidence).expect("read").replacen(
            "ORNA-CONF-001",
            "MISSING-REQUIREMENT",
            1,
        );
        fs::write(&evidence, changed).expect("write");
        assert!(generate(&root).is_err());
        let _ = fs::remove_dir_all(root);
    }
    #[test]
    fn rejects_omitted_normative_payload() {
        let root = copy_corpus();
        let release = root.join("release.json");
        let mut value: Value =
            serde_json::from_str(&fs::read_to_string(&release).expect("release")).expect("json");
        value["normative_payload_sha256"]
            .as_object_mut()
            .expect("digest inventory")
            .remove("profiles/key-path.md");
        fs::write(&release, serde_json::to_vec(&value).expect("json")).expect("write");
        assert!(generate(&root).is_err());
        let _ = fs::remove_dir_all(root);
    }
    #[test]
    fn frozen_evidence_cannot_promote_fake_execution_references() {
        let root = copy_corpus();
        let evidence = root.join("tests/requirement-evidence.json");
        let mut value: Value =
            serde_json::from_str(&fs::read_to_string(&evidence).expect("evidence")).expect("json");
        let entry = &mut value["requirements"][0];
        entry["implementation_result"] = Value::String("executed".into());
        entry["implementation_ref"] = Value::String("crates/orna-runtime-v1/src/lib.rs".into());
        entry["tests"][0]["status"] = Value::String("unexecuted".into());
        entry["tests"][0]["evidence_ref"] =
            Value::String("crates/orna-runtime-v1/tests/conformance.rs".into());
        fs::write(&evidence, serde_json::to_vec(&value).expect("json")).expect("write");
        let report = generate(&root).expect("frozen evidence remains a gap");
        assert_eq!(report.requirements[0].status, Status::JustifiedGap);
        assert!(
            report
                .requirements
                .iter()
                .all(|requirement| requirement.status != Status::Executed)
        );
        assert!(
            report
                .normative_payloads
                .iter()
                .all(|payload| payload.status == Status::JustifiedGap)
        );
        let _ = fs::remove_dir_all(root);
    }
    #[test]
    fn transport_requirements_remain_justified_gaps_without_execution_register() {
        let report = generate(corpus()).expect("valid corpus");
        for requirement_id in ["ORNA-PROTO-001", "ORNA-WIRE-005", "ORNA-WIRE-011"] {
            let requirement = report
                .requirements
                .iter()
                .find(|item| item.requirement_id == requirement_id)
                .expect("transport requirement");
            assert_eq!(requirement.status, Status::JustifiedGap);
            assert!(
                requirement
                    .boundaries
                    .iter()
                    .all(|boundary| boundary.status != Status::Executed)
            );
        }
    }
    #[test]
    fn digest_bound_engine_witnesses_add_only_explicit_executed_boundaries() {
        let root = corpus();
        let harness = orna_conformance_v1::Harness::new(
            orna_conformance_v1::Corpus::load(&root).expect("conformance corpus loads"),
        );
        let mut adapter = orna_conformance_v1::RuntimeAdapter::new(
            orna_conformance_v1::BoundedEvaluator::default(),
        );
        let conformance_report = harness.run(&mut adapter);
        let witnesses = harness
            .engine_witnesses(
                &conformance_report,
                &[orna_conformance_v1::FixtureStageBinding {
                    requirement_id: "ORNA-SOURCE-001".into(),
                    fixture_id: "valid/minimal-root.orna".into(),
                    fixture_path: "examples/valid/minimal-root.orna".into(),
                    stage: Stage::Parse,
                    implementation_ref: "orna.syntax.module-entrypoint".into(),
                    test_ref: "conformance.reference_corpus.engine_witnesses".into(),
                }],
            )
            .expect("conformance report produces a witness");
        let report = generate_with_engine_witnesses(&root, &witnesses)
            .expect("digest-bound witness is accepted");
        let requirement = report
            .requirements
            .iter()
            .find(|requirement| requirement.requirement_id == "ORNA-SOURCE-001")
            .expect("witnessed requirement");
        assert_eq!(requirement.status, Status::PartiallyExecuted);
        assert!(requirement.boundaries.iter().any(|boundary| {
            boundary.kind == "engine-witness"
                && boundary.status == Status::Executed
                && boundary.implementation_ref.as_deref() == Some("orna.syntax.module-entrypoint")
        }));

        let mut mismatched = witnesses;
        mismatched
            .publication_digests
            .insert("release.json".into(), "0".repeat(64));
        assert!(generate_with_engine_witnesses(&root, &mismatched).is_err());
    }
    #[test]
    fn reproducible() {
        assert_eq!(
            generate(corpus()).expect("one"),
            generate(corpus()).expect("two")
        );
    }
}
