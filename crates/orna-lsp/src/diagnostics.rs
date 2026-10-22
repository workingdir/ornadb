//! Diagnostics for open documents, computed off the request path.
//!
//! Every open document gets its syntax diagnostics. Documents in a project
//! also get the project check (`project::check`), published per module file,
//! including modules that are not open. A worker thread computes and
//! publishes them; when edits arrive faster than checks finish, it checks
//! only the newest editor state.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use lsp_server::{Message, Notification};
use lsp_types::{Diagnostic, PublishDiagnosticsParams, Uri};

use crate::analysis;
use crate::documents::PositionMapper;
use crate::project::{self, OpenModule, Project, ProjectHome};
use crate::workspace::OpenDocument;

/// The open documents diagnostics are computed from.
pub(crate) type Snapshot = Arc<Vec<Arc<OpenDocument>>>;

/// Diagnostics for one URI, with the document version they describe.
pub(crate) struct Report {
    pub uri: Uri,
    /// `None` for a module that is not open.
    pub version: Option<i32>,
    pub diagnostics: Vec<Diagnostic>,
    /// The project root, for project modules.
    root: Option<PathBuf>,
}

/// Diagnostics for `uri` in `snapshot`, as the worker would publish them.
pub(crate) fn document_report(snapshot: &[Arc<OpenDocument>], uri: &Uri) -> Option<Report> {
    let open = snapshot.iter().find(|open| &open.document.uri == uri)?;
    let Some(home) = &open.home else {
        return Some(syntax_report(open));
    };
    project_reports(snapshot, &home.project)
        .into_iter()
        .find(|report| &report.uri == uri)
}

fn syntax_report(open: &OpenDocument) -> Report {
    let file = open.analysis();
    let mapper = PositionMapper::new(file.text());
    Report {
        uri: open.document.uri.clone(),
        version: Some(open.document.version),
        diagnostics: analysis::syntax_diagnostics(file.uri(), &file.parse, &mapper),
        root: None,
    }
}

/// Reports for every open module of `project` and every closed module with
/// diagnostics, in URI order.
fn project_reports(snapshot: &[Arc<OpenDocument>], project: &Arc<Project>) -> Vec<Report> {
    let root = project.root().to_path_buf();
    let members = snapshot
        .iter()
        .filter_map(|open| {
            let home = open.home.as_ref()?;
            Arc::ptr_eq(&home.project, project).then_some((open, home.logical_path.as_str()))
        })
        .collect::<Vec<_>>();
    let syntax = members
        .iter()
        .map(|(open, logical_path)| (*logical_path, syntax_report(open)))
        .collect::<Vec<_>>();
    let open_modules = members
        .iter()
        .zip(&syntax)
        .map(|((open, logical_path), (_, report))| OpenModule {
            logical_path,
            text: &open.document.text,
            has_syntax_errors: !report.diagnostics.is_empty(),
        })
        .collect::<Vec<_>>();
    let mut checked = project::check(project, &open_modules).diagnostics;
    let mut reports = Vec::new();
    for (logical_path, mut report) in syntax {
        report
            .diagnostics
            .extend(checked.remove(logical_path).unwrap_or_default());
        report.root = Some(root.clone());
        reports.push(report);
    }
    for (logical_path, diagnostics) in checked {
        if let Some(uri) = project.module_uri(&logical_path) {
            reports.push(Report {
                uri,
                version: None,
                diagnostics,
                root: Some(root.clone()),
            });
        }
    }
    reports.sort_by(|left, right| left.uri.as_str().cmp(right.uri.as_str()));
    reports
}

/// Sends `textDocument/publishDiagnostics` notifications to the client.
pub(crate) struct Publisher(Box<dyn Fn(Message) + Send>);

impl Publisher {
    pub(crate) fn new(send: impl Fn(Message) + Send + 'static) -> Self {
        Self(Box::new(send))
    }

    fn publish(&self, report: &Report) {
        (self.0)(Message::Notification(Notification {
            method: "textDocument/publishDiagnostics".to_owned(),
            params: serde_json::to_value(PublishDiagnosticsParams {
                uri: report.uri.clone(),
                diagnostics: report.diagnostics.clone(),
                version: report.version,
            })
            .expect("serialisable diagnostics"),
        }));
    }
}

