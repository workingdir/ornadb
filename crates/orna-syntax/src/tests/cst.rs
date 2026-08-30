//! Concrete syntax tree snapshot tests.

use super::*;
#[test]
fn cst_snapshot_preserves_schema_tokens_trivia_and_ranges() {
    let source = "CREATE SCHEMA app.core; -- keep";
    let parsed = parse(source);

    assert!(parsed.diagnostics().is_empty());
    assert_eq!(parsed.syntax().text(), source);
    assert_eq!(
        dump_cst(parsed.syntax().root()),
        "\
node Root [0..31]
  node CreateSchemaStatement [0..23]
    token Word \"CREATE\" [0..6]
    token Whitespace \" \" [6..7]
    token Word \"SCHEMA\" [7..13]
    token Whitespace \" \" [13..14]
    node QualifiedName [14..22]
      token Word \"app\" [14..17]
      token Dot \".\" [17..18]
      token Word \"core\" [18..22]
    token Semicolon \";\" [22..23]
  token Whitespace \" \" [23..24]
  token LineComment \"-- keep\" [24..31]
"
    );
}

#[test]
fn cst_snapshot_records_nested_client_call_structure() {
    let source = "CREATE CLIENT FUNCTION app.check(flag BOOL) RETURNS BOOL AS app.is_ready(flag);";
    let parsed = parse(source);

    assert!(parsed.diagnostics().is_empty());
    assert_eq!(parsed.syntax().text(), source);
    assert_eq!(
        dump_cst(parsed.syntax().root()),
        "\
node Root [0..79]
  node CreateClientFunctionStatement [0..79]
    token Word \"CREATE\" [0..6]
    token Whitespace \" \" [6..7]
    token Word \"CLIENT\" [7..13]
    token Whitespace \" \" [13..14]
    token Word \"FUNCTION\" [14..22]
    token Whitespace \" \" [22..23]
    node QualifiedName [23..32]
      token Word \"app\" [23..26]
      token Dot \".\" [26..27]
      token Word \"check\" [27..32]
    token LeftParenthesis \"(\" [32..33]
    node ClientFunctionParameter [33..42]
      token Word \"flag\" [33..37]
      token Whitespace \" \" [37..38]
      node NamedTypeSpecification [38..42]
        node QualifiedName [38..42]
          token Word \"BOOL\" [38..42]
    token RightParenthesis \")\" [42..43]
    token Whitespace \" \" [43..44]
    token Word \"RETURNS\" [44..51]
    token Whitespace \" \" [51..52]
    node ClientFunctionReturnType [52..57]
      node NamedTypeSpecification [52..57]
        node QualifiedName [52..57]
          token Word \"BOOL\" [52..56]
          token Whitespace \" \" [56..57]
    token Word \"AS\" [57..59]
    token Whitespace \" \" [59..60]
    node ClientExpressionBody [60..78]
      node ClientCallExpression [60..78]
        node QualifiedName [60..72]
          token Word \"app\" [60..63]
          token Dot \".\" [63..64]
          token Word \"is_ready\" [64..72]
        token LeftParenthesis \"(\" [72..73]
        node ClientCallArgument [73..77]
          token Word \"flag\" [73..77]
        token RightParenthesis \")\" [77..78]
    token Semicolon \";\" [78..79]
"
    );
}

#[test]
fn cst_snapshot_keeps_recovery_tokens_and_later_declaration() {
    let source = "CREATE SCHEMA app; ? CREATE SCHEMA later;";
    let parsed = parse(source);

    assert_eq!(parsed.diagnostics().len(), 1);
    assert_eq!(parsed.syntax().text(), source);
    assert_eq!(
        dump_cst(parsed.syntax().root()),
        "\
node Root [0..41]
  node CreateSchemaStatement [0..18]
    token Word \"CREATE\" [0..6]
    token Whitespace \" \" [6..7]
    token Word \"SCHEMA\" [7..13]
    token Whitespace \" \" [13..14]
    node QualifiedName [14..17]
      token Word \"app\" [14..17]
    token Semicolon \";\" [17..18]
  token Whitespace \" \" [18..19]
  token Other \"?\" [19..20]
  token Whitespace \" \" [20..21]
  node CreateSchemaStatement [21..41]
    token Word \"CREATE\" [21..27]
    token Whitespace \" \" [27..28]
    token Word \"SCHEMA\" [28..34]
    token Whitespace \" \" [34..35]
    node QualifiedName [35..40]
      token Word \"later\" [35..40]
    token Semicolon \";\" [40..41]
"
    );
}

fn dump_cst(root: &rowan::SyntaxNode<crate::parser::OrnaLanguage>) -> String {
    use std::fmt::Write as _;

    fn visit(
        node: &rowan::SyntaxNode<crate::parser::OrnaLanguage>,
        indent: usize,
        output: &mut String,
    ) {
        let range = node.text_range();
        writeln!(
            output,
            "{}node {:?} [{}..{}]",
            " ".repeat(indent),
            node.kind(),
            u32::from(range.start()),
            u32::from(range.end()),
        )
        .expect("writing CST node snapshot");

        for element in node.children_with_tokens() {
            match element {
                rowan::NodeOrToken::Node(child) => visit(&child, indent + 2, output),
                rowan::NodeOrToken::Token(token) => {
                    let range = token.text_range();
                    writeln!(
                        output,
                        "{}token {:?} {:?} [{}..{}]",
                        " ".repeat(indent + 2),
                        token.kind(),
                        token.text(),
                        u32::from(range.start()),
                        u32::from(range.end()),
                    )
                    .expect("writing CST token snapshot");
                }
            }
        }
    }

    let mut output = String::new();
    visit(root, 0, &mut output);
    output
}
