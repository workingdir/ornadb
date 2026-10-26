//! One lexical model behind every generated editor highlighter.
//!
//! The rules transcribe the lexical productions of the Orna 1.1.0 grammar
//! (`grammar/orna.ebnf`: `identifier`, `integer_literal`, `decimal_integer`,
//! `decimal_source_numeral`, `float_literal`, `exponent`, `escape_sequence`,
//! `date_literal`, `instant_literal`) and the token rules of the Lexical
//! structure chapter: ORNA-LEX-003/004 (comments), ORNA-LEX-007 (keywords),
//! ORNA-LEX-009 (longest-match operators), ORNA-LIT-004 (interpolation),
//! ORNA-LIT-005/006 (numerals), "Contextual names and construction" and
//! "Numeric and string token boundaries".
//!
//! Each rule is a small regular-expression tree rendered once per editor
//! dialect, so no emitter hand-translates another dialect's escaping.

use crate::{
    Keyword,
    lexer::{OPERATORS, PUNCTUATION},
};

/// A regular-expression tree over the constructs every editor dialect shares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Re {
    /// Literal text, escaped per dialect.
    Text(String),
    /// A character class of inclusive ASCII ranges; `(c, c)` is one character.
    Class(Vec<(char, char)>),
    /// `identifier_start`: `_` or a Unicode `XID_Start` character.
    IdentifierStart,
    /// `identifier_continue`: `identifier_start` or a Unicode `XID_Continue`
    /// character.
    IdentifierContinue,
    Sequence(Vec<Re>),
    Choice(Vec<Re>),
    Optional(Box<Re>),
    ZeroOrMore(Box<Re>),
    Exactly(Box<Re>, u32),
    Between(Box<Re>, u32, u32),
}

/// The regular-expression dialects the generated editor grammars use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Dialect {
    /// Oniguruma, as read by TextMate (VS Code) and Sublime Text grammars.
    /// Identifier classes use Unicode general categories because not every
    /// engine that loads these grammars knows the `XID_*` properties.
    Oniguruma,
    /// Emacs Lisp regular expressions, before Lisp string quoting.
    Emacs,
    /// Vim patterns in very-magic (`\v`) mode.
    VimVeryMagic,
    /// Tree-sitter grammar regular expressions (JavaScript regex literals).
    TreeSitter,
}

impl Re {
    pub(super) fn text(value: &str) -> Self {
        Self::Text(value.to_owned())
    }

    pub(super) fn render(&self, dialect: Dialect) -> String {
        let mut out = String::new();
        self.write(dialect, &mut out);
        out
    }

    fn write(&self, dialect: Dialect, out: &mut String) {
        match self {
            Self::Text(text) => {
                for character in text.chars() {
                    write_literal(dialect, character, out);
                }
            }
            Self::Class(ranges) => write_class(dialect, ranges, out),
            Self::IdentifierStart => out.push_str(match dialect {
                Dialect::Oniguruma => r"[_\p{L}\p{Nl}]",
                Dialect::Emacs => "[_[:alpha:]]",
                Dialect::VimVeryMagic => r"%([_a-zA-Z]|[^\x00-\x7f])",
                Dialect::TreeSitter => r"[_\p{XID_Start}]",
            }),
            Self::IdentifierContinue => out.push_str(match dialect {
                Dialect::Oniguruma => r"[\p{L}\p{Nl}\p{Mn}\p{Mc}\p{Nd}\p{Pc}]",
                Dialect::Emacs => "[_[:alnum:][:nonascii:]]",
                Dialect::VimVeryMagic => r"%([_a-zA-Z0-9]|[^\x00-\x7f])",
                Dialect::TreeSitter => r"\p{XID_Continue}",
            }),
            Self::Sequence(items) => {
                for item in items {
                    // A nested choice renders as its own group, so plain
                    // concatenation never changes a member's meaning.
                    item.write(dialect, out);
                }
            }
            Self::Choice(items) => {
                out.push_str(group_open(dialect));
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push_str(alternation(dialect));
                    }
                    item.write(dialect, out);
                }
                out.push_str(group_close(dialect));
            }
            Self::Optional(inner) => {
                inner.write_atom(dialect, out);
                out.push('?');
            }
            Self::ZeroOrMore(inner) => {
                inner.write_atom(dialect, out);
                out.push('*');
            }
            Self::Exactly(inner, count) => {
                inner.write_atom(dialect, out);
                out.push_str(&counted(dialect, &count.to_string()));
            }
            Self::Between(inner, low, high) => {
                inner.write_atom(dialect, out);
                out.push_str(&counted(dialect, &format!("{low},{high}")));
            }
        }
    }

    /// Writes the expression as one quantifiable atom, grouping it if needed.
    fn write_atom(&self, dialect: Dialect, out: &mut String) {
        let atomic = match self {
            Self::Class(_) | Self::IdentifierStart | Self::IdentifierContinue | Self::Choice(_) => {
                true
            }
            Self::Text(text) => text.chars().count() == 1,
            _ => false,
        };
        if atomic {
            self.write(dialect, out);
        } else {
            out.push_str(group_open(dialect));
            self.write(dialect, out);
            out.push_str(group_close(dialect));
        }
    }
}

