//! `sys.explain`: the structure of a relation query and of a function, read from the HIR the executor runs.
//!
//! Explain evaluates nothing and reads no rows. Its operator tree is the pipeline `relation::pipeline` builds, and its
//! function report is the signature, declared effects and reachable tables of the HIR. Both are Rust values here. The
//! `sys.Plan` and `sys.PlanNode` records the spec defines hold `RowRef` handles, snapshot pins and store object refs,
//! which the executor's `Value` cannot carry yet, so the `sys.explain` natives do not return them.

mod function;
mod query;
mod render;

pub use function::{FunctionExplain, explain_function};
pub use query::{Callback, Operator, OperatorKind, explain_query};
pub use render::{render_function, render_operator};
