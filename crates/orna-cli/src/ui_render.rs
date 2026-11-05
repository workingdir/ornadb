//! Renders `std.ui` presentation nodes as semantic HTML.
//!
//! The evaluator builds renderer-neutral node records
//! (`crates/orna-evaluator/src/ui.rs`); this module turns them into native
//! controls that stay usable without any stylesheet. A value that is not a
//! recognised node is left to the Inspect fallback, and every text value is
//! escaped so a stored string cannot introduce markup.

use orna_foundation::{CanonicalValue, OvbRaw};

/// Renders `value` as HTML when it is a `std.ui` node, or `None` so the caller
/// can fall back to Inspect.
pub fn render_ui_html(value: &CanonicalValue) -> Option<String> {
    let mut html = String::new();
    render_node(value.raw(), &mut html, 0)?;
    Some(html)
}

const MAX_DEPTH: usize = 32;

fn render_node(raw: &OvbRaw, out: &mut String, depth: usize) -> Option<()> {
    if depth >= MAX_DEPTH {
        return None;
    }
    let node = as_map(raw)?;
    if field(node, "kind").and_then(as_text)? != "node" {
        return None;
    }
    let contract = as_map(field(node, "contract")?)?;
    let name = field(contract, "name").and_then(as_text)?;
    let kind = name.strip_prefix("std.ui.")?;
    let properties = as_map(field(node, "properties")?)?;
    let slots = as_map(field(node, "slots")?)?;
    let children = field(slots, "content").and_then(as_list).unwrap_or(&[]);
    match kind {
        "text" => {
            out.push_str("<p>");
            push_value(properties, "value", out)?;
            out.push_str("</p>");
        }
        "field" => {
            out.push_str("<div><span class=\"label\">");
            push_value(properties, "label", out)?;
            out.push_str("</span> <span class=\"value\">");
            push_value(properties, "value", out)?;
            out.push_str("</span></div>");
        }
        "rows" | "cols" | "stack" => {
            out.push_str("<div>");
            for child in children {
                render_node(child, out, depth + 1)?;
            }
            out.push_str("</div>");
        }
        "details" => {
            out.push_str("<dl>");
            for child in children {
                render_node(child, out, depth + 1)?;
            }
            out.push_str("</dl>");
        }
        "table" => render_table_node(properties, children, out, depth)?,
        "code" => {
            out.push_str("<pre><code>");
            push_value(properties, "source", out)?;
            out.push_str("</code></pre>");
        }
        "button" => {
            out.push_str("<button type=\"submit\">");
            push_value(properties, "label", out)?;
            out.push_str("</button>");
        }
        "form" => {
            out.push_str("<form method=\"get\">");
            for child in children {
                render_node(child, out, depth + 1)?;
            }
            out.push_str("</form>");
        }
        "input" => {
            let name = properties_field_name(node)?;
            out.push_str("<label for=\"");
            push_escaped(&name, out);
            out.push_str("\">");
            push_value(properties, "label", out)?;
            out.push_str("</label><input id=\"");
            push_escaped(&name, out);
            out.push_str("\" name=\"");
            push_escaped(&name, out);
            out.push_str("\" type=\"text\">");
        }
        _ => return None,
    }
    Some(())
}

/// The name a form input posts under: the node's declared input name when the
/// node carries one, otherwise its label.
fn properties_field_name(node: &[(OvbRaw, OvbRaw)]) -> Option<String> {
    let properties = as_map(field(node, "properties")?)?;
    if let Some(name) = field(properties, "name").and_then(value_text) {
        return Some(name);
    }
    field(properties, "label").and_then(value_text)
}

fn push_value(properties: &[(OvbRaw, OvbRaw)], name: &str, out: &mut String) -> Option<()> {
    let value = field(properties, name).and_then(value_text)?;
    push_escaped(&value, out);
    Some(())
}

