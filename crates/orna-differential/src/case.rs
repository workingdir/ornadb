//! The corpus: every program the repository owns, as [`Case`] data.
//!
//! `docs/design/hir.md` section 6.2 names the sources. Today's tests hold the
//! knowledge of how each fixture is driven in Rust code; this module lifts the
//! families that are already data-driven into [`Case`] values, and records the
//! rest in the exclusion ratchet (`tests/differential/excluded.toml`) with the
//! reason they are not loaded yet.
//!
//! # The account
//!
//! [`Corpus::load`] walks every `.orna` file under `crates/` and `stdlib/`,
//! subtracts the files it turned into cases, and requires the remainder to be
//! covered exactly once by the exclusion scopes, with the file count each scope
//! records. A new fixture therefore cannot be silently ignored: it either
//! becomes a case or fails the account until its family is listed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use orna_evaluator::{Environment, Limits};
use orna_syntax::{Declaration, SyntaxTree, Visibility, parse_module_with_file};
use serde::Deserialize;

use crate::{differential_dir, read_file, repository_root, toml_lite};

/// The vendored reference corpus the conformance harness reads.
///
/// `crates/orna-conformance/tests/fixtures/reference` is the checked-in copy of
/// `reference/Orna-1.1.0`, including the manifest that records each fixture's
/// published expectation. The other crates' vendored copies are excluded: they
/// hold the same programs, and one case per program is what parity needs.
pub const REFERENCE_ROOT: &str = "crates/orna-conformance/tests/fixtures/reference";

/// The pinned standard library sources, as the standard dependency bundle.
pub const STANDARD_ROOT: &str = "stdlib/std";

/// Evaluator fixture directories that hold one expression per file.
pub const EXPRESSION_DIRS: [&str; 5] = [
    "crates/orna-evaluator/tests/fixtures/expressions_conformance",
    "crates/orna-evaluator/tests/fixtures/and_or_precedence",
    "crates/orna-evaluator/tests/fixtures/power_operator",
    "crates/orna-evaluator/tests/fixtures/wildcard_structured_lambda",
    "crates/orna-evaluator/tests/fixtures/anonymous_pipeline_stages",
];

/// The directory whose fixtures submit one REPL input per non-empty line.
pub const REPL_ADMISSION_DIR: &str = "crates/orna-conformance/tests/fixtures/repl-admission";

/// The source family a case belongs to.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Family {
    /// A reference corpus fixture: a module with a published expectation.
    Reference,
    /// One expression from an evaluator fixture.
    Expression,
    /// An ordered list of REPL inputs.
    Repl,
    /// One pinned `std` module.
    Standard,
}

impl Family {
    /// The stable name this family is counted and reported under.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::Reference => "reference",
            Self::Expression => "expression",
            Self::Repl => "repl",
            Self::Standard => "standard",
        }
    }
}

/// How a case is driven.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaseKind {
    /// `admit_module` and `evaluate_staged` against an
    /// [`ApplicationAuthority`](orna_application::ApplicationAuthority).
    Module,
    /// `evaluate_expression`, the evaluator's expression entry point.
    Expression,
    /// An [`AdmittedReplSession`](orna_project_repl::AdmittedReplSession).
    Repl,
    /// The pinned standard dependency bundle, admitted as one catalogue.
    StandardBundle,
}

/// One source file a case is built from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Source {
    /// Repository-relative path of the file on disk.
    pub path: String,
    /// The logical path the module is admitted under.
    pub logical_path: String,
    /// The source text.
    pub text: String,
}

/// Table rows the activation needs.
///
/// The container exists because section 6.2 lists the rows as part of a case;
/// no loaded family needs rows yet, and the families that do (row admission,
/// loose rows) are excluded with that reason until the store path is wired.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableRows {
    /// Table name as the admitted schema spells it.
    pub table: String,
    /// Canonical OVB-1 row bytes, in key order.
    pub rows: Vec<Vec<u8>>,
}

