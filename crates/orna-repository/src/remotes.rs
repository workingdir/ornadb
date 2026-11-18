//! The remotes a repository configures, as `sys.Remote` projects them: each
//! remote's fetch and push URLs and whether it is a promisor.
//!
//! One `git config` run lists every `remote.<name>.*` entry and the
//! partial-clone remote `extensions.partialClone` names, so the remotes of one
//! listing are one observation of the configuration. The configuration is read
//! as written: `url.<base>.insteadOf` rewrites are not applied, and nothing
//! contacts a remote.
//!
//! Credentials never leave this module. A URL is redacted as it is read
//! ([`redact_remote_url`]), so no caller can hold, print or serve the configured
//! text of one.

use std::collections::BTreeMap;

use crate::{
    Repository, RepositoryError, listing_text, parse_git_bool, run_bounded_listing_accepting,
};

/// What stands in for the credentials of a remote URL: an explicit marker, never
/// plausible text (ORNA-SYS-100).
pub const REDACTED_CREDENTIALS: &str = "<redacted>";

/// One remote the repository configures.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfiguredRemote {
    name: String,
    fetch_urls: Vec<String>,
    push_urls: Vec<String>,
    promisor: bool,
}

impl ConfiguredRemote {
    /// The remote's name, as it appears in `remote.<name>.url`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The remote's `url` values in configuration order, credentials redacted.
    /// A remote with no `url` has none.
    pub fn fetch_urls(&self) -> &[String] {
        &self.fetch_urls
    }

    /// The URLs a push goes to, credentials redacted: the `pushurl` values, or
    /// the `url` values when no `pushurl` is configured, as
    /// `git remote get-url --push --all` reports them.
    pub fn push_urls(&self) -> &[String] {
        &self.push_urls
    }

    /// Whether the remote is a promisor remote: `remote.<name>.promisor` is
    /// true, or `extensions.partialClone` names it. Git treats both as a
    /// promisor.
    pub fn is_promisor(&self) -> bool {
        self.promisor
    }
}

/// The configuration entries that define a remote and its promisor role. Keys
/// reach the pattern in the lowercase spelling Git prints for the section and
/// variable names.
const REMOTE_CONFIG_PATTERN: &str = r"^(remote\..+\..+|extensions\.partialclone)$";

/// The key of the partial-clone remote, as Git prints it.
const PARTIAL_CLONE_KEY: &str = "extensions.partialclone";

impl Repository {
    /// Lists the configured remotes in name order, with their URLs redacted of
    /// credentials and their promisor flag.
    ///
    /// A remote is every name some `remote.<name>.<variable>` entry carries, as
    /// `git remote` lists them; one with no `url` has no URL. More than
    /// `max_remotes` remotes is refused rather than served truncated, and so is
    /// configuration this reader does not understand: a `url` with no value, a
    /// `promisor` that is not a boolean, or an empty remote name.
    pub fn configured_remotes(
        &self,
        max_remotes: usize,
    ) -> Result<Vec<ConfiguredRemote>, RepositoryError> {
        let mut command = self.command();
        command.env("GIT_NO_LAZY_FETCH", "1").args([
            "config",
            "--null",
            "--get-regexp",
            REMOTE_CONFIG_PATTERN,
        ]);
        // `git config` exits 1 when no entry matches: a repository with no remote.
        let output = run_bounded_listing_accepting(command, &[1])?;
        parse_remote_config(&output, max_remotes)
    }
}

/// What the entries of one remote add up to.
#[derive(Default)]
struct RemoteEntries {
    urls: Vec<String>,
    push_urls: Vec<String>,
    promisor: Option<bool>,
}

