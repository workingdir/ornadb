//! Bounded, redacted rendering of evaluator values for the interactive console.
//!
//! [`DISPLAY`] bounds a result line; [`FULL`] bounds the `:inspect` view. Both
//! bounds redact protected values identically; they differ only in how much
//! structure and text they show.

use std::fmt::Write as _;

use orna_sys_id::SysType;
use orna_value::{CanonicalValue, ContextValue, Raw, canonical_uuid_text};

/// How much of a value the console renders.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Bounds {
    /// Nesting depth at which a value renders as `…`.
    depth: usize,
    /// Items shown per array or map before `…`.
    items: usize,
    /// Text bytes shown per string, byte sequence or name before `…`.
    text: usize,
}

/// The bounds of a result line.
pub(crate) const DISPLAY: Bounds = Bounds {
    depth: 4,
    items: 16,
    text: 256,
};

/// The bounds of `:inspect`, which shows more of the same value.
pub(crate) const FULL: Bounds = Bounds {
    depth: 12,
    items: 256,
    text: 4096,
};

pub fn inspect(value: &CanonicalValue) -> String {
    inspect_text(value.raw(), DISPLAY)
}

/// Full inspect text of a value, for `:inspect`.
pub(crate) fn inspect_full(value: &CanonicalValue) -> String {
    inspect_text(value.raw(), FULL)
}

/// Inspect text of a contextual (ROV-3) value, for a page served without a
/// portable evaluator value.
pub fn inspect_context(value: &ContextValue) -> String {
    inspect_text(value.raw(), DISPLAY)
}

fn inspect_text(raw: &Raw, bounds: Bounds) -> String {
    let (text, ty) = render(raw, 0, bounds);
    format!("{text} : {ty}")
}

/// Renders `raw` nested at `depth`, bounded by `bounds`. Returns the text and
/// the type name.
pub(crate) fn render(raw: &Raw, depth: usize, bounds: Bounds) -> (String, &'static str) {
    if depth >= bounds.depth {
        return ("…".into(), "Value");
    }
    match raw {
        Raw::Null => ("null".into(), "Null"),
        Raw::Bool(value) => (value.to_string(), "Bool"),
        Raw::Int(value) => (truncate(&value.to_string(), bounds), "Int"),
        Raw::Float(bits) => (format_float(*bits), "Float"),
        Raw::Bytes(bytes) => (inspect_bytes(bytes, bounds), "Bytes"),
        Raw::Text(value) => (format!("\"{}\"", escape(value, bounds)), "Str"),
        Raw::Array(values) => (inspect_sequence(values, depth, bounds), "Array"),
        Raw::Map(entries) => (inspect_map(entries, depth, bounds), "Map"),
        // OVB tag 0 is the protected-value boundary. Discard the whole inner
        // subtree before rendering so even nested inspect fallbacks cannot
        // turn a secret payload into printable structure.
        Raw::Tag(0, _) => ("<redacted>".into(), "Secret"),
        // This generic display has no field-level disclosure proof. Diagnostic
        // text, Error payloads, renderer properties, and typed sys.Value data
        // therefore remain opaque here; callers needing a privileged view must
        // use a separately authorized projection.
        Raw::Tag(60011, _) => ("<redacted>".into(), "Diagnostic"),
        Raw::Tag(60012, _) => ("<redacted>".into(), "Present"),
        Raw::Tag(60016, _) => ("<redacted>".into(), "Error"),
        Raw::Tag(60026, _) => ("<redacted>".into(), SysType::Value.name()),
        // OVB tag 37 is the closed 1.0 UUID representation. Keep its
        // structural fallback human-readable without turning presentation
        // into an alternate persistence encoding.
        Raw::Tag(37, value) => inspect_uuid(value),
        Raw::Tag(tag, value) => {
            if let Some(shown) = fixed_point_display(*tag, value) {
                return shown;
            }
            let (text, _) = render(value, depth + 1, bounds);
            match registered_tag_name(*tag) {
                Some(name) => (format!("{name}({text})"), name),
                None => (format!("Tag<{tag}>({text})"), "Tagged"),
            }
        }
    }
}

