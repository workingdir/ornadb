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
            "E1002",
            "diagnostic code is unknown",
            "supply a code printed by an Orna diagnostic, such as `ORNA-S010-IMPORT`",
        ));
    };
    Ok(format!(
        "{}: {}\n{}\nhelp: {}",
        documentation.code, documentation.title, documentation.explanation, documentation.help
    ))
}