/// Reads the `git config --null --get-regexp` listing: NUL-terminated entries,
/// each `key`, a newline and the value, or the bare `key` when the file gives it
/// no value. Entries come in the order Git applies them, so the last value of
/// `promisor` wins and an empty `url` or `pushurl` clears the earlier ones, as
/// it does for Git.
fn parse_remote_config(
    listing: &[u8],
    max_remotes: usize,
) -> Result<Vec<ConfiguredRemote>, RepositoryError> {
    let mut entries = listing.split(|byte| *byte == 0);
    // The listing ends with the NUL that ends its last entry.
    match entries.next_back() {
        Some([]) => {}
        _ => return Err(RepositoryError::GitOperationFailed),
    }
    let mut remotes: BTreeMap<String, RemoteEntries> = BTreeMap::new();
    let mut partial_clone = None;
    for entry in entries {
        let (key, value) = match entry.iter().position(|byte| *byte == b'\n') {
            Some(at) => (&entry[..at], Some(&entry[at + 1..])),
            None => (entry, None),
        };
        let key = listing_text(key)?;
        if key == PARTIAL_CLONE_KEY {
            let name = value.ok_or(RepositoryError::GitOperationFailed)?;
            partial_clone = Some(listing_text(name)?.to_owned());
            continue;
        }
        let (name, variable) = key
            .strip_prefix("remote.")
            .and_then(|rest| rest.rsplit_once('.'))
            .ok_or(RepositoryError::GitOperationFailed)?;
        // Git ignores a remote whose name starts with a slash.
        if name.starts_with('/') {
            continue;
        }
        if name.is_empty() {
            return Err(RepositoryError::GitOperationFailed);
        }
        let remote = remotes.entry(name.to_owned()).or_default();
        match variable {
            "url" => push_url(&mut remote.urls, value)?,
            "pushurl" => push_url(&mut remote.push_urls, value)?,
            "promisor" => {
                // A key with no value is Git's spelling of true.
                remote.promisor = Some(match value {
                    None => true,
                    Some(value) => parse_git_bool(listing_text(value)?)
                        .ok_or(RepositoryError::GitOperationFailed)?,
                });
            }
            _ => {}
        }
    }
    if remotes.len() > max_remotes {
        return Err(RepositoryError::GitOperationFailed);
    }
    Ok(remotes
        .into_iter()
        .map(|(name, remote)| {
            let promisor =
                remote.promisor == Some(true) || partial_clone.as_deref() == Some(name.as_str());
            let push_urls = if remote.push_urls.is_empty() {
                remote.urls.clone()
            } else {
                remote.push_urls
            };
            ConfiguredRemote {
                name,
                fetch_urls: remote.urls,
                push_urls,
                promisor,
            }
        })
        .collect())
}

/// Records one `url` or `pushurl` value, redacted. A key with no value is not a
/// URL, and an empty value clears the URLs configured before it.
fn push_url(urls: &mut Vec<String>, value: Option<&[u8]>) -> Result<(), RepositoryError> {
    let value = listing_text(value.ok_or(RepositoryError::GitOperationFailed)?)?;
    if value.is_empty() {
        urls.clear();
    } else {
        urls.push(redact_remote_url(value));
    }
    Ok(())
}

/// Replaces the credentials of a remote URL with [`REDACTED_CREDENTIALS`].
///
/// The credentials of a URL are the user information of a `scheme://` URL, the
/// user name included: a token is often the user name, and a password after it
/// is the other half. A remote helper that runs a command (`ext::`) takes its
/// whole address from a command line, which may carry a credential, so that
/// address is redacted whole. Nothing else carries a credential: the `user@host:path`
/// form and a local path have no password syntax. The rest of the URL is kept as
/// configured, so what the remote points at stays readable.
pub(crate) fn redact_remote_url(url: &str) -> String {
    if let Some((helper, _)) = url.split_once("::")
        && helper.eq_ignore_ascii_case("ext")
    {
        return format!("{helper}::{REDACTED_CREDENTIALS}");
    }
    let Some(scheme_end) = url.find("://") else {
        return url.to_owned();
    };
    let authority_start = scheme_end + "://".len();
    let authority_end = url[authority_start..]
        .find(['/', '?', '#'])
        .map_or(url.len(), |end| authority_start + end);
    // A user name or password with an `@` of its own ends at the last one.
    let Some(at) = url[authority_start..authority_end].rfind('@') else {
        return url.to_owned();
    };
    format!(
        "{}{REDACTED_CREDENTIALS}{}",
        &url[..authority_start],
        &url[authority_start + at..]
    )
}

#[cfg(test)]
mod tests {
    use super::{ConfiguredRemote, REDACTED_CREDENTIALS, parse_remote_config, redact_remote_url};
    use crate::RepositoryError;

    /// A `git config --null --get-regexp` listing of `entries`, each `key=value`
    /// or a bare `key`.
    fn listing(entries: &[&str]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for entry in entries {
            bytes.extend_from_slice(entry.replacen('=', "\n", 1).as_bytes());
            bytes.push(0);
        }
        bytes
    }

    fn parse(entries: &[&str]) -> Vec<ConfiguredRemote> {
        parse_remote_config(&listing(entries), 16).expect("a remote listing")
    }

    fn refused(entries: &[&str]) -> bool {
        matches!(
            parse_remote_config(&listing(entries), 16),
            Err(RepositoryError::GitOperationFailed)
        )
    }