fn inspect_uuid(value: &Raw) -> (String, &'static str) {
    let Raw::Bytes(bytes) = value else {
        return ("Uuid(<invalid>)".into(), "Uuid");
    };
    let Ok(bytes) = <[u8; 16]>::try_from(bytes.as_slice()) else {
        return ("Uuid(<invalid>)".into(), "Uuid");
    };
    (canonical_uuid_text(bytes), "Uuid")
}

/// The plain body of a Decimal (`60000`) or Money (`60007`) payload: the
/// amount only. Money's currency is a UUID witness that the value never reduces
/// to a code, so its `Money<CODE>` label comes from the static type. Any other
/// payload shape keeps the structural fallback.
fn fixed_point_display(tag: u64, value: &Raw) -> Option<(String, &'static str)> {
    match tag {
        60000 => Some((decimal_payload_text(value)?, "Decimal")),
        60007 => {
            let Raw::Array(parts) = value else {
                return None;
            };
            let [Raw::Tag(60000, amount), _] = parts.as_slice() else {
                return None;
            };
            Some((decimal_payload_text(amount)?, "Money"))
        }
        _ => None,
    }
}

/// Plain decimal text of a `[coefficient, exponent]` payload, such as `3.1415`
/// for `[31415, -4]`.
fn decimal_payload_text(value: &Raw) -> Option<String> {
    let Raw::Array(parts) = value else {
        return None;
    };
    let [Raw::Int(coefficient), Raw::Int(exponent)] = parts.as_slice() else {
        return None;
    };
    plain_decimal(&coefficient.to_string(), &exponent.to_string())
}

/// Renders `coefficient * 10^exponent` in positional form, trimming trailing
/// fraction zeroes. It works on the decimal digit strings, so it stays exact at
/// any magnitude the canonical payload admits.
fn plain_decimal(coefficient: &str, exponent: &str) -> Option<String> {
    let exponent: i64 = exponent.parse().ok()?;
    let (sign, digits) = match coefficient.strip_prefix('-') {
        Some(digits) => ("-", digits),
        None => ("", coefficient),
    };
    if digits == "0" {
        return Some("0".to_owned());
    }
    if exponent >= 0 {
        let zeros = "0".repeat(usize::try_from(exponent).ok()?);
        return Some(format!("{sign}{digits}{zeros}"));
    }
    let places = usize::try_from(exponent.checked_neg()?).ok()?;
    let padded = if digits.len() <= places {
        format!("{}{digits}", "0".repeat(places - digits.len() + 1))
    } else {
        digits.to_owned()
    };
    let (whole, fraction) = padded.split_at(padded.len() - places);
    let fraction = fraction.trim_end_matches('0');
    if fraction.is_empty() {
        Some(format!("{sign}{whole}"))
    } else {
        Some(format!("{sign}{whole}.{fraction}"))
    }
}

/// Type name of each OVB 1.1 registered tag (60000..=60027). Such a value is
/// shown by its type name, never as `Tag<n>`.
fn registered_tag_name(tag: u64) -> Option<&'static str> {
    Some(match tag {
        60000 => "Decimal",
        60001 => "Date",
        60002 => "Instant",
        60003 => "LocalDateTime",
        60004 => "TimeZone",
        60005 => "Duration",
        60006 => "Quantity",
        60007 => "Money",
        60008 => "Enum",
        60009 => "Record",
        60010 => "RowRef",
        60011 => "Diagnostic",
        60012 => "Present",
        60013 => "Option",
        60014 => "Unit",
        60015 => "Tuple",
        60016 => "Error",
        60017 => "TimeOfDay",
        60018 => "ZonedDateTime",
        60019 => "Range",
        60020 => "Refined",
        60021 => "TableRef",
        60022 => "LiveHandle",
        60023 => "SystemId",
        60024 => "Path",
        60025 => "Digest",
        60026 => SysType::Value.name(),
        60027 => "Relation",
        _ => return None,
    })
}

fn inspect_sequence(values: &[Raw], depth: usize, bounds: Bounds) -> String {
    let mut items = values
        .iter()
        .take(bounds.items)
        .map(|value| render(value, depth + 1, bounds).0)
        .collect::<Vec<_>>();
    if values.len() > bounds.items {
        items.push("…".into());
    }
    format!("[{}]", items.join(", "))
}

