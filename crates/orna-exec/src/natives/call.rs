//! Calls a closure from a native, the way a value call does.

use crate::binders;
use crate::error::Stop;
use crate::machine::Machine;
use crate::value::Value;

impl Machine<'_> {
    /// Calls `callee` with `arguments` in parameter order. The call charges no step of its own: the body's evaluation
    /// charges, and the native that calls it charges what the old evaluator charged. A callee that is not a closure is
    /// a type error. An argument count or binder the closure does not take is an argument error, as a value call
    /// reports it.
    pub(crate) fn call_closure(
        &mut self,
        callee: &Value,
        arguments: Vec<Value>,
    ) -> Result<Value, Stop> {
        let Value::Closure(closure) = callee else {
            return Err(Stop::Error("ORNA-EVAL-TYPE"));
        };
        let program = self.program;
        let body = &program.bodies[closure.body];
        if arguments.len() != body.params.len() || binders::binds_a_name_twice(body) {
            return Err(Stop::Error("ORNA-EVAL-ARGUMENT"));
        }
        let arguments = arguments.into_iter().enumerate().collect();
        self.invoke(closure.body, arguments, &closure.captured, closure.env.clone())
    }
}
