use std::{fs, path::Path, process::Command};

use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_project_v1::{AttachedDatabaseSession, PinnedDatabase, ProjectLoader};
use orna_repository_v1::Repository;
use orna_value_v1::Value;
use tempfile::TempDir;

fn git(directory: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(directory)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn repository(source: &str) -> (TempDir, Repository, String) {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("main.orna"), source).unwrap();
    git(directory.path(), &["init", "--quiet"]);
    git(
        directory.path(),
        &["config", "user.name", "kierandrewett"],
    );
    git(
        directory.path(),
        &["config", "user.email", "kieran@drewett.dev"],
    );
    git(directory.path(), &["config", "commit.gpgsign", "false"]);
    git(directory.path(), &["add", "main.orna"]);
    git(directory.path(), &["commit", "--quiet", "-m", "route fixture"]);
    let commit = git(directory.path(), &["rev-parse", "HEAD"]);
    let repository = Repository::discover(directory.path()).unwrap();
    (directory, repository, commit)
}

fn commit_source(directory: &Path, source: &str) -> String {
    fs::write(directory.join("main.orna"), source).unwrap();
    git(directory, &["add", "main.orna"]);
    git(directory, &["commit", "--quiet", "-m", "advance package snapshot"]);
    git(directory, &["rev-parse", "HEAD"])
}

#[test]
fn main_alias_and_prefix_alias_import_their_own_pinned_root_modules() {
    let (_primary_dir, primary_repository, primary_commit) = repository(include_str!(
        "fixtures/attachment-route-primary.orna"
    ));
    let (package_dir, package_repository, main_alias_commit) = repository(include_str!(
        "fixtures/attachment-route-package.orna"
    ));
    let longer_alias_commit = commit_source(
        package_dir.path(),
        &include_str!("fixtures/attachment-route-package.orna").replace("42", "43"),
    );

    let loader = ProjectLoader::default();
    let primary =
        PinnedDatabase::resolve("app", primary_repository, &primary_commit, loader).unwrap();
    let main_alias = PinnedDatabase::resolve(
        "main",
        package_repository.clone(),
        &main_alias_commit,
        loader,
    )
    .unwrap();
    let longer_alias = PinnedDatabase::resolve(
        "main_archive",
        package_repository.clone(),
        &longer_alias_commit,
        loader,
    )
    .unwrap();
    let mut databases = AttachedDatabaseSession::new(primary).unwrap();
    databases.attach_database(main_alias).unwrap();
    databases.attach_database(longer_alias).unwrap();
    let modules = databases.module_inputs();
    assert_eq!(
        modules
            .iter()
            .filter(|module| module.logical_path == "main.orna")
            .count(),
        1,
        "the primary retains the root module route"
    );
    assert!(modules.iter().any(|module| {
        module.logical_path == "main.orna" && module.source.contains("primary_value")
    }));
    assert!(modules.iter().any(|module| {
        module.logical_path == "main/main.orna" && module.source.contains("= 42")
    }));
    assert!(modules.iter().any(|module| {
        module.logical_path == "main_archive.orna" && module.source.contains("= 43")
    }));

    let mut session =
        AdmittedReplSession::from_attached_database_session(&databases, Limits::default()).unwrap();
    // Prefix aliases are exact names; a failed near-match must leave the
    // evaluator able to recover by importing either real root alias.
    assert!(session.submit("use main_arch;").is_err());
    assert_eq!(session.submit("use main;"), Ok(None));
    assert_eq!(
        session.submit("main.package_value()"),
        Ok(Some(Value::int(42.into())))
    );
    assert_eq!(session.submit("use main_archive;"), Ok(None));
    assert_eq!(
        session.submit("main_archive.package_value()"),
        Ok(Some(Value::int(43.into())))
    );
    assert_eq!(
        session
            .attached_databases()
            .unwrap()
            .database("main")
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        main_alias_commit
    );
    assert_eq!(
        session
            .attached_databases()
            .unwrap()
            .database("main_archive")
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        longer_alias_commit
    );

    // An admitted REPL keeps the exact alias pins it was built from when the
    // mutable attachment session later detaches or replaces one of them.
    let historical_session = session.clone();
    databases.detach_database("main").unwrap();
    let mut detached_session =
        AdmittedReplSession::from_attached_database_session(&databases, Limits::default()).unwrap();
    assert!(detached_session.submit("use main;").is_err());
    assert_eq!(detached_session.submit("use main_archive;"), Ok(None));
    assert_eq!(
        detached_session.submit("main_archive.package_value()"),
        Ok(Some(Value::int(43.into())))
    );

    let replacement_commit = commit_source(
        package_dir.path(),
        &include_str!("fixtures/attachment-route-package.orna").replace("42", "99"),
    );
    let replacement = PinnedDatabase::resolve(
        "main",
        package_repository,
        &replacement_commit,
        loader,
    )
    .unwrap();
    databases.attach_database(replacement).unwrap();
    let mut replacement_session =
        AdmittedReplSession::from_attached_database_session(&databases, Limits::default()).unwrap();
    assert_eq!(replacement_session.submit("use main;"), Ok(None));
    assert_eq!(
        replacement_session.submit("main.package_value()"),
        Ok(Some(Value::int(99.into())))
    );
    assert_eq!(
        replacement_session.submit("use main_archive;"),
        Ok(None)
    );
    assert_eq!(
        replacement_session.submit("main_archive.package_value()"),
        Ok(Some(Value::int(43.into())))
    );
    assert_eq!(
        historical_session
            .clone()
            .submit("main.package_value()"),
        Ok(Some(Value::int(42.into())))
    );
    assert_eq!(
        historical_session
            .clone()
            .submit("main_archive.package_value()"),
        Ok(Some(Value::int(43.into())))
    );
}
