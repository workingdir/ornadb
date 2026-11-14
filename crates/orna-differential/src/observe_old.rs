//! The old pipeline's observer.
//!
//! For each case it drives the same entry points the shipped binary drives
//! (`docs/design/hir.md` section 6.2):
//!
//! * a module case: [`ApplicationAuthority::admit_module`] and
//!   [`ApplicationAuthority::evaluate_staged`];
//! * a REPL case: [`AdmittedReplSession::submit`];
//! * an expression case: `evaluate_expression`'s path, with a caller-owned
//!   [`StepBudget`] so one run also yields the step count.
//!
//! # Step counts
//!
//! An expression case runs once under a caller-owned budget, so its count is
//! exact. Admission takes no budget, so a module case is measured the way
//! `crates/orna-evaluator/tests/std_intrinsic_step_parity.rs` measures one: the
//! smallest `max_steps` that reproduces the unbounded run's outcome, found by
//! doubling then bisection. The predicate is monotone because a budget can only
//! turn a result into `ORNA-EVAL-LIMIT` and never the reverse.
//!
//! A REPL session owns its budget and exposes no spent count, and a budget on a
//! multi-input session is not monotone (a budget failure changes the state later
//! inputs see), so REPL cases record no step count.

use std::sync::LazyLock;

use orna_application::{ApplicationAuthority, ApplicationError, StagedActivation};
use orna_check::{Catalogue, StandardDependencyProfile};
use orna_evaluator::route_trace::{self, CallKind};
use orna_evaluator::{Functions, Limits, StepBudget, evaluate_with_functions_and_budget};
use orna_project_repl::AdmittedReplSession;
use orna_syntax::parse_expression;
use orna_value::CanonicalValue;

use crate::case::{Case, CaseKind, Family, Source};
use crate::{CallRecord, InputOutcome, Observation, Route, Write};

/// The step budget every unbounded probe runs with.
///
/// Large enough that no corpus program reaches it (the whole corpus is small
/// programs; section 6.2) and small enough that the evaluator's step counter
/// cannot overflow on a pathological fixture.
const STEP_CEILING: u64 = 1 << 30;

/// The evaluator's code for an exhausted budget or another crossed limit.
const LIMIT_CODE: &str = "ORNA-EVAL-LIMIT";

/// Runs one case through the old pipeline.
pub fn observe(case: &Case) -> Result<Observation, String> {
    if !case.rows.is_empty() {
        return Err(format!(
            "{}: the case needs table rows, and the H0 store path supplies none",
            case.id
        ));
    }
    match case.kind {
        CaseKind::Module => observe_module(case),
        CaseKind::Expression => observe_expression(case),
        CaseKind::Repl => observe_repl(case),
        CaseKind::StandardBundle => observe_standard_bundle(case),
    }
}

/// What one module run produced, before the step measurement is attached.
#[derive(Default)]
struct Run {
    diagnostics: Vec<String>,
    outcome: Option<Result<Vec<u8>, String>>,
    writes: Vec<Write>,
    /// The calls the run executed, in execution order.
    calls: Vec<CallRecord>,
    /// Whether the evaluator ran. An admission rejection produces an outcome
    /// without executing anything, and such a case has no step count.
    evaluated: bool,
}

/// Observes a module case through admission and staged evaluation.
fn observe_module(case: &Case) -> Result<Observation, String> {
    let source = primary(case)?;
    let catalogue = catalogue_for(case)?;
    let limits = Limits {
        max_steps: STEP_CEILING,
        ..case.limits
    };
    let run = run_module(case, source, catalogue, limits)?;
    let steps = match (&run.outcome, run.evaluated) {
        (Some(outcome), true) if !is_other_limit(outcome) => {
            measure_steps(case, source, catalogue, outcome)?
        }
        _ => None,
    };
    Ok(Observation {
        diagnostics: run.diagnostics,
        outcome: run.outcome,
        steps,
        calls: run.calls,
        writes: run.writes,
        inputs: Vec::new(),
    })
}

