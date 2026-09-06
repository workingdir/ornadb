use unicode_ident::{is_xid_continue, is_xid_start};
use unicode_normalization::UnicodeNormalization;

use crate::parser::SourceSpan;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Keyword {
    As,
    Assert,
    Base,
    Break,
    Case,
    Continue,
    Dim,
    Else,
    Enum,
    False,
    Fn,
    For,
    If,
    Impl,
    In,
    Let,
    Loop,
    Null,
    Offset,
    Affine,
    Protocol,
    Pub,
    Return,
    SelfValue,
    Static,
    Table,
    True,
    Type,
    Unit,
    Use,
    While,
}

impl Keyword {
    pub fn from_text(s: &str) -> Option<Self> {
        Some(match s {
            "as" => Self::As,
            "assert" => Self::Assert,
            "base" => Self::Base,
            "break" => Self::Break,
            "case" => Self::Case,
            "continue" => Self::Continue,
            "dim" => Self::Dim,
            "else" => Self::Else,
            "enum" => Self::Enum,
            "false" => Self::False,
            "fn" => Self::Fn,
            "for" => Self::For,
            "if" => Self::If,
            "impl" => Self::Impl,
            "in" => Self::In,
            "let" => Self::Let,
            "loop" => Self::Loop,
            "null" => Self::Null,
            "offset" => Self::Offset,
            "affine" => Self::Affine,
            "protocol" => Self::Protocol,
            "pub" => Self::Pub,
            "return" => Self::Return,
            "self" => Self::SelfValue,
            "static" => Self::Static,
            "table" => Self::Table,
            "true" => Self::True,
            "type" => Self::Type,
            "unit" => Self::Unit,
            "use" => Self::Use,
            "while" => Self::While,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    Identifier {
        normalized: String,
    },
    Keyword(Keyword),
    Integer,
    Decimal,
    Float,
    Date,
    Instant,
    /// A complete, non-interpolated string literal.  Its spelling, including
    /// quotes and escapes, is retained in `Token::text`.
    String,
    /// Opening quote of an interpolated string.
    StringStart,
    /// Raw source spelling between string/interpolation delimiters. Escapes
    /// are deliberately not decoded by the lexer.
    StringText,
    /// The `{` which begins an interpolation expression.
    InterpolationStart,
    /// The `}` which ends an interpolation expression.
    InterpolationEnd,
    /// Closing quote of an interpolated string.
    StringEnd,
    ReplBinding,
    Punct(&'static str),
    Eof,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub text: String,
    pub span: SourceSpan,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexError {
    pub code: &'static str,
    pub message: String,
    pub span: SourceSpan,
}

/// Lexes UTF-8 source. Comments and whitespace are deliberately omitted from
/// the grammar stream; their byte locations are retained by every token span.
pub fn lex(source: &str) -> Result<Vec<Token>, Vec<LexError>> {
    let mut l = Lexer {
        source,
        at: 0,
        tokens: Vec::new(),
        errors: Vec::new(),
        string_depth: 0,
        string_limit_reported: false,
    };
    l.run();
    l.tokens.push(Token {
        kind: TokenKind::Eof,
        text: String::new(),
        span: SourceSpan::new(source.len(), source.len()),
    });
    if l.errors.is_empty() {
        Ok(l.tokens)
    } else {
        Err(l.errors)
    }
}
// Interpolation can recurse before parser admission; bound that lexer stack
// separately from ordinary braces and sequential string literals.
const MAX_STRING_NESTING: usize = 32;

struct Lexer<'a> {
    source: &'a str,
    at: usize,
    tokens: Vec<Token>,
    errors: Vec<LexError>,
    string_depth: usize,
    string_limit_reported: bool,
}
impl<'a> Lexer<'a> {
    fn run(&mut self) {
        self.code(false, 0);
    }

    /// Scan ordinary source, optionally stopping at the matching end of a
    /// string interpolation. Braces nested in the expression remain ordinary
    /// punctuation, so expression tokenisation is identical to top-level
    /// source tokenisation.
    fn code(&mut self, interpolation: bool, interpolation_start: usize) -> bool {
        let mut brace_depth = 0usize;
        while self.at < self.source.len() {
            let start = self.at;
            let c = self.peek();
            if interpolation && c == '}' {
                if brace_depth == 0 {
                    self.bump();
                    self.push(TokenKind::InterpolationEnd, start);
                    return true;
                }
                brace_depth -= 1;
                self.bump();
                self.push(TokenKind::Punct("}"), start);
                continue;
            }
            if c.is_whitespace() {
                self.bump();
                continue;
            }
            if self.take("//") {
                while self.at < self.source.len() && !matches!(self.peek(), '\n' | '\r') {
                    self.bump()
                }
                continue;
            }
            if self.take("/*") {
                self.comment(start);
                continue;
            }
            if c == '"' {
                self.string(start);
                continue;
            }
            if c == '$' && (self.rest().starts_with("$_") || self.rest().starts_with("$?")) {
                self.at += 2;
                self.push(TokenKind::ReplBinding, start);
                continue;
            }
            if c == '_' || is_xid_start(c) {
                self.ident(start);
                continue;
            }
            if c.is_ascii_digit() {
                self.number_or_time(start);
                continue;
            }
            let p = [
                "..=", "=>", "==", "!=", "<=", ">=", "??", "|?", "&&", "||", "+=", "-=", "*=",
                "/=", "..", "0x", "0b",
            ];
            if let Some(x) = p.into_iter().find(|x| self.rest().starts_with(*x)) {
                self.at += x.len();
                self.push(TokenKind::Punct(x), start);
                continue;
            }
            let one = match c {
                '{' => {
                    if interpolation {
                        brace_depth += 1;
                    }
                    "{"
                }
                '}' => "}",
                '(' => "(",
                ')' => ")",
                '[' => "[",
                ']' => "]",
                ',' => ",",
                ';' => ";",
                ':' => ":",
                '.' => ".",
                '|' => "|",
                '!' => "!",
                '=' => "=",
                '<' => "<",
                '>' => ">",
                '+' => "+",
                '-' => "-",
                '*' => "*",
                '/' => "/",
                '%' => "%",
                '^' => "^",
                '?' => "?",
                _ => "",
            };
            if one.is_empty() {
                self.bump();
                self.error("ORNA-LEX-001", "unexpected character", start, self.at)
            } else {
                self.bump();
                self.push(TokenKind::Punct(one), start)
            }
        }
        if interpolation {
            self.error(
                "ORNA-LEX-012",
                "unterminated string interpolation",
                interpolation_start,
                self.at,
            );
        }
        false
    }
    fn comment(&mut self, start: usize) {
        let mut depth = 1;
        while self.at < self.source.len() {
            if self.take("/*") {
                depth += 1
            } else if self.take("*/") {
                depth -= 1;
                if depth == 0 {
                    return;
                }
            } else {
                self.bump()
            }
        }
        self.error("ORNA-LEX-004", "unterminated block comment", start, self.at)
    }
    fn ident(&mut self, start: usize) {
        self.bump();
        while self.at < self.source.len() && (self.peek() == '_' || is_xid_continue(self.peek())) {
            self.bump()
        }
        let s = &self.source[start..self.at];
        if let Some(k) = Keyword::from_text(s) {
            self.push(TokenKind::Keyword(k), start)
        } else {
            self.push(
                TokenKind::Identifier {
                    normalized: s.nfc().collect(),
                },
                start,
            )
        }
    }
    fn number_or_time(&mut self, start: usize) {
        // maximal numeric candidate; grammar validation is parser responsibility.
        if self.rest().starts_with("0x") || self.rest().starts_with("0b") {
            self.at += 2;
            while self.at < self.source.len()
                && (self.peek().is_ascii_hexdigit() || self.peek() == '_')
            {
                self.bump()
            }
            let literal = &self.source[start..self.at];
            let digits = &literal[2..];
            let radix_ok = if literal.starts_with("0x") {
                valid_digit_component(digits, |c| c.is_ascii_hexdigit())
            } else {
                valid_digit_component(digits, |c| matches!(c, '0' | '1'))
            };
            if !radix_ok {
                self.error(
                    "ORNA-LEX-006",
                    "invalid radix integer literal",
                    start,
                    self.at,
                )
            }
            self.push(TokenKind::Integer, start);
            return;
        }
        // Dates and instants contain punctuation that would otherwise be
        // infix operators.  Their fixed leading shape makes them unambiguous.
        let date_shaped = self.rest().len() >= 10
            && self.rest().as_bytes().get(4) == Some(&b'-')
            && self.rest().as_bytes().get(7) == Some(&b'-');
        if date_shaped {
            while self.at < self.source.len()
                && (self.peek().is_ascii_alphanumeric()
                    || matches!(self.peek(), '-' | ':' | '.' | '+' | 'T' | 'Z'))
            {
                self.bump()
            }
        } else {
            let mut dot = false;
            let mut exponent = false;
            while self.at < self.source.len() {
                let c = self.peek();
                if c.is_ascii_digit() || c == '_' {
                    self.bump();
                    continue;
                }
                if c == '.'
                    && !dot
                    && !exponent
                    && self
                        .rest()
                        .as_bytes()
                        .get(1)
                        .is_some_and(|next| next.is_ascii_digit() || *next == b'_')
                {
                    dot = true;
                    self.bump();
                    continue;
                }
                if matches!(c, 'e' | 'E') && !exponent {
                    exponent = true;
                    self.bump();
                    if self.at < self.source.len() && matches!(self.peek(), '+' | '-') {
                        self.bump()
                    };
                    continue;
                }
                break;
            }
            if self.at < self.source.len() && self.peek() == 'f' {
                self.bump();
            }
        }
        let s = &self.source[start..self.at];
        let kind = if looks_instant(s) {
            if !valid_instant(s) {
                self.error("ORNA-LEX-008", "invalid instant literal", start, self.at);
            }
            TokenKind::Instant
        } else if looks_date(s) {
            if !valid_date(s) {
                self.error("ORNA-LEX-007", "invalid date literal", start, self.at);
            }
            TokenKind::Date
        } else if let Some(number) = s.strip_suffix('f') {
            if !valid_number(number) {
                self.error("ORNA-LEX-006", "invalid float literal", start, self.at)
            }
            TokenKind::Float
        } else if s.contains('.') || s.contains('e') || s.contains('E') {
            if !valid_number(s) {
                self.error("ORNA-LEX-006", "invalid decimal literal", start, self.at)
            }
            TokenKind::Decimal
        } else {
            if !valid_number(s) {
                self.error("ORNA-LEX-006", "invalid integer literal", start, self.at)
            }
            TokenKind::Integer
        };
        self.push(kind, start)
    }
    fn string(&mut self, start: usize) {
        if self.string_depth == MAX_STRING_NESTING {
            if !self.string_limit_reported {
                self.error(
                    "ORNA-LEX-013",
                    "maximum string interpolation nesting exceeded",
                    start,
                    start + 1,
                );
                self.string_limit_reported = true;
            }
            self.skip_limited_string();
            return;
        }
        self.string_depth += 1;
        self.string_inner(start);
        self.string_depth -= 1;
    }
    fn string_inner(&mut self, start: usize) {
        self.bump();
        let mut segment_start = self.at;
        let mut interpolated = false;
        while self.at < self.source.len() {
            let c = self.peek();
            if c == '\\' {
                self.string_escape();
                continue;
            }
            if c == '{' {
                if !interpolated {
                    self.push_range(TokenKind::StringStart, start, start + 1);
                    interpolated = true;
                }
                if segment_start < self.at {
                    self.push_range(TokenKind::StringText, segment_start, self.at);
                }
                self.bump();
                self.push_range(TokenKind::InterpolationStart, self.at - 1, self.at);
                if !self.code(true, start) {
                    return;
                }
                segment_start = self.at;
                continue;
            }
            if c == '"' {
                self.bump();
                if interpolated {
                    if segment_start < self.at - 1 {
                        self.push_range(TokenKind::StringText, segment_start, self.at - 1);
                    }
                    self.push_range(TokenKind::StringEnd, self.at - 1, self.at);
                } else {
                    self.push(TokenKind::String, start);
                }
                return;
            }
            self.bump();
        }
        self.error(
            "ORNA-LEX-010",
            "unterminated string literal",
            start,
            self.at,
        )
    }
    fn skip_limited_string(&mut self) {
        self.bump();
        while self.at < self.source.len() {
            if self.peek() == '\\' {
                self.bump();
                if self.at < self.source.len() {
                    self.bump();
                }
            } else if self.peek() == '"' {
                self.bump();
                return;
            } else {
                self.bump();
            }
        }
    }
    fn string_escape(&mut self) {
        let escape_start = self.at;
        self.bump();
        if self.at == self.source.len() {
            self.error(
                "ORNA-LEX-011",
                "unterminated string escape",
                escape_start,
                self.at,
            );
            return;
        }
        let c = self.peek();
        self.bump();
        if c != 'u' {
            if !matches!(c, '"' | '\\' | 'n' | 'r' | 't' | '0') {
                self.error(
                    "ORNA-LEX-011",
                    "invalid string escape",
                    escape_start,
                    self.at,
                );
            }
            return;
        }
        if !self.take("{") {
            self.error(
                "ORNA-LEX-011",
                "unicode escape requires `{`",
                escape_start,
                self.at,
            );
            return;
        }
        let digits_start = self.at;
        while self.at < self.source.len() && self.peek().is_ascii_hexdigit() {
            self.bump();
        }
        let digits = &self.source[digits_start..self.at];
        let scalar = u32::from_str_radix(digits, 16)
            .ok()
            .filter(|v| *v <= 0x10ffff && !(0xd800..=0xdfff).contains(v));
        if digits.is_empty() || digits.len() > 6 || scalar.is_none() || !self.take("}") {
            self.error(
                "ORNA-LEX-011",
                "invalid unicode scalar escape",
                escape_start,
                self.at,
            );
        }
    }
    fn peek(&self) -> char {
        self.rest().chars().next().unwrap()
    }
    fn rest(&self) -> &str {
        &self.source[self.at..]
    }
    fn bump(&mut self) {
        self.at += self.peek().len_utf8()
    }
    fn take(&mut self, s: &str) -> bool {
        if self.rest().starts_with(s) {
            self.at += s.len();
            true
        } else {
            false
        }
    }
    fn push(&mut self, kind: TokenKind, start: usize) {
        self.push_range(kind, start, self.at)
    }
    fn push_range(&mut self, kind: TokenKind, start: usize, end: usize) {
        self.tokens.push(Token {
            kind,
            text: self.source[start..end].to_owned(),
            span: SourceSpan::new(start, end),
        })
    }
    fn error(&mut self, code: &'static str, message: &str, start: usize, end: usize) {
        self.errors.push(LexError {
            code,
            message: message.into(),
            span: SourceSpan::new(start, end),
        })
    }
}
fn looks_date(s: &str) -> bool {
    s.len() == 10 && s.as_bytes().get(4) == Some(&b'-') && s.as_bytes().get(7) == Some(&b'-')
}
/// A numeral component has separators only between two digits. The caller
/// chooses the digit alphabet; decimal/exponent components are ASCII decimal
/// while radix literals use the same rule over their radix-specific digits.
fn valid_digit_component(s: &str, digit: impl Fn(char) -> bool) -> bool {
    let mut previous_was_digit = false;
    let mut any = false;
    for c in s.chars() {
        if digit(c) {
            previous_was_digit = true;
            any = true;
        } else if c == '_' && previous_was_digit {
            previous_was_digit = false;
        } else {
            return false;
        }
    }
    any && previous_was_digit
}

fn valid_decimal_integer(s: &str) -> bool {
    valid_digit_component(s, |c| c.is_ascii_digit())
}

fn valid_number(s: &str) -> bool {
    let (mantissa, exponent) = match s.find(['e', 'E']) {
        Some(index) => {
            if s[index + 1..].contains(['e', 'E']) {
                return false;
            }
            let exponent = &s[index + 1..];
            let exponent = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
            (&s[..index], Some(exponent))
        }
        None => (s, None),
    };
    if exponent.is_some_and(|part| !valid_decimal_integer(part)) {
        return false;
    }
    match mantissa.split_once('.') {
        Some((whole, fraction)) if !fraction.contains('.') => {
            valid_decimal_integer(whole) && valid_decimal_integer(fraction)
        }
        Some(_) => false,
        None => valid_decimal_integer(mantissa),
    }
}
fn looks_instant(s: &str) -> bool {
    s.len() >= 20
        && s.as_bytes().get(10) == Some(&b'T')
        && (s.ends_with('Z') || s[19..].contains('+') || s[19..].contains('-'))
}
fn valid_date(s: &str) -> bool {
    if !looks_date(s) {
        return false;
    }
    let y = s[0..4].parse::<i32>().ok();
    let m = s[5..7].parse::<u32>().ok();
    let d = s[8..10].parse::<u32>().ok();
    match (y, m, d) {
        (Some(y), Some(m), Some(d)) if y >= 1 && (1..=12).contains(&m) => {
            let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
            let max = match m {
                1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
                4 | 6 | 9 | 11 => 30,
                2 if leap => 29,
                2 => 28,
                _ => 0,
            };
            (1..=max).contains(&d)
        }
        _ => false,
    }
}
fn valid_instant(s: &str) -> bool {
    if s.len() < 20 || !valid_date(&s[..10]) || s.as_bytes().get(10) != Some(&b'T') {
        return false;
    };
    let bytes = s.as_bytes();
    let number = |a: usize, b: usize| s.get(a..b).and_then(|x| x.parse::<u32>().ok());
    if !matches!((number(11,13),number(14,16),number(17,19)),(Some(h),Some(m),Some(sec)) if h<24&&m<60&&sec<60)
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
    {
        return false;
    }
    let mut tail = &s[19..];
    if let Some(rest) = tail.strip_prefix('.') {
        let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
        if !(1..=9).contains(&digits) {
            return false;
        }
        tail = &rest[digits..];
    }
    if tail == "Z" {
        return true;
    }
    let Some(sign) = tail.chars().next() else {
        return false;
    };
    if !matches!(sign, '+' | '-') || tail.len() != 6 || tail.as_bytes().get(3) != Some(&b':') {
        return false;
    }
    matches!((tail[1..3].parse::<u32>(), tail[4..6].parse::<u32>()), (Ok(hours), Ok(minutes)) if hours <= 23 && minutes < 60)
}
