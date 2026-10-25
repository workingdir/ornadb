//! The workspace a request sees, built from the open documents and, for a
//! project, its saved modules, with caches shared by every request.
//!
//! Open documents are analysed once per version (`OpenDocument`). Closed
//! project modules are analysed once per content, and a project's captured
//! standard library once per profile digest. A whole workspace is reused
//! until a document of its project (or, for detached documents, any detached
//! document) opens, changes, saves or closes.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use lsp_types::Uri;
use orna_project_v1::{LoadedProject, ProjectLoader};

use crate::documents::Document;
use crate::project::Project;
use crate::standard::StandardLibrary;
use crate::workspace::{FileAnalysis, Module, OpenDocument, Workspace};

/// The open documents, by URI.
pub(crate) type Documents = HashMap<Uri, Arc<OpenDocument>>;

/// Which open documents a cached workspace was built from.
type Fingerprint = Vec<(String, i32)>;

#[derive(Default)]
pub(crate) struct Views {
    /// Closed project modules by path: content hash and analysis.
    closed: Mutex<HashMap<PathBuf, (u64, Arc<FileAnalysis>)>>,
    /// Captured standard libraries by the revision digest of their profile.
    standard: Mutex<HashMap<[u8; 32], Arc<StandardLibrary>>>,
    projects: Mutex<HashMap<PathBuf, (Fingerprint, Arc<Workspace>)>>,
    detached: Mutex<Option<(Fingerprint, Arc<Workspace>)>>,
}

impl Views {
    /// The workspace that contains the open document `uri`, and its module.
    pub(crate) fn workspace(
        &self,
        documents: &Documents,
        uri: &Uri,
    ) -> Option<(Arc<Workspace>, usize)> {
        let document = documents.get(uri)?;
        let workspace = match &document.home {
            Some(home) => self.project_workspace(documents, &home.project),
            None => self.detached_workspace(documents),
        };
        let module = workspace.module_of(uri)?;
        Some((workspace, module))
    }

    /// The workspace that contains `uri`, which may also be a closed module
    /// of a project with an open document, as hierarchy items can name one.
    pub(crate) fn workspace_containing(
        &self,
        documents: &Documents,
        uri: &Uri,
    ) -> Option<(Arc<Workspace>, usize)> {
        if documents.contains_key(uri) {
            return self.workspace(documents, uri);
        }
        let mut searched = Vec::<&Arc<Project>>::new();
        for document in documents.values() {
            let Some(home) = &document.home else {
                continue;
            };
            if searched
                .iter()
                .any(|project| Arc::ptr_eq(project, &home.project))
            {
                continue;
            }
            searched.push(&home.project);
            let workspace = self.project_workspace(documents, &home.project);
            if let Some(module) = workspace.module_of(uri) {
                return Some((workspace, module));
            }
        }
        None
    }

    /// Drops cached workspaces after a saved file may have changed on disk.
    pub(crate) fn invalidate(&self) {
        lock(&self.projects).clear();
        *lock(&self.detached) = None;
    }

    fn detached_workspace(&self, documents: &Documents) -> Arc<Workspace> {
        let mut members = documents
            .values()
            .filter(|document| document.home.is_none() && is_orna(&document.document.uri))
            .collect::<Vec<_>>();
        members.sort_by(|left, right| left.document.uri.as_str().cmp(right.document.uri.as_str()));
        let fingerprint = fingerprint(&members);
        let mut cache = lock(&self.detached);
        if let Some((cached, workspace)) = cache.as_ref()
            && cached == &fingerprint
        {
            return Arc::clone(workspace);
        }
        let workspace = Arc::new(Workspace::detached(
            members
                .iter()
                .map(|document| Arc::clone(document.analysis()))
                .collect(),
        ));
        *cache = Some((fingerprint, Arc::clone(&workspace)));
        workspace
    }

