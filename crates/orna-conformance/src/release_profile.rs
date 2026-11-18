//! The published conformance profile and repository coordinates.
//!
//! `profiles/conformance-1.1.json` and `profiles/coordinates.json` are read
//! beside `release.json`. Both must be bound to the release's version and
//! publication revision, every profile's `requires` must name a profile, and
//! every required identifier and coordinate value must resolve either to a
//! coordinate or to a term the normative `source/` text defines.

use std::{collections::BTreeMap, fs, path::Path};

use serde::Deserialize;

use crate::CorpusError;

#[derive(Debug, Clone, Deserialize)]
pub struct ConformanceProfiles {
    pub version: String,
    pub publication_revision: String,
    pub profiles: BTreeMap<String, ProfileRequirements>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProfileRequirements {
    #[serde(default)]
    pub required: Vec<String>,
    #[serde(default)]
    pub required_prefixes: Vec<String>,
    #[serde(default)]
    pub requires: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Coordinates {
    pub language: String,
    pub publication_revision: String,
    pub repository_format: u32,
    pub graph: String,
    pub rows: String,
    pub blob_storage: String,
    pub stored_values: String,
    pub semantic_values: String,
    pub portable_values: String,
    pub virtual_filesystem: String,
    pub media_policy: String,
}

impl Coordinates {
    fn identifiers(&self) -> [&str; 8] {
        [
            &self.graph,
            &self.rows,
            &self.blob_storage,
            &self.stored_values,
            &self.semantic_values,
            &self.portable_values,
            &self.virtual_filesystem,
            &self.media_policy,
        ]
    }
}

#[derive(Debug, Deserialize)]
struct Release {
    version: String,
    publication_revision: String,
}

/// Loads and binds the 1.1.0 profile and coordinate records to `release.json`.
pub fn load_checked(root: &Path) -> Result<(ConformanceProfiles, Coordinates), CorpusError> {
    let release: Release = read_json(root, "release.json")?;
    let profiles: ConformanceProfiles = read_json(root, "profiles/conformance-1.1.json")?;
    let coordinates: Coordinates = read_json(root, "profiles/coordinates.json")?;
    if profiles.version != release.version
        || profiles.publication_revision != release.publication_revision
    {
        return Err(CorpusError(
            "conformance profile version or publication revision disagrees with release.json"
                .into(),
        ));
    }
    if coordinates.language != release.version
        || coordinates.publication_revision != release.publication_revision
    {
        return Err(CorpusError(
            "repository coordinates disagree with release.json".into(),
        ));
    }
    for (name, profile) in &profiles.profiles {
        if let Some(target) = &profile.requires
            && !profiles.profiles.contains_key(target)
        {
            return Err(CorpusError(format!(
                "profile {name} requires unknown profile {target}"
            )));
        }
    }
    let spec = normative_source_text(root)?;
    let coordinate_values = coordinates.identifiers();
    for identifier in coordinate_values {
        if !contains_token(&spec, identifier) {
            return Err(CorpusError(format!(
                "coordinate {identifier} is not defined by the normative source"
            )));
        }
    }
    for (name, profile) in &profiles.profiles {
        for identifier in &profile.required {
            if !coordinate_values.contains(&identifier.as_str())
                && !contains_token(&spec, identifier)
            {
                return Err(CorpusError(format!(
                    "required identifier {identifier} of profile {name} is neither a coordinate nor defined by the normative source"
                )));
            }
        }
    }
    Ok((profiles, coordinates))
}

fn read_json<T: for<'de> Deserialize<'de>>(root: &Path, relative: &str) -> Result<T, CorpusError> {
    let body = fs::read_to_string(root.join(relative))
        .map_err(|_| CorpusError(format!("cannot read reference JSON: {relative}")))?;
    serde_json::from_str(&body)
        .map_err(|_| CorpusError(format!("invalid reference JSON: {relative}")))
}

fn normative_source_text(root: &Path) -> Result<String, CorpusError> {
    let directory = root.join("source");
    let entries = fs::read_dir(&directory)
        .map_err(|_| CorpusError("cannot read normative source directory".into()))?;
    let mut paths = Vec::new();
    for entry in entries {
        let path = entry
            .map_err(|_| CorpusError("cannot read normative source entry".into()))?
            .path();
        if path.extension().is_some_and(|extension| extension == "md") {
            paths.push(path);
        }
    }
    paths.sort();
    let mut text = String::new();
    for path in paths {
        let body = fs::read_to_string(&path).map_err(|_| {
            CorpusError(format!("cannot read normative source: {}", path.display()))
        })?;
        text.push_str(&body);
        text.push('\n');
    }
    Ok(text)
}

fn contains_token(text: &str, token: &str) -> bool {
    text.match_indices(token).any(|(start, _)| {
        let before = text[..start].chars().next_back();
        let after = text[start + token.len()..].chars().next();
        !before.is_some_and(is_identifier_character) && !after.is_some_and(is_identifier_character)
    })
}

fn is_identifier_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '-' || character == '_'
}
