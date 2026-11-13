//! Proves that each of the nine branches of the evaluator's call cascade
//! records its route (HIR design section 1.3 and the `route-trace` hook of
//! section 6.2), one tiny program per branch.
//!
//! This test target only compiles when the `route-trace` feature is enabled:
//!
//! ```sh
//! cargo test -p orna-evaluator --features route-trace --test route_trace_kinds
//! ```

#![cfg(feature = "route-trace")]

use num_bigint::BigInt;
use orna_evaluator::route_trace::{CallKind, collect};
use orna_evaluator::{
    EffectHandler, Environment, Functions, Limits, PureFunction, RelationPage, StepBudget,
    evaluate_expression_with_functions, invoke_named_with_effects,
};
use orna_syntax_ast::SourceSpan as SyntaxSpan;
use orna_syntax_ast::{Argument, Expr, LiteralKind};
use orna_value::CanonicalValue;

fn span() -> SyntaxSpan {
    SyntaxSpan::new(0, 0)
}

fn argument(value: Expr) -> Argument {
    Argument {
        name: None,
        value,
        span: span(),
    }
}

fn string(text: &str) -> Expr {
    Expr::Literal {
        text: format!("{text:?}"),
        kind: LiteralKind::String,
        span: span(),
    }
}

/// `$__orna_relation.source("<source>")`: the relation source form the REPL
/// and the conformance adapter build (design section 1.3 branch 5).
fn relation_source(source: &str) -> Expr {
    Expr::Call {
        callee: Box::new(Expr::Field {
            base: Box::new(Expr::ReplBinding {
                text: "$__orna_relation".into(),
                span: span(),
            }),
            name: "source".into(),
            span: span(),
        }),
        arguments: vec![argument(string(source))],
        span: span(),
    }
}

/// `<input> | <name>(<arguments>)`: one relation pipeline stage (branch 6).
fn relation_stage(input: Expr, name: &str, arguments: Vec<Expr>) -> Expr {
    Expr::Binary {
        lhs: Box::new(input),
        op: "|".into(),
        rhs: Box::new(Expr::Call {
            callee: Box::new(Expr::Name {
                text: name.into(),
                span: span(),
            }),
            arguments: arguments.into_iter().map(argument).collect(),
            span: span(),
        }),
        span: span(),
    }
}

/// One source row of `sys.Storage`.
struct OneRow;

impl EffectHandler for OneRow {
    fn handle(
        &mut self,
        _: &Expr,
        _: &[CanonicalValue],
    ) -> Result<Option<CanonicalValue>, orna_evaluator::EvaluationError> {
        Ok(None)
    }

    fn scan_relation_page(
        &mut self,
        source: &str,
        _after: Option<&[u8]>,
        _limit: usize,
        _budget: &mut StepBudget,
    ) -> Result<Option<RelationPage>, orna_evaluator::EvaluationError> {
        assert_eq!(source, "sys.Storage");
        Ok(Some(RelationPage {
            rows: vec![CanonicalValue::new(orna_value::Raw::Int(BigInt::from(1))).unwrap()],
            next: None,
        }))
    }
}

/// A handler attached to an activation, for `sys.rt.info()` (branch 7).
#[cfg(feature = "system-host-bindings")]
struct AttachedHost {
    attachment: orna_evaluator::SysHostAttachment,
}

#[cfg(feature = "system-host-bindings")]
impl EffectHandler for AttachedHost {
    fn handle(
        &mut self,
        _: &Expr,
        _: &[CanonicalValue],
    ) -> Result<Option<CanonicalValue>, orna_evaluator::EvaluationError> {
        Ok(None)
    }

    fn sys_host_context(&self) -> Option<&orna_evaluator::SysHostAttachment> {
        Some(&self.attachment)
    }
}

/// A function table holding one bodyless-named entry, so the call can be
/// classified. The body never runs for a native or a codec route.
fn functions_with(name: &str) -> Functions {
    let body = orna_syntax::parse_expression("0");
    assert!(body.is_ok(), "{:#?}", body.diagnostics);
    let mut functions = Functions::new();
    functions.insert(
        name.to_owned(),
        PureFunction {
            parameters: Vec::new(),
            body: body.value,
            environment: Environment::new(),
        },
    );
    functions
}

fn kinds_of(run: impl FnOnce()) -> Vec<CallKind> {
    let ((), records) = collect(run);
    records.into_iter().map(|record| record.kind).collect()
}

fn evaluate(source: &str, functions: &Functions) -> orna_value::CanonicalValue {
    let result = evaluate_expression_with_functions(
        source,
        &Environment::new(),
        functions,
        Limits::default(),
    );
    result.unwrap_or_else(|error| panic!("{source}: {}", error.code()))
}

