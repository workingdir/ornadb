//! Strict JSON syntax for the `core.encoding.json` natives. The grammar is RFC 8259: duplicate object keys, lone
//! surrogate escapes and trailing content are rejected. Number tokens stay as their source text, so each target type
//! parses them exactly and no value is rounded through a binary Float on the way.

use std::collections::HashSet;

use crate::error::Stop;
use crate::machine::ExecLimits;

/// One parsed JSON value. Numbers keep their source token.
#[derive(Debug)]
pub(super) enum Node {
    Null,
    Bool(bool),
    Number(String),
    Text(String),
    Array(Vec<Node>),
    Object(Vec<(String, Node)>),
}

/// Parses `input` as exactly one JSON value. Syntax errors are `ORNA-EVAL-VALUE`; exceeding a limit is
/// `ORNA-EVAL-LIMIT`.
pub(super) fn parse(limits: &ExecLimits, input: &str) -> Result<Node, Stop> {
    let mut parser = Parser {
        bytes: input.as_bytes(),
        position: 0,
        limits,
    };
    parser.skip_whitespace();
    let node = parser.value(0)?;
    parser.skip_whitespace();
    if parser.position != parser.bytes.len() {
        return Err(invalid());
    }
    Ok(node)
}

/// The error for any input that is not strict JSON for the requested target.
pub(super) fn invalid() -> Stop {
    Stop::Error("ORNA-EVAL-VALUE")
}

struct Parser<'a> {
    bytes: &'a [u8],
    position: usize,
    limits: &'a ExecLimits,
}

impl Parser<'_> {
    fn value(&mut self, depth: usize) -> Result<Node, Stop> {
        if depth > self.limits.max_depth {
            return Err(Stop::Error("ORNA-EVAL-LIMIT"));
        }
        match self.peek() {
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => self.string().map(Node::Text),
            Some(b't') => self.literal(b"true", Node::Bool(true)),
            Some(b'f') => self.literal(b"false", Node::Bool(false)),
            Some(b'n') => self.literal(b"null", Node::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => Err(invalid()),
        }
    }

    fn object(&mut self, depth: usize) -> Result<Node, Stop> {
        self.position += 1;
        let mut fields = Vec::new();
        let mut keys = HashSet::new();
        self.skip_whitespace();
        if self.eat(b'}') {
            return Ok(Node::Object(fields));
        }
        loop {
            self.skip_whitespace();
            if self.peek() != Some(b'"') {
                return Err(invalid());
            }
            let key = self.string()?;
            if !keys.insert(key.clone()) {
                return Err(invalid());
            }
            self.skip_whitespace();
            if !self.eat(b':') {
                return Err(invalid());
            }
            self.skip_whitespace();
            let value = self.value(depth + 1)?;
            self.check_items(fields.len())?;
            fields.push((key, value));
            self.skip_whitespace();
            if self.eat(b'}') {
                return Ok(Node::Object(fields));
            }
            if !self.eat(b',') {
                return Err(invalid());
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<Node, Stop> {
        self.position += 1;
        let mut items = Vec::new();
        self.skip_whitespace();
        if self.eat(b']') {
            return Ok(Node::Array(items));
        }
        loop {
            self.skip_whitespace();
            let item = self.value(depth + 1)?;
            self.check_items(items.len())?;
            items.push(item);
            self.skip_whitespace();
            if self.eat(b']') {
                return Ok(Node::Array(items));
            }
            if !self.eat(b',') {
                return Err(invalid());
            }
        }
    }

    /// Fails when one more element would exceed the collection limit.
    fn check_items(&self, present: usize) -> Result<(), Stop> {
        if present >= self.limits.max_collection_items {
            return Err(Stop::Error("ORNA-EVAL-LIMIT"));
        }
        Ok(())
    }

    fn string(&mut self) -> Result<String, Stop> {
        self.position += 1;
        let mut bytes = Vec::new();
        loop {
            let Some(byte) = self.next_byte() else {
                return Err(invalid());
            };
            match byte {
                b'"' => break,
                b'\\' => self.escape(&mut bytes)?,
                0..=0x1f => return Err(invalid()),
                _ => bytes.push(byte),
            }
            if bytes.len() > self.limits.max_string_bytes {
                return Err(Stop::Error("ORNA-EVAL-LIMIT"));
            }
        }
        String::from_utf8(bytes).map_err(|_| invalid())
    }

    fn escape(&mut self, bytes: &mut Vec<u8>) -> Result<(), Stop> {
        let Some(byte) = self.next_byte() else {
            return Err(invalid());
        };
        let character = match byte {
            b'"' => '"',
            b'\\' => '\\',
            b'/' => '/',
            b'b' => '\u{8}',
            b'f' => '\u{c}',
            b'n' => '\n',
            b'r' => '\r',
            b't' => '\t',
            b'u' => self.unicode_escape()?,
            _ => return Err(invalid()),
        };
        let mut encoded = [0u8; 4];
        bytes.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
        Ok(())
    }

    /// Reads the code unit after `\u`, joining a high surrogate with the low surrogate that must follow it.
    fn unicode_escape(&mut self) -> Result<char, Stop> {
        let unit = self.hex4()?;
        match unit {
            0xd800..=0xdbff => {
                if !(self.eat(b'\\') && self.eat(b'u')) {
                    return Err(invalid());
                }
                let low = self.hex4()?;
                if !(0xdc00..=0xdfff).contains(&low) {
                    return Err(invalid());
                }
                let code = 0x10000 + ((unit - 0xd800) << 10) + (low - 0xdc00);
                char::from_u32(code).ok_or_else(invalid)
            }
            0xdc00..=0xdfff => Err(invalid()),
            _ => char::from_u32(unit).ok_or_else(invalid),
        }
    }

    fn hex4(&mut self) -> Result<u32, Stop> {
        let mut code = 0u32;
        for _ in 0..4 {
            let digit = self
                .next_byte()
                .and_then(|byte| (byte as char).to_digit(16))
                .ok_or_else(invalid)?;
            code = code * 16 + digit;
        }
        Ok(code)
    }

    /// Reads one number token: `-?(0|[1-9][0-9]*)(\.[0-9]+)?([eE][+-]?[0-9]+)?`.
    fn number(&mut self) -> Result<Node, Stop> {
        let start = self.position;
        self.eat(b'-');
        if self.eat(b'0') {
        } else if !self.digits() {
            return Err(invalid());
        }
        if self.eat(b'.') && !self.digits() {
            return Err(invalid());
        }
        if self.eat(b'e') || self.eat(b'E') {
            if !self.eat(b'+') {
                self.eat(b'-');
            }
            if !self.digits() {
                return Err(invalid());
            }
        }
        let token =
            std::str::from_utf8(&self.bytes[start..self.position]).map_err(|_| invalid())?;
        Ok(Node::Number(token.to_owned()))
    }

    /// Consumes one or more decimal digits, reporting whether there was any.
    fn digits(&mut self) -> bool {
        let start = self.position;
        while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            self.position += 1;
        }
        self.position > start
    }

    fn literal(&mut self, word: &[u8], node: Node) -> Result<Node, Stop> {
        if self.bytes[self.position..].starts_with(word) {
            self.position += word.len();
            Ok(node)
        } else {
            Err(invalid())
        }
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.position += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.position).copied()
    }

    fn next_byte(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.position += 1;
        Some(byte)
    }

    fn eat(&mut self, expected: u8) -> bool {
        if self.peek() == Some(expected) {
            self.position += 1;
            true
        } else {
            false
        }
    }
}