/// A published expectation for a case, when the corpus publishes one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Expectation {
    /// Whether the program is expected to be accepted.
    pub accept: bool,
    /// The published primary diagnostic code of a rejected program.
    pub code: Option<String>,
    /// The phase that rejects it: `parse`, `resolve` or `typecheck`.
    pub phase: Option<String>,
}

/// One program, in the form both pipelines are driven from.
#[derive(Clone, Debug)]
pub struct Case {
    /// Repository-relative identity, stable across runs.
    pub id: String,
    /// The family the case came from.
    pub family: Family,
    /// How the case is driven.
    pub kind: CaseKind,
    /// The case's sources, primary first.
    pub sources: Vec<Source>,
    /// The entry function of a module case, when it has one.
    pub entry: Option<String>,
    /// The arguments bound before evaluation (the case's environment).
    pub arguments: Environment,
    /// Whether the observer evaluates after admission.
    pub evaluate: bool,
    /// The inputs of a REPL case, submitted in order.
    pub inputs: Vec<String>,
    /// The limits the case is driven with.
    pub limits: Limits,
    /// The table rows the activation needs.
    pub rows: Vec<TableRows>,
    /// The published expectation, when the corpus publishes one.
    pub expectation: Option<Expectation>,
}

impl Case {
    /// Builds a module case from one source file.
    #[must_use]
    pub fn module(id: String, family: Family, source: Source) -> Self {
        let (entry, evaluates) = entry_of(&source);
        Self {
            id,
            family,
            kind: CaseKind::Module,
            sources: vec![source],
            entry,
            arguments: Environment::new(),
            evaluate: evaluates,
            inputs: Vec::new(),
            limits: Limits::default(),
            rows: Vec::new(),
            expectation: None,
        }
    }

    /// Builds an expression case from one fixture file.
    #[must_use]
    pub fn expression(source: Source) -> Self {
        Self {
            id: source.path.clone(),
            family: Family::Expression,
            kind: CaseKind::Expression,
            sources: vec![source],
            entry: None,
            arguments: Environment::new(),
            evaluate: true,
            inputs: Vec::new(),
            limits: Limits::default(),
            rows: Vec::new(),
            expectation: None,
        }
    }

    /// Builds a REPL case from an ordered list of inputs.
    #[must_use]
    pub fn repl(path: String, source: Source, inputs: Vec<String>) -> Self {
        Self {
            id: path,
            family: Family::Repl,
            kind: CaseKind::Repl,
            sources: vec![source],
            entry: None,
            arguments: Environment::new(),
            evaluate: true,
            inputs,
            limits: Limits::default(),
            rows: Vec::new(),
            expectation: None,
        }
    }

    /// Builds the standard dependency bundle case from its modules.
    #[must_use]
    pub fn standard_bundle(sources: Vec<Source>) -> Self {
        Self {
            id: STANDARD_ROOT.to_owned(),
            family: Family::Standard,
            kind: CaseKind::StandardBundle,
            sources,
            entry: None,
            arguments: Environment::new(),
            evaluate: true,
            inputs: Vec::new(),
            limits: Limits::default(),
            rows: Vec::new(),
            expectation: None,
        }
    }

    /// Sets the published expectation.
    #[must_use]
    pub fn with_expectation(mut self, expectation: Expectation) -> Self {
        self.expectation = Some(expectation);
        self
    }
}

/// One exclusion scope: a directory of programs the loader does not load.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExcludedScope {
    /// Repository-relative directory the scope covers.
    pub scope: String,
    /// How many unloaded `.orna` files the scope holds.
    pub files: usize,
    /// Why they are not loaded.
    pub reason: String,
}

/// The loaded corpus and its exclusion account.
#[derive(Clone, Debug)]
pub struct Corpus {
    /// Every case, in family then path order.
    pub cases: Vec<Case>,
    /// The exclusion scopes, in path order.
    pub excluded: Vec<ExcludedScope>,
}

