//! ORNA-KNOW-003 end-to-end evidence for the production remote-analysis gate.
//!
//! Each test builds a real Git repository, loads its committed project through
//! `ProjectLoader`, admits the module through `ApplicationAuthority`, and drives
//! `RemoteAnalysisGate`. They prove the production path, not the security types
//! in isolation: provenance comes from the committed snapshot and admitted
//! source, authorisation and recording are one call, and a default policy
//! yields no record at all.

use orna_application::{
    AdmittedApplication, ApplicationAuthority, CostAcknowledgement, RemoteAnalysisDecision,
    RemoteAnalysisGate, RemoteAnalysisPolicy, RemoteAnalysisProvenance,
    RemoteAnalysisProvenanceError, RemoteAnalysisRecordError, Reproducibility, RetainedArtifact,
};
use orna_evaluator::Limits;
use orna_project::{LoadedProject, ProjectLoader};
use orna_repository::Repository;
use orna_semantic::Catalogue;
use std::{fs, path::Path, process::Command};
use tempfile::TempDir;

const MODULE: &str = "pub fn analyse(): Str = \"label\";\n";

struct CommittedProject {
    _directory: TempDir,
    project: LoadedProject,
    application: AdmittedApplication,
}

fn git(directory: &Path, arguments: &[&str]) {
    let status = Command::new("git")
        .args(arguments)
        .current_dir(directory)
        .status()
        .expect("git starts");
    assert!(status.success(), "git {arguments:?} succeeds");
}

fn committed_project() -> CommittedProject {
    let directory = tempfile::tempdir().expect("temporary directory is created");
    fs::write(directory.path().join("main.orna"), MODULE).expect("module fixture is written");
    git(directory.path(), &["init", "--quiet"]);
    for (key, value) in [
        ("user.email", "kieran@drewett.dev"),
        ("user.name", "kierandrewett"),
        ("commit.gpgsign", "false"),
    ] {
        git(directory.path(), &["config", key, value]);
    }
    git(directory.path(), &["add", "."]);
    git(directory.path(), &["commit", "--quiet", "-m", "fixture"]);

    let repository = Repository::discover(directory.path()).expect("fixture is a Git worktree");
    let commit = repository
        .head()
        .expect("HEAD is readable")
        .expect("the fixture has a commit");
    let project = ProjectLoader::default()
        .load_committed_snapshot(&repository, &commit)
        .expect("the committed project loads");
    let application = ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default())
        .admit_module("main.orna", MODULE, "analyse")
        .expect("the committed module is admitted");
    CommittedProject {
        _directory: directory,
        project,
        application,
    }
}

