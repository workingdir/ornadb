//! The portable `sys.explain` overloads of
//! `source/34-system-reference.md`: `sys.explain(Query)`,
//! `sys.explain(FunctionRef)` and `sys.explain(Diagnostic)`.
//!
//! Query and function plans are lowered into the published `orna-sys-v1`
//! planner inputs only where every operator has an exact representation, and
//! the host catalogue pins each input to its snapshot; no reference is ever
//! synthesised. A diagnostic explanation is a projection of the
//! `sys.Diagnostic` row it explains.
//!
//! Every failure carries a portable code from the system reference's failure
//! table (ORNA-SYS-096/097):
//!
//! - the host supplies no catalogue, or the planner cannot represent a stage or
//!   source exactly: `sys.version.incompatible`, a required coordinate is
//!   unavailable (ORNA-SYS-008);
//! - the catalogue holds no object for the source or function reference:
//!   `sys.object.not_found`;
//! - the reference names a row of another relation: `sys.object.wrong_kind`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;

use orna_foundation_v1::{
    CanonicalSnapshot, CanonicalValue, FunctionRef, OvbRaw, RowRef, SYS_FUNCTION_TABLE_ID,
    SafeText, Snapshot, SystemReferenceError, TypedRowRef, snapshot_reference,
};
use orna_sys_v1::{
    ExplainError, FunctionPlanDescription, ObjectRef, Plan, QueryPlanDescription, SnapshotRef,
    explain_function, explain_query,
};

use crate::relation::{RelationPlan, RelationStage};
use crate::{DecimalValue, EvaluationError, Value, parse_decimal};

/// A required API or profile coordinate is unavailable (ORNA-SYS-008).
const VERSION_INCOMPATIBLE: &str = "sys.version.incompatible";
/// No object matches the exact accessible selector.
const OBJECT_NOT_FOUND: &str = "sys.object.not_found";
/// The resolved object is not the requested kind.
const OBJECT_WRONG_KIND: &str = "sys.object.wrong_kind";
/// The snapshot selector cannot be resolved.
const SNAPSHOT_NOT_FOUND: &str = "sys.snapshot.not_found";
/// Required snapshot objects are absent or unavailable.
const SNAPSHOT_INCOMPLETE: &str = "sys.snapshot.incomplete";

/// The fields of one `sys.Diagnostic` row (`api/sys.json`).
const DIAGNOSTIC_FIELDS: [&str; 14] = [
    "reference",
    "id",
    "severity",
    "code",
    "message",
    "object",
    "definition",
    "primary_span",
    "labels",
    "causes",
    "help",
    "data",
    "redacted",
    "trace",
];

/// Resolves explain inputs against one snapshot-pinned catalogue.
///
/// Implementations return pins the host has already resolved. An input the
/// catalogue does not hold yields `None`; no reference is invented.
pub trait ExplainCatalogue: Send + Sync + Debug {
    /// Pins a relation source to the catalogue snapshot and its object.
    fn resolve_source(&self, source: &str) -> Option<(SnapshotRef, ObjectRef)>;

    /// Whether the catalogue holds function object identities at all. Without
    /// them no `sys.FunctionRef` can be described, so a function explain fails
    /// with the unavailable coordinate `sys.version.incompatible`
    /// (ORNA-SYS-008) instead of claiming the function is absent.
    fn resolves_functions(&self) -> bool {
        false
    }

    /// Describes the function a `sys.FunctionRef` names at the reference's
    /// pinned snapshot, together with the function's direct statically known
    /// dependencies. Called only when [`Self::resolves_functions`] holds.
    fn describe_function(&self, _function: &FunctionRef) -> Option<FunctionPlanDescription> {
        None
    }
}

/// The snapshot-pinned explain catalogue of one host activation.
///
/// Its sources are the relations the activation can name at its pinned
/// snapshot: the admitted project tables and the system relations whose rows
/// the host serves. `orna-sys-v1` plans carry descriptive references, so the
/// snapshot is named by its `sys.Snapshot` natural key and each object by its
/// canonical relation name, which names exactly one object at that snapshot
/// (ORNA-SYS-033). The project catalogue records no function object
/// identities, so this catalogue resolves no `sys.FunctionRef`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActivationExplainCatalogue {
    snapshot: SnapshotRef,
    sources: BTreeSet<String>,
}

impl ActivationExplainCatalogue {
    /// Pins `sources` to the activation's CWD or commit snapshot.
    pub fn new<S: Into<String>>(
        snapshot: &CanonicalSnapshot,
        sources: impl IntoIterator<Item = S>,
    ) -> Result<Self, SystemReferenceError> {
        let database = match snapshot {
            Snapshot::Cwd { database, .. } | Snapshot::Commit { database, .. } => *database,
        };
        let reference = snapshot_reference(database, snapshot.clone())?;
        let OvbRaw::Bytes(id) = &reference.as_row_ref().key else {
            return Err(SystemReferenceError::InvalidSnapshotKey);
        };
        let mut name = String::with_capacity("snapshot:".len() + id.len() * 2);
        name.push_str("snapshot:");
        for byte in id {
            name.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
            name.push(char::from(b"0123456789abcdef"[usize::from(byte & 0x0f)]));
        }
        Ok(Self {
            snapshot: SnapshotRef::descriptive(name),
            sources: sources.into_iter().map(Into::into).collect(),
        })
    }
}

