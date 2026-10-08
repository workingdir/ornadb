//! Engine for the `orna.regex/1` dialect used by `std.regex`.
//!
//! Matching works over Unicode scalar positions with half-open spans. Offsets
//! are zero-based and `captures[0]` is the whole match. The whole match is
//! leftmost-longest; at the same start and end, alternation and quantifier
//! choices follow source order, and quantifiers are greedy unless suffixed by
//! `?`. Every operation has a fixed work budget and fails with
//! [`RegexError::LimitExhausted`] instead of returning a partial result.

use icu_properties::props::{Alphabetic, GeneralCategory, WhiteSpace};
use icu_properties::{CodePointMapData, CodePointSetData};

/// Upper bound on a counted repetition, so compilation cost stays bounded.
const MAX_REPEAT_BOUND: u32 = 1_000;
/// Upper bound on backtracking steps for one operation.
const STEP_LIMIT: u64 = 10_000_000;

/// A syntax or resource error. Syntax errors carry the scalar position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegexError {
    Syntax { position: usize, message: &'static str },
    LimitExhausted,
}

/// One successful match. `captures[0]` is the whole match; later entries
/// follow opening-group order and are `None` when the group did not
/// participate. Positions are scalar offsets into the searched text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub start: usize,
    pub end: usize,
    pub captures: Vec<Option<(usize, usize)>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Predicate {
    Digit,
    Space,
    Word,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ClassItem {
    Literal(char),
    Range(char, char),
    Predicate(Predicate),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Node {
    Empty,
    Literal(char),
    Any,
    Predicate(Predicate),
    Class { negated: bool, items: Vec<ClassItem> },
    Start,
    End,
    WordBoundary,
    Group { index: Option<usize>, inner: Box<Node> },
    Concat(Vec<Node>),
    Alternation(Vec<Node>),
    Repeat { inner: Box<Node>, min: u32, max: Option<u32>, greedy: bool },
}

/// A compiled `orna.regex/1` pattern.
#[derive(Debug, Clone)]
pub struct Regex {
    root: Node,
    group_count: usize,
}

fn is_metacharacter(value: char) -> bool {
    matches!(value, '\\' | '.' | '^' | '$' | '[' | ']' | '(' | ')' | '|' | '*' | '+' | '?' | '{' | '}')
}

fn predicate_matches(predicate: Predicate, value: char) -> bool {
    match predicate {
        Predicate::Digit => {
            CodePointMapData::<GeneralCategory>::new().get(value) == GeneralCategory::DecimalNumber
        }
        Predicate::Space => CodePointSetData::new::<WhiteSpace>().contains(value),
        Predicate::Word => {
            CodePointSetData::new::<Alphabetic>().contains(value)
                || matches!(
                    CodePointMapData::<GeneralCategory>::new().get(value),
                    GeneralCategory::NonspacingMark
                        | GeneralCategory::SpacingMark
                        | GeneralCategory::EnclosingMark
                        | GeneralCategory::DecimalNumber
                        | GeneralCategory::ConnectorPunctuation
                )
        }
    }
}

struct Parser<'a> {
    chars: &'a [char],
    position: usize,
    group_count: usize,
}

impl<'a> Parser<'a> {
    fn error<T>(&self, message: &'static str) -> Result<T, RegexError> {
        Err(RegexError::Syntax { position: self.position, message })
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.position).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let value = self.peek()?;
        self.position += 1;
        Some(value)
    }

    fn parse_alternation(&mut self) -> Result<Node, RegexError> {
        let mut branches = vec![self.parse_concatenation()?];
        while self.peek() == Some('|') {
            self.bump();
            branches.push(self.parse_concatenation()?);
        }
        Ok(if branches.len() == 1 {
            branches.remove(0)
        } else {
            Node::Alternation(branches)
        })
    }

    fn parse_concatenation(&mut self) -> Result<Node, RegexError> {
        let mut items = Vec::new();
        while let Some(value) = self.peek() {
            if value == '|' || value == ')' {
                break;
            }
            let atom = self.parse_atom()?;
            items.push(self.parse_quantifiers(atom)?);
        }
        Ok(match items.len() {
            0 => Node::Empty,
            1 => items.remove(0),
            _ => Node::Concat(items),
        })
    }

    fn parse_atom(&mut self) -> Result<Node, RegexError> {
        let Some(value) = self.bump() else {
            return self.error("unexpected end of pattern");
        };
        match value {
            '(' => self.parse_group(),
            '[' => self.parse_class(),
            '.' => Ok(Node::Any),
            '^' => Ok(Node::Start),
            '$' => Ok(Node::End),
            '\\' => self.parse_escape(),
            '*' | '+' | '?' | '{' => {
                self.position -= 1;
                self.error("quantifier has no operand")
            }
            ']' | '}' => {
                self.position -= 1;
                self.error("unescaped closing bracket")
            }
            other => Ok(Node::Literal(other)),
        }
    }

    fn parse_group(&mut self) -> Result<Node, RegexError> {
        let index = if self.peek() == Some('?') {
            self.bump();
            if self.bump() != Some(':') {
                self.position -= 1;
                return self.error("unsupported group construct");
            }
            None
        } else {
            self.group_count += 1;
            Some(self.group_count)
        };
        let inner = self.parse_alternation()?;
        if self.bump() != Some(')') {
            return self.error("unclosed group");
        }
        Ok(Node::Group { index, inner: Box::new(inner) })
    }

    fn parse_escape(&mut self) -> Result<Node, RegexError> {
        let Some(value) = self.bump() else {
            return self.error("pattern ends with an escape");
        };
        Ok(match value {
            'd' => Node::Predicate(Predicate::Digit),
            's' => Node::Predicate(Predicate::Space),
            'w' => Node::Predicate(Predicate::Word),
            'b' => Node::WordBoundary,
            'n' => Node::Literal('\n'),
            'r' => Node::Literal('\r'),
            't' => Node::Literal('\t'),
            other if is_metacharacter(other) => Node::Literal(other),
            _ => {
                self.position -= 1;
                return self.error("unsupported escape");
            }
        })
    }

    fn parse_class(&mut self) -> Result<Node, RegexError> {
        let negated = if self.peek() == Some('^') {
            self.bump();
            true
        } else {
            false
        };
        let mut items = Vec::new();
        loop {
            let Some(value) = self.bump() else {
                return self.error("unclosed character class");
            };
            if value == ']' {
                break;
            }
            let item = if value == '\\' {
                match self.parse_class_escape()? {
                    ClassItem::Literal(literal) => ClassItem::Literal(literal),
                    predicate => {
                        items.push(predicate);
                        continue;
                    }
                }
            } else {
                ClassItem::Literal(value)
            };
            if let ClassItem::Literal(low) = item
                && self.peek() == Some('-')
                && self.chars.get(self.position + 1).is_some_and(|next| *next != ']')
            {
                self.bump();
                let high = match self.bump() {
                    Some('\\') => match self.parse_class_escape()? {
                        ClassItem::Literal(literal) => literal,
                        _ => return self.error("class range endpoint must be a literal"),
                    },
                    Some(literal) => literal,
                    None => return self.error("unclosed character class"),
                };
                if high < low {
                    return self.error("class range is reversed");
                }
                items.push(ClassItem::Range(low, high));
                continue;
            }
            items.push(item);
        }
        if items.is_empty() {
            return self.error("empty character class");
        }
        Ok(Node::Class { negated, items })
    }

    fn parse_class_escape(&mut self) -> Result<ClassItem, RegexError> {
        let Some(value) = self.bump() else {
            return self.error("pattern ends with an escape");
        };
        Ok(match value {
            'd' => ClassItem::Predicate(Predicate::Digit),
            's' => ClassItem::Predicate(Predicate::Space),
            'w' => ClassItem::Predicate(Predicate::Word),
            'n' => ClassItem::Literal('\n'),
            'r' => ClassItem::Literal('\r'),
            't' => ClassItem::Literal('\t'),
            other if is_metacharacter(other) => ClassItem::Literal(other),
            _ => {
                self.position -= 1;
                return self.error("unsupported escape");
            }
        })
    }

    fn parse_quantifiers(&mut self, mut atom: Node) -> Result<Node, RegexError> {
        let mut quantified = false;
        loop {
            let (min, max) = match self.peek() {
                Some('*') => {
                    self.bump();
                    (0, None)
                }
                Some('+') => {
                    self.bump();
                    (1, None)
                }
                Some('?') => {
                    self.bump();
                    (0, Some(1))
                }
                Some('{') => {
                    self.bump();
                    self.parse_bounds()?
                }
                _ => return Ok(atom),
            };
            if quantified {
                return self.error("nested quantifier");
            }
            quantified = true;
            let greedy = if self.peek() == Some('?') {
                self.bump();
                false
            } else {
                true
            };
            atom = Node::Repeat { inner: Box::new(atom), min, max, greedy };
        }
    }

    fn parse_bounds(&mut self) -> Result<(u32, Option<u32>), RegexError> {
        let min = self.parse_number()?;
        let max = if self.peek() == Some(',') {
            self.bump();
            if self.peek() == Some('}') {
                None
            } else {
                Some(self.parse_number()?)
            }
        } else {
            Some(min)
        };
        if self.bump() != Some('}') {
            return self.error("malformed counted repetition");
        }
        if max.is_some_and(|upper| upper < min) {
            return self.error("counted repetition minimum exceeds maximum");
        }
        Ok((min, max))
    }

    fn parse_number(&mut self) -> Result<u32, RegexError> {
        let start = self.position;
        let mut value: u32 = 0;
        while let Some(digit) = self.peek().and_then(|value| value.to_digit(10)) {
            self.bump();
            value = value.saturating_mul(10).saturating_add(digit);
        }
        if self.position == start {
            return self.error("expected a repetition count");
        }
        if value > MAX_REPEAT_BOUND {
            return self.error("repetition count exceeds the dialect bound");
        }
        Ok(value)
    }
}

