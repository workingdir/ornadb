//! Stage N1 of the native declaration form: the grammar, AST and parser of
//! `docs/spec-extensions/NATIVE-1.md` (ORNA-NATIVE-001, -002, -003 and -009 and
//! the parser diagnostics of its section 3).
//!
//! Every case reads a corpus fixture under `tests/fixtures/` and pins the exact
//! diagnostics: one fixture per admissible form and one per rejection.

use orna_syntax::{
    Declaration, EFFECT_CLASSES, FunctionSignature, ItemKind, NativeEffectSet, Pattern, ReplInput,
    SyntaxDiagnostic, TokenKind, lex, parse_expression, parse_module, parse_repl, parse_row,
};

const TRIM: &str = include_str!("fixtures/native-trim.orna");
const MEAN: &str = include_str!("fixtures/native-mean.orna");
const BITS: &str = include_str!("fixtures/native-bits.orna");
const RANDOM: &str = include_str!("fixtures/native-random.orna");
const PARALLEL: &str = include_str!("fixtures/native-parallel.orna");
const HASH: &str = include_str!("fixtures/native-hash.orna");
const WINDOW_RATE: &str = include_str!("fixtures/native-window-rate.orna");
const CONTEXTUAL_NAMES: &str = include_str!("fixtures/native-contextual-names.orna");

const BAD_RESULT_TYPE: &str = include_str!("fixtures/bad-native-result-type.orna");
const BAD_PARAMETER_TYPE: &str = include_str!("fixtures/bad-native-parameter-type.orna");
const BAD_SELF: &str = include_str!("fixtures/bad-native-self.orna");
const BAD_EFFECT_MISSING: &str = include_str!("fixtures/bad-native-effect-missing.orna");
const BAD_EFFECT_CLASS: &str = include_str!("fixtures/bad-native-effect-class.orna");
const BAD_EFFECT_EMPTY: &str = include_str!("fixtures/bad-native-effect-empty.orna");
const BAD_BODY: &str = include_str!("fixtures/bad-native-body.orna");
const BAD_BLOCK_BODY: &str = include_str!("fixtures/bad-native-block-body.orna");
const BAD_SEMICOLON: &str = include_str!("fixtures/bad-native-semicolon.orna");
const BAD_PLACE_IMPL: &str = include_str!("fixtures/bad-native-place-impl.orna");
const BAD_PLACE_PROTOCOL: &str = include_str!("fixtures/bad-native-place-protocol.orna");
const BAD_PLACE_TABLE: &str = include_str!("fixtures/bad-native-place-table.orna");
const BAD_PLACE_TYPE: &str = include_str!("fixtures/bad-native-place-type.orna");
const BAD_PLACE_ENUM: &str = include_str!("fixtures/bad-native-place-enum.orna");
const BAD_PLACE_BLOCK: &str = include_str!("fixtures/bad-native-place-block.orna");
const BAD_PLACE_ROW: &str = include_str!("fixtures/bad-native-place-row.orna");
const BAD_DECLARE: &str = include_str!("fixtures/bad-declare-without-native.orna");

fn diagnostics(diagnostics: &[SyntaxDiagnostic]) -> Vec<(&'static str, &str)> {
    diagnostics
        .iter()
        .map(|diagnostic| (diagnostic.code, diagnostic.message.as_str()))
        .collect()
}

fn assert_rejected(observed: &[SyntaxDiagnostic], expected: &[(&str, &str)]) {
    assert_eq!(diagnostics(observed), expected);
}

fn module_items(source: &str) -> Vec<orna_syntax::Item> {
    let parsed = parse_module(source);
    assert!(
        parsed.is_ok(),
        "{source} should parse, got {:?}",
        diagnostics(&parsed.diagnostics)
    );
    parsed.value.items
}

fn native_declaration(source: &str) -> (FunctionSignature, Option<NativeEffectSet>) {
    let mut items = module_items(source).into_iter();
    let (Some(item), None) = (items.next(), items.next()) else {
        panic!("{source} should be exactly one item");
    };
    assert_eq!(item.kind, ItemKind::Native);
    let Declaration::Native { signature, effects } = item.declaration else {
        panic!("{source} should declare a native function");
    };
    (signature, effects)
}

fn names(signature: &FunctionSignature) -> Vec<&str> {
    signature
        .parameters
        .iter()
        .map(|parameter| match &parameter.pattern {
            Pattern::Name(name, _) => name.as_str(),
            other => panic!("native parameters are plain identifiers, got {other:?}"),
        })
        .collect()
}

fn classes(effects: &Option<NativeEffectSet>) -> Vec<&str> {
    match effects {
        Some(NativeEffectSet::Classes { classes, .. }) => {
            classes.iter().map(|class| class.text.as_str()).collect()
        }
        Some(NativeEffectSet::None { .. }) => Vec::new(),
        None => panic!("a native declaration states its effect clause"),
    }
}

fn is_none(effects: &Option<NativeEffectSet>) -> bool {
    matches!(effects, Some(NativeEffectSet::None { .. }))
}

