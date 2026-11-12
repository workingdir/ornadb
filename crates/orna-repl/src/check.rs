//! Lowers and typechecks a composed module, then evaluates its entry on the executor.

use orna_exec::{ExecError, ExecLimits, Globals, MemoryStore, Value, measure_entry_in};
use orna_hir::{
    DefId, DefKind, ModulePath, Name, PreludeExports, Program, lower_with_prelude, print_type,
    typeck,
};
use orna_syntax::parse_session_module_with_file;

use crate::compose::ENTRY;
use crate::error::ReplError;
use crate::standard;

/// The module path the session's source lowers under.
const MODULE: &str = "repl";

/// The bounds the old REPL evaluator applied to one submission.
const LIMITS: ExecLimits = ExecLimits {
    max_steps: 419_600,
    max_depth: 64,
    max_collection_items: 2_048,
    max_string_bytes: 16_384,
    max_integer_digits: 1_024,
};

/// What one entry evaluated to. An effectful entry is typed but not run, so it has no value.
pub(crate) struct Evaluated {
    pub(crate) value: Option<Value>,
    pub(crate) ty: String,
    pub(crate) effectful: bool,
}

/// Lowers and typechecks `source`. The first diagnostic raised in the session module rejects it.
pub(crate) fn lower_checked(source: &str) -> Result<Program, ReplError> {
    let parsed = parse_session_module_with_file(source, "repl.orna");
    if !parsed.is_ok() {
        return Err(ReplError::new(
            "ORNA-REPL-INTERNAL",
            "the composed session source does not parse",
        ));
    }
    let prelude = PreludeExports::default().with_module(
        ModulePath::new([Name::new("std"), Name::new("prelude")]),
        orna_standard_sources::REFERENCE_STANDARD_PRELUDE_EXPORTS_V1
            .iter()
            .copied(),
    );
    let mut program = lower_with_prelude(
        &standard::units_with(&parsed.value, ModulePath::new([Name::new(MODULE)])),
        &prelude,
    );
    typeck(&mut program);
    if let Err(errors) = program.validate() {
        return Err(ReplError::new(
            "ORNA-REPL-INTERNAL",
            format!("the lowered session module is invalid: {errors:?}"),
        ));
    }
    let first = program
        .diagnostics()
        .find(|(module, _)| program.modules[*module].path.to_string() == MODULE)
        .map(|(_, diagnostic)| (diagnostic.code, diagnostic.message.to_string()));
    match first {
        Some((code, message)) => Err(ReplError::new(code, message)),
        None => Ok(program),
    }
}

/// Lowers `source`, finds its entry, and evaluates it with `arguments` in parameter order unless it is effectful.
/// Each `seed` is the value of the module-level `let` of that name, so that `let` is kept and not evaluated again.
pub(crate) fn evaluate(
    source: &str,
    arguments: Vec<Value>,
    seeds: &[(String, Value)],
) -> Result<Evaluated, ReplError> {
    let program = lower_checked(source)?;
    let entry = module_def(&program, ENTRY, |kind| matches!(kind, DefKind::Fn(_)))
        .ok_or_else(|| ReplError::new("ORNA-REPL-INTERNAL", "the composed session has no entry"))?;
    let DefKind::Fn(function) = &program.defs[entry].kind else {
        return Err(ReplError::new(
            "ORNA-REPL-INTERNAL",
            "the entry is not a function",
        ));
    };
    let ty = print_type(&program, function.ret);
    if !function.effects.is_empty() {
        return Ok(Evaluated {
            value: None,
            ty,
            effectful: true,
        });
    }
    let mut globals = Globals::new();
    for (name, value) in seeds {
        let def = module_def(&program, name, |kind| matches!(kind, DefKind::Let(_))).ok_or_else(
            || {
                ReplError::new(
                    "ORNA-REPL-INTERNAL",
                    format!("the session global {name} is not a module let"),
                )
            },
        )?;
        globals.insert(def, value.clone());
    }
    let (result, _store) = measure_entry_in(
        &program,
        entry,
        arguments,
        LIMITS,
        Box::new(MemoryStore::new()),
        &mut globals,
    );
    let measured = result.map_err(exec_error)?;
    Ok(Evaluated {
        value: Some(measured.value),
        ty,
        effectful: false,
    })
}

/// The definition named `name` in the session module, of a kind `kind` accepts.
fn module_def(program: &Program, name: &str, kind: impl Fn(&DefKind) -> bool) -> Option<DefId> {
    program
        .modules
        .iter()
        .filter(|module| module.path.to_string() == MODULE)
        .flat_map(|module| module.items.iter().copied())
        .find(|def| program.defs[*def].name.as_str() == name && kind(&program.defs[*def].kind))
}

fn exec_error(error: ExecError) -> ReplError {
    match error {
        ExecError::Code(code) => ReplError::new(code, "the input failed at runtime"),
        ExecError::NotYet(construct) => ReplError::new(
            "ORNA-EVAL-UNSUPPORTED",
            format!("{construct} is not executed yet"),
        ),
        ExecError::Cancelled => ReplError::new("ORNA-REPL-CANCELLED", "the input was cancelled"),
    }
}