impl ExplainCatalogue for ActivationExplainCatalogue {
    fn resolve_source(&self, source: &str) -> Option<(SnapshotRef, ObjectRef)> {
        self.sources
            .contains(source)
            .then(|| (self.snapshot.clone(), ObjectRef::descriptive(source)))
    }
}

/// Explains one relation query (`sys.explain(Query)`).
pub(crate) fn explain_query_plan(
    plan: &RelationPlan,
    catalogue: Option<&dyn ExplainCatalogue>,
) -> Result<Value, EvaluationError> {
    let catalogue = catalogue.ok_or_else(|| failure(VERSION_INCOMPATIBLE))?;
    // Unions and value-backed relations have no single catalogue source.
    if plan.source_union.is_some() || plan.source_values.is_some() {
        return Err(failure(VERSION_INCOMPATIBLE));
    }
    let (snapshot, source) = catalogue
        .resolve_source(&plan.source)
        .ok_or_else(|| failure(OBJECT_NOT_FOUND))?;
    let (distinct, limit) = lower_stages(&plan.stages)?;
    let query = QueryPlanDescription {
        snapshot,
        source,
        source_statistics: None,
        joins: Vec::new(),
        predicate: None,
        projections: Vec::new(),
        distinct,
        ordering: Vec::new(),
        limit,
        mutations: Vec::new(),
        materialize_into: None,
    };
    let explained = explain_query(&query).map_err(planner_failure)?;
    plan_record(explained.plan())
}

/// Explains one function (`sys.explain(FunctionRef)`) through the function
/// description the catalogue supplies for the reference. A reference to another
/// relation is `sys.object.wrong_kind`; a catalogue that holds no function
/// identities cannot describe any function, so the coordinate is unavailable.
pub(crate) fn explain_function_plan(
    function: &CanonicalValue,
    catalogue: Option<&dyn ExplainCatalogue>,
) -> Result<Value, EvaluationError> {
    let catalogue = catalogue.ok_or_else(|| failure(VERSION_INCOMPATIBLE))?;
    let reference = function
        .encode()
        .ok()
        .and_then(|bytes| RowRef::decode(&bytes).ok())
        .ok_or_else(|| failure(OBJECT_NOT_FOUND))?;
    if reference.table_id != SYS_FUNCTION_TABLE_ID {
        return Err(failure(OBJECT_WRONG_KIND));
    }
    if !catalogue.resolves_functions() {
        return Err(failure(VERSION_INCOMPATIBLE));
    }
    let description = catalogue
        .describe_function(&TypedRowRef::from_row_ref(reference))
        .ok_or_else(|| failure(OBJECT_NOT_FOUND))?;
    let explained = explain_function(&description).map_err(planner_failure)?;
    plan_record(explained.plan())
}

/// Explains one diagnostic (`sys.explain(Diagnostic)`).
///
/// A `sys.Diagnostic` row has already crossed the system-value boundary, so
/// its message and help are the safe text that redaction admitted
/// (ORNA-SYS-048, ORNA-SYS-099), and a redacted row keeps its explicit marker
/// (ORNA-SYS-100). The explanation therefore summarises the row by its message,
/// suggests its help, lists its causal diagnostics and relates the objects of
/// the row and its causes, first occurrence first. No plan authority exists for
/// a diagnostic, so `plan` is null.
pub(crate) fn explain_diagnostic(
    diagnostic: BTreeMap<String, Value>,
) -> Result<Value, EvaluationError> {
    let mut related_objects = Vec::new();
    let (summary, causes, suggestions) = {
        let parts = validate_diagnostic(&diagnostic, &mut related_objects)?;
        (
            parts.message.clone(),
            parts.causes.to_vec(),
            parts.help.to_vec(),
        )
    };
    let mut fields = BTreeMap::new();
    fields.insert("summary".to_owned(), Value::String(summary));
    fields.insert("causes".to_owned(), Value::List(causes.into()));
    fields.insert("suggestions".to_owned(), Value::List(suggestions.into()));
    fields.insert(
        "related_objects".to_owned(),
        Value::List(related_objects.into_iter().map(Value::Reference).collect()),
    );
    fields.insert("plan".to_owned(), Value::Option(None));
    fields.insert("diagnostic".to_owned(), Value::Record(diagnostic));
    Ok(Value::Record(fields))
}

/// The parts of a validated `sys.Diagnostic` row an explanation reuses.
struct DiagnosticParts<'a> {
    message: &'a String,
    causes: &'a [Value],
    help: &'a [Value],
}