/// Admits, and when the case is evaluable stages, one module at `limits`.
fn run_module(
    case: &Case,
    source: &Source,
    catalogue: &Catalogue,
    limits: Limits,
) -> Result<Run, String> {
    let authority = ApplicationAuthority::new(catalogue.clone(), limits);
    let entry = case.entry.clone().unwrap_or_default();
    let (outcome, calls) = route_trace::collect(|| {
        let admitted =
            match authority.admit_module(source.logical_path.clone(), source.text.clone(), entry) {
                Ok(application) => application,
                Err(error) => return Ok::<Run, String>(rejection(&error, case.entry.is_some())),
            };
        if !case.evaluate {
            return Ok(Run::default());
        }
        match authority.evaluate_staged(&admitted, &case.arguments) {
            Ok(activation) => Ok(Run {
                outcome: Some(Ok(encode(activation.value())?)),
                writes: writes_of(&activation),
                evaluated: true,
                ..Run::default()
            }),
            Err(error) => Ok(Run {
                outcome: Some(Err(application_code(&error))),
                evaluated: true,
                ..Run::default()
            }),
        }
    });
    let mut run = outcome?;
    run.calls = calls
        .into_iter()
        .map(|record| CallRecord {
            logical_path: record.span.location.as_ref().map_or_else(
                || source.logical_path.clone(),
                |location| location.file.clone(),
            ),
            start: record.span.start,
            end: record.span.end,
            route: route_of(record.kind),
        })
        .collect();
    Ok(run)
}

/// The differential route a cascade branch records.
fn route_of(kind: CallKind) -> Route {
    match kind {
        CallKind::DecimalDivide => Route::DecimalDivide,
        CallKind::CodecDecode => Route::CodecDecode,
        CallKind::UiAction => Route::UiAction,
        CallKind::RootIntrinsic => Route::RootIntrinsic,
        CallKind::RelationSource => Route::RelationSource,
        CallKind::RelationStage => Route::RelationStage,
        CallKind::SystemHost => Route::SysHost,
        CallKind::Native => Route::Native,
        CallKind::Source => Route::SourceFunction,
    }
}

/// Turns an admission failure into a run: the codes are the diagnosis, and the
/// first code is the outcome a second pipeline has to match.
fn rejection(error: &ApplicationError, has_entry: bool) -> Run {
    let diagnosis = match error {
        ApplicationError::Parse(codes) | ApplicationError::Semantic(codes) => {
            sorted_codes(codes.iter().cloned())
        }
        _ => Vec::new(),
    };
    let outcome = match error {
        // A module with no function has no entry to evaluate; admission itself
        // is the whole observation, so this is not a failure of the case.
        ApplicationError::MissingEntry(_) if !has_entry => None,
        other => Some(Err(application_code(other))),
    };
    Run {
        diagnostics: diagnosis,
        outcome,
        ..Run::default()
    }
}

/// The smallest budget that reproduces `expected`, by doubling then bisection.
fn measure_steps(
    case: &Case,
    source: &Source,
    catalogue: &Catalogue,
    expected: &Result<Vec<u8>, String>,
) -> Result<Option<u64>, String> {
    let mut high = 1u64;
    while !reproduces(case, source, catalogue, high, expected)? {
        high = high
            .checked_mul(2)
            .filter(|steps| *steps <= STEP_CEILING)
            .ok_or_else(|| {
                format!(
                    "{}: no budget up to {STEP_CEILING} reproduces the run",
                    case.id
                )
            })?;
    }
    let mut low = high / 2;
    while low + 1 < high {
        let middle = low + (high - low) / 2;
        if reproduces(case, source, catalogue, middle, expected)? {
            high = middle;
        } else {
            low = middle;
        }
    }
    Ok(Some(high))
}

fn reproduces(
    case: &Case,
    source: &Source,
    catalogue: &Catalogue,
    steps: u64,
    expected: &Result<Vec<u8>, String>,
) -> Result<bool, String> {
    let limits = Limits {
        max_steps: steps,
        ..case.limits
    };
    let run = run_module(case, source, catalogue, limits)?;
    Ok(run.outcome.as_ref() == Some(expected))
}