    #[test]
    fn credentials_are_redacted_wherever_a_url_carries_them() {
        let marker = REDACTED_CREDENTIALS;
        for (url, expected) in [
            (
                "https://account:secret@example.invalid/private.git",
                format!("https://{marker}@example.invalid/private.git"),
            ),
            (
                "https://ghp_token@github.com/org/repo.git",
                format!("https://{marker}@github.com/org/repo.git"),
            ),
            (
                "ssh://git@example.invalid:2222/org/repo.git",
                format!("ssh://{marker}@example.invalid:2222/org/repo.git"),
            ),
            (
                "https://user:p@ss@example.invalid/x.git",
                format!("https://{marker}@example.invalid/x.git"),
            ),
            (
                "https://user:pw@example.invalid",
                format!("https://{marker}@example.invalid"),
            ),
            (
                "https://u:pw@example.invalid?ref=main#frag",
                format!("https://{marker}@example.invalid?ref=main#frag"),
            ),
            (
                "hg::https://u:pw@example.invalid/repo",
                format!("hg::https://{marker}@example.invalid/repo"),
            ),
            ("ext::ssh -i /key example.invalid", format!("ext::{marker}")),
            ("EXT::sh -c 'tok=abc fetch'", format!("EXT::{marker}")),
        ] {
            assert_eq!(redact_remote_url(url), expected, "{url}");
        }
    }

    #[test]
    fn a_url_without_credentials_is_kept_as_configured() {
        for url in [
            "https://example.invalid/org/repo.git",
            "https://example.invalid:8443/org/repo.git",
            "https://example.invalid/@scope/repo.git",
            "https://example.invalid/org/user@name.git",
            "git@example.invalid:org/repo.git",
            "example.invalid:org/repo.git",
            "file:///srv/repo.git",
            "/srv/repo.git",
            "../sibling",
            "https://example.invalid/a::b",
        ] {
            assert_eq!(redact_remote_url(url), url);
        }
    }

    #[test]
    fn no_redacted_url_still_holds_the_secret() {
        for url in [
            "https://account:secret@example.invalid/private.git",
            "https://secret@example.invalid/private.git",
            "ssh://account:secret@example.invalid/private.git",
            "ext::fetch --token=secret",
        ] {
            let redacted = redact_remote_url(url);
            assert!(!redacted.contains("secret"), "{redacted}");
            assert!(!redacted.contains("account"), "{redacted}");
        }
    }

    #[test]
    fn a_remote_has_its_urls_and_pushes_to_them_unless_it_names_push_urls() {
        let remotes = parse(&[
            "remote.origin.url=https://a.example.invalid/x.git",
            "remote.origin.fetch=+refs/heads/*:refs/remotes/origin/*",
            "remote.origin.url=https://b.example.invalid/x.git",
            "remote.fork.url=https://f.example.invalid/x.git",
            "remote.fork.pushurl=ssh://p.example.invalid/x.git",
            "remote.fork.pushurl=ssh://q.example.invalid/x.git",
        ]);
        let [fork, origin] = remotes.as_slice() else {
            panic!("two remotes, in name order");
        };
        assert_eq!(origin.name(), "origin");
        assert_eq!(
            origin.fetch_urls(),
            [
                "https://a.example.invalid/x.git",
                "https://b.example.invalid/x.git"
            ]
        );
        assert_eq!(
            origin.push_urls(),
            origin.fetch_urls(),
            "with no pushurl a push goes to the urls"
        );
        assert_eq!(fork.fetch_urls(), ["https://f.example.invalid/x.git"]);
        assert_eq!(
            fork.push_urls(),
            [
                "ssh://p.example.invalid/x.git",
                "ssh://q.example.invalid/x.git"
            ]
        );
    }

    #[test]
    fn the_urls_of_a_listing_are_redacted_not_just_the_first() {
        let remotes = parse(&[
            "remote.origin.url=https://account:secret@example.invalid/private.git",
            "remote.origin.pushurl=https://pusher:hunter2@example.invalid/private.git",
        ]);
        let [origin] = remotes.as_slice() else {
            panic!("one remote");
        };
        assert_eq!(
            origin.fetch_urls(),
            [format!(
                "https://{REDACTED_CREDENTIALS}@example.invalid/private.git"
            )]
        );
        assert_eq!(
            origin.push_urls(),
            [format!(
                "https://{REDACTED_CREDENTIALS}@example.invalid/private.git"
            )]
        );
        let shown = format!("{remotes:?}");
        for secret in ["secret", "account", "hunter2", "pusher"] {
            assert!(!shown.contains(secret), "{secret} leaked: {shown}");
        }
    }

