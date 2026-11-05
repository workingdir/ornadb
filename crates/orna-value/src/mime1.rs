//! The pinned MIME-1 media registry (Orna 1.1.0 `profiles/media-1.json`).
//!
//! MIME and filename code reads this one registry: the aliases, the entries,
//! the bounds and the unique-suffix reverse index derived from the entries.
//! The tables transcribe the profile copy pinned at
//! `src/fixtures/reference/profiles/media-1.json` (SHA-256
//! `7b232d68b7e4a6a2e9c225339639165569896b7ee8e1b8bd06e3331af2f73120`, the
//! release `SHA256SUMS` value), and the parity tests below fail if the copy or
//! a table drifts. Hint rules follow the profile's normative model,
//! `tools/final_model.py` `annotation()`. Changing the table requires a new
//! profile, not an edit here.

use crate::{Error, Result};

/// `profile`: the registry name a format context selects.
pub(crate) const PROFILE: &str = "MIME-1";
/// `maximum_media_type_bytes`.
pub(crate) const MAX_MEDIA_TYPE_BYTES: usize = 1024;
/// `maximum_parameters`.
pub(crate) const MAX_PARAMETERS: usize = 32;
/// `maximum_suffix_bytes`.
pub(crate) const MAX_SUFFIX_BYTES: usize = 32;
/// `unknown_preferred`: the suffix of an essence outside `entries`.
pub(crate) const UNKNOWN_PREFERRED: &str = "bin";
/// The declaration of a constructor without knowledge (ORNA-MIME-001). Its
/// entry lists only `bin`, but like an unknown essence it admits any safe hint.
pub(crate) const OCTET_STREAM: &str = "application/octet-stream";

/// `aliases`: an alias essence and the canonical essence it normalizes to.
const ALIASES: &[(&str, &str)] = &[
    ("application/javascript", "text/javascript"),
    ("application/ecmascript", "text/javascript"),
    ("text/ecmascript", "text/javascript"),
    ("audio/x-wav", "audio/wav"),
    ("audio/wave", "audio/wav"),
    ("audio/x-flac", "audio/flac"),
];

/// One `entries` row: the preferred and compatible suffixes of an essence.
#[derive(Debug)]
pub(crate) struct Entry {
    pub(crate) essence: &'static str,
    pub(crate) preferred: &'static str,
    pub(crate) compatible: &'static [&'static str],
}

const fn entry(
    essence: &'static str,
    preferred: &'static str,
    compatible: &'static [&'static str],
) -> Entry {
    Entry {
        essence,
        preferred,
        compatible,
    }
}

/// `entries`, sorted by essence; compatible suffixes keep profile order.
const ENTRIES: &[Entry] = &[
    entry("application/gzip", "gz", &["gz", "tar.gz", "tgz"]),
    entry("application/json", "json", &["json"]),
    entry("application/octet-stream", "bin", &["bin"]),
    entry("application/pdf", "pdf", &["pdf"]),
    entry("application/wasm", "wasm", &["wasm"]),
    entry("application/zip", "zip", &["zip"]),
    entry("audio/flac", "flac", &["flac"]),
    entry("audio/mp4", "m4a", &["m4a", "m4b", "mp4", "mpg4"]),
    entry("audio/mpeg", "mp3", &["mp1", "mp2", "mp3"]),
    entry("audio/ogg", "ogg", &["oga", "ogg", "opus"]),
    entry("audio/wav", "wav", &["wav"]),
    entry("image/gif", "gif", &["gif"]),
    entry("image/jpeg", "jpg", &["jpe", "jpeg", "jpg"]),
    entry("image/png", "png", &["png"]),
    entry("image/svg+xml", "svg", &["svg"]),
    entry("image/webp", "webp", &["webp"]),
    entry("text/css", "css", &["css"]),
    entry("text/javascript", "js", &["js", "mjs"]),
    entry("text/plain", "txt", &["text", "txt"]),
    entry("video/mp4", "mp4", &["m4v", "mp4"]),
    entry("video/webm", "webm", &["webm"]),
];

/// The essence of a canonical media type: the text before any parameter.
pub(crate) fn essence(media_type: &str) -> &str {
    media_type
        .split_once(';')
        .map_or(media_type, |(essence, _)| essence)
}

