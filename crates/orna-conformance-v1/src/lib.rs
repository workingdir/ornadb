//! Authoritative loader and execution harness for the frozen Orna 1.0.0 corpus.
//!
//! The harness owns no copy of the corpus.  It reads a reference directory at
//! runtime, validates its cross-file contracts, then delegates source stages to
//! a compiler/runtime adapter.  Consequently a skipped adapter can never be
//! mistaken for a passing implementation.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
};

mod admitted_repl;
pub mod row_admission;
mod semantic_adapter;
mod syntax_adapter;
pub use admitted_repl::{AdmittedReplSession, ReplError};
pub use semantic_adapter::{
    BoundedEvaluator, DurableTransactionalEvaluator, RuntimeAdapter, RuntimeEvaluator,
    SemanticAdapter, TransactionalEvaluator,
};
pub use syntax_adapter::SyntaxAdapter;

/// The only shared diagnostic carrier accepted by new Orna 1.0 integration.
/// Existing generic adapters remain source-compatible during migration, but
/// callers must use this helper rather than introduce a second harness shape.
pub fn shared_diagnostic_outcome(
    diagnostic: orna_foundation_v1::Diagnostic,
) -> StageOutcome<orna_foundation_v1::Diagnostic> {
    StageOutcome::Failed(diagnostic)
}

