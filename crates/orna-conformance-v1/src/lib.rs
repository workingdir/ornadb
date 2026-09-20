//! Authoritative loader and execution harness for the frozen Orna 1.0.0 corpus.
//!
//! The harness owns no copy of the corpus.  It reads a reference directory at
//! runtime, validates its cross-file contracts, then delegates source stages to
//! a compiler/runtime adapter.  Consequently a skipped adapter can never be
//! mistaken for a passing implementation.

use futures::executor::block_on;
use num_bigint::BigInt;
use orna_evaluator_v1::Environment;
use orna_foundation_v1::{OvbRaw, Value as CanonicalValue};
use orna_repository_v1::Repository;
use orna_runtime_v1::{RuntimeIdentity, RuntimeState};
use orna_stream_v1::{Checkpoint, CheckpointKey, Component, ConsumerIdentity};
use orna_syntax_v1::{Declaration, Expr, Pattern, TableMember, TypeExpr, parse_module};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

const SPECIFICATION_VERSION: &str = "1.0.0";
const NORMATIVE_PAYLOAD_COUNT: usize = 46;
const EXPECTED_DIAGNOSTIC_STATUS: &str = "expected-not-executed";

mod admitted_repl;
pub mod catalogue_projection;
pub mod row_admission;
mod semantic_adapter;
mod syntax_adapter;
pub use admitted_repl::{AdmittedReplSession, ReplError};
pub use catalogue_projection::{
    CatalogueProjectionError, SourceCatalogueActivationError,
    commit_resolved_source_catalogue_activation, project_source_catalogue,
};
pub use semantic_adapter::{
    BoundedEvaluator, DurableTransactionalEvaluator, RunningTableRequestDisposition,
    RuntimeAdapter, RuntimeEvaluator, RuntimeTarget, SemanticAdapter, TransactionalEvaluator,
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
    RuntimeAdapter,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EvidenceStatus {
    Passed,
    Failed,
    Cancelled,
    Skipped,
    Specified,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StageOutcome<D> {
    Passed,
    /// The adapter's native structured diagnostic; the harness serializes it
    /// unchanged rather than defining a second diagnostic representation.
    Failed(D),
    Cancelled(D),
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
            Self::Cancelled(_) => EvidenceStatus::Cancelled,
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
    #[serde(default)]
    pub negative_cases: Vec<ProjectNegativeCase>,
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
pub struct ProjectNegativeCase {
    pub invoke: String,
    pub args: Vec<Value>,
    pub expect: String,
}
#[derive(Debug, Clone, Deserialize)]
pub struct ProjectManifest {
    pub project: String,
    pub entry: String,
    pub modules: Vec<String>,
    pub expected: String,
    pub implementation_execution: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReferenceProjectInvocationEvidence {
    pub invoke: String,
    pub status: EvidenceStatus,
    pub checks: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReferenceProjectNegativeEvidence {
    pub invoke: String,
    pub args: Vec<Value>,
    pub expected: String,
    pub status: EvidenceStatus,
    pub rollback_verified: bool,
}

/// Evidence from the existing durable project/table/stream adapter.
///
/// This is intentionally not part of [`RunReport`]: it is not compiler
/// artifact execution and cannot be used as an Orna-engine conformance
/// witness.  The separate classification makes that boundary machine-visible.
#[derive(Debug, Clone, Serialize)]
pub struct ReferenceProjectRuntimeEvidence {
    pub classification: EvidenceClass,
    pub specification_version: String,
    pub profile: String,
    pub implementation_execution: String,
    pub compiler_artifact_execution: bool,
    pub full_orna_engine_conformance: bool,
    pub status: EvidenceStatus,
    pub detail: String,
    pub invocations: Vec<ReferenceProjectInvocationEvidence>,
    pub negative_cases: Vec<ReferenceProjectNegativeEvidence>,
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
    pub version: String,
    pub fixture: String,
    pub failing_phase: String,
    pub primary_diagnostic: String,
    pub message_contains: String,
    pub status: String,
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
                checked_path(relative)?;
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
        if self.manifest.version != SPECIFICATION_VERSION
            || self.invalid_metadata.version != SPECIFICATION_VERSION
        {
            return Err(CorpusError("expected Orna reference version 1.0.0".into()));
        }
        let kinds = self
            .manifest
            .fixtures
            .iter()
            .fold(BTreeMap::new(), |mut counts, fixture| {
                *counts.entry(fixture.kind.as_str()).or_insert(0usize) += 1;
                counts
            });
        if self.manifest.fixtures.len() != 167
            || self.manifest.counts.total != self.manifest.fixtures.len()
            || self.manifest.counts.valid != kinds.get("valid").copied().unwrap_or(0)
            || self.manifest.counts.invalid != kinds.get("invalid").copied().unwrap_or(0)
            || self.manifest.counts.project != kinds.get("project").copied().unwrap_or(0)
        {
            return Err(CorpusError(
                "conformance manifest counts disagree with its fixtures".into(),
            ));
        }
        if kinds.get("valid") != Some(&86)
            || kinds.get("invalid") != Some(&80)
            || kinds.get("project") != Some(&1)
        {
            return Err(CorpusError(
                "manifest fixture kind counts do not match 86 valid / 80 invalid / 1 project"
                    .into(),
            ));
        }
        if self.invalid_metadata.count != self.invalid_metadata.fixtures.len()
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
        let paths = self
            .manifest
            .fixtures
            .iter()
            .map(|fixture| fixture.path.as_str())
            .collect::<BTreeSet<_>>();
        if paths.len() != self.manifest.fixtures.len() {
            return Err(CorpusError("fixture paths must be unique".into()));
        }
        let invalid_paths = self
            .manifest
            .fixtures
            .iter()
            .filter(|fixture| fixture.kind == "invalid")
            .map(|fixture| fixture.path.as_str())
            .collect::<BTreeSet<_>>();
        let metadata_paths = self
            .invalid_metadata
            .fixtures
            .iter()
            .map(|fixture| fixture.path.as_str())
            .collect::<BTreeSet<_>>();
        let diagnostic_paths = self
            .diagnostics
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        if metadata_paths.len() != self.invalid_metadata.fixtures.len()
            || metadata_paths != invalid_paths
            || diagnostic_paths != invalid_paths
        {
            return Err(CorpusError(
                "invalid metadata and expected diagnostics must exactly cover invalid fixtures"
                    .into(),
            ));
        }
        for fixture in &self.manifest.fixtures {
            checked_path(&fixture.path)?;
            if fixture.id.is_empty() || fixture.parse_as.is_empty() {
                return Err(CorpusError(format!(
                    "fixture identity or parse mode is empty: {}",
                    fixture.path
                )));
            }
            if !matches!(fixture.kind.as_str(), "valid" | "invalid" | "project") {
                return Err(CorpusError(format!(
                    "fixture kind is not part of the published schema: {}",
                    fixture.kind
                )));
            }
            if fixture.expect.iter().any(|(stage, result)| {
                !matches!(
                    stage.as_str(),
                    "parse" | "resolve" | "typecheck" | "evaluate" | "load_rows"
                ) || !matches!(result.as_str(), "pass" | "fail" | "not-run")
            }) {
                return Err(CorpusError(format!(
                    "fixture has an invalid stage expectation: {}",
                    fixture.path
                )));
            }
            if !self.root.join(&fixture.path).is_file() && fixture.kind != "project" {
                return Err(CorpusError(format!(
                    "fixture source missing: {}",
                    fixture.path
                )));
            }
            match fixture.kind.as_str() {
                "valid" => {
                    if fixture.failing_phase.is_some()
                        || fixture.diagnostic.is_some()
                        || fixture.message_contains.is_some()
                        || fixture.expected_diagnostic.is_some()
                        || fixture.expect.iter().any(|(stage, result)| {
                            matches!(stage.as_str(), "parse" | "resolve" | "typecheck")
                                && result != "pass"
                        })
                    {
                        return Err(CorpusError(format!(
                            "valid fixture has invalid failure metadata or expectations: {}",
                            fixture.path
                        )));
                    }
                }
                "project" => {
                    if fixture.parse_as != "database_project"
                        || fixture.failing_phase.is_some()
                        || fixture.diagnostic.is_some()
                        || fixture.message_contains.is_some()
                        || fixture.expected_diagnostic.is_some()
                        || fixture.expect.get("parse").map(String::as_str) != Some("pass")
                        || fixture.expect.get("resolve").map(String::as_str) != Some("pass")
                        || fixture.expect.get("typecheck").map(String::as_str) != Some("pass")
                        || fixture.expect.get("load_rows").map(String::as_str) != Some("pass")
                    {
                        return Err(CorpusError(format!(
                            "project fixture schema is invalid: {}",
                            fixture.path
                        )));
                    }
                }
                "invalid" => {
                    if fixture.failing_phase.is_none()
                        || fixture.diagnostic.is_none()
                        || fixture.expected_diagnostic.is_none()
                    {
                        return Err(CorpusError(format!(
                            "invalid fixture lacks diagnostic metadata: {}",
                            fixture.path
                        )));
                    }
                }
                _ => unreachable!("fixture kind validated above"),
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
                if expected.version != SPECIFICATION_VERSION
                    || expected.status != EXPECTED_DIAGNOSTIC_STATUS
                    || fixture.failing_phase.as_deref() != Some(&metadata.failing_phase)
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
        for entry in &self.requirement_evidence.requirements {
            for test in &entry.tests {
                validate_requirement_evidence_test(test, &entry.requirement)?;
            }
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
            || project.implementation_execution != "not executed"
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

    /// Loads the immutable reference project as independently addressable
    /// modules for the durable runtime adapter.  The project manifest's
    /// implementation execution marker is validated before this seam returns.
    pub fn reference_project(&self) -> Result<ProjectUnit, CorpusError> {
        let manifest: ProjectManifest = read_json(&self.root, "tests/project-manifest.json")?;
        if manifest.implementation_execution != "not executed" {
            return Err(CorpusError(
                "reference project implementation execution marker must remain not executed".into(),
            ));
        }
        let root = self.root.join(&manifest.project);
        let declared_modules = manifest.modules.iter().cloned().collect::<BTreeSet<_>>();
        let modules = manifest
            .modules
            .into_iter()
            .map(|name| {
                Ok(SourceUnit {
                    fixture_id: "PROJECT-REFERENCE".into(),
                    source_id: format!("{}/{}", manifest.project, name),
                    parse_as: "module_unit".into(),
                    source: fs::read_to_string(root.join(name)).map_err(|_| {
                        CorpusError("reference project module is unreadable".into())
                    })?,
                })
            })
            .collect::<Result<Vec<_>, CorpusError>>()?;
        Ok(ProjectUnit {
            fixture_id: "PROJECT-REFERENCE".into(),
            project_id: manifest.project,
            environment_id: Some("reference-offline".into()),
            modules,
            loose_rows: discover_loose_rows(&root, &declared_modules)?,
            expectations: self.project_expectations.clone(),
        })
    }
}

/// Encoded `(key, value)` byte pair for one admitted reference table row.
type ReferenceEncodedRow = (Vec<u8>, Vec<u8>);

#[derive(Debug, Clone)]
struct ReferenceTableSchema {
    fields: Vec<(String, String)>,
    key_count: usize,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct ReferenceRuntimeSnapshot {
    tables: BTreeMap<String, Vec<ReferenceEncodedRow>>,
    checkpoint: Option<Checkpoint>,
}

/// Execute the immutable reference project through the existing durable
/// project/table/stream adapter.  The returned record is deliberately a
/// separate evidence product: it cannot be promoted to compiler or engine
/// conformance evidence.
pub fn run_reference_project_runtime_adapter(corpus: &Corpus) -> ReferenceProjectRuntimeEvidence {
    match block_on(execute_reference_project_runtime_adapter(corpus)) {
        Ok(mut evidence) => {
            evidence.classification = EvidenceClass::RuntimeAdapter;
            evidence
        }
        Err(detail) => ReferenceProjectRuntimeEvidence {
            classification: EvidenceClass::RuntimeAdapter,
            specification_version: corpus.manifest.version.clone(),
            profile: "reference-project-runtime-adapter".into(),
            implementation_execution: "not executed".into(),
            compiler_artifact_execution: false,
            full_orna_engine_conformance: false,
            status: EvidenceStatus::Failed,
            detail,
            invocations: Vec::new(),
            negative_cases: Vec::new(),
        },
    }
}

async fn execute_reference_project_runtime_adapter(
    corpus: &Corpus,
) -> Result<ReferenceProjectRuntimeEvidence, String> {
    let project = corpus
        .reference_project()
        .map_err(|_| "immutable reference project could not be loaded".to_owned())?;
    let schemas = reference_table_schemas(&project)?;
    let (root, repository) = create_reference_runtime_repository()?;
    let result = async {
        let identity = RuntimeIdentity {
            database_id: [121; 16],
            repository_id: [122; 16],
        };
        let owner_id = [123; 16];
        let initial_digest = [124; 32];
        let evaluator = DurableTransactionalEvaluator::default();
        let mut invocations = Vec::new();

        if project.expectations.steps.len() != 4 {
            return Err("immutable reference project must contain four positive steps".into());
        }
        for (step_index, step) in project.expectations.steps.iter().enumerate() {
            let entry = reference_entry_name(&project, &step.invoke)?;
            if step_index == 2 || step_index == 3 {
                if entry != "sensors.ingest" {
                    return Err("reference stream expectation is not sensors.ingest".into());
                }
                let before = if step_index == 3 {
                    let state = RuntimeState::open(&repository, identity, initial_digest)
                        .await
                        .map_err(|_| "reference runtime state could not be reopened".to_owned())?;
                    let key = reference_stream_checkpoint_key(&project, identity)?;
                    let snapshot = reference_runtime_snapshot(&state, &schemas, &key).await?;
                    drop(state);
                    Some(snapshot)
                } else {
                    None
                };
                expect_pass(
                    evaluator
                        .execute_project_stream(
                            &repository,
                            identity,
                            owner_id,
                            initial_digest,
                            &project,
                            &entry,
                        )
                        .await
                        .map_err(|_| "reference stream activation failed".to_owned())?,
                    &entry,
                )?;
                let state = RuntimeState::open(&repository, identity, initial_digest)
                    .await
                    .map_err(|_| "reference runtime state could not be reopened".to_owned())?;
                let mut checks = verify_reference_step(
                    &state,
                    &project,
                    &schemas,
                    step,
                    before
                        .as_ref()
                        .and_then(|snapshot| snapshot.tables.get("Reading"))
                        .map(Vec::len),
                )
                .await?;
                if let Some(before) = before {
                    let key = reference_stream_checkpoint_key(&project, identity)?;
                    let after = reference_runtime_snapshot(&state, &schemas, &key).await?;
                    if before != after {
                        return Err(
                            "second reference stream invocation changed rows or checkpoint"
                                .into(),
                        );
                    }
                    checks.push("all table rows and checkpoint unchanged on rerun".into());
                }
                drop(state);
                invocations.push(ReferenceProjectInvocationEvidence {
                    invoke: entry,
                    status: EvidenceStatus::Passed,
                    checks,
                });
            } else {
                if entry != format!("main.{}", step.invoke) {
                    return Err("reference transaction expectation is not rooted in main".into());
                }
                expect_pass(
                    evaluator
                        .execute_project(
                            &repository,
                            identity,
                            owner_id,
                            initial_digest,
                            &project,
                            &entry,
                        )
                        .await
                        .map_err(|_| "reference transaction activation failed".to_owned())?,
                    &entry,
                )?;
                let state = RuntimeState::open(&repository, identity, initial_digest)
                    .await
                    .map_err(|_| "reference runtime state could not be reopened".to_owned())?;
                let checks = verify_reference_step(
                    &state,
                    &project,
                    &schemas,
                    step,
                    None,
                )
                .await?;
                drop(state);
                invocations.push(ReferenceProjectInvocationEvidence {
                    invoke: entry,
                    status: EvidenceStatus::Passed,
                    checks,
                });
            }
        }

        let mut negative_cases = Vec::new();
        for case in &project.expectations.negative_cases {
            let before = {
                let state = RuntimeState::open(&repository, identity, initial_digest)
                    .await
                    .map_err(|_| "reference runtime state could not be reopened".to_owned())?;
                let snapshot = reference_table_snapshot(&state, &schemas).await?;
                drop(state);
                snapshot
            };
            let arguments = reference_arguments(&project, case)?;
            let outcome = evaluator
                .execute_project_with_arguments(
                    RuntimeTarget {
                        repository: &repository,
                        identity,
                        owner_id,
                        initial_digest,
                    },
                    &project,
                    &case.invoke,
                    &arguments,
                )
                .await
                .map_err(|_| "reference negative activation could not be admitted".to_owned())?;
            if !matches!(outcome, StageOutcome::Failed(_)) {
                return Err(format!("negative reference invocation did not fail: {}", case.invoke));
            }
            let state = RuntimeState::open(&repository, identity, initial_digest)
                .await
                .map_err(|_| "reference runtime state could not be reopened".to_owned())?;
            let after = reference_table_snapshot(&state, &schemas).await?;
            drop(state);
            let rollback_verified = before == after;
            if !rollback_verified {
                return Err(format!("negative reference invocation changed rows: {}", case.invoke));
            }
            negative_cases.push(ReferenceProjectNegativeEvidence {
                invoke: case.invoke.clone(),
                args: case.args.clone(),
                expected: case.expect.clone(),
                status: EvidenceStatus::Passed,
                rollback_verified,
            });
        }

        Ok(ReferenceProjectRuntimeEvidence {
            classification: EvidenceClass::RuntimeAdapter,
            specification_version: corpus.manifest.version.clone(),
            profile: "reference-project-runtime-adapter".into(),
            implementation_execution: "not executed".into(),
            compiler_artifact_execution: false,
            full_orna_engine_conformance: false,
            status: EvidenceStatus::Passed,
            detail: "durable runtime-adapter evidence; not compiler-produced artifact execution or full Orna-engine conformance".into(),
            invocations,
            negative_cases,
        })
    }
    .await;
    drop(repository);
    match (result, cleanup_reference_runtime_repository(&root)) {
        (Ok(evidence), Ok(())) => Ok(evidence),
        (Err(detail), Ok(())) => Err(detail),
        (Ok(_), Err(cleanup)) => Err(cleanup),
        (Err(detail), Err(cleanup)) => Err(format!("{detail}; {cleanup}")),
    }
}

fn cleanup_reference_runtime_repository(root: &Path) -> Result<(), String> {
    fs::remove_dir_all(root).map_err(|_| "reference runtime scratch cleanup failed".to_owned())
}

fn create_reference_runtime_repository() -> Result<(PathBuf, Repository), String> {
    let target_root = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join("target")
        });
    let root = target_root.join(format!(
        "orna-reference-runtime-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos())
    ));
    fs::create_dir_all(&target_root)
        .map_err(|_| "reference runtime target directory could not be created".to_owned())?;
    fs::create_dir(&root)
        .map_err(|_| "reference runtime scratch repository could not be created".to_owned())?;
    let initialized = Command::new("git")
        .args(["init", "--quiet", "-b", "main"])
        .current_dir(&root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    if !initialized {
        return Err(match cleanup_reference_runtime_repository(&root) {
            Ok(()) => "reference runtime scratch repository could not be initialized".into(),
            Err(cleanup) => {
                format!("reference runtime scratch repository could not be initialized; {cleanup}")
            }
        });
    }
    let repository = match Repository::discover(&root) {
        Ok(repository) => repository,
        Err(_) => {
            return Err(match cleanup_reference_runtime_repository(&root) {
                Ok(()) => "reference runtime repository could not be discovered".to_owned(),
                Err(cleanup) => {
                    format!("reference runtime repository could not be discovered; {cleanup}")
                }
            });
        }
    };
    Ok((root, repository))
}

fn expect_pass(
    outcome: StageOutcome<orna_foundation_v1::Diagnostic>,
    entry: &str,
) -> Result<(), String> {
    match outcome {
        StageOutcome::Passed => Ok(()),
        StageOutcome::Failed(_) => Err(format!("reference invocation failed: {entry}")),
        StageOutcome::Cancelled(_) => Err(format!("reference invocation was cancelled: {entry}")),
        StageOutcome::Skipped { .. } => Err(format!("reference invocation was skipped: {entry}")),
    }
}

fn reference_entry_name(project: &ProjectUnit, invoke: &str) -> Result<String, String> {
    if invoke.contains('.') {
        return Ok(invoke.to_owned());
    }
    let module = project
        .modules
        .iter()
        .find(|unit| unit.source_id.ends_with("/main.orna"))
        .ok_or_else(|| "reference entry module is missing".to_owned())?;
    let namespace = module
        .source_id
        .rsplit('/')
        .next()
        .and_then(|name| name.strip_suffix(".orna"))
        .ok_or_else(|| "reference entry module identity is invalid".to_owned())?;
    Ok(format!("{namespace}.{invoke}"))
}

async fn verify_reference_step(
    state: &RuntimeState,
    project: &ProjectUnit,
    schemas: &BTreeMap<String, ReferenceTableSchema>,
    step: &ProjectExpectationStep,
    previous_reading_count: Option<usize>,
) -> Result<Vec<String>, String> {
    let object = step
        .expect
        .as_object()
        .ok_or_else(|| "reference expectation must be an object".to_owned())?;
    let mut checks = Vec::new();
    for (name, expected) in object {
        match name.as_str() {
            "checkpoint_next" => {
                let expected = expected.as_u64().ok_or_else(|| {
                    "reference checkpoint expectation is not an integer".to_owned()
                })?;
                let key = reference_stream_checkpoint_key(
                    project,
                    state
                        .identity()
                        .await
                        .map_err(|_| "reference runtime identity could not be read".to_owned())?,
                )?;
                let checkpoint = state
                    .stream_checkpoint(&key)
                    .await
                    .map_err(|_| "reference stream checkpoint could not be read".to_owned())?;
                let observed = checkpoint
                    .committed
                    .as_ref()
                    .and_then(|position| position.token.as_str().parse::<u64>().ok())
                    .ok_or_else(|| {
                        "reference stream checkpoint has no numeric position".to_owned()
                    })?;
                if observed != expected {
                    return Err(
                        "reference stream checkpoint disagrees with immutable expectation".into(),
                    );
                }
                checks.push("checkpoint matched immutable expectation".into());
            }
            "additional_rows" => {
                let expected = expected.as_u64().ok_or_else(|| {
                    "reference additional-row expectation is not an integer".to_owned()
                })?;
                let before = previous_reading_count.ok_or_else(|| {
                    "reference additional-row expectation lacks a baseline".to_owned()
                })?;
                let observed = state
                    .committed_table_rows("Reading")
                    .await
                    .map_err(|_| "reference Reading rows could not be read".to_owned())?
                    .len()
                    .saturating_sub(before);
                if observed as u64 != expected {
                    return Err("reference stream added an unexpected number of rows".into());
                }
                checks.push("additional row count matched immutable expectation".into());
            }
            table_name => {
                let table = table_name
                    .rsplit_once('.')
                    .map_or(table_name, |(_, table)| table);
                let schema = schemas
                    .get(table)
                    .ok_or_else(|| format!("reference expectation names unknown table: {table}"))?;
                let actual = state
                    .committed_table_rows(table)
                    .await
                    .map_err(|_| format!("reference table rows could not be read: {table}"))?;
                if let Some(expected_count) = expected.as_u64() {
                    if actual.len() as u64 != expected_count {
                        return Err(format!("reference row count disagrees for {table}"));
                    }
                } else if expected.is_array() {
                    let expected_rows = reference_expected_rows(schema, expected)?;
                    if actual != expected_rows {
                        return Err(format!("reference rows disagree for {table}"));
                    }
                } else {
                    return Err(format!(
                        "reference expectation has unsupported shape for {table}"
                    ));
                }
                checks.push(format!("exact rows/count matched for {table}"));
            }
        }
    }
    Ok(checks)
}

async fn reference_table_snapshot(
    state: &RuntimeState,
    schemas: &BTreeMap<String, ReferenceTableSchema>,
) -> Result<BTreeMap<String, Vec<(Vec<u8>, Vec<u8>)>>, String> {
    let mut snapshot = BTreeMap::new();
    for table in schemas.keys() {
        snapshot.insert(
            table.clone(),
            state
                .committed_table_rows(table)
                .await
                .map_err(|_| format!("reference table snapshot could not read {table}"))?,
        );
    }
    Ok(snapshot)
}

async fn reference_runtime_snapshot(
    state: &RuntimeState,
    schemas: &BTreeMap<String, ReferenceTableSchema>,
    checkpoint_key: &CheckpointKey,
) -> Result<ReferenceRuntimeSnapshot, String> {
    Ok(ReferenceRuntimeSnapshot {
        tables: reference_table_snapshot(state, schemas).await?,
        checkpoint: Some(
            state
                .stream_checkpoint(checkpoint_key)
                .await
                .map_err(|_| "reference stream checkpoint could not be read".to_owned())?,
        ),
    })
}

fn reference_table_schemas(
    project: &ProjectUnit,
) -> Result<BTreeMap<String, ReferenceTableSchema>, String> {
    let mut schemas = BTreeMap::new();
    for module in &project.modules {
        let parsed = parse_module(&module.source);
        if !parsed.is_ok() {
            return Err("reference project module could not be parsed".into());
        }
        for item in parsed.value.items {
            let Declaration::Table {
                name,
                keys,
                members,
            } = item.declaration
            else {
                continue;
            };
            let mut fields = Vec::new();
            for key in &keys {
                let field = match &key.pattern {
                    Pattern::Name(field, _) => field.clone(),
                    _ => return Err("reference table key is not a named field".into()),
                };
                let ty = key
                    .annotation
                    .as_ref()
                    .and_then(reference_type_name)
                    .ok_or_else(|| "reference table key type is missing".to_owned())?;
                fields.push((field, ty.to_owned()));
            }
            for member in members {
                if let TableMember::Field { name, ty, .. } = member {
                    let ty = reference_type_name(&ty)
                        .ok_or_else(|| "reference table field type is unsupported".to_owned())?;
                    fields.push((name, ty.to_owned()));
                }
            }
            if schemas
                .insert(
                    name,
                    ReferenceTableSchema {
                        fields,
                        key_count: keys.len(),
                    },
                )
                .is_some()
            {
                return Err("reference project declares a duplicate table".into());
            }
        }
    }
    Ok(schemas)
}

fn reference_type_name(ty: &TypeExpr) -> Option<&str> {
    match ty {
        TypeExpr::Name { path, .. } => path.last().map(String::as_str),
        _ => None,
    }
}

fn reference_expected_rows(
    schema: &ReferenceTableSchema,
    expected: &Value,
) -> Result<Vec<ReferenceEncodedRow>, String> {
    let mut rows = Vec::new();
    for row in expected
        .as_array()
        .ok_or_else(|| "reference rows expectation is not an array".to_owned())?
    {
        let values = row
            .as_array()
            .ok_or_else(|| "reference row expectation is not an array".to_owned())?;
        if values.len() != schema.fields.len() {
            return Err("reference row expectation does not match its table schema".into());
        }
        let mut encoded_fields = schema
            .fields
            .iter()
            .zip(values)
            .map(|((name, ty), value)| {
                Ok((
                    OvbRaw::Text(name.clone()),
                    reference_json_value(value, ty)?.raw().clone(),
                ))
            })
            .collect::<Result<Vec<_>, String>>()?;
        encoded_fields.sort_by_cached_key(|(name, _)| {
            CanonicalValue::new(name.clone())
                .expect("reference field name is canonical")
                .encode()
                .expect("reference field name encodes")
        });
        let record = CanonicalValue::new(OvbRaw::Map(encoded_fields))
            .map_err(|_| "reference expected row is not canonical".to_owned())?;
        let key_values = schema
            .fields
            .iter()
            .take(schema.key_count)
            .zip(values)
            .map(|((_, ty), value)| reference_json_value(value, ty))
            .collect::<Result<Vec<_>, String>>()?;
        let key = if key_values.len() == 1 {
            key_values[0].clone()
        } else {
            CanonicalValue::new(OvbRaw::Array(
                key_values.iter().map(|value| value.raw().clone()).collect(),
            ))
            .map_err(|_| "reference expected composite key is not canonical".to_owned())?
        };
        rows.push((
            key.encode()
                .map_err(|_| "reference expected key does not encode".to_owned())?,
            record
                .encode()
                .map_err(|_| "reference expected row does not encode".to_owned())?,
        ));
    }
    rows.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(rows)
}

fn reference_json_value(value: &Value, ty: &str) -> Result<CanonicalValue, String> {
    match ty {
        "Str" => value
            .as_str()
            .map(|text| {
                CanonicalValue::new(OvbRaw::Text(text.to_owned()))
                    .expect("reference text is canonical")
            })
            .ok_or_else(|| "reference string value has the wrong JSON type".to_owned()),
        "Int" => value
            .as_i64()
            .map(|integer| CanonicalValue::int(BigInt::from(integer)))
            .ok_or_else(|| "reference integer value has the wrong JSON type".to_owned()),
        "Decimal" => value
            .as_str()
            .and_then(reference_decimal)
            .ok_or_else(|| "reference decimal value is not canonical text".to_owned()),
        _ => Err(format!("unsupported reference field type: {ty}")),
    }
}

fn reference_decimal(text: &str) -> Option<CanonicalValue> {
    let (mantissa, exponent) = match text.find(['e', 'E']) {
        Some(index) => (&text[..index], text[index + 1..].parse::<i64>().ok()?),
        None => (text, 0),
    };
    let (whole, fraction) = mantissa.split_once('.')?;
    let coefficient = BigInt::parse_bytes(format!("{whole}{fraction}").as_bytes(), 10)?;
    let exponent = exponent.checked_sub(i64::try_from(fraction.len()).ok()?)?;
    CanonicalValue::decimal(coefficient, BigInt::from(exponent)).ok()
}

fn reference_arguments(
    project: &ProjectUnit,
    case: &ProjectNegativeCase,
) -> Result<Environment, String> {
    let (namespace, function) = case
        .invoke
        .split_once('.')
        .ok_or_else(|| "reference negative invocation is not qualified".to_owned())?;
    let module = project
        .modules
        .iter()
        .find(|unit| unit.source_id.ends_with(&format!("/{namespace}.orna")))
        .ok_or_else(|| "reference negative invocation module is missing".to_owned())?;
    let parsed = parse_module(&module.source);
    let signature = parsed
        .value
        .items
        .into_iter()
        .find_map(|item| match item.declaration {
            Declaration::Function { signature, .. } if signature.name == function => {
                Some(signature)
            }
            _ => None,
        })
        .ok_or_else(|| "reference negative invocation function is missing".to_owned())?;
    if signature.parameters.len() != case.args.len() {
        return Err("reference negative invocation argument count disagrees".into());
    }
    signature
        .parameters
        .into_iter()
        .zip(&case.args)
        .map(|(parameter, value)| {
            let name = match parameter.pattern {
                Pattern::Name(name, _) => name,
                _ => return Err("reference negative argument binding is not named".into()),
            };
            Ok((name, reference_untyped_json_value(value)?))
        })
        .collect()
}

fn reference_untyped_json_value(value: &Value) -> Result<CanonicalValue, String> {
    if let Some(text) = value.as_str() {
        return Ok(CanonicalValue::new(OvbRaw::Text(text.to_owned()))
            .expect("reference text is canonical"));
    }
    if let Some(integer) = value.as_i64() {
        return Ok(CanonicalValue::int(BigInt::from(integer)));
    }
    Err("reference negative argument has unsupported JSON type".into())
}

fn reference_stream_checkpoint_key(
    project: &ProjectUnit,
    identity: RuntimeIdentity,
) -> Result<CheckpointKey, String> {
    let module = project
        .modules
        .iter()
        .find(|unit| unit.source_id.ends_with("/sensors.orna"))
        .ok_or_else(|| "reference sensor module is missing".to_owned())?;
    let parsed = parse_module(&module.source);
    let body = parsed
        .value
        .items
        .into_iter()
        .find_map(|item| match item.declaration {
            Declaration::Function { signature, body } if signature.name == "input" => Some(body),
            _ => None,
        })
        .ok_or_else(|| "reference sensor input function is missing".to_owned())?;
    let Expr::Call {
        callee, arguments, ..
    } = body
    else {
        return Err("reference sensor input is not a list source".into());
    };
    if reference_expr_path(&callee) != ["Stream", "from_list"] {
        return Err("reference sensor input uses an unsupported source".into());
    }
    let [values, source] = arguments.as_slice() else {
        return Err("reference sensor input arguments are incomplete".into());
    };
    let Expr::List { elements, .. } = &values.value else {
        return Err("reference sensor input values are not a list".into());
    };
    let source_name = source
        .name
        .as_deref()
        .filter(|name| *name == "source_identity")
        .and_then(|_| match &source.value {
            Expr::Literal { text, kind, .. } if *kind == orna_syntax_v1::LiteralKind::String => {
                reference_string(text)
            }
            _ => None,
        })
        .ok_or_else(|| "reference sensor source identity is missing".to_owned())?;
    let payloads = elements
        .iter()
        .map(reference_literal)
        .collect::<Result<Vec<_>, String>>()?
        .into_iter()
        .map(|value| {
            value
                .encode()
                .map_err(|_| "reference sensor payload is not canonical".to_owned())
        })
        .collect::<Result<Vec<_>, String>>()?;
    let mut digest = Sha256::new();
    digest.update(b"ORNA-LIST-STREAM-IDENTITY\0");
    for payload in &payloads {
        digest.update(
            u64::try_from(payload.len())
                .map_err(|_| "reference sensor payload is too large".to_owned())?
                .to_be_bytes(),
        );
        digest.update(payload);
    }
    let suffix = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let source_identity = format!("{source_name}:{suffix}");
    let component = |value: String| {
        Component::new(value).map_err(|_| "reference stream identity is invalid".to_owned())
    };
    let database = identity
        .database_id
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(CheckpointKey {
        consumer: ConsumerIdentity {
            principal: component(format!("database:{database}"))?,
            root: component("public-function".into())?,
            function: component("sensors.ingest".into())?,
            binding: component("arguments:[]".into())?,
        },
        source_format: component("orna-stream-v1".into())?,
        source: component(source_identity)?,
        partition_format: component("literal-list".into())?,
        partition: None,
        position_format: component("ordinal".into())?,
    })
}

fn reference_expr_path(expr: &Expr) -> Vec<&str> {
    match expr {
        Expr::Name { text, .. } => vec![text],
        Expr::Field { base, name, .. } => {
            let mut path = reference_expr_path(base);
            path.push(name);
            path
        }
        _ => Vec::new(),
    }
}

fn reference_literal(expr: &Expr) -> Result<CanonicalValue, String> {
    match expr {
        Expr::Literal { text, kind, .. } => match kind {
            orna_syntax_v1::LiteralKind::Integer => text
                .parse::<BigInt>()
                .map(CanonicalValue::int)
                .map_err(|_| "reference integer literal is invalid".into()),
            orna_syntax_v1::LiteralKind::Decimal => {
                reference_decimal(text).ok_or_else(|| "reference decimal literal is invalid".into())
            }
            orna_syntax_v1::LiteralKind::String => reference_string(text)
                .map(|text| {
                    CanonicalValue::new(OvbRaw::Text(text)).expect("reference text is canonical")
                })
                .ok_or_else(|| "reference string literal is invalid".into()),
            orna_syntax_v1::LiteralKind::Boolean => {
                Ok(CanonicalValue::new(OvbRaw::Bool(text == "true"))
                    .expect("reference boolean is canonical"))
            }
            orna_syntax_v1::LiteralKind::Null => {
                Ok(CanonicalValue::new(OvbRaw::Null).expect("reference null is canonical"))
            }
            _ => Err("reference literal kind is unsupported".into()),
        },
        Expr::Record { fields, .. } | Expr::Nominal { fields, .. } => {
            let mut encoded = fields
                .iter()
                .map(|field| {
                    Ok((
                        OvbRaw::Text(field.name.clone()),
                        reference_literal(&field.value)?.raw().clone(),
                    ))
                })
                .collect::<Result<Vec<_>, String>>()?;
            encoded.sort_by_cached_key(|(name, _)| {
                CanonicalValue::new(name.clone())
                    .expect("reference field name is canonical")
                    .encode()
                    .expect("reference field name encodes")
            });
            CanonicalValue::new(OvbRaw::Map(encoded))
                .map_err(|_| "reference record literal is not canonical".into())
        }
        _ => Err("reference source payload is not a literal record".into()),
    }
}

fn reference_string(text: &str) -> Option<String> {
    let body = text.strip_prefix('"')?.strip_suffix('"')?;
    (!body.contains('\\')).then(|| body.to_owned())
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
        version: String,
        normative_payload_sha256: BTreeMap<String, String>,
    }
    let release: Release = read_json(root, "release.json")?;
    if release.version != SPECIFICATION_VERSION
        || release.normative_payload_sha256.len() != NORMATIVE_PAYLOAD_COUNT
    {
        return Err(CorpusError(
            "release version or digest inventory is invalid".into(),
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
    requirement_id: String,
    fixture_id: String,
    fixture_path: String,
    stage: Stage,
    implementation_ref: String,
    test_ref: String,
    observed_status: EvidenceStatus,
}

impl EngineWitness {
    #[must_use]
    pub fn requirement_id(&self) -> &str {
        &self.requirement_id
    }
    #[must_use]
    pub fn fixture_id(&self) -> &str {
        &self.fixture_id
    }
    #[must_use]
    pub fn fixture_path(&self) -> &str {
        &self.fixture_path
    }
    #[must_use]
    pub fn stage(&self) -> &Stage {
        &self.stage
    }
    #[must_use]
    pub fn implementation_ref(&self) -> &str {
        &self.implementation_ref
    }
    #[must_use]
    pub fn test_ref(&self) -> &str {
        &self.test_ref
    }
    #[must_use]
    pub fn observed_status(&self) -> &EvidenceStatus {
        &self.observed_status
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct EngineWitnesses {
    publication_digests: BTreeMap<String, String>,
    witnesses: Vec<EngineWitness>,
}

impl EngineWitnesses {
    #[must_use]
    pub fn publication_digests(&self) -> &BTreeMap<String, String> {
        &self.publication_digests
    }
    #[must_use]
    pub fn witnesses(&self) -> &[EngineWitness] {
        &self.witnesses
    }
}

/// A reviewed binding for an executed scenario contract.  Unlike an
/// [`EngineWitness`], this records implementation-scenario execution only;
/// it makes no claim that an Orna engine executed the prose scenario.
#[derive(Debug, Clone, Serialize)]
pub struct ScenarioExecutionBinding {
    pub requirement_id: String,
    pub scenario_id: String,
    pub implementation_ref: String,
    pub test_ref: String,
}

/// Traceable implementation-scenario evidence, pinned to the publication
/// digests in the report that observed it.
#[derive(Debug, Clone, Serialize)]
pub struct ScenarioExecutionWitness {
    requirement_id: String,
    scenario_id: String,
    implementation_ref: String,
    test_ref: String,
    observed_status: EvidenceStatus,
}

impl ScenarioExecutionWitness {
    #[must_use]
    pub fn requirement_id(&self) -> &str {
        &self.requirement_id
    }
    #[must_use]
    pub fn scenario_id(&self) -> &str {
        &self.scenario_id
    }
    #[must_use]
    pub fn implementation_ref(&self) -> &str {
        &self.implementation_ref
    }
    #[must_use]
    pub fn test_ref(&self) -> &str {
        &self.test_ref
    }
    #[must_use]
    pub fn observed_status(&self) -> &EvidenceStatus {
        &self.observed_status
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ScenarioExecutionWitnesses {
    publication_digests: BTreeMap<String, String>,
    witnesses: Vec<ScenarioExecutionWitness>,
}

impl ScenarioExecutionWitnesses {
    #[must_use]
    pub fn publication_digests(&self) -> &BTreeMap<String, String> {
        &self.publication_digests
    }
    #[must_use]
    pub fn witnesses(&self) -> &[ScenarioExecutionWitness] {
        &self.witnesses
    }
}

/// The origin of reviewed implementation evidence.  Only bounded production
/// unit evidence belongs in an [`ImplementationEvidenceOverlay`]; model,
/// skipped, and engine evidence remain deliberately separate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ImplementationEvidenceSource {
    ProductionUnit,
    Model,
    Skipped,
    EngineWitness,
}

/// A reviewed, publication-pinned production-unit result.  This is not an
/// [`EngineWitness`]: it records a bounded implementation result without
/// claiming that an Orna engine executed a corpus fixture.
#[derive(Debug, Clone, Serialize)]
pub struct ImplementationEvidenceBinding {
    pub requirement_id: String,
    pub publication_digests: BTreeMap<String, String>,
    pub implementation_ref: String,
    pub test_ref: String,
    /// The machine-readable origin/classification of this bounded evidence.
    /// Only [`ImplementationEvidenceSource::ProductionUnit`] is admitted.
    pub source: ImplementationEvidenceSource,
    /// The concrete test subject that produced this observation.
    pub subject: String,
    /// The exact command used to observe the result.
    pub command: String,
    /// The recorded result of that command, distinct from its normalized
    /// [`EvidenceStatus`].
    pub result: String,
    pub observed_status: EvidenceStatus,
}

/// Immutable machine-readable bounded production evidence retained by the
/// staging overlay. This is not Orna-engine execution evidence.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct ImplementationEvidence {
    requirement_id: String,
    implementation_ref: String,
    test_ref: String,
    source: ImplementationEvidenceSource,
    subject: String,
    command: String,
    result: String,
    observed_status: EvidenceStatus,
}

impl ImplementationEvidence {
    #[must_use]
    pub fn requirement_id(&self) -> &str {
        &self.requirement_id
    }
    #[must_use]
    pub fn implementation_ref(&self) -> &str {
        &self.implementation_ref
    }
    #[must_use]
    pub fn test_ref(&self) -> &str {
        &self.test_ref
    }
    #[must_use]
    pub fn source(&self) -> &ImplementationEvidenceSource {
        &self.source
    }
    #[must_use]
    pub fn subject(&self) -> &str {
        &self.subject
    }
    #[must_use]
    pub fn command(&self) -> &str {
        &self.command
    }
    #[must_use]
    pub fn result(&self) -> &str {
        &self.result
    }
    #[must_use]
    pub fn observed_status(&self) -> &EvidenceStatus {
        &self.observed_status
    }
}

/// Bounded production-unit traceability.  Its aggregate is intentionally not
/// an execution claim: even passing entries remain only partially executed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ImplementationEvidenceAggregate {
    PartiallyExecuted,
}

/// Reviewed bounded implementation evidence pinned to one exact publication.
/// The API has no conversion to [`EngineWitnesses`] and is deliberately a
/// staging/report input, not an Orna-engine execution claim.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct ImplementationEvidenceOverlay {
    publication_digests: BTreeMap<String, String>,
    evidence: Vec<ImplementationEvidence>,
    aggregate: ImplementationEvidenceAggregate,
}

impl ImplementationEvidenceOverlay {
    #[must_use]
    pub fn publication_digests(&self) -> &BTreeMap<String, String> {
        &self.publication_digests
    }
    #[must_use]
    pub fn evidence(&self) -> &[ImplementationEvidence] {
        &self.evidence
    }
    #[must_use]
    pub fn aggregate(&self) -> ImplementationEvidenceAggregate {
        self.aggregate
    }
}

fn validate_repository_reference(reference: &str, kind: &str) -> Result<(), String> {
    let Some((path, symbol)) = reference.split_once("::") else {
        return Err(format!("invalid repository-relative {kind} reference"));
    };
    let valid_path = !path.is_empty()
        && !path.starts_with('/')
        && !path.starts_with('~')
        && !path.contains(':')
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && path.split('/').all(|segment| {
            !segment.is_empty()
                && segment != "."
                && segment != ".."
                && segment.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
                })
        });
    let valid_symbol = !symbol.is_empty()
        && !symbol.chars().any(char::is_control)
        && symbol.split("::").all(|segment| {
            let mut characters = segment.chars();
            matches!(characters.next(), Some(character) if character.is_ascii_alphabetic() || character == '_')
                && characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
        });
    if !valid_path || !valid_symbol {
        return Err(format!("invalid repository-relative {kind} reference"));
    }
    Ok(())
}

fn validate_evidence_text(value: &str, kind: &str) -> Result<(), String> {
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(format!("invalid implementation evidence {kind}"));
    }
    Ok(())
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
                StageOutcome::Cancelled(_) => "scenario execution was cancelled".into(),
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
        // "mapped" counts only stage rows with an executed outcome. A
        // requirement association on a skipped row remains serialized in
        // `requirement_mapping`, but must not look like executed coverage.
        report.coverage.mapped_stage_evidence = stages
            .clone()
            .filter(|e| {
                e.status != EvidenceStatus::Skipped
                    && matches!(e.requirement_mapping, RequirementMapping::Mapped { .. })
            })
            .count();
        report.coverage.unmapped_stage_evidence = report
            .fixtures
            .iter()
            .flat_map(|fixture| &fixture.stages)
            .filter(|e| matches!(e.requirement_mapping, RequirementMapping::Unmapped { .. }))
            .count();
        // The claim is metadata supplied by the runner, not evidence.  Only
        // scenarios that actually passed through this adapter may appear as
        // executed contracts in the published report.  Derive their order
        // from the frozen corpus rather than caller input so equivalent
        // claims produce byte-for-byte reproducible reports.
        let passed_scenarios = report
            .scenarios
            .iter()
            .filter(|scenario| {
                scenario.class == EvidenceClass::Runtime
                    && scenario.status == EvidenceStatus::Passed
            })
            .map(|scenario| scenario.scenario.as_str())
            .collect::<BTreeSet<_>>();
        let claimed_scenarios = report
            .implementation_claim
            .executed_scenario_contracts
            .iter()
            .filter(|scenario| passed_scenarios.contains(scenario.as_str()))
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let executed = report
            .scenarios
            .iter()
            .filter(|scenario| claimed_scenarios.contains(scenario.scenario.as_str()))
            .map(|scenario| scenario.scenario.clone())
            .collect();
        report.implementation_claim.executed_scenario_contracts = executed;
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
            validate_repository_reference(&binding.implementation_ref, "implementation")?;
            validate_repository_reference(&binding.test_ref, "test")?;
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
        // Reviewed binding order is caller-controlled metadata. Canonicalize
        // the published witness collection so equivalent reviewed input has
        // one reproducible serialization without changing its authority.
        witnesses.sort_by(|left, right| {
            (
                left.requirement_id.as_str(),
                left.fixture_id.as_str(),
                left.stage.phase_name(),
                left.implementation_ref.as_str(),
                left.test_ref.as_str(),
            )
                .cmp(&(
                    right.requirement_id.as_str(),
                    right.fixture_id.as_str(),
                    right.stage.phase_name(),
                    right.implementation_ref.as_str(),
                    right.test_ref.as_str(),
                ))
        });
        Ok(EngineWitnesses {
            publication_digests: report.publication_digests.clone(),
            witnesses,
        })
    }

    /// Bind independently reviewed bounded production-unit evidence without
    /// changing the frozen requirement register or creating engine witnesses.
    /// Every entry is pinned to the complete publication digest set, and the
    /// aggregate remains [`ImplementationEvidenceAggregate::PartiallyExecuted`]
    /// regardless of individual pass/fail observations.
    pub fn implementation_evidence_overlay(
        &self,
        bindings: &[ImplementationEvidenceBinding],
    ) -> Result<ImplementationEvidenceOverlay, String> {
        if bindings.is_empty() {
            return Err("implementation evidence overlay requires at least one binding".into());
        }
        let requirements = self
            .corpus
            .requirements
            .iter()
            .map(|requirement| requirement.id.as_str())
            .collect::<BTreeSet<_>>();
        let mut seen = BTreeSet::new();
        let mut evidence = Vec::with_capacity(bindings.len());
        for binding in bindings {
            if !requirements.contains(binding.requirement_id.as_str()) {
                return Err(format!(
                    "unknown implementation evidence requirement: {}",
                    binding.requirement_id
                ));
            }
            if binding.publication_digests != self.corpus.publication_digests {
                return Err(
                    "implementation evidence publication digests do not match corpus".into(),
                );
            }
            match &binding.source {
                ImplementationEvidenceSource::ProductionUnit => {}
                ImplementationEvidenceSource::Model => {
                    return Err("model evidence cannot enter implementation overlay".into());
                }
                ImplementationEvidenceSource::Skipped => {
                    return Err("skipped evidence cannot enter implementation overlay".into());
                }
                ImplementationEvidenceSource::EngineWitness => {
                    return Err("engine evidence cannot enter implementation overlay".into());
                }
            }
            if !matches!(
                binding.observed_status,
                EvidenceStatus::Passed | EvidenceStatus::Failed
            ) {
                return Err("implementation evidence status must be passed or failed".into());
            }
            validate_repository_reference(&binding.implementation_ref, "implementation")?;
            validate_repository_reference(&binding.test_ref, "test")?;
            validate_evidence_text(&binding.subject, "subject")?;
            validate_evidence_text(&binding.command, "command")?;
            validate_evidence_text(&binding.result, "result")?;
            if !seen.insert((
                binding.requirement_id.as_str(),
                binding.implementation_ref.as_str(),
                binding.test_ref.as_str(),
            )) {
                return Err(format!(
                    "duplicate implementation evidence binding: {}",
                    binding.requirement_id
                ));
            }
            evidence.push(ImplementationEvidence {
                requirement_id: binding.requirement_id.clone(),
                implementation_ref: binding.implementation_ref.clone(),
                test_ref: binding.test_ref.clone(),
                source: binding.source.clone(),
                subject: binding.subject.clone(),
                command: binding.command.clone(),
                result: binding.result.clone(),
                observed_status: binding.observed_status.clone(),
            });
        }
        // Production-unit evidence is not an engine claim. Its ordering is
        // nevertheless part of its serialized report surface, so normalize
        // caller order after validation.
        evidence.sort_by(|left, right| {
            (
                left.requirement_id.as_str(),
                left.implementation_ref.as_str(),
                left.test_ref.as_str(),
            )
                .cmp(&(
                    right.requirement_id.as_str(),
                    right.implementation_ref.as_str(),
                    right.test_ref.as_str(),
                ))
        });
        Ok(ImplementationEvidenceOverlay {
            publication_digests: self.corpus.publication_digests.clone(),
            evidence,
            aggregate: ImplementationEvidenceAggregate::PartiallyExecuted,
        })
    }

    /// Convert reviewed bindings into traceability evidence only when the
    /// exact corpus scenario is declared and passed in this report.  This
    /// boundary deliberately does not produce engine witnesses: the corpus
    /// labels scenarios as implementation scenarios rather than executions
    /// by an Orna engine.
    pub fn scenario_execution_witnesses(
        &self,
        report: &RunReport,
        bindings: &[ScenarioExecutionBinding],
    ) -> Result<ScenarioExecutionWitnesses, String> {
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
            let scenario = self.corpus.scenarios["scenarios"]
                .as_array()
                .expect("validated scenarios")
                .iter()
                .find(|value| value["id"].as_str() == Some(binding.scenario_id.as_str()))
                .cloned()
                .map(|value| serde_json::from_value::<Scenario>(value).expect("validated scenario"))
                .ok_or_else(|| format!("unknown scenario binding: {}", binding.scenario_id))?;
            if scenario.evidence_level != "implementation scenario, not executed by an Orna engine"
                || !scenario.requirements.contains(&binding.requirement_id)
            {
                return Err(format!(
                    "scenario requirement mismatch: {}",
                    binding.scenario_id
                ));
            }
            if !report
                .implementation_claim
                .executed_scenario_contracts
                .contains(&binding.scenario_id)
            {
                return Err(format!(
                    "scenario is not declared executed: {}",
                    binding.scenario_id
                ));
            }
            if !seen.insert((
                binding.requirement_id.as_str(),
                binding.scenario_id.as_str(),
            )) {
                return Err(format!(
                    "duplicate scenario binding: {}",
                    binding.scenario_id
                ));
            }
            validate_repository_reference(&binding.implementation_ref, "implementation")?;
            validate_repository_reference(&binding.test_ref, "test")?;
            let result = report
                .scenarios
                .iter()
                .find(|result| result.scenario == binding.scenario_id)
                .ok_or_else(|| {
                    format!("scenario is absent from report: {}", binding.scenario_id)
                })?;
            if result.status != EvidenceStatus::Passed
                || result.class != EvidenceClass::Runtime
                || !result.requirements.contains(&binding.requirement_id)
            {
                return Err(format!(
                    "scenario is not executed evidence: {}",
                    binding.scenario_id
                ));
            }
            witnesses.push(ScenarioExecutionWitness {
                requirement_id: binding.requirement_id.clone(),
                scenario_id: binding.scenario_id.clone(),
                implementation_ref: binding.implementation_ref.clone(),
                test_ref: binding.test_ref.clone(),
                observed_status: result.status.clone(),
            });
        }
        // Scenario witnesses remain implementation-scenario evidence, not
        // engine execution. Canonical ordering makes equivalent approved
        // binding sets reproducible without promoting their status.
        witnesses.sort_by(|left, right| {
            (
                left.requirement_id.as_str(),
                left.scenario_id.as_str(),
                left.implementation_ref.as_str(),
                left.test_ref.as_str(),
            )
                .cmp(&(
                    right.requirement_id.as_str(),
                    right.scenario_id.as_str(),
                    right.implementation_ref.as_str(),
                    right.test_ref.as_str(),
                ))
        });
        Ok(ScenarioExecutionWitnesses {
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
        // Requirement mappings are resolved per stage. A fixture-level link
        // cannot prove which parser/checker/runtime obligation was exercised.
        // Expected `not-run` stages remain visible without acquiring a
        // blanket fixture mapping.
        for stage in stages {
            let mapping = self.requirement_mapping(fixture, &stage);
            let requirements = match &mapping {
                RequirementMapping::Mapped { requirements } => requirements.clone(),
                RequirementMapping::Unmapped { .. } => Vec::new(),
            };
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
                StageOutcome::Cancelled(_) => true,
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
                requirement_mapping: mapping,
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
    fn requirement_mapping(&self, fixture: &Fixture, stage: &Stage) -> RequirementMapping {
        let source_requirements = self
            .corpus
            .requirements
            .iter()
            .map(|requirement| requirement.id.as_str())
            .collect::<BTreeSet<_>>();
        let mut linked = Vec::new();
        let mut saw_fixture_link_without_stage = false;
        let mut seen = BTreeSet::new();
        let mut saw_malformed_link = false;
        for entry in &self.corpus.requirement_evidence.requirements {
            if !source_requirements.contains(entry.requirement.as_str()) {
                continue;
            }
            for test in &entry.tests {
                let fixture_matches = match (
                    test.get("fixture").and_then(Value::as_str),
                    test.get("path").and_then(Value::as_str),
                ) {
                    (Some(fixture_id), Some(path)) => {
                        fixture_id == fixture.id && path == fixture.path
                    }
                    (Some(fixture_id), None) => fixture_id == fixture.id,
                    (None, Some(path)) => path == fixture.path,
                    (None, None) => false,
                };
                if !fixture_matches {
                    continue;
                }
                if let Some(subject) = test.get("subject") {
                    if subject.as_str() != Some(entry.requirement.as_str()) {
                        saw_malformed_link = true;
                        continue;
                    }
                }
                let stage_values = match evidence_test_stages(test) {
                    Ok(Some(stages)) => stages,
                    Ok(None) => {
                        saw_fixture_link_without_stage = true;
                        continue;
                    }
                    Err(_) => {
                        saw_malformed_link = true;
                        continue;
                    }
                };
                if stage_values.iter().any(|value| {
                    *value == stage.expectation_key() || *value == stage.phase_name()
                }) && seen.insert(entry.requirement.clone())
                {
                    linked.push(entry.requirement.clone());
                }
            }
        }
        if linked.is_empty() {
            let reason = if saw_malformed_link {
                format!(
                    "authoritative requirement-evidence fixture/path link is malformed for stage: {}",
                    stage.phase_name()
                )
            } else if saw_fixture_link_without_stage {
                format!(
                    "authoritative requirement-evidence fixture/path link does not identify stage: {}",
                    stage.phase_name()
                )
            } else {
                format!(
                    "authoritative requirement-evidence/tests contains no fixture/path link for stage: {}",
                    stage.phase_name()
                )
            };
            RequirementMapping::Unmapped { reason }
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

fn evidence_test_stages(test: &Value) -> Result<Option<Vec<&str>>, &'static str> {
    let Some(object) = test.as_object() else {
        return Err("requirement evidence test must be an object");
    };
    if object.contains_key("stage") && object.contains_key("stages") {
        return Err("requirement evidence stage and stages are mutually exclusive");
    }
    if let Some(stage) = object.get("stage") {
        let Some(stage) = stage.as_str() else {
            return Err("requirement evidence stage must be a string");
        };
        if !is_evidence_stage(stage) {
            return Err("requirement evidence stage is unknown");
        }
        return Ok(Some(vec![stage]));
    }
    let Some(stages) = object.get("stages") else {
        return Ok(None);
    };
    let Some(stages) = stages.as_array() else {
        return Err("requirement evidence stages must be an array");
    };
    if stages.is_empty() {
        return Err("requirement evidence stages must contain known stages");
    }
    let mut names = Vec::with_capacity(stages.len());
    for stage in stages {
        let Some(stage) = stage.as_str() else {
            return Err("requirement evidence stages must contain strings");
        };
        if !is_evidence_stage(stage) {
            return Err("requirement evidence stages must contain known stages");
        }
        names.push(stage);
    }
    Ok(Some(names))
}

fn is_evidence_stage(stage: &str) -> bool {
    matches!(
        stage,
        "parse" | "resolve" | "typecheck" | "evaluate" | "load_rows" | "row-validation"
    )
}

fn validate_requirement_evidence_test(
    test: &Value,
    requirement: &str,
) -> Result<(), CorpusError> {
    let object = test
        .as_object()
        .ok_or_else(|| CorpusError("requirement evidence test must be an object".into()))?;
    if let Some(subject) = object.get("subject") {
        if subject.as_str() != Some(requirement) {
            return Err(CorpusError(
                "requirement evidence test subject must match requirement".into(),
            ));
        }
    }
    evidence_test_stages(test)
        .map(|_| ())
        .map_err(|message| CorpusError(message.into()))
}


/// Reports expose only the diagnostic identity.  Adapter diagnostics may have
/// spans, labels or native payloads containing source observations; preserving
/// any of those would let a source-bearing adapter bypass the logical-only
/// conformance boundary.
fn failed_diagnostic<D: Serialize>(outcome: &StageOutcome<D>) -> Option<Value> {
    let diagnostic = match outcome {
        StageOutcome::Failed(diagnostic) | StageOutcome::Cancelled(diagnostic) => diagnostic,
        _ => return None,
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

#[cfg(test)]
mod authority_tests {
    use super::*;

    fn write_release(root: &Path, version: &str, count: usize) {
        let digests = (0..count)
            .map(|index| (format!("member-{index}"), "0".repeat(64)))
            .collect::<BTreeMap<_, _>>();
        fs::write(
            root.join("release.json"),
            serde_json::to_vec(&serde_json::json!({
                "version": version,
                "normative_payload_sha256": digests,
            }))
            .expect("release JSON serializes"),
        )
        .expect("release JSON writes");
    }

    #[test]
    fn release_metadata_is_pinned_to_the_authoritative_1_0_0_inventory() {
        let root = tempfile::tempdir().expect("temporary authority root");
        write_release(root.path(), "0.9.0", NORMATIVE_PAYLOAD_COUNT);
        assert_eq!(
            release_digests(root.path())
                .expect_err("wrong release version")
                .to_string(),
            "release version or digest inventory is invalid"
        );

        write_release(
            root.path(),
            SPECIFICATION_VERSION,
            NORMATIVE_PAYLOAD_COUNT - 1,
        );
        assert_eq!(
            release_digests(root.path())
                .expect_err("incomplete digest inventory")
                .to_string(),
            "release version or digest inventory is invalid"
        );
    }
}