fn inspect_map(entries: &[(Raw, Raw)], depth: usize, bounds: Bounds) -> String {
    let mut items = entries
        .iter()
        .take(bounds.items)
        .map(|(key, value)| {
            let key = render(key, depth + 1, bounds).0;
            let value = render(value, depth + 1, bounds).0;
            format!("{key}: {value}")
        })
        .collect::<Vec<_>>();
    if entries.len() > bounds.items {
        items.push("…".into());
    }
    format!("{{{}}}", items.join(", "))
}

fn inspect_bytes(bytes: &[u8], bounds: Bounds) -> String {
    let mut text = String::new();
    for byte in bytes.iter().take(bounds.text / 2) {
        write!(&mut text, "{byte:02x}").expect("writing to String is infallible");
    }
    if bytes.len() > bounds.text / 2 {
        text.push('…');
    }
    format!("0x{text}")
}

fn format_float(bits: u64) -> String {
    let value = f64::from_bits(bits);
    if value.is_nan() {
        "NaN".into()
    } else if value.is_infinite() {
        if value.is_sign_negative() {
            "-Infinity".into()
        } else {
            "Infinity".into()
        }
    } else {
        value.to_string()
    }
}

fn escape(value: &str, bounds: Bounds) -> String {
    truncate(&value.escape_default().to_string(), bounds)
}