/// Renders a table node. Its records live in the `rows` property (the
/// evaluator materialises a relation there), so they are rendered from the
/// property rather than from child nodes; any child nodes still render after
/// the rows.
fn render_table_node(
    properties: &[(OvbRaw, OvbRaw)],
    children: &[OvbRaw],
    out: &mut String,
    depth: usize,
) -> Option<()> {
    let rows = field(properties, "rows")
        .and_then(value_list)
        .unwrap_or(&[]);
    let columns = table_columns(properties, rows);
    out.push_str("<table>");
    if !columns.is_empty() {
        out.push_str("<thead><tr>");
        for column in &columns {
            out.push_str("<th>");
            push_escaped(column, out);
            out.push_str("</th>");
        }
        out.push_str("</tr></thead>");
    }
    out.push_str("<tbody>");
    for row in rows {
        out.push_str("<tr>");
        if columns.is_empty() {
            out.push_str("<td>");
            if let Some(text) = scalar_text(row) {
                push_escaped(&text, out);
            }
            out.push_str("</td>");
        } else {
            let entries = as_map(row).unwrap_or(&[]);
            for column in &columns {
                out.push_str("<td>");
                if let Some(text) = field(entries, column).and_then(scalar_text) {
                    push_escaped(&text, out);
                }
                out.push_str("</td>");
            }
        }
        out.push_str("</tr>");
    }
    for child in children {
        render_node(child, out, depth + 1)?;
    }
    out.push_str("</tbody></table>");
    Some(())
}

/// A property entry's value when it is a list: `rows` carries the table's
/// records as `{ type, value }` with the records in `value`.
fn value_list(entry: &OvbRaw) -> Option<&[OvbRaw]> {
    let value = field(as_map(entry)?, "value")?;
    as_list(value)
}