/// The canonical essence that `essence` is an alias of, if it is one.
pub(crate) fn alias_target(essence: &str) -> Option<&'static str> {
    ALIASES
        .iter()
        .find(|(alias, _)| *alias == essence)
        .map(|(_, target)| *target)
}

/// The registry entry of a canonical essence; `None` when it is unknown.
fn lookup(essence: &str) -> Option<&'static Entry> {
    ENTRIES.iter().find(|entry| entry.essence == essence)
}

/// The preferred suffix of a canonical essence; an unknown essence uses `bin`.
pub(crate) fn preferred_suffix(essence: &str) -> &'static str {
    lookup(essence).map_or(UNKNOWN_PREFERRED, |entry| entry.preferred)
}

/// The unique-suffix reverse index: the one entry whose compatible list holds
/// the lowercase `suffix`. A suffix that two entries list (`mp4`, for
/// audio/mp4 and video/mp4) or that no entry lists maps to nothing.
pub(crate) fn unique_entry_for_suffix(suffix: &str) -> Option<&'static Entry> {
    let mut listing = ENTRIES
        .iter()
        .filter(|entry| entry.compatible.contains(&suffix));
    let entry = listing.next()?;
    listing.next().is_none().then_some(entry)
}

/// The canonical stored hint for an explicit hint on a canonical essence, as
/// the model's `annotation()` computes it: lowercase the hint, enforce the
/// suffix grammar and bound, require a compatible suffix for a known essence
/// other than octet-stream, then store the preferred suffix as no hint.
pub(crate) fn canonical_hint(essence: &str, hint: &str) -> Result<Option<String>> {
    // `to_lowercase` is the model's `str.lower()`; only an ASCII result can
    // pass the suffix grammar.
    let hint = hint.to_lowercase();
    if hint.len() > MAX_SUFFIX_BYTES || !valid_suffix(&hint) {
        return Err(Error::InvalidSuffix);
    }
    let entry = lookup(essence);
    if entry
        .is_some_and(|entry| essence != OCTET_STREAM && !entry.compatible.contains(&hint.as_str()))
    {
        return Err(Error::IncompatibleSuffix);
    }
    let preferred = entry.map_or(UNKNOWN_PREFERRED, |entry| entry.preferred);
    Ok((hint != preferred).then_some(hint))
}

