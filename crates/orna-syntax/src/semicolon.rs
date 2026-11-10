//! The statement terminator a parser diagnostic says is missing.
//!
//! `ORNA-PARSE-002` carries its message text only. Editor quick fixes must
//! recognise exactly the messages the parser emits for a repairable missing
//! `;`, so the message spelling and that classification live together here,
//! next to the call sites that report them. A new terminator message is added
//! once, in [`MissingSemicolon::message`], and both the parser and the editor
//! read it from there.
//!
//! Two other `ORNA-PARSE-002` messages ("expected `;` before end of
//! statement" and "`continue` takes no value") are not statement terminators
//! and are deliberately absent: inserting `;` cannot repair them.

/// One production whose statement terminator is missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MissingSemicolon {
    DimensionDeclaration,
    UnitDeclaration,
    AssertionItem,
    LetItem,
    UseDeclaration,
    FunctionExpression,
    LetStatement,
    ControlStatement,
    ExpressionStatement,
    TableAssertion,
    StaticProperty,
    ProtocolFunction,
    ImplementationStaticProperty,
    TypeAssertion,
    NativeDeclaration,
}

impl MissingSemicolon {
    /// Every repairable terminator, for the classifier and its tests.
    pub const ALL: [Self; 15] = [
        Self::DimensionDeclaration,
        Self::UnitDeclaration,
        Self::AssertionItem,
        Self::LetItem,
        Self::UseDeclaration,
        Self::FunctionExpression,
        Self::LetStatement,
        Self::ControlStatement,
        Self::ExpressionStatement,
        Self::TableAssertion,
        Self::StaticProperty,
        Self::ProtocolFunction,
        Self::ImplementationStaticProperty,
        Self::TypeAssertion,
        Self::NativeDeclaration,
    ];

    /// The `ORNA-PARSE-002` message that names this production.
    pub const fn message(self) -> &'static str {
        match self {
            Self::DimensionDeclaration => "dimension declarations require `;`",
            Self::UnitDeclaration => "unit declarations require `;`",
            Self::AssertionItem => "assertion statements require `;`",
            Self::LetItem => "let statements require `;`",
            Self::UseDeclaration => "use declarations require `;`",
            Self::FunctionExpression => "expected `;` after function expression",
            Self::LetStatement => "expected `;` after let statement",
            Self::ControlStatement => "expected `;` after control statement",
            Self::ExpressionStatement => "expected `;` after expression statement",
            Self::TableAssertion => "expected `;` after table assertion",
            Self::StaticProperty => "expected `;` after static property",
            Self::ProtocolFunction => "protocol functions require `;`",
            Self::ImplementationStaticProperty => "implementation static properties require `;`",
            Self::TypeAssertion => "expected `;` after type assertion",
            Self::NativeDeclaration => "native declarations require `;`",
        }
    }

    /// Classifies the diagnostic `code` and `message` as a repairable missing
    /// terminator. Any other diagnostic, including the non-repairable
    /// `ORNA-PARSE-002` messages, is not one.
    pub fn classify(code: &str, message: &str) -> Option<Self> {
        if code != "ORNA-PARSE-002" {
            return None;
        }
        Self::ALL
            .into_iter()
            .find(|reason| reason.message() == message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_reason_has_a_distinct_message() {
        for (index, reason) in MissingSemicolon::ALL.into_iter().enumerate() {
            assert_eq!(
                MissingSemicolon::classify("ORNA-PARSE-002", reason.message()),
                Some(reason)
            );
            assert!(
                !MissingSemicolon::ALL[..index]
                    .iter()
                    .any(|earlier| earlier.message() == reason.message()),
                "duplicate message for {reason:?}"
            );
        }
    }

    #[test]
    fn only_parse_002_is_classified() {
        let reason = MissingSemicolon::ALL[0];
        assert_eq!(
            MissingSemicolon::classify("ORNA-PARSE-001", reason.message()),
            None
        );
        assert_eq!(
            MissingSemicolon::classify("ORNA-PARSE-002", "unterminated block"),
            None
        );
    }
}
