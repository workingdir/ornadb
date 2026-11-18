//! `:plan EXPR`: explains a query as the structured `sys.Plan` the planner
//! derives for it, without running the query.
//!
//! The plan comes from `sys.explain(EXPR)` evaluated by the console's own
//! submission path, the one that pins the explain to the session's snapshot
//! catalogue, so the console never plans with a second implementation and no
//! plan can drift from `sys.explain` in a script (ORNA-SYS-053, ORNA-PLAN-002).
//! The explain runs against a copy of the session: it neither publishes `$_`
//! nor adds a binding.
//!
//! `sys.explain` evaluates its argument, so `:plan` first admits the expression
//! and refuses one that would do more than read tables. Planning is an
//! inspection and never changes the database (ORNA-REPL-003).

use std::io::{self, Write};

use orna_check::{EffectKind, EffectSummary};
use orna_evaluator::ReplValue;
use orna_project_repl::{AdmittedReplSession, ReplDetail, ReplOutcome};
use orna_value::Raw;

use crate::repl::{
    AnsiColor, ReplFailure, ReplSubmissionHandler, write_repl_outcome, write_result_line,
    write_styled,
};
use crate::repl_inspect::{DISPLAY, render};

/// The call that asks the planner for the plan of its argument.
const EXPLAIN: &str = "sys.explain(";

/// The OVB tag of an `Option` value; its payload holds the present value, or
/// nothing.
const OPTION_TAG: u64 = 60013;

/// Why `:plan` could not produce a plan.
pub(crate) enum PlanRefusal {
    /// The console refused the expression itself; the text says why.
    Expression(String),
    /// Admission or evaluation failed; the failure's span is already relative
    /// to the expression the user typed.
    Failure(ReplFailure),
}

/// The evaluated `sys.explain` result.
pub(crate) struct Plan {
    outcome: ReplOutcome,
}

/// Explains `expr` as `sys.explain(expr)` would, against a copy of `session`.
///
/// The expression may read tables and the clock. Any other effect would run
/// while the argument is evaluated, so it is refused before anything runs.
pub(crate) fn explain(
    expr: &str,
    session: &AdmittedReplSession,
    evaluate: &mut ReplSubmissionHandler<'_>,
) -> Result<Plan, PlanRefusal> {
    let wrapped = format!("{EXPLAIN}{expr})");
    let relocate_to_expr =
        |failure: ReplFailure| PlanRefusal::Failure(locate_in(failure, EXPLAIN.len(), expr.len()));
    match session.stage_activation(&wrapped) {
        Ok(staged) => {
            if let Some(effects) = beyond_reads(staged.effects()) {
                return Err(PlanRefusal::Expression(format!(
                    "`:plan` explains a query without running it, so its expression cannot \
                     perform {effects}"
                )));
            }
        }
        // An input that stages no effect is pure; every other failure is the
        // expression's own parse or type error.
        Err(error) if error.code() == "ORNA-REPL-EFFECT" => {}
        Err(error) => return Err(relocate_to_expr(ReplFailure::from(error))),
    }
    let mut scratch = session.clone();
    let outcome = evaluate(&mut scratch, &wrapped).map_err(relocate_to_expr)?;
    Ok(Plan { outcome })
}

/// Maps a failure's span in `sys.explain(EXPR)` back onto `EXPR`, which starts
/// at `prefix` and is `expr_len` bytes long. A span over the whole call is the
/// whole expression; a span that overlaps the expression keeps the overlap;
/// one inside the call's own text has no position in what the user typed and
/// is dropped.
///
/// A portable `sys` failure arrives as its bare code. The planner raises one
/// when it cannot describe the query exactly, so it is given a located
/// message that says so instead of the generic evaluation text.
fn locate_in(mut failure: ReplFailure, prefix: usize, expr_len: usize) -> ReplFailure {
    if failure.detail.is_none() && failure.code.starts_with("sys.") {
        failure.detail = Some(ReplDetail {
            code: failure.code.clone(),
            message: "the planner has no exact plan for this query".to_owned(),
            span: Some(0..expr_len),
            notes: Vec::new(),
            help: vec!["explain a simpler query, such as one of its sources".to_owned()],
        });
        return failure;
    }
    if let Some(detail) = failure.detail.as_mut() {
        detail.span = detail.span.take().and_then(|span| {
            let whole_call = span.start < prefix && span.end >= prefix + expr_len;
            let start = span.start.saturating_sub(prefix).min(expr_len);
            let end = span.end.saturating_sub(prefix).min(expr_len);
            if whole_call {
                Some(0..expr_len)
            } else {
                (start < end).then_some(start..end)
            }
        });
    }
    failure
}

