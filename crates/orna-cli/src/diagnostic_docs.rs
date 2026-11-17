use super::Diagnostic;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DiagnosticDocumentation {
    pub(crate) code: &'static str,
    pub(crate) title: &'static str,
    pub(crate) explanation: &'static str,
    pub(crate) help: &'static str,
}

/// The catalogued diagnostics `orna explain` documents and the REPL and
/// one-shot runners publish runtime failures under.
pub(crate) const DIAGNOSTIC_DOCUMENTATION: &[DiagnosticDocumentation] = &[
    DiagnosticDocumentation {
        code: "ORNA-S010-IMPORT",
        title: "imported module is unavailable",
        explanation: "The project names a module that is not part of its captured dependencies, so the module cannot be checked or executed.",
        help: "use a captured standard dependency or remove the import",
    },
    DiagnosticDocumentation {
        code: "ORNA-S011-AMBIGUOUS",
        title: "standard import name is ambiguous",
        explanation: "Two standard imports bind the same unqualified name to different functions, so the name cannot be resolved.",
        help: "import the functions under distinct names or use their qualified paths",
    },
    DiagnosticDocumentation {
        code: "ORNA-S012-UNRESOLVED",
        title: "name could not be resolved",
        explanation: "The source refers to a name that is not declared or imported in the current namespace.",
        help: "declare the name or add the matching `use` import before using it",
    },
    DiagnosticDocumentation {
        code: "ORNA-S021-TYPE",
        title: "expression has the wrong type",
        explanation: "The value produced by an expression does not satisfy the type required at that source location.",
        help: "change the expression or its declared type so the value and requirement agree",
    },
    DiagnosticDocumentation {
        code: "ORNA-REPL-EFFECT",
        title: "REPL preview cannot perform an effect",
        explanation: "Interactive previews are read-only and cannot perform network, filesystem, process, or other external effects.",
        help: "evaluate a pure expression or invoke the operation through its admitted runtime entry point",
    },
    DiagnosticDocumentation {
        code: "ORNA-REPL-EFFECT-UNCERTAIN",
        title: "REPL effect outcome is uncertain",
        explanation: "The input was evaluated before its request was durably committed, so external effects may already have happened. The outcome is uncertain and recovery is pending; the request is not replayed.",
        help: "check the external system for the effect before repeating the input, because repeating it may repeat an external effect",
    },
    DiagnosticDocumentation {
        code: "ORNA-REPL-AT",
        title: "snapshot selection failed",
        explanation: "The requested CWD, HEAD, or reference snapshot could not be loaded as an admitted REPL context.",
        help: "choose an existing snapshot and keep the repository available while selecting it",
    },
    DiagnosticDocumentation {
        code: "ORNA091-E-RETURN-ARROW",
        title: "function return type uses the arrow form",
        explanation: "Orna function declarations separate the parameter list from the return type with a colon.",
        help: "replace `->` with `:` in the function declaration",
    },
    DiagnosticDocumentation {
        code: "ORNA091-E-VAR",
        title: "local declaration uses `var`",
        explanation: "Orna uses `let` for local declarations; reassignment updates that binding when permitted.",
        help: "replace `var` with `let`",
    },
    DiagnosticDocumentation {
        code: "ORNA-A091-001",
        title: "refined type uses the legacy `where self` form",
        explanation: "Refined Orna types state their invariant in a brace-delimited assertion block owned by the type.",
        help: "replace `where self` with a brace-delimited `assert` block",
    },
    DiagnosticDocumentation {
        code: "ORNA-A091-002",
        title: "table assertion repeats its owner",
        explanation: "A table assertion is already evaluated for its owning table, so an explicit owner pipeline is redundant.",
        help: "remove the leading `self |` or table-name pipeline from the assertion",
    },
    DiagnosticDocumentation {
        code: "ORNA-A091-003",
        title: "module assertion belongs to one table",
        explanation: "An assertion that depends on only one table must live with that table so its owner is explicit.",
        help: "move the assertion into the table body it depends on",
    },
    DiagnosticDocumentation {
        code: "ORNA-A091-004",
        title: "assertion predicate has the wrong owner",
        explanation: "The declaration's predicate does not describe the type or table that owns the assertion.",
        help: "rewrite the predicate for the reported owner type",
    },
    DiagnosticDocumentation {
        code: "ORNA-A091-005",
        title: "assertion is missing its terminator",
        explanation: "An assertion clause must end with a semicolon before the next declaration.",
        help: "terminate the assertion clause with `;`",
    },
    DiagnosticDocumentation {
        code: "ORNA-A091-006",
        title: "assertion uses a dedicated recovery branch",
        explanation: "Assertions report failed propositions; they do not provide an assertion-specific `else` branch.",
        help: "use ordinary failure, recovery, or control flow instead",
    },
    DiagnosticDocumentation {
        code: "ORNA-A091-007",
        title: "assertion has a forbidden effect",
        explanation: "Declaration and refined assertions must be deterministic and cannot perform external effects.",
        help: "remove the identified effect or nondeterministic operation",
    },
    DiagnosticDocumentation {
        code: "ORNA-A091-008",
        title: "assertion is false",
        explanation: "The executable or refined assertion does not hold for the value being checked.",
        help: "inspect the owner and proposition, then provide a safe value",
    },
    DiagnosticDocumentation {
        code: "ORNA-A091-009",
        title: "table or cross-table assertion is false",
        explanation: "A table or cross-table invariant does not hold for the deterministic rows being checked.",
        help: "inspect the reported witness and repair the affected table data",
    },
    DiagnosticDocumentation {
        code: "ORNA-A091-010",
        title: "assertion form is unsupported",
        explanation: "Orna 1.0 supports the sole owner-local `assert` form for declarations and tables.",
        help: "rewrite the construct using the supported `assert` form",
    },
    DiagnosticDocumentation {
        code: "ORNA-A091-011",
        title: "assertion is empty",
        explanation: "An assertion must contain a proposition that can be checked.",
        help: "provide a proposition after `assert`",
    },
    DiagnosticDocumentation {
        code: "ORNA-A091-012",
        title: "module assertion has no table dependency",
        explanation: "A module assertion is only valid when it depends on a table; otherwise it has no executable owner.",
        help: "place the check in executable or test code",
    },
    DiagnosticDocumentation {
        code: "ORNA091-E-MATCH",
        title: "value branching uses the legacy `match` form",
        explanation: "Orna branches on values with `case` and colon-separated arms rather than `match` and arrows.",
        help: "replace `match` with `case` and use `:` for each arm",
    },
    DiagnosticDocumentation {
        code: "ORNA091-E-RESULT",
        title: "source uses an explicit `Result` value",
        explanation: "Orna returns the successful type directly and propagates failures automatically.",
        help: "return the successful type and use `|?` for recovery",
    },
    DiagnosticDocumentation {
        code: "ORNA091-E-POSTFIX-QUESTION",
        title: "operation uses postfix failure propagation",
        explanation: "Orna propagates operation failures automatically instead of using a postfix `?` operator.",
        help: "remove the postfix `?` and use `|?` only for recovery",
    },
    DiagnosticDocumentation {
        code: "ORNA091-E-CURRENCY",
        title: "source uses a special currency declaration",
        explanation: "Currencies are nominal types with a nested `Currency` implementation, not a separate declaration form.",
        help: "replace the currency declaration with a nominal type and nested `impl Currency`",
    },
    DiagnosticDocumentation {
        code: "ORNA091-E-CURRENCY-SYMBOL",
        title: "currency declares a universal symbol",
        explanation: "Currency symbols and placement depend on locale formatter data rather than a universal static field.",
        help: "move symbol and placement choices to the locale-aware money formatter",
    },
    DiagnosticDocumentation {
        code: "ORNA091-E-IMPL-FOR",
        title: "protocol implementation uses the legacy top-level form",
        explanation: "Orna protocol implementations are nested inside the type that owns them.",
        help: "nest `impl P` inside the owning type instead of writing `impl P for T`",
    },
    DiagnosticDocumentation {
        code: "ORNA091-E-BOUND-COLON",
        title: "generic bound uses the legacy colon form",
        explanation: "Generic protocol bounds use the `impl` keyword in Orna type parameters.",
        help: "replace `<T: P>` with `<T impl P>`",
    },
    DiagnosticDocumentation {
        code: "ORNA091-E-STATIC-FN",
        title: "protocol declares a static function",
        explanation: "Protocols expose static properties or ordinary functions rather than static function declarations.",
        help: "use a static property or an ordinary function",
    },
    DiagnosticDocumentation {
        code: "ORNA091-E-TRYFROM",
        title: "source uses `TryFrom`",
        explanation: "Orna's `From<S>` conversion may fail without wrapping the successful value in a separate result type.",
        help: "use `From<S>` for a conversion that may fail",
    },
    DiagnosticDocumentation {
        code: "ORNA091-E-CONVERSION-CHAIN",
        title: "conversion chain is implicit",
        explanation: "Orna does not search for unspoken multi-step conversions between types.",
        help: "name each conversion step explicitly",
    },
    DiagnosticDocumentation {
        code: "ORNA091-E-OPAQUE",
        title: "source uses an opaque type declaration",
        explanation: "Orna uses one nominal or refined `type` form instead of a separate opaque declaration.",
        help: "replace `opaque Name` with the unified nominal or refined `type` form",
    },
    DiagnosticDocumentation {
        code: "ORNA091-E-FIELD-CONSTRAINT",
        title: "field declares a legacy constraint",
        explanation: "Field uniqueness and checks are table assertions owned by the table, not field suffixes.",
        help: "write an owner-local table assertion using `all_unique` or `every`",
    },
    DiagnosticDocumentation {
        code: "ORNA-CMD-ARGUMENT-MISSING",
        title: "required argument is missing",
        explanation: "The command line ends before a word the command requires, so the command cannot choose what to do.",
        help: "supply the missing argument, then run the command again; `orna help` lists the forms",
    },
    DiagnosticDocumentation {
        code: "ORNA-CMD-ARGUMENT-INVALID",
        title: "argument is not recognised or not valid",
        explanation: "A command name, flag, value or extra word is not accepted by this command, so nothing was changed.",
        help: "correct the spelling or value, or remove the word, then run the command again; `orna help` lists the forms",
    },
    DiagnosticDocumentation {
        code: "ORNA-CMD-ARGUMENT-UNEXPECTED",
        title: "argument is not allowed here",
        explanation: "A flag or word appears where this command does not accept it, so the command was refused before it ran.",
        help: "move the flag to a position the command accepts, or remove it, then run the command again",
    },
    DiagnosticDocumentation {
        code: "ORNA-CMD-ENDPOINT-INVALID",
        title: "endpoint is not valid",
        explanation: "The endpoint names a scheme, host, port, path or socket that Orna does not accept, so no connection was attempted.",
        help: "use a local path, an absolute Orna Unix socket URI, or a secure Orna URI",
    },
    DiagnosticDocumentation {
        code: "ORNA-CMD-ENDPOINT-CREDENTIALS",
        title: "endpoint carries credentials or debug options",
        explanation: "The endpoint contains unsupported credentials, query parameters or fragments, so no connection was attempted.",
        help: "use transport authentication and omit credentials, fragments, and query parameters",
    },
    DiagnosticDocumentation {
        code: "ORNA-CMD-TARGET-UNAVAILABLE",
        title: "target is unavailable",
        explanation: "The command named a target, such as a row, table, mount, snapshot or Git object, that does not exist in the current state or cannot be used now.",
        help: "check the target's name and state, then retry the command",
    },
    DiagnosticDocumentation {
        code: "ORNA-CMD-ANNOTATE-FAILED",
        title: "annotation was not applied",
        explanation: "The annotation could not be applied: its media type is invalid, or its output could not be read, staged or installed.",
        help: "check the media type, source file and destination named in the message, then run the command again",
    },
    DiagnosticDocumentation {
        code: "ORNA-CMD-MOUNT-OUTCOME-UNKNOWN",
        title: "mount outcome is unknown",
        explanation: "The mount stopped at a point where Orna cannot tell whether the change was accepted, so the state must be inspected before anything is retried.",
        help: "inspect the mount and the listed drafts before retrying, so an accepted change is not applied twice",
    },
    DiagnosticDocumentation {
        code: "ORNA-CMD-REPOSITORY-UNAVAILABLE",
        title: "repository state could not be read",
        explanation: "The project's repository or source tree could not be read or loaded, so the command has no trusted state to act on.",
        help: "fix the repository or source boundary the message names, then run the command again",
    },
    DiagnosticDocumentation {
        code: "ORNA-CMD-REMOTE-FAILED",
        title: "remote operation failed",
        explanation: "Git or the remote did not complete the requested fetch, push or ref operation, so the local state was not changed to match it.",
        help: "check the remote's address, credentials and availability, then retry the command",
    },
    DiagnosticDocumentation {
        code: "ORNA-CMD-SEMANTIC-FAILED",
        title: "project semantic analysis failed",
        explanation: "The project's source does not type-check or does not satisfy its semantic rules, so the command refused to run or publish it.",
        help: "fix the first reported source error, then run the command again",
    },
    DiagnosticDocumentation {
        code: "ORNA-CMD-NO-SERVING-ENTRY",
        title: "no serving entry is declared",
        explanation: "`orna serve` needs the project's main.orna to declare `pub fn serve()`, and it does not.",
        help: "add `pub fn serve()` to main.orna to serve `/`; Git transport remains available",
    },
    DiagnosticDocumentation {
        code: "ORNA-CMD-NO-ROOT-MAIN",
        title: "no root main() is declared",
        explanation: "`orna run` needs a root `main()` function in the project and the project has none.",
        help: "declare `main()` in main.orna, or run a qualified function such as `orna run MODULE.FUNCTION`",
    },
    DiagnosticDocumentation {
        code: "ORNA-CMD-RUNTIME-UNAVAILABLE",
        title: "runtime is unavailable",
        explanation: "The durable runtime could not start or complete the requested operation, so no result was published.",
        help: "check the repository and runtime state the message names, then retry the command or start a new session",
    },
    DiagnosticDocumentation {
        code: "ORNA-CMD-OUTPUT-FAILED",
        title: "command output could not be written",
        explanation: "The command completed its work but its output stream or terminal rejected the write, so the result could not be shown.",
        help: "check that the output stream or terminal is available, then retry the command or start a new session",
    },
    DiagnosticDocumentation {
        code: "ORNA-CMD-WORKFLOW-REFUSED",
        title: "workflow step was refused",
        explanation: "A workflow step, such as publication, commit, staging or a serve request, was refused because its preconditions do not hold, so nothing was published.",
        help: "follow the remedy the message names, then retry the workflow step",
    },
    DiagnosticDocumentation {
        code: "ORNA-CMD-GIT-IDENTITY-MISSING",
        title: "Git identity is missing",
        explanation: "Git has no author or committer identity for the commit, so Orna cannot record who made it.",
        help: "set `user.name` and `user.email` for the repository, then commit again",
    },
    DiagnosticDocumentation {
        code: "ORNA-CMD-CANDIDATE-INVALID",
        title: "candidate is invalid",
        explanation: "The staged or prepared candidate does not match the committed state it must be built from, so it cannot be published.",
        help: "restage the change from the committed state, then commit again",
    },
    DiagnosticDocumentation {
        code: "ORNA-CMD-CANDIDATE-CHANGED",
        title: "candidate changed after it was prepared",
        explanation: "The candidate changed between preparation and publication, so the prepared result no longer describes what would be committed.",
        help: "prepare the candidate again, then commit it",
    },
    DiagnosticDocumentation {
        code: "ORNA-CMD-NOTHING-TO-COMMIT",
        title: "nothing is staged to commit",
        explanation: "The index holds no change that differs from the committed state, so there is nothing to record.",
        help: "stage changes with `orna add` or `git add`, then retry `orna commit -m MESSAGE`",
    },
    DiagnosticDocumentation {
        code: "ORNA-EVAL-AMBIGUOUS",
        title: "name is ambiguous at run time",
        explanation: "The name resolves to more than one binding when the expression runs, so the evaluator cannot choose one.",
        help: "rename or qualify the conflicting names so each refers to one binding",
    },
    DiagnosticDocumentation {
        code: "ORNA-EVAL-ARGUMENT",
        title: "argument does not match its parameter",
        explanation: "A function, intrinsic or table operation received an argument that its declared parameters do not accept.",
        help: "pass arguments that match the declared parameters and their types",
    },
    DiagnosticDocumentation {
        code: "ORNA-EVAL-BROWSER",
        title: "browser profile failed",
        explanation: "The browser profile could not be admitted or evaluated, so its source was not run.",
        help: "fix the browser profile source or its declared capabilities, then retry",
    },
    DiagnosticDocumentation {
        code: "ORNA-EVAL-BROWSER-ENTRY",
        title: "browser profile entry is invalid",
        explanation: "The browser profile entry point is missing, ambiguous or not a supported callable.",
        help: "declare exactly one supported entry point in the browser profile",
    },
    DiagnosticDocumentation {
        code: "ORNA-EVAL-CANCELLED",
        title: "evaluation was cancelled",
        explanation: "Evaluation stopped because cancellation was requested, so no result was produced.",
        help: "retry the evaluation; a cancelled evaluation commits no partial results",
    },
    DiagnosticDocumentation {
        code: "ORNA-EVAL-DIVIDE-BY-ZERO",
        title: "division by zero",
        explanation: "An arithmetic operation divided by zero, so the expression has no value.",
        help: "guard the divisor so it is never zero before dividing",
    },
    DiagnosticDocumentation {
        code: "ORNA-EVAL-ERROR",
        title: "evaluation raised an error",
        explanation: "A function or error value raised a failure that the evaluator carries together with its causes.",
        help: "handle the error where it is produced, or correct the input that causes it",
    },
    DiagnosticDocumentation {
        code: "ORNA-EVAL-FIELD",
        title: "field is unavailable",
        explanation: "The value has no field with the requested name, or the field cannot be read here.",
        help: "read a field the value declares, or check the value's type first",
    },
    DiagnosticDocumentation {
        code: "ORNA-EVAL-IMMUTABLE-CAPTURE",
        title: "captured value is immutable",
        explanation: "A closure or deferred computation tried to change a value it captured, and captured values cannot change.",
        help: "bind a new value instead of changing the captured one",
    },
    DiagnosticDocumentation {
        code: "ORNA-EVAL-INDEX",
        title: "index is out of range",
        explanation: "The index points outside the bounds of the collection, so no element exists there.",
        help: "check the length before indexing, or use a checked lookup",
    },
    DiagnosticDocumentation {
        code: "ORNA-EVAL-LIMIT",
        title: "evaluation limit exceeded",
        explanation: "Evaluation exceeded a configured bound such as recursion depth, size or count, so it stopped before completing.",
        help: "reduce the input size or recursion depth, or split the work into smaller steps",
    },
    DiagnosticDocumentation {
        code: "ORNA-EVAL-NAME",
        title: "name is not bound",
        explanation: "The expression refers to a name that is not bound at run time in the evaluated scope.",
        help: "declare the name, import it, or evaluate the expression in a scope that binds it",
    },
    DiagnosticDocumentation {
        code: "ORNA-EVAL-NO-MATCH",
        title: "no pattern matched",
        explanation: "No arm of the match expression matched the value, so there is no result.",
        help: "add an arm that covers this value, or a catch-all arm",
    },
    DiagnosticDocumentation {
        code: "ORNA-EVAL-PARSE",
        title: "source could not be parsed",
        explanation: "The submitted source is not valid Orna syntax, so it cannot be evaluated.",
        help: "fix the syntax at the reported location, then evaluate again",
    },
    DiagnosticDocumentation {
        code: "ORNA-EVAL-RELATION-ONE-MULTIPLE",
        title: "one-to-many relation is violated",
        explanation: "A relation expected one or more related rows, but the stored rows do not satisfy that cardinality.",
        help: "correct the related rows so the relation's cardinality holds",
    },
    DiagnosticDocumentation {
        code: "ORNA-EVAL-RELATION-ONE-ZERO",
        title: "required relation is missing",
        explanation: "A relation expected exactly one related row, but none or more than one exists.",
        help: "make the relation point at exactly one existing row",
    },
    DiagnosticDocumentation {
        code: "ORNA-EVAL-TABLE-ARGUMENT",
        title: "table argument is invalid",
        explanation: "A table operation received an argument that does not match the table's declared columns or key.",
        help: "pass values that match the table's declared columns and key",
    },
    DiagnosticDocumentation {
        code: "ORNA-EVAL-TABLE-DUPLICATE-KEY",
        title: "table key already exists",
        explanation: "The operation would insert a row whose key already exists in the table.",
        help: "use a key that is not yet present, or update the existing row",
    },
    DiagnosticDocumentation {
        code: "ORNA-EVAL-TABLE-KEY",
        title: "table key is invalid",
        explanation: "The key does not identify a row of the table with the shape the table declares.",
        help: "supply a key with the table's declared key shape",
    },
    DiagnosticDocumentation {
        code: "ORNA-EVAL-TABLE-MISSING-ROW",
        title: "table row is missing",
        explanation: "The operation names a row that does not exist in the table.",
        help: "check the key, or insert the row before updating or reading it",
    },
    DiagnosticDocumentation {
        code: "ORNA-EVAL-TABLE-ROW",
        title: "table row is invalid",
        explanation: "A row does not satisfy the table's declared columns or constraints.",
        help: "correct the row values to match the table's declared columns",
    },
    DiagnosticDocumentation {
        code: "ORNA-EVAL-TABLE-SNAPSHOT",
        title: "table snapshot is unavailable",
        explanation: "The table snapshot this evaluation needs is missing or does not match the table.",
        help: "retry against the current table state, or admit the table again",
    },
    DiagnosticDocumentation {
        code: "ORNA-EVAL-TABLE-UNADMITTED",
        title: "table is not admitted",
        explanation: "The table is referenced but was not admitted to this evaluation.",
        help: "declare the table in the project and admit it before using it",
    },
    DiagnosticDocumentation {
        code: "ORNA-EVAL-TIMEOUT",
        title: "evaluation timed out",
        explanation: "Evaluation exceeded its time bound and was stopped before it completed.",
        help: "reduce the work per evaluation or retry with a longer bound",
    },
    DiagnosticDocumentation {
        code: "ORNA-EVAL-TYPE",
        title: "value has the wrong type",
        explanation: "A value does not have the type the operation requires when the expression runs.",
        help: "convert or select a value of the required type",
    },
    DiagnosticDocumentation {
        code: "ORNA-EVAL-UNSUPPORTED",
        title: "operation is not supported",
        explanation: "This evaluator does not support the operation, so it was not run.",
        help: "use a supported operation, or the integrated runtime that provides it",
    },
    DiagnosticDocumentation {
        code: "ORNA-EVAL-VALUE",
        title: "value cannot be produced",
        explanation: "The evaluator cannot represent or return this value, so no result is produced.",
        help: "return a value the evaluator can represent, such as a supported scalar or collection",
    },
    DiagnosticDocumentation {
        code: "ORNA-REF-DANGLING",
        title: "stored reference names no row",
        explanation: "A stored table reference must name a live row of its table when the activation commits (ORNA-MUT-007). An activation that leaves a reference without its row is refused and none of its writes commit: a reference to a row that does not exist, or the delete or re-key of a row that a reference still names. Non-optional references default to `restrict` and Orna does not cascade (ORNA-MUT-008, ORNA-REF-003).",
        help: "insert the referenced row, or update the dependent references to a live row in the same activation, before deleting or re-keying the row they name",
    },
];

pub(crate) fn diagnostic_documentation(code: &str) -> Option<DiagnosticDocumentation> {
    DIAGNOSTIC_DOCUMENTATION
        .iter()
        .find(|documentation| documentation.code == code)
        .copied()
}

pub(crate) fn explain_diagnostic(code: &str) -> Result<String, Diagnostic> {
    let Some(documentation) = diagnostic_documentation(code) else {
        return Err(Diagnostic::usage(
            "ORNA-CMD-ARGUMENT-INVALID",
            "diagnostic code is unknown",
            "supply a code printed by an Orna diagnostic, such as `ORNA-S010-IMPORT`",
        ));
    };
    Ok(format!(
        "{}: {}\n{}\nhelp: {}",
        documentation.code, documentation.title, documentation.explanation, documentation.help
    ))
}