    #[test]
    fn a_remote_is_every_name_some_entry_carries() {
        let remotes = parse(&[
            "remote.stub.fetch=+refs/heads/*:refs/remotes/stub/*",
            "remote.dotted.name.url=file:///srv/x.git",
            "remote.Mixed.Case.tagopt=--no-tags",
            "remote.spaced name.url=ssh://h/y",
        ]);
        let names = remotes
            .iter()
            .map(ConfiguredRemote::name)
            .collect::<Vec<_>>();
        assert_eq!(names, ["Mixed.Case", "dotted.name", "spaced name", "stub"]);
        assert!(remotes[3].fetch_urls().is_empty(), "a stub has no URL");
        assert!(remotes[3].push_urls().is_empty());
    }

    #[test]
    fn a_remote_git_ignores_is_not_one() {
        let remotes = parse(&["remote./abs.url=https://x.invalid/", "remote.ok.url=a"]);
        assert_eq!(
            remotes
                .iter()
                .map(ConfiguredRemote::name)
                .collect::<Vec<_>>(),
            ["ok"]
        );
    }

    #[test]
    fn an_empty_url_clears_the_urls_before_it() {
        let remotes = parse(&[
            "remote.o.url=https://a.invalid/",
            "remote.o.url=https://b.invalid/",
            "remote.o.url=",
            "remote.o.url=https://c.invalid/",
            "remote.o.pushurl=https://p.invalid/",
            "remote.o.pushurl=",
        ]);
        assert_eq!(remotes[0].fetch_urls(), ["https://c.invalid/"]);
        assert_eq!(
            remotes[0].push_urls(),
            ["https://c.invalid/"],
            "a cleared pushurl list pushes to the urls"
        );
    }

    #[test]
    fn promisor_is_the_last_boolean_or_the_partial_clone_remote() {
        let remotes = parse(&[
            "remote.a.promisor=true",
            "remote.b.promisor=true",
            "remote.b.promisor=false",
            "remote.c.promisor=yes",
            "remote.d.promisor",
            "remote.e.url=x",
            "remote.f.promisor=false",
            "extensions.partialclone=f",
        ]);
        let promisor = |name: &str| {
            remotes
                .iter()
                .find(|remote| remote.name() == name)
                .unwrap_or_else(|| panic!("no remote {name}"))
                .is_promisor()
        };
        assert!(promisor("a"));
        assert!(!promisor("b"), "the last value wins");
        assert!(promisor("c"), "Git spells true several ways");
        assert!(promisor("d"), "a key with no value is true");
        assert!(
            !promisor("e"),
            "no promisor entry, not the partial-clone remote"
        );
        assert!(promisor("f"), "extensions.partialClone names it");
    }

    #[test]
    fn the_partial_clone_remote_is_not_a_remote_unless_it_is_configured() {
        let remotes = parse(&["remote.a.url=x", "extensions.partialclone=ghost"]);
        assert_eq!(remotes.len(), 1);
        assert!(!remotes[0].is_promisor());
    }

    #[test]
    fn an_empty_listing_has_no_remote() {
        assert_eq!(parse(&[]), Vec::new());
    }

    #[test]
    fn configuration_this_reader_does_not_understand_is_refused() {
        assert!(refused(&["remote.o.url"]), "a url with no value");
        assert!(refused(&["remote.o.pushurl"]), "a pushurl with no value");
        assert!(refused(&["remote.o.promisor=maybe"]), "not a boolean");
        assert!(refused(&["remote..url=x"]), "an empty remote name");
        assert!(refused(&["extensions.partialclone"]), "no remote named");
        assert!(
            refused(&["remote.pushdefault=x"]),
            "not an entry of a remote"
        );
        assert!(
            matches!(
                parse_remote_config(b"remote.o.url\nx", 16),
                Err(RepositoryError::GitOperationFailed)
            ),
            "an entry with no terminator"
        );
        assert!(
            matches!(
                parse_remote_config(b"remote.o.url\n\xff\0", 16),
                Err(RepositoryError::GitOperationFailed)
            ),
            "a url that is not text"
        );
    }

    #[test]
    fn more_remotes_than_the_bound_are_refused_not_truncated() {
        let entries = listing(&["remote.a.url=x", "remote.b.url=y"]);
        assert_eq!(
            parse_remote_config(&entries, 2)
                .expect("at the bound")
                .len(),
            2
        );
        assert!(matches!(
            parse_remote_config(&entries, 1),
            Err(RepositoryError::GitOperationFailed)
        ));
    }
}
