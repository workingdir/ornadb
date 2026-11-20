//! Canonical JSON check for the opaque JSON codec.

use std::borrow::Cow;

/// Returns whether `body` is byte-identical to its canonical form. `body` must
/// already have passed `serde_json` parsing, which bounds the nesting depth this
/// recursive check relies on. The canonical form has no insignificant
/// whitespace; object keys are strictly ascending by UTF-8 bytes, which also
/// rules out duplicate keys; each string is written exactly as `serde_json`
/// escapes its decoded text; and each number is the token received, which must
/// not carry an exponent. Negative zero is the standard JSON token `-0`.
///
/// The received bytes are checked directly instead of re-rendering a parsed
/// value. A re-rendered copy would depend on serde_json features that other
/// crates in the build may enable: `preserve_order` changes key order, and
/// `-0` keeps its sign only under `arbitrary_precision`.
pub(super) fn is_canonical_json(body: &[u8]) -> bool {
    let mut scanner = Scanner { body, position: 0 };
    scanner.value() && scanner.position == body.len()
}

struct Scanner<'a> {
    body: &'a [u8],
    position: usize,
}

impl<'a> Scanner<'a> {
    fn value(&mut self) -> bool {
        match self.body.get(self.position) {
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => self.string().is_some(),
            Some(b't') => self.literal(b"true"),
            Some(b'f') => self.literal(b"false"),
            Some(b'n') => self.literal(b"null"),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => false,
        }
    }

    fn object(&mut self) -> bool {
        if !self.eat(b'{') {
            return false;
        }
        if self.eat(b'}') {
            return true;
        }
        let mut previous: Option<Cow<'a, [u8]>> = None;
        loop {
            let Some(key) = self.string() else {
                return false;
            };
            if previous.as_deref().is_some_and(|prior| prior >= &*key) {
                return false;
            }
            if !self.eat(b':') || !self.value() {
                return false;
            }
            if self.eat(b'}') {
                return true;
            }
            if !self.eat(b',') {
                return false;
            }
            previous = Some(key);
        }
    }

    fn array(&mut self) -> bool {
        if !self.eat(b'[') {
            return false;
        }
        if self.eat(b']') {
            return true;
        }
        loop {
            if !self.value() {
                return false;
            }
            if self.eat(b']') {
                return true;
            }
            if !self.eat(b',') {
                return false;
            }
        }
    }

    /// Consumes one string token and returns its decoded UTF-8 bytes, or `None`
    /// when the token is not the canonical escaping of those bytes. A string
    /// without a backslash is already canonical, so its bytes are borrowed.
    fn string(&mut self) -> Option<Cow<'a, [u8]>> {
        let body = self.body;
        let start = self.position;
        if body.get(start) != Some(&b'"') {
            return None;
        }
        let mut end = start + 1;
        let mut escaped = false;
        loop {
            match body.get(end)? {
                b'\\' => {
                    escaped = true;
                    end += 2;
                }
                b'"' => break,
                _ => end += 1,
            }
        }
        self.position = end + 1;
        let token = &body[start..=end];
        if !escaped {
            return Some(Cow::Borrowed(&token[1..token.len() - 1]));
        }
        let decoded: String = serde_json::from_slice(token).ok()?;
        let mut canonical = Vec::with_capacity(token.len());
        serde_json::to_writer(&mut canonical, &decoded).ok()?;
        (canonical == token).then(|| Cow::Owned(decoded.into_bytes()))
    }

    /// Consumes one number token. `serde_json` has already checked the grammar,
    /// so the token is canonical exactly when it has no exponent.
    fn number(&mut self) -> bool {
        let start = self.position;
        while matches!(
            self.body.get(self.position),
            Some(b'-' | b'+' | b'.' | b'0'..=b'9' | b'e' | b'E')
        ) {
            self.position += 1;
        }
        !self.body[start..self.position]
            .iter()
            .any(|byte| matches!(byte, b'e' | b'E'))
    }

    fn literal(&mut self, text: &[u8]) -> bool {
        if self
            .body
            .get(self.position..)
            .is_some_and(|rest| rest.starts_with(text))
        {
            self.position += text.len();
            true
        } else {
            false
        }
    }

    fn eat(&mut self, expected: u8) -> bool {
        if self.body.get(self.position) == Some(&expected) {
            self.position += 1;
            true
        } else {
            false
        }
    }
}
