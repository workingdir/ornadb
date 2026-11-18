//! `Repository::local_tags` lists the tags of a real repository: the object each
//! names and, for an annotated tag, its message and tagger.

#[path = "../src/test_support.rs"]
mod test_support;

use orna_repository::{CommittedTag, Repository, RepositoryError};
use tempfile::TempDir;
use test_support::{commit_initial_module, git, git_ok, git_with_input};

/// A repository on `main` with one commit.
fn repository() -> (TempDir, Repository) {
    let directory = tempfile::tempdir().expect("repository directory");
    test_support::initialize_fixture_repository(directory.path());
    commit_initial_module(directory.path());
    let repository = Repository::discover(directory.path()).expect("a Git worktree");
    (directory, repository)
}

fn names(tags: &[CommittedTag]) -> Vec<&str> {
    tags.iter().map(CommittedTag::name).collect()
}

fn tag<'a>(tags: &'a [CommittedTag], name: &str) -> &'a CommittedTag {
    tags.iter()
        .find(|tag| tag.name() == name)
        .unwrap_or_else(|| panic!("no tag {name}"))
}

/// Writes a tag object from its text, which Git is not asked to validate, and
/// points `refs/tags/name` at it.
fn tag_object(root: &std::path::Path, name: &str, text: &str) {
    let oid = git_with_input(
        root,
        &["hash-object", "-t", "tag", "-w", "--stdin", "--literally"],
        text,
    );
    git_ok(root, &["update-ref", &format!("refs/tags/{name}"), &oid]);
}

#[test]
fn lists_lightweight_and_annotated_tags_in_name_order() {
    let (directory, repository) = repository();
    let root = directory.path();
    let commit = git(root, &["rev-parse", "HEAD"]);
    git_ok(root, &["tag", "zulu"]);
    git_ok(root, &["tag", "-a", "-m", "Release 1\n\nbody", "v1"]);
    git_ok(root, &["tag", "alpha"]);

    let tags = repository.local_tags(16).expect("a listing");
    assert_eq!(names(&tags), ["alpha", "v1", "zulu"]);

    for name in ["alpha", "zulu"] {
        let lightweight = tag(&tags, name);
        assert_eq!(lightweight.object().as_str(), commit, "{name}");
        assert!(!lightweight.is_annotated(), "{name}");
        assert_eq!(lightweight.annotation(), None, "{name}");
    }

    let annotated = tag(&tags, "v1");
    let tag_oid = git(root, &["rev-parse", "refs/tags/v1"]);
    assert_ne!(tag_oid, commit, "the ref names a tag object");
    assert_eq!(
        annotated.object().as_str(),
        commit,
        "the tag names what its tag object tags, not the tag object"
    );
    assert!(annotated.is_annotated());
    let annotation = annotated.annotation().expect("an annotation");
    assert_eq!(annotation.message(), "Release 1\n\nbody\n");
    let tagger = annotation.tagger().expect("a tagger");
    assert_eq!(tagger.name(), "kierandrewett");
    assert_eq!(tagger.email(), Some("kieran@drewett.dev"));
}

#[test]
fn a_lightweight_tag_has_no_message_even_when_its_commit_has_one() {
    let (directory, repository) = repository();
    let root = directory.path();
    git_ok(
        root,
        &[
            "commit",
            "--allow-empty",
            "-m",
            "subject\n\nthe commit body",
        ],
    );
    git_ok(root, &["tag", "light"]);
    let tags = repository.local_tags(16).expect("a listing");
    let light = tag(&tags, "light");
    assert_eq!(light.annotation(), None);
    assert_eq!(
        light.object().as_str(),
        git(root, &["rev-parse", "HEAD"]),
        "the commit the ref names"
    );
}

#[test]
fn a_tag_of_a_tag_names_the_inner_tag_object() {
    let (directory, repository) = repository();
    let root = directory.path();
    git_ok(root, &["tag", "-a", "-m", "inner", "inner"]);
    let inner_object = git(root, &["rev-parse", "refs/tags/inner"]);
    git_ok(
        root,
        &[
            "-c",
            "advice.nestedTag=false",
            "tag",
            "-a",
            "-m",
            "outer",
            "outer",
            &inner_object,
        ],
    );

    let tags = repository.local_tags(16).expect("a listing");
    assert_eq!(
        tag(&tags, "outer").object().as_str(),
        inner_object,
        "one level: the object the outer tag object names"
    );
    assert_eq!(
        tag(&tags, "inner").object().as_str(),
        git(root, &["rev-parse", "HEAD"])
    );
}

#[test]
fn a_tag_may_name_a_tree_or_a_blob() {
    let (directory, repository) = repository();
    let root = directory.path();
    let tree = git(root, &["rev-parse", "HEAD^{tree}"]);
    let blob = git(root, &["rev-parse", "HEAD:main.orna"]);
    git_ok(root, &["update-ref", "refs/tags/a-tree", &tree]);
    git_ok(root, &["update-ref", "refs/tags/a-blob", &blob]);
    tag_object(
        root,
        "tag-of-tree",
        &format!(
            "object {tree}\ntype tree\ntag tag-of-tree\ntagger T <t@example.invalid> 1700000000 +0000\n\nm\n"
        ),
    );

    let tags = repository.local_tags(16).expect("a listing");
    assert_eq!(tag(&tags, "a-tree").object().as_str(), tree);
    assert_eq!(tag(&tags, "a-blob").object().as_str(), blob);
    assert!(!tag(&tags, "a-tree").is_annotated());
    assert_eq!(tag(&tags, "tag-of-tree").object().as_str(), tree);
    assert!(tag(&tags, "tag-of-tree").is_annotated());
}