/// The model's `SUFFIX_RE`: dot-separated, nonempty segments of `a-z`, `0-9`,
/// `_` and `-`. That excludes a leading dot, empty segments, slashes,
/// backslashes and traversal.
fn valid_suffix(value: &str) -> bool {
    value.split('.').all(|segment| {
        !segment.is_empty()
            && segment.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
            })
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use sha2::{Digest as _, Sha256};

    use super::*;
    use crate::{MediaAnnotation, MimeRegistry, selected_suffix};

    /// Byte-identical copies of Orna 1.1.0 release files, pinned to the
    /// release `SHA256SUMS` values.
    const PROFILE_JSON: &str = include_str!("fixtures/reference/profiles/media-1.json");
    const PROFILE_SHA256: &str = "7b232d68b7e4a6a2e9c225339639165569896b7ee8e1b8bd06e3331af2f73120";
    const FINAL_VECTORS_JSON: &str = include_str!("fixtures/reference/tests/final-vectors.json");
    const FINAL_VECTORS_SHA256: &str =
        "985140787cde755ae53034de28d46cb4f5696d8bc95e385876e95ec0c953899d";

    fn pinned(text: &str, sha256: &str) -> serde_json::Value {
        assert_eq!(
            hex::encode(Sha256::digest(text.as_bytes())),
            sha256,
            "the pinned reference copy no longer matches the release"
        );
        serde_json::from_str(text).expect("the pinned reference copy is JSON")
    }

    fn text(value: &serde_json::Value) -> &str {
        value.as_str().expect("a profile string")
    }

    #[test]
    fn registry_tables_match_the_pinned_media_profile() {
        let profile = pinned(PROFILE_JSON, PROFILE_SHA256);
        assert_eq!(profile["profile"], PROFILE);
        assert_eq!(profile["version"], 1);
        assert_eq!(profile["maximum_media_type_bytes"], MAX_MEDIA_TYPE_BYTES);
        assert_eq!(profile["maximum_parameters"], MAX_PARAMETERS);
        assert_eq!(profile["maximum_suffix_bytes"], MAX_SUFFIX_BYTES);
        assert_eq!(profile["unknown_preferred"], UNKNOWN_PREFERRED);
        assert_eq!(profile["normative_model"], "tools/final_model.py");

        let aliases = profile["aliases"].as_object().expect("profile aliases");
        assert_eq!(ALIASES.len(), aliases.len());
        assert_eq!(
            ALIASES.iter().copied().collect::<BTreeMap<_, _>>(),
            aliases
                .iter()
                .map(|(alias, target)| (alias.as_str(), text(target)))
                .collect::<BTreeMap<_, _>>()
        );

        let entries = profile["entries"].as_object().expect("profile entries");
        assert_eq!(ENTRIES.len(), entries.len());
        assert!(
            ENTRIES
                .windows(2)
                .all(|pair| pair[0].essence < pair[1].essence),
            "entries are sorted and unique by essence"
        );
        assert_eq!(
            ENTRIES
                .iter()
                .map(|entry| (entry.essence, (entry.preferred, entry.compatible.to_vec())))
                .collect::<BTreeMap<_, _>>(),
            entries
                .iter()
                .map(|(essence, entry)| {
                    let compatible = entry["compatible"].as_array().expect("compatible suffixes");
                    (
                        essence.as_str(),
                        (
                            text(&entry["preferred"]),
                            compatible.iter().map(text).collect::<Vec<_>>(),
                        ),
                    )
                })
                .collect::<BTreeMap<_, _>>()
        );
        assert!(lookup(OCTET_STREAM).is_some());
        for (_, target) in ALIASES {
            assert!(
                lookup(target).is_some(),
                "alias target {target} has an entry"
            );
        }
    }

    #[test]
    fn unique_suffix_index_maps_only_suffixes_one_entry_lists() {
        let profile = pinned(PROFILE_JSON, PROFILE_SHA256);
        let mut listings = BTreeMap::<&str, Vec<&str>>::new();
        for (essence, entry) in profile["entries"].as_object().expect("profile entries") {
            for suffix in entry["compatible"].as_array().expect("compatible suffixes") {
                listings
                    .entry(text(suffix))
                    .or_default()
                    .push(essence.as_str());
            }
        }
        for (suffix, essences) in &listings {
            let expected = match essences.as_slice() {
                [essence] => Some(*essence),
                _ => None,
            };
            assert_eq!(
                unique_entry_for_suffix(suffix).map(|entry| entry.essence),
                expected,
                "suffix {suffix}"
            );
        }
        assert_eq!(listings["mp4"], ["audio/mp4", "video/mp4"]);

        let registry = MimeRegistry::mime1();
        let mapped = |suffix: &str| {
            registry.annotation_for_suffix(suffix).map(|annotation| {
                (
                    annotation.media_type().to_owned(),
                    annotation.suffix().map(str::to_owned),
                )
            })
        };
        assert_eq!(mapped("mp4"), None);
        assert_eq!(
            mapped("m4v"),
            Some(("video/mp4".into(), Some("m4v".into())))
        );
        assert_eq!(mapped("M4A"), Some(("audio/mp4".into(), None)));
        assert_eq!(mapped("MP3"), Some(("audio/mpeg".into(), None)));
        assert_eq!(
            mapped("tgz"),
            Some(("application/gzip".into(), Some("tgz".into())))
        );
        assert_eq!(mapped("bin"), Some((OCTET_STREAM.into(), None)));
        assert_eq!(mapped("xyz"), None);
        assert_eq!(mapped(""), None);
    }

    #[test]
    fn final_vector_media_cases_hold() {
        let vectors = pinned(FINAL_VECTORS_JSON, FINAL_VECTORS_SHA256);
        let media = vectors["media"].as_array().expect("media vectors");
        assert_eq!(media.len(), 7);
        for case in media {
            let input = text(&case["input"]);
            let hint = case["hint"].as_str();
            let canonical_type = text(&case["canonical_type"]);
            let canonical_hint = case["canonical_hint"].as_str();
            let suffix = text(&case["suffix"]);

            let annotation = MimeRegistry::mime1()
                .canonical_annotation(input, hint)
                .expect("a media vector annotates");
            assert_eq!(annotation.media_type(), canonical_type, "{case}");
            assert_eq!(annotation.suffix(), canonical_hint, "{case}");
            assert_eq!(annotation.selected_suffix(), suffix, "{case}");
            // The canonical pair is also the wire form, so it validates as is.
            assert_eq!(
                MediaAnnotation::new(canonical_type, canonical_hint),
                Ok(annotation),
                "{case}"
            );
            assert_eq!(
                selected_suffix(canonical_type, canonical_hint).as_deref(),
                Ok(suffix),
                "{case}"
            );
        }
    }

    /// ORNA-MIME-003/-004 (A08-08): octet-stream and unknown essences admit any
    /// safe hint, a known essence admits only a compatible one, and the
    /// preferred suffix is stored as no hint.
    #[test]
    fn hint_rules_follow_the_normative_model() {
        let registry = MimeRegistry::mime1();
        let stored = |media_type: &str, hint: &str| {
            registry
                .canonical_annotation(media_type, Some(hint))
                .map(|annotation| annotation.suffix().map(str::to_owned))
        };
        assert_eq!(stored(OCTET_STREAM, "dat"), Ok(Some("dat".into())));
        assert_eq!(stored(OCTET_STREAM, "Tar.GZ"), Ok(Some("tar.gz".into())));
        assert_eq!(stored(OCTET_STREAM, "BIN"), Ok(None));
        assert_eq!(
            stored("application/x-example", "my_ext"),
            Ok(Some("my_ext".into()))
        );
        assert_eq!(stored("application/x-example", "bin"), Ok(None));
        assert_eq!(stored("audio/mpeg", "MP2"), Ok(Some("mp2".into())));
        assert_eq!(stored("audio/mpeg", "mp3"), Ok(None));
        assert_eq!(stored("audio/mpeg", "m4a"), Err(Error::IncompatibleSuffix));
        assert_eq!(stored("audio/x-wav", "bin"), Err(Error::IncompatibleSuffix));
        // The model lowercases with `str.lower()`, which maps the Kelvin sign
        // to an ASCII `k`.
        assert_eq!(
            stored("application/x-example", "\u{212A}"),
            Ok(Some("k".into()))
        );
        for unsafe_hint in [
            "", ".bin", "bin.", "a..b", "a/b", "a\\b", "..", "a b", "\u{e9}",
        ] {
            for media_type in [OCTET_STREAM, "application/x-example", "image/jpeg"] {
                assert_eq!(
                    stored(media_type, unsafe_hint),
                    Err(Error::InvalidSuffix),
                    "{media_type} {unsafe_hint:?}"
                );
            }
        }
        let longest = "a".repeat(MAX_SUFFIX_BYTES);
        assert_eq!(stored(OCTET_STREAM, &longest), Ok(Some(longest.clone())));
        assert_eq!(
            stored(OCTET_STREAM, &format!("{longest}a")),
            Err(Error::InvalidSuffix)
        );

        // Wire values must already be canonical.
        assert!(MediaAnnotation::new(OCTET_STREAM, Some("dat")).is_ok());
        assert!(MediaAnnotation::new("application/x-example", Some("my_ext")).is_ok());
        assert_eq!(
            MediaAnnotation::new(OCTET_STREAM, Some("bin")),
            Err(Error::NonCanonical)
        );
        assert_eq!(
            MediaAnnotation::new(OCTET_STREAM, Some("DAT")),
            Err(Error::NonCanonical)
        );
        assert_eq!(
            MediaAnnotation::new("image/png", Some("jpg")),
            Err(Error::IncompatibleSuffix)
        );
    }

    #[test]
    fn selected_suffix_and_essence_read_the_registry() {
        let annotation = |media_type: &str, suffix: Option<&str>| {
            MediaAnnotation::new(media_type, suffix).expect("a canonical annotation")
        };
        let plain = annotation("text/plain;charset=utf-8;title=\"a b\"", None);
        assert_eq!(plain.essence(), "text/plain");
        assert_eq!(plain.selected_suffix(), "txt");
        assert_eq!(
            annotation("application/x-example", None).selected_suffix(),
            "bin"
        );
        assert_eq!(
            annotation(OCTET_STREAM, Some("dat")).selected_suffix(),
            "dat"
        );
        assert_eq!(annotation("image/jpeg", None).selected_suffix(), "jpg");
        assert_eq!(
            annotation("video/mp4;codecs=avc1", None).selected_suffix(),
            "mp4"
        );
    }
}