/// The comma-separated names of the effects in `effects` that `:plan` does not
/// admit, or `None` when the expression only reads.
fn beyond_reads(effects: &EffectSummary) -> Option<String> {
    let mut names: Vec<&str> = effects
        .effects
        .iter()
        .filter(|kind| !matches!(kind, EffectKind::TableRead | EffectKind::Clock))
        .map(|kind| kind.as_str())
        .collect();
    if effects.opaque {
        names.push("an operation that has no effect name");
    }
    (!names.is_empty()).then(|| names.join(", "))
}

/// The fields of a `sys.Plan` the console prints.
struct PlanFields {
    id: String,
    snapshot: String,
    root: String,
    estimated_cost: Option<String>,
    actual_available: bool,
    warnings: usize,
}

impl PlanFields {
    /// Reads the printed fields of a `sys.Plan` record, or `None` when the
    /// value is not one.
    fn from_raw(raw: &Raw) -> Option<Self> {
        let Raw::Map(entries) = raw else {
            return None;
        };
        let field = |name: &str| {
            entries.iter().find_map(|(key, value)| {
                matches!(key, Raw::Text(text) if text == name).then_some(value)
            })
        };
        let text = |name: &str| match field(name)? {
            Raw::Text(text) => Some(text.clone()),
            _ => None,
        };
        // An `Option` is `[0]` when absent and `[1, value]` when present.
        let estimated_cost = match field("estimated_cost")? {
            Raw::Tag(OPTION_TAG, payload) => match payload.as_ref() {
                Raw::Array(items) => match items.as_slice() {
                    [Raw::Int(flag)] if flag.to_string() == "0" => None,
                    [Raw::Int(flag), cost] if flag.to_string() == "1" => {
                        Some(render(cost, 1, DISPLAY).0)
                    }
                    _ => return None,
                },
                _ => return None,
            },
            _ => return None,
        };
        let Raw::Bool(actual_available) = field("actual_available")? else {
            return None;
        };
        let Raw::Array(warnings) = field("warnings")? else {
            return None;
        };
        Some(Self {
            id: text("id")?,
            snapshot: text("snapshot")?,
            root: text("root")?,
            estimated_cost,
            actual_available: *actual_available,
            warnings: warnings.len(),
        })
    }
}

