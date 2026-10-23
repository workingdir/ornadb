use crate::lexer::{Token, TokenKind};

/// Sentinel for "no such position" in the next-index tables below.
const NO_POSITION: usize = usize::MAX;

/// Answers the parser's two forward lookaheads once per token array.
///
/// `has_terminated_initializer` and `generic_call_ahead` used to rescan the
/// tokens from every probe point, which is quadratic on inputs such as many
/// `var` lines without `;`. The outcome of each scan depends only on running
/// sums of bracket depth (`(`/`[`/`{` = +1, `)`/`]`/`}` = -1) and of angle
/// depth (`<` = +1, `>` = -1). The first position where a scan from `s` stops
/// is a next-index query on those sums, so both answers are precomputed in
/// linear time and the parser reads them in constant time.
#[derive(Clone)]
pub(in crate::parser) struct TokenLookahead {
    /// `terminated_initializers[s]` is the result of the terminator scan that
    /// starts at token `s`.
    terminated_initializers: Vec<bool>,
    /// `generic_calls[s]` is the result of the generic-call scan when the
    /// parser is at token `s`.
    generic_calls: Vec<bool>,
}

impl TokenLookahead {
    pub(in crate::parser) fn new(tokens: &[Token]) -> Self {
        let len = tokens.len();
        // `depth[k]` / `angles[k]` are the sums over tokens `0..k`.
        let mut depth = vec![0i64; len + 1];
        let mut angles = vec![0i64; len + 1];
        for (index, token) in tokens.iter().enumerate() {
            let (bracket, angle) = match token.kind {
                TokenKind::Punct("(" | "[" | "{") => (1, 0),
                TokenKind::Punct(")" | "]" | "}") => (-1, 0),
                TokenKind::Punct("<") => (0, 1),
                TokenKind::Punct(">") => (0, -1),
                _ => (0, 0),
            };
            depth[index + 1] = depth[index] + bracket;
            angles[index + 1] = angles[index] + angle;
        }

        // The initializer scan from `s` returns true at the first `;` where the
        // saturating depth is zero, that is, where `depth[j]` is no larger than
        // `depth[k]` for every `k` in `s..=j`. Those positions form the chain
        // `s -> next_at_or_below(s) -> ...`, so the first `;` on that chain is
        // the answer, provided it comes before the first `Eof`.
        let next_at_or_below = next_index(&depth, false);
        let mut chain_semicolon = vec![NO_POSITION; len + 1];
        let mut next_eof = vec![NO_POSITION; len + 1];
        for index in (0..len).rev() {
            let next = next_at_or_below[index];
            chain_semicolon[index] = if matches!(tokens[index].kind, TokenKind::Punct(";")) {
                index
            } else if next == NO_POSITION {
                NO_POSITION
            } else {
                chain_semicolon[next]
            };
            next_eof[index] = if matches!(tokens[index].kind, TokenKind::Eof) {
                index
            } else {
                next_eof[index + 1]
            };
        }
        let terminated_initializers = (0..len)
            .map(|start| {
                if matches!(tokens[start].kind, TokenKind::Punct(";") | TokenKind::Eof) {
                    return false;
                }
                let next = next_at_or_below[start];
                if next == NO_POSITION {
                    return false;
                }
                let semicolon = chain_semicolon[next];
                semicolon != NO_POSITION && semicolon < next_eof[start]
            })
            .collect();

        // The generic scan from `<` at `s` stops at the first of: a stop token
        // (`;` `=` `=>` `&&` `||` or Eof), an unmatched closer, or the `>` that
        // returns the angle depth to zero. The angle depth returns to its
        // starting level at `k` (the first `k > s` with `angles[k] <= angles[s]`)
        // exactly when token `k - 1` is that `>`. The unmatched closer is at
        // `k - 1` for the first `k > s` with `depth[k] < depth[s]`.
        let mut next_stop = vec![NO_POSITION; len + 1];
        for index in (0..len).rev() {
            next_stop[index] = if matches!(
                tokens[index].kind,
                TokenKind::Punct(";" | "=" | "=>" | "&&" | "||") | TokenKind::Eof
            ) {
                index
            } else {
                next_stop[index + 1]
            };
        }
        let depth_drop = next_index(&depth, true);
        let angle_return = next_index(&angles, false);
        let generic_calls = (0..len)
            .map(|start| {
                if !matches!(tokens[start].kind, TokenKind::Punct("<")) {
                    return false;
                }
                let returned = angle_return[start];
                if returned == NO_POSITION {
                    return false;
                }
                let close = returned - 1;
                let unmatched = match depth_drop[start] {
                    NO_POSITION => NO_POSITION,
                    dropped => dropped - 1,
                };
                close < next_stop[start]
                    && close < unmatched
                    && matches!(
                        tokens.get(returned).map(|token| &token.kind),
                        Some(TokenKind::Punct("("))
                    )
            })
            .collect();

        Self {
            terminated_initializers,
            generic_calls,
        }
    }

