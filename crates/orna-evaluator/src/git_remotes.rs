//! The `sys.git.remotes` rows: the `sys.Remote` projection of the remotes the
//! repository the host attached configures (ORNA-SYS-111, ORNA-SYS-112).
//!
//! `reference` is the `sys.RemoteRef` of the name. `fetch_urls` and `push_urls`
//! are the URLs as the configuration holds them with their credentials redacted
//! by the repository reader (`orna_repository::REDACTED_CREDENTIALS`), so no
//! plaintext credential reaches a row ("Git remote without plaintext
//! credentials"). `promisor` says whether Git treats the remote as a promisor.
//! `last_error` is always null: no remote operation failure is retained anywhere
//! this runtime could read one back, so there is no error to report.
//! [`crate::git_rows`] holds what every Git member shares: the repository read,
//! the commit pin and the failure when either is unavailable.

use std::collections::BTreeMap;

use orna_repository::ConfiguredRemote;
use orna_sys::SysHostContext;
use orna_value::remote_reference;

use crate::git_rows::{GitPage, RowPin, incomplete, page_rows, repository};
use crate::{EvaluationError, Value};

/// Most remotes one read lists. A repository with more is refused with
/// `sys.snapshot.incomplete` rather than served truncated.
const MAX_REMOTES: usize = 1_000;

/// Serves the page of `sys.git.remotes` that follows the remote named by
/// `after`, from the repository `context` attached.
pub(crate) fn remotes_page(
    context: &SysHostContext,
    after: Option<&[u8]>,
    rows: usize,
) -> Result<GitPage, EvaluationError> {
    let mut remotes = repository(context)?
        .configured_remotes(MAX_REMOTES)
        .map_err(|_| incomplete())?;
    remotes.sort_by(|left, right| left.name().cmp(right.name()));
    let facts = remotes.iter().map(RemoteFacts::of).collect::<Vec<_>>();
    page_rows(
        context,
        &facts,
        |remote| remote.name,
        after,
        rows,
        remote_row,
    )
}

/// What one `sys.Remote` row is built from.
struct RemoteFacts<'a> {
    name: &'a str,
    fetch_urls: &'a [String],
    push_urls: &'a [String],
    promisor: bool,
}

impl<'a> RemoteFacts<'a> {
    fn of(remote: &'a ConfiguredRemote) -> Self {
        Self {
            name: remote.name(),
            fetch_urls: remote.fetch_urls(),
            push_urls: remote.push_urls(),
            promisor: remote.is_promisor(),
        }
    }
}

/// The `sys.Remote` row of one remote: `{ reference, name, fetch_urls,
/// push_urls, promisor, last_error }` (34-system-reference.md, `sys.Remote`).
fn remote_row(pin: &RowPin, remote: &RemoteFacts<'_>) -> Result<Value, EvaluationError> {
    let urls =
        |urls: &[String]| Value::list(urls.iter().map(|url| Value::String(url.clone())).collect());
    Ok(Value::Record(BTreeMap::from([
        (
            "reference".to_owned(),
            pin.reference(|database, snapshot| remote_reference(database, snapshot, remote.name))?,
        ),
        ("name".to_owned(), Value::String(remote.name.to_owned())),
        ("fetch_urls".to_owned(), urls(remote.fetch_urls)),
        ("push_urls".to_owned(), urls(remote.push_urls)),
        ("promisor".to_owned(), Value::Bool(remote.promisor)),
        ("last_error".to_owned(), Value::Option(None)),
    ])))
}

#[cfg(test)]
mod tests {
    use super::{RemoteFacts, remote_row};
    use crate::Value;
    use crate::git_rows::testing::{DATABASE, fields, pin, reference_of, row_pin};
    use orna_value::{relations, remote_reference};

    fn urls(row: &Value, field: &str) -> Vec<String> {
        let Value::List(values) = &fields(row)[field] else {
            panic!("`{field}` is a list");
        };
        values
            .iter()
            .map(|value| match value {
                Value::String(url) => url.clone(),
                other => panic!("a URL is text, not {other:?}"),
            })
            .collect()
    }

    // Specified: 34-system-reference.md `sys.Remote`; ORNA-SYS-111, ORNA-SYS-112.
    #[test]
    fn a_remote_row_has_its_urls_promisor_flag_and_no_last_error() {
        let fetch = ["https://example.invalid/org/repo.git".to_owned()];
        let push = [
            "ssh://example.invalid/org/a.git".to_owned(),
            "ssh://example.invalid/org/b.git".to_owned(),
        ];
        let remote = RemoteFacts {
            name: "origin",
            fetch_urls: &fetch,
            push_urls: &push,
            promisor: true,
        };
        let row = remote_row(&row_pin(), &remote).expect("a remote row");
        let fields = fields(&row);
        assert_eq!(
            fields.keys().map(String::as_str).collect::<Vec<_>>(),
            [
                "fetch_urls",
                "last_error",
                "name",
                "promisor",
                "push_urls",
                "reference"
            ],
            "exactly the declared fields"
        );
        assert_eq!(fields["name"], Value::String("origin".to_owned()));
        assert_eq!(fields["promisor"], Value::Bool(true));
        assert_eq!(fields["last_error"], Value::Option(None));
        assert_eq!(urls(&row, "fetch_urls"), fetch);
        assert_eq!(urls(&row, "push_urls"), push);

        let reference = reference_of(&fields["reference"]);
        assert!(reference.is_of(&relations::REMOTE), "a sys.RemoteRef");
        assert_eq!(reference.key_text(), Some("origin"));
        assert_eq!(
            reference.as_row_ref().to_canonical().expect("encodes"),
            remote_reference(DATABASE, pin(), "origin")
                .expect("remote reference")
                .to_canonical()
                .expect("encodes")
        );
    }

    #[test]
    fn a_remote_with_no_url_has_empty_url_lists() {
        let remote = RemoteFacts {
            name: "stub",
            fetch_urls: &[],
            push_urls: &[],
            promisor: false,
        };
        let row = remote_row(&row_pin(), &remote).expect("a remote row");
        assert_eq!(urls(&row, "fetch_urls"), Vec::<String>::new());
        assert_eq!(urls(&row, "push_urls"), Vec::<String>::new());
        assert_eq!(fields(&row)["promisor"], Value::Bool(false));
    }

    #[test]
    fn a_remote_with_no_name_has_no_reference() {
        let remote = RemoteFacts {
            name: "",
            fetch_urls: &[],
            push_urls: &[],
            promisor: false,
        };
        let failure = remote_row(&row_pin(), &remote).expect_err("a reference over an empty name");
        assert_eq!(failure.code(), "sys.snapshot.incomplete");
    }
}