fn group_open(dialect: Dialect) -> &'static str {
    match dialect {
        Dialect::Oniguruma | Dialect::TreeSitter => "(?:",
        Dialect::Emacs => r"\(?:",
        Dialect::VimVeryMagic => "%(",
    }
}

fn group_close(dialect: Dialect) -> &'static str {
    match dialect {
        Dialect::Emacs => r"\)",
        _ => ")",
    }
}

fn alternation(dialect: Dialect) -> &'static str {
    match dialect {
        Dialect::Emacs => r"\|",
        _ => "|",
    }
}

fn counted(dialect: Dialect, count: &str) -> String {
    match dialect {
        Dialect::Emacs => format!(r"\{{{count}\}}"),
        _ => format!("{{{count}}}"),
    }
}

fn write_literal(dialect: Dialect, character: char, out: &mut String) {
    let special = match dialect {
        Dialect::Oniguruma => r"\^$.|?*+()[]{}",
        Dialect::TreeSitter => r"\^$.|?*+()[]{}/",
        Dialect::Emacs => r"\^$.*+?[",
        // Very magic gives every ASCII character except letters, digits and
        // `_` a meaning, so punctuation is written as a one-item class.
        Dialect::VimVeryMagic => {
            if character.is_ascii_punctuation() && character != '_' {
                write_class(dialect, &[(character, character)], out);
                return;
            }
            ""
        }
    };
    if special.contains(character) {
        out.push('\\');
    }
    out.push(character);
}

fn write_class(dialect: Dialect, ranges: &[(char, char)], out: &mut String) {
    out.push('[');
    if dialect == Dialect::Emacs {
        // Emacs classes have no escapes: `]` must come first and `-` last.
        let mut ordered = ranges.to_vec();
        ordered.sort_by_key(|&(low, _)| match low {
            ']' => 0,
            '-' => 2,
            _ => 1,
        });
        for (low, high) in ordered {
            out.push(low);
            if high != low {
                out.push('-');
                out.push(high);
            }
        }
    } else {
        for &(low, high) in ranges {
            write_class_member(dialect, low, out);
            if high != low {
                out.push('-');
                write_class_member(dialect, high, out);
            }
        }
    }
    out.push(']');
}

fn write_class_member(dialect: Dialect, character: char, out: &mut String) {
    // Inside a Vim collection a backslash escapes only `]`, `^`, `-` and `\`;
    // before any other character it is itself a member, so `[` stays bare.
    let special = match dialect {
        Dialect::VimVeryMagic => r"\]^-",
        _ => r"\][^-",
    };
    if special.contains(character) {
        out.push('\\');
    }
    out.push(character);
}

fn digit_class() -> Re {
    Re::Class(vec![('0', '9')])
}

/// `first { [ "_" ] , next }`: an underscore only between two digits of the
/// same numeral part ("Numeric and string token boundaries").
fn separated_digits(digit: Re) -> Re {
    Re::Sequence(vec![
        digit.clone(),
        Re::ZeroOrMore(Box::new(Re::Sequence(vec![
            Re::Optional(Box::new(Re::text("_"))),
            digit,
        ]))),
    ])
}

fn decimal_integer() -> Re {
    separated_digits(digit_class())
}

/// Every source numeral: `instant_literal`, `date_literal`, the prefixed
/// `integer_literal` forms, then `decimal_integer` with an optional fraction,
/// exponent and `f` suffix (`decimal_source_numeral`, `float_literal`).
/// Instants come before dates and dates before integers, because ORNA-LIT-005
/// recognises a complete date or instant before subtraction.
pub(super) fn number() -> Re {
    let digits = |count| Re::Exactly(Box::new(digit_class()), count);
    let date = Re::Sequence(vec![
        digits(4),
        Re::text("-"),
        digits(2),
        Re::text("-"),
        digits(2),
    ]);
    let offset = Re::Sequence(vec![
        Re::Class(vec![('+', '+'), ('-', '-')]),
        digits(2),
        Re::text(":"),
        digits(2),
    ]);
    let instant = Re::Sequence(vec![
        date.clone(),
        Re::text("T"),
        digits(2),
        Re::text(":"),
        digits(2),
        Re::text(":"),
        digits(2),
        Re::Optional(Box::new(Re::Sequence(vec![
            Re::text("."),
            digit_class(),
            Re::ZeroOrMore(Box::new(digit_class())),
        ]))),
        Re::Choice(vec![Re::text("Z"), offset]),
    ]);
    let hexadecimal = Re::Sequence(vec![
        Re::text("0x"),
        separated_digits(Re::Class(vec![('0', '9'), ('A', 'F'), ('a', 'f')])),
    ]);
    let binary = Re::Sequence(vec![
        Re::text("0b"),
        separated_digits(Re::Class(vec![('0', '0'), ('1', '1')])),
    ]);
    let exponent = Re::Sequence(vec![
        Re::Class(vec![('E', 'E'), ('e', 'e')]),
        Re::Optional(Box::new(Re::Class(vec![('+', '+'), ('-', '-')]))),
        decimal_integer(),
    ]);
    let decimal = Re::Sequence(vec![
        decimal_integer(),
        Re::Optional(Box::new(Re::Sequence(vec![
            Re::text("."),
            decimal_integer(),
        ]))),
        Re::Optional(Box::new(exponent)),
        Re::Optional(Box::new(Re::text("f"))),
    ]);
    Re::Choice(vec![instant, date, hexadecimal, binary, decimal])
}