#[test]
fn tag_objects_without_a_tagger_or_an_address_are_read_as_written() {
    let (directory, repository) = repository();
    let root = directory.path();
    let commit = git(root, &["rev-parse", "HEAD"]);
    let object = format!("object {commit}\ntype commit\n");
    tag_object(root, "old", &format!("{object}tag old\n\nold style tag\n"));
    tag_object(
        root,
        "noaddress",
        &format!("{object}tag noaddress\ntagger Nameless <> 1700000000 +0000\n\nm\n"),
    );
    tag_object(
        root,
        "nameless",
        &format!("{object}tag nameless\ntagger <n@example.invalid> 1700000000 +0000\n\nm\n"),
    );
    tag_object(
        root,
        "empty",
        &format!("{object}tag empty\ntagger E <e@example.invalid> 1700000000 +0000\n\n"),
    );
    tag_object(
        root,
        "signed",
        &format!(
            "{object}tag signed\ntagger S <s@example.invalid> 1700000000 +0000\n\nsigned message\n-----BEGIN PGP SIGNATURE-----\n\nc2lnbmF0dXJl\n-----END PGP SIGNATURE-----\n"
        ),
    );

    let tags = repository.local_tags(16).expect("a listing");
    let annotation = |name: &str| tag(&tags, name).annotation().expect("annotated");

    assert_eq!(annotation("old").tagger(), None, "no tagger header");
    assert_eq!(annotation("old").message(), "old style tag\n");

    let tagger = annotation("noaddress").tagger().expect("a tagger");
    assert_eq!((tagger.name(), tagger.email()), ("Nameless", None));

    let tagger = annotation("nameless").tagger().expect("a tagger");
    assert_eq!(
        (tagger.name(), tagger.email()),
        ("", Some("n@example.invalid"))
    );

    assert_eq!(
        annotation("empty").message(),
        "",
        "an empty message is present"
    );

    let message = annotation("signed").message();
    assert!(message.starts_with("signed message\n"), "{message:?}");
    assert!(
        message.contains("-----BEGIN PGP SIGNATURE-----"),
        "the signature block is part of the tag object's message: {message:?}"
    );
}

#[test]
fn a_repository_with_no_tag_has_none() {
    let (_directory, repository) = repository();
    assert!(repository.local_tags(16).expect("a listing").is_empty());

    let empty = tempfile::tempdir().expect("repository directory");
    test_support::initialize_fixture_repository(empty.path());
    let repository = Repository::discover(empty.path()).expect("a Git worktree");
    assert!(
        repository.local_tags(16).expect("a listing").is_empty(),
        "a repository with no commit has no tag"
    );
}

#[test]
fn only_tags_are_listed() {
    let (directory, repository) = repository();
    let root = directory.path();
    git_ok(root, &["branch", "topic"]);
    git_ok(root, &["tag", "v1"]);
    git_ok(
        root,
        &[
            "update-ref",
            "refs/notes/commits",
            &git(root, &["rev-parse", "HEAD"]),
        ],
    );
    let tags = repository.local_tags(16).expect("a listing");
    assert_eq!(names(&tags), ["v1"]);
}

#[test]
fn nested_tag_names_keep_their_slashes() {
    let (directory, repository) = repository();
    git_ok(directory.path(), &["tag", "release/2.0/rc1"]);
    let tags = repository.local_tags(16).expect("a listing");
    assert_eq!(names(&tags), ["release/2.0/rc1"]);
}

#[test]
fn a_tag_ref_may_start_with_a_dash_though_git_tag_will_not_create_one() {
    let (directory, repository) = repository();
    let root = directory.path();
    let commit = git(root, &["rev-parse", "HEAD"]);
    git_ok(root, &["update-ref", "refs/tags/-odd", &commit]);
    let tags = repository.local_tags(16).expect("a listing");
    assert_eq!(names(&tags), ["-odd"]);
    assert_eq!(tags[0].object().as_str(), commit);
}

#[test]
fn more_tags_than_the_bound_are_refused_not_truncated() {
    let (directory, repository) = repository();
    git_ok(directory.path(), &["tag", "one"]);
    git_ok(directory.path(), &["tag", "two"]);
    assert_eq!(repository.local_tags(2).expect("at the bound").len(), 2);
    assert!(matches!(
        repository.local_tags(1),
        Err(RepositoryError::GitOperationFailed)
    ));
    assert!(matches!(
        repository.local_tags(0),
        Err(RepositoryError::GitOperationFailed)
    ));
}

#[test]
fn a_tag_whose_object_is_missing_refuses_the_listing() {
    let (directory, repository) = repository();
    let root = directory.path();
    git_ok(root, &["tag", "fine"]);
    std::fs::write(
        root.join(".git/refs/tags/ghost"),
        "1111111111111111111111111111111111111111\n",
    )
    .expect("a ref to an object the repository does not have");
    assert!(
        repository.local_tags(16).is_err(),
        "no listing is served over a tag that cannot be read"
    );
}