/// Compile a pattern under `orna.regex/1`. Unsupported constructs such as
/// look-around, backreferences and unknown escapes are syntax errors.
pub fn compile(pattern: &str) -> Result<Regex, RegexError> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut parser = Parser { chars: &chars, position: 0, group_count: 0 };
    let root = parser.parse_alternation()?;
    if parser.position != chars.len() {
        return parser.error("unmatched closing parenthesis");
    }
    Ok(Regex { root, group_count: parser.group_count })
}

type Captures = Vec<Option<(usize, usize)>>;

struct Matcher<'a> {
    text: &'a [char],
    steps: u64,
}

impl Matcher<'_> {
    fn step(&mut self) -> Result<(), RegexError> {
        self.steps += 1;
        if self.steps > STEP_LIMIT {
            return Err(RegexError::LimitExhausted);
        }
        Ok(())
    }

    fn is_word_at(&self, position: usize) -> bool {
        self.text
            .get(position)
            .is_some_and(|value| predicate_matches(Predicate::Word, *value))
    }

    /// Match `node` at `position`, calling `next` with each end position in
    /// priority order. Returning `true` from `next` stops the search.
    fn run(
        &mut self,
        node: &Node,
        position: usize,
        captures: &mut Captures,
        next: &mut dyn FnMut(&mut Self, usize, &mut Captures) -> Result<bool, RegexError>,
    ) -> Result<bool, RegexError> {
        self.step()?;
        match node {
            Node::Empty => next(self, position, captures),
            Node::Literal(expected) => match self.text.get(position) {
                Some(value) if value == expected => next(self, position + 1, captures),
                _ => Ok(false),
            },
            Node::Any => match self.text.get(position) {
                Some(_) => next(self, position + 1, captures),
                None => Ok(false),
            },
            Node::Predicate(predicate) => match self.text.get(position) {
                Some(value) if predicate_matches(*predicate, *value) => {
                    next(self, position + 1, captures)
                }
                _ => Ok(false),
            },
            Node::Class { negated, items } => match self.text.get(position) {
                Some(value) => {
                    let listed = items.iter().any(|item| match item {
                        ClassItem::Literal(literal) => literal == value,
                        ClassItem::Range(low, high) => low <= value && value <= high,
                        ClassItem::Predicate(predicate) => predicate_matches(*predicate, *value),
                    });
                    if listed != *negated {
                        next(self, position + 1, captures)
                    } else {
                        Ok(false)
                    }
                }
                None => Ok(false),
            },
            Node::Start => {
                if position == 0 {
                    next(self, position, captures)
                } else {
                    Ok(false)
                }
            }
            Node::End => {
                if position == self.text.len() {
                    next(self, position, captures)
                } else {
                    Ok(false)
                }
            }
            Node::WordBoundary => {
                let before = position > 0 && self.is_word_at(position - 1);
                if before != self.is_word_at(position) {
                    next(self, position, captures)
                } else {
                    Ok(false)
                }
            }
            Node::Group { index, inner } => {
                let index = *index;
                self.run(inner, position, captures, &mut |matcher, end, captures| {
                    let Some(index) = index else {
                        return next(matcher, end, captures);
                    };
                    let previous = captures[index];
                    captures[index] = Some((position, end));
                    let stop = next(matcher, end, captures)?;
                    captures[index] = previous;
                    Ok(stop)
                })
            }
            Node::Concat(items) => self.run_sequence(items, position, captures, next),
            Node::Alternation(branches) => {
                for branch in branches {
                    if self.run(branch, position, captures, next)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            Node::Repeat { inner, min, max, greedy } => {
                self.run_repeat(inner, *min, *max, *greedy, 0, position, captures, next)
            }
        }
    }

    fn run_sequence(
        &mut self,
        items: &[Node],
        position: usize,
        captures: &mut Captures,
        next: &mut dyn FnMut(&mut Self, usize, &mut Captures) -> Result<bool, RegexError>,
    ) -> Result<bool, RegexError> {
        match items.split_first() {
            None => next(self, position, captures),
            Some((first, rest)) => self.run(first, position, captures, &mut |matcher, end, captures| {
                matcher.run_sequence(rest, end, captures, next)
            }),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn run_repeat(
        &mut self,
        inner: &Node,
        min: u32,
        max: Option<u32>,
        greedy: bool,
        count: u32,
        position: usize,
        captures: &mut Captures,
        next: &mut dyn FnMut(&mut Self, usize, &mut Captures) -> Result<bool, RegexError>,
    ) -> Result<bool, RegexError> {
        let may_repeat = max.is_none_or(|upper| count < upper);
        let try_iteration = |matcher: &mut Self,
                                 captures: &mut Captures,
                                 next: &mut dyn FnMut(&mut Self, usize, &mut Captures) -> Result<bool, RegexError>|
         -> Result<bool, RegexError> {
            if !may_repeat {
                return Ok(false);
            }
            matcher.run(inner, position, captures, &mut |matcher, end, captures| {
                // An iteration that consumes nothing cannot make progress once
                // the minimum is met, so it is not explored again.
                if end == position && count >= min {
                    return Ok(false);
                }
                matcher.run_repeat(inner, min, max, greedy, count + 1, end, captures, next)
            })
        };
        if count < min {
            return try_iteration(self, captures, next);
        }
        if greedy {
            if try_iteration(self, captures, next)? {
                return Ok(true);
            }
            next(self, position, captures)
        } else {
            if next(self, position, captures)? {
                return Ok(true);
            }
            try_iteration(self, captures, next)
        }
    }
}

impl Regex {
    /// Number of capturing groups, excluding the whole match.
    pub fn group_count(&self) -> usize {
        self.group_count
    }

    /// Return the leftmost-longest match starting at or after `from`.
    pub fn find_from(&self, text: &[char], from: usize) -> Result<Option<Match>, RegexError> {
        let mut matcher = Matcher { text, steps: 0 };
        for start in from..=text.len() {
            if let Some(found) = self.match_at(&mut matcher, start)? {
                return Ok(Some(found));
            }
        }
        Ok(None)
    }

    fn match_at(&self, matcher: &mut Matcher<'_>, start: usize) -> Result<Option<Match>, RegexError> {
        let mut best: Option<(usize, Captures)> = None;
        let mut captures: Captures = vec![None; self.group_count + 1];
        matcher.run(&self.root, start, &mut captures, &mut |_, end, captures| {
            if best.as_ref().is_none_or(|(best_end, _)| end > *best_end) {
                best = Some((end, captures.clone()));
            }
            // Keep exploring so a longer end at this start can still win.
            Ok(false)
        })?;
        Ok(best.map(|(end, captures)| {
            let mut captures = captures;
            captures[0] = Some((start, end));
            Match { start, end, captures }
        }))
    }

    /// Return every match in scalar order using the `orna.regex/1` scanning
    /// rule: positive-width matches resume at their end, zero-width matches
    /// advance one scalar, and a zero-width match at end-of-input is kept once.
    pub fn find_all(&self, text: &[char]) -> Result<Vec<Match>, RegexError> {
        let mut found = Vec::new();
        let mut position = 0;
        loop {
            let Some(next) = self.find_from(text, position)? else {
                break;
            };
            let (start, end) = (next.start, next.end);
            found.push(next);
            if end > start {
                position = end;
            } else if start >= text.len() {
                break;
            } else {
                position = start + 1;
            }
        }
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scalars(text: &str) -> Vec<char> {
        text.chars().collect()
    }

    fn first(pattern: &str, text: &str) -> Option<(usize, usize)> {
        compile(pattern)
            .expect("pattern compiles")
            .find_from(&scalars(text), 0)
            .expect("within budget")
            .map(|found| (found.start, found.end))
    }

    fn matched<'a>(pattern: &str, text: &'a str) -> Option<&'a str> {
        let chars = scalars(text);
        let found = compile(pattern)
            .expect("pattern compiles")
            .find_from(&chars, 0)
            .expect("within budget")?;
        Some(&text[text.char_indices().nth(found.start)?.0..text.char_indices().nth(found.end).map_or(text.len(), |(index, _)| index)])
    }

    fn syntax_position(pattern: &str) -> usize {
        match compile(pattern) {
            Err(RegexError::Syntax { position, .. }) => position,
            other => panic!("expected a syntax error for {pattern:?}, got {other:?}"),
        }
    }

    #[test]
    fn literals_and_escaped_metacharacters_match_exactly() {
        assert_eq!(first("a.b", "xa.b"), Some((1, 4)));
        assert_eq!(first(r"a\.b", "axb a.b"), Some((4, 7)));
        assert_eq!(first(r"\(\)", "f()"), Some((1, 3)));
        assert_eq!(first(r"\n", "a\nb"), Some((1, 2)));
    }

    #[test]
    fn dot_matches_any_scalar_and_anchors_are_whole_text() {
        assert_eq!(first("^a", "ba"), None);
        assert_eq!(first("a$", "ab"), None);
        assert_eq!(first("^ab$", "ab"), Some((0, 2)));
        assert_eq!(first("a.c", "a😀c"), Some((0, 3)));
    }

    #[test]
    fn bracket_classes_support_ranges_negation_and_escapes() {
        assert_eq!(first("[a-c]+", "xxbcaz"), Some((2, 5)));
        assert_eq!(first("[^a-c]", "abcd"), Some((3, 4)));
        assert_eq!(first(r"[\d_]+", "ab12_x"), Some((2, 5)));
        assert_eq!(syntax_position("[]"), 2);
    }

    #[test]
    fn alternation_is_leftmost_longest_at_the_same_start() {
        assert_eq!(matched("ab|abc", "xabcd"), Some("abc"));
        assert_eq!(matched("a|ab", "ab"), Some("ab"));
        assert_eq!(first("z|a", "ba"), Some((1, 2)));
    }

    #[test]
    fn quantifiers_match_the_longest_span_at_the_leftmost_start() {
        assert_eq!(matched("a+", "baaa"), Some("aaa"));
        // The whole match is leftmost-longest, so the lazy suffix does not
        // shorten the span; it only changes which capture wins at equal length.
        assert_eq!(matched("a+?", "baaa"), Some("aaa"));
        assert_eq!(matched("a*", "baaa"), Some(""));
        assert_eq!(matched("a??b", "ab"), Some("ab"));
    }

    #[test]
    fn lazy_quantifiers_only_reorder_captures_at_equal_length() {
        let pattern = compile("(a+?)(a*)").expect("compiles");
        let found = pattern.find_from(&scalars("aaa"), 0).expect("within budget").expect("matches");
        assert_eq!(found.captures, vec![Some((0, 3)), Some((0, 1)), Some((1, 3))]);
    }

    #[test]
    fn counted_repetition_respects_its_bounds() {
        assert_eq!(matched("a{2,3}", "aaaaa"), Some("aaa"));
        assert_eq!(matched("a{2,}", "aaaaa"), Some("aaaaa"));
        assert_eq!(matched("a{2}", "aaaaa"), Some("aa"));
        assert_eq!(matched("a{2,3}?", "aaaaa"), Some("aaa"));
        assert_eq!(first("a{2,3}", "a"), None);
    }

    #[test]
    fn captures_report_participation_in_opening_group_order() {
        let pattern = compile("(a)|(b)").expect("compiles");
        let found = pattern.find_from(&scalars("b"), 0).expect("within budget").expect("matches");
        assert_eq!(found.captures, vec![Some((0, 1)), None, Some((0, 1))]);
        assert_eq!(pattern.group_count(), 2);
    }

    #[test]
    fn nested_groups_keep_the_last_iteration_capture() {
        let pattern = compile("((a)b)+").expect("compiles");
        let found = pattern.find_from(&scalars("abab"), 0).expect("within budget").expect("matches");
        assert_eq!(found.captures, vec![Some((0, 4)), Some((2, 4)), Some((2, 3))]);
    }

    #[test]
    fn noncapturing_groups_do_not_consume_an_index() {
        let pattern = compile("(?:a)(b)").expect("compiles");
        assert_eq!(pattern.group_count(), 1);
    }

    #[test]
    fn word_boundaries_separate_words_from_non_words() {
        assert_eq!(matched(r"\bcat\b", "a cat sat"), Some("cat"));
        assert_eq!(first(r"\bcat\b", "concat"), None);
        assert_eq!(first(r"\b", ""), None);
    }

    #[test]
    fn unicode_predicates_follow_their_properties() {
        assert_eq!(matched(r"\d+", "x٣٤"), Some("٣٤"));
        assert_eq!(matched(r"\w+", "éa1_ "), Some("éa1_"));
        assert_eq!(matched(r"\s", "a\u{3000}b"), Some("\u{3000}"));
        assert_eq!(first(r"\d", "½"), None);
    }

    #[test]
    fn find_all_resumes_after_positive_matches_and_keeps_empty_matches_once() {
        let pattern = compile("a*").expect("compiles");
        let spans: Vec<(usize, usize)> = pattern
            .find_all(&scalars("baaa"))
            .expect("within budget")
            .iter()
            .map(|found| (found.start, found.end))
            .collect();
        assert_eq!(spans, vec![(0, 0), (1, 4), (4, 4)]);
    }

    #[test]
    fn unsupported_constructs_fail_with_their_scalar_position() {
        assert_eq!(syntax_position("(?=a)"), 2);
        assert_eq!(syntax_position(r"(a)\1"), 4);
        assert_eq!(syntax_position("(a"), 2);
        assert_eq!(syntax_position("a)"), 1);
        assert_eq!(syntax_position("*a"), 0);
        assert_eq!(syntax_position("a{3,2}"), 6);
        assert_eq!(syntax_position("a{1001}"), 6);
        assert_eq!(syntax_position("[z-a]"), 4);
        assert_eq!(syntax_position("a**"), 3);
        assert_eq!(syntax_position(r"\q"), 1);
    }

    #[test]
    fn exponential_backtracking_fails_at_the_budget_instead_of_hanging() {
        let pattern = compile("(a|a)*c").expect("compiles");
        let text = scalars(&"a".repeat(64));
        assert_eq!(pattern.find_from(&text, 0), Err(RegexError::LimitExhausted));
    }

    const ANCHOR_FIXTURE: &str = include_str!("../tests/fixtures/stdlib-regex-lite-anchors.orna");

    // Read the string literal returned by `pub fn {name}(): Str = "..."`.
    fn fixture_string(name: &str) -> &'static str {
        let marker = format!("pub fn {name}(): Str = \"");
        let start = ANCHOR_FIXTURE.find(&marker).expect("fixture declares the function") + marker.len();
        let length = ANCHOR_FIXTURE[start..].find('"').expect("fixture literal is closed");
        &ANCHOR_FIXTURE[start..start + length]
    }

    #[test]
    fn anchored_fixture_matches_only_the_whole_text() {
        let pattern = compile(fixture_string("anchored_pattern")).expect("fixture pattern compiles");
        let anchored = scalars(fixture_string("anchored_text"));
        let embedded = scalars(fixture_string("embedded_text"));
        assert_eq!(
            pattern.find_from(&anchored, 0).expect("within budget").map(|found| (found.start, found.end)),
            Some((0, 2))
        );
        assert_eq!(pattern.find_from(&embedded, 0).expect("within budget"), None);
    }
}
