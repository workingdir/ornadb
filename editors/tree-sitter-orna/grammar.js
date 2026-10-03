// Generated from orna-syntax-v1 lexer token definitions; edit the Rust source instead.
module.exports = grammar({
  name: 'orna',
  extras: $ => [/\s/],
  word: $ => $.identifier,
  rules: {
    source_file: $ => repeat(choice($.comment, $.string, $.keyword, $.identifier, $.number, $.operator, $.punctuation)),
    comment: $ => choice($.line_comment, $.block_comment),
    line_comment: _ => token(seq('//', /[^\r\n]*/)),
    block_comment: $ => seq('/*', repeat(choice(/[^*/]+/, /\*[^/]/, /\/[^*]/, $.block_comment)), '*/'),
    string: _ => token(seq('"', repeat(choice(/[^"\\]/, /\\./)), '"')),
    keyword: _ => token(prec(2, choice("as", "assert", "base", "break", "case", "continue", "dim", "else", "enum", "false", "fn", "for", "if", "impl", "in", "let", "loop", "null", "offset", "affine", "protocol", "pub", "return", "self", "static", "table", "true", "type", "unit", "use", "while"))),
    identifier: _ => token(prec(1, /[_\p{XID_Start}][_\p{XID_Continue}]*/)),
    number: _ => token(/(?:[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(?:\.[0-9]*)?(?:Z|[+-][0-9]{2}:[0-9]{2})|[0-9]{4}-[0-9]{2}-[0-9]{2}|0x[0-9A-Fa-f_]*|0b[01_]*|[0-9][0-9_]*(?:\.[0-9_]+)?(?:[eE][+-]?[0-9_]*)?f?)/),
    operator: _ => token(choice("..=", "=>", "==", "!=", "<=", ">=", "??", "|?", "&&", "||", "+=", "-=", "*=", "/=", "..", "|", "!", "=", "<", ">", "+", "-", "*", "/", "%", "^", "?")),
    punctuation: _ => token(choice("{", "}", "(", ")", "[", "]", ",", ";", ":", "."))
  }
});