impl Corpus {
    /// Loads every family and reconciles them with the exclusion ratchet.
    pub fn load() -> Result<Self, String> {
        let root = repository_root();
        let mut cases = load_reference(&root)?;
        cases.extend(load_expressions(&root)?);
        cases.extend(load_repl(&root)?);
        cases.extend(load_standard(&root)?);
        cases.sort_by(|left, right| (left.family, &left.id).cmp(&(right.family, &right.id)));
        check_unique_ids(&cases)?;

        let population = populate(&root)?;
        let excluded = load_exclusions()?;
        reconcile(&cases, &population, &excluded)?;
        Ok(Self { cases, excluded })
    }

    /// Returns how many cases each family holds.
    #[must_use]
    pub fn family_counts(&self) -> BTreeMap<&'static str, usize> {
        let mut counts = BTreeMap::new();
        for case in &self.cases {
            *counts.entry(case.family.id()).or_insert(0usize) += 1;
        }
        counts
    }

    /// Returns the case with `id`.
    #[must_use]
    pub fn case(&self, id: &str) -> Option<&Case> {
        self.cases.iter().find(|case| case.id == id)
    }
}

/// The reference manifest, read for the fields the harness uses.
#[derive(Debug, Deserialize)]
struct Manifest {
    fixtures: Vec<Fixture>,
}

#[derive(Debug, Deserialize)]
struct Fixture {
    id: String,
    kind: String,
    path: String,
    parse_as: String,
    #[serde(default)]
    failing_phase: Option<String>,
    #[serde(default)]
    diagnostic: Option<String>,
}

#[derive(Debug, Deserialize)]
struct InvalidMetadata {
    fixtures: Vec<InvalidFixture>,
}

#[derive(Debug, Deserialize)]
struct InvalidFixture {
    path: String,
    failing_phase: String,
    diagnostic: String,
}

/// Loads `examples/valid` and `examples/invalid` as module cases.
///
/// Each case carries the expectation the published manifest records, so the
/// corpus is the reference semantics and not a private opinion about it. The
/// database project under `examples/final` and the multi-module project under
/// `examples/reference` need project scaffolding; the exclusion ratchet
/// accounts for them.
fn load_reference(root: &Path) -> Result<Vec<Case>, String> {
    let reference = root.join(REFERENCE_ROOT);
    let manifest: Manifest = read_json(&reference, "tests/conformance-manifest.json")?;
    let invalid: InvalidMetadata = read_json(&reference, "tests/invalid-metadata.json")?;
    let invalid_codes = invalid
        .fixtures
        .into_iter()
        .map(|fixture| (fixture.path, (fixture.failing_phase, fixture.diagnostic)))
        .collect::<BTreeMap<_, _>>();
    let mut cases = Vec::new();
    for fixture in manifest.fixtures {
        let published = fixture.path.starts_with("examples/valid/")
            || fixture.path.starts_with("examples/invalid/");
        if !published || fixture.parse_as != "module_unit" {
            continue;
        }
        let text = read_file(&reference.join(&fixture.path))?;
        let source = Source {
            path: format!("{REFERENCE_ROOT}/{}", fixture.path),
            logical_path: fixture.path.clone(),
            text,
        };
        let expectation = reference_expectation(&fixture, &invalid_codes);
        cases.push(
            Case::module(fixture.id, Family::Reference, source).with_expectation(expectation),
        );
    }
    Ok(cases)
}

fn reference_expectation(
    fixture: &Fixture,
    invalid_codes: &BTreeMap<String, (String, String)>,
) -> Expectation {
    if fixture.kind != "invalid" {
        return Expectation {
            accept: true,
            code: None,
            phase: None,
        };
    }
    let recorded = invalid_codes.get(&fixture.path);
    Expectation {
        accept: false,
        code: recorded
            .map(|(_, code)| code.clone())
            .or_else(|| fixture.diagnostic.clone()),
        phase: recorded
            .map(|(phase, _)| phase.clone())
            .or_else(|| fixture.failing_phase.clone()),
    }
}