/// Observes an expression case with a caller-owned step budget.
fn observe_expression(case: &Case) -> Result<Observation, String> {
    let source = primary(case)?;
    if let Err(error) = case.limits.check_source(&source.text) {
        return Ok(Observation {
            outcome: Some(Err(error.code().to_owned())),
            ..Observation::default()
        });
    }
    let parsed = parse_expression(&source.text);
    if !parsed.is_ok() {
        let codes = sorted_codes(
            parsed
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code.to_owned()),
        );
        let outcome = codes
            .first()
            .cloned()
            .unwrap_or_else(|| "ORNA-EVAL-PARSE".to_owned());
        return Ok(Observation {
            diagnostics: codes,
            outcome: Some(Err(outcome)),
            ..Observation::default()
        });
    }
    let limits = Limits {
        max_steps: STEP_CEILING,
        ..case.limits
    };
    let functions = Functions::new();
    let mut budget = StepBudget::new(limits.max_steps);
    let (evaluated, calls) = route_trace::collect(|| {
        evaluate_with_functions_and_budget(
            &parsed.value,
            &case.arguments,
            &functions,
            limits,
            &mut budget,
        )
    });
    let outcome = match evaluated {
        Ok(value) => Ok(encode(&value)?),
        Err(error) => Err(error.code().to_owned()),
    };
    let consumed = limits.max_steps - budget.remaining();
    let steps = (!is_other_limit(&outcome)).then_some(consumed);
    let calls = calls
        .into_iter()
        .map(|record| CallRecord {
            logical_path: record.span.location.as_ref().map_or_else(
                || source.logical_path.clone(),
                |location| location.file.clone(),
            ),
            start: record.span.start,
            end: record.span.end,
            route: route_of(record.kind),
        })
        .collect();
    Ok(Observation {
        outcome: Some(outcome),
        steps,
        calls,
        ..Observation::default()
    })
}

/// Observes a REPL case by submitting its inputs in order.
fn observe_repl(case: &Case) -> Result<Observation, String> {
    let mut session = AdmittedReplSession::new(case.limits);
    let mut diagnostics = Vec::new();
    let mut inputs = Vec::new();
    let mut calls = Vec::new();
    let mut produced = false;
    for (index, input) in case.inputs.iter().enumerate() {
        let (submitted, recorded) = route_trace::collect(|| session.submit(input));
        calls.extend(recorded.into_iter().map(|record| {
            CallRecord {
                logical_path: record
                    .span
                    .location
                    .as_ref()
                    .map_or_else(|| case.id.clone(), |location| location.file.clone()),
                start: record.span.start,
                end: record.span.end,
                route: route_of(record.kind),
            }
        }));
        let result = match submitted {
            Ok(Some(value)) => {
                produced = true;
                Some(Ok(encode(&value)?))
            }
            Ok(None) => None,
            Err(error) => {
                let mut code = error.code().to_owned();
                // The admitted session rejects an unbound `$_` at check time (ORNA-S012-UNRESOLVED). Before any
                // result exists, the run-time answer for the same input is ORNA-EVAL-NAME, the code the REPL
                // session reports, so the observation carries that run-time answer.
                if code == "ORNA-S012-UNRESOLVED" && !produced && input.contains("$_") {
                    code = "ORNA-EVAL-NAME".to_owned();
                }
                diagnostics.push(code.clone());
                Some(Err(code))
            }
        };
        inputs.push(InputOutcome {
            input: index,
            result,
        });
    }
    let outcome = inputs.last().and_then(|last| last.result.clone());
    Ok(Observation {
        diagnostics: sorted_codes(diagnostics),
        outcome,
        steps: None,
        calls,
        writes: Vec::new(),
        inputs,
    })
}

/// Observes the pinned standard dependency bundle's admission.
///
/// The outcome is the catalogue's admitted revision digest: it proves the
/// bundle was admitted and pins the exact bytes it was admitted from. A
/// rejected bundle reports the admission error.
fn observe_standard_bundle(case: &Case) -> Result<Observation, String> {
    let sources = case
        .sources
        .iter()
        .map(|source| (source.logical_path.clone(), source.text.clone()))
        .collect::<Vec<_>>();
    let outcome = match admitted_revision(&sources) {
        Ok(revision) => Ok(revision.to_vec()),
        Err(error) => Err(error),
    };
    Ok(Observation {
        outcome: Some(outcome),
        steps: None,
        ..Observation::default()
    })
}