#[test]
fn native_declaration_is_accepted() {
    for source in [
        TRIM,
        MEAN,
        BITS,
        RANDOM,
        PARALLEL,
        HASH,
        WINDOW_RATE,
        CONTEXTUAL_NAMES,
    ] {
        module_items(source);
    }

    // std.text: today a marker leaf behind a forwarder.
    let (signature, effects) = native_declaration(TRIM);
    assert_eq!(signature.name, "trim");
    assert_eq!(names(&signature), ["value"]);
    assert!(signature.result.is_some());
    assert!(is_none(&effects));

    // std.stats: generic parameters and declared defaults.
    let (signature, effects) = native_declaration(MEAN);
    assert_eq!(signature.name, "mean");
    assert_eq!(signature.generics.len(), 1);
    assert_eq!(names(&signature), ["rows", "scale", "rounding"]);
    assert_eq!(
        signature
            .parameters
            .iter()
            .filter(|parameter| parameter.default.is_some())
            .count(),
        2
    );
    assert!(signature.result.is_some());
    assert!(is_none(&effects));

    // std.bits: two fully typed parameters.
    let (signature, effects) = native_declaration(BITS);
    assert_eq!(signature.name, "bit_or");
    assert_eq!(names(&signature), ["left", "right"]);
    assert!(
        signature
            .parameters
            .iter()
            .all(|parameter| parameter.annotation.is_some())
    );
    assert!(is_none(&effects));

    // std.random: the operation reads host entropy.
    let (signature, effects) = native_declaration(RANDOM);
    assert_eq!(signature.name, "integer");
    assert_eq!(names(&signature), ["lower_inclusive", "upper_exclusive"]);
    assert_eq!(classes(&effects), ["randomness"]);

    // std.concurrent: a function type inside a list type.
    let (signature, effects) = native_declaration(PARALLEL);
    assert_eq!(signature.name, "parallel");
    assert!(is_none(&effects));

    // std.hash: the hashing itself reads no host state.
    let (signature, effects) = native_declaration(HASH);
    assert_eq!(signature.name, "sha256");
    assert!(is_none(&effects));

    // std.query: a private declaration with a multi-line parameter list.
    let (signature, effects) = native_declaration(WINDOW_RATE);
    assert_eq!(signature.name, "__window_rate");
    assert_eq!(names(&signature), ["points", "size", "step"]);
    assert!(is_none(&effects));
}

#[test]
fn native_declaration_records_every_declared_effect_class() {
    let parsed = parse_module("pub native fn both(path: Str): Blob effect filesystem, network;");
    assert!(parsed.is_ok(), "{:?}", diagnostics(&parsed.diagnostics));
    let Declaration::Native { effects, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a native declaration");
    };
    assert_eq!(classes(effects), ["filesystem", "network"]);
}

#[test]
fn native_declaration_needs_a_result_type() {
    let parsed = parse_module(BAD_RESULT_TYPE);
    assert_rejected(
        &parsed.diagnostics,
        &[(
            "ORNA091-E-NATIVE-TYPE",
            "a native declaration requires a result type",
        )],
    );
}

#[test]
fn native_declaration_needs_parameter_types() {
    let parsed = parse_module(BAD_PARAMETER_TYPE);
    assert_rejected(
        &parsed.diagnostics,
        &[(
            "ORNA091-E-NATIVE-TYPE",
            "a native declaration requires a type for every parameter",
        )],
    );
}

#[test]
fn native_declaration_rejects_self() {
    let parsed = parse_module(BAD_SELF);
    assert_rejected(
        &parsed.diagnostics,
        &[(
            "ORNA091-E-NATIVE-SELF",
            "a native declaration must not declare a `self` parameter",
        )],
    );
}

#[test]
fn native_declaration_needs_an_effect() {
    let parsed = parse_module(BAD_EFFECT_MISSING);
    assert_rejected(
        &parsed.diagnostics,
        &[(
            "ORNA091-E-NATIVE-EFFECT",
            "a native declaration requires an `effect` clause",
        )],
    );
}

#[test]
fn native_declaration_rejects_an_unknown_effect_class() {
    let parsed = parse_module(BAD_EFFECT_CLASS);
    assert_rejected(
        &parsed.diagnostics,
        &[(
            "ORNA091-E-NATIVE-EFFECT",
            "unknown effect class; name a `sys.EffectKind` value or `none`",
        )],
    );

    let parsed = parse_module(BAD_EFFECT_EMPTY);
    assert_rejected(
        &parsed.diagnostics,
        &[(
            "ORNA091-E-NATIVE-EFFECT",
            "expected an effect class name or `none`",
        )],
    );

    let parsed = parse_module("pub native fn f(): Int effect none, clock;");
    assert_rejected(
        &parsed.diagnostics,
        &[(
            "ORNA091-E-NATIVE-EFFECT",
            "`none` is the whole effect set of a native declaration",
        )],
    );
}

#[test]
fn native_declaration_rejects_a_body() {
    let parsed = parse_module(BAD_BODY);
    assert_rejected(
        &parsed.diagnostics,
        &[("ORNA091-E-NATIVE-BODY", "a native declaration has no body")],
    );

    let parsed = parse_module(BAD_BLOCK_BODY);
    assert_rejected(
        &parsed.diagnostics,
        &[("ORNA091-E-NATIVE-BODY", "a native declaration has no body")],
    );
}

