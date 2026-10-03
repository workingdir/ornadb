//! Markdown hover cards for syntax-v1 declarations.

use lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind};

pub fn declaration(
    kind: &str,
    name: &str,
    signature: Option<&str>,
    parameters: &[String],
    documentation: Option<&str>,
) -> Hover {
    let mut value = format!("**{kind}** `{name}`");
    if let Some(signature) = signature {
        value.push_str("\n\n```orna\n");
        value.push_str(signature);
        value.push_str("\n```");
    }
    if !parameters.is_empty() {
        value.push_str("\n\n**Parameters**\n");
        for parameter in parameters {
            value.push_str(&format!("- `{parameter}`\n"));
        }
        value.pop();
    }
    if let Some(documentation) = documentation.filter(|text| !text.trim().is_empty()) {
        value.push_str("\n\n");
        value.push_str(documentation.trim());
    }
    Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }),
        range: None,
    }
}