fn provenance(fixture: &CommittedProject) -> RemoteAnalysisProvenance {
    RemoteAnalysisProvenance::from_admitted_project(&fixture.project, &fixture.application)
        .expect("the admitted source is the committed module")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn disclosed() -> Vec<String> {
    ["source", "dependency-snapshot", "query"]
        .map(str::to_owned)
        .to_vec()
}

#[test]
fn remote_analysis_is_denied_by_default_and_records_nothing() {
    let fixture = committed_project();
    let provenance = provenance(&fixture);
    let gate = RemoteAnalysisGate::new(RemoteAnalysisPolicy::default());

    assert_eq!(
        gate.authorise(
            &provenance,
            disclosed(),
            0,
            CostAcknowledgement::Given,
            Reproducibility::NotReproducible,
        ),
        Err(RemoteAnalysisRecordError::NotAllowed(
            RemoteAnalysisDecision::DeniedLocalOnly
        ))
    );
    // The provenance itself is exact even though nothing was recorded for the
    // denied request: it names the committed snapshot and admitted source.
    let snapshot = fixture
        .project
        .source_snapshot_id()
        .expect("a committed project records its snapshot");
    assert!(
        provenance
            .identity()
            .starts_with(&format!("git:{}", hex(&snapshot)))
    );
    assert!(provenance.identity().contains(";module:main.orna:"));
}

#[test]
fn allowed_analysis_records_the_exact_provenance_and_disclosure() {
    let fixture = committed_project();
    let provenance = provenance(&fixture);
    let gate =
        RemoteAnalysisGate::new(RemoteAnalysisPolicy::AllowRemoteDisclosure { max_cost_units: 0 });

    let recorded = gate
        .authorise(
            &provenance,
            disclosed(),
            0,
            CostAcknowledgement::Withheld,
            Reproducibility::NotReproducible,
        )
        .expect("a zero-cost disclosure is allowed by an explicit host policy");
    assert_eq!(recorded.source_snapshot(), provenance.identity());
    assert_eq!(recorded.disclosed_data(), disclosed().as_slice());
    assert_eq!(recorded.declared_cost_units(), 0);
    assert_eq!(
        recorded.reproducibility(),
        &Reproducibility::NotReproducible
    );
}

#[test]
fn nontrivial_cost_needs_an_acknowledgement_within_the_host_ceiling() {
    let fixture = committed_project();
    let provenance = provenance(&fixture);
    let gate =
        RemoteAnalysisGate::new(RemoteAnalysisPolicy::AllowRemoteDisclosure { max_cost_units: 10 });

    assert_eq!(
        gate.authorise(
            &provenance,
            disclosed(),
            5,
            CostAcknowledgement::Withheld,
            Reproducibility::NotReproducible,
        ),
        Err(RemoteAnalysisRecordError::NotAllowed(
            RemoteAnalysisDecision::DeniedCostUnacknowledged
        ))
    );
    let recorded = gate
        .authorise(
            &provenance,
            disclosed(),
            5,
            CostAcknowledgement::Given,
            Reproducibility::NotReproducible,
        )
        .expect("an acknowledged cost within the ceiling is allowed");
    assert_eq!(recorded.declared_cost_units(), 5);
}

#[test]
fn cost_above_the_host_ceiling_is_refused_even_when_acknowledged() {
    let fixture = committed_project();
    let provenance = provenance(&fixture);
    let gate =
        RemoteAnalysisGate::new(RemoteAnalysisPolicy::AllowRemoteDisclosure { max_cost_units: 10 });

    assert_eq!(
        gate.authorise(
            &provenance,
            disclosed(),
            11,
            CostAcknowledgement::Given,
            Reproducibility::NotReproducible,
        ),
        Err(RemoteAnalysisRecordError::NotAllowed(
            RemoteAnalysisDecision::DeniedCostExceedsPolicy
        ))
    );
}

#[test]
fn provenance_is_not_a_reproducibility_claim() {
    let fixture = committed_project();
    let provenance = provenance(&fixture);
    let gate =
        RemoteAnalysisGate::new(RemoteAnalysisPolicy::AllowRemoteDisclosure { max_cost_units: 0 });
    let output = RetainedArtifact::new("remote-output:sha256:abc").expect("artifact identity");
    let artifacts = RetainedArtifact::new("model-artifacts:sha256:def").expect("artifact identity");

    let replay = gate
        .authorise(
            &provenance,
            disclosed(),
            0,
            CostAcknowledgement::Withheld,
            Reproducibility::RetainedOutputReplay(output.clone()),
        )
        .expect("replay of a retained output is allowed");
    let reexecution = gate
        .authorise(
            &provenance,
            disclosed(),
            0,
            CostAcknowledgement::Withheld,
            Reproducibility::ReExecutionWithRetainedArtifacts(artifacts.clone()),
        )
        .expect("re-execution with retained artifacts is allowed");
    let not_reproducible = gate
        .authorise(
            &provenance,
            disclosed(),
            0,
            CostAcknowledgement::Withheld,
            Reproducibility::NotReproducible,
        )
        .expect("a non-reproducible analysis is allowed");

    // The same exact provenance backs three different reproduction promises,
    // so the record's provenance cannot be read as a reproduction guarantee.
    assert_eq!(replay.source_snapshot(), reexecution.source_snapshot());
    assert_eq!(
        replay.reproducibility(),
        &Reproducibility::RetainedOutputReplay(output)
    );
    assert_eq!(
        reexecution.reproducibility(),
        &Reproducibility::ReExecutionWithRetainedArtifacts(artifacts)
    );
    assert_eq!(
        not_reproducible.reproducibility(),
        &Reproducibility::NotReproducible
    );
}

#[test]
fn worktree_project_has_no_recordable_provenance() {
    let directory = tempfile::tempdir().expect("temporary directory is created");
    fs::write(directory.path().join("main.orna"), MODULE).expect("module fixture is written");
    git(directory.path(), &["init", "--quiet"]);
    let repository = Repository::discover(directory.path()).expect("fixture is a Git worktree");
    let project = ProjectLoader::default()
        .load(&repository)
        .expect("a worktree project loads");
    assert_eq!(project.source_snapshot_id(), None);
    let application = ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default())
        .admit_module("main.orna", MODULE, "analyse")
        .expect("the worktree module is admitted");

    assert_eq!(
        RemoteAnalysisProvenance::from_admitted_project(&project, &application),
        Err(RemoteAnalysisProvenanceError::UnpinnedSource)
    );
}

#[test]
fn source_outside_the_pinned_snapshot_is_refused() {
    let fixture = committed_project();
    let edited = format!("{MODULE}pub fn revise(): Int = 1;\n");
    let application = ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default())
        .admit_module("main.orna", edited, "analyse")
        .expect("the edited module is admitted");

    assert_eq!(
        RemoteAnalysisProvenance::from_admitted_project(&fixture.project, &application),
        Err(RemoteAnalysisProvenanceError::SourceNotInSnapshot)
    );
}
