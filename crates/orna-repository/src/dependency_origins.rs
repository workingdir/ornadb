//! Dependency origins declared by a superproject's `.gitmodules`.
//!
//! A gitlink pins a dependency commit; `.gitmodules` declares where that
//! dependency is fetched from. Git's config parser decodes quoted paths and
//! URLs, so the values returned here are the ones Git itself would use.

use std::{collections::BTreeMap, path::Path};

use crate::{git_command_in, run_command_with_input};

/// Largest `.gitmodules` file read while resolving dependency origins.
pub const MAX_GITMODULES_BYTES: usize = 1 << 20;

/// A `[submodule]` section that declares both a `path` and a `url`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DependencyOrigin {
    path: String,
    url: String,
}

impl DependencyOrigin {
    /// The superproject path the section declares.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The URL the section declares for [`Self::path`].
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }
}

/// Why `.gitmodules` bytes yield no usable dependency origins.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitmodulesError {
    /// The file is longer than [`MAX_GITMODULES_BYTES`].
    TooLarge,
    /// The file is not UTF-8.
    NotUtf8,
    /// A declared `path` or `url` is empty or holds a control character.
    InvalidValue,
    /// Git could not decode the file.
    GitUnavailable,
}

/// Parses `.gitmodules` bytes with Git's config parser, so quoted paths and
/// URLs are decoded before they are matched to gitlinks. A section yields an
/// origin only when it declares both a `path` and a `url`.
///
/// # Errors
///
/// Returns [`GitmodulesError`] when the bytes are oversized, not UTF-8, carry
/// an invalid declared value, or cannot be decoded by Git.
pub fn parse_dependency_origins(bytes: &[u8]) -> Result<Vec<DependencyOrigin>, GitmodulesError> {
    if bytes.len() > MAX_GITMODULES_BYTES {
        return Err(GitmodulesError::TooLarge);
    }
    std::str::from_utf8(bytes).map_err(|_| GitmodulesError::NotUtf8)?;
    let listing = list_config(bytes)?;
    let mut sections: BTreeMap<String, (Option<String>, Option<String>)> = BTreeMap::new();
    for record in listing
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let (key, value) = match record.iter().position(|byte| *byte == b'\n') {
            Some(separator) => (&record[..separator], &record[separator + 1..]),
            // Git writes a key without `=` as the key followed by NUL, with no separator.
            None => (record, &record[record.len()..]),
        };
        let key = std::str::from_utf8(key).map_err(|_| GitmodulesError::GitUnavailable)?;
        let value = std::str::from_utf8(value).map_err(|_| GitmodulesError::NotUtf8)?;
        let Some(key) = key.get("submodule.".len()..).filter(|_| {
            key.get(.."submodule.".len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("submodule."))
        }) else {
            continue;
        };
        let (name, is_path) = if key.len() >= ".path".len()
            && key
                .get(key.len() - ".path".len()..)
                .is_some_and(|suffix| suffix.eq_ignore_ascii_case(".path"))
        {
            (&key[..key.len() - ".path".len()], true)
        } else if key.len() >= ".url".len()
            && key
                .get(key.len() - ".url".len()..)
                .is_some_and(|suffix| suffix.eq_ignore_ascii_case(".url"))
        {
            (&key[..key.len() - ".url".len()], false)
        } else {
            continue;
        };
        if name.is_empty() || value.is_empty() || value.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(GitmodulesError::InvalidValue);
        }
        let pair = sections.entry(name.to_owned()).or_default();
        if is_path {
            pair.0 = Some(value.to_owned());
        } else {
            pair.1 = Some(value.to_owned());
        }
    }
    Ok(sections
        .into_values()
        .filter_map(|(path, url)| {
            Some(DependencyOrigin {
                path: path?,
                url: url?,
            })
        })
        .collect())
}

/// Lists `bytes` as `--null`-terminated config entries. Includes are not
/// followed, so only the declared file is consulted.
fn list_config(bytes: &[u8]) -> Result<Vec<u8>, GitmodulesError> {
    let mut command = git_command_in(Path::new("."));
    command.args(["config", "--no-includes", "--null", "--list", "--file", "-"]);
    run_command_with_input(command, bytes)
        .map(|output| output.stdout)
        .map_err(|_| GitmodulesError::GitUnavailable)
}

#[cfg(test)]
mod tests {
    use super::{
        DependencyOrigin, GitmodulesError, MAX_GITMODULES_BYTES, parse_dependency_origins,
    };

    fn origin(path: &str, url: &str) -> DependencyOrigin {
        DependencyOrigin {
            path: path.to_owned(),
            url: url.to_owned(),
        }
    }

    #[test]
    fn gitmodules_pairs_are_read_one_section_at_a_time() {
        let text = "[submodule \"std\"]\n\tpath = stdlib/std\n\turl = https://example.invalid/std.git\n\
                    [submodule \"tools\"]\n\tpath = tools\n\turl = ../tools.git\n\tbranch = main\n";
        assert_eq!(
            parse_dependency_origins(text.as_bytes()).unwrap(),
            vec![
                origin("stdlib/std", "https://example.invalid/std.git"),
                origin("tools", "../tools.git"),
            ]
        );
    }

    #[test]
    fn gitmodules_quoted_values_are_decoded() {
        let text = "[submodule \"reference\"]\n\tpath = \"deps/reference project\"\n\turl = \"file:///tmp/reference project\"\n";
        assert_eq!(
            parse_dependency_origins(text.as_bytes()).unwrap(),
            vec![origin(
                "deps/reference project",
                "file:///tmp/reference project"
            )]
        );
    }

    #[test]
    fn section_without_a_url_declares_no_origin() {
        assert_eq!(
            parse_dependency_origins(b"[submodule \"a\"]\n\tpath = a\n").unwrap(),
            Vec::new()
        );
    }

    #[test]
    fn gitmodules_values_with_control_characters_are_refused() {
        assert_eq!(
            parse_dependency_origins(b"[submodule \"a\"]\n\tpath = a\n\turl = oops\ttab\n"),
            Err(GitmodulesError::InvalidValue)
        );
        assert_eq!(
            parse_dependency_origins(b"[submodule \"a\"]\n\tpath = a\n\turl = \n"),
            Err(GitmodulesError::InvalidValue)
        );
    }

    #[test]
    fn oversized_or_non_utf8_gitmodules_is_refused_before_git_runs() {
        let oversized = vec![b'#'; MAX_GITMODULES_BYTES + 1];
        assert_eq!(
            parse_dependency_origins(&oversized),
            Err(GitmodulesError::TooLarge)
        );
        assert_eq!(
            parse_dependency_origins(b"\xff"),
            Err(GitmodulesError::NotUtf8)
        );
    }
}