    fn project_workspace(&self, documents: &Documents, project: &Arc<Project>) -> Arc<Workspace> {
        let mut members = documents
            .values()
            .filter(|document| {
                document
                    .home
                    .as_ref()
                    .is_some_and(|home| Arc::ptr_eq(&home.project, project))
            })
            .collect::<Vec<_>>();
        members.sort_by(|left, right| left.document.uri.as_str().cmp(right.document.uri.as_str()));
        let fingerprint = fingerprint(&members);
        let root = project.root().to_path_buf();
        if let Some((cached, workspace)) = lock(&self.projects).get(&root)
            && cached == &fingerprint
        {
            return Arc::clone(workspace);
        }
        let workspace = Arc::new(self.build_project(project, &members));
        lock(&self.projects).insert(root, (fingerprint, Arc::clone(&workspace)));
        workspace
    }

    fn build_project(&self, project: &Project, open: &[&Arc<OpenDocument>]) -> Workspace {
        let open_by_path = open
            .iter()
            .filter_map(|document| {
                let home = document.home.as_ref()?;
                Some((home.logical_path.as_str(), *document))
            })
            .collect::<BTreeMap<_, _>>();
        let overlays = open_by_path
            .iter()
            .map(|(logical_path, document)| {
                ((*logical_path).to_owned(), document.document.text.clone())
            })
            .collect::<BTreeMap<_, _>>();
        let mut modules = Vec::new();
        let mut included = HashSet::new();
        let mut standard = StandardLibrary::none();
        let incomplete = match ProjectLoader::default()
            .load_with_overlays(project.repository(), &overlays)
        {
            Ok(loaded) => {
                standard = self.captured_standard(&loaded);
                for module in loaded.modules() {
                    let logical_path = module.logical_path.as_str();
                    let file = match open_by_path.get(logical_path) {
                        Some(document) => Arc::clone(document.analysis()),
                        None => match self.closed_analysis(project, logical_path, &module.source) {
                            Some(file) => file,
                            None => continue,
                        },
                    };
                    included.insert(logical_path.to_owned());
                    modules.push(Module::project(file, logical_path));
                }
                None
            }
            Err(error) => Some(error.to_string()),
        };
        // Open modules that the module graph does not reach still navigate.
        for (logical_path, document) in &open_by_path {
            if included.insert((*logical_path).to_owned()) {
                modules.push(Module::project(
                    Arc::clone(document.analysis()),
                    logical_path,
                ));
            }
        }
        Workspace::project(modules, standard, incomplete)
    }

    /// The standard dependency `loaded` captures, as `orna check` analyses it.
    fn captured_standard(&self, loaded: &LoadedProject) -> Arc<StandardLibrary> {
        let Some(profile) = loaded.standard_profile() else {
            return StandardLibrary::none();
        };
        let mut libraries = lock(&self.standard);
        Arc::clone(
            libraries
                .entry(profile.revision_digest())
                .or_insert_with(|| {
                    Arc::new(StandardLibrary::captured(profile, loaded.standard_sources()))
                }),
        )
    }

    fn closed_analysis(
        &self,
        project: &Project,
        logical_path: &str,
        source: &str,
    ) -> Option<Arc<FileAnalysis>> {
        let path = project.root().join(logical_path);
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        source.hash(&mut hasher);
        let hash = hasher.finish();
        let mut closed = lock(&self.closed);
        if let Some((cached, file)) = closed.get(&path)
            && *cached == hash
        {
            return Some(Arc::clone(file));
        }
        let uri = project.module_uri(logical_path)?;
        let file = Arc::new(FileAnalysis::new(Document::new(uri, source.to_owned(), 0)));
        closed.insert(path, (hash, Arc::clone(&file)));
        Some(file)
    }
}

fn fingerprint(documents: &[&Arc<OpenDocument>]) -> Fingerprint {
    documents
        .iter()
        .map(|document| {
            (
                document.document.uri.as_str().to_owned(),
                document.document.version,
            )
        })
        .collect()
}

fn is_orna(uri: &Uri) -> bool {
    uri.as_str().ends_with(".orna")
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}