#[test]
fn branch_1_decimal_divide_is_recorded() {
    let kinds = kinds_of(|| {
        let _ = evaluate(
            "1.decimal.divide(3.decimal, scale: 2, rounding: half_even)",
            &Functions::new(),
        );
    });
    assert_eq!(kinds, vec![CallKind::DecimalDivide]);
}

#[test]
fn branch_2_codec_decode_is_recorded() {
    let functions = functions_with("std.encoding.json.decode");
    let kinds = kinds_of(|| {
        let _ = evaluate(r#"std.encoding.json.decode("1", as: Int)"#, &functions);
    });
    assert_eq!(kinds, vec![CallKind::CodecDecode]);
}

#[test]
fn branch_3_ui_action_is_recorded() {
    let kinds = kinds_of(|| {
        let _ = evaluate(r#"std.ui.action("save", as: Int)"#, &Functions::new());
    });
    assert_eq!(kinds, vec![CallKind::UiAction]);
}

#[test]
fn branch_4_root_intrinsic_is_recorded() {
    let kinds = kinds_of(|| {
        let _ = evaluate("Some(1)", &Functions::new());
    });
    assert_eq!(kinds, vec![CallKind::RootIntrinsic]);
}

#[test]
fn branch_5_relation_source_is_recorded() {
    let kinds = kinds_of(|| {
        let body = relation_stage(relation_source("sys.Storage"), "first", Vec::new());
        let mut functions = Functions::new();
        functions.insert(
            "run".into(),
            PureFunction {
                parameters: Vec::new(),
                body,
                environment: Environment::new(),
            },
        );
        let mut effects = OneRow;
        let value = invoke_named_with_effects(
            "run",
            &functions,
            &Environment::new(),
            Limits::default(),
            &mut effects,
        );
        assert!(
            value.is_ok(),
            "{:?}",
            value.err().map(|error| error.code().to_owned())
        );
    });
    assert!(kinds.contains(&CallKind::RelationSource), "{kinds:?}");
}

#[test]
fn branch_6_relation_stage_is_recorded() {
    let kinds = kinds_of(|| {
        let body = relation_stage(relation_source("sys.Storage"), "first", Vec::new());
        let mut functions = Functions::new();
        functions.insert(
            "run".into(),
            PureFunction {
                parameters: Vec::new(),
                body,
                environment: Environment::new(),
            },
        );
        let mut effects = OneRow;
        let value = invoke_named_with_effects(
            "run",
            &functions,
            &Environment::new(),
            Limits::default(),
            &mut effects,
        );
        assert!(
            value.is_ok(),
            "{:?}",
            value.err().map(|error| error.code().to_owned())
        );
    });
    assert!(kinds.contains(&CallKind::RelationStage), "{kinds:?}");
}

#[cfg(feature = "system-host-bindings")]
#[test]
fn branch_7_system_host_is_recorded() {
    use orna_sys::{RuntimeMode, StdSnapshotPin};

    let attachment = orna_evaluator::SysHostAttachment::new(orna_evaluator::SysHostContext {
        database_id: [1; 16],
        repository_id: [2; 16],
        runtime_id: [3; 16],
        root: std::path::PathBuf::new(),
        read_only: true,
        mode: RuntimeMode::Run,
        repository_layout_version: "layout-1".to_owned(),
        storage_manifest_version: "store-3".to_owned(),
        std_snapshot: StdSnapshotPin::Unpinned,
    });
    let body = orna_syntax::parse_expression("sys.rt.info().read_only");
    assert!(body.is_ok(), "{:#?}", body.diagnostics);
    let mut functions = Functions::new();
    functions.insert(
        "run".into(),
        PureFunction {
            parameters: Vec::new(),
            body: body.value,
            environment: Environment::new(),
        },
    );
    let kinds = kinds_of(|| {
        let mut effects = AttachedHost { attachment };
        let value = invoke_named_with_effects(
            "run",
            &functions,
            &Environment::new(),
            Limits::default(),
            &mut effects,
        );
        assert_eq!(
            value.map(|value| value.raw().clone()),
            Ok(orna_value::Raw::Bool(true)),
            "sys.rt.info().read_only reports the attached host"
        );
    });
    assert_eq!(kinds, vec![CallKind::SystemHost]);
}

#[test]
fn branch_8_native_is_recorded() {
    let kinds = kinds_of(|| {
        let _ = evaluate("std.bits.bit_or(1, 2)", &Functions::new());
    });
    assert_eq!(kinds, vec![CallKind::Native]);
}

#[test]
fn branch_9_source_function_is_recorded() {
    let kinds = kinds_of(|| {
        let _ = evaluate("f()", &functions_with("f"));
    });
    assert_eq!(kinds, vec![CallKind::Source]);
}
