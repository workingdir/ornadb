// Generated from orna-syntax-v1 lexical rules; edit crates/orna-syntax-v1/src/editor.rs instead.
// A lexical highlighter: token classes only, not the orna.ebnf productions.
module.exports = grammar({
  name: "orna",
  extras: $ => [/\s/],
  word: $ => $.identifier,
  rules: {
    source_file: $ => repeat($._token),
    _token: $ => choice($.comment, $.string, $.block, $.member, $.label, $.keyword, $.number, $.operator, $.punctuation, $.identifier),
    block: $ => seq("{", repeat($._token), "}"),
    member: $ => prec(1, seq(".", choice($.identifier, alias($.keyword, $.identifier)))),
    label: $ => prec(1, seq(alias($._label_keyword, $.identifier), ":")),
    _label_keyword: _ => prec(1, choice("as", "assert", "base", "break", "case", "continue", "dim", "else", "enum", "fn", "for", "if", "impl", "in", "let", "loop", "offset", "affine", "protocol", "pub", "return", "self", "static", "table", "type", "unit", "use", "while")),
    comment: $ => choice($.line_comment, $.block_comment),
    line_comment: _ => token(prec(1, seq("//", /[^\r\n]*/))),
    block_comment: $ => seq(/\/+\*/, repeat(choice(/[^*/]+/, /\*+[^*/]/, /\/+[^*/]/, $.block_comment)), /\*+\//),
    string: $ => seq("\"", repeat(choice($.string_content, $.escape_sequence, $.interpolation)), token.immediate("\"")),
    string_content: _ => token.immediate(prec(1, /[^"\\{]+/)),
    escape_sequence: _ => token.immediate(/\\(?:["0\\nrt]|u\{[0-9A-Fa-f]{1,6}\})/),
    interpolation: $ => seq(token.immediate("{"), repeat($._token), "}"),
    keyword: _ => choice("as", "assert", "base", "break", "case", "continue", "dim", "else", "enum", "false", "fn", "for", "if", "impl", "in", "let", "loop", "null", "offset", "affine", "protocol", "pub", "return", "self", "static", "table", "true", "type", "unit", "use", "while"),
    identifier: _ => /[_\p{XID_Start}]\p{XID_Continue}*/,
    number: _ => token(/(?:[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(?:\.[0-9][0-9]*)?(?:Z|[+\-][0-9]{2}:[0-9]{2})|[0-9]{4}-[0-9]{2}-[0-9]{2}|0x[0-9A-Fa-f](?:_?[0-9A-Fa-f])*|0b[01](?:_?[01])*|[0-9](?:_?[0-9])*(?:\.[0-9](?:_?[0-9])*)?(?:[Ee][+\-]?[0-9](?:_?[0-9])*)?f?)/),
    operator: _ => token(choice("..=", "=>", "==", "!=", "<=", ">=", "??", "|?", "&&", "||", "+=", "-=", "*=", "/=", "..", "|", "!", "=", "<", ">", "+", "-", "*", "/", "%", "^", "?")),
    punctuation: _ => choice("(", ")", "[", "]", ",", ";", ":", ".")
  }
});