/// Loads one expression per file from the evaluator's expression fixtures.
fn load_expressions(root: &Path) -> Result<Vec<Case>, String> {
    let mut cases = Vec::new();
    for directory in EXPRESSION_DIRS {
        for path in walk(root, directory)? {
            let text = read_file(&root.join(&path))?;
            let source = Source {
                path: path.clone(),
                logical_path: path,
                text,
            };
            cases.push(Case::expression(source));
        }
    }
    Ok(cases)
}

/// Loads the REPL fixtures as ordered input lists.
///
/// A fixture is one input per non-empty line, the shape
/// `crates/orna-conformance/tests/repl_admission.rs` submits.
fn load_repl(root: &Path) -> Result<Vec<Case>, String> {
    let mut paths = walk(root, REPL_ADMISSION_DIR)?;
    for path in walk(root, "crates/orna-evaluator/tests/fixtures")? {
        if let Some(name) = path.rsplit('/').next()
            && name.starts_with("admitted_repl_")
        {
            paths.push(path);
        }
    }
    paths.sort();
    let mut cases = Vec::new();
    for path in paths {
        let text = read_file(&root.join(&path))?;
        let inputs = inputs_of(&text);
        if inputs.is_empty() {
            continue;
        }
        let source = Source {
            path: path.clone(),
            logical_path: path.clone(),
            text,
        };
        cases.push(Case::repl(path, source, inputs));
    }
    Ok(cases)
}

/// Loads the pinned standard dependency bundle as one case.
///
/// `std` is a reserved root for project source: admitting one standard module
/// through `admit_module` is refused with `ORNA-S003-RESERVED`, and the binary
/// admits the bundle as a whole through `Catalogue::with_standard_sources`
/// (which verifies the profile and derives every declaration). So the corpus
/// case is that admission, over every module the bundle declares, and the
/// per-function entries of section 6.2 arrive when the REPL can call declared
/// standard functions.
fn load_standard(root: &Path) -> Result<Vec<Case>, String> {
    let mut sources = Vec::new();
    for logical_path in orna_standard_sources::reference_standard_sources_v1()
        .into_iter()
        .map(|(logical_path, _)| logical_path)
    {
        let path = format!("stdlib/{logical_path}");
        let text = read_file(&root.join(&path))?;
        sources.push(Source {
            path,
            logical_path,
            text,
        });
    }
    Ok(vec![Case::standard_bundle(sources)])
}

/// Splits a REPL fixture into its submitted inputs.
fn inputs_of(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("//"))
        .map(str::to_owned)
        .collect()
}

/// Returns the entry a module case calls, and whether it can be evaluated.
///
/// The reference corpus does not name its entry function, so the loader takes
/// the rule the corpus's own tests follow: a public zero-parameter function,
/// `main` first. A module without one is admitted through its first function
/// and not evaluated, because the harness supplies no arguments.
fn entry_of(source: &Source) -> (Option<String>, bool) {
    let parsed = parse_module_with_file(&source.text, source.logical_path.clone());
    if !parsed.is_ok() {
        return (None, false);
    }
    match ordered_functions(&parsed.value).first() {
        Some((name, parameters)) => (Some(name.clone()), *parameters == 0),
        None => (None, false),
    }
}

/// Returns `(name, parameter count)` for each function, ordered by the entry
/// preference: public before private, zero parameters first, `main` first.
fn ordered_functions(tree: &SyntaxTree) -> Vec<(String, usize)> {
    let mut functions = Vec::new();
    for item in &tree.items {
        if let Declaration::Function { signature, .. } = &item.declaration {
            let public = matches!(item.visibility, Visibility::Public { .. });
            functions.push((signature.name.clone(), signature.parameters.len(), !public));
        }
    }
    functions
        .sort_by_key(|(name, parameters, private)| (*private, *parameters != 0, name != "main"));
    functions
        .into_iter()
        .map(|(name, parameters, _)| (name, parameters))
        .collect()
}