#[test]
fn native_declaration_requires_a_semicolon() {
    let parsed = parse_module(BAD_SEMICOLON);
    assert_rejected(
        &parsed.diagnostics,
        &[("ORNA-PARSE-002", "native declarations require `;`")],
    );
}

#[test]
fn native_is_not_a_keyword() {
    // ORNA-LEX-007: the keyword set is unchanged, so `native`, `effect` and
    // every effect class name lex as identifiers.
    let tokens = lex("native effect none clock").expect("the source lexes");
    for token in &tokens[..4] {
        assert!(
            matches!(&token.kind, TokenKind::Identifier { .. }),
            "{} must stay an identifier",
            token.text
        );
    }
    for class in EFFECT_CLASSES {
        let tokens = lex(class).expect("the class name lexes");
        assert!(
            matches!(&tokens[0].kind, TokenKind::Identifier { .. }),
            "{class} must stay an identifier"
        );
    }

    // Each remains usable as a function name, member name, field name and
    // call target.
    for source in [
        "fn native(): Int = 1;",
        "fn effect(): Int = 1;",
        "fn f(native: Int, effect: Int): Int = native;",
        "type T { native: Int, effect: Int, }",
        "table T { native: Int, effect: Int, }",
    ] {
        module_items(source);
    }
    for source in [
        "x.native",
        "x.effect",
        "{native: 1}",
        "{effect: 1}",
        "native(x)",
        "clock(x)",
    ] {
        let parsed = parse_expression(source);
        assert!(
            parsed.is_ok(),
            "{source} should parse, got {:?}",
            diagnostics(&parsed.diagnostics)
        );
    }

    // The declared names of the declaration form itself are ordinary
    // identifiers: one fixture carries both on the same source lines.
    let items = module_items(CONTEXTUAL_NAMES);
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].kind, ItemKind::Native);
    assert_eq!(items[1].kind, ItemKind::Function);
    let Declaration::Native { signature, effects } = &items[0].declaration else {
        panic!("expected a native declaration");
    };
    assert_eq!(signature.name, "native");
    assert_eq!(names(signature), ["effect"]);
    assert!(is_none(effects));
}

#[test]
fn native_declaration_is_top_level_only() {
    let place = "a native declaration is permitted only as a module top-level item";
    for source in [
        BAD_PLACE_IMPL,
        BAD_PLACE_PROTOCOL,
        BAD_PLACE_TABLE,
        BAD_PLACE_TYPE,
        BAD_PLACE_ENUM,
        BAD_PLACE_BLOCK,
    ] {
        let parsed = parse_module(source);
        assert_rejected(&parsed.diagnostics, &[("ORNA091-E-NATIVE-PLACE", place)]);
    }

    // A visibility prefix belongs to the declaration, so a nested
    // `pub native fn` is the same rejected production.
    for source in [
        "fn g(): Int { pub native fn f(): Int effect none; 1 }",
        "type T { pub native fn f(): Int effect none; }",
    ] {
        let parsed = parse_module(source);
        assert_rejected(&parsed.diagnostics, &[("ORNA091-E-NATIVE-PLACE", place)]);
    }

    let parsed = parse_row(BAD_PLACE_ROW);
    assert_rejected(&parsed.diagnostics, &[("ORNA091-E-NATIVE-PLACE", place)]);

    // `repl_input` does not admit the declaration either.
    let parsed = parse_repl(TRIM);
    assert_rejected(
        &parsed.diagnostics,
        &[(
            "ORNA091-E-NATIVE-PLACE",
            "the REPL does not admit native declarations",
        )],
    );
    assert!(matches!(
        parsed.value,
        ReplInput::Expression(orna_syntax::Expr::Tuple { .. })
    ));
}

#[test]
fn declare_without_native_is_still_an_error() {
    let parsed = parse_module(BAD_DECLARE);
    assert_rejected(
        &parsed.diagnostics,
        &[(
            "ORNA-PARSE-001",
            "module top level accepts declarations only",
        )],
    );
}

#[test]
fn native_declaration_visibility_and_arity_follow_the_grammar() {
    let parsed = parse_module("native fn f(): Int effect none;");
    assert!(parsed.is_ok(), "{:?}", diagnostics(&parsed.diagnostics));
    assert!(matches!(
        parsed.value.items[0].visibility,
        orna_syntax::Visibility::Private
    ));

    let parsed = parse_module("pub native fn f(a: Int, b: Int,): Int effect none;");
    assert!(parsed.is_ok(), "{:?}", diagnostics(&parsed.diagnostics));
    let Declaration::Native { signature, .. } = &parsed.value.items[0].declaration else {
        panic!("expected a native declaration");
    };
    assert_eq!(names(signature), ["a", "b"]);

    let parsed = parse_module("native fn f(): Int effect none");
    assert_eq!(parsed.diagnostics.len(), 1);
    assert!(parsed.is_incomplete(), "a missing `;` may still arrive");
}