fn truncate(value: &str, bounds: Bounds) -> String {
    let mut end = value.len().min(bounds.text);
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    let mut text = value[..end].to_owned();
    if end < value.len() {
        text.push('…');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inspect_is_bounded_and_redacts_protected_values() {
        let value = CanonicalValue::protected();
        assert_eq!(inspect(&value), "<redacted> : Secret");
        assert_eq!(
            truncate(&"x".repeat(DISPLAY.text + 1), DISPLAY),
            format!("{}…", "x".repeat(DISPLAY.text))
        );
    }

    #[test]
    fn inspect_redacts_payload_bearing_protected_values_inside_collections() {
        let fixture = include_str!("../tests/fixtures/secret-surface.orna").trim();
        let raw = Raw::Map(vec![
            (
                Raw::Text("credential".into()),
                Raw::Array(vec![Raw::Tag(0, Box::new(Raw::Text(fixture.to_owned())))]),
            ),
            (
                Raw::Tag(0, Box::new(Raw::Text(fixture.to_owned()))),
                Raw::Text("keyed payload".into()),
            ),
        ]);

        let rendered = render(&raw, 0, DISPLAY).0;
        assert!(rendered.contains("<redacted>"), "{rendered}");
        assert!(!rendered.contains(fixture), "{rendered}");
    }

    #[test]
    fn inspect_redacts_payloads_in_diagnostic_present_error_and_sys_value_tags() {
        let fixture = include_str!("../tests/fixtures/secret-surface.orna").trim();
        let tagged = |number, payload| Raw::Tag(number, Box::new(payload));
        let diagnostic = tagged(
            60011,
            Raw::Map(vec![
                (Raw::Int(0.into()), Raw::Text("ORNA-E-TEST".into())),
                (Raw::Int(1.into()), Raw::Int(3.into())),
                (Raw::Int(2.into()), Raw::Text(fixture.into())),
                (Raw::Int(3.into()), Raw::Array(vec![])),
                (
                    Raw::Int(4.into()),
                    Raw::Array(vec![Raw::Text(fixture.into())]),
                ),
                (Raw::Int(5.into()), Raw::Array(vec![])),
                (Raw::Int(6.into()), Raw::Bool(false)),
            ]),
        );
        let present = tagged(
            60012,
            Raw::Array(vec![
                Raw::Text("value".into()),
                Raw::Null,
                Raw::Map(vec![(
                    Raw::Text("credential".into()),
                    Raw::Text(fixture.into()),
                )]),
                Raw::Array(vec![]),
            ]),
        );
        let error = tagged(
            60016,
            Raw::Map(vec![
                (Raw::Int(0.into()), Raw::Text("ORNA-E-TEST".into())),
                (Raw::Int(1.into()), Raw::Text(fixture.into())),
                (Raw::Int(2.into()), Raw::Array(vec![])),
                (
                    Raw::Int(3.into()),
                    Raw::Map(vec![(
                        Raw::Text("credential".into()),
                        Raw::Text(fixture.into()),
                    )]),
                ),
            ]),
        );
        let sys_value = tagged(
            60026,
            Raw::Array(vec![
                Raw::Array(vec![Raw::Int(0.into()), Raw::Text("Str".into())]),
                Raw::Text(fixture.into()),
            ]),
        );

        for raw in [diagnostic, present, error, sys_value] {
            let value = CanonicalValue::new(raw).expect("fixture wrapper is canonical");
            let display = inspect(&value);
            assert!(display.starts_with("<redacted> : "), "{display}");
            assert!(!display.contains(fixture), "{display}");
        }
    }

    #[test]
    fn composed_debug_inspect_and_trace_projections_keep_wrapper_tails_opaque() {
        let fixture = include_str!("../tests/fixtures/secret-surface.orna").trim();
        let tagged = |number, payload| Raw::Tag(number, Box::new(payload));
        let diagnostic = tagged(
            60011,
            Raw::Map(vec![
                (Raw::Int(0.into()), Raw::Text(fixture.into())),
                (Raw::Int(1.into()), Raw::Int(3.into())),
                (Raw::Int(2.into()), Raw::Text(fixture.into())),
                (Raw::Int(3.into()), Raw::Array(vec![])),
                (
                    Raw::Int(4.into()),
                    Raw::Array(vec![Raw::Text(fixture.into())]),
                ),
                (Raw::Int(5.into()), Raw::Array(vec![])),
                (Raw::Int(6.into()), Raw::Bool(false)),
            ]),
        );
        let sys_value = tagged(
            60026,
            Raw::Array(vec![
                Raw::Array(vec![Raw::Int(0.into()), Raw::Text("Str".into())]),
                Raw::Text(fixture.into()),
            ]),
        );
        let present = tagged(
            60012,
            Raw::Array(vec![
                Raw::Text("value".into()),
                Raw::Null,
                Raw::Map(vec![(
                    Raw::Text("diagnostic".into()),
                    Raw::Array(vec![diagnostic.clone(), sys_value.clone()]),
                )]),
                Raw::Array(vec![]),
            ]),
        );
        let error = tagged(
            60016,
            Raw::Map(vec![
                (Raw::Int(0.into()), Raw::Text("ORNA-E-TEST".into())),
                (Raw::Int(1.into()), Raw::Text(fixture.into())),
                (Raw::Int(2.into()), Raw::Array(vec![])),
                (
                    Raw::Int(3.into()),
                    Raw::Map(vec![(
                        Raw::Text("credential".into()),
                        Raw::Text(fixture.into()),
                    )]),
                ),
            ]),
        );
        let raw = Raw::Array(vec![diagnostic, present, error, sys_value]);
        let value = CanonicalValue::new(raw.clone()).expect("composed wrappers are canonical");

        let debug = format!("{raw:?} {value:?}");
        let display = inspect(&value);
        let trace = value.redacted_for_trace().unwrap().encode().unwrap();
        assert!(!debug.contains(fixture), "{debug}");
        assert!(!display.contains(fixture), "{display}");
        assert!(
            !trace
                .windows(fixture.len())
                .any(|part| part == fixture.as_bytes())
        );
    }

    #[test]
    fn inspect_renders_canonical_uuid_text_without_affecting_ovb() {
        let value = CanonicalValue::uuid([
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff,
        ]);
        assert_eq!(
            inspect(&value),
            "00112233-4455-6677-8899-aabbccddeeff : Uuid"
        );
        assert_eq!(
            value.encode().expect("canonical UUID OVB"),
            vec![
                0xd8, 0x25, 0x50, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa,
                0xbb, 0xcc, 0xdd, 0xee, 0xff
            ]
        );
    }

    #[test]
    fn inspect_names_registered_tags_instead_of_tag_numbers() {
        let decimal = Raw::Tag(
            60000,
            Box::new(Raw::Array(vec![
                Raw::Int(125.into()),
                Raw::Int((-2).into()),
            ])),
        );
        let date = Raw::Tag(60001, Box::new(Raw::Text("2026-11-07".into())));
        for (raw, expected) in [
            (decimal, "1.25 : Decimal"),
            (date, "Date(\"2026-11-07\") : Date"),
        ] {
            let value = CanonicalValue::new(raw).expect("registered tag is canonical");
            assert_eq!(inspect(&value), expected);
        }
    }

    #[test]
    fn decimal_body_is_its_plain_amount() {
        for (coefficient, exponent, expected) in [
            (31415, -4, "3.1415"),
            (-125, -2, "-1.25"),
            (5, -3, "0.005"),
            (12, 3, "12000"),
        ] {
            let raw = Raw::Tag(
                60000,
                Box::new(Raw::Array(vec![
                    Raw::Int(coefficient.into()),
                    Raw::Int(exponent.into()),
                ])),
            );
            let value = CanonicalValue::new(raw).expect("registered tag is canonical");
            assert_eq!(inspect(&value), format!("{expected} : Decimal"));
        }
    }

    #[test]
    fn every_registered_tag_is_shown_by_its_type_name() {
        // Every OVB 1.1 registered tag 60000..=60027, in tag order. Tags
        // 60011, 60012, 60016 and 60026 are redacted before the name table
        // is consulted, so they show their redacted type name instead.
        let expected: [(u64, &str); 28] = [
            (60000, "Decimal(0) : Decimal"),
            (60001, "Date(0) : Date"),
            (60002, "Instant(0) : Instant"),
            (60003, "LocalDateTime(0) : LocalDateTime"),
            (60004, "TimeZone(0) : TimeZone"),
            (60005, "Duration(0) : Duration"),
            (60006, "Quantity(0) : Quantity"),
            (60007, "Money(0) : Money"),
            (60008, "Enum(0) : Enum"),
            (60009, "Record(0) : Record"),
            (60010, "RowRef(0) : RowRef"),
            (60011, "<redacted> : Diagnostic"),
            (60012, "<redacted> : Present"),
            (60013, "Option(0) : Option"),
            (60014, "Unit(0) : Unit"),
            (60015, "Tuple(0) : Tuple"),
            (60016, "<redacted> : Error"),
            (60017, "TimeOfDay(0) : TimeOfDay"),
            (60018, "ZonedDateTime(0) : ZonedDateTime"),
            (60019, "Range(0) : Range"),
            (60020, "Refined(0) : Refined"),
            (60021, "TableRef(0) : TableRef"),
            (60022, "LiveHandle(0) : LiveHandle"),
            (60023, "SystemId(0) : SystemId"),
            (60024, "Path(0) : Path"),
            (60025, "Digest(0) : Digest"),
            (60026, "<redacted> : sys.Value"),
            (60027, "Relation(0) : Relation"),
        ];
        for (index, (tag, expected_text)) in expected.iter().enumerate() {
            assert_eq!(
                *tag,
                60000 + index as u64,
                "table covers the registered range"
            );
            // `inspect` formats `render` output as `{text} : {type}`. The
            // canonical constructor refuses these ill-shaped payloads, so the
            // table calls `render` directly; that is the same renderer.
            let (text, ty) = render(&Raw::Tag(*tag, Box::new(Raw::Int(0.into()))), 0, DISPLAY);
            let text = format!("{text} : {ty}");
            assert_eq!(text, *expected_text, "tag {tag}");
            assert!(!text.contains("Tag<"), "tag {tag} must not show its number");
        }
    }

    #[test]
    fn invalid_uuid_under_tag_37_is_named_uuid_not_a_tag_number() {
        let invalid = [
            Raw::Int(7.into()),
            Raw::Bytes(vec![0; 15]),
            Raw::Bytes(vec![0; 17]),
        ];
        // The canonical constructor refuses these payloads, so the test calls
        // `render` (the renderer `inspect` wraps) directly.
        for payload in invalid {
            let (text, ty) = render(&Raw::Tag(37, Box::new(payload)), 0, DISPLAY);
            assert_eq!(format!("{text} : {ty}"), "Uuid(<invalid>) : Uuid");
        }
        let (text, ty) = render(&Raw::Tag(37, Box::new(Raw::Bytes(vec![0; 16]))), 0, DISPLAY);
        assert_eq!(
            format!("{text} : {ty}"),
            "00000000-0000-0000-0000-000000000000 : Uuid"
        );
    }
}
