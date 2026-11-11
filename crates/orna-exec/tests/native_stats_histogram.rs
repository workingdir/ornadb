//! The `core.stats` histogram native over lowered source, against the old evaluator's outcomes: the bin fixtures, the
//! final-upper-bound switch, and the `ORNA-EVAL-TYPE` and `ORNA-EVAL-VALUE` refusals of malformed bins.

use std::rc::Rc;

use orna_exec::{ExecError, ExecLimits, Value, run_entry};
use orna_hir::{DefId, DefKind, ModulePath, Name, Program, SourceId, SourceUnit, lower};
use orna_syntax::parse_module_with_file;

const LIMITS: ExecLimits = ExecLimits {
    max_steps: 100_000,
    max_depth: 64,
    max_string_bytes: 1 << 20,
    max_collection_items: 2_048,
    max_integer_digits: 1_024,
};

fn lower_one(source: &str) -> Program {
    let parsed = parse_module_with_file(source, "test.orna");
    assert!(
        parsed.is_ok(),
        "test source must parse: {:?}",
        parsed.diagnostics
    );
    let program = lower(&[SourceUnit {
        source: SourceId::new(0),
        tree: &parsed.value,
        path: ModulePath::new([Name::new("main")]),
    }]);
    assert_eq!(program.validate(), Ok(()), "lowered program must validate");
    program
}

fn run(source: &str) -> Result<Value, ExecError> {
    let program = lower_one(source);
    let run_def: DefId = program
        .modules
        .iter()
        .flat_map(|module| module.items.iter().copied())
        .find(|def| {
            matches!(program.defs[*def].kind, DefKind::Fn(_))
                && program.defs[*def].name.as_str() == "run"
        })
        .expect("the source defines `run`");
    run_entry(&program, run_def, LIMITS).result
}

fn call_stats(body: &str) -> Result<Value, ExecError> {
    run(&format!(
        "use core.stats;\nuse core.collection;\npub fn run() = {body};\n"
    ))
}

fn ints(values: &[i64]) -> Value {
    Value::List(Rc::from(
        values
            .iter()
            .map(|value| Value::Int((*value).into()))
            .collect::<Vec<_>>(),
    ))
}

#[test]
fn histogram_counts_half_open_bins_and_the_final_upper_bound() {
    assert_eq!(
        call_stats("stats.histogram([0, 1, 2, 3], [(0, 2), (2, 4)])"),
        Ok(ints(&[2, 2]))
    );
    assert_eq!(
        call_stats("stats.histogram([0, 2, 4], [(0, 2), (2, 4)], include_final_upper: true)"),
        Ok(ints(&[1, 2]))
    );
    assert_eq!(
        call_stats(
            "stats.histogram([-1, 0, 1, 2, 3, 4, 5], [(0, 2), (2, 4)], include_final_upper: true)"
        ),
        Ok(ints(&[2, 3]))
    );
    assert_eq!(
        call_stats("stats.histogram([0, 2, 4], [(0, 2), (2, 4)])"),
        Ok(ints(&[1, 1]))
    );
}

#[test]
fn histogram_of_no_rows_counts_zero_per_bin() {
    assert_eq!(
        call_stats("stats.histogram(collection.drop([1], 1), [(0, 2)])"),
        Ok(ints(&[0]))
    );
}

#[test]
fn histogram_refuses_malformed_bins_with_the_old_codes() {
    assert_eq!(
        call_stats("stats.histogram([1], [(2, 2)])"),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
    assert_eq!(
        call_stats("stats.histogram([1], [(0, 4), (3, 5)])"),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
}