    /// Same answer as the forward terminator scan from `start`.
    pub(in crate::parser) fn has_terminated_initializer(&self, start: usize) -> bool {
        self.terminated_initializers
            .get(start)
            .copied()
            .unwrap_or(false)
    }

    /// Same answer as the forward generic-call scan from `at`.
    pub(in crate::parser) fn generic_call_at(&self, at: usize) -> bool {
        self.generic_calls.get(at).copied().unwrap_or(false)
    }
}

/// For each position `p`, the first `k > p` with `values[k] <= values[p]`, or
/// with `values[k] < values[p]` when `strict`. `NO_POSITION` when none exists.
fn next_index(values: &[i64], strict: bool) -> Vec<usize> {
    let mut next = vec![NO_POSITION; values.len()];
    let mut pending: Vec<usize> = Vec::new();
    for (index, &value) in values.iter().enumerate() {
        while let Some(&top) = pending.last() {
            let found = if strict {
                values[top] > value
            } else {
                values[top] >= value
            };
            if !found {
                break;
            }
            next[top] = index;
            pending.pop();
        }
        pending.push(index);
    }
    next
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::{Keyword, TokenKind};
    use crate::parser::SourceSpan;

    /// The forward scans that `TokenLookahead` replaced, kept as the oracle.
    fn naive_terminated_initializer(tokens: &[Token], start: usize) -> bool {
        if matches!(
            tokens.get(start).map(|token| &token.kind),
            None | Some(TokenKind::Punct(";")) | Some(TokenKind::Eof)
        ) {
            return false;
        }
        let mut nesting = 0usize;
        for token in &tokens[start..] {
            match token.kind {
                TokenKind::Punct("(" | "[" | "{") => nesting += 1,
                TokenKind::Punct(")" | "]" | "}") => nesting = nesting.saturating_sub(1),
                TokenKind::Punct(";") if nesting == 0 => return true,
                TokenKind::Eof => return false,
                _ => {}
            }
        }
        false
    }

    fn naive_generic_call_ahead(tokens: &[Token], at: usize) -> bool {
        if !matches!(
            tokens.get(at).map(|token| &token.kind),
            Some(TokenKind::Punct("<"))
        ) {
            return false;
        }
        let mut angles = 0usize;
        let mut delimiters = 0usize;
        for (index, token) in tokens.iter().enumerate().skip(at) {
            match token.kind {
                TokenKind::Punct("<") => angles += 1,
                TokenKind::Punct(">") if angles > 0 => {
                    angles -= 1;
                    if angles == 0 {
                        return matches!(
                            tokens.get(index + 1).map(|next| &next.kind),
                            Some(TokenKind::Punct("("))
                        );
                    }
                }
                TokenKind::Punct("(" | "[" | "{") => delimiters += 1,
                TokenKind::Punct(")" | "]" | "}") => match delimiters.checked_sub(1) {
                    Some(depth) => delimiters = depth,
                    None => return false,
                },
                TokenKind::Punct(";" | "=" | "=>" | "&&" | "||") | TokenKind::Eof => return false,
                _ => {}
            }
        }
        false
    }

    fn token(kind: TokenKind) -> Token {
        Token {
            kind,
            text: String::new(),
            span: SourceSpan::new(0, 0),
        }
    }

    #[test]
    fn lookahead_answers_match_the_forward_scans_they_replace() {
        const PUNCTS: [&str; 15] = [
            "(", ")", "[", "]", "{", "}", "<", ">", ";", "=", "=>", "&&", "||", ">=", ".",
        ];
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..4000 {
            let length = (next() % 15) as usize;
            let tokens: Vec<Token> = (0..length)
                .map(|_| match next() % 20 {
                    0 => token(TokenKind::Eof),
                    1 => token(TokenKind::Identifier {
                        normalized: String::from("x"),
                    }),
                    2 => token(TokenKind::Keyword(Keyword::Pub)),
                    choice => token(TokenKind::Punct(PUNCTS[(choice as usize) % PUNCTS.len()])),
                })
                .collect();
            let lookahead = TokenLookahead::new(&tokens);
            for start in 0..=tokens.len() {
                assert_eq!(
                    lookahead.has_terminated_initializer(start),
                    naive_terminated_initializer(&tokens, start),
                    "initializer at {start} of {:?}",
                    tokens.iter().map(|t| &t.kind).collect::<Vec<_>>()
                );
                assert_eq!(
                    lookahead.generic_call_at(start),
                    naive_generic_call_ahead(&tokens, start),
                    "generic call at {start} of {:?}",
                    tokens.iter().map(|t| &t.kind).collect::<Vec<_>>()
                );
            }
        }
    }
}