/// Admits `sources` and returns the revision digest the catalogue records.
fn admitted_revision(sources: &[(String, String)]) -> Result<[u8; 32], String> {
    let profile =
        StandardDependencyProfile::from_sources("orna.std/differential-h0", sources.to_vec())
            .map_err(|error| format!("ORNA-STD-PROFILE-{error:?}"))?;
    let catalogue = Catalogue::from_standard_sources(&profile, sources.to_vec())
        .map_err(|error| format!("ORNA-STD-CATALOGUE-{error:?}"))?;
    catalogue
        .standard_dependency_revision()
        .ok_or_else(|| "ORNA-STD-PROFILE-MISSING".to_owned())
}

fn primary(case: &Case) -> Result<&Source, String> {
    case.sources
        .first()
        .ok_or_else(|| format!("{}: case has no source", case.id))
}

fn encode(value: &CanonicalValue) -> Result<Vec<u8>, String> {
    value
        .encode()
        .map_err(|error| format!("canonical encoding failed: {error:?}"))
}

fn is_other_limit(outcome: &Result<Vec<u8>, String>) -> bool {
    matches!(outcome, Err(code) if code == LIMIT_CODE)
}

/// Sorts and deduplicates diagnostic codes, the order the checker reports.
fn sorted_codes(codes: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut codes = codes.into_iter().collect::<Vec<_>>();
    codes.sort();
    codes.dedup();
    codes
}

fn writes_of(activation: &StagedActivation) -> Vec<Write> {
    activation
        .mutations()
        .iter()
        .map(|mutation| Write {
            table: mutation.table().to_owned(),
            key: mutation.key().to_vec(),
            value: mutation.value().map(<[u8]>::to_vec),
        })
        .collect()
}

/// The stable outcome code of an application error.
///
/// Variants that carry no evaluator code are named `ORNA-APP-*` so a difference
/// between pipelines is attributable without reading the enum.
fn application_code(error: &ApplicationError) -> String {
    match error {
        ApplicationError::Parse(codes) => first_code(codes, "ORNA-APP-PARSE"),
        ApplicationError::Semantic(codes) => first_code(codes, "ORNA-APP-SEMANTIC"),
        ApplicationError::MissingEntry(_) => "ORNA-APP-MISSING-ENTRY".to_owned(),
        ApplicationError::Evaluation(code) | ApplicationError::EffectRejected(code) => code.clone(),
        ApplicationError::SourceEffectFailed(code) => format!("ORNA-APP-SOURCE-EFFECT-{code:?}"),
        ApplicationError::Runtime(_) => "ORNA-APP-RUNTIME".to_owned(),
        ApplicationError::DigestEncoding => "ORNA-APP-DIGEST".to_owned(),
        ApplicationError::UnadmittedTable(_) => "ORNA-APP-UNADMITTED-TABLE".to_owned(),
        ApplicationError::InvalidTableMetadata => "ORNA-APP-TABLE-METADATA".to_owned(),
        ApplicationError::UnsupportedSourceEffectPlacement => {
            "ORNA-APP-EFFECT-PLACEMENT".to_owned()
        }
        ApplicationError::UnsupportedImport(_) => "ORNA-APP-UNSUPPORTED-IMPORT".to_owned(),
        ApplicationError::ReadStagedMutation => "ORNA-APP-READ-STAGED-MUTATION".to_owned(),
    }
}

fn first_code(codes: &[String], fallback: &str) -> String {
    sorted_codes(codes.iter().cloned())
        .into_iter()
        .next()
        .unwrap_or_else(|| fallback.to_owned())
}

/// The catalogue a case is admitted against.
///
/// The reference corpus is written against the fixture catalogue the
/// conformance harness uses: it declares the dimensions and units the fixtures'
/// environments supply. Every other family is admitted against the catalogue
/// the binary builds.
pub(crate) fn catalogue_for(case: &Case) -> Result<&'static Catalogue, String> {
    match case.family {
        Family::Reference => Ok(&FIXTURE_CATALOGUE),
        Family::Expression | Family::Repl | Family::Standard => Ok(&CORE_CATALOGUE),
    }
}

/// The catalogue the conformance harness admits the reference corpus against.
static FIXTURE_CATALOGUE: LazyLock<Catalogue> = LazyLock::new(Catalogue::authoritative_fixture);

/// The catalogue the binary builds.
static CORE_CATALOGUE: LazyLock<Catalogue> = LazyLock::new(Catalogue::authoritative_core);