/// `identifier = identifier_start , { identifier_continue }`.
pub(super) fn identifier() -> Re {
    Re::Sequence(vec![
        Re::IdentifierStart,
        Re::ZeroOrMore(Box::new(Re::IdentifierContinue)),
    ])
}

/// `escape_sequence`: `\"`, `\\`, `\n`, `\r`, `\t`, `\0` and `\u{` one to six
/// hexadecimal digits `}` ("String escapes").
pub(super) fn escape_sequence() -> Re {
    Re::Sequence(vec![
        Re::text("\\"),
        Re::Choice(vec![
            Re::Class(vec![
                ('"', '"'),
                ('0', '0'),
                ('\\', '\\'),
                ('n', 'n'),
                ('r', 'r'),
                ('t', 't'),
            ]),
            Re::Sequence(vec![
                Re::text("u{"),
                Re::Between(
                    Box::new(Re::Class(vec![('0', '9'), ('A', 'F'), ('a', 'f')])),
                    1,
                    6,
                ),
                Re::text("}"),
            ]),
        ]),
    ])
}

/// Alternation of literal spellings, longest first, as ORNA-LEX-009 requires
/// for operators.
pub(super) fn spellings(values: &[&str]) -> Re {
    let mut sorted = values.to_vec();
    sorted.sort_by_key(|value| std::cmp::Reverse(value.len()));
    sorted.dedup();
    Re::Choice(sorted.into_iter().map(Re::text).collect())
}

/// ORNA-LEX-007 keywords in lexer table order.
pub(super) fn keywords() -> Vec<&'static str> {
    Keyword::ALL
        .iter()
        .map(|keyword| keyword.spelling())
        .collect()
}

/// Reserved words that are literal values. Before a `:` they are case-arm
/// literal patterns (`literal_pattern`), not record or argument labels.
pub(super) const LITERAL_KEYWORDS: &[&str] = &["false", "null", "true"];

/// Reserved words that act as names when they label a record field or a
/// named argument (`record_field`, `argument`; "Contextual names").
pub(super) fn label_keywords() -> Vec<&'static str> {
    keywords()
        .into_iter()
        .filter(|keyword| !LITERAL_KEYWORDS.contains(keyword))
        .collect()
}

pub(super) fn operators() -> Re {
    spellings(OPERATORS)
}

pub(super) fn punctuation() -> Re {
    spellings(PUNCTUATION)
}

/// Punctuation other than braces, for grammars that pair `{` with `}`.
pub(super) fn unpaired_punctuation() -> Vec<&'static str> {
    PUNCTUATION
        .iter()
        .copied()
        .filter(|value| !matches!(*value, "{" | "}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_render_once_per_dialect_without_dialect_leaks() {
        let number = number();
        let oniguruma = number.render(Dialect::Oniguruma);
        assert!(oniguruma.starts_with("(?:[0-9]{4}-[0-9]{2}-[0-9]{2}T"));
        assert!(oniguruma.contains("0x[0-9A-Fa-f](?:_?[0-9A-Fa-f])*"));
        let emacs = number.render(Dialect::Emacs);
        assert!(emacs.starts_with(r"\(?:[0-9]\{4\}-"));
        assert!(!emacs.contains(r"\?") && !emacs.contains(r"\+"));
        let vim = number.render(Dialect::VimVeryMagic);
        assert!(vim.starts_with(r"%([0-9]{4}[\-][0-9]{2}"));
        assert!(!vim.contains(r"\%("));
        let tree_sitter = number.render(Dialect::TreeSitter);
        assert!(tree_sitter.contains("0b[01](?:_?[01])*"));
    }

    #[test]
    fn classes_order_emacs_members_and_escape_the_others() {
        let class = Re::Class(vec![('+', '+'), ('-', '-')]);
        assert_eq!(class.render(Dialect::Emacs), "[+-]");
        assert_eq!(class.render(Dialect::Oniguruma), r"[+\-]");
        let reversed = Re::Class(vec![('-', '-'), ('+', '+')]);
        assert_eq!(reversed.render(Dialect::Emacs), "[+-]");
        assert_eq!(Re::text("*/").render(Dialect::VimVeryMagic), "[*][/]");
        assert_eq!(Re::text("..=").render(Dialect::Emacs), r"\.\.=");
    }

    #[test]
    fn label_keywords_exclude_only_literal_values() {
        let labels = label_keywords();
        assert_eq!(labels.len() + LITERAL_KEYWORDS.len(), keywords().len());
        assert!(labels.contains(&"as") && labels.contains(&"table"));
        assert!(!labels.contains(&"true"));
    }
}
