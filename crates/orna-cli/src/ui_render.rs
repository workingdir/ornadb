//! Renders `std.ui` presentation nodes as semantic HTML.
//!
//! The evaluator builds renderer-neutral node records
//! (`crates/orna-evaluator/src/ui.rs`); this module turns them into native
//! controls that stay usable without any stylesheet. A value that is not a
//! recognised node is left to the Inspect fallback, and every text value is
//! escaped so a stored string cannot introduce markup.

use std::fmt::Write as _;

use orna_foundation::OvbRaw;
use orna_value::{ContextValue, ROV3_BLOB_TAG};

/// Renders a value read through a Format-3 context as HTML when it is a
/// `std.ui` node, or `None` so the caller can fall back to Inspect.
/// `media_commit` is the HEAD commit that Blob nodes link to through
/// `/media/<commit>/<descriptor>`; without it a Blob node has no route and the
/// whole value falls back to Inspect.
pub fn render_ui_html(value: &ContextValue, media_commit: Option<&str>) -> Option<String> {
    render_value_html(value.raw(), media_commit)
}

/// Renders a canonical (OVB-1) value's raw form the same way. A canonical value
/// cannot hold a Blob node, so the serve path renders it without copying it
/// into a context.
pub fn render_value_html(raw: &OvbRaw, media_commit: Option<&str>) -> Option<String> {
    let mut html = String::new();
    render_node(raw, &mut html, 0, media_commit)?;
    Some(html)
}

const MAX_DEPTH: usize = 32;

fn render_node(raw: &OvbRaw, out: &mut String, depth: usize, media: Option<&str>) -> Option<()> {
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
                render_node(child, out, depth + 1, media)?;
            }
            out.push_str("</div>");
        }
        "details" => {
            out.push_str("<dl>");
            for child in children {
                render_node(child, out, depth + 1, media)?;
            }
            out.push_str("</dl>");
        }
        "table" => render_table_node(properties, children, out, depth, media)?,
        "image" => {
            let href = media_href(properties, "blob", media)?;
            out.push_str("<img src=\"");
            push_escaped(&href, out);
            out.push_str("\" alt=\"");
            push_value(properties, "alt", out)?;
            out.push_str("\">");
        }
        "audio" => {
            let href = media_href(properties, "blob", media)?;
            out.push_str("<audio controls src=\"");
            push_escaped(&href, out);
            out.push_str("\"></audio>");
        }
        "download" => {
            let href = media_href(properties, "blob", media)?;
            out.push_str("<a download href=\"");
            push_escaped(&href, out);
            out.push_str("\">");
            push_value(properties, "label", out)?;
            out.push_str("</a>");
        }
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
                render_node(child, out, depth + 1, media)?;
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