/// A document whose open, change, save or close caused a diagnostics job,
/// with the project it belonged to at that moment.
pub(crate) type Trigger = (Uri, Option<ProjectHome>);

struct Job {
    snapshot: Snapshot,
    /// Always published, even when their diagnostics did not change.
    triggers: Vec<Trigger>,
}

/// Owns the diagnostics thread. Dropping it stops the thread after the
/// queued work.
pub(crate) struct DiagnosticsWorker {
    jobs: Option<Sender<Job>>,
    thread: Option<JoinHandle<()>>,
}

impl DiagnosticsWorker {
    pub(crate) fn spawn(publisher: Publisher) -> Self {
        let (jobs, receiver) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("OrnaLspDiagnostics".to_owned())
            .spawn(move || run(&receiver, &publisher))
            .expect("spawn Orna LSP diagnostics thread");
        Self {
            jobs: Some(jobs),
            thread: Some(thread),
        }
    }

    /// Queues a check of `snapshot` that publishes at least `triggers`.
    pub(crate) fn schedule(&self, snapshot: Snapshot, triggers: Vec<Trigger>) {
        if let Some(jobs) = &self.jobs {
            let _ = jobs.send(Job { snapshot, triggers });
        }
    }
}

impl Drop for DiagnosticsWorker {
    fn drop(&mut self) {
        drop(self.jobs.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// What each URI last received, and the project root it was published for.
type Published = HashMap<Uri, (Vec<Diagnostic>, Option<PathBuf>)>;

fn run(receiver: &Receiver<Job>, publisher: &Publisher) {
    let mut published = Published::new();
    while let Ok(mut job) = receiver.recv() {
        while let Ok(newer) = receiver.try_recv() {
            job.triggers.extend(newer.triggers);
            job.snapshot = newer.snapshot;
        }
        for report in reports_for(&job, &published) {
            publisher.publish(&report);
            if report.diagnostics.is_empty() && report.version.is_none() {
                published.remove(&report.uri);
            } else {
                published.insert(report.uri, (report.diagnostics, report.root));
            }
        }
    }
}

/// The reports one job publishes: every trigger, plus every other URI whose
/// diagnostics changed in the projects the triggers belong to.
fn reports_for(job: &Job, published: &Published) -> Vec<Report> {
    let is_open = |uri: &Uri| job.snapshot.iter().any(|file| &file.document.uri == uri);
    let mut projects = BTreeMap::<PathBuf, Arc<Project>>::new();
    let mut reports = Vec::<Report>::new();
    for (uri, home) in &job.triggers {
        if let Some(home) = home {
            projects.insert(home.project.root().to_path_buf(), Arc::clone(&home.project));
        } else if !reports.iter().any(|report| &report.uri == uri) {
            reports.push(
                match job.snapshot.iter().find(|file| &file.document.uri == uri) {
                    Some(file) => syntax_report(file),
                    None => cleared(uri, None),
                },
            );
        }
    }
    for (root, project) in projects {
        let current = project_reports(&job.snapshot, &project);
        for (uri, (_, published_root)) in published {
            if published_root.as_deref() == Some(root.as_path())
                && !is_open(uri)
                && !current.iter().any(|report| &report.uri == uri)
            {
                reports.push(cleared(uri, Some(root.clone())));
            }
        }
        for report in current {
            let triggered = job.triggers.iter().any(|(uri, _)| uri == &report.uri);
            let changed = published
                .get(&report.uri)
                .is_none_or(|(diagnostics, _)| diagnostics != &report.diagnostics);
            if triggered || changed {
                reports.push(report);
            }
        }
        // A closed module with no diagnostics left is cleared explicitly.
        for (uri, home) in &job.triggers {
            let in_project = home
                .as_ref()
                .is_some_and(|home| home.project.root() == root.as_path());
            if in_project && !is_open(uri) && !reports.iter().any(|report| &report.uri == uri) {
                reports.push(cleared(uri, Some(root.clone())));
            }
        }
    }
    reports
}

fn cleared(uri: &Uri, root: Option<PathBuf>) -> Report {
    Report {
        uri: uri.clone(),
        version: None,
        diagnostics: Vec::new(),
        root,
    }
}