/// Validates one `sys.Diagnostic` row and its causes, collecting the object
/// references they name in first-occurrence order.
fn validate_diagnostic<'a>(
    diagnostic: &'a BTreeMap<String, Value>,
    related: &mut Vec<CanonicalValue>,
) -> Result<DiagnosticParts<'a>, EvaluationError> {
    let fields_match = diagnostic.len() == DIAGNOSTIC_FIELDS.len()
        && DIAGNOSTIC_FIELDS
            .iter()
            .all(|field| diagnostic.contains_key(*field));
    if !fields_match
        || !matches!(diagnostic["id"], Value::String(_))
        || !matches!(diagnostic["code"], Value::String(_))
        || !matches!(diagnostic["redacted"], Value::Bool(_))
    {
        return Err(type_error());
    }
    let (Value::String(message), Value::List(causes), Value::List(help)) = (
        &diagnostic["message"],
        &diagnostic["causes"],
        &diagnostic["help"],
    ) else {
        return Err(type_error());
    };
    if help.iter().any(|item| !matches!(item, Value::String(_))) {
        return Err(type_error());
    }
    let object = match &diagnostic["object"] {
        Value::Null | Value::Option(None) => None,
        Value::Option(Some(object)) => match object.as_ref() {
            Value::Reference(object) => Some(object),
            _ => return Err(type_error()),
        },
        Value::Reference(object) => Some(object),
        _ => return Err(type_error()),
    };
    if let Some(object) = object
        && !related.contains(object)
    {
        related.push(object.clone());
    }
    for cause in causes {
        let Value::Record(cause) = cause else {
            return Err(type_error());
        };
        validate_diagnostic(cause, related)?;
    }
    Ok(DiagnosticParts {
        message,
        causes,
        help,
    })
}

/// Lowers `distinct` and `take` stages. Filter and map stages need a
/// snapshot-pinned `ExpressionRef` for their closure, which the evaluator does
/// not have, so they fail closed. A `take` followed by `distinct` is not the
/// same as `distinct` then `take`, so it also fails closed.
fn lower_stages(stages: &[RelationStage]) -> Result<(bool, Option<u64>), EvaluationError> {
    let mut distinct = false;
    let mut limit: Option<u64> = None;
    for stage in stages {
        match stage {
            RelationStage::Distinct => {
                if limit.is_some() {
                    return Err(failure(VERSION_INCOMPATIBLE));
                }
                distinct = true;
            }
            RelationStage::Take(count) => {
                let count = u64::try_from(*count).map_err(|_| failure(VERSION_INCOMPATIBLE))?;
                limit = Some(limit.map_or(count, |current| current.min(count)));
            }
            _ => return Err(failure(VERSION_INCOMPATIBLE)),
        }
    }
    Ok((distinct, limit))
}

/// Maps a planner rejection of catalogue-supplied input onto the portable
/// failure the input caused. Bounds and shapes outside the planner profile
/// mean the required planning coordinate is unavailable (ORNA-SYS-008).
fn planner_failure(error: ExplainError) -> EvaluationError {
    failure(match error {
        ExplainError::InvalidSnapshot => SNAPSHOT_NOT_FOUND,
        ExplainError::InvalidObject => OBJECT_NOT_FOUND,
        ExplainError::InvalidDependency => SNAPSHOT_INCOMPLETE,
        ExplainError::InvalidExpression
        | ExplainError::TooManyNodes
        | ExplainError::TooManyExpressions
        | ExplainError::InvalidPlanTree => VERSION_INCOMPATIBLE,
    })
}

/// Converts the explained `sys.Plan` row into a plain record value.
fn plan_record(plan: &Plan) -> Result<Value, EvaluationError> {
    // The planner attaches no warnings to these plans. Fail closed rather
    // than silently dropping a diagnostic this boundary cannot represent.
    if !plan.warnings().is_empty() {
        return Err(failure(VERSION_INCOMPATIBLE));
    }
    let estimated_cost = match plan.estimated_cost() {
        None => Value::Option(None),
        Some(text) => {
            let (coefficient, exponent10) = parse_decimal(text)?;
            Value::Option(Some(Box::new(Value::Decimal(DecimalValue::new(
                coefficient,
                exponent10,
            )?))))
        }
    };
    let mut fields = BTreeMap::new();
    fields.insert(
        "reference".to_owned(),
        Value::String(plan.reference().as_str().to_owned()),
    );
    fields.insert("id".to_owned(), Value::String(plan.id().to_owned()));
    fields.insert(
        "snapshot".to_owned(),
        Value::String(plan.snapshot().as_str().to_owned()),
    );
    fields.insert(
        "root".to_owned(),
        Value::String(plan.root().as_str().to_owned()),
    );
    fields.insert("estimated_cost".to_owned(), estimated_cost);
    fields.insert(
        "actual_available".to_owned(),
        Value::Bool(plan.actual_available()),
    );
    fields.insert("warnings".to_owned(), Value::list(Vec::new()));
    Ok(Value::Record(fields))
}

fn failure(code: &'static str) -> EvaluationError {
    EvaluationError::redacted(SafeText::new(code).expect("static safe code"))
}

/// An argument that is not a `sys.Diagnostic` row is a host-language type
/// error; semantic admission selects the overload by static type.
fn type_error() -> EvaluationError {
    failure("ORNA-EVAL-TYPE")
}