/// The `/media` route of a Blob property: `/media/<commit>/<descriptor>` at
/// HEAD. The node carries only the committed ROV-3 reference, so its descriptor
/// names the bytes and they are never inlined. A value that is not such a
/// reference, or a page without a HEAD commit, has no media route and returns
/// `None`, so the caller falls back to Inspect.
fn media_href(properties: &[(OvbRaw, OvbRaw)], name: &str, media: Option<&str>) -> Option<String> {
    let commit = media?;
    let entry = as_map(field(properties, name)?)?;
    let OvbRaw::Tag(ROV3_BLOB_TAG, payload) = field(entry, "value")? else {
        return None;
    };
    let [_, _, _, _, OvbRaw::Bytes(descriptor)] = as_list(payload)? else {
        return None;
    };
    let mut hex = String::with_capacity(descriptor.len() * 2);
    for byte in descriptor {
        // Writing into a `String` cannot fail.
        let _ = write!(hex, "{byte:02x}");
    }
    Some(format!("/media/{commit}/{hex}"))
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
    media: Option<&str>,
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
        render_node(child, out, depth + 1, media)?;
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
    use orna_foundation::CanonicalValue;
    use orna_value::{
        AvailabilityState, BlobResolver, ContentReference, Format3Context, Format3Quota, GitHash,
        NativeOid, OwnerLifetime, Snapshot, ValueFormat,
    };
    use std::sync::Arc;

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
        render_value_html(value.raw(), None)
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
        assert!(render_value_html(value.raw(), None).is_none());
    }

    /// A Blob property as ROV-3 stores a committed reference: `{ type, value }`
    /// whose value is `[length, sha256, media, suffix, descriptor]`.
    fn blob_property(descriptor: [u8; 20]) -> OvbRaw {
        ordered(vec![
            ("type", OvbRaw::Text("std.binary_large_object".into())),
            (
                "value",
                OvbRaw::Tag(
                    ROV3_BLOB_TAG,
                    Box::new(OvbRaw::Array(vec![
                        OvbRaw::Int(3.into()),
                        OvbRaw::Bytes(vec![7; 32]),
                        OvbRaw::Text("image/png".into()),
                        OvbRaw::Null,
                        OvbRaw::Bytes(descriptor.to_vec()),
                    ])),
                ),
            ),
        ])
    }

    /// Synthetic Format-3 context. The resolver is never read: rendering needs
    /// only the committed reference, not the blob bytes.
    struct NoBytes;

    impl BlobResolver for NoBytes {
        fn read_range(
            &self,
            _reference: &ContentReference,
            _offset: u64,
            _length: u64,
        ) -> orna_value::Result<Vec<u8>> {
            Err(orna_value::Error::MissingDescriptor)
        }
    }

    fn format3_context() -> Format3Context {
        let database = [1; 16];
        Format3Context::from_persisted(
            database,
            Snapshot::cwd(database, [2; 16], 0.into()).expect("snapshot"),
            GitHash::Sha1,
            NativeOid::from_bytes(&[9; 20]).expect("schema root"),
            NativeOid::from_bytes(&[8; 20]).expect("store root"),
            AvailabilityState::Available,
            OwnerLifetime::new([3; 16]),
            Format3Quota::new(8 * 1024 * 1024, 1_u64 << 62).expect("quota"),
            Arc::new(NoBytes),
        )
        .expect("context")
    }

    /// A blob-bearing node is a ROV-3 value, so it is built inside a context.
    fn render_at(raw: OvbRaw, commit: Option<&str>) -> Option<String> {
        let value = ContextValue::new_in_context(&format3_context(), ValueFormat::Rov3, raw)
            .expect("blob node is a valid ROV-3 value");
        render_ui_html(&value, commit)
    }

    #[test]
    // Specified: Orna-1.1.0 std.ui Image, Audio and Download link a committed
    // blob through the explicit media route and never inline its bytes.
    fn blob_nodes_link_the_media_route_without_inlining_bytes() {
        let commit = "0123456789abcdef0123456789abcdef01234567";
        let hex = "ab".repeat(20);
        let href = format!("/media/{commit}/{hex}");

        let image = node(
            "image",
            vec![("blob", blob_property([0xab; 20])), ("alt", entry("Cover"))],
            Vec::new(),
        );
        let html = render_at(image, Some(commit)).expect("image renders");
        assert!(
            html.starts_with(&format!("<img src=\"{href}\" alt=\"")),
            "{html}"
        );
        assert!(
            !html.contains("base64") && !html.contains("data:"),
            "{html}"
        );

        let audio = node(
            "audio",
            vec![("blob", blob_property([0xab; 20]))],
            Vec::new(),
        );
        assert_eq!(
            render_at(audio, Some(commit)).as_deref(),
            Some(format!("<audio controls src=\"{href}\"></audio>").as_str())
        );

        let download = node(
            "download",
            vec![
                ("blob", blob_property([0xab; 20])),
                ("label", entry("Save")),
            ],
            Vec::new(),
        );
        assert_eq!(
            render_at(download, Some(commit)).as_deref(),
            Some(format!("<a download href=\"{href}\">Save</a>").as_str())
        );
    }

    #[test]
    // Specified: std.ui Image takes a required `alt: Str` (stdlib/std/ui.orna),
    // so an image without alt is not a contract node and falls back to Inspect.
    // Alt text is escaped as an attribute value, so it cannot add an attribute.
    fn image_alt_is_required_and_escaped() {
        let commit = "0123456789abcdef0123456789abcdef01234567";
        let without_alt = node(
            "image",
            vec![("blob", blob_property([0xab; 20]))],
            Vec::new(),
        );
        assert_eq!(render_at(without_alt, Some(commit)), None);

        let hostile = node(
            "image",
            vec![
                ("blob", blob_property([0xab; 20])),
                ("alt", entry("x\" onerror=\"alert(1)")),
            ],
            Vec::new(),
        );
        let html = render_at(hostile, Some(commit)).expect("image renders");
        assert!(
            html.ends_with("alt=\"x&quot; onerror=&quot;alert(1)\">"),
            "{html}"
        );
        assert!(!html.contains("\" onerror=\""), "{html}");
    }

    #[test]
    // Specified: a Blob node without a HEAD commit or without a committed
    // reference has no media route, so the whole value falls back to Inspect.
    fn blob_nodes_without_a_route_fall_back_to_inspect() {
        let audio = || {
            node(
                "audio",
                vec![("blob", blob_property([0xab; 20]))],
                Vec::new(),
            )
        };
        assert!(render_at(audio(), None).is_none());

        let not_a_reference = node(
            "audio",
            vec![(
                "blob",
                ordered(vec![
                    ("type", OvbRaw::Text("std.binary_large_object".into())),
                    ("value", OvbRaw::Bytes(vec![1, 2, 3])),
                ]),
            )],
            Vec::new(),
        );
        let value = CanonicalValue::new(not_a_reference).expect("node is canonical");
        assert!(render_value_html(value.raw(), Some("abc")).is_none());
    }
}
