//! Object identities of renamed tables and fields.
//!
//! A table or field has an object identity in the repository's staged
//! identity inventory. Renaming it in the editor must carry that identity to
//! the new name, or the next commit would see a different object. The
//! workspace edit is returned to the client first and applied later, in one
//! step or several, and the user may decline or undo it. The identity rename
//! is therefore held here, per document, until a save shows which steps
//! reached the saved text.

use std::collections::HashMap;
use std::sync::Mutex;

use lsp_types::{Position, Uri};
use orna_project::{ObjectIdentityManifest, ObjectIdentityRename, ObjectKind, ProjectLoader};
use orna_repository::relation_admission::StagedCandidate;
use orna_repository::{Repository, RepositoryFormat};

use crate::documents::{Document, PositionMapper};
use crate::sync::lock;
use crate::workspace::DeclaredKind;
use crate::workspace::{Target, Workspace};

/// How far one rename of a chain has got.
enum Progress {
    /// Returned to the client; the document has not reached `expected_text`.
    Requested,
    /// The document reached `expected_text` at `version`.
    Applied { version: i32 },
}

/// One rename of a chain. A chain is the sequential renames of one object
/// (`A` to `B`, then `B` to `C`) made before a save.
struct Step {
    rename: ObjectIdentityRename,
    requested_version: i32,
    pre_edit_text: String,
    /// Exact full text produced by the returned WorkspaceEdit for this document.
    expected_text: String,
    progress: Progress,
}

impl Step {
    /// The version at which the document reached this step's text.
    fn applied_at(&self) -> Option<i32> {
        match self.progress {
            Progress::Requested => None,
            Progress::Applied { version } => Some(version),
        }
    }
}

/// Identity renames returned to the client and not yet saved, per document.
#[derive(Default)]
pub(crate) struct PendingRenames {
    by_document: Mutex<HashMap<Uri, Vec<Vec<Step>>>>,
}

impl PendingRenames {
    /// The table or field identity rename a rename request at `position` of
    /// `document` implies. `Ok(None)` when the file is outside a repository or the
    /// rename changes no identity.
    pub(crate) fn rename_for_request(
        &self,
        document: Option<&Document>,
        uri: &Uri,
        position: Position,
        new_name: &str,
    ) -> Result<Option<ObjectIdentityRename>, String> {
        let Some(document) = document else {
            return Ok(None);
        };
        let Ok(url) = url::Url::parse(uri.as_str()) else {
            return Ok(None);
        };
        let Ok(path) = url.to_file_path() else {
            return Ok(None);
        };
        // Outside a repository there is no identity store to keep.
        let Ok(repository) = Repository::discover(&path) else {
            return Ok(None);
        };
        let Ok(relative) = path.strip_prefix(repository.worktree()) else {
            return Ok(None);
        };
        let logical_path = relative.to_string_lossy().replace('\\', "/");
        let Some(old_name) = identifier_at(&document.text, position) else {
            return Ok(None);
        };
        let namespace = module_namespace(&logical_path);
        let qualify = |name: &str| {
            if namespace.is_empty() {
                name.to_owned()
            } else {
                format!("{namespace}.{name}")
            }
        };
        let table = qualify(&old_name);
        // The latest rename of each chain. Copied out so the lock is not held
        // across the repository reads below, which would stall the edits that
        // `mark_applied` records on the server loop.
        let pending = self.latest_renames(uri);

        if pending.iter().any(|rename| {
            rename.kind() == ObjectKind::Table && rename.new_qualified_name() == table
        }) {
            return Ok(Some(ObjectIdentityRename::new(
                ObjectKind::Table,
                table,
                qualify(new_name),
            )));
        }

        let suffix = format!(".{old_name}");
        let pending_fields = pending
            .iter()
            .filter(|rename| {
                let target = rename.new_qualified_name();
                rename.kind() == ObjectKind::Field
                    && target.ends_with(&suffix)
                    && target[..target.len() - suffix.len()].starts_with(&namespace)
            })
            .collect::<Vec<_>>();
        if pending_fields.len() > 1 {
            return Err(format!(
                "the field name {old_name} matches several pending renames"
            ));
        }
        if let Some(rename) = pending_fields.first() {
            let current = rename.new_qualified_name();
            let parent = &current[..current.len() - old_name.len()];
            return Ok(Some(ObjectIdentityRename::new(
                ObjectKind::Field,
                current,
                format!("{parent}{new_name}"),
            )));
        }

        let (_, previous) = staged_identities(&repository)?;
        if previous.id(ObjectKind::Table, &table).is_some() {
            return Ok(Some(ObjectIdentityRename::new(
                ObjectKind::Table,
                table,
                qualify(new_name),
            )));
        }
        let mut matches = previous.records().iter().filter(|record| {
            !record.is_retired()
                && record.kind() == ObjectKind::Field
                && record.qualified_name().ends_with(&suffix)
                && record.qualified_name()[..record.qualified_name().len() - suffix.len()]
                    .starts_with(&namespace)
        });
        let Some(record) = matches.next() else {
            return Ok(None);
        };
        if matches.next().is_some() {
            return Err(format!("the field name {old_name} matches several fields"));
        }
        let parent = &record.qualified_name()[..record.qualified_name().len() - old_name.len()];
        Ok(Some(ObjectIdentityRename::new(
            ObjectKind::Field,
            record.qualified_name().to_owned(),
            format!("{parent}{new_name}"),
        )))
    }

