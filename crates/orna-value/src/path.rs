//! Path component and collision-key encoding.

use crate::Result;
use crate::error::Error;

/// Repository key path component codec (ORNA-PATH-001 through 012).
pub fn path_encode_component(text: &str) -> Result<String> {
    if text.is_empty() {
        return Ok("~ff".into());
    }
    let raw = text.as_bytes();
    let reserved = reserved_name(text);
    let trailing = raw.iter().rposition(|b| *b != b'.').map_or(0, |x| x + 1);
    let mut out = String::new();
    for (i, b) in raw.iter().enumerate() {
        let force = (reserved && i == 0) || (*b == b'.' && i >= trailing);
        if !force && matches!(*b,b'A'..=b'Z'|b'a'..=b'z'|b'0'..=b'9'|b'.'|b'_'|b'-') {
            out.push(*b as char)
        } else {
            out.push_str(&format!("~{b:02x}"))
        }
    }
    if out.len() > 200 {
        return Err(Error::InvalidPath);
    }
    Ok(out)
}

pub fn path_decode_component(encoded: &str) -> Result<String> {
    if !encoded.is_ascii() || encoded.len() > 200 {
        return Err(Error::InvalidPath);
    }
    if encoded == "~ff" {
        return Ok(String::new());
    }
    if encoded.contains("~ff") {
        return Err(Error::InvalidPath);
    }
    let mut o = Vec::new();
    let b = encoded.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'~' {
            if i + 2 >= b.len() {
                return Err(Error::InvalidPath);
            }
            let h = std::str::from_utf8(&b[i + 1..i + 3]).map_err(|_| Error::InvalidPath)?;
            if !h
                .bytes()
                .all(|x| x.is_ascii_digit() || (b'a'..=b'f').contains(&x))
            {
                return Err(Error::InvalidPath);
            };
            o.push(u8::from_str_radix(h, 16).map_err(|_| Error::InvalidPath)?);
            i += 3
        } else {
            if !matches!(b[i],b'A'..=b'Z'|b'a'..=b'z'|b'0'..=b'9'|b'.'|b'_'|b'-') {
                return Err(Error::InvalidPath);
            };
            o.push(b[i]);
            i += 1
        }
    }
    let s = String::from_utf8(o).map_err(|_| Error::InvalidPath)?;
    if path_encode_component(&s)? != encoded {
        return Err(Error::NonCanonical);
    }
    Ok(s)
}

pub fn path_collision_key(encoded: &str) -> String {
    encoded
        .bytes()
        .map(|x| {
            if x.is_ascii_uppercase() {
                (x + 32) as char
            } else {
                x as char
            }
        })
        .collect()
}

/// Encodes a composite editable-row key.  Only the final component has the
/// `.orna` extension; the resulting repository-relative path is bounded.
pub fn path_encode_key_components(components: &[String]) -> Result<Vec<String>> {
    if components.is_empty() {
        return Err(Error::InvalidPath);
    }
    let mut output: Vec<String> = components
        .iter()
        .map(|component| path_encode_component(component))
        .collect::<Result<_>>()?;
    let last = output.last_mut().ok_or(Error::InvalidPath)?;
    last.push_str(".orna");
    if output.join("/").len() > 1024 {
        return Err(Error::InvalidPath);
    }
    Ok(output)
}

/// Decodes table-relative editable-row path components into canonical key text
/// in declared component order. The owning schema must still interpret that
/// text as typed keys. Filesystem containment and sibling collisions remain
/// the responsibility of the repository adapter.
pub fn path_decode_key_components(components: &[String]) -> Result<Vec<String>> {
    let (last, parents) = components.split_last().ok_or(Error::InvalidPath)?;
    let mut length = 0usize;
    for (index, component) in components.iter().enumerate() {
        length = length
            .checked_add(component.len())
            .and_then(|sum| sum.checked_add(usize::from(index != 0)))
            .filter(|sum| *sum <= 1024)
            .ok_or(Error::InvalidPath)?;
    }
    path_validate_relative_components(components)?;
    let mut decoded = parents
        .iter()
        .map(|component| path_decode_component(component))
        .collect::<Result<Vec<_>>>()?;
    decoded.push(path_decode_component(
        last.strip_suffix(".orna").ok_or(Error::InvalidPath)?,
    )?);
    Ok(decoded)
}

/// Rejects path traversal syntax before a repository layer performs a
/// symlink/reparse-point-aware containment check at materialisation time.
pub fn path_validate_relative_components(components: &[String]) -> Result<()> {
    if components.is_empty()
        || components.iter().any(|component| {
            component.is_empty()
                || component == "."
                || component == ".."
                || component.contains('/')
                || component.contains('\\')
        })
    {
        return Err(Error::InvalidPath);
    }
    if components.join("/").len() > 1024 {
        return Err(Error::InvalidPath);
    }
    Ok(())
}

fn reserved_name(s: &str) -> bool {
    if s == "." || s == ".." {
        return true;
    }
    let t = s.trim_end_matches([' ', '.']);
    let lower = t.to_ascii_lowercase();
    if lower == ".git" {
        return true;
    }
    let first = lower.split('.').next().unwrap_or("");
    matches!(
        first,
        "con" | "prn" | "aux" | "nul" | "clock$" | "conin$" | "conout$"
    ) || (first.len() == 4
        && ((first.starts_with("com") || first.starts_with("lpt"))
            && matches!(first.as_bytes()[3], b'1'..=b'9')))
}