/// The table's column labels: its declared `columns`, or the scalar field
/// names of its first row when the source declared none, so every row shares
/// one header.
fn table_columns(properties: &[(OvbRaw, OvbRaw)], rows: &[OvbRaw]) -> Vec<String> {
    let declared = field(properties, "columns")
        .and_then(value_list)
        .map(|labels| {
            labels
                .iter()
                .filter_map(|label| value_text(label).or_else(|| scalar_text(label)))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !declared.is_empty() {
        return declared;
    }
    rows.first()
        .and_then(as_map)
        .map(|row| {
            row.iter()
                .filter(|(_, value)| scalar_text(value).is_some())
                .filter_map(|(key, _)| as_text(key).map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Property entries carry `{ type, value }`; the rendered text is the value.
fn value_text(raw: &OvbRaw) -> Option<String> {
    let entry = as_map(raw)?;
    field(entry, "value").and_then(scalar_text)
}

fn scalar_text(raw: &OvbRaw) -> Option<String> {
    Some(match raw {
        OvbRaw::Text(text) => text.clone(),
        OvbRaw::Int(value) => value.to_string(),
        OvbRaw::Float(bits) => f64::from_bits(*bits).to_string(),
        OvbRaw::Bool(value) => value.to_string(),
        OvbRaw::Null => String::new(),
        _ => return None,
    })
}

fn as_map(raw: &OvbRaw) -> Option<&[(OvbRaw, OvbRaw)]> {
    match raw {
        OvbRaw::Map(entries) => Some(entries),
        _ => None,
    }
}

fn as_list(raw: &OvbRaw) -> Option<&[OvbRaw]> {
    match raw {
        OvbRaw::Array(items) => Some(items),
        _ => None,
    }
}

fn as_text(raw: &OvbRaw) -> Option<&str> {
    match raw {
        OvbRaw::Text(text) => Some(text),
        _ => None,
    }
}

fn field<'a>(entries: &'a [(OvbRaw, OvbRaw)], name: &str) -> Option<&'a OvbRaw> {
    entries
        .iter()
        .find_map(|(key, value)| (as_text(key) == Some(name)).then_some(value))
}

/// Escapes text for HTML element content and attribute values.
fn push_escaped(text: &str, out: &mut String) {
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// OVB orders map keys by their complete encoded form, and
    /// `CanonicalValue::new` validates that order instead of sorting.
    fn ordered(fields: Vec<(&str, OvbRaw)>) -> OvbRaw {
        let mut encoded = fields
            .into_iter()
            .map(|(key, value)| {
                let bytes = CanonicalValue::new(OvbRaw::Text(key.to_owned()))
                    .expect("text key is canonical")
                    .encode()
                    .expect("text key encodes");
                (bytes, key, value)
            })
            .collect::<Vec<_>>();
        encoded.sort_by(|(left, _, _), (right, _, _)| left.cmp(right));
        OvbRaw::Map(
            encoded
                .into_iter()
                .map(|(_, key, value)| (OvbRaw::Text(key.to_owned()), value))
                .collect(),
        )
    }

    fn entry(value: &str) -> OvbRaw {
        ordered(vec![
            ("type", OvbRaw::Text("std.text".into())),
            ("value", OvbRaw::Text(value.into())),
        ])
    }

    fn node(kind: &str, properties: Vec<(&str, OvbRaw)>, children: Vec<OvbRaw>) -> OvbRaw {
        ordered(vec![
            ("kind", OvbRaw::Text("node".into())),
            (
                "contract",
                ordered(vec![("name", OvbRaw::Text(format!("std.ui.{kind}")))]),
            ),
            ("properties", ordered(properties.into_iter().collect())),
            ("slots", ordered(vec![("content", OvbRaw::Array(children))])),
            ("actions", OvbRaw::Map(Vec::new())),
        ])
    }

    fn render(raw: OvbRaw) -> Option<String> {
        let value = CanonicalValue::new(raw).expect("node is canonical");
        render_ui_html(&value)
    }

    #[test]
    // Specified: Orna-1.1.0 std.ui renders native controls without CSS.
    fn renders_text_field_button_and_input_as_native_controls() {
        let text = node("text", vec![("value", entry("hello"))], Vec::new());
        assert_eq!(render(text).as_deref(), Some("<p>hello</p>"));

        let input = node(
            "input",
            vec![("label", entry("Query")), ("name", entry("q"))],
            Vec::new(),
        );
        let html = render(input).expect("input renders");
        assert!(html.contains("<label for=\"q\">Query</label>"), "{html}");
        assert!(html.contains("<input id=\"q\" name=\"q\""), "{html}");

        let button = node("button", vec![("label", entry("Run"))], Vec::new());
        assert_eq!(
            render(button).as_deref(),
            Some("<button type=\"submit\">Run</button>")
        );
    }

    #[test]
    fn renders_a_table_of_rows() {
        let row = node("text", vec![("value", entry("row"))], Vec::new());
        let table = node("table", Vec::new(), vec![row]);
        let html = render(table).expect("table renders");
        assert!(html.starts_with("<table><tbody>"), "{html}");
        assert!(html.ends_with("</tbody></table>"), "{html}");
    }

    #[test]
    // Specified: Orna-1.1.0 std.ui; a table node's records live in its `rows`
    // property, so they render as table rows and not only as child nodes.
    fn renders_table_rows_from_the_rows_property() {
        let row = ordered(vec![
            ("columns", OvbRaw::Array(Vec::new())),
            ("name", OvbRaw::Text("Note".into())),
            ("row_count_exact", OvbRaw::Bool(false)),
        ]);
        let table = node(
            "table",
            vec![(
                "rows",
                ordered(vec![
                    ("type", OvbRaw::Text("std.list<sys.Table>".into())),
                    ("value", OvbRaw::Array(vec![row])),
                ]),
            )],
            Vec::new(),
        );
        let html = render(table).expect("table renders");
        assert!(html.contains("<thead><tr>"), "{html}");
        assert!(html.contains("<th>name</th>"), "{html}");
        assert!(html.contains("<th>row_count_exact</th>"), "{html}");
        assert!(html.contains("<td>Note</td>"), "{html}");
        assert!(html.contains("<td>false</td>"), "{html}");
    }

    #[test]
    fn escapes_script_text() {
        let text = node(
            "text",
            vec![("value", entry("<script>alert(1)</script>"))],
            Vec::new(),
        );
        let html = render(text).expect("text renders");
        assert!(!html.contains("<script>"), "{html}");
        assert!(html.contains("&lt;script&gt;"), "{html}");
    }

    #[test]
    fn an_unknown_node_falls_back_to_inspect() {
        let unknown = node("sparkline", Vec::new(), Vec::new());
        assert!(render(unknown).is_none(), "unknown contracts use Inspect");
    }

    #[test]
    fn a_plain_value_is_not_html() {
        let value = CanonicalValue::new(OvbRaw::Text("plain".into())).expect("text is canonical");
        assert!(render_ui_html(&value).is_none());
    }
}