/// Reads one JSON document from the reference corpus.
fn read_json<T: for<'de> Deserialize<'de>>(root: &Path, relative: &str) -> Result<T, String> {
    let text = read_file(&root.join(relative))?;
    serde_json::from_str(&text).map_err(|error| format!("{relative}: {error}"))
}

/// Walks `root/relative` and returns repository-relative paths of `.orna` files.
fn walk(root: &Path, relative: &str) -> Result<Vec<String>, String> {
    let mut files = Vec::new();
    collect_orna(root, relative, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_orna(root: &Path, relative: &str, files: &mut Vec<String>) -> Result<(), String> {
    let path = root.join(relative);
    let entries =
        std::fs::read_dir(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("{}: {error}", path.display()))?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|name| format!("{}: non-UTF-8 name {name:?}", path.display()))?;
        let child = format!("{relative}/{name}");
        let metadata = entry
            .metadata()
            .map_err(|error| format!("{}: {error}", entry.path().display()))?;
        if metadata.is_dir() {
            collect_orna(root, &child, files)?;
        } else if name.ends_with(".orna") {
            files.push(child);
        }
    }
    Ok(())
}

/// Every `.orna` file the repository owns, under the two roots that hold them.
fn populate(root: &Path) -> Result<Vec<String>, String> {
    let mut files = walk(root, "crates")?;
    files.extend(walk(root, STANDARD_ROOT)?);
    files.sort();
    Ok(files)
}

/// Reads the exclusion ratchet.
fn load_exclusions() -> Result<Vec<ExcludedScope>, String> {
    let path = differential_dir().join("excluded.toml");
    let document = toml_lite::parse(&read_file(&path)?)?;
    let mut scopes = Vec::new();
    for table in document.tables_named("excluded") {
        let files = table.integer("files")?;
        let files =
            usize::try_from(files).map_err(|_| format!("{}: negative `files`", table.name))?;
        let scope = table.string("scope")?.to_owned();
        let reason = table.string("reason")?.to_owned();
        if reason.trim().is_empty() {
            return Err(format!("{scope}: the exclusion needs a reason"));
        }
        scopes.push(ExcludedScope {
            scope,
            files,
            reason,
        });
    }
    scopes.sort_by(|left, right| left.scope.cmp(&right.scope));
    Ok(scopes)
}

fn check_unique_ids(cases: &[Case]) -> Result<(), String> {
    let mut ids = BTreeSet::new();
    for case in cases {
        if !ids.insert(case.id.as_str()) {
            return Err(format!("duplicate case id {}", case.id));
        }
    }
    Ok(())
}

/// Requires every program to be either a case source or inside exactly one
/// exclusion scope whose recorded count matches.
fn reconcile(
    cases: &[Case],
    population: &[String],
    excluded: &[ExcludedScope],
) -> Result<(), String> {
    let consumed = cases
        .iter()
        .flat_map(|case| case.sources.iter())
        .map(|source| source.path.as_str())
        .collect::<BTreeSet<_>>();
    let mut errors = Vec::new();
    let mut counted = BTreeMap::<&str, usize>::new();
    for program in population {
        if consumed.contains(program.as_str()) {
            continue;
        }
        let covering = excluded
            .iter()
            .filter(|scope| program.starts_with(&format!("{}/", scope.scope)))
            .collect::<Vec<_>>();
        match covering.as_slice() {
            [scope] => *counted.entry(scope.scope.as_str()).or_insert(0) += 1,
            [] => errors.push(format!("{program}: neither loaded nor excluded")),
            _ => errors.push(format!(
                "{program}: covered by more than one exclusion scope ({})",
                covering
                    .iter()
                    .map(|scope| scope.scope.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }
    for scope in excluded {
        let measured = counted.get(scope.scope.as_str()).copied().unwrap_or(0);
        if measured != scope.files {
            errors.push(format!(
                "{}: scope records {} excluded files, the checkout holds {measured}; update \
                 tests/differential/excluded.toml in the same PR that changes the family",
                scope.scope, scope.files
            ));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("\n"))
    }
}