    /// The latest rename of each chain pending for `uri`.
    fn latest_renames(&self, uri: &Uri) -> Vec<ObjectIdentityRename> {
        lock(&self.by_document)
            .get(uri)
            .map(|chains| {
                chains
                    .iter()
                    .filter_map(|chain| chain.last().map(|step| step.rename.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Holds `rename` until a save. `document` is what the rename was
    /// requested on and `expected_text` the text its workspace edit produces.
    /// A rename that starts from the text the last rename of a chain
    /// produced continues that chain; any other starts a new one.
    pub(crate) fn stage(
        &self,
        uri: &Uri,
        rename: ObjectIdentityRename,
        document: &Document,
        expected_text: String,
    ) {
        let requested_version = document.version;
        let pre_edit_text = document.text.clone();
        let mut by_document = lock(&self.by_document);
        let pending = by_document.entry(uri.clone()).or_default();
        let continues = pending.iter().position(|chain| {
            chain.last().is_some_and(|last| {
                last.rename.kind() == rename.kind()
                    && last.rename.new_qualified_name() == rename.old_qualified_name()
                    && last.expected_text == pre_edit_text
            })
        });
        let step = Step {
            rename,
            requested_version,
            pre_edit_text,
            expected_text,
            progress: Progress::Requested,
        };
        match continues {
            Some(index) => pending[index].push(step),
            None => pending.push(vec![step]),
        }
    }

    /// Records that `uri` reached `text` at `version`: every step whose
    /// edit produces that text, and was requested at an earlier version, is
    /// now applied.
    pub(crate) fn mark_applied(&self, uri: &Uri, version: i32, text: &str) {
        let mut by_document = lock(&self.by_document);
        let Some(chains) = by_document.get_mut(uri) else {
            return;
        };
        for step in chains.iter_mut().flatten() {
            if version > step.requested_version && text == step.expected_text {
                step.progress = Progress::Applied { version };
            }
        }
    }

    /// Forgets the renames of a closed or re-opened document. `true` when one
    /// had been applied to the buffer but never saved, because its identity is
    /// then not carried; a declined rename is dropped silently.
    pub(crate) fn discard(&self, uri: &Uri) -> bool {
        let removed = lock(&self.by_document).remove(uri);
        removed.is_some_and(|chains| {
            chains
                .iter()
                .flatten()
                .any(|step| step.applied_at().is_some())
        })
    }

    /// Saves the identity renames the saved source applies, into the staged store.
    /// `open` is the open document as the editor holds it. Every pending rename
    /// that cannot be carried is returned as an error for the caller to report;
    /// the saved text is saved either way.
    pub(crate) fn persist(&self, uri: &Uri, open: Option<&Document>) -> Result<(), String> {
        let Some(chains) = lock(&self.by_document).remove(uri) else {
            return Ok(());
        };
        let url = url::Url::parse(uri.as_str()).map_err(|error| format!("{error:?}"))?;
        let path = url
            .to_file_path()
            .map_err(|()| format!("{uri:?} is not a file"))?;
        let Some(open) = open else {
            return Err("the document is no longer open".to_owned());
        };
        let saved_version = open.version;
        let saved_text = std::fs::read_to_string(&path).map_err(|error| format!("{error:?}"))?;
        if saved_text != open.text {
            return Err("the saved text differs from the editor buffer".to_owned());
        }
        let repository = Repository::discover(&path).map_err(|error| format!("{error:?}"))?;
        let project = ProjectLoader::default()
            .load(&repository)
            .map_err(|error| format!("{error:?}"))?;
        let modules = project.into_modules();
        let (staged, previous) = staged_identities(&repository)?;
        // Intermediate destinations are not in the saved source, so the furthest
        // applied step that reconciles against it is the one saved.
        // `Ok(None)`: no step of the chain reached the saved text, so the rename was
        // declined and there is no identity to carry.
        let rename_for = |chain: &[Step]| -> Result<Option<ObjectIdentityRename>, String> {
            let Some(first) = chain.first() else {
                return Ok(None);
            };
            let applied = chain
                .iter()
                .take_while(|step| {
                    step.applied_at()
                        .is_some_and(|version| version <= saved_version)
                })
                .collect::<Vec<_>>();
            if applied.is_empty() {
                return Ok(None);
            }
            let mut reason = format!(
                "the saved source does not show the rename of {}",
                first.rename.old_qualified_name()
            );
            for step in applied.into_iter().rev() {
                let rename = ObjectIdentityRename::new(
                    first.rename.kind(),
                    first.rename.old_qualified_name(),
                    step.rename.new_qualified_name(),
                );
                match previous.with_renames(&modules, std::slice::from_ref(&rename)) {
                    Ok(_) => return Ok(Some(rename)),
                    Err(error) => reason = format!("{error:?}"),
                }
            }
            Err(reason)
        };
        let mut applicable = Vec::new();
        let mut failures = Vec::new();
        for chain in &chains {
            match rename_for(chain) {
                // Field renames change the committed schema, which this build does
                // not admit (ORNA-EVOL-002), so nothing is staged for them.
                Ok(Some(rename)) if rename.kind() == ObjectKind::Field => {
                    failures.push("schema evolution is not admitted".to_owned());
                }
                Ok(Some(rename)) => applicable.push(rename),
                Ok(None) => {}
                Err(reason) => failures.push(reason),
            }
        }
        if !applicable.is_empty() {
            let relative = path
                .strip_prefix(repository.worktree())
                .map_err(|error| format!("{error:?}"))?;
            let logical_path = relative.to_string_lossy().replace('\\', "/");
            // The save stages the whole file, so it is only written when the index
            // entry is still HEAD's or the text this edit started from; otherwise
            // partial staging the user made would be overwritten.
            let index = repository
                .index_file_bytes(&logical_path)
                .map_err(|error| format!("{error:?}"))?;
            let head = repository
                .head_file_bytes(&logical_path)
                .map_err(|error| format!("{error:?}"))?;
            let clean = index == head
                || chains.iter().any(|chain| {
                    chain
                        .first()
                        .is_some_and(|step| index.as_deref() == Some(step.pre_edit_text.as_bytes()))
                });
            if clean {
                let next = applicable.iter().try_fold(previous, |manifest, rename| {
                    manifest
                        .with_renames(&modules, std::slice::from_ref(rename))
                        .map_err(|error| format!("{error:?}"))
                })?;
                repository
                    .stage_object_identities(
                        &staged,
                        &next.to_stored(),
                        &[(logical_path.as_str(), saved_text.as_bytes())],
                    )
                    .map_err(|error| format!("{error:?}"))?;
            } else {
                failures.push(format!(
                    "index entry for {logical_path} differs from HEAD; stage manually"
                ));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }
}

/// Captures the staged candidate and reads the identity inventory it carries,
/// refusing a store that is not retained, so renames never write to an
/// unverified store.
fn staged_identities(
    repository: &Repository,
) -> Result<(StagedCandidate, ObjectIdentityManifest), String> {
    repository
        .refuse_unretained_committed_store()
        .map_err(|error| format!("{error:?}"))?;
    require_identity_format(repository)?;
    let staged = repository
        .capture_staged_candidate()
        .map_err(|error| format!("{error:?}"))?;
    let identities = ObjectIdentityManifest::read_staged(repository, &staged)
        .map_err(|error| format!("{error:?}"))?
        .ok_or_else(|| "the staged object identities are missing".to_owned())?;
    Ok((staged, identities))
}

/// Object identities exist only in format 3. Older repositories refuse the
/// carry rather than renaming without an identity.
fn require_identity_format(repository: &Repository) -> Result<(), String> {
    let format = repository
        .open_format_context()
        .map_err(|error| format!("{error:?}"))?;
    if format.repository_format() == RepositoryFormat::Format3 {
        Ok(())
    } else {
        Err("this repository uses a legacy format without object identities".to_owned())
    }
}

/// Whether a rename of `target` concerns an object identity. Only declared
/// tables and fields have one; local names, functions and modules do not.
pub(crate) fn carries_identity(workspace: &Workspace, target: &Target) -> bool {
    match target {
        Target::Declared { module, index } => matches!(
            workspace.declaration(*module, *index).kind,
            DeclaredKind::Table
                | DeclaredKind::Field
                | DeclaredKind::KeyField
                | DeclaredKind::PayloadField
        ),
        _ => false,
    }
}

/// The warning for an object identity that a rename or save did not carry.
/// The text is already changed, so the remedy is `orna mv`.
pub(crate) fn identity_not_carried(reason: &str) -> String {
    format!(
        "ORNA-LSP-RENAME-001: object identity was not carried ({reason}); use orna mv to rename a table with its identity"
    )
}

fn module_namespace(path: &str) -> String {
    let mut parts = path
        .strip_suffix(".orna")
        .unwrap_or(path)
        .split('/')
        .collect::<Vec<_>>();
    let file = parts.pop().unwrap_or_default();
    if file != "main" {
        parts.push(file);
    }
    parts.join(".")
}

fn identifier_at(source: &str, position: Position) -> Option<String> {
    let mapper = PositionMapper::new(source);
    let byte = mapper.byte_offset(position).min(source.len());
    let start = source[..byte]
        .char_indices()
        .rev()
        .find_map(|(index, ch)| (!is_identifier_char(ch)).then_some(index + ch.len_utf8()))
        .unwrap_or(0);
    let end = source[byte..]
        .char_indices()
        .find_map(|(offset, ch)| (!is_identifier_char(ch)).then_some(byte + offset))
        .unwrap_or(source.len());
    let identifier = source.get(start..end)?;
    (!identifier.is_empty()).then(|| identifier.to_owned())
}

fn is_identifier_char(ch: char) -> bool {
    ch == '_' || ch.is_alphanumeric()
}
