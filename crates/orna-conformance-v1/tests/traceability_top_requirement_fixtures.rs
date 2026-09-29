//! Executable parser/lexer witnesses for selected normative fixture gaps.
//! The requirement register remains frozen: these tests establish fixture
//! existence and parser/lexer outcomes only, not full runtime conformance.

use orna_syntax_v1::{TokenKind, lex, parse_module, parse_row};

const SOURCE_MODULE: &str = include_str!("fixtures/traceability-source-module.orna");
const SOURCE_ROW: &str = include_str!("fixtures/traceability-source-row.orna");
const NESTED_COMMENTS: &str = include_str!("fixtures/traceability-nested-comments.orna");
const NFC_IDENTIFIERS: &str = include_str!("fixtures/traceability-nfc-identifiers.orna");
const LONGEST_OPERATORS: &str = include_str!("fixtures/traceability-longest-operators.orna");
const BIG_INTEGER: &str = include_str!("fixtures/traceability-big-integer.orna");
const EXACT_DECIMALS: &str = include_str!("fixtures/traceability-exact-decimals.orna");
const INTERPOLATION: &str = include_str!("fixtures/traceability-interpolation.orna");
const RECORD_PUNNING_INVALID: &str =
    include_str!("fixtures/traceability-record-punning-invalid.orna");
const ARROW_BLOCK: &str = include_str!("fixtures/traceability-arrow-block.orna");
const RECORD_CONDITION_VALID: &str =
    include_str!("fixtures/traceability-record-condition-valid.orna");
const RECORD_CONDITION_INVALID: &str =
    include_str!("fixtures/traceability-record-condition-invalid.orna");

// ORNA-SOURCE-001, source/03-source-modules.md:8.
#[test]
fn module_and_row_fixtures_use_distinct_parser_entrypoints() {
    assert!(parse_module(SOURCE_MODULE).diagnostics.is_empty());
    assert!(parse_row(SOURCE_ROW).diagnostics.is_empty());
}

// ORNA-LEX-004, source/04-lexical.md:19.
#[test]
fn nested_comments_and_string_delimiters_are_lexed_as_specified() {
    assert!(parse_module(NESTED_COMMENTS).diagnostics.is_empty());
    let tokens = lex(NESTED_COMMENTS).expect("nested comments lex");
    assert!(tokens.iter().any(|token| token.kind == TokenKind::String));
}

// ORNA-LEX-005, source/04-lexical.md:25.
#[test]
fn equivalent_unicode_identifiers_share_nfc_comparison_but_keep_spelling() {
    let tokens = lex(NFC_IDENTIFIERS).expect("Unicode identifiers lex");
    let identifiers = tokens
        .iter()
        .filter_map(|token| match &token.kind {
            TokenKind::Identifier { normalized } => Some((&token.text, normalized)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(identifiers.len(), 2);
    assert_ne!(identifiers[0].0, identifiers[1].0);
    assert_eq!(identifiers[0].1, identifiers[1].1);
}

// ORNA-LEX-009, source/04-lexical.md:35.
#[test]
fn multi_character_operator_fixture_keeps_the_required_tokens_indivisible() {
    let tokens = lex(LONGEST_OPERATORS).expect("operator fixture lexes");
    let punctuation = tokens
        .iter()
        .filter_map(|token| match &token.kind {
            TokenKind::Punct(punctuation) => Some(*punctuation),
            _ => None,
        })
        .collect::<Vec<_>>();
    for required in ["|?", "??", "||", "=>", "..=", "<=", ">=", "==", "!="] {
        assert!(punctuation.contains(&required), "missing token {required}");
    }
}

// ORNA-LIT-001, source/04-lexical.md:57.
#[test]
fn large_integer_fixture_remains_one_integer_token() {
    assert!(parse_module(BIG_INTEGER).diagnostics.is_empty());
    let tokens = lex(BIG_INTEGER).expect("large integer lexes");
    let integers = tokens
        .iter()
        .filter(|token| token.kind == TokenKind::Integer)
        .collect::<Vec<_>>();
    assert_eq!(integers.len(), 1);
    assert!(integers[0].text.len() > 32);
}

// ORNA-LIT-002, source/04-lexical.md:59.
#[test]
fn exact_fraction_and_exponent_fixtures_parse_without_float_suffixes() {
    assert!(parse_module(EXACT_DECIMALS).diagnostics.is_empty());
    let tokens = lex(EXACT_DECIMALS).expect("decimal forms lex");
    assert_eq!(
        tokens
            .iter()
            .filter(|token| token.kind == TokenKind::Decimal)
            .count(),
        2
    );
    assert!(!tokens.iter().any(|token| token.kind == TokenKind::Float));
}

// ORNA-LIT-004, source/04-lexical.md:63.
#[test]
fn interpolation_fixture_parses_and_has_interpolation_tokens() {
    assert!(parse_module(INTERPOLATION).diagnostics.is_empty());
    let tokens = lex(INTERPOLATION).expect("interpolation lexes");
    assert!(tokens.iter().any(|token| token.kind == TokenKind::InterpolationStart));
    assert!(tokens.iter().any(|token| token.kind == TokenKind::InterpolationEnd));
}

// ORNA-RECORD-001, source/04-lexical.md:83.
#[test]
fn record_literal_punning_fixture_is_rejected() {
    assert!(!parse_module(RECORD_PUNNING_INVALID).diagnostics.is_empty());
}

// ORNA-ARROW-001, source/04-lexical.md:89-93.
#[test]
fn anonymous_function_direct_braced_block_fixture_parses() {
    assert!(parse_module(ARROW_BLOCK).diagnostics.is_empty());
}

// ORNA-RECORD-004, source/04-lexical.md:99.
#[test]
fn record_condition_requires_parentheses() {
    assert!(parse_module(RECORD_CONDITION_VALID).diagnostics.is_empty());
    assert!(!parse_module(RECORD_CONDITION_INVALID).diagnostics.is_empty());
}
