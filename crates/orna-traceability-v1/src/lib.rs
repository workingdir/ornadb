//! Bounded, logical traceability for the published Orna 1.0.0 reference bundle.
//! The report intentionally contains identifiers and statuses, never source bodies or host paths.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path},
};

const VERSION: &str = "1.0.0";
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
    pub requirements: Vec<RequirementTrace>,
    pub named_algorithms: Vec<InventoryEntry>,
    pub schema_profile_members: Vec<InventoryEntry>,
    pub fixture_classes: Vec<FixtureClass>,
    pub behavioral_scenarios: Vec<ScenarioTrace>,
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

pub fn generate(root: impl AsRef<Path>) -> Result<Report> {
    let root = root.as_ref();
    let release: Release = read_json(root, "release.json")?;
    if release.version != VERSION || release.normative_payload_sha256.is_empty() {
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
    Ok(Report {
        format: "orna.traceability.v1".into(),
        specification_version: VERSION.into(),
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
    })
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
    if value.contains("executed") && !value.contains("not executed") && !value.contains("model") {
        Status::Executed
    } else if value.contains("model") || value.contains("specified") {
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
                copy_dir(&entry.path(), &out)
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
        refresh_digest(&root, "tests/requirement-evidence.json");
        assert!(generate(&root).is_err());
        let _ = fs::remove_dir_all(root);
    }
    fn refresh_digest(root: &Path, logical: &str) {
        let release = root.join("release.json");
        let mut value: Value =
            serde_json::from_str(&fs::read_to_string(&release).expect("release")).expect("json");
        let digest = format!(
            "{:x}",
            Sha256::digest(fs::read(root.join(logical)).expect("member"))
        );
        value["normative_payload_sha256"][logical] = Value::String(digest);
        fs::write(release, serde_json::to_vec(&value).expect("json")).expect("write");
    }
    #[test]
    fn reproducible() {
        assert_eq!(
            generate(corpus()).expect("one"),
            generate(corpus()).expect("two")
        );
    }
}