/// Writes the plan: its identity and type on the first line, then one line for
/// each fact the plan carries. A result that is not a `sys.Plan` record prints
/// as an ordinary result.
pub(crate) fn write_plan<W: Write>(
    writer: &mut W,
    plan: &Plan,
    expr: &str,
    color_enabled: bool,
) -> io::Result<()> {
    let fields = match &plan.outcome.value {
        Some(ReplValue::Value(value)) => PlanFields::from_raw(value.raw()),
        _ => None,
    };
    let Some(fields) = fields else {
        return write_repl_outcome(writer, &plan.outcome, expr, color_enabled);
    };
    write_result_line(writer, &fields.id, "sys.Plan", color_enabled)?;
    let counters = if fields.actual_available {
        "measured"
    } else {
        "absent"
    };
    let warnings = if fields.warnings == 0 {
        "none".to_owned()
    } else {
        fields.warnings.to_string()
    };
    let rows = [
        ("snapshot", fields.snapshot),
        ("root", fields.root),
        (
            "estimated cost",
            fields
                .estimated_cost
                .unwrap_or_else(|| "unknown".to_owned()),
        ),
        ("actual counters", counters.to_owned()),
        ("warnings", warnings),
    ];
    for (label, text) in rows {
        write_styled(
            writer,
            AnsiColor::Dim,
            format!("  {label:<16}").as_bytes(),
            color_enabled,
        )?;
        writer.write_all(text.as_bytes())?;
        writer.write_all(b"\n")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use orna_value::CanonicalValue;

    fn text(value: &str) -> Raw {
        Raw::Text(value.to_owned())
    }

    fn some(value: Raw) -> Raw {
        Raw::Tag(
            OPTION_TAG,
            Box::new(Raw::Array(vec![Raw::Int(1.into()), value])),
        )
    }

    fn none() -> Raw {
        Raw::Tag(OPTION_TAG, Box::new(Raw::Array(vec![Raw::Int(0.into())])))
    }

    /// A `sys.Plan` record with its keys in canonical order: shorter keys
    /// first, then by byte value.
    fn plan_record(cost: Raw, actual: bool, warnings: usize) -> Raw {
        Raw::Map(vec![
            (text("id"), text("plan:abc")),
            (text("root"), text("plan:abc:0")),
            (text("snapshot"), text("snapshot:def")),
            (
                text("warnings"),
                Raw::Array((0..warnings).map(|_| Raw::Null).collect()),
            ),
            (text("reference"), text("plan:abc")),
            (text("estimated_cost"), cost),
            (text("actual_available"), Raw::Bool(actual)),
        ])
    }

    fn written(raw: Raw) -> String {
        let value = CanonicalValue::new(raw).expect("canonical plan record");
        let plan = Plan {
            outcome: ReplOutcome {
                value: Some(ReplValue::Value(value)),
                ty: None,
            },
        };
        let mut out = Vec::new();
        write_plan(&mut out, &plan, "q", false).expect("in-memory write");
        String::from_utf8(out).expect("utf8 output")
    }

    #[test]
    fn a_plan_prints_its_identity_and_each_fact() {
        assert_eq!(
            written(plan_record(some(Raw::Int(5.into())), false, 0)),
            "plan:abc : sys.Plan\n  snapshot        snapshot:def\n  root            plan:abc:0\n  \
             estimated cost  5\n  actual counters absent\n  warnings        none\n"
        );
    }

    #[test]
    fn an_unknown_cost_measured_counters_and_warnings_are_named() {
        let text = written(plan_record(none(), true, 2));
        assert!(text.contains("  estimated cost  unknown\n"), "{text}");
        assert!(text.contains("  actual counters measured\n"), "{text}");
        assert!(text.contains("  warnings        2\n"), "{text}");
    }

    #[test]
    fn a_result_that_is_not_a_plan_prints_as_an_ordinary_result() {
        assert_eq!(written(Raw::Int(7.into())), "7 : Int\n");
        let incomplete = Raw::Map(vec![(text("id"), text("plan:abc"))]);
        assert!(PlanFields::from_raw(&incomplete).is_none());
    }

    #[test]
    fn reads_beyond_table_reads_and_the_clock_are_named() {
        let mut effects = EffectSummary::default();
        assert_eq!(beyond_reads(&effects), None);
        effects.effects.insert(EffectKind::TableRead);
        effects.effects.insert(EffectKind::Clock);
        assert_eq!(beyond_reads(&effects), None);
        effects.effects.insert(EffectKind::TableWrite);
        effects.effects.insert(EffectKind::Network);
        assert_eq!(
            beyond_reads(&effects).as_deref(),
            Some("table_write, network")
        );
        effects.opaque = true;
        assert!(
            beyond_reads(&effects)
                .expect("opaque effect")
                .ends_with("an operation that has no effect name")
        );
    }

    fn failure_at(span: Option<std::ops::Range<usize>>) -> ReplFailure {
        ReplFailure {
            code: "ORNA-S021-TYPE".to_owned(),
            detail: Some(ReplDetail {
                code: "ORNA-S021-TYPE".to_owned(),
                message: "m".to_owned(),
                span,
                notes: Vec::new(),
                help: Vec::new(),
            }),
        }
    }

    fn span_of(failure: &ReplFailure) -> Option<std::ops::Range<usize>> {
        failure
            .detail
            .as_ref()
            .and_then(|detail| detail.span.clone())
    }

    #[test]
    fn spans_in_the_wrapped_call_map_onto_the_expression() {
        // `sys.explain(` is 12 bytes; the expression `1 + 1` is 5.
        let located = |span| span_of(&locate_in(failure_at(Some(span)), 12, 5));
        assert_eq!(
            located(0..18),
            Some(0..5),
            "the whole call is the expression"
        );
        assert_eq!(located(12..17), Some(0..5));
        assert_eq!(located(14..16), Some(2..4));
        assert_eq!(located(10..14), Some(0..2), "an overlap is kept");
        assert_eq!(located(0..11), None, "the call's own text has no position");
        assert_eq!(span_of(&locate_in(failure_at(None), 12, 5)), None);
    }

    #[test]
    fn a_bare_portable_failure_is_described_and_located_on_the_expression() {
        let bare = ReplFailure::code("sys.version.incompatible");
        let located = locate_in(bare, 12, 5);
        let detail = located.detail.expect("a located detail");
        assert_eq!(detail.code, "sys.version.incompatible");
        assert_eq!(
            detail.message,
            "the planner has no exact plan for this query"
        );
        assert_eq!(detail.span, Some(0..5));
        // A bare failure that is not a portable `sys` code stays as it is.
        let other = locate_in(ReplFailure::code("ORNA-REPL-EFFECT"), 12, 5);
        assert!(other.detail.is_none());
    }
}