pub const VECTOR_FILES: [&str; 6] = [
    "float-vectors.json",
    "numeric-vectors.json",
    "path-vectors.json",
    "protocol-vectors.json",
    "snapshot-vectors.json",
    "value-vectors.json",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Stage {
    Parse,
    Resolve,
    Typecheck,
    Evaluate,
    RowValidation,
}

impl Stage {
    fn expectation_key(&self) -> &'static str {
        match self {
            Self::Parse => "parse",
            Self::Resolve => "resolve",
            Self::Typecheck => "typecheck",
            Self::Evaluate => "evaluate",
            Self::RowValidation => "load_rows",
        }
    }
    fn phase_name(&self) -> &'static str {
        match self {
            Self::Parse => "parse",
            Self::Resolve => "resolve",
            Self::Typecheck => "typecheck",
            Self::Evaluate => "evaluate",
            Self::RowValidation => "row-validation",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EvidenceClass {
    Static,
    Model,
    Semantic,
    Runtime,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EvidenceStatus {
    Passed,
    Failed,
    Skipped,
    Specified,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StageOutcome<D> {
    Passed,
    /// The adapter's native structured diagnostic; the harness serializes it
    /// unchanged rather than defining a second diagnostic representation.
    Failed(D),
    Skipped {
        reason: String,
    },
}

impl<D> StageOutcome<D> {
    fn class(&self) -> EvidenceClass {
        if matches!(self, Self::Skipped { .. }) {
            EvidenceClass::Skipped
        } else {
            EvidenceClass::Runtime
        }
    }
    fn stage_class(&self, stage: &Stage) -> EvidenceClass {
        if matches!(self, Self::Skipped { .. }) {
            EvidenceClass::Skipped
        } else if matches!(
            stage,
            Stage::Resolve | Stage::Typecheck | Stage::RowValidation
        ) {
            EvidenceClass::Semantic
        } else {
            EvidenceClass::Runtime
        }
    }
    fn status(&self) -> EvidenceStatus {
        match self {
            Self::Passed => EvidenceStatus::Passed,
            Self::Failed(_) => EvidenceStatus::Failed,
            Self::Skipped { .. } => EvidenceStatus::Skipped,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SourceUnit {
    pub fixture_id: String,
    /// Corpus-relative logical identifier; host paths never cross this seam.
    pub source_id: String,
    pub parse_as: String,
    pub source: String,
}

#[derive(Debug, Clone)]
pub struct ProjectUnit {
    pub fixture_id: String,
    pub project_id: String,
    pub environment_id: Option<String>,
    /// Every declared reachable module, loaded before resolution/type checking.
    pub modules: Vec<SourceUnit>,
    /// Loose row units discovered under the project tree (empty only when the
    /// reference tree genuinely contains none).
    pub loose_rows: Vec<SourceUnit>,
    pub expectations: ProjectExpectations,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProjectExpectations {
    pub environment: ProjectEnvironment,
    pub steps: Vec<ProjectExpectationStep>,
}
#[derive(Debug, Clone, Deserialize)]
pub struct ProjectEnvironment {
    pub network: bool,
    pub credentials: bool,
    pub intrinsics: String,
    pub stdlib: Option<Value>,
    pub initial_tables: String,
}
#[derive(Debug, Clone, Deserialize)]
pub struct ProjectExpectationStep {
    pub invoke: String,
    pub expect: Value,
}
#[derive(Debug, Clone, Deserialize)]
pub struct ProjectManifest {
    pub project: String,
    pub entry: String,
    pub modules: Vec<String>,
    pub expected: String,
}

/// A requirement-linked scenario from the authoritative corpus.  Scenarios
/// are prose obligations, so an adapter must not claim one passed unless it
/// has an executable runtime contract for it.
#[derive(Debug, Clone, Deserialize)]
pub struct Scenario {
    pub id: String,
    pub title: String,
    pub given: Vec<String>,
    pub when: Vec<String>,
    pub then: Vec<String>,
    pub requirements: Vec<String>,
    pub evidence_level: String,
}

/// Integration seam for `orna-syntax`, compiler semantic analysis and runtime.
/// Each method is deliberately separate so evidence preserves the first actual
/// failing stage instead of collapsing compiler errors into a generic failure.
pub trait ConformanceAdapter {
    /// The shared compiler's native type (including spans and payload) flows
    /// through this associated type without a competing harness model.
    type Diagnostic: Serialize;
    fn diagnostic_code(&self, diagnostic: &Self::Diagnostic) -> String;
    fn diagnostic_message(&self, diagnostic: &Self::Diagnostic) -> String;
    fn parse(&mut self, unit: &SourceUnit) -> StageOutcome<Self::Diagnostic>;
    fn resolve(&mut self, unit: &SourceUnit) -> StageOutcome<Self::Diagnostic>;
    fn typecheck(&mut self, unit: &SourceUnit) -> StageOutcome<Self::Diagnostic>;
    fn evaluate(&mut self, unit: &SourceUnit) -> StageOutcome<Self::Diagnostic>;
    /// Project stages receive all reachable modules/rows, never a synthetic
    /// empty source unit.
    fn parse_project(&mut self, _: &ProjectUnit) -> StageOutcome<Self::Diagnostic> {
        skipped()
    }
    fn resolve_project(&mut self, _: &ProjectUnit) -> StageOutcome<Self::Diagnostic> {
        skipped()
    }
    fn typecheck_project(&mut self, _: &ProjectUnit) -> StageOutcome<Self::Diagnostic> {
        skipped()
    }
    /// Executes the project only through an explicitly integrated project
    /// runtime boundary. This remains distinct from row validation so a
    /// project executor cannot turn skipped rows into execution evidence.
    fn evaluate_project(&mut self, _: &ProjectUnit) -> StageOutcome<Self::Diagnostic> {
        skipped()
    }
    /// Validates a loose row unit after its table schema has been resolved.
    fn validate_row(&mut self, unit: &SourceUnit) -> StageOutcome<Self::Diagnostic>;
    fn validate_rows(&mut self, project: &ProjectUnit) -> StageOutcome<Self::Diagnostic>;
    /// Execute a corpus scenario only when the adapter has an executable
    /// contract for its prose fixture.  The default is deliberately a
    /// justified gap, never a model-derived pass.
    fn run_scenario(&mut self, _: &Scenario) -> StageOutcome<Self::Diagnostic> {
        StageOutcome::Skipped {
            reason: "authoritative scenario is prose-only; adapter exposes no scenario execution contract".into(),
        }
    }
}

/// Safe default: it records lack of an integrated implementation explicitly.
#[derive(Default)]
pub struct SkippingAdapter;
impl ConformanceAdapter for SkippingAdapter {
    type Diagnostic = Value;
    fn diagnostic_code(&self, _: &Self::Diagnostic) -> String {
        unreachable!("skipping adapter has no diagnostic")
    }
    fn diagnostic_message(&self, _: &Self::Diagnostic) -> String {
        unreachable!("skipping adapter has no diagnostic")
    }
    fn parse(&mut self, _: &SourceUnit) -> StageOutcome<Self::Diagnostic> {
        skipped()
    }
    fn resolve(&mut self, _: &SourceUnit) -> StageOutcome<Self::Diagnostic> {
        skipped()
    }
    fn typecheck(&mut self, _: &SourceUnit) -> StageOutcome<Self::Diagnostic> {
        skipped()
    }
    fn evaluate(&mut self, _: &SourceUnit) -> StageOutcome<Self::Diagnostic> {
        skipped()
    }
    fn validate_row(&mut self, _: &SourceUnit) -> StageOutcome<Self::Diagnostic> {
        skipped()
    }
    fn validate_rows(&mut self, _: &ProjectUnit) -> StageOutcome<Self::Diagnostic> {
        skipped()
    }
}
fn skipped<D>() -> StageOutcome<D> {
    StageOutcome::Skipped {
        reason: "no compiler/runtime adapter integrated".into(),
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub version: String,
    pub counts: ManifestCounts,
    pub fixtures: Vec<Fixture>,
}
#[derive(Debug, Clone, Deserialize)]
pub struct ManifestCounts {
    pub valid: usize,
    pub invalid: usize,
    pub project: usize,
    pub total: usize,
}
#[derive(Debug, Clone, Deserialize)]
pub struct Fixture {
    pub id: String,
    pub kind: String,
    pub path: String,
    pub parse_as: String,
    #[serde(default)]
    pub expect: BTreeMap<String, String>,
    #[serde(default)]
    pub failing_phase: Option<String>,
    #[serde(default)]
    pub diagnostic: Option<String>,
    #[serde(default)]
    pub message_contains: Option<String>,
    #[serde(default)]
    pub expected_diagnostic: Option<String>,
    #[serde(default)]
    pub environment: Option<String>,
}
#[derive(Debug, Clone, Deserialize)]
pub struct InvalidMetadata {
    pub version: String,
    pub count: usize,
    pub fixtures: Vec<InvalidFixture>,
}
#[derive(Debug, Clone, Deserialize)]
pub struct InvalidFixture {
    pub path: String,
    pub failing_phase: String,
    pub diagnostic: String,
    pub message_contains: String,
}
#[derive(Debug, Clone, Deserialize)]
pub struct ExpectedDiagnostic {
    pub fixture: String,
    pub failing_phase: String,
    pub primary_diagnostic: String,
    pub message_contains: String,
}
#[derive(Debug, Clone, Deserialize)]
pub struct Requirement {
    pub id: String,
    pub chapter: String,
    pub source: String,
    pub text: String,
}
#[derive(Debug, Clone, Deserialize)]
pub struct RequirementEvidence {
    pub meaning: String,
    pub requirements: Vec<RequirementEvidenceEntry>,
}
#[derive(Debug, Clone, Deserialize)]
pub struct RequirementEvidenceEntry {
    pub requirement: String,
    pub tests: Vec<Value>,
}
#[derive(Debug, Clone)]
pub struct Corpus {
    pub root: PathBuf,
    pub manifest: Manifest,
    pub invalid_metadata: InvalidMetadata,
    pub diagnostics: BTreeMap<String, ExpectedDiagnostic>,
    pub vectors: BTreeMap<String, Value>,
    pub scenarios: Value,
    pub requirements: Vec<Requirement>,
    pub requirement_evidence: RequirementEvidence,
    pub project_expectations: ProjectExpectations,
    /// Exact published normative inventory from `release.json`; it is an
    /// object of member paths to SHA-256 digests, not a lossy synthetic hash.
    pub publication_digests: BTreeMap<String, String>,
}

#[derive(Debug)]
pub struct CorpusError(pub String);
impl std::fmt::Display for CorpusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
impl std::error::Error for CorpusError {}

impl Corpus {
    pub fn default_root() -> PathBuf {
        env::var_os("ORNA_REFERENCE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../reference/Orna-1.0.0")
            })
    }
    pub fn load_default() -> Result<Self, CorpusError> {
        Self::load(Self::default_root())
    }
    pub fn load(root: impl Into<PathBuf>) -> Result<Self, CorpusError> {
        let root = root.into();
        let manifest: Manifest = read_json(&root, "tests/conformance-manifest.json")?;
        let invalid_metadata: InvalidMetadata = read_json(&root, "tests/invalid-metadata.json")?;
        let requirements = read_json(&root, "tests/requirements.json")?;
        let requirement_evidence = read_json(&root, "tests/requirement-evidence.json")?;
        let scenarios = read_json(&root, "tests/scenarios.json")?;
        let project_expectations = read_json(&root, "examples/reference/expectations.json")?;
        let mut vectors = BTreeMap::new();
        for name in VECTOR_FILES {
            vectors.insert(name.into(), read_json(&root, &format!("tests/{name}"))?);
        }
        let mut diagnostics = BTreeMap::new();
        for fixture in &manifest.fixtures {
            if let Some(relative) = &fixture.expected_diagnostic {
                let diagnostic: ExpectedDiagnostic = read_json(&root, relative)?;
                diagnostics.insert(fixture.path.clone(), diagnostic);
            }
        }
        let publication_digests = release_digests(&root)?;
        let corpus = Self {
            root,
            manifest,
            invalid_metadata,
            diagnostics,
            vectors,
            scenarios,
            requirements,
            requirement_evidence,
            project_expectations,
            publication_digests,
        };
        corpus.validate()?;
        Ok(corpus)
    }
    pub fn validate(&self) -> Result<(), CorpusError> {
        if self.manifest.version != "1.0.0" || self.invalid_metadata.version != "1.0.0" {
            return Err(CorpusError("expected Orna reference version 1.0.0".into()));
        }
        if self.manifest.fixtures.len() != 167 || self.manifest.counts.total != 167 {
            return Err(CorpusError(
                "conformance manifest must contain exactly 167 fixtures".into(),
            ));
        }
        let kinds = self
            .manifest
            .fixtures
            .iter()
            .fold(BTreeMap::new(), |mut counts, fixture| {
                *counts.entry(fixture.kind.as_str()).or_insert(0usize) += 1;
                counts
            });
        if kinds.get("valid") != Some(&86)
            || kinds.get("invalid") != Some(&80)
            || kinds.get("project") != Some(&1)
        {
            return Err(CorpusError(
                "manifest fixture kind counts do not match 86 valid / 80 invalid / 1 project"
                    .into(),
            ));
        }
        if self.invalid_metadata.count != 80
            || self.invalid_metadata.fixtures.len() != 80
            || self.diagnostics.len() != 80
        {
            return Err(CorpusError(
                "invalid corpus must contain 80 metadata and diagnostic entries".into(),
            ));
        }
        let ids: BTreeSet<_> = self
            .manifest
            .fixtures
            .iter()
            .map(|f| f.id.as_str())
            .collect();
        if ids.len() != self.manifest.fixtures.len() {
            return Err(CorpusError("fixture ids must be unique".into()));
        }
        for fixture in &self.manifest.fixtures {
            checked_path(&fixture.path)?;
            if !self.root.join(&fixture.path).is_file() && fixture.kind != "project" {
                return Err(CorpusError(format!(
                    "fixture source missing: {}",
                    fixture.path
                )));
            }
            if fixture.kind == "invalid" {
                let metadata = self
                    .invalid_metadata
                    .fixtures
                    .iter()
                    .find(|item| item.path == fixture.path)
                    .ok_or_else(|| {
                        CorpusError(format!("invalid metadata missing: {}", fixture.path))
                    })?;
                let expected = self.diagnostics.get(&fixture.path).ok_or_else(|| {
                    CorpusError(format!("expected diagnostic missing: {}", fixture.path))
                })?;
                if fixture.failing_phase.as_deref() != Some(&metadata.failing_phase)
                    || fixture.diagnostic.as_deref() != Some(&metadata.diagnostic)
                    || expected.primary_diagnostic != metadata.diagnostic
                    || expected.failing_phase != metadata.failing_phase
                    || expected.fixture != fixture.path
                    || expected.message_contains != metadata.message_contains
                {
                    return Err(CorpusError(format!(
                        "primary diagnostic contract disagrees for {}",
                        fixture.path
                    )));
                }
            }
        }
        verify_normative_members(&self.root, &self.publication_digests)?;
        if !self
            .manifest
            .fixtures
            .iter()
            .any(|f| f.failing_phase.as_deref() == Some("row-validation"))
        {
            return Err(CorpusError(
                "row-validation diagnostic case is absent".into(),
            ));
        }
        let required: BTreeSet<_> = self.requirements.iter().map(|r| r.id.as_str()).collect();
        if required.len() != self.requirements.len() || self.requirements.len() != 870 {
            return Err(CorpusError(
                "requirements corpus must contain 870 unique entries".into(),
            ));
        }
        let mapped: BTreeSet<_> = self
            .requirement_evidence
            .requirements
            .iter()
            .map(|item| item.requirement.as_str())
            .collect();
        if self.requirement_evidence.requirements.len() != self.requirements.len()
            || mapped.len() != self.requirements.len()
            || mapped != required
            || self
                .requirement_evidence
                .requirements
                .iter()
                .any(|r| r.tests.is_empty() || !required.contains(r.requirement.as_str()))
        {
            return Err(CorpusError(
                "requirement evidence must cover every requirement with a non-empty test plan"
                    .into(),
            ));
        }
        self.validate_project_assets()?;
        let scenarios = self
            .scenarios
            .get("scenarios")
            .and_then(Value::as_array)
            .ok_or_else(|| CorpusError("scenarios must contain a scenarios array".into()))?;
        if scenarios.len() != 144
            || scenarios.iter().any(|scenario| {
                scenario.get("id").and_then(Value::as_str).is_none()
                    || scenario
                        .get("requirements")
                        .and_then(Value::as_array)
                        .is_none()
            })
        {
            return Err(CorpusError(
                "scenario corpus must contain 144 identified requirement-linked scenarios".into(),
            ));
        }
        if self.vectors.len() != VECTOR_FILES.len() || self.vectors.values().any(Value::is_null) {
            return Err(CorpusError(
                "all six non-null vector suites are required".into(),
            ));
        }
        Ok(())
    }
    fn validate_project_assets(&self) -> Result<(), CorpusError> {
        let project: ProjectManifest = read_json(&self.root, "tests/project-manifest.json")?;
        if project.project != "examples/reference"
            || project.entry != "main.orna"
            || project.modules.len() != 5
            || project.expected != "examples/reference/expectations.json"
        {
            return Err(CorpusError(
                "project manifest does not describe the complete reference project".into(),
            ));
        }
        for module in &project.modules {
            checked_path(module)?;
            if !self.root.join("examples/reference").join(module).is_file() {
                return Err(CorpusError(format!("project module missing: {module}")));
            }
        }
        let declared = project.modules.iter().cloned().collect::<BTreeSet<_>>();
        discover_loose_rows(&self.root.join("examples/reference"), &declared)?;
        if self.project_expectations.steps.is_empty()
            || self.project_expectations.environment.network
            || self.project_expectations.environment.credentials
            || self.project_expectations.environment.intrinsics != "Orna 1.0.0 core"
        {
            return Err(CorpusError(
                "project expectations have an invalid offline intrinsic environment".into(),
            ));
        }
        Ok(())
    }
}

fn checked_path(relative: &str) -> Result<(), CorpusError> {
    if Path::new(relative).is_absolute()
        || Path::new(relative)
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        Err(CorpusError(format!("unsafe reference path: {relative}")))
    } else {
        Ok(())
    }
}
fn read_json<T: for<'a> Deserialize<'a>>(root: &Path, relative: &str) -> Result<T, CorpusError> {
    checked_path(relative)?;
    let body = fs::read_to_string(root.join(relative))
        .map_err(|_| CorpusError(format!("cannot read reference JSON: {relative}")))?;
    serde_json::from_str(&body)
        .map_err(|_| CorpusError(format!("invalid reference JSON: {relative}")))
}
fn release_digests(root: &Path) -> Result<BTreeMap<String, String>, CorpusError> {
    #[derive(Deserialize)]
    struct Release {
        normative_payload_sha256: BTreeMap<String, String>,
    }
    let release: Release = read_json(root, "release.json")?;
    if release.normative_payload_sha256.is_empty() {
        return Err(CorpusError(
            "release has an empty normative digest inventory".into(),
        ));
    }
    Ok(release.normative_payload_sha256)
}
pub fn verify_normative_members(
    root: &Path,
    digests: &BTreeMap<String, String>,
) -> Result<(), CorpusError> {
    for (relative, expected) in digests {
        checked_path(relative)?;
        if expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(CorpusError(format!(
                "invalid SHA-256 declaration for normative member: {relative}"
            )));
        }
        let bytes = fs::read(root.join(relative))
            .map_err(|_| CorpusError(format!("missing normative member: {relative}")))?;
        let actual = format!("{:x}", Sha256::digest(bytes));
        if actual != *expected {
            return Err(CorpusError(format!(
                "normative member digest mismatch: {relative}"
            )));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct Evidence {
    pub subject: String,
    pub stage: Option<Stage>,
    pub class: EvidenceClass,
    pub status: EvidenceStatus,
    pub detail: String,
    /// Structured assertion used by traceability; callers must not infer it
    /// by parsing the human-readable detail.
    pub expectation_satisfied: bool,
    /// JSON serialization of the adapter's native diagnostic, including any
    /// shared SourceSpan, labels and structured payload it exposes.
    pub diagnostic: Option<Value>,
    pub requirements: Vec<String>,
    pub requirement_mapping: RequirementMapping,
}
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum RequirementMapping {
    Mapped { requirements: Vec<String> },
    Unmapped { reason: String },
}
#[derive(Debug, Clone, Serialize)]
pub struct FixtureResult {
    pub fixture: String,
    pub passed: bool,
    pub stages: Vec<Evidence>,
}
#[derive(Debug, Clone, Serialize)]
pub struct RunReport {
    pub specification_version: String,
    pub implementation_claim: ImplementationClaim,
    pub publication_digests: BTreeMap<String, String>,
    pub fixtures: Vec<FixtureResult>,
    pub scenarios: Vec<ScenarioResult>,
    pub static_evidence: Vec<Evidence>,
    pub model_evidence: Vec<Evidence>,
    pub semantic_evidence: Vec<Evidence>,
    pub runtime_evidence: Vec<Evidence>,
    pub skipped_evidence: Vec<Evidence>,
    pub coverage: CoverageReport,
}
#[derive(Debug, Clone, Serialize)]
pub struct ScenarioResult {
    pub scenario: String,
    pub requirements: Vec<String>,
    pub class: EvidenceClass,
    pub status: EvidenceStatus,
    pub detail: String,
    pub diagnostic: Option<Value>,
}
#[derive(Debug, Clone, Serialize)]
pub struct ImplementationClaim {
    pub implementation_id: String,
    pub profile: String,
    pub command: String,
    pub environment: BTreeMap<String, String>,
    pub executed_scenario_contracts: Vec<String>,
}
#[derive(Debug, Clone, Serialize)]
pub struct CoverageReport {
    pub mapped_stage_evidence: usize,
    pub unmapped_stage_evidence: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct FixtureStageBinding {
    pub requirement_id: String,
    pub fixture_id: String,
    pub fixture_path: String,
    pub stage: Stage,
    pub implementation_ref: String,
    pub test_ref: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct EngineWitness {
    pub requirement_id: String,
    pub fixture_id: String,
    pub fixture_path: String,
    pub stage: Stage,
    pub implementation_ref: String,
    pub test_ref: String,
    pub observed_status: EvidenceStatus,
}

#[derive(Debug, Clone, Serialize)]
pub struct EngineWitnesses {
    pub publication_digests: BTreeMap<String, String>,
    pub witnesses: Vec<EngineWitness>,
}

pub struct Harness {
    corpus: Corpus,
    claim: ImplementationClaim,
}
impl Harness {
    pub fn new(corpus: Corpus) -> Self {
        Self {
            corpus,
            claim: ImplementationClaim {
                implementation_id: "orna-conformance-v1".into(),
                profile: "syntax-parse".into(),
                command: "orna-conformance --profile syntax-parse".into(),
                environment: BTreeMap::from([(
                    "adapter".into(),
                    "SyntaxAdapter (parse-only)".into(),
                )]),
                executed_scenario_contracts: Vec::new(),
            },
        }
    }
    pub fn with_claim(mut self, claim: ImplementationClaim) -> Self {
        self.claim = claim;
        self
    }
    pub fn run<A: ConformanceAdapter>(&self, adapter: &mut A) -> RunReport {
        let mut report = RunReport { specification_version: self.corpus.manifest.version.clone(), implementation_claim: self.claim.clone(), publication_digests: self.corpus.publication_digests.clone(), fixtures: Vec::new(), scenarios: Vec::new(), static_evidence: vec![Evidence { subject: "reference corpus".into(), stage: None, class: EvidenceClass::Static, status: EvidenceStatus::Passed, detail: "manifest, diagnostics, vectors, scenarios, requirements and normative member digests verified".into(), expectation_satisfied: false, diagnostic: None, requirements: vec![], requirement_mapping: RequirementMapping::Unmapped { reason: "authoritative requirement-evidence has no corpus-validation fixture link".into() } }], model_evidence: vec![Evidence { subject: "reference vectors and scenarios".into(), stage: None, class: EvidenceClass::Model, status: EvidenceStatus::Specified, detail: "loaded unchanged; reference models are not implementation execution".into(), expectation_satisfied: false, diagnostic: None, requirements: vec![], requirement_mapping: RequirementMapping::Unmapped { reason: "authoritative requirement-evidence has no vector/scenario fixture link".into() } }], semantic_evidence: vec![], runtime_evidence: vec![], skipped_evidence: vec![], coverage: CoverageReport { mapped_stage_evidence: 0, unmapped_stage_evidence: 0 } };
        for fixture in &self.corpus.manifest.fixtures {
            let result = self.run_fixture(fixture, adapter);
            for stage in &result.stages {
                match stage.class {
                    EvidenceClass::Semantic => report.semantic_evidence.push(stage.clone()),
                    EvidenceClass::Runtime => report.runtime_evidence.push(stage.clone()),
                    EvidenceClass::Skipped => report.skipped_evidence.push(stage.clone()),
                    _ => {}
                }
            }
            report.fixtures.push(result);
        }
        let scenarios = self.corpus.scenarios["scenarios"]
            .as_array()
            .expect("validated scenarios");
        for value in scenarios {
            let scenario: Scenario =
                serde_json::from_value(value.clone()).expect("validated scenario shape");
            let outcome = adapter.run_scenario(&scenario);
            let class = outcome.class();
            let status = outcome.status();
            let detail = match &outcome {
                StageOutcome::Passed => "scenario execution satisfied its adapter contract".into(),
                StageOutcome::Failed(_) => "scenario execution failed its adapter contract".into(),
                StageOutcome::Skipped { reason } => format!("scenario execution skipped: {reason}"),
            };
            report.scenarios.push(ScenarioResult {
                scenario: scenario.id,
                requirements: scenario.requirements,
                class,
                status,
                detail,
                diagnostic: failed_diagnostic(&outcome),
            });
        }
        let stages = report.fixtures.iter().flat_map(|fixture| &fixture.stages);
        report.coverage.mapped_stage_evidence = stages
            .clone()
            .filter(|e| matches!(e.requirement_mapping, RequirementMapping::Mapped { .. }))
            .count();
        report.coverage.unmapped_stage_evidence = report
            .fixtures
            .iter()
            .flat_map(|fixture| &fixture.stages)
            .filter(|e| matches!(e.requirement_mapping, RequirementMapping::Unmapped { .. }))
            .count();
        report
    }

    /// Convert explicit reviewed bindings into execution witnesses only when
    /// the current report contains the exact fixture/path/stage and that
    /// stage passed its declared expectation. Static, model and skipped rows
    /// can never become engine evidence through this boundary.
    pub fn engine_witnesses(
        &self,
        report: &RunReport,
        bindings: &[FixtureStageBinding],
    ) -> Result<EngineWitnesses, String> {
        let requirements = self
            .corpus
            .requirements
            .iter()
            .map(|requirement| requirement.id.as_str())
            .collect::<BTreeSet<_>>();
        let mut seen = BTreeSet::new();
        let mut witnesses = Vec::with_capacity(bindings.len());
        for binding in bindings {
            if !requirements.contains(binding.requirement_id.as_str()) {
                return Err(format!(
                    "unknown requirement binding: {}",
                    binding.requirement_id
                ));
            }
            let fixture = self
                .corpus
                .manifest
                .fixtures
                .iter()
                .find(|fixture| fixture.id == binding.fixture_id)
                .ok_or_else(|| format!("unknown fixture binding: {}", binding.fixture_id))?;
            if fixture.path != binding.fixture_path {
                return Err(format!(
                    "fixture binding path mismatch: {}",
                    binding.fixture_id
                ));
            }
            let key = (
                binding.requirement_id.as_str(),
                binding.fixture_id.as_str(),
                format!("{:?}", binding.stage),
            );
            if !seen.insert(key) {
                return Err(format!(
                    "duplicate fixture stage binding: {}",
                    binding.fixture_id
                ));
            }
            let result = report
                .fixtures
                .iter()
                .find(|result| result.fixture == binding.fixture_id)
                .ok_or_else(|| format!("fixture is absent from report: {}", binding.fixture_id))?;
            let evidence = result
                .stages
                .iter()
                .find(|evidence| evidence.stage.as_ref() == Some(&binding.stage))
                .ok_or_else(|| {
                    format!(
                        "fixture stage is absent from report: {}",
                        binding.fixture_id
                    )
                })?;
            if !evidence.expectation_satisfied
                || !matches!(
                    evidence.class,
                    EvidenceClass::Semantic | EvidenceClass::Runtime
                )
                || evidence.status == EvidenceStatus::Skipped
            {
                return Err(format!(
                    "fixture stage is not executed evidence: {}",
                    binding.fixture_id
                ));
            }
            witnesses.push(EngineWitness {
                requirement_id: binding.requirement_id.clone(),
                fixture_id: binding.fixture_id.clone(),
                fixture_path: binding.fixture_path.clone(),
                stage: binding.stage.clone(),
                implementation_ref: binding.implementation_ref.clone(),
                test_ref: binding.test_ref.clone(),
                observed_status: evidence.status.clone(),
            });
        }
        Ok(EngineWitnesses {
            publication_digests: report.publication_digests.clone(),
            witnesses,
        })
    }
    fn run_fixture<A: ConformanceAdapter>(
        &self,
        fixture: &Fixture,
        adapter: &mut A,
    ) -> FixtureResult {
        let source_path = self.corpus.root.join(&fixture.path);
        let project = fixture.kind == "project";
        let unit = SourceUnit {
            fixture_id: fixture.id.clone(),
            source_id: fixture.path.clone(),
            parse_as: fixture.parse_as.clone(),
            source: if project {
                String::new()
            } else {
                fs::read_to_string(&source_path).unwrap_or_default()
            },
        };
        let project_unit = project.then(|| self.project_unit(fixture));
        let stages = if project || fixture.failing_phase.as_deref() == Some("row-validation") {
            vec![
                Stage::Parse,
                Stage::Resolve,
                Stage::Typecheck,
                Stage::Evaluate,
                Stage::RowValidation,
            ]
        } else {
            vec![
                Stage::Parse,
                Stage::Resolve,
                Stage::Typecheck,
                Stage::Evaluate,
            ]
        };
        let mut halted = false;
        let mut evidence = Vec::new();
        let requirement_mapping = self.requirement_mapping(fixture);
        let requirements = match &requirement_mapping {
            RequirementMapping::Mapped { requirements } => requirements.clone(),
            RequirementMapping::Unmapped { .. } => Vec::new(),
        };
        for stage in stages {
            let expected = expected_stage(fixture, &stage);
            let outcome = if halted {
                StageOutcome::Skipped {
                    reason: "earlier stage did not pass".into(),
                }
            } else if expected == "not-run" {
                StageOutcome::Skipped {
                    reason: "stage is outside the fixture execution expectation".into(),
                }
            } else {
                match stage {
                    Stage::Parse if project => {
                        adapter.parse_project(project_unit.as_ref().expect("project unit"))
                    }
                    Stage::Resolve if project => {
                        adapter.resolve_project(project_unit.as_ref().expect("project unit"))
                    }
                    Stage::Typecheck if project => {
                        adapter.typecheck_project(project_unit.as_ref().expect("project unit"))
                    }
                    Stage::Evaluate if project => {
                        adapter.evaluate_project(project_unit.as_ref().expect("project unit"))
                    }
                    Stage::Parse => adapter.parse(&unit),
                    Stage::Resolve => adapter.resolve(&unit),
                    Stage::Typecheck => adapter.typecheck(&unit),
                    Stage::Evaluate => adapter.evaluate(&unit),
                    Stage::RowValidation if project => {
                        adapter.validate_rows(project_unit.as_ref().expect("project unit"))
                    }
                    Stage::RowValidation => adapter.validate_row(&unit),
                }
            };
            let correct = stage_matches(fixture, &stage, expected, &outcome, adapter);
            // An explicitly unimplemented stage may be `not-run` while a
            // later independent stage (notably project row validation) still
            // has its own executable adapter. Record the skip as evidence,
            // but do not let it erase that later evidence.
            let should_halt = match &outcome {
                StageOutcome::Passed => false,
                StageOutcome::Skipped { .. } if expected == "not-run" => false,
                StageOutcome::Failed(_) | StageOutcome::Skipped { .. } => true,
            };
            if should_halt {
                halted = true;
            }
            let class = outcome.stage_class(&stage);
            evidence.push(Evidence {
                subject: fixture.id.clone(),
                stage: Some(stage),
                class,
                status: outcome.status(),
                detail: format!(
                    "expected {expected}; {}",
                    if correct {
                        "expectation satisfied"
                    } else {
                        "expectation NOT satisfied"
                    }
                ),
                expectation_satisfied: correct,
                diagnostic: failed_diagnostic(&outcome),
                requirements: requirements.clone(),
                requirement_mapping: requirement_mapping.clone(),
            });
        }
        let passed = evidence.iter().all(|e| !e.detail.contains("NOT satisfied"))
            && evidence
                .iter()
                .any(|e| e.status == EvidenceStatus::Passed || e.status == EvidenceStatus::Failed);
        FixtureResult {
            fixture: fixture.id.clone(),
            passed,
            stages: evidence,
        }
    }
    fn requirement_mapping(&self, fixture: &Fixture) -> RequirementMapping {
        let linked = self
            .corpus
            .requirement_evidence
            .requirements
            .iter()
            .filter(|entry| {
                entry.tests.iter().any(|test| {
                    test.get("fixture").and_then(Value::as_str) == Some(fixture.id.as_str())
                        || test.get("path").and_then(Value::as_str) == Some(fixture.path.as_str())
                })
            })
            .map(|entry| entry.requirement.clone())
            .collect::<Vec<_>>();
        if linked.is_empty() {
            RequirementMapping::Unmapped {
                reason: "authoritative requirement-evidence/tests contains no fixture or path link"
                    .into(),
            }
        } else {
            RequirementMapping::Mapped {
                requirements: linked,
            }
        }
    }
    fn project_unit(&self, fixture: &Fixture) -> ProjectUnit {
        let root = self.corpus.root.join(&fixture.path);
        let manifest: ProjectManifest = read_json(&self.corpus.root, "tests/project-manifest.json")
            .expect("validated project manifest");
        let declared_modules = manifest.modules.iter().cloned().collect::<BTreeSet<_>>();
        let modules = manifest
            .modules
            .into_iter()
            .map(|name| SourceUnit {
                fixture_id: fixture.id.clone(),
                source_id: format!("{}/{}", fixture.path, name),
                parse_as: "module_unit".into(),
                source: fs::read_to_string(root.join(name)).expect("validated project module"),
            })
            .collect();
        ProjectUnit {
            fixture_id: fixture.id.clone(),
            project_id: fixture.path.clone(),
            environment_id: fixture.environment.clone(),
            modules,
            loose_rows: discover_loose_rows(&root, &declared_modules)
                .expect("validated project traversal"),
            expectations: self.corpus.project_expectations.clone(),
        }
    }
}

/// Reports expose only the diagnostic identity.  Adapter diagnostics may have
/// spans, labels or native payloads containing source observations; preserving
/// any of those would let a source-bearing adapter bypass the logical-only
/// conformance boundary.
fn failed_diagnostic<D: Serialize>(outcome: &StageOutcome<D>) -> Option<Value> {
    let StageOutcome::Failed(diagnostic) = outcome else {
        return None;
    };
    let code = serde_json::to_value(diagnostic)
        .ok()
        .and_then(|value| value.get("code").cloned())
        .unwrap_or(Value::String("ORNA-CONFORMANCE-REDACTED".into()));
    Some(serde_json::json!({"code": code, "spans": [], "redacted": true}))
}

fn discover_loose_rows(
    root: &Path,
    declared_modules: &BTreeSet<String>,
) -> Result<Vec<SourceUnit>, CorpusError> {
    fn visit(
        root: &Path,
        dir: &Path,
        declared_modules: &BTreeSet<String>,
        rows: &mut Vec<SourceUnit>,
    ) -> Result<(), CorpusError> {
        let entries = fs::read_dir(dir)
            .map_err(|_| CorpusError("cannot traverse reference project rows".into()))?;
        for entry in entries {
            let entry =
                entry.map_err(|_| CorpusError("cannot traverse reference project rows".into()))?;
            let path = entry.path();
            if path.is_dir() {
                visit(root, &path, declared_modules, rows)?;
            } else if path.extension().is_some_and(|ext| ext == "orna")
                && path.parent() != Some(root)
                && !declared_modules.contains(
                    &path
                        .strip_prefix(root)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .replace('\\', "/"),
                )
            {
                let source = fs::read_to_string(&path)
                    .map_err(|_| CorpusError("cannot read discovered loose row".into()))?;
                rows.push(SourceUnit {
                    fixture_id: "PROJECT-REFERENCE".into(),
                    source_id: format!(
                        "examples/reference/{}",
                        path.strip_prefix(root)
                            .unwrap_or(&path)
                            .to_string_lossy()
                            .replace('\\', "/")
                    ),
                    parse_as: "row_unit".into(),
                    source,
                });
            }
        }
        Ok(())
    }
    let mut rows = Vec::new();
    visit(root, root, declared_modules, &mut rows)?;
    Ok(rows)
}

/// The invalid metadata is authoritative for an explicit row-validation failure
/// even though the legacy manifest field retains `typecheck: fail`.
fn expected_stage<'a>(fixture: &'a Fixture, stage: &Stage) -> &'a str {
    if fixture.failing_phase.as_deref() == Some("row-validation") {
        return match stage {
            Stage::Parse | Stage::Resolve | Stage::Typecheck => "pass",
            Stage::RowValidation => "fail",
            Stage::Evaluate => "not-run",
        };
    }
    fixture
        .expect
        .get(stage.expectation_key())
        .map(String::as_str)
        .unwrap_or("not-run")
}

fn stage_matches<A: ConformanceAdapter>(
    fixture: &Fixture,
    stage: &Stage,
    expected: &str,
    outcome: &StageOutcome<A::Diagnostic>,
    adapter: &A,
) -> bool {
    match (expected, outcome) {
        ("not-run", StageOutcome::Skipped { .. }) => true,
        ("pass", StageOutcome::Passed) => true,
        ("fail", StageOutcome::Failed(actual)) => {
            fixture.failing_phase.as_deref() == Some(stage.phase_name())
                && fixture.diagnostic.as_deref() == Some(adapter.diagnostic_code(actual).as_str())
                && fixture
                    .message_contains
                    .as_deref()
                    .is_none_or(|text| adapter.diagnostic_message(actual).contains(text))
        }
        // A skipped required pass/fail is absent execution, never a pass.
        (_, StageOutcome::Skipped { .. }) => false,
        _ => false,
    }
}
