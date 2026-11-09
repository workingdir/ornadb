//! Deterministic `std_intrinsics.json` export.
//!
//! The renderer uses no external serializer so the checked-in artifact and the
//! in-memory catalogue cannot drift through a dependency's formatting choice.

use std::fmt::Write;

use crate::dispatch::Dispatch;
use crate::modules::{Intrinsic, REEXPORTS, catalogue};
use crate::signature::{Param, Signature};

/// Renders the complete catalogue as the checked-in JSON artifact.
pub fn json() -> String {
    let mut out = String::new();
    out.push_str("{\n");
    out.push_str("  \"schema\": \"orna.std_intrinsics.v1\",\n");
    let _ = writeln!(
        out,
        "  \"source_digest\": \"{}\",",
        crate::digest::source_digest()
    );
    out.push_str("  \"intrinsics\": [\n");
    for (index, intrinsic) in catalogue().iter().enumerate() {
        if index != 0 {
            out.push_str(",\n");
        }
        render_intrinsic(&mut out, intrinsic);
    }
    out.push_str("\n  ],\n");
    out.push_str("  \"reexports\": [\n");
    for (index, (module, name, id)) in REEXPORTS.iter().enumerate() {
        if index != 0 {
            out.push_str(",\n");
        }
        let _ = write!(
            out,
            "    {{ \"module\": \"{}\", \"name\": \"{}\", \"id\": \"{}\" }}",
            escape(module.name()),
            escape(name),
            id.name()
        );
    }
    out.push_str("\n  ]\n}\n");
    out
}

fn render_intrinsic(out: &mut String, intrinsic: &Intrinsic) {
    out.push_str("    {\n");
    let _ = writeln!(out, "      \"id\": \"{}\",", intrinsic.id.name());
    let _ = writeln!(out, "      \"id_value\": {},", intrinsic.id.as_u16());
    let _ = writeln!(
        out,
        "      \"module\": \"{}\",",
        escape(intrinsic.module.name())
    );
    let _ = writeln!(out, "      \"name\": \"{}\",", escape(intrinsic.name));
    match intrinsic.leaf {
        Some(leaf) => {
            let _ = writeln!(out, "      \"leaf\": \"{}\",", escape(leaf));
        }
        None => out.push_str("      \"leaf\": null,\n"),
    }
    out.push_str("      \"signature\": ");
    render_signature(out, &intrinsic.signature);
    out.push_str(",\n");
    let _ = writeln!(out, "      \"effect\": \"{}\",", intrinsic.effect.as_str());
    out.push_str("      \"dispatch\": ");
    render_dispatch(out, intrinsic.dispatch);
    out.push_str(",\n");
    let _ = writeln!(
        out,
        "      \"justification\": \"{}\",",
        intrinsic.justification.as_str()
    );
    let _ = writeln!(out, "      \"note\": \"{}\",", escape(intrinsic.note));
    out.push_str("      \"pins\": {");
    let _ = write!(
        out,
        " \"since\": \"{}\", \"until\": ",
        escape(intrinsic.pins.since)
    );
    match intrinsic.pins.until {
        Some(until) => {
            let _ = write!(out, "\"{}\"", escape(until));
        }
        None => out.push_str("null"),
    }
    out.push_str(" }\n    }");
}

fn render_signature(out: &mut String, signature: &Signature) {
    out.push_str("{\n        \"type_params\": [");
    for (index, name) in signature.type_params.iter().enumerate() {
        if index != 0 {
            out.push_str(", ");
        }
        let _ = write!(out, "\"{}\"", escape(name));
    }
    out.push_str("],\n        \"params\": [");
    for (index, param) in signature.params.iter().enumerate() {
        if index != 0 {
            out.push_str(", ");
        }
        render_param(out, param);
    }
    out.push_str("],\n        \"result\": ");
    let _ = write!(out, "\"{}\"", escape(&signature.result.render()));
    out.push_str(",\n        \"arities\": [");
    for (index, arity) in signature.arities.iter().enumerate() {
        if index != 0 {
            out.push_str(", ");
        }
        let _ = write!(out, "{arity}");
    }
    out.push_str("]\n      }");
}

fn render_param(out: &mut String, param: &Param) {
    let _ = write!(
        out,
        "{{ \"name\": \"{}\", \"type\": \"{}\", \"has_default\": {}, \"pipeline\": {} }}",
        escape(param.name),
        escape(&param.ty.render()),
        param.has_default,
        param.pipeline
    );
}

fn render_dispatch(out: &mut String, dispatch: Dispatch) {
    match dispatch {
        Dispatch::Native => out.push_str("{ \"kind\": \"Native\" }"),
        Dispatch::SourceOnly => out.push_str("{ \"kind\": \"SourceOnly\" }"),
        Dispatch::Host { operation } => {
            let _ = write!(
                out,
                "{{ \"kind\": \"Host\", \"operation\": \"{}\" }}",
                escape(operation)
            );
        }
    }
}

fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if character.is_control() => {
                let _ = write!(escaped, "\\u{:04x}", u32::from(character));
            }
            character => escaped.push(character),
        }
    }
    escaped
}
