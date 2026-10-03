//! Snapshot-pinned database attachments and repository-resolved packages.
//!
//! A session reads each database from one exact Git commit. The small
//! `.orna/packages` file is committed with the parent snapshot and contains
//! only `name <commit-oid>` rows; hosts map those names to repositories they
//! already have open. This keeps historical resolution deterministic without
//! adding a registry, semantic-version solver, or separate lockfile.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
    path::Path,
    sync::Arc,
};

use orna_repository_v1::{
    CommittedTreeEntryKind, GitCommitRef, Repository, RepositoryError,
};
use orna_semantic_v1::ModuleInput;
use orna_syntax_v1::{Declaration, Keyword, TokenKind, lex, parse_module};

use crate::{LoadedProject, LooseRowCandidate, ProjectLoadError, ProjectLoader};

/// The committed file containing exact package attachment pins.
pub const PACKAGE_PIN_MANIFEST_PATH: &str = ".orna/packages";
const MAX_PACKAGE_PIN_MANIFEST_BYTES: usize = 64 * 1024;
const MAX_PACKAGE_PINS: usize = 256;
const MAX_PACKAGE_PARENT_TREE_ENTRIES: usize = 4_096;

/// One unverified exact commit row from `.orna/packages`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackagePinSpec {
    name: String,
    object_id: String,
}

impl PackagePinSpec {
    /// Package or database alias recorded in the parent snapshot.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Exact native Git object ID recorded in the parent snapshot.
    pub fn object_id(&self) -> &str {
        &self.object_id
    }
}

/// Parsed, bounded package pins from one immutable parent snapshot.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PackagePinManifest {
    pins: Vec<PackagePinSpec>,
}

impl PackagePinManifest {
    /// Parses rows of `name <40-or-64-character-lowercase-git-oid>`.
    /// Blank lines and whole-line comments are ignored.
    pub fn parse(source: &str) -> Result<Self, AttachmentError> {
        if source.len() > MAX_PACKAGE_PIN_MANIFEST_BYTES {
            return Err(AttachmentError::ManifestTooLarge);
        }
        let mut pins = Vec::new();
        let mut names = BTreeSet::new();
        for line in source.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut fields = line.split_ascii_whitespace();
            let Some(name) = fields.next() else {
                continue;
            };
            let Some(object_id) = fields.next() else {
                return Err(AttachmentError::MalformedManifest);
            };
            if fields.next().is_some()
                || !valid_attachment_name(name)
                || name == "sys"
                || !valid_full_git_oid(object_id)
                || !names.insert(name.to_owned())
            {
                return Err(AttachmentError::MalformedManifest);
            }
            pins.push(PackagePinSpec {
                name: name.to_owned(),
                object_id: object_id.to_owned(),
            });
            if pins.len() > MAX_PACKAGE_PINS {
                return Err(AttachmentError::TooManyPackages);
            }
        }
        Ok(Self { pins })
    }

    /// Reads the manifest from the exact committed parent snapshot.
    pub fn load(
        repository: &Repository,
        parent: &GitCommitRef,
    ) -> Result<Self, AttachmentError> {
        let entries = repository
            .list_committed_tree(parent, MAX_PACKAGE_PARENT_TREE_ENTRIES)
            .map_err(AttachmentError::Repository)?;
        let path = Path::new(PACKAGE_PIN_MANIFEST_PATH);
        let manifest = entries.iter().find(|entry| entry.path().as_path() == path);
        if let Some(entry) = manifest {
            if !matches!(entry.kind(), CommittedTreeEntryKind::File { .. }) {
                return Err(AttachmentError::MalformedManifest);
            }
        } else if entries
            .iter()
            .any(|entry| entry.path().as_path().starts_with(path))
        {
            // `.orna/packages` is a file boundary. A directory below that
            // name is malformed rather than an empty optional dependency set.
            return Err(AttachmentError::MalformedManifest);
        } else {
            // The reference makes `std` and other packages optional, but does
            // not prescribe a manifest format. In v1, omitting our exact-pin
            // manifest means the parent has no package attachments.
            return Ok(Self::default());
        }
        let bytes = repository
            .read_committed_file(parent, PACKAGE_PIN_MANIFEST_PATH, MAX_PACKAGE_PIN_MANIFEST_BYTES)
            .map_err(AttachmentError::Repository)?;
        let source = std::str::from_utf8(&bytes).map_err(|_| AttachmentError::MalformedManifest)?;
        Self::parse(source)
    }

    /// Exact pins in their committed order.
    pub fn pins(&self) -> &[PackagePinSpec] {
        &self.pins
    }
}

/// A package/database identity resolved to one immutable Git commit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackagePin {
    name: String,
    commit: GitCommitRef,
}

impl PackagePin {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn commit(&self) -> &GitCommitRef {
        &self.commit
    }
}

/// A database loaded entirely from its pinned commit.
#[derive(Clone, Debug)]
pub struct PinnedDatabase {
    pin: PackagePin,
    repository: Repository,
    project: LoadedProject,
}

impl PinnedDatabase {
    /// Resolves a selector once, then loads the resulting immutable commit.
    /// A branch or tag therefore never remains live inside the session.
    pub fn resolve(
        name: impl Into<String>,
        repository: Repository,
        selector: &str,
        loader: ProjectLoader,
    ) -> Result<Self, AttachmentError> {
        let name = checked_name(name.into())?;
        let commit = repository
            .resolve_snapshot(selector)
            .map_err(AttachmentError::Repository)?;
        Self::load_at_commit(name, repository, commit, loader)
    }

    fn load_at_commit(
        name: String,
        repository: Repository,
        commit: GitCommitRef,
        loader: ProjectLoader,
    ) -> Result<Self, AttachmentError> {
        let project = loader
            .load_committed_snapshot(&repository, &commit)
            .map_err(AttachmentError::Project)?;
        Ok(Self {
            pin: PackagePin { name, commit },
            repository,
            project,
        })
    }

    pub fn pin(&self) -> &PackagePin {
        &self.pin
    }

    pub fn project(&self) -> &LoadedProject {
        &self.project
    }
}

/// Resolves aliases in a committed pin manifest against repositories already
/// opened by the host. It does not discover repositories from ambient paths.
#[derive(Clone, Debug)]
pub struct PackageResolver {
    repositories: BTreeMap<String, Repository>,
    loader: ProjectLoader,
}

impl PackageResolver {
    /// Builds the host's alias-to-repository mapping.
    pub fn new(
        repositories: impl IntoIterator<Item = (String, Repository)>,
        loader: ProjectLoader,
    ) -> Result<Self, AttachmentError> {
        let mut resolved = BTreeMap::new();
        for (name, repository) in repositories {
            let name = checked_name(name)?;
            if name == "sys" || resolved.insert(name, repository).is_some() {
                return Err(AttachmentError::DuplicateRepository);
            }
        }
        Ok(Self {
            repositories: resolved,
            loader,
        })
    }

    /// Loads the exact attachments declared by `primary`'s pinned snapshot.
    /// Parent history is authoritative: later edits to the primary manifest
    /// cannot move this session to newer package commits. Resolution is
    /// all-or-nothing: a failed pin never returns a partially attached session.
    /// Nested closure is one parent manifest per call; pass a selected attached
    /// database back to this method to resolve the next edge from its own pin.
    /// Each manifest alias is a complete lookup key, so prefix-related aliases
    /// and aliases sharing a repository remain independent. The reference does
    /// not define host mapping precedence for those aliases; v1 uses exact-key
    /// lookup for every selected parent's historical manifest.
    /// After a caller rebinds an alias, selecting that replacement here loads
    /// its own committed manifest. Sibling aliases remain pinned in the source
    /// session and do not retarget the replacement's closure. Each replacement
    /// starts a new resolution branch: aliases from the prior parent's
    /// manifest are not inherited or used as fallback at the next depth.
    /// If an alias is rebound through several candidates before nested
    /// expansion, the currently attached pin supplies that session's next
    /// manifest; earlier candidates are not accumulated. A caller can retain a
    /// candidate pin before rebinding again. It remains immutable, and
    /// explicitly resolving it later reads its own manifest after the source
    /// session has moved on. Since the reference is silent on storm history,
    /// v1 treats the explicitly passed pin as the caller's closure choice.
    /// Repeating detach/attach runs on one session replaces only the selected
    /// exact alias in sequence. At the end of each run, that session's current
    /// pin alone supplies the next manifest; previous run candidates remain
    /// selectable only when the caller kept their pins separately. The
    /// reference does not define how repeated runs compose, so v1 uses the
    /// last attached pin for the session's current exact alias.
    ///
    /// Rebinding prefix-related aliases in one session keeps them as
    /// independent exact-key entries. The session retains every unmodified
    /// key, including longer prefix-related siblings. Expanding one replacement
    /// uses only its manifest to create a fresh next-depth session, even when
    /// that manifest names an alias matching a retained sibling. The reference
    /// is silent on precedence across rebound alias chains; v1 makes the
    /// selected parent's committed manifest authoritative at that depth. The
    /// reference requires exact pins for each historical parent but does not
    /// require flattening a recursive closure into one session.
    /// Apply the same precedence independently at every edge of a longer
    /// chain: rebinding a descendant affects only that descendant session, and
    /// the next closure uses its final selected pin's manifest. Since the
    /// reference is silent on repeated storms across nested chains, v1 does
    /// not overlay manifests from earlier candidates or ancestor sessions.
    /// A pin retained from an earlier outer storm can still be expanded after
    /// later storms; its own manifest starts that closure branch. Rebinding a
    /// descendant in one expanded branch cannot retarget another retained
    /// branch, and expanding a selected descendant uses only that pin's
    /// manifest rather than carrying forward the ancestor's prefix siblings.
    /// This keeps precedence stable across repeated storms at multiple closure
    /// depths, including when those storms finish in different candidates.
    /// If the caller returns to an ancestor session after rebinding descendants,
    /// a later expansion follows the ancestor pin selected at that time. A
    /// closure already expanded from a retained pin remains its own branch;
    /// descendant storms do not become fallback pins for a newly selected
    /// ancestor candidate. Where the reference is silent, v1 gives precedence
    /// to the exact pin passed to each closure expansion.
    /// A storm cascade is resolved edge by edge from that pin: selecting a new
    /// ancestor starts a fresh descendant chain, and the terminal route follows
    /// the final pin chosen at every edge. Pins retained from an earlier
    /// cascade continue to resolve their own terminal routes.
    /// After expanding the terminal parent, rebinding its exact terminal alias
    /// changes only that closure's route to the latest attached terminal pin.
    /// Earlier terminal candidates and the manifest-selected snapshot remain
    /// independently resolvable; v1 applies the same last-pin rule at this
    /// final edge when the reference does not specify terminal rebind storms.
    /// Apply that rule separately to every retained closure branch: a terminal
    /// alias storm follows the exact terminal pin selected through that
    /// branch's ancestor manifests, and cannot retarget a same-named terminal
    /// route in another branch. Reopening a retained ancestor starts its own
    /// terminal route chain from that pin's manifest.
    /// Expanding one retained parent pin more than once also creates sibling
    /// closure branches: storms at paired nested depths remain local to each
    /// session, and each terminal route follows only its branch's selected
    /// pins.
    /// A sibling expanded later from the retained ancestor still starts at
    /// that ancestor's manifest-selected child; an earlier sibling's terminal
    /// rebind does not supply a fallback for the late branch.
    /// This remains true through additional nested edges: a late branch reads
    /// each selected middle and deep manifest in turn, then applies terminal
    /// alias storms only to the route reached through those exact pins.
    /// A middle pin retained before a sibling rebind is also an independent
    /// late-branch root: reopening it follows its own deep manifest and
    /// terminal route rather than inheriting the sibling's later selections.
    /// Sibling manifests may converge on the same exact deep and terminal
    /// pins; rebinding that terminal alias in one closure changes only that
    /// closure's route, while another retained branch can still resolve the
    /// shared manifest-selected terminal pin.
    /// Distinct sibling closures expanded before any terminal rebind also
    /// keep independent route state when their terminal storms are interleaved.
    /// Each session follows only its own last attached terminal pin, and a
    /// fresh expansion from their shared deep pin still starts at the terminal
    /// pin recorded in that deep manifest. The reference is silent on shared
    /// terminal route storms, so v1 applies the exact-alias last-pin rule per
    /// session.
    /// Paired sibling branches may also storm their middle and deep aliases
    /// independently before reaching a shared terminal pin. Each terminal
    /// closure still begins at the exact pin in its selected deep manifest;
    /// interleaving later terminal storms cannot change the other branch or a
    /// fresh expansion from either retained deep pin.
    /// Sibling middle pins that converge on one exact deep pin can then take
    /// independent deep-rebind storms whose candidates select the same
    /// terminal pin. Terminal closures opened from those selected deep pins
    /// still begin on that shared route; later terminal rebinds remain local
    /// to each sibling closure.
    /// This holds across a wider sibling set as well: when several middle
    /// routes converge on one deep pin, each branch can take its own paired
    /// depth and terminal storms while retaining the exact shared route from
    /// each selected pin's manifest.
    /// Since the reference does not define an event order across sibling
    /// sessions, v1 makes independent storms order-stable: interleaving or
    /// reversing operations across branches leaves each branch at its own
    /// last selected pin on every edge.
    /// When distinct sibling middle pins converge on one deep pin, their
    /// terminal closures may also converge after independent rebind storms.
    /// Each closure still resolves its terminal edge from the exact selected
    /// deep pin, and a terminal session retained before rebinding keeps the
    /// manifest-selected route. Since the reference is silent on this
    /// reconvergent storm case, v1 keeps convergence based on exact pin
    /// identity while preserving each session's prior route snapshot.
    /// Reopening the retained sibling middle pins after those storms starts
    /// each closure again from its committed manifest. A later rebind wave on
    /// the new closures remains local and may converge on the same exact
    /// terminal pin; previously retained storm sessions keep their selections.
    /// The reference does not define this post-storm reopen sequence, so v1
    /// treats each newly expanded parent pin as a fresh route root.
    /// A terminal rebind in one sibling may interleave with a later paired
    /// middle/deep rebind in another. The new terminal closure follows only
    /// its selected deep pin's manifest, while the first sibling keeps its
    /// own last terminal pin. Since the reference is silent on cross-depth
    /// event order, v1 preserves each closure's exact-pin route independently.
    /// If another paired middle/deep rebind wave follows that reopen, each
    /// new terminal closure still starts from its selected deep pin's
    /// manifest, regardless of terminal pins retained from earlier waves.
    /// Siblings whose selected deep manifests name the same terminal pin can
    /// then converge again through local terminal rebinds. The reference is
    /// silent on repeated post-storm depth waves; v1 keeps each wave rooted
    /// in the exact pins selected along its own path.
    /// After an earlier terminal storm across several siblings, a later
    /// paired middle/deep closure wave also starts from each sibling's newly
    /// selected middle pin, then follows that middle pin's exact deep pin.
    /// Rebinding those reopened terminal closures can converge on one exact
    /// terminal pin without merging their state: prior storm sessions and
    /// pre-rebind snapshots retain their original routes. Since the reference
    /// is silent on this wider post-storm sequence, v1 treats every reopened
    /// closure as a fresh route root and keeps rebinds local to that session.
    /// If a later paired middle/deep rebind selects different child pins for
    /// sibling roots, each new deep closure follows the exact middle
    /// manifest it was opened from. Rebinding its deep alias affects only
    /// that closure; retained pre-storm and manifest-selected snapshots keep
    /// their own pins. The reference is silent on diverging post-storm child
    /// routes, so v1 resolves each edge from its selected immutable pin and
    /// does not inherit a sibling's prior route.
    /// Route consistency also applies to retained sessions: each attached
    /// module resolves through the same package snapshot as that session's
    /// pin. A paired depth rebind can make a fresh descendant closure follow
    /// another manifest route, while the old closure keeps its matching pin
    /// and module route. Since the reference does not define cross-session
    /// route refresh, v1 keeps pin and module routing in the same session
    /// snapshot.
    /// Applying the same paired depth rebinds to sibling sessions in another
    /// interleaving produces the same per-sibling pin and module-route pairs.
    /// v1 makes independent sibling route updates order-stable, while keeping
    /// each earlier closure snapshot tied to its original selected pins.
    /// Across repeated paired depth storms, distinct sibling middle and deep
    /// pins may reconverge on a shared terminal pin. The reference is silent
    /// on this sequence, so v1 derives each route from its exact selected pin.
    /// Each session's modules follow the pins in that snapshot; later sibling
    /// rebinds do not refresh or retarget earlier sessions.
    /// After those paired depth waves, a terminal rebind storm updates only
    /// that sibling session's terminal pin and module route. Siblings can
    /// diverge and reconverge on the same terminal pin, while retained
    /// manifest and pre-rebind snapshots keep their prior routes.
    /// A later paired depth storm also leaves those rebound terminal
    /// snapshots intact. Newly opened terminal closures start from their
    /// latest exact deep pins and follow those pins' manifest routes.
    /// The reference is silent on terminal rebinds in those reopened
    /// sessions, so v1 updates only each selected sibling's route and keeps
    /// every retained manifest snapshot on its original pin-to-module route.
    /// Reordering terminal rebind events between siblings while preserving
    /// each sibling's own event order leaves both final routes unchanged.
    /// v1 treats the sibling sessions as independent; retained snapshots
    /// continue to use their manifest-selected terminal pins.
    /// Repeating the same post-storm terminal rebind wave stabilizes each
    /// sibling on its own final pin while snapshots from earlier waves retain
    /// the route they captured. The reference is silent on repeated waves, so
    /// v1 applies the exact-pin rule independently on every rebind.
    /// Repeating paired middle and deep rebinds after those terminal storms
    /// opens each new terminal route from the exact selected deep pin; the
    /// earlier terminal sessions keep their captured routes. The reference
    /// does not define this post-storm sequence, so v1 resolves each edge from
    /// the pin selected in that wave.
    /// Reopening a retained pre-rebind middle snapshot after later paired
    /// depth waves still follows the deep and terminal pins in that snapshot's
    /// manifest, even if a sibling's newer rebound route now differs.
    /// Terminal sessions retained before those waves keep both their
    /// manifest-selected terminal pin and the ancestor module route captured
    /// from the selected deep pin. Later sibling depth rebinds do not refresh
    /// either part of that session snapshot. The reference is silent on
    /// cross-wave refresh, so v1 keeps both routes bound to the captured pins.
    pub fn resolve_for_parent(
        &self,
        primary: PinnedDatabase,
    ) -> Result<AttachedDatabaseSession, AttachmentError> {
        let manifest = PackagePinManifest::load(&primary.repository, &primary.pin.commit)?;
        // `stdlib/std` is the project import's source authority; `.orna/packages`
        // is the explicit host attachment map. If both pin the `std` alias,
        // require one immutable snapshot so the package map cannot retarget
        // `use std.*` away from the program's captured Git module.
        let standard_gitlink =
            captured_standard_gitlink(&primary.repository, &primary.pin.commit)?;
        AttachedDatabaseSession::validate_primary_name(&primary.pin.name)?;
        let mut resolved_databases = Vec::with_capacity(manifest.pins().len());
        for spec in manifest.pins() {
            if spec.name() == primary.pin.name {
                return Err(AttachmentError::DuplicateAttachment);
            }
            if spec.name() == "std"
                && standard_gitlink
                    .as_ref()
                    .is_some_and(|commit| commit.as_str() != spec.object_id())
            {
                return Err(AttachmentError::PinUnavailable);
            }
            let repository = self
                .repositories
                .get(spec.name())
                .cloned()
                .ok_or(AttachmentError::RepositoryUnavailable)?;
            let commit = repository
                .resolve_snapshot(spec.object_id())
                .map_err(|_| AttachmentError::PinUnavailable)?;
            if commit.as_str() != spec.object_id() {
                return Err(AttachmentError::PinUnavailable);
            }
            let database = PinnedDatabase::load_at_commit(
                spec.name().to_owned(),
                repository,
                commit,
                self.loader,
            )
            .map_err(|error| match error {
                // The normative contract fixes exact commit identity but not
                // package-load diagnostics. Keep host paths and parser details
                // out of resolver errors while distinguishing a present pin
                // whose contents are not a loadable database.
                AttachmentError::Project(_) => AttachmentError::PinnedPackageInvalid,
                error => error,
            })?;
            resolved_databases.push(database);
        }

        // The reference requires each attachment to use its exact pin but
        // leaves batch failure visibility unspecified. Resolve every package
        // before creating the session so callers can observe no partial set.
        let mut session = AttachedDatabaseSession::new(primary)?;
        for database in resolved_databases {
            session.attach_database(database)?;
        }
        Ok(session)
    }

    /// Resolves the next closure from an exact alias selected in `parent`.
    /// This uses that session's current pin, so a retained sibling session
    /// continues from its own route after another session is rebound.
    pub fn resolve_nested_for_alias(
        &self,
        parent: &AttachedDatabaseSession,
        alias: &str,
    ) -> Result<AttachedDatabaseSession, AttachmentError> {
        let alias = checked_name(alias.to_owned())?;
        let selected = parent
            .database(&alias)
            .cloned()
            .ok_or(AttachmentError::DatabaseUnavailable)?;
        self.resolve_for_parent(selected)
    }

    /// Rebinds one exact alias on a private copy of `parent`, then resolves
    /// the replacement's next closure. The returned snapshot preserves the
    /// prior route, and a failed expansion leaves the caller's session intact.
    /// Where the reference does not define rebind ordering, v1 uses the new
    /// pin's committed manifest for this edge.
    pub fn resolve_nested_rebind(
        &self,
        parent: &AttachedDatabaseSession,
        replacement: PinnedDatabase,
    ) -> Result<ReboundPathResolution, AttachmentError> {
        let alias = replacement.pin.name.clone();
        let retained = vec![parent.clone()];
        let mut rebound_parent = parent.clone();
        rebound_parent.rebind_database(replacement)?;
        let final_session = self.resolve_nested_for_alias(&rebound_parent, &alias)?;
        Ok(ReboundPathResolution {
            final_session,
            retained_sessions: retained,
            retained_wave_lengths: vec![1],
            route_identity: Arc::new(()),
            retained_snapshot_identities: vec![Arc::new(())],
        })
    }

    /// Applies replacements one closure depth at a time and resolves the
    /// resulting terminal route. One pre-rebind snapshot is retained per
    /// depth. The whole path is atomic from the caller's view: if any later
    /// replacement or closure fails, no partial result is returned and
    /// `parent` remains unchanged. An empty path returns a clone of `parent`.
    pub fn resolve_nested_rebind_path(
        &self,
        parent: &AttachedDatabaseSession,
        replacements: &[PinnedDatabase],
    ) -> Result<ReboundPathResolution, AttachmentError> {
        let mut current = parent.clone();
        let mut retained_sessions = Vec::with_capacity(replacements.len());
        let mut retained_snapshot_identities = Vec::with_capacity(replacements.len());
        for replacement in replacements {
            let mut extension = self.resolve_nested_rebind(&current, replacement.clone())?;
            retained_sessions.append(&mut extension.retained_sessions);
            retained_snapshot_identities.append(&mut extension.retained_snapshot_identities);
            current = extension.final_session;
        }
        let retained_wave_lengths = if retained_sessions.is_empty() {
            Vec::new()
        } else {
            vec![retained_sessions.len()]
        };
        Ok(ReboundPathResolution {
            final_session: current,
            retained_sessions,
            retained_wave_lengths,
            route_identity: Arc::new(()),
            retained_snapshot_identities,
        })
    }

    /// Resolves two ordered terminal-depth replacements as one atomic wave.
    /// The two prior sessions remain together in `retained_wave(0)`, so the
    /// pre-pair and between-depth routes can both be reopened independently.
    pub fn resolve_nested_terminal_pair(
        &self,
        parent: &AttachedDatabaseSession,
        replacements: [PinnedDatabase; 2],
    ) -> Result<ReboundPathResolution, AttachmentError> {
        self.resolve_nested_rebind_path(parent, &replacements)
    }

    /// Continues a resolved rebind path with another wave of replacements.
    /// Snapshots from the earlier wave stay in order, followed by the prior
    /// terminal session and any new intermediate routes. A failed extension
    /// leaves `previous` available with its original final route.
    pub fn extend_nested_rebind_path(
        &self,
        previous: &ReboundPathResolution,
        replacements: &[PinnedDatabase],
    ) -> Result<ReboundPathResolution, AttachmentError> {
        let extension =
            self.resolve_nested_rebind_path(previous.final_session(), replacements)?;
        Ok(Self::append_rebound_extension(previous, extension))
    }

    /// Continues a route from one of its retained snapshots rather than its
    /// deepest resolved closure. `retained_session` indexes the flattened
    /// history returned by `retained_sessions()`. This keeps post-storm
    /// closure work rooted in the exact intermediate pins captured earlier.
    /// The reference is silent on reopening these historical routes; v1 uses
    /// the flattened index to select the exact snapshot and retains the
    /// superseded final route as its own wave. Any snapshots produced by the
    /// new call form another retained wave. A missing index or failed closure
    /// returns no new route.
    pub fn extend_nested_rebind_path_from_retained(
        &self,
        previous: &ReboundPathResolution,
        retained_session: usize,
        replacements: &[PinnedDatabase],
    ) -> Result<ReboundPathResolution, AttachmentError> {
        let parent = previous
            .retained_sessions()
            .get(retained_session)
            .ok_or(AttachmentError::RetainedSnapshotUnavailable)?;
        let extension = self.resolve_nested_rebind_path(parent, replacements)?;
        Ok(Self::append_retained_rebound_extension(previous, extension))
    }

    /// Continues a route from a snapshot selected by retained wave and
    /// position within that wave. The reference does not define reopening
    /// between waves; v1 resolves from the exact selected pins and appends the
    /// superseded final route as its own wave before appending the new
    /// snapshots as another wave. Invalid wave or snapshot positions and
    /// failed closures return no new route.
    pub fn extend_nested_rebind_path_from_wave(
        &self,
        previous: &ReboundPathResolution,
        wave: usize,
        snapshot: usize,
        replacements: &[PinnedDatabase],
    ) -> Result<ReboundPathResolution, AttachmentError> {
        let parent = previous
            .retained_wave(wave)
            .and_then(|sessions| sessions.get(snapshot))
            .ok_or(AttachmentError::RetainedSnapshotUnavailable)?;
        let extension = self.resolve_nested_rebind_path(parent, replacements)?;
        Ok(Self::append_retained_rebound_extension(previous, extension))
    }

    fn append_rebound_extension(
        previous: &ReboundPathResolution,
        extension: ReboundPathResolution,
    ) -> ReboundPathResolution {
        let ReboundPathResolution {
            final_session,
            mut retained_sessions,
            retained_wave_lengths: extension_wave_lengths,
            mut retained_snapshot_identities,
            ..
        } = extension;
        let mut all_retained = previous.retained_sessions.clone();
        all_retained.append(&mut retained_sessions);
        let mut retained_wave_lengths = previous.retained_wave_lengths.clone();
        retained_wave_lengths.extend(extension_wave_lengths);
        let mut all_snapshot_identities = previous.retained_snapshot_identities.clone();
        all_snapshot_identities.append(&mut retained_snapshot_identities);
        ReboundPathResolution {
            final_session,
            retained_sessions: all_retained,
            retained_wave_lengths,
            route_identity: previous.route_identity.clone(),
            retained_snapshot_identities: all_snapshot_identities,
        }
    }

    fn append_retained_rebound_extension(
        previous: &ReboundPathResolution,
        extension: ReboundPathResolution,
    ) -> ReboundPathResolution {
        let ReboundPathResolution {
            final_session,
            mut retained_sessions,
            retained_wave_lengths: extension_wave_lengths,
            mut retained_snapshot_identities,
            ..
        } = extension;
        let mut all_retained = previous.retained_sessions.clone();
        all_retained.push(previous.final_session.clone());
        all_retained.append(&mut retained_sessions);
        let mut retained_wave_lengths = previous.retained_wave_lengths.clone();
        retained_wave_lengths.push(1);
        retained_wave_lengths.extend(extension_wave_lengths);
        let mut all_snapshot_identities = previous.retained_snapshot_identities.clone();
        all_snapshot_identities.push(Arc::new(()));
        all_snapshot_identities.append(&mut retained_snapshot_identities);
        ReboundPathResolution {
            final_session,
            retained_sessions: all_retained,
            retained_wave_lengths,
            route_identity: previous.route_identity.clone(),
            retained_snapshot_identities: all_snapshot_identities,
        }
    }

    /// Extends a post-storm route with two ordered terminal-depth rebinds.
    /// The prior history stays intact and the new pair is exposed together in
    /// the last retained wave. A failure leaves `previous` unchanged.
    pub fn extend_nested_terminal_pair(
        &self,
        previous: &ReboundPathResolution,
        replacements: [PinnedDatabase; 2],
    ) -> Result<ReboundPathResolution, AttachmentError> {
        self.extend_nested_rebind_path(previous, &replacements)
    }

    /// Extends a route with a terminal-depth pair from one of its retained
    /// snapshots. This is useful after a rebind storm has resolved a deeper
    /// closure and callers need to continue from an earlier exact route.
    /// Prior snapshots stay in order; the previous final route is then
    /// retained as its own wave, followed by the selected root and
    /// intermediate route captured by the new pair. The input route remains
    /// unchanged if either replacement or closure fails.
    pub fn extend_nested_terminal_pair_from_retained(
        &self,
        previous: &ReboundPathResolution,
        retained_session: usize,
        replacements: [PinnedDatabase; 2],
    ) -> Result<ReboundPathResolution, AttachmentError> {
        self.extend_nested_rebind_path_from_retained(previous, retained_session, &replacements)
    }

    /// Extends a terminal-depth pair from one snapshot in a retained wave.
    /// This keeps repeated post-storm continuations attached to their explicit
    /// wave and snapshot positions while preserving every prior route.
    pub fn extend_nested_terminal_pair_from_wave(
        &self,
        previous: &ReboundPathResolution,
        wave: usize,
        snapshot: usize,
        replacements: [PinnedDatabase; 2],
    ) -> Result<ReboundPathResolution, AttachmentError> {
        self.extend_nested_rebind_path_from_wave(previous, wave, snapshot, &replacements)
    }

    /// Applies an ordered chain of terminal-depth pairs to one nested route.
    /// Each pair starts from the last snapshot in the latest retained wave,
    /// carrying the preceding rebind forward by one closure depth. With no
    /// retained history, the first pair starts from the current final route.
    /// The reference is silent on this continuation rule; v1 uses that
    /// fallback and returns no partial chain if a pair fails.
    pub fn extend_nested_terminal_pair_chain(
        &self,
        previous: &ReboundPathResolution,
        replacement_waves: &[[PinnedDatabase; 2]],
    ) -> Result<ReboundPathResolution, AttachmentError> {
        let mut route = previous.clone();
        for replacements in replacement_waves {
            if let Some(latest_wave) = route.retained_wave_lengths.len().checked_sub(1) {
                let latest_snapshot = route.retained_wave_lengths[latest_wave]
                    .checked_sub(1)
                    .ok_or(AttachmentError::RetainedSnapshotUnavailable)?;
                route = self.extend_nested_terminal_pair_from_wave(
                    &route,
                    latest_wave,
                    latest_snapshot,
                    replacements.clone(),
                )?;
            } else {
                route = self.extend_nested_terminal_pair(&route, replacements.clone())?;
            }
        }
        Ok(route)
    }

    /// Compacts retained history through paired snapshot folds while keeping
    /// the current terminal route and the identities of the selected
    /// snapshots. Each fold keeps exactly two labeled snapshots in the given
    /// order and discards the other retained history. Labels are resolved by
    /// their unique snapshot identity, so labels and checkpoints captured
    /// before a compaction remain usable after their wave/depth coordinates
    /// move. The reference defines storage compaction as preserving semantic
    /// rows and checkpoints, but does not define in-memory attach-route
    /// compaction; v1 applies that preservation rule to the two selected
    /// closure snapshots and rejects stale, duplicate, or cross-route labels
    /// atomically.
    pub fn compact_nested_terminal_pair_history_folds_preserving_terminal_identity(
        &self,
        previous: &ReboundPathResolution,
        folds: &[[NestedPairDepthLabel; 2]],
    ) -> Result<
        (
            ReboundPathResolution,
            Vec<(NestedPairTerminalRouteIdentity, NestedPairTerminalRouteIdentity)>,
        ),
        AttachmentError,
    > {
        let mut route = previous.clone();
        let mut transitions = Vec::with_capacity(folds.len());
        for fold in folds {
            let before = route.terminal_route_identity();
            route = route.compact_retained_depth_pair(fold)?;
            let after = route.terminal_route_identity();
            route.validate_terminal_route_identity(&before)?;
            transitions.push((before, after));
        }
        Ok((route, transitions))
    }

    /// Applies paired history-compaction folds grouped into caller-visible
    /// pages. Folds run in order across page boundaries, so stable snapshot
    /// labels from an earlier page still select the same pins after later
    /// compaction moves their coordinates. The reference is silent on
    /// paginating in-memory attach history; v1 preserves one transition list
    /// per input page (including empty pages) and returns no partial result if
    /// any fold is invalid.
    pub fn compact_nested_terminal_pair_history_pages_preserving_terminal_identity(
        &self,
        previous: &ReboundPathResolution,
        pages: &[&[[NestedPairDepthLabel; 2]]],
    ) -> Result<
        (
            ReboundPathResolution,
            Vec<Vec<(NestedPairTerminalRouteIdentity, NestedPairTerminalRouteIdentity)>>,
        ),
        AttachmentError,
    > {
        let mut route = previous.clone();
        let mut page_transitions = Vec::with_capacity(pages.len());
        for page in pages {
            let (compacted, transitions) = self
                .compact_nested_terminal_pair_history_folds_preserving_terminal_identity(
                    &route, page,
                )?;
            route = compacted;
            page_transitions.push(transitions);
        }
        Ok((route, page_transitions))
    }

    /// Repeatedly compacts one labelled terminal pair, rotating its order
    /// after each fold. `folds_per_page` controls how many rotations occur in
    /// each page; the orientation carries across page boundaries and a zero
    /// count preserves an empty page without changing it. The reference does
    /// not define rotation for attach-history folds; v1 rotates the two exact
    /// snapshot identities and validates the terminal route after every fold.
    /// If a label is stale, no partial route is returned.
    pub fn compact_nested_terminal_pair_history_rotation_pages_preserving_terminal_identity(
        &self,
        previous: &ReboundPathResolution,
        labels: &[NestedPairDepthLabel; 2],
        folds_per_page: &[usize],
    ) -> Result<
        (
            ReboundPathResolution,
            Vec<Vec<(NestedPairTerminalRouteIdentity, NestedPairTerminalRouteIdentity)>>,
        ),
        AttachmentError,
    > {
        let mut route = previous.clone();
        let mut ordered_labels = [labels[0].clone(), labels[1].clone()];
        let mut page_transitions = Vec::with_capacity(folds_per_page.len());
        for fold_count in folds_per_page {
            let mut transitions = Vec::with_capacity(*fold_count);
            for _ in 0..*fold_count {
                let (compacted, fold_transitions) = self
                    .compact_nested_terminal_pair_history_folds_preserving_terminal_identity(
                        &route,
                        std::slice::from_ref(&ordered_labels),
                    )?;
                route = compacted;
                transitions.extend(fold_transitions);
                ordered_labels.rotate_left(1);
            }
            page_transitions.push(transitions);
        }
        Ok((route, page_transitions))
    }

    /// Compacts retained history through paired handoff checkpoints. Each
    /// fold keeps the two exact checkpoint snapshots in caller order, so a
    /// checkpoint captured before compaction remains replayable while its
    /// snapshot is retained. The reference does not define checkpoint-backed
    /// attach-history compaction; v1 requires both checkpoints to belong to
    /// this route and validates their identities after every fold.
    pub fn compact_nested_terminal_pair_checkpoint_folds_preserving_terminal_identity(
        &self,
        previous: &ReboundPathResolution,
        checkpoint_folds: &[[&ReboundPathCheckpoint; 2]],
    ) -> Result<
        (
            ReboundPathResolution,
            Vec<(NestedPairTerminalRouteIdentity, NestedPairTerminalRouteIdentity)>,
        ),
        AttachmentError,
    > {
        let mut route = previous.clone();
        let mut transitions = Vec::with_capacity(checkpoint_folds.len());
        for checkpoints in checkpoint_folds {
            for checkpoint in checkpoints {
                checkpoint.validate_depth_identity()?;
                route.validate_depth_label(checkpoint.depth_label())?;
            }
            let labels = [
                checkpoints[0].depth_label().clone(),
                checkpoints[1].depth_label().clone(),
            ];
            let (compacted, fold_transitions) = self
                .compact_nested_terminal_pair_history_folds_preserving_terminal_identity(
                    &route,
                    std::slice::from_ref(&labels),
                )?;
            route = compacted;
            for checkpoint in checkpoints {
                route.validate_depth_label(checkpoint.depth_label())?;
            }
            transitions.push(
                fold_transitions
                    .into_iter()
                    .next()
                    .ok_or(AttachmentError::RetainedSnapshotUnavailable)?,
            );
        }
        Ok((route, transitions))
    }

    /// Compacts paired checkpoints across retained history and checkpoint
    /// spills. A same-route checkpoint may restore its exact snapshot into a
    /// temporary retained wave after an earlier fold removed that snapshot;
    /// the fold then keeps the selected pair and their original identities.
    /// The reference does not define spilled in-memory attach checkpoints, so
    /// v1 treats the checkpoint's owned handoff as the spill record. Foreign,
    /// malformed, or duplicate checkpoint identities fail atomically.
    pub fn compact_nested_terminal_pair_checkpoint_spill_folds_preserving_terminal_identity(
        &self,
        previous: &ReboundPathResolution,
        checkpoint_folds: &[[&ReboundPathCheckpoint; 2]],
    ) -> Result<
        (
            ReboundPathResolution,
            Vec<(NestedPairTerminalRouteIdentity, NestedPairTerminalRouteIdentity)>,
        ),
        AttachmentError,
    > {
        self.compact_nested_terminal_pair_checkpoint_spill_stream_preserving_terminal_identity(
            previous,
            checkpoint_folds.iter().copied(),
        )
    }

    /// Consumes ordered checkpoint pairs once and compacts each pair before
    /// requesting the next one. A same-route checkpoint may restore its exact
    /// snapshot into a temporary retained wave after an earlier fold removed
    /// it; every fold retains the selected snapshots and their original
    /// identities. The reference does not define streaming checkpoint folds
    /// for attach routes, so v1 treats each yielded pair as one ordered fold
    /// and returns no route unless the complete stream validates. Foreign,
    /// malformed, or duplicate identities stop consumption with an error.
    pub fn compact_nested_terminal_pair_checkpoint_spill_stream_preserving_terminal_identity<'a>(
        &self,
        previous: &ReboundPathResolution,
        checkpoint_folds: impl IntoIterator<Item = [&'a ReboundPathCheckpoint; 2]>,
    ) -> Result<
        (
            ReboundPathResolution,
            Vec<(NestedPairTerminalRouteIdentity, NestedPairTerminalRouteIdentity)>,
        ),
        AttachmentError,
    > {
        let mut route = previous.clone();
        let mut transitions = Vec::new();
        for checkpoints in checkpoint_folds {
            for checkpoint in checkpoints {
                checkpoint.validate_depth_identity()?;
                if !Arc::ptr_eq(
                    &route.route_identity,
                    &checkpoint.depth_label.route_identity,
                ) {
                    return Err(AttachmentError::RetainedSnapshotUnavailable);
                }
            }
            if Arc::ptr_eq(
                &checkpoints[0].depth_label.snapshot_identity,
                &checkpoints[1].depth_label.snapshot_identity,
            ) {
                return Err(AttachmentError::RetainedSnapshotUnavailable);
            }

            let mut spilled_sessions = Vec::new();
            let mut spilled_identities = Vec::new();
            for checkpoint in checkpoints {
                let label = checkpoint.depth_label();
                if route
                    .retained_snapshot_identities
                    .iter()
                    .any(|identity| Arc::ptr_eq(identity, &label.snapshot_identity))
                {
                    route.validate_depth_label(label)?;
                } else {
                    spilled_sessions.push(checkpoint.handoff().clone());
                    spilled_identities.push(label.snapshot_identity.clone());
                }
            }
            if !spilled_sessions.is_empty() {
                route
                    .retained_sessions
                    .extend(spilled_sessions.iter().cloned());
                route
                    .retained_snapshot_identities
                    .extend(spilled_identities);
                route.retained_wave_lengths.push(spilled_sessions.len());
            }

            let labels = [
                checkpoints[0].depth_label().clone(),
                checkpoints[1].depth_label().clone(),
            ];
            let before = route.terminal_route_identity();
            route = route.compact_retained_depth_pair(&labels)?;
            let after = route.terminal_route_identity();
            route.validate_terminal_route_identity(&before)?;
            for checkpoint in checkpoints {
                route.validate_depth_label(checkpoint.depth_label())?;
            }
            transitions.push((before, after));
        }
        Ok((route, transitions))
    }

    /// Continues a terminal-pair chain from an exact retained session. The
    /// first pair uses that session as its handoff root; later pairs continue
    /// from the preceding pair's newest handoff. The reference does not define
    /// selecting an older handoff for a chained pair; v1 keeps that explicit
    /// depth and preserves the displaced terminal route with the new history.
    /// An empty chain leaves the previous route unchanged.
    pub fn extend_nested_terminal_pair_chain_from_retained(
        &self,
        previous: &ReboundPathResolution,
        retained_session: usize,
        replacement_waves: &[[PinnedDatabase; 2]],
    ) -> Result<ReboundPathResolution, AttachmentError> {
        let Some((first, remaining)) = replacement_waves.split_first() else {
            return Ok(previous.clone());
        };
        let route = self.extend_nested_terminal_pair_from_retained(
            previous,
            retained_session,
            first.clone(),
        )?;
        self.extend_nested_terminal_pair_chain(&route, remaining)
    }

    /// Continues a terminal-pair chain from an exact snapshot in a retained
    /// wave. The selected route determines the first pair's closure depth;
    /// subsequent pairs preserve the normal one-depth handoff between waves.
    /// Invalid wave or snapshot positions return no new route. The reference
    /// is silent on this selection rule; v1 resolves from the exact snapshot.
    pub fn extend_nested_terminal_pair_chain_from_wave(
        &self,
        previous: &ReboundPathResolution,
        wave: usize,
        snapshot: usize,
        replacement_waves: &[[PinnedDatabase; 2]],
    ) -> Result<ReboundPathResolution, AttachmentError> {
        let selected_wave = previous
            .retained_wave(wave)
            .ok_or(AttachmentError::RetainedSnapshotUnavailable)?;
        if snapshot >= selected_wave.len() {
            return Err(AttachmentError::RetainedSnapshotUnavailable);
        }
        let retained_session = previous.retained_wave_lengths[..wave]
            .iter()
            .sum::<usize>()
            + snapshot;
        self.extend_nested_terminal_pair_chain_from_retained(
            previous,
            retained_session,
            replacement_waves,
        )
    }

    /// Rebinds each terminal pair in a storm from the same retained route.
    /// Unlike a pair chain, one pair's handoff is not used as the next pair's
    /// root, so repeated rebinds do not widen the closure depth. The selected
    /// wave and snapshot remain stable because extensions append history.
    /// The reference is silent on storm root selection; v1 uses the exact
    /// caller-selected snapshot and returns no partial extension on failure.
    pub fn extend_nested_terminal_pair_storm_from_wave(
        &self,
        previous: &ReboundPathResolution,
        wave: usize,
        snapshot: usize,
        replacement_waves: &[[PinnedDatabase; 2]],
    ) -> Result<ReboundPathResolution, AttachmentError> {
        let label = previous.retained_depth_label(wave, snapshot)?;
        self.extend_nested_terminal_pair_storm_from_label(previous, &label, replacement_waves)
    }

    /// Rebinds each terminal pair from one labelled retained depth.
    ///
    /// The reference does not assign labels to snapshots in folded storms.
    /// In v1, a label records its wave, its pre-rebind depth within that wave,
    /// and the exact primary and attached pins at that position. Each fold
    /// verifies the label still selects those pins, so a regrouped history
    /// cannot silently redirect a later pair. Even an empty fold validates
    /// the label before returning the unchanged route.
    pub fn extend_nested_terminal_pair_storm_from_label(
        &self,
        previous: &ReboundPathResolution,
        label: &NestedPairDepthLabel,
        replacement_waves: &[[PinnedDatabase; 2]],
    ) -> Result<ReboundPathResolution, AttachmentError> {
        previous.validate_depth_label(label)?;
        let mut route = previous.clone();
        for replacements in replacement_waves {
            let (wave, depth) = route.retained_position_for_depth_label(label)?;
            route = self.extend_nested_terminal_pair_from_wave(
                &route,
                wave,
                depth,
                replacements.clone(),
            )?;
        }
        route.validate_depth_label(label)?;
        Ok(route)
    }

    /// Applies successive checkpoint-storm batches from one exact retained
    /// depth label. The selected label remains bound to its original route
    /// identity and pins across every batch, even as newer closures are
    /// appended. The reference is silent on chaining labelled storms; v1
    /// validates the label at each batch boundary and returns no partial route
    /// if any batch fails.
    pub fn extend_nested_terminal_pair_storm_rounds_from_label(
        &self,
        previous: &ReboundPathResolution,
        label: &NestedPairDepthLabel,
        rounds: &[&[[PinnedDatabase; 2]]],
    ) -> Result<ReboundPathResolution, AttachmentError> {
        previous.validate_depth_label(label)?;
        let mut route = previous.clone();
        for replacement_waves in rounds {
            route.validate_depth_label(label)?;
            route = self.extend_nested_terminal_pair_storm_from_label(
                &route,
                label,
                replacement_waves,
            )?;
            route.validate_depth_label(label)?;
        }
        Ok(route)
    }

    /// Continues a nested terminal-pair chain from a saved handoff checkpoint.
    /// The first pair uses the checkpoint's exact attached pins even when
    /// `previous` has since gone through another rebind cascade; later pairs
    /// continue from their latest handoff. The reference is silent on replaying
    /// retained routes across cascades, so v1 records the checkpoint route in
    /// the new history and keeps the input route unchanged on failure.
    pub fn extend_nested_terminal_pair_chain_from_checkpoint(
        &self,
        previous: &ReboundPathResolution,
        checkpoint: &ReboundPathCheckpoint,
        replacement_waves: &[[PinnedDatabase; 2]],
    ) -> Result<ReboundPathResolution, AttachmentError> {
        self.extend_nested_terminal_pair_chain_from_checkpoint_with_labels(
            previous,
            checkpoint,
            replacement_waves,
            &[],
        )
        .map(|(route, _)| route)
    }

    /// Applies ordered checkpoint-rooted pair chains as one atomic cascade.
    /// Every checkpoint route is appended to retained history, then its exact
    /// depth label is checked after each later pair fold, including folds from
    /// subsequent checkpoints. The reference does not define this composition;
    /// v1 keeps each selected checkpoint independent and preserves all earlier
    /// checkpoint labels. Failure returns no partial cascade.
    pub fn extend_nested_terminal_pair_checkpoint_cascades(
        &self,
        previous: &ReboundPathResolution,
        cascades: &[(&ReboundPathCheckpoint, &[[PinnedDatabase; 2]])],
    ) -> Result<ReboundPathResolution, AttachmentError> {
        let mut route = previous.clone();
        let mut retained_checkpoint_labels = Vec::new();
        for (checkpoint, replacement_waves) in cascades {
            let (folded, checkpoint_label) = self
                .extend_nested_terminal_pair_chain_from_checkpoint_with_labels(
                    &route,
                    checkpoint,
                    replacement_waves,
                    &retained_checkpoint_labels,
                )?;
            if let Some(label) = checkpoint_label {
                retained_checkpoint_labels.push(label);
            }
            route = folded;
        }
        Ok(route)
    }

    /// Applies checkpoint-anchored paired rebind chains while validating the
    /// terminal identity on both sides of every anchor. Labels emitted by an
    /// earlier anchor remain attached to their exact snapshots through later
    /// chains. The reference does not specify identity edges between nested
    /// anchor chains; v1 checks each edge in order and returns no partial route
    /// or transition list when an anchor or identity is stale.
    pub fn extend_nested_terminal_pair_checkpoint_cascades_validating_terminal_identity_transitions(
        &self,
        previous: &ReboundPathResolution,
        cascades: &[(&ReboundPathCheckpoint, &[[PinnedDatabase; 2]])],
        expected_transitions: &[(NestedPairTerminalRouteIdentity, NestedPairTerminalRouteIdentity)],
    ) -> Result<(
        ReboundPathResolution,
        Vec<(NestedPairTerminalRouteIdentity, NestedPairTerminalRouteIdentity)>,
    ), AttachmentError> {
        if cascades.len() != expected_transitions.len() {
            return Err(AttachmentError::RetainedSnapshotUnavailable);
        }

        let mut route = previous.clone();
        let mut retained_checkpoint_labels = Vec::new();
        let mut transitions = Vec::with_capacity(cascades.len());
        for ((checkpoint, replacement_waves), (expected_before, expected_after)) in
            cascades.iter().zip(expected_transitions)
        {
            let before = route.terminal_route_identity();
            route.validate_terminal_route_identity(expected_before)?;
            let (folded, checkpoint_label) = self
                .extend_nested_terminal_pair_chain_from_checkpoint_with_labels(
                    &route,
                    checkpoint,
                    replacement_waves,
                    &retained_checkpoint_labels,
                )?;
            if let Some(label) = checkpoint_label {
                retained_checkpoint_labels.push(label);
            }
            route = folded;
            for label in &retained_checkpoint_labels {
                route.validate_depth_label(label)?;
            }
            let after = route.terminal_route_identity();
            route.validate_terminal_route_identity(expected_after)?;
            transitions.push((before, after));
        }

        Ok((route, transitions))
    }

    /// Applies repeated paired checkpoint excursions and restorations. Each
    /// fold contains exactly two anchored rebind chains; both identity edges
    /// are checked, and the second chain must restore the fold's starting
    /// terminal identity. Labels emitted by every anchor remain bound through
    /// later folds. The reference does not specify this restoration pairing;
    /// v1 requires exact lineage and pin equality at every boundary.
    pub fn extend_nested_terminal_pair_checkpoint_restoration_folds_validating_terminal_identity_transitions(
        &self,
        previous: &ReboundPathResolution,
        restoration_folds: &[[(&ReboundPathCheckpoint, &[[PinnedDatabase; 2]]); 2]],
        expected_fold_transitions: &[
            [(NestedPairTerminalRouteIdentity, NestedPairTerminalRouteIdentity); 2]
        ],
    ) -> Result<
        (
            ReboundPathResolution,
            Vec<[(NestedPairTerminalRouteIdentity, NestedPairTerminalRouteIdentity); 2]>,
        ),
        AttachmentError,
    > {
        if restoration_folds.len() != expected_fold_transitions.len() {
            return Err(AttachmentError::RetainedSnapshotUnavailable);
        }

        self.extend_nested_terminal_pair_checkpoint_restoration_stream_validating_terminal_identity_transitions(
            previous,
            restoration_folds
                .iter()
                .copied()
                .zip(expected_fold_transitions.iter().cloned()),
        )
    }

    /// Consumes paired checkpoint restoration folds once, in order. Each
    /// yielded fold contains two checkpoint-rooted chains and their expected
    /// identity edges. A fold must return to the terminal identity it started
    /// from, and labels emitted earlier in the stream remain bound through
    /// every later fold. The reference does not specify streamed restore-fold
    /// semantics; v1 validates each yielded fold before requesting the next
    /// and returns no partial route if any edge or restoration is invalid.
    pub fn extend_nested_terminal_pair_checkpoint_restoration_stream_validating_terminal_identity_transitions<'a>(
        &self,
        previous: &ReboundPathResolution,
        restoration_folds: impl IntoIterator<
            Item = (
                [(&'a ReboundPathCheckpoint, &'a [[PinnedDatabase; 2]]); 2],
                [(NestedPairTerminalRouteIdentity, NestedPairTerminalRouteIdentity); 2],
            ),
        >,
    ) -> Result<
        (
            ReboundPathResolution,
            Vec<[(NestedPairTerminalRouteIdentity, NestedPairTerminalRouteIdentity); 2]>,
        ),
        AttachmentError,
    > {
        let mut route = previous.clone();
        let mut retained_checkpoint_labels = Vec::new();
        let mut all_transitions = Vec::new();
        for (fold, expected_transitions) in restoration_folds {
            let starting_identity = route.terminal_route_identity();
            let mut transitions = Vec::with_capacity(2);
            for ((checkpoint, replacement_waves), (expected_before, expected_after)) in
                fold.iter().zip(&expected_transitions)
            {
                let before = route.terminal_route_identity();
                route.validate_terminal_route_identity(expected_before)?;
                let (folded, checkpoint_label) = self
                    .extend_nested_terminal_pair_chain_from_checkpoint_with_labels(
                        &route,
                        checkpoint,
                        replacement_waves,
                        &retained_checkpoint_labels,
                    )?;
                if let Some(label) = checkpoint_label {
                    retained_checkpoint_labels.push(label);
                }
                route = folded;
                for label in &retained_checkpoint_labels {
                    route.validate_depth_label(label)?;
                }
                let after = route.terminal_route_identity();
                route.validate_terminal_route_identity(expected_after)?;
                transitions.push((before, after));
            }

            route.validate_terminal_route_identity(&starting_identity)?;
            let transitions: [
                (NestedPairTerminalRouteIdentity, NestedPairTerminalRouteIdentity);
                2
            ] = transitions
                .try_into()
                .map_err(|_| AttachmentError::RetainedSnapshotUnavailable)?;
            all_transitions.push(transitions);
        }

        Ok((route, all_transitions))
    }

    fn extend_nested_terminal_pair_chain_from_checkpoint_with_labels(
        &self,
        previous: &ReboundPathResolution,
        checkpoint: &ReboundPathCheckpoint,
        replacement_waves: &[[PinnedDatabase; 2]],
        retained_checkpoint_labels: &[NestedPairDepthLabel],
    ) -> Result<(ReboundPathResolution, Option<NestedPairDepthLabel>), AttachmentError> {
        checkpoint.validate_depth_identity()?;
        previous.validate_depth_label(&checkpoint.depth_label)?;
        let Some((first, remaining)) = replacement_waves.split_first() else {
            for label in retained_checkpoint_labels {
                previous.validate_depth_label(label)?;
            }
            return Ok((previous.clone(), None));
        };
        let checkpoint_wave = previous
            .retained_wave_lengths
            .len()
            .checked_add(1)
            .ok_or(AttachmentError::RetainedSnapshotUnavailable)?;
        let extension =
            self.resolve_nested_rebind_path(&checkpoint.handoff, first.as_slice())?;
        let mut route = Self::append_retained_rebound_extension(previous, extension);
        let retained_checkpoint = route.retained_snapshot_at_depth(checkpoint_wave, 0)?;
        if !checkpoint.depth_label.matches_session(retained_checkpoint) {
            return Err(AttachmentError::RetainedSnapshotUnavailable);
        }
        let folded_label = NestedPairDepthLabel::from_session(
            checkpoint_wave,
            0,
            &route.route_identity,
            route.retained_snapshot_identity_at_depth(checkpoint_wave, 0)?,
            retained_checkpoint,
        );
        route.validate_depth_label(&folded_label)?;
        for label in retained_checkpoint_labels {
            route.validate_depth_label(label)?;
        }

        for replacements in remaining {
            let latest_wave = route
                .retained_wave_lengths
                .len()
                .checked_sub(1)
                .ok_or(AttachmentError::RetainedSnapshotUnavailable)?;
            let latest_snapshot = route.retained_wave_lengths[latest_wave]
                .checked_sub(1)
                .ok_or(AttachmentError::RetainedSnapshotUnavailable)?;
            route = self.extend_nested_terminal_pair_from_wave(
                &route,
                latest_wave,
                latest_snapshot,
                replacements.clone(),
            )?;
            route.validate_depth_label(&folded_label)?;
            for label in retained_checkpoint_labels {
                route.validate_depth_label(label)?;
            }
        }
        Ok((route, Some(folded_label)))
    }

    /// Applies checkpoint-rooted storms in plan order, allowing a later storm
    /// to return to an earlier nested anchor. Each pair starts from that
    /// checkpoint's exact pins. The source wave/depth label and every emitted
    /// checkpoint-root label are revalidated after subsequent folds, so a
    /// cycle cannot silently redirect a handoff. The reference is silent on
    /// cyclic checkpoint storms; v1 preserves order and exact routes, and
    /// returns no partial result if a label or replacement fails. Empty plans
    /// still validate their source handoff.
    pub fn extend_nested_terminal_pair_checkpoint_storms(
        &self,
        previous: &ReboundPathResolution,
        storms: &[(&ReboundPathCheckpoint, &[[PinnedDatabase; 2]])],
    ) -> Result<ReboundPathResolution, AttachmentError> {
        let mut route = previous.clone();
        let mut retained_checkpoint_labels = Vec::new();
        for (checkpoint, replacement_waves) in storms {
            checkpoint.validate_depth_identity()?;
            route.validate_depth_label(&checkpoint.depth_label)?;
            if replacement_waves.is_empty() {
                let (folded, _) = self
                    .extend_nested_terminal_pair_chain_from_checkpoint_with_labels(
                        &route,
                        checkpoint,
                        &[],
                        &retained_checkpoint_labels,
                    )?;
                route = folded;
                continue;
            }
            for replacements in *replacement_waves {
                let (folded, checkpoint_label) = self
                    .extend_nested_terminal_pair_chain_from_checkpoint_with_labels(
                        &route,
                        checkpoint,
                        std::slice::from_ref(replacements),
                        &retained_checkpoint_labels,
                    )?;
                let label = checkpoint_label
                    .ok_or(AttachmentError::RetainedSnapshotUnavailable)?;
                retained_checkpoint_labels.push(label);
                route = folded;
            }
        }
        Ok(route)
    }

    /// Applies rounds of checkpoint storms selected by retained depth labels.
    /// Each round may select a different depth in the same nested route; all
    /// selected labels remain bound to their original route and snapshot
    /// identities across later folds. The reference is silent on composing
    /// depth-selected closure storms; v1 validates labels at every round
    /// boundary and returns no partial route if a selected depth or closure
    /// fails.
    pub fn extend_nested_terminal_pair_checkpoint_storm_rounds_from_depth_labels(
        &self,
        previous: &ReboundPathResolution,
        rounds: &[&[(&NestedPairDepthLabel, &[[PinnedDatabase; 2]])]],
    ) -> Result<ReboundPathResolution, AttachmentError> {
        let mut route = previous.clone();
        let mut retained_labels = Vec::new();
        for round in rounds {
            let checkpoints = round
                .iter()
                .map(|(label, _)| route.handoff_checkpoint_for_depth_label(label))
                .collect::<Result<Vec<_>, AttachmentError>>()?;
            let storms = round
                .iter()
                .zip(&checkpoints)
                .map(|((_, replacements), checkpoint)| (checkpoint, *replacements))
                .collect::<Vec<_>>();
            route = self.extend_nested_terminal_pair_checkpoint_storms(&route, &storms)?;
            retained_labels.extend(round.iter().map(|(label, _)| (*label).clone()));
            for label in &retained_labels {
                route.validate_depth_label(label)?;
            }
        }
        Ok(route)
    }

    /// Applies depth-labelled checkpoint storm rounds with stable omission
    /// slots. A slot keeps the route identity of its first selected label;
    /// `None` leaves that slot unchanged, and a later label cannot take its
    /// place. The reference is silent on nested paired omissions, so v1 fixes
    /// the slot count from the first round and revalidates every selected
    /// identity after each fold. Failure returns no partial route.
    pub fn extend_nested_terminal_pair_checkpoint_storm_rounds_from_depth_labels_with_omissions(
        &self,
        previous: &ReboundPathResolution,
        rounds: &[&[Option<(&NestedPairDepthLabel, &[[PinnedDatabase; 2]])>]],
    ) -> Result<ReboundPathResolution, AttachmentError> {
        let mut route = previous.clone();
        let mut retained_labels: Option<Vec<Option<NestedPairDepthLabel>>> = None;
        for round in rounds {
            let labels = retained_labels.get_or_insert_with(|| vec![None; round.len()]);
            if labels.len() != round.len() {
                return Err(AttachmentError::RetainedSnapshotUnavailable);
            }

            let checkpoints = round
                .iter()
                .enumerate()
                .map(|(slot, plan)| match plan {
                    Some((label, replacements)) => {
                        if let Some(retained) = &labels[slot] {
                            if retained != *label {
                                return Err(AttachmentError::RetainedSnapshotUnavailable);
                            }
                        } else {
                            labels[slot] = Some((*label).clone());
                        }
                        let checkpoint = route.handoff_checkpoint_for_depth_label(label)?;
                        Ok(Some((checkpoint, *replacements)))
                    }
                    None => Ok(None),
                })
                .collect::<Result<Vec<_>, AttachmentError>>()?;
            let storms = checkpoints
                .iter()
                .filter_map(|checkpoint| {
                    checkpoint
                        .as_ref()
                        .map(|(checkpoint, replacements)| (checkpoint, *replacements))
                })
                .collect::<Vec<_>>();
            route = self.extend_nested_terminal_pair_checkpoint_storms(&route, &storms)?;
            for label in labels.iter().flatten() {
                route.validate_depth_label(label)?;
            }
        }
        Ok(route)
    }

    /// Applies sparse storm rounds keyed by exact nested depth labels. Round
    /// widths may vary because an omitted entry is not assigned a new slot;
    /// every label previously selected or explicitly omitted remains tracked
    /// and is revalidated after each fold. The reference is silent on sparse
    /// nested storm batches, so v1 treats labels as stable keys, orders each
    /// round by `(wave, depth)`, and rejects a duplicate key within one round.
    /// This makes a sparse round deterministic regardless of entry order while
    /// keeping each replacement wave's internal order intact. Failure returns
    /// no partial route.
    pub fn extend_nested_terminal_pair_sparse_checkpoint_storm_rounds_from_depth_labels(
        &self,
        previous: &ReboundPathResolution,
        rounds: &[&[(&NestedPairDepthLabel, Option<&[[PinnedDatabase; 2]]>)]],
    ) -> Result<ReboundPathResolution, AttachmentError> {
        let mut route = previous.clone();
        let mut retained_labels: Vec<NestedPairDepthLabel> = Vec::new();
        for round in rounds {
            let mut seen_labels: Vec<NestedPairDepthLabel> = Vec::with_capacity(round.len());
            let mut checkpoints = round
                .iter()
                .map(|(label, replacements)| {
                    let (wave, depth) = route.retained_position_for_depth_label(label)?;
                    if seen_labels.contains(label) {
                        return Err(AttachmentError::RetainedSnapshotUnavailable);
                    }
                    seen_labels.push((*label).clone());
                    if !retained_labels.contains(label) {
                        retained_labels.push((*label).clone());
                    }
                    let checkpoint = match replacements {
                        Some(replacement_waves) => {
                            let checkpoint = route.handoff_checkpoint(wave, depth)?;
                            Some((checkpoint, *replacement_waves))
                        }
                        None => None,
                    };
                    Ok((wave, depth, checkpoint))
                })
                .collect::<Result<Vec<_>, AttachmentError>>()?;
            checkpoints.sort_by_key(|(wave, depth, _)| (*wave, *depth));
            let storms = checkpoints
                .iter()
                .filter_map(|(_, _, checkpoint)| {
                    checkpoint
                        .as_ref()
                        .map(|(checkpoint, replacement_waves)| (checkpoint, *replacement_waves))
                })
                .collect::<Vec<_>>();
            route = self.extend_nested_terminal_pair_checkpoint_storms(&route, &storms)?;
            for label in &retained_labels {
                route.validate_depth_label(label)?;
            }
        }
        Ok(route)
    }

    /// Applies several sparse-round chains as one nested terminal-pair fold.
    /// Labels seen in an earlier chain remain bound to their original retained
    /// snapshots and are revalidated after every later round, including rounds
    /// that omit them. Every omission-only round also preserves the exact
    /// terminal route identity, even after an active round in the same chain.
    /// The reference is silent on cross-chain sparse folds; v1 carries depth
    /// identities across segment boundaries and returns no partial route if any
    /// round invalidates one.
    pub fn extend_nested_terminal_pair_sparse_checkpoint_storm_chains_from_depth_labels(
        &self,
        previous: &ReboundPathResolution,
        chains: &[&[&[(&NestedPairDepthLabel, Option<&[[PinnedDatabase; 2]]>)]]],
    ) -> Result<ReboundPathResolution, AttachmentError> {
        let mut route = previous.clone();
        let mut retained_labels: Vec<NestedPairDepthLabel> = Vec::new();
        for chain in chains {
            for round in *chain {
                let omission_only = round.iter().all(|(_, replacements)| match replacements {
                    Some(waves) => waves.is_empty(),
                    None => true,
                });
                let terminal_identity = omission_only.then(|| route.terminal_route_identity());
                route = self
                    .extend_nested_terminal_pair_sparse_checkpoint_storm_rounds_from_depth_labels(
                        &route,
                        std::slice::from_ref(round),
                    )?;
                for (label, _) in *round {
                    if !retained_labels.contains(label) {
                        retained_labels.push((*label).clone());
                    }
                }
                for label in &retained_labels {
                    route.validate_depth_label(label)?;
                }
                if let Some(identity) = &terminal_identity {
                    route.validate_terminal_route_identity(identity)?;
                }
            }
        }
        Ok(route)
    }

    /// Applies sparse nested terminal-pair chains only when they preserve a
    /// caller-captured terminal route identity. The identity is checked before
    /// and after the complete fold, so a foreign route or unexpected terminal
    /// change returns no result. The reference is silent on identity-guarded
    /// omission chains; v1 uses exact lineage and pin equality.
    pub fn extend_nested_terminal_pair_sparse_checkpoint_storm_chains_preserving_terminal_identity(
        &self,
        previous: &ReboundPathResolution,
        expected_terminal_identity: &NestedPairTerminalRouteIdentity,
        chains: &[&[&[(&NestedPairDepthLabel, Option<&[[PinnedDatabase; 2]]>)]]],
    ) -> Result<ReboundPathResolution, AttachmentError> {
        previous.validate_terminal_route_identity(expected_terminal_identity)?;
        let route = self
            .extend_nested_terminal_pair_sparse_checkpoint_storm_chains_from_depth_labels(
                previous, chains,
            )?;
        route.validate_terminal_route_identity(expected_terminal_identity)?;
        Ok(route)
    }

    /// Applies sparse nested terminal-pair chains while checking the exact
    /// terminal route identity after each chain. The expected identities must
    /// have one entry per chain and bind that chain's computed lineage and
    /// pins. The reference is silent on intermediate chain-boundary checks;
    /// v1 rejects drift immediately, even when a later chain would restore the
    /// final pins.
    pub fn extend_nested_terminal_pair_sparse_checkpoint_storm_chains_validating_terminal_identities(
        &self,
        previous: &ReboundPathResolution,
        chains: &[&[&[(&NestedPairDepthLabel, Option<&[[PinnedDatabase; 2]]>)]]],
        expected_terminal_identities: &[NestedPairTerminalRouteIdentity],
    ) -> Result<ReboundPathResolution, AttachmentError> {
        if chains.len() != expected_terminal_identities.len() {
            return Err(AttachmentError::RetainedSnapshotUnavailable);
        }

        let mut route = previous.clone();
        for (chain, expected_identity) in chains.iter().zip(expected_terminal_identities) {
            route = self
                .extend_nested_terminal_pair_sparse_checkpoint_storm_chains_from_depth_labels(
                    &route,
                    &[*chain],
                )?;
            route.validate_terminal_route_identity(expected_identity)?;
        }
        Ok(route)
    }

    /// Applies sparse nested terminal-pair rebind chains and captures the
    /// computed terminal identity at each chain boundary. The reference is
    /// silent on returning a route-identity history; v1 reports the exact
    /// lineage and ordered terminal pins after every chain and returns no
    /// partial history if a later chain fails.
    pub fn extend_nested_terminal_pair_sparse_checkpoint_storm_chains_capturing_terminal_identities(
        &self,
        previous: &ReboundPathResolution,
        chains: &[&[&[(&NestedPairDepthLabel, Option<&[[PinnedDatabase; 2]]>)]]],
    ) -> Result<(ReboundPathResolution, Vec<NestedPairTerminalRouteIdentity>), AttachmentError> {
        let mut route = previous.clone();
        let mut terminal_identities = Vec::with_capacity(chains.len());
        for chain in chains {
            route = self
                .extend_nested_terminal_pair_sparse_checkpoint_storm_chains_from_depth_labels(
                    &route,
                    &[*chain],
                )?;
            terminal_identities.push(route.terminal_route_identity());
        }
        Ok((route, terminal_identities))
    }

    /// Applies sparse nested terminal-pair rebind chains and records the
    /// terminal identity on both sides of every chain. The reference is silent
    /// on identity transitions across fold boundaries; v1 reports adjacent
    /// before/after identities, including equal identities for no-op chains,
    /// and returns no partial transition list if a chain fails.
    pub fn extend_nested_terminal_pair_sparse_checkpoint_storm_chains_capturing_terminal_identity_transitions(
        &self,
        previous: &ReboundPathResolution,
        chains: &[&[&[(&NestedPairDepthLabel, Option<&[[PinnedDatabase; 2]]>)]]],
    ) -> Result<(
        ReboundPathResolution,
        Vec<(NestedPairTerminalRouteIdentity, NestedPairTerminalRouteIdentity)>,
    ), AttachmentError> {
        let mut route = previous.clone();
        let mut transitions = Vec::with_capacity(chains.len());
        for chain in chains {
            let before = route.terminal_route_identity();
            route = self
                .extend_nested_terminal_pair_sparse_checkpoint_storm_chains_from_depth_labels(
                    &route,
                    &[*chain],
                )?;
            let after = route.terminal_route_identity();
            transitions.push((before, after));
        }
        Ok((route, transitions))
    }

    /// Applies sparse nested terminal-pair rebind chains only when every
    /// before/after terminal identity matches the supplied transition. The
    /// reference is silent on validating paired identity edges across fold
    /// chains; v1 checks both ends at each boundary and rejects drift before
    /// any later chain can mask it by restoring the same terminal pins.
    pub fn extend_nested_terminal_pair_sparse_checkpoint_storm_chains_validating_terminal_identity_transitions(
        &self,
        previous: &ReboundPathResolution,
        chains: &[&[&[(&NestedPairDepthLabel, Option<&[[PinnedDatabase; 2]]>)]]],
        expected_transitions: &[(NestedPairTerminalRouteIdentity, NestedPairTerminalRouteIdentity)],
    ) -> Result<ReboundPathResolution, AttachmentError> {
        if chains.len() != expected_transitions.len() {
            return Err(AttachmentError::RetainedSnapshotUnavailable);
        }

        let mut route = previous.clone();
        for (chain, (expected_before, expected_after)) in
            chains.iter().zip(expected_transitions)
        {
            route.validate_terminal_route_identity(expected_before)?;
            route = self
                .extend_nested_terminal_pair_sparse_checkpoint_storm_chains_from_depth_labels(
                    &route,
                    &[*chain],
                )?;
            route.validate_terminal_route_identity(expected_after)?;
        }
        Ok(route)
    }

    /// Applies nested terminal-pair rebind cascades, where each cascade may
    /// contain several sparse rebind chains, and validates the exact identity
    /// before and after every cascade. The reference is silent on grouped
    /// cascade boundaries; v1 rejects a stale edge before a later cascade can
    /// restore its terminal pins and returns no partial transition history.
    pub fn extend_nested_terminal_pair_sparse_checkpoint_storm_rebind_cascades_validating_terminal_identity_transitions(
        &self,
        previous: &ReboundPathResolution,
        rebind_cascades: &[&[&[&[(&NestedPairDepthLabel, Option<&[[PinnedDatabase; 2]]>)]]]],
        expected_transitions: &[(NestedPairTerminalRouteIdentity, NestedPairTerminalRouteIdentity)],
    ) -> Result<(
        ReboundPathResolution,
        Vec<(NestedPairTerminalRouteIdentity, NestedPairTerminalRouteIdentity)>,
    ), AttachmentError> {
        if rebind_cascades.len() != expected_transitions.len() {
            return Err(AttachmentError::RetainedSnapshotUnavailable);
        }

        let mut route = previous.clone();
        let mut transitions = Vec::with_capacity(rebind_cascades.len());
        for (cascade, (expected_before, expected_after)) in
            rebind_cascades.iter().zip(expected_transitions)
        {
            let before = route.terminal_route_identity();
            route.validate_terminal_route_identity(expected_before)?;
            route = self
                .extend_nested_terminal_pair_sparse_checkpoint_storm_chains_from_depth_labels(
                    &route,
                    *cascade,
                )?;
            route.validate_terminal_route_identity(expected_after)?;
            let after = route.terminal_route_identity();
            transitions.push((before, after));
        }

        Ok((route, transitions))
    }

    /// Applies sparse paired terminal pin folds one round at a time, checking
    /// the expected terminal identity before and after every fold. Depth labels
    /// selected by earlier folds remain bound and are revalidated after each
    /// later fold. The reference is silent on per-fold identity edges; v1
    /// rejects drift at the first changed edge and returns no partial route.
    pub fn extend_nested_terminal_pair_sparse_checkpoint_storm_pin_folds_validating_terminal_identity_transitions(
        &self,
        previous: &ReboundPathResolution,
        pin_folds: &[&[(&NestedPairDepthLabel, Option<&[[PinnedDatabase; 2]]>)]],
        expected_transitions: &[(NestedPairTerminalRouteIdentity, NestedPairTerminalRouteIdentity)],
    ) -> Result<ReboundPathResolution, AttachmentError> {
        if pin_folds.len() != expected_transitions.len() {
            return Err(AttachmentError::RetainedSnapshotUnavailable);
        }

        let mut route = previous.clone();
        let mut retained_labels: Vec<NestedPairDepthLabel> = Vec::new();
        for (fold, (expected_before, expected_after)) in
            pin_folds.iter().zip(expected_transitions)
        {
            route.validate_terminal_route_identity(expected_before)?;
            for (label, _) in *fold {
                route.validate_depth_label(label)?;
                if !retained_labels.contains(label) {
                    retained_labels.push((*label).clone());
                }
            }
            route = self
                .extend_nested_terminal_pair_sparse_checkpoint_storm_rounds_from_depth_labels(
                    &route,
                    &[*fold],
                )?;
            for label in &retained_labels {
                route.validate_depth_label(label)?;
            }
            route.validate_terminal_route_identity(expected_after)?;
        }

        Ok(route)
    }

    /// Applies paired rebinding and omission chains as consecutive terminal
    /// route segments. Each omission chain must preserve the terminal identity
    /// computed by its preceding rebind chain. Returns the final route and the
    /// verified terminal identity for every segment. The reference is silent
    /// on interleaved sparse route segments; v1 requires one omission chain
    /// per rebind chain and returns no partial result on drift.
    pub fn extend_nested_terminal_pair_sparse_checkpoint_storm_rebind_omission_segments_preserving_terminal_identities(
        &self,
        previous: &ReboundPathResolution,
        rebind_chains: &[&[&[(&NestedPairDepthLabel, Option<&[[PinnedDatabase; 2]]>)]]],
        omission_chains: &[&[&[(&NestedPairDepthLabel, Option<&[[PinnedDatabase; 2]]>)]]],
    ) -> Result<(ReboundPathResolution, Vec<NestedPairTerminalRouteIdentity>), AttachmentError> {
        if rebind_chains.len() != omission_chains.len() {
            return Err(AttachmentError::RetainedSnapshotUnavailable);
        }

        let mut route = previous.clone();
        let mut terminal_identities = Vec::with_capacity(rebind_chains.len());
        for (rebind_chain, omission_chain) in rebind_chains.iter().zip(omission_chains) {
            route = self
                .extend_nested_terminal_pair_sparse_checkpoint_storm_rebind_then_omission_chains_preserving_terminal_identity(
                    &route,
                    &[*rebind_chain],
                    &[*omission_chain],
                )?;
            terminal_identities.push(route.terminal_route_identity());
        }
        Ok((route, terminal_identities))
    }

    /// Applies paired sparse rebinding chains, captures the resulting terminal
    /// route identity, and carries that exact identity through later omission
    /// chains. Non-empty replacement waves are rejected in the omission phase.
    /// The reference is silent on this composed fold; v1 captures exact
    /// lineage and pin equality at the rebind/omission boundary.
    pub fn extend_nested_terminal_pair_sparse_checkpoint_storm_rebind_then_omission_chains_preserving_terminal_identity(
        &self,
        previous: &ReboundPathResolution,
        rebind_chains: &[&[&[(&NestedPairDepthLabel, Option<&[[PinnedDatabase; 2]]>)]]],
        omission_chains: &[&[&[(&NestedPairDepthLabel, Option<&[[PinnedDatabase; 2]]>)]]],
    ) -> Result<ReboundPathResolution, AttachmentError> {
        for chain in omission_chains {
            for round in *chain {
                for entry in round.iter() {
                    if matches!(entry.1, Some(replacements) if !replacements.is_empty()) {
                        return Err(AttachmentError::RetainedSnapshotUnavailable);
                    }
                }
            }
        }

        let rebound_route = self
            .extend_nested_terminal_pair_sparse_checkpoint_storm_chains_from_depth_labels(
                previous,
                rebind_chains,
            )?;
        let rebound_identity = rebound_route.terminal_route_identity();
        self.extend_nested_terminal_pair_sparse_checkpoint_storm_chains_preserving_terminal_identity(
            &rebound_route,
            &rebound_identity,
            omission_chains,
        )
    }

    /// Applies a sparse paired rebind fold only when it reaches the caller's
    /// expected terminal identity, then carries that exact identity through
    /// nested omission chains. The reference is silent on guarding this
    /// boundary across omission folds; v1 rejects a stale rebind identity or
    /// any omission drift and returns no partial route.
    pub fn extend_nested_terminal_pair_sparse_checkpoint_storm_rebind_then_omission_chains_validating_rebound_terminal_identity(
        &self,
        previous: &ReboundPathResolution,
        rebind_chains: &[&[&[(&NestedPairDepthLabel, Option<&[[PinnedDatabase; 2]]>)]]],
        expected_rebound_terminal_identity: &NestedPairTerminalRouteIdentity,
        omission_chains: &[&[&[(&NestedPairDepthLabel, Option<&[[PinnedDatabase; 2]]>)]]],
    ) -> Result<ReboundPathResolution, AttachmentError> {
        let rebound_route = self
            .extend_nested_terminal_pair_sparse_checkpoint_storm_chains_from_depth_labels(
                previous,
                rebind_chains,
            )?;
        rebound_route.validate_terminal_route_identity(expected_rebound_terminal_identity)?;
        self.extend_nested_terminal_pair_sparse_checkpoint_storm_chains_preserving_terminal_identity(
            &rebound_route,
            expected_rebound_terminal_identity,
            omission_chains,
        )
    }

    /// Applies a sparse rebind fold after verifying its terminal identity, then
    /// records the exact before/after identity for each paired omission chain.
    /// Omission chains reject active replacement waves, and any drift aborts
    /// the whole operation without returning a partial transition list. The
    /// reference is silent on this composition; v1 treats each omission-chain
    /// boundary as an auditable identity-preservation edge.
    pub fn extend_nested_terminal_pair_sparse_checkpoint_storm_rebind_then_omission_chains_capturing_terminal_identity_transitions(
        &self,
        previous: &ReboundPathResolution,
        rebind_chains: &[&[&[(&NestedPairDepthLabel, Option<&[[PinnedDatabase; 2]]>)]]],
        expected_rebound_terminal_identity: &NestedPairTerminalRouteIdentity,
        omission_chains: &[&[&[(&NestedPairDepthLabel, Option<&[[PinnedDatabase; 2]]>)]]],
    ) -> Result<(
        ReboundPathResolution,
        Vec<(NestedPairTerminalRouteIdentity, NestedPairTerminalRouteIdentity)>,
    ), AttachmentError> {
        for chain in omission_chains {
            for round in *chain {
                if round.iter().any(|(_, replacements)| {
                    matches!(replacements, Some(waves) if !waves.is_empty())
                }) {
                    return Err(AttachmentError::RetainedSnapshotUnavailable);
                }
            }
        }

        let rebound_route = self
            .extend_nested_terminal_pair_sparse_checkpoint_storm_chains_from_depth_labels(
                previous,
                rebind_chains,
            )?;
        rebound_route.validate_terminal_route_identity(expected_rebound_terminal_identity)?;

        let mut route = rebound_route;
        let mut transitions = Vec::with_capacity(omission_chains.len());
        for omission_chain in omission_chains {
            let before = route.terminal_route_identity();
            route = self
                .extend_nested_terminal_pair_sparse_checkpoint_storm_chains_from_depth_labels(
                    &route,
                    &[*omission_chain],
                )?;
            route.validate_terminal_route_identity(expected_rebound_terminal_identity)?;
            let after = route.terminal_route_identity();
            transitions.push((before, after));
        }

        Ok((route, transitions))
    }

    /// Applies paired rebind/omission segments and captures the terminal
    /// identity before rebind, after rebind, and after its paired omission
    /// fold. Omission segments reject active replacement waves and must retain
    /// the identity produced by their rebind. The reference is silent on this
    /// three-boundary audit; v1 returns no partial transition history on error.
    pub fn extend_nested_terminal_pair_sparse_checkpoint_storm_rebind_omission_segments_capturing_terminal_identity_transitions(
        &self,
        previous: &ReboundPathResolution,
        rebind_chains: &[&[&[(&NestedPairDepthLabel, Option<&[[PinnedDatabase; 2]]>)]]],
        omission_chains: &[&[&[(&NestedPairDepthLabel, Option<&[[PinnedDatabase; 2]]>)]]],
    ) -> Result<(
        ReboundPathResolution,
        Vec<(
            NestedPairTerminalRouteIdentity,
            NestedPairTerminalRouteIdentity,
            NestedPairTerminalRouteIdentity,
        )>,
    ), AttachmentError> {
        if rebind_chains.len() != omission_chains.len() {
            return Err(AttachmentError::RetainedSnapshotUnavailable);
        }
        for chain in omission_chains {
            for round in *chain {
                if round.iter().any(|(_, replacements)| {
                    matches!(replacements, Some(waves) if !waves.is_empty())
                }) {
                    return Err(AttachmentError::RetainedSnapshotUnavailable);
                }
            }
        }

        let mut route = previous.clone();
        let mut transitions = Vec::with_capacity(rebind_chains.len());
        for (rebind_chain, omission_chain) in rebind_chains.iter().zip(omission_chains) {
            let before_rebind = route.terminal_route_identity();
            route = self
                .extend_nested_terminal_pair_sparse_checkpoint_storm_chains_from_depth_labels(
                    &route,
                    &[*rebind_chain],
                )?;
            let after_rebind = route.terminal_route_identity();
            route = self
                .extend_nested_terminal_pair_sparse_checkpoint_storm_chains_preserving_terminal_identity(
                    &route,
                    &after_rebind,
                    &[*omission_chain],
                )?;
            let after_omission = route.terminal_route_identity();
            transitions.push((before_rebind, after_rebind, after_omission));
        }

        Ok((route, transitions))
    }

    /// Applies paired rebind/omission segments only when each segment matches
    /// its expected identity before rebind, after rebind, and after omission.
    /// Omission segments reject active replacement waves. The reference is
    /// silent on validating this three-edge fold; v1 checks every boundary and
    /// returns no partial route when an identity or segment count drifts.
    pub fn extend_nested_terminal_pair_sparse_checkpoint_storm_rebind_omission_segments_validating_terminal_identity_transitions(
        &self,
        previous: &ReboundPathResolution,
        rebind_chains: &[&[&[(&NestedPairDepthLabel, Option<&[[PinnedDatabase; 2]]>)]]],
        omission_chains: &[&[&[(&NestedPairDepthLabel, Option<&[[PinnedDatabase; 2]]>)]]],
        expected_transitions: &[(
            NestedPairTerminalRouteIdentity,
            NestedPairTerminalRouteIdentity,
            NestedPairTerminalRouteIdentity,
        )],
    ) -> Result<ReboundPathResolution, AttachmentError> {
        if rebind_chains.len() != omission_chains.len()
            || rebind_chains.len() != expected_transitions.len()
        {
            return Err(AttachmentError::RetainedSnapshotUnavailable);
        }
        for chain in omission_chains {
            for round in *chain {
                if round.iter().any(|(_, replacements)| {
                    matches!(replacements, Some(waves) if !waves.is_empty())
                }) {
                    return Err(AttachmentError::RetainedSnapshotUnavailable);
                }
            }
        }

        let mut route = previous.clone();
        for ((rebind_chain, omission_chain), (expected_before, expected_rebound, expected_omitted)) in
            rebind_chains
                .iter()
                .zip(omission_chains)
                .zip(expected_transitions)
        {
            route.validate_terminal_route_identity(expected_before)?;
            route = self
                .extend_nested_terminal_pair_sparse_checkpoint_storm_chains_from_depth_labels(
                    &route,
                    &[*rebind_chain],
                )?;
            route.validate_terminal_route_identity(expected_rebound)?;
            route = self
                .extend_nested_terminal_pair_sparse_checkpoint_storm_chains_preserving_terminal_identity(
                    &route,
                    expected_rebound,
                    &[*omission_chain],
                )?;
            route.validate_terminal_route_identity(expected_omitted)?;
        }

        Ok(route)
    }

    /// Folds checkpoint-rooted terminal-pair storms across independent parent
    /// routes. Each table row is `(route, storms)` and resolves only checkpoints
    /// captured from that row's route; row order and anchor identity are kept
    /// independently. The reference is silent on tabular multi-parent folds,
    /// so v1 validates every row through the single-route checkpoint rules and
    /// returns no partial batch if any row or fold fails.
    pub fn extend_sibling_terminal_pair_checkpoint_storms(
        &self,
        paths: &[(
            &ReboundPathResolution,
            &[(&ReboundPathCheckpoint, &[[PinnedDatabase; 2]])],
        )],
    ) -> Result<SiblingRebindResolution, AttachmentError> {
        let routes = paths
            .iter()
            .map(|(previous, storms)| {
                self.extend_nested_terminal_pair_checkpoint_storms(previous, storms)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(SiblingRebindResolution { routes })
    }

    /// Continues a sibling checkpoint-storm result with one plan per parent
    /// row. Plans are paired with routes by input index, and each checkpoint
    /// is validated only against the route at that index. The reference is
    /// silent on nested multi-parent folds; v1 requires matching row counts
    /// and revalidates every pre-existing depth identity after the folds.
    /// No partial result is returned if any row fails.
    pub fn extend_sibling_terminal_pair_checkpoint_storms_from_siblings(
        &self,
        previous: &SiblingRebindResolution,
        storms_by_row: &[&[(&ReboundPathCheckpoint, &[[PinnedDatabase; 2]])]],
    ) -> Result<SiblingRebindResolution, AttachmentError> {
        if storms_by_row.len() != previous.routes.len() {
            return Err(AttachmentError::RetainedSnapshotUnavailable);
        }
        for (route, storms) in storms_by_row.iter().enumerate() {
            for (checkpoint, _) in *storms {
                previous.validate_handoff_checkpoint(route, checkpoint)?;
            }
        }
        let paths = previous
            .routes
            .iter()
            .zip(storms_by_row)
            .map(|(route, storms)| (route, *storms))
            .collect::<Vec<_>>();
        let folded = self.extend_sibling_terminal_pair_checkpoint_storms(&paths)?;
        for (previous_route, folded_route) in previous.routes.iter().zip(&folded.routes) {
            for (wave, length) in previous_route.retained_wave_lengths.iter().enumerate() {
                for depth in 0..*length {
                    let label = previous_route.retained_depth_label(wave, depth)?;
                    folded_route.validate_depth_label(&label)?;
                }
            }
        }
        Ok(folded)
    }

    /// Continues sibling checkpoint storms from explicit retained
    /// `(wave, depth)` coordinates. Each coordinate is resolved within its
    /// own sibling row before any route is folded, so equal pins at another
    /// row or depth cannot substitute for the selected handoff. The reference
    /// is silent on multi-parent depth selection; v1 binds every selection to
    /// its original row identity and returns no partial result on failure.
    pub fn extend_sibling_terminal_pair_checkpoint_storms_from_depths(
        &self,
        previous: &SiblingRebindResolution,
        storms_by_row: &[&[(usize, usize, &[[PinnedDatabase; 2]])]],
    ) -> Result<SiblingRebindResolution, AttachmentError> {
        if storms_by_row.len() != previous.routes.len() {
            return Err(AttachmentError::RetainedSnapshotUnavailable);
        }

        let checkpoints_by_row = storms_by_row
            .iter()
            .enumerate()
            .map(|(row, storms)| {
                storms
                    .iter()
                    .map(|(wave, depth, _)| previous.handoff_checkpoint(row, *wave, *depth))
                    .collect::<Result<Vec<_>, _>>()
            })
            .collect::<Result<Vec<_>, _>>()?;
        let checkpoint_storms_by_row = storms_by_row
            .iter()
            .zip(&checkpoints_by_row)
            .map(|(storms, checkpoints)| {
                storms
                    .iter()
                    .zip(checkpoints)
                    .map(|((_, _, replacements), checkpoint)| (checkpoint, *replacements))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let checkpoint_storm_rows = checkpoint_storms_by_row
            .iter()
            .map(Vec::as_slice)
            .collect::<Vec<_>>();
        self.extend_sibling_terminal_pair_checkpoint_storms_from_siblings(
            previous,
            &checkpoint_storm_rows,
        )
    }

    /// Continues sibling checkpoint storms from exact retained depth labels.
    /// Each label is checked against its indexed sibling row before any fold
    /// starts, so equal pins and coordinates from another parent cannot be
    /// substituted. The reference is silent on label-selected multi-parent
    /// folds; v1 preserves each selected row identity and returns no partial
    /// result if any label or replacement fails.
    pub fn extend_sibling_terminal_pair_checkpoint_storms_from_depth_labels(
        &self,
        previous: &SiblingRebindResolution,
        storms_by_row: &[&[(&NestedPairDepthLabel, &[[PinnedDatabase; 2]])]],
    ) -> Result<SiblingRebindResolution, AttachmentError> {
        if storms_by_row.len() != previous.routes.len() {
            return Err(AttachmentError::RetainedSnapshotUnavailable);
        }

        let checkpoint_storms_by_row = storms_by_row
            .iter()
            .enumerate()
            .map(|(row, storms)| {
                let route = previous
                    .routes
                    .get(row)
                    .ok_or(AttachmentError::RetainedSnapshotUnavailable)?;
                storms
                    .iter()
                    .map(|(label, replacements)| {
                        let checkpoint = route.handoff_checkpoint_for_depth_label(label)?;
                        Ok((checkpoint, *replacements))
                    })
                    .collect::<Result<Vec<_>, AttachmentError>>()
            })
            .collect::<Result<Vec<_>, _>>()?;
        let checkpoint_storm_rows = checkpoint_storms_by_row
            .iter()
            .map(|storms| {
                storms
                    .iter()
                    .map(|(checkpoint, replacements)| (checkpoint, *replacements))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let checkpoint_storm_row_slices = checkpoint_storm_rows
            .iter()
            .map(Vec::as_slice)
            .collect::<Vec<_>>();
        self.extend_sibling_terminal_pair_checkpoint_storms_from_siblings(
            previous,
            &checkpoint_storm_row_slices,
        )
    }

    /// Applies ordered rounds from exact sibling depth labels captured before
    /// the first round. A source label remains bound to its original row and
    /// retained snapshot throughout the continuation, including when the
    /// selected rows have equal pins. The reference does not define repeated
    /// label-selected multi-parent folds; v1 validates every round against the
    /// accumulated route and returns no partial result if one fails.
    pub fn extend_sibling_terminal_pair_checkpoint_storm_rounds_from_depth_labels(
        &self,
        previous: &SiblingRebindResolution,
        rounds: &[&[&[(&NestedPairDepthLabel, &[[PinnedDatabase; 2]])]]],
    ) -> Result<SiblingRebindResolution, AttachmentError> {
        let mut folded = previous.clone();
        for round in rounds {
            folded = self.extend_sibling_terminal_pair_checkpoint_storms_from_depth_labels(
                &folded, round,
            )?;
        }
        Ok(folded)
    }

    /// Applies rounds of label-selected checkpoint storms with one slot per
    /// sibling route. `None` retains that row unchanged for the round; it is
    /// not compacted away, so a later row plan cannot inherit its identity.
    /// Present plans are validated against their original row and all earlier
    /// labels are revalidated after each fold. The reference is silent on
    /// paired closure omissions; v1 keeps row positions stable and returns no
    /// partial result if any present plan fails.
    pub fn extend_sibling_terminal_pair_checkpoint_storm_rounds_from_depth_labels_with_omissions(
        &self,
        previous: &SiblingRebindResolution,
        rounds: &[&[Option<(&NestedPairDepthLabel, &[[PinnedDatabase; 2]])>]],
    ) -> Result<SiblingRebindResolution, AttachmentError> {
        let mut folded = previous.clone();
        for round in rounds {
            if round.len() != folded.routes.len() {
                return Err(AttachmentError::RetainedSnapshotUnavailable);
            }
            let checkpoint_storms_by_row = round
                .iter()
                .enumerate()
                .map(|(row, plan)| match plan {
                    Some((label, replacements)) => {
                        let route = folded
                            .routes
                            .get(row)
                            .ok_or(AttachmentError::RetainedSnapshotUnavailable)?;
                        let checkpoint = route.handoff_checkpoint_for_depth_label(label)?;
                        Ok(vec![(checkpoint, *replacements)])
                    }
                    None => Ok(Vec::new()),
                })
                .collect::<Result<Vec<_>, AttachmentError>>()?;
            let checkpoint_storm_rows = checkpoint_storms_by_row
                .iter()
                .map(|storms| {
                    storms
                        .iter()
                        .map(|(checkpoint, replacements)| (checkpoint, *replacements))
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            let checkpoint_storm_row_slices = checkpoint_storm_rows
                .iter()
                .map(Vec::as_slice)
                .collect::<Vec<_>>();
            folded = self.extend_sibling_terminal_pair_checkpoint_storms_from_siblings(
                &folded,
                &checkpoint_storm_row_slices,
            )?;
        }
        Ok(folded)
    }

    /// Applies ordered rounds of depth-selected sibling checkpoint storms.
    /// Each round contains one plan slice per sibling row, and its coordinates
    /// are resolved against the result of the preceding round. The reference
    /// is silent on composing multi-parent continuation rounds; v1 preserves
    /// all earlier row-scoped labels and returns no partial result if any
    /// round selects a missing depth or fails to fold.
    pub fn extend_sibling_terminal_pair_checkpoint_storm_rounds_from_depths(
        &self,
        previous: &SiblingRebindResolution,
        rounds: &[&[&[(usize, usize, &[[PinnedDatabase; 2]])]]],
    ) -> Result<SiblingRebindResolution, AttachmentError> {
        let mut folded = previous.clone();
        for round in rounds {
            folded = self.extend_sibling_terminal_pair_checkpoint_storms_from_depths(
                &folded, round,
            )?;
        }
        Ok(folded)
    }

    /// Rebinds each terminal pair in a storm from one saved checkpoint.
    /// Every fold starts from the same exact nested pins and appends that
    /// checkpoint route to retained history. Each emitted route receives its
    /// own wave/depth label, and every earlier emitted label is checked after
    /// each later fold. The source checkpoint must still match its original
    /// wave/depth in `previous`; empty storms validate that identity too.
    /// The reference is silent on checkpoint-rooted storm folds; v1 preserves
    /// exact pins and ordering and returns no partial route if a replacement
    /// fails.
    pub fn extend_nested_terminal_pair_storm_from_checkpoint(
        &self,
        previous: &ReboundPathResolution,
        checkpoint: &ReboundPathCheckpoint,
        replacement_waves: &[[PinnedDatabase; 2]],
    ) -> Result<ReboundPathResolution, AttachmentError> {
        self.extend_nested_terminal_pair_checkpoint_storms(
            previous,
            &[(checkpoint, replacement_waves)],
        )
    }

    /// Resolves independently rebound paths for sibling parent snapshots.
    /// Each input plan is `(parent, replacements)`; results keep input order
    /// and each route retains its own pre-rebind sessions. No partial batch is
    /// returned if any sibling path fails. Where sibling event ordering is
    /// unspecified, v1 resolves every path from its own exact pins.
    pub fn resolve_sibling_rebind_paths(
        &self,
        paths: &[(&AttachedDatabaseSession, &[PinnedDatabase])],
    ) -> Result<SiblingRebindResolution, AttachmentError> {
        let routes = paths
            .iter()
            .map(|(parent, replacements)| {
                self.resolve_nested_rebind_path(parent, replacements)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(SiblingRebindResolution { routes })
    }

    /// Applies one identical replacement path to every sibling parent while
    /// retaining each parent's route snapshots independently. Results keep
    /// parent order and are returned only when every sibling resolves.
    pub fn resolve_sibling_rebind_wave(
        &self,
        parents: &[AttachedDatabaseSession],
        replacements: &[PinnedDatabase],
    ) -> Result<SiblingRebindResolution, AttachmentError> {
        let paths = parents
            .iter()
            .map(|parent| (parent, replacements))
            .collect::<Vec<_>>();
        self.resolve_sibling_rebind_paths(&paths)
    }

    /// Extends independently selected sibling routes with another wave.
    /// Every earlier snapshot remains in its route's history, and input order
    /// is retained. A failed sibling extension returns no partial batch and
    /// leaves all supplied results unchanged.
    pub fn extend_sibling_rebind_paths(
        &self,
        paths: &[(&ReboundPathResolution, &[PinnedDatabase])],
    ) -> Result<SiblingRebindResolution, AttachmentError> {
        let routes = paths
            .iter()
            .map(|(previous, replacements)| {
                self.extend_nested_rebind_path(previous, replacements)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(SiblingRebindResolution { routes })
    }

    /// Applies one ordered terminal-depth pair to each sibling route. Every
    /// branch retains its own two pre-rebind snapshots, input order is stable,
    /// and no partial sibling batch is returned on failure.
    pub fn extend_sibling_terminal_pair_paths(
        &self,
        paths: &[(&ReboundPathResolution, &[PinnedDatabase; 2])],
    ) -> Result<SiblingRebindResolution, AttachmentError> {
        let routes = paths
            .iter()
            .map(|(previous, replacements)| {
                self.extend_nested_terminal_pair(previous, (**replacements).clone())
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(SiblingRebindResolution { routes })
    }

    /// Continues sibling routes from their own retained snapshots with one
    /// terminal-depth pair per route. `retained_session` is each route's
    /// flattened history index; results preserve input order and prior waves.
    /// The batch is returned only if every selected snapshot and closure is
    /// available.
    pub fn extend_sibling_terminal_pair_paths_from_retained(
        &self,
        paths: &[(&ReboundPathResolution, usize, &[PinnedDatabase; 2])],
    ) -> Result<SiblingRebindResolution, AttachmentError> {
        let routes = paths
            .iter()
            .map(|(previous, retained_session, replacements)| {
                self.extend_nested_terminal_pair_from_retained(
                    previous,
                    *retained_session,
                    (**replacements).clone(),
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(SiblingRebindResolution { routes })
    }

    /// Continues sibling routes from explicit retained-wave snapshots with
    /// one terminal-depth pair per route. Results preserve sibling order and
    /// each route's prior waves; the batch is atomic when any wave, snapshot,
    /// replacement or closure is unavailable.
    pub fn extend_sibling_terminal_pair_paths_from_waves(
        &self,
        paths: &[(&ReboundPathResolution, usize, usize, &[PinnedDatabase; 2])],
    ) -> Result<SiblingRebindResolution, AttachmentError> {
        let routes = paths
            .iter()
            .map(|(previous, wave, snapshot, replacements)| {
                self.extend_nested_terminal_pair_from_wave(
                    previous,
                    *wave,
                    *snapshot,
                    (**replacements).clone(),
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(SiblingRebindResolution { routes })
    }

    /// Applies one identical terminal-depth pair to sibling routes selected
    /// from retained waves. Each route resolves from its own exact snapshot,
    /// keeps its displaced final endpoint and earlier waves, and preserves
    /// input order. No partial sibling batch is returned on failure.
    pub fn extend_sibling_terminal_pair_wave_from_waves(
        &self,
        paths: &[(&ReboundPathResolution, usize, usize)],
        replacements: [PinnedDatabase; 2],
    ) -> Result<SiblingRebindResolution, AttachmentError> {
        let routes = paths
            .iter()
            .map(|(previous, wave, snapshot)| {
                self.extend_nested_terminal_pair_from_wave(
                    previous,
                    *wave,
                    *snapshot,
                    replacements.clone(),
                )
        })
        .collect::<Result<Vec<_>, _>>()?;
        Ok(SiblingRebindResolution { routes })
    }

    /// Applies one identical terminal-depth pair to the latest retained-wave
    /// root of every sibling route. The reference is silent on continuing a
    /// convergent post-storm pair; v1 uses the first snapshot in each latest
    /// wave as that route's root, retains the displaced final session as its
    /// own wave, and returns no partial batch if any branch fails.
    pub fn extend_sibling_terminal_pair_wave(
        &self,
        previous: &SiblingRebindResolution,
        replacements: [PinnedDatabase; 2],
    ) -> Result<SiblingRebindResolution, AttachmentError> {
        let routes = previous
            .routes
            .iter()
            .map(|route| {
                let latest_wave = route
                    .retained_wave_lengths
                    .len()
                    .checked_sub(1)
                    .ok_or(AttachmentError::RetainedSnapshotUnavailable)?;
                self.extend_nested_terminal_pair_from_wave(
                    route,
                    latest_wave,
                    0,
                    replacements.clone(),
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(SiblingRebindResolution { routes })
    }

    /// Applies an ordered chain of shared terminal-depth pairs across sibling
    /// routes. Each pair continues from the last snapshot in the latest wave,
    /// allowing the next pair to follow the newly rebound closure one depth
    /// farther. The reference does not define chained post-storm selection;
    /// v1 uses this retained-wave tail and returns no partial chain if any
    /// sibling or pair fails.
    pub fn extend_sibling_terminal_pair_chain(
        &self,
        previous: &SiblingRebindResolution,
        replacement_waves: &[[PinnedDatabase; 2]],
    ) -> Result<SiblingRebindResolution, AttachmentError> {
        let routes = previous
            .routes
            .iter()
            .map(|route| self.extend_nested_terminal_pair_chain(route, replacement_waves))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(SiblingRebindResolution { routes })
    }

    /// Repeats one identical rebind path across a completed sibling batch.
    /// Each branch extends from its own terminal session and keeps its earlier
    /// route snapshots; the returned batch preserves sibling order.
    pub fn extend_sibling_rebind_wave(
        &self,
        previous: &SiblingRebindResolution,
        replacements: &[PinnedDatabase],
    ) -> Result<SiblingRebindResolution, AttachmentError> {
        let paths = previous
            .routes
            .iter()
            .map(|route| (route, replacements))
            .collect::<Vec<_>>();
        self.extend_sibling_rebind_paths(&paths)
    }

    /// Resolves a chain of exact aliases from a retained session snapshot.
    /// Each edge is selected from the session opened at the preceding edge;
    /// a failure leaves the caller's snapshot untouched.
    pub fn resolve_nested_path(
        &self,
        parent: &AttachedDatabaseSession,
        aliases: &[&str],
    ) -> Result<AttachedDatabaseSession, AttachmentError> {
        let mut current = parent.clone();
        for alias in aliases {
            current = self.resolve_nested_for_alias(&current, alias)?;
        }
        Ok(current)
    }
}

/// The terminal session and route snapshots retained while resolving a
/// sequence of nested alias rebinds.
///
/// `retained_sessions` is ordered from the original parent through each
/// intermediate closure, with one snapshot recorded before each replacement.
/// The final session is the closure reached after the last replacement.
/// Cloned routes preserve lineage and retained-depth identities; separately
/// resolved folds receive fresh identities for their new retained snapshots.
#[derive(Clone, Debug)]
pub struct ReboundPathResolution {
    final_session: AttachedDatabaseSession,
    retained_sessions: Vec<AttachedDatabaseSession>,
    retained_wave_lengths: Vec<usize>,
    route_identity: Arc<()>,
    retained_snapshot_identities: Vec<Arc<()>>,
}

impl ReboundPathResolution {
    /// The closure reached after all requested replacements.
    pub fn final_session(&self) -> &AttachedDatabaseSession {
        &self.final_session
    }

    /// Captures the route lineage and exact pins of the current terminal
    /// closure. The reference leaves terminal identity across omitted nested
    /// rebinds unspecified; v1 binds it to the originating route and terminal
    /// pin set so an unchanged terminal remains verifiable across omissions.
    pub fn terminal_route_identity(&self) -> NestedPairTerminalRouteIdentity {
        NestedPairTerminalRouteIdentity::from_session(&self.route_identity, &self.final_session)
    }

    /// Verifies that this resolution still ends at the exact terminal route
    /// captured earlier, including its lineage and ordered attached pins.
    pub fn validate_terminal_route_identity(
        &self,
        identity: &NestedPairTerminalRouteIdentity,
    ) -> Result<(), AttachmentError> {
        if Arc::ptr_eq(&self.route_identity, &identity.route_identity)
            && identity.matches_session(&self.final_session)
        {
            Ok(())
        } else {
            Err(AttachmentError::RetainedSnapshotUnavailable)
        }
    }

    /// Snapshots retained before each replacement, in path order.
    pub fn retained_sessions(&self) -> &[AttachedDatabaseSession] {
        &self.retained_sessions
    }

    /// Snapshots retained by one resolution wave, in the order they were
    /// captured. The reference fixes exact historical pins but does not define
    /// how paired rebind snapshots are grouped; v1 keeps each call's boundary.
    pub fn retained_wave(&self, wave: usize) -> Option<&[AttachedDatabaseSession]> {
        let length = *self.retained_wave_lengths.get(wave)?;
        let start = self.retained_wave_lengths[..wave].iter().sum::<usize>();
        self.retained_sessions.get(start..start + length)
    }

    /// Labels one pre-rebind depth in a retained wave by its position, exact
    /// pin route, and originating route lineage. The reference leaves labels
    /// across folded storms unspecified; v1 rejects reuse from an independently
    /// resolved route even when its pins and coordinates happen to be equal.
    pub fn retained_depth_label(
        &self,
        wave: usize,
        depth: usize,
    ) -> Result<NestedPairDepthLabel, AttachmentError> {
        let session = self.retained_snapshot_at_depth(wave, depth)?;
        Ok(NestedPairDepthLabel::from_session(
            wave,
            depth,
            &self.route_identity,
            self.retained_snapshot_identity_at_depth(wave, depth)?,
            session,
        ))
    }

    fn retained_snapshot_at_depth(
        &self,
        wave: usize,
        depth: usize,
    ) -> Result<&AttachedDatabaseSession, AttachmentError> {
        let index = self.retained_snapshot_index(wave, depth)?;
        self.retained_sessions
            .get(index)
            .ok_or(AttachmentError::RetainedSnapshotUnavailable)
    }

    fn retained_snapshot_identity_at_depth(
        &self,
        wave: usize,
        depth: usize,
    ) -> Result<&Arc<()>, AttachmentError> {
        let index = self.retained_snapshot_index(wave, depth)?;
        self.retained_snapshot_identities
            .get(index)
            .ok_or(AttachmentError::RetainedSnapshotUnavailable)
    }

    fn retained_snapshot_index(
        &self,
        wave: usize,
        depth: usize,
    ) -> Result<usize, AttachmentError> {
        let length = *self
            .retained_wave_lengths
            .get(wave)
            .ok_or(AttachmentError::RetainedSnapshotUnavailable)?;
        if depth >= length {
            return Err(AttachmentError::RetainedSnapshotUnavailable);
        }
        let start = self.retained_wave_lengths[..wave]
            .iter()
            .try_fold(0usize, |total, length| total.checked_add(*length))
            .ok_or(AttachmentError::RetainedSnapshotUnavailable)?;
        let index = start
            .checked_add(depth)
            .ok_or(AttachmentError::RetainedSnapshotUnavailable)?;
        Ok(index)
    }

    fn validate_depth_label(
        &self,
        label: &NestedPairDepthLabel,
    ) -> Result<(), AttachmentError> {
        self.retained_position_for_depth_label(label).map(|_| ())
    }

    fn retained_position_for_depth_label(
        &self,
        label: &NestedPairDepthLabel,
    ) -> Result<(usize, usize), AttachmentError> {
        if !Arc::ptr_eq(&self.route_identity, &label.route_identity) {
            return Err(AttachmentError::RetainedSnapshotUnavailable);
        }
        let index = self
            .retained_snapshot_identities
            .iter()
            .position(|identity| Arc::ptr_eq(identity, &label.snapshot_identity))
            .ok_or(AttachmentError::RetainedSnapshotUnavailable)?;
        let session = self
            .retained_sessions
            .get(index)
            .ok_or(AttachmentError::RetainedSnapshotUnavailable)?;
        if !label.matches_session(session) {
            return Err(AttachmentError::RetainedSnapshotUnavailable);
        }

        let mut start = 0usize;
        for (wave, length) in self.retained_wave_lengths.iter().copied().enumerate() {
            let end = start
                .checked_add(length)
                .ok_or(AttachmentError::RetainedSnapshotUnavailable)?;
            if index < end {
                return Ok((wave, index - start));
            }
            start = end;
        }
        Err(AttachmentError::RetainedSnapshotUnavailable)
    }

    fn handoff_checkpoint_for_depth_label(
        &self,
        label: &NestedPairDepthLabel,
    ) -> Result<ReboundPathCheckpoint, AttachmentError> {
        let (wave, depth) = self.retained_position_for_depth_label(label)?;
        self.handoff_checkpoint(wave, depth)
    }

    fn compact_retained_depth_pair(
        &self,
        labels: &[NestedPairDepthLabel; 2],
    ) -> Result<Self, AttachmentError> {
        let positions = labels
            .iter()
            .map(|label| self.retained_position_for_depth_label(label))
            .collect::<Result<Vec<_>, _>>()?;
        let indices = positions
            .iter()
            .map(|(wave, depth)| self.retained_snapshot_index(*wave, *depth))
            .collect::<Result<Vec<_>, _>>()?;
        if indices[0] == indices[1] {
            return Err(AttachmentError::RetainedSnapshotUnavailable);
        }

        let retained_sessions = indices
            .iter()
            .map(|index| {
                self.retained_sessions
                    .get(*index)
                    .cloned()
                    .ok_or(AttachmentError::RetainedSnapshotUnavailable)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let retained_snapshot_identities = indices
            .iter()
            .map(|index| {
                self.retained_snapshot_identities
                    .get(*index)
                    .cloned()
                    .ok_or(AttachmentError::RetainedSnapshotUnavailable)
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self {
            final_session: self.final_session.clone(),
            retained_sessions,
            retained_wave_lengths: vec![2],
            route_identity: self.route_identity.clone(),
            retained_snapshot_identities,
        })
    }

    /// Saves one exact handoff route for replay after later rebinding
    /// cascades. The checkpoint owns the selected session, so extending or
    /// restoring another route cannot change its primary or attached pins.
    pub fn handoff_checkpoint(
        &self,
        wave: usize,
        snapshot: usize,
    ) -> Result<ReboundPathCheckpoint, AttachmentError> {
        let handoff = self.retained_snapshot_at_depth(wave, snapshot)?.clone();
        let depth_label = NestedPairDepthLabel::from_session(
            wave,
            snapshot,
            &self.route_identity,
            self.retained_snapshot_identity_at_depth(wave, snapshot)?,
            &handoff,
        );
        Ok(ReboundPathCheckpoint {
            handoff,
            depth_label,
        })
    }

    /// Takes ownership of the final closure and every retained route snapshot.
    pub fn into_parts(self) -> (AttachedDatabaseSession, Vec<AttachedDatabaseSession>) {
        (self.final_session, self.retained_sessions)
    }
}

/// A retained wave/depth coordinate and stable identity for one exact pinned
/// snapshot in a route lineage. Compaction can move the snapshot's current
/// coordinate while labels captured before the move continue to identify it.
#[derive(Clone, Debug)]
pub struct NestedPairDepthLabel {
    wave: usize,
    depth: usize,
    route_identity: Arc<()>,
    snapshot_identity: Arc<()>,
    primary: PackagePin,
    attached: Vec<(String, PackagePin)>,
}

impl NestedPairDepthLabel {
    fn from_session(
        wave: usize,
        depth: usize,
        route_identity: &Arc<()>,
        snapshot_identity: &Arc<()>,
        session: &AttachedDatabaseSession,
    ) -> Self {
        Self {
            wave,
            depth,
            route_identity: route_identity.clone(),
            snapshot_identity: snapshot_identity.clone(),
            primary: session.primary().pin().clone(),
            attached: session
                .attached()
                .map(|(name, database)| (name.to_owned(), database.pin().clone()))
                .collect(),
        }
    }

    fn matches_session(&self, session: &AttachedDatabaseSession) -> bool {
        self.primary.eq(session.primary().pin())
            && self
                .attached
                .iter()
                .map(|(name, pin)| (name.as_str(), pin))
                .eq(session
                    .attached()
                    .map(|(name, database)| (name, database.pin())))
    }

    /// Index of the retained wave containing this depth.
    pub fn wave(&self) -> usize {
        self.wave
    }

    /// Pre-rebind snapshot index within the retained wave.
    pub fn depth(&self) -> usize {
        self.depth
    }
}

impl PartialEq for NestedPairDepthLabel {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.route_identity, &other.route_identity)
            && Arc::ptr_eq(&self.snapshot_identity, &other.snapshot_identity)
            && self.primary == other.primary
            && self.attached == other.attached
    }
}

impl Eq for NestedPairDepthLabel {}

/// An opaque identity for one resolution's terminal closure. Equality binds
/// the complete pin route to its originating nested route lineage.
#[derive(Clone, Debug)]
pub struct NestedPairTerminalRouteIdentity {
    route_identity: Arc<()>,
    primary: PackagePin,
    attached: Vec<(String, PackagePin)>,
}

impl NestedPairTerminalRouteIdentity {
    fn from_session(route_identity: &Arc<()>, session: &AttachedDatabaseSession) -> Self {
        Self {
            route_identity: route_identity.clone(),
            primary: session.primary().pin().clone(),
            attached: session
                .attached()
                .map(|(alias, database)| (alias.to_owned(), database.pin().clone()))
                .collect(),
        }
    }

    fn matches_session(&self, session: &AttachedDatabaseSession) -> bool {
        self.primary.eq(session.primary().pin())
            && self
                .attached
                .iter()
                .map(|(alias, pin)| (alias.as_str(), pin))
                .eq(session
                    .attached()
                    .map(|(alias, database)| (alias, database.pin())))
    }
}

impl PartialEq for NestedPairTerminalRouteIdentity {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.route_identity, &other.route_identity)
            && self.primary == other.primary
            && self.attached == other.attached
    }
}

impl Eq for NestedPairTerminalRouteIdentity {}

/// An immutable copy of a retained nested route, suitable for replay after a
/// later route has been extended or rebound.
#[derive(Clone, Debug)]
pub struct ReboundPathCheckpoint {
    handoff: AttachedDatabaseSession,
    depth_label: NestedPairDepthLabel,
}

impl ReboundPathCheckpoint {
    /// The exact primary and attached pins saved at this handoff.
    pub fn handoff(&self) -> &AttachedDatabaseSession {
        &self.handoff
    }

    /// The original retained wave/depth coordinate and exact nested pins.
    pub fn depth_label(&self) -> &NestedPairDepthLabel {
        &self.depth_label
    }

    fn validate_depth_identity(&self) -> Result<(), AttachmentError> {
        if self.depth_label.matches_session(&self.handoff) {
            Ok(())
        } else {
            Err(AttachmentError::RetainedSnapshotUnavailable)
        }
    }
}

/// Independently resolved sibling paths, kept in input order.
#[derive(Clone, Debug, Default)]
pub struct SiblingRebindResolution {
    routes: Vec<ReboundPathResolution>,
}

impl SiblingRebindResolution {
    /// The sibling results in the same order as their input plans.
    pub fn routes(&self) -> &[ReboundPathResolution] {
        &self.routes
    }

    /// Labels one retained depth from a sibling row without dropping that
    /// row's route and snapshot identities. `route` is the row's index in the
    /// input plan. The reference does not define multi-parent label selection;
    /// v1 keeps each label bound to its source row even when another row has
    /// matching pins and coordinates.
    pub fn retained_depth_label(
        &self,
        route: usize,
        wave: usize,
        depth: usize,
    ) -> Result<NestedPairDepthLabel, AttachmentError> {
        self.routes
            .get(route)
            .ok_or(AttachmentError::RetainedSnapshotUnavailable)?
            .retained_depth_label(wave, depth)
    }

    /// Saves one retained route from a sibling row without dropping that
    /// row's route and snapshot identities. `route` is the row's index in the
    /// input plan. The reference does not define multi-parent checkpoint
    /// selection; v1 keeps each checkpoint bound to its source row even when
    /// another row has matching pins and coordinates.
    pub fn handoff_checkpoint(
        &self,
        route: usize,
        wave: usize,
        depth: usize,
    ) -> Result<ReboundPathCheckpoint, AttachmentError> {
        self.routes
            .get(route)
            .ok_or(AttachmentError::RetainedSnapshotUnavailable)?
            .handoff_checkpoint(wave, depth)
    }

    /// Verifies that a checkpoint belongs to one retained sibling row.
    /// Equal pins and wave/depth coordinates in another row do not transfer
    /// identity. The reference does not define cross-row checkpoint reuse;
    /// v1 accepts only the selected row's original route and snapshot token.
    pub fn validate_handoff_checkpoint(
        &self,
        route: usize,
        checkpoint: &ReboundPathCheckpoint,
    ) -> Result<(), AttachmentError> {
        checkpoint.validate_depth_identity()?;
        self.routes
            .get(route)
            .ok_or(AttachmentError::RetainedSnapshotUnavailable)?
            .validate_depth_label(&checkpoint.depth_label)
    }

    /// Takes ownership of the sibling results in input order.
    pub fn into_routes(self) -> Vec<ReboundPathResolution> {
        self.routes
    }
}

/// A primary database and zero or more read-only, commit-pinned attachments.
#[derive(Clone, Debug)]
pub struct AttachedDatabaseSession {
    primary: PinnedDatabase,
    attached: BTreeMap<String, PinnedDatabase>,
}

impl AttachedDatabaseSession {
    pub fn new(primary: PinnedDatabase) -> Result<Self, AttachmentError> {
        Self::validate_primary_name(&primary.pin.name)?;
        Ok(Self {
            primary,
            attached: BTreeMap::new(),
        })
    }

    fn validate_primary_name(name: &str) -> Result<(), AttachmentError> {
        if name == "sys" || name == "std" {
            return Err(AttachmentError::InvalidName);
        }
        Ok(())
    }

    /// Adds a secondary snapshot. The source and rows remain read-only for the
    /// lifetime of the session; writes are admitted only against the primary.
    pub fn attach_database(
        &mut self,
        database: PinnedDatabase,
    ) -> Result<(), AttachmentError> {
        let name = database.pin.name.clone();
        if name == "sys" {
            return Err(AttachmentError::SystemDatabaseCannotAttach);
        }
        if name == self.primary.pin.name {
            return Err(AttachmentError::DuplicateAttachment);
        }
        if self.attached.contains_key(&name) {
            return Err(AttachmentError::DuplicateAttachment);
        }
        self.attached.insert(name, database);
        Ok(())
    }

    /// Atomically replaces one attached alias with a new immutable pin and
    /// returns the previous pin for later use as a retained closure root.
    /// Matching is by the new pin's exact alias; sibling aliases are unchanged.
    /// The alias must already be attached, and failures leave the session as-is.
    pub fn rebind_database(
        &mut self,
        database: PinnedDatabase,
    ) -> Result<PinnedDatabase, AttachmentError> {
        let name = checked_name(database.pin.name.clone())?;
        if name == "sys" {
            return Err(AttachmentError::SystemDatabaseCannotAttach);
        }
        if name == self.primary.pin.name {
            return Err(AttachmentError::DuplicateAttachment);
        }
        let current = self
            .attached
            .get_mut(&name)
            .ok_or(AttachmentError::AttachmentNotFound)?;
        Ok(std::mem::replace(current, database))
    }

    /// Detaches an optional database alias from subsequent session lookups.
    /// The primary and implementation-provided `sys` facility are not
    /// detachable. The reference does not define live detach timing; v1 drops
    /// the alias immediately, so callers must revalidate module admission
    /// before the next evaluation. A detached alias is unavailable as both a
    /// read and write target; reattaching it admits only the new read-only pin.
    /// Matching is by exact alias name; a prefix does not select or detach a
    /// neighboring alias.
    /// Each clone keeps its own alias map: later detach or reattach operations
    /// on one value do not update another, and a clone made while detached
    /// stays detached. Module and relation routing use the same per-instance
    /// map as read and write validation.
    pub fn detach_database(&mut self, name: &str) -> Result<(), AttachmentError> {
        let name = checked_name(name.to_owned())?;
        if name == self.primary.pin.name {
            return Err(AttachmentError::PrimaryDatabaseCannotDetach);
        }
        if name == "sys" {
            return Err(AttachmentError::SystemDatabaseCannotDetach);
        }
        self.attached
            .remove(&name)
            .map(drop)
            .ok_or(AttachmentError::AttachmentNotFound)
    }

    pub fn primary(&self) -> &PinnedDatabase {
        &self.primary
    }

    pub fn attached(&self) -> impl Iterator<Item = (&str, &PinnedDatabase)> {
        self.attached
            .iter()
            .map(|(name, database)| (name.as_str(), database))
    }

    pub fn database(&self, name: &str) -> Option<&PinnedDatabase> {
        if self.primary.pin.name == name {
            Some(&self.primary)
        } else {
            self.attached.get(name)
        }
    }

    /// Reports the one database whose transaction may be written by this
    /// session. Independent attached logs are never described as atomic.
    pub fn is_writable_database(&self, name: &str) -> bool {
        self.validate_write_target(name).is_ok()
    }

    /// Enforces the attach-layer write boundary before a caller starts a
    /// mutation. ORNA-ATTACH-002 and ORNA-HIST-003 require attached snapshots
    /// to stay read-only; v1 reports a stable refusal for attached aliases and
    /// the system namespace, while only the session's primary is writable.
    pub fn validate_write_target(&self, name: &str) -> Result<(), AttachmentError> {
        let name = checked_name(name.to_owned())?;
        if name == self.primary.pin.name {
            return Ok(());
        }
        if name == "sys" {
            return Err(AttachmentError::SystemDatabaseReadOnly);
        }
        if self.attached.contains_key(&name) {
            return Err(AttachmentError::AttachedSnapshotReadOnly);
        }
        Err(AttachmentError::DatabaseUnavailable)
    }

    /// Source modules for typed session admission. An attachment alias is the
    /// exact first namespace component for its modules; prefix-related aliases
    /// therefore stay independent. Package-local import targets are rebased
    /// under that component while retaining their authored import tails, so
    /// ordinary binding-conflict and import-precedence rules still apply. A
    /// root `main` attachment uses
    /// `main/main.orna` because the primary database already owns `main.orna`.
    /// The reference defines source namespace and import precedence, but does
    /// not prescribe attachment alias decoding; v1 keeps the full alias as one
    /// namespace component and does not use prefix matching.
    pub fn module_inputs(&self) -> Vec<ModuleInput> {
        let mut modules = self.primary.project.modules().to_vec();
        for (name, database) in &self.attached {
            if name == "std" {
                continue;
            }
            let local_namespaces = database
                .project
                .identities()
                .iter()
                .map(|identity| identity.namespace().join("."))
                .collect::<BTreeSet<_>>();
            modules.extend(database.project.modules().iter().map(|module| {
                let logical_path = if module.logical_path == "main.orna" {
                    if name == "main" {
                        // Keep the attached `main` namespace distinct from the
                        // primary root module, which already owns `main.orna`.
                        "main/main.orna".to_owned()
                    } else {
                        format!("{name}.orna")
                    }
                } else {
                    format!("{name}/{}", module.logical_path)
                };
                let source = prefix_attached_local_imports(name, &module.source, &local_namespaces);
                ModuleInput::new(logical_path, source)
            }));
        }
        modules
    }

    /// Source units supplied by the optional `std` attachment, if present.
    /// Core language and `sys` are not sourced from this package.
    pub fn standard_sources(&self) -> Vec<(String, String)> {
        self.attached.get("std").map_or_else(Vec::new, |database| {
            database
                .project
                .modules()
                .iter()
                .map(|module| {
                    let logical_path = if module.logical_path == "main.orna" {
                        "std/main.orna".to_owned()
                    } else {
                        format!("std/{}", module.logical_path)
                    };
                    (logical_path, module.source.clone())
                })
                .collect()
        })
    }

    /// Exact source snapshot used by an optional `std` package attachment.
    pub fn standard_snapshot(&self) -> Option<&GitCommitRef> {
        self.attached.get("std").map(|database| database.pin.commit())
    }

    /// Returns every row source for a table path together with the exact
    /// database snapshot that supplied it. Routing follows this session's own
    /// alias map, so clones retain distinct relation sources across detach and
    /// reattach. Detaching one alias leaves other aliases' rows routable. A
    /// query layer can compose these reads without pretending the separate
    /// write logs are atomic. V1 deliberately returns overlapping row paths
    /// from every alias without deduplicating them; consumers keep the alias
    /// and commit with each row and decide how their query treats that overlap.
    /// Aliases are exact names, so prefix overlap never selects or replaces a
    /// different route; `app`, `app_copy`, and `app_copy_archive` stay distinct.
    pub fn relation_sources<'a>(
        &'a self,
        table_path: &str,
    ) -> Vec<AttachedRelationSource<'a>> {
        let mut sources = Vec::new();
        for database in std::iter::once(&self.primary).chain(self.attached.values()) {
            for row in database
                .project
                .loose_rows()
                .iter()
                .filter(|row| row.table_path() == table_path)
            {
                sources.push(AttachedRelationSource {
                    database: database.pin.name(),
                    commit: database.pin.commit(),
                    row,
                });
            }
        }
        sources
    }

    /// Structural unit compatibility used at database boundaries. Names and
    /// source locations are excluded; dimension and definition tokens must
    /// otherwise match after the language lexer removes whitespace/comments.
    /// This conservative v1 rule never coerces on a name match alone.
    pub fn units_structurally_equivalent(
        &self,
        left_database: &str,
        left_unit: &str,
        right_database: &str,
        right_unit: &str,
    ) -> bool {
        let Some(left) = self
            .database(left_database)
            .and_then(|database| unit_signature(&database.project, left_unit))
        else {
            return false;
        };
        let Some(right) = self
            .database(right_database)
            .and_then(|database| unit_signature(&database.project, right_unit))
        else {
            return false;
        };
        left == right
    }
}

fn prefix_attached_local_imports(
    alias: &str,
    source: &str,
    local_namespaces: &BTreeSet<String>,
) -> String {
    let parsed = parse_module(source);
    if !parsed.is_ok() {
        return source.to_owned();
    }
    let mut starts = parsed
        .value
        .items
        .iter()
        .filter_map(|item| {
            let Declaration::Use { path, .. } = &item.declaration else {
                return None;
            };
            if path.is_empty() || matches!(path[0].name.as_str(), "sys" | "std") {
                return None;
            }
            let target = path
                .iter()
                .map(|segment| segment.name.as_str())
                .collect::<Vec<_>>()
                .join(".");
            local_namespaces
                .contains(&target)
                .then_some(path[0].span.start)
        })
        .collect::<Vec<_>>();
    starts.sort_unstable_by(|left, right| right.cmp(left));
    let mut rebased = source.to_owned();
    for start in starts {
        rebased.insert_str(start, &format!("{alias}."));
    }
    rebased
}

fn captured_standard_gitlink(
    repository: &Repository,
    parent: &GitCommitRef,
) -> Result<Option<GitCommitRef>, AttachmentError> {
    let tree = repository
        .list_committed_tree(parent, 4_096)
        .map_err(AttachmentError::Repository)?;
    let Some(entry) = tree
        .iter()
        .find(|entry| entry.path().as_path() == Path::new("stdlib/std"))
    else {
        return Ok(None);
    };
    if entry.kind() != CommittedTreeEntryKind::Submodule {
        return Err(AttachmentError::PinUnavailable);
    }
    repository
        .committed_submodule_commit(parent, "stdlib/std")
        .map(Some)
        .map_err(AttachmentError::Repository)
}

/// One row source from a pinned database snapshot.
#[derive(Clone, Copy, Debug)]
pub struct AttachedRelationSource<'a> {
    database: &'a str,
    commit: &'a GitCommitRef,
    row: &'a LooseRowCandidate,
}

impl<'a> AttachedRelationSource<'a> {
    pub fn database(&self) -> &'a str {
        self.database
    }

    pub fn commit(&self) -> &'a GitCommitRef {
        self.commit
    }

    pub fn row(&self) -> &'a LooseRowCandidate {
        self.row
    }
}

/// Redacted failures while resolving a committed attachment set.
#[derive(Debug)]
pub enum AttachmentError {
    InvalidName,
    MalformedManifest,
    ManifestTooLarge,
    TooManyPackages,
    DuplicateRepository,
    SystemDatabaseCannotAttach,
    RepositoryUnavailable,
    PinUnavailable,
    PinnedPackageInvalid,
    DuplicateAttachment,
    AttachedSnapshotReadOnly,
    SystemDatabaseReadOnly,
    DatabaseUnavailable,
    PrimaryDatabaseCannotDetach,
    SystemDatabaseCannotDetach,
    AttachmentNotFound,
    RetainedSnapshotUnavailable,
    Repository(RepositoryError),
    Project(ProjectLoadError),
}

impl fmt::Display for AttachmentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidName => "attachment name is invalid or reserved",
            Self::MalformedManifest => "package pin manifest is malformed",
            Self::ManifestTooLarge => "package pin manifest exceeds its size limit",
            Self::TooManyPackages => "package pin manifest exceeds its entry limit",
            Self::DuplicateRepository => "repository aliases are invalid or duplicated",
            Self::SystemDatabaseCannotAttach => "the system database is provided by the host",
            Self::RepositoryUnavailable => "pinned package repository is unavailable",
            Self::PinUnavailable => "pinned package commit is unavailable",
            Self::PinnedPackageInvalid => "pinned package is not a loadable database",
            Self::DuplicateAttachment => "database attachment name is already in use",
            Self::AttachedSnapshotReadOnly => "attached database snapshots are read-only",
            Self::SystemDatabaseReadOnly => "the system database cannot be written",
            Self::DatabaseUnavailable => "database is not available in this session",
            Self::PrimaryDatabaseCannotDetach => "the primary database cannot be detached",
            Self::SystemDatabaseCannotDetach => "the system database cannot be detached",
            Self::AttachmentNotFound => "database attachment does not exist",
            Self::RetainedSnapshotUnavailable => "retained route snapshot does not exist",
            Self::Repository(_) => "repository snapshot could not be read",
            Self::Project(_) => "pinned database source could not be loaded",
        })
    }
}

impl Error for AttachmentError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Repository(error) => Some(error),
            Self::Project(error) => Some(error),
            _ => None,
        }
    }
}

fn checked_name(name: String) -> Result<String, AttachmentError> {
    if valid_attachment_name(&name) {
        Ok(name)
    } else {
        Err(AttachmentError::InvalidName)
    }
}

fn valid_attachment_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_lowercase() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn valid_full_git_oid(object_id: &str) -> bool {
    matches!(object_id.len(), 40 | 64)
        && object_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn unit_signature(project: &LoadedProject, requested_name: &str) -> Option<String> {
    let mut found = None;
    for module in project.modules() {
        let parsed = parse_module(&module.source);
        if !parsed.is_ok() {
            return None;
        }
        let Ok(tokens) = lex(&module.source) else {
            return None;
        };
        for item in &parsed.value.items {
            let Declaration::Unit { name, .. } = &item.declaration else {
                continue;
            };
            if name != requested_name {
                continue;
            }
            let mut saw_unit = false;
            let mut saw_name = false;
            let signature = tokens
                .iter()
                .filter(|token| {
                    token.span.start >= item.span.start && token.span.end <= item.span.end
                })
                .filter_map(|token| {
                    if matches!(token.kind, TokenKind::Keyword(Keyword::Pub)) {
                        return None;
                    }
                    if !saw_unit && matches!(token.kind, TokenKind::Keyword(Keyword::Unit)) {
                        saw_unit = true;
                        return None;
                    }
                    if saw_unit && !saw_name {
                        saw_name = true;
                        return None;
                    }
                    Some(format!("{:?}:{}", token.kind, token.text))
                })
                .collect::<Vec<_>>()
                .join("\0");
            if found.as_ref().is_some_and(|previous| previous != &signature) {
                return None;
            }
            found = Some(signature);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, process::Command};
    use tempfile::TempDir;

    fn git(directory: &std::path::Path, args: &[&str]) -> String {
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
        git(directory.path(), &["config", "user.name", "kierandrewett"]);
        git(directory.path(), &["config", "user.email", "kieran@drewett.dev"]);
        git(directory.path(), &["config", "commit.gpgsign", "false"]);
        git(directory.path(), &["add", "main.orna"]);
        git(directory.path(), &["commit", "--quiet", "-m", "snapshot"]);
        let commit = git(directory.path(), &["rev-parse", "HEAD"]);
        let repository = Repository::discover(directory.path()).unwrap();
        (directory, repository, commit)
    }

    fn write_commit(directory: &std::path::Path, path: &str, contents: &str) -> String {
        let path = directory.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
        git(directory, &["add", "--all"]);
        git(directory, &["commit", "--quiet", "-m", "snapshot"]);
        git(directory, &["rev-parse", "HEAD"])
    }

    #[test]
    fn parent_snapshot_resolves_exact_package_commit_after_head_moves() {
        let package_dir = tempfile::tempdir().unwrap();
        git(package_dir.path(), &["init", "--quiet"]);
        git(package_dir.path(), &["config", "user.name", "kierandrewett"]);
        git(package_dir.path(), &["config", "user.email", "kieran@drewett.dev"]);
        git(package_dir.path(), &["config", "commit.gpgsign", "false"]);
        let package_commit = write_commit(
            package_dir.path(),
            "main.orna",
            include_str!("fixtures/attached-package-main.orna"),
        );
        let package_repository = Repository::discover(package_dir.path()).unwrap();

        let primary_dir = tempfile::tempdir().unwrap();
        git(primary_dir.path(), &["init", "--quiet"]);
        git(primary_dir.path(), &["config", "user.name", "kierandrewett"]);
        git(primary_dir.path(), &["config", "user.email", "kieran@drewett.dev"]);
        git(primary_dir.path(), &["config", "commit.gpgsign", "false"]);
        write_commit(
            primary_dir.path(),
            "main.orna",
            include_str!("fixtures/attached-primary-main.orna"),
        );
        let parent = write_commit(
            primary_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("widgets {package_commit}\n"),
        );
        let primary_repository = Repository::discover(primary_dir.path()).unwrap();
        let loader = ProjectLoader::default();
        let primary = PinnedDatabase::resolve(
            "app",
            primary_repository,
            &parent,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [("widgets".to_owned(), package_repository.clone())],
            loader,
        )
        .unwrap();

        let session = resolver.resolve_for_parent(primary).unwrap();
        let pinned_widgets = session.database("widgets").unwrap();
        assert_eq!(pinned_widgets.pin().commit().as_str(), package_commit);
        assert!(session.is_writable_database("app"));
        assert!(!session.is_writable_database("widgets"));
        assert!(session
            .module_inputs()
            .iter()
            .any(|module| module.logical_path == "widgets.orna"));

        write_commit(
            package_dir.path(),
            "main.orna",
            &include_str!("fixtures/attached-package-main.orna").replace("42", "99"),
        );
        assert_eq!(
            session
                .database("widgets")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            package_commit
        );
        assert!(session
            .database("widgets")
            .unwrap()
            .project()
            .modules()
            .iter()
            .any(|module| module.source.contains("= 42")));
    }

    #[test]
    fn nested_archive_keeps_its_child_pin_and_unit_identity_scoped() {
        let (_source_dir, source_repository, source_commit) =
            repository(include_str!("../tests/fixtures/attached-incompatible-main.orna"));
        let (archive_dir, archive_repository, _) =
            repository(include_str!("../tests/fixtures/attached-equivalent-main.orna"));
        let archive_commit = write_commit(
            archive_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("source {source_commit}\n"),
        );

        let primary_source =
            include_str!("../tests/fixtures/attached-equivalent-main.orna").replace("42", "7");
        let (primary_dir, primary_repository, _) = repository(&primary_source);
        let parent_commit = write_commit(
            primary_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let primary = PinnedDatabase::resolve(
            "app",
            primary_repository,
            &parent_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("archive".to_owned(), archive_repository),
                ("source".to_owned(), source_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(primary).unwrap();
        assert_eq!(
            root_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_commit
        );
        assert!(root_session.database("source").is_none());
        assert!(root_session.units_structurally_equivalent(
            "app", "meter", "archive", "meter"
        ));

        // The reference pins attachments from the selected parent but does not
        // prescribe recursive namespace flattening. V1 reads an archive's own
        // child manifest when that pinned archive is selected as a parent.
        let archive = root_session.database("archive").unwrap().clone();
        let nested_session = resolver.resolve_for_parent(archive).unwrap();
        assert_eq!(
            nested_session.primary().pin().commit().as_str(),
            archive_commit
        );
        assert_eq!(
            nested_session
                .database("source")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            source_commit
        );
        assert!(!nested_session.units_structurally_equivalent(
            "archive",
            "meter",
            "source",
            "meter"
        ));

        // Resolving the nested pin set does not retarget or flatten the root.
        assert_eq!(
            root_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_commit
        );
        assert!(root_session.database("source").is_none());
    }

    #[test]
    fn same_nested_alias_keeps_each_parent_snapshot_pin() {
        let (source_dir, source_repository, child_commit) =
            repository(include_str!("../tests/fixtures/attached-incompatible-main.orna"));
        let direct_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let direct_commit = write_commit(source_dir.path(), "main.orna", direct_source);
        assert_eq!(
            git(source_dir.path(), &["rev-parse", "HEAD"]),
            direct_commit
        );

        let (archive_dir, archive_repository, _) =
            repository(include_str!("../tests/fixtures/attached-equivalent-main.orna"));
        let archive_commit = write_commit(
            archive_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("source {child_commit}\n"),
        );

        let primary_source = direct_source.replace("42", "7");
        let (primary_dir, primary_repository, _) = repository(&primary_source);
        let parent_commit = write_commit(
            primary_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_commit}\nsource {direct_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let primary = PinnedDatabase::resolve(
            "app",
            primary_repository,
            &parent_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("archive".to_owned(), archive_repository),
                ("source".to_owned(), source_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(primary).unwrap();
        assert_eq!(
            root_session
                .database("source")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            direct_commit
        );
        assert!(root_session.units_structurally_equivalent(
            "app", "meter", "source", "meter"
        ));

        // The reference does not define a flattened namespace for nested
        // manifests. V1 resolves the same alias independently for each pinned
        // parent, so the archive's `source` cannot replace the root's pin.
        let archive = root_session.database("archive").unwrap().clone();
        let nested_session = resolver.resolve_for_parent(archive).unwrap();
        assert_eq!(
            nested_session
                .database("source")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            child_commit
        );
        assert_ne!(direct_commit, child_commit);
        assert!(!nested_session.units_structurally_equivalent(
            "archive",
            "meter",
            "source",
            "meter"
        ));

        assert_eq!(
            root_session
                .database("source")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            direct_commit
        );
        assert!(root_session.units_structurally_equivalent(
            "app", "meter", "source", "meter"
        ));
    }

    #[test]
    fn nested_pin_chain_keeps_repeated_archive_alias_at_each_commit() {
        let equivalent_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let archive_later_source = equivalent_source.replace("42", "99");
        let (archive_dir, archive_repository, archive_later_commit) =
            repository(&archive_later_source);

        let (source_dir, source_repository, _) =
            repository(include_str!("../tests/fixtures/attached-incompatible-main.orna"));
        let source_commit = write_commit(
            source_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_later_commit}\n"),
        );

        write_commit(archive_dir.path(), "main.orna", equivalent_source);
        let archive_parent_commit = write_commit(
            archive_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("source {source_commit}\n"),
        );

        let primary_source = equivalent_source.replace("42", "7");
        let (primary_dir, primary_repository, _) = repository(&primary_source);
        let parent_commit = write_commit(
            primary_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_parent_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let primary = PinnedDatabase::resolve(
            "app",
            primary_repository,
            &parent_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("archive".to_owned(), archive_repository),
                ("source".to_owned(), source_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(primary).unwrap();
        assert_eq!(
            root_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_parent_commit
        );
        assert!(root_session.database("source").is_none());
        assert!(root_session.units_structurally_equivalent(
            "app", "meter", "archive", "meter"
        ));

        // The reference fixes each selected parent's exact pins but is silent
        // on recursive pin closure. V1 follows one manifest edge per session;
        // repeated aliases at later depths keep the commit selected there.
        let archive = root_session.database("archive").unwrap().clone();
        let archive_session = resolver.resolve_for_parent(archive).unwrap();
        assert_eq!(
            archive_session.primary().pin().commit().as_str(),
            archive_parent_commit
        );
        assert_eq!(
            archive_session
                .database("source")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            source_commit
        );
        assert!(!archive_session.units_structurally_equivalent(
            "archive",
            "meter",
            "source",
            "meter"
        ));

        let source = archive_session.database("source").unwrap().clone();
        let source_session = resolver.resolve_for_parent(source).unwrap();
        assert_eq!(
            source_session.primary().pin().commit().as_str(),
            source_commit
        );
        assert_eq!(
            source_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_later_commit
        );
        assert_ne!(archive_parent_commit, archive_later_commit);
        assert!(!source_session.units_structurally_equivalent(
            "source",
            "meter",
            "archive",
            "meter"
        ));

        assert_eq!(
            root_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_parent_commit
        );
    }

    #[test]
    fn nested_pin_cycle_closes_on_historical_parent_snapshot() {
        let app_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, app_historical_commit) = repository(app_source);
        let current_app_source = app_source.replace("42", "7");
        write_commit(app_dir.path(), "main.orna", &current_app_source);

        let (archive_dir, archive_repository, _) =
            repository(include_str!("../tests/fixtures/attached-incompatible-main.orna"));
        let archive_commit = write_commit(
            archive_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("app {app_historical_commit}\n"),
        );
        let current_app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &current_app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), archive_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        assert_eq!(
            root_session.primary().pin().commit().as_str(),
            current_app_commit
        );
        assert_eq!(
            root_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_commit
        );
        assert!(!root_session.units_structurally_equivalent(
            "app",
            "meter",
            "archive",
            "meter"
        ));

        // The reference fixes each selected parent's exact pins but leaves
        // recursive closure unspecified. V1 advances one parent manifest at
        // a time, so this alias cycle terminates at the historical empty map.
        let archive = root_session.database("archive").unwrap().clone();
        let archive_session = resolver.resolve_for_parent(archive).unwrap();
        assert_eq!(
            archive_session.primary().pin().commit().as_str(),
            archive_commit
        );
        assert_eq!(
            archive_session
                .database("app")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            app_historical_commit
        );
        assert!(!archive_session.units_structurally_equivalent(
            "archive",
            "meter",
            "app",
            "meter"
        ));

        let historical_app = archive_session.database("app").unwrap().clone();
        let closed_session = resolver.resolve_for_parent(historical_app).unwrap();
        assert_eq!(
            closed_session.primary().pin().commit().as_str(),
            app_historical_commit
        );
        assert_eq!(closed_session.attached().count(), 0);
        assert_eq!(
            root_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_commit
        );
    }

    #[test]
    fn nested_pin_cycle_keeps_repeated_aliases_distinct_to_the_terminal_edge() {
        let equivalent_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let archive_historical_source = equivalent_source.replace("42", "99");
        let (archive_dir, archive_repository, _) =
            repository(&archive_historical_source);

        let (app_dir, app_repository, app_base_commit) = repository(equivalent_source);
        let archive_historical_commit = write_commit(
            archive_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("app {app_base_commit}\n"),
        );
        let app_historical_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_historical_commit}\n"),
        );

        write_commit(
            archive_dir.path(),
            "main.orna",
            include_str!("../tests/fixtures/attached-incompatible-main.orna"),
        );
        let archive_current_commit = write_commit(
            archive_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("app {app_historical_commit}\n"),
        );

        let current_app_source = equivalent_source.replace("42", "7");
        write_commit(app_dir.path(), "main.orna", &current_app_source);
        let app_current_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_current_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_current_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), archive_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        assert_eq!(
            root_session.primary().pin().commit().as_str(),
            app_current_commit
        );
        assert_eq!(
            root_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_current_commit
        );
        assert!(!root_session.units_structurally_equivalent(
            "app",
            "meter",
            "archive",
            "meter"
        ));

        // The reference fixes exact pins per selected parent but leaves
        // recursive cycle handling unspecified. V1 opens one manifest edge
        // per session; repeated aliases remain distinct by their pinned OID.
        let archive = root_session.database("archive").unwrap().clone();
        let archive_session = resolver.resolve_for_parent(archive).unwrap();
        assert_eq!(
            archive_session.primary().pin().commit().as_str(),
            archive_current_commit
        );
        assert_eq!(
            archive_session
                .database("app")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            app_historical_commit
        );
        assert!(!archive_session.units_structurally_equivalent(
            "archive",
            "meter",
            "app",
            "meter"
        ));

        let historical_app = archive_session.database("app").unwrap().clone();
        let historical_app_session = resolver.resolve_for_parent(historical_app).unwrap();
        assert_eq!(
            historical_app_session.primary().pin().commit().as_str(),
            app_historical_commit
        );
        assert_eq!(
            historical_app_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_historical_commit
        );
        assert!(historical_app_session.units_structurally_equivalent(
            "app",
            "meter",
            "archive",
            "meter"
        ));

        let historical_archive = historical_app_session
            .database("archive")
            .unwrap()
            .clone();
        let closure_session = resolver.resolve_for_parent(historical_archive).unwrap();
        assert_eq!(
            closure_session.primary().pin().commit().as_str(),
            archive_historical_commit
        );
        assert_eq!(
            closure_session
                .database("app")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            app_base_commit
        );
        assert!(closure_session.units_structurally_equivalent(
            "archive",
            "meter",
            "app",
            "meter"
        ));

        let base_app = closure_session.database("app").unwrap().clone();
        let terminal_session = resolver.resolve_for_parent(base_app).unwrap();
        assert_eq!(
            terminal_session.primary().pin().commit().as_str(),
            app_base_commit
        );
        assert_eq!(terminal_session.attached().count(), 0);
        assert_eq!(
            root_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_current_commit
        );
    }

    #[test]
    fn nested_pin_closure_rejects_oid_present_only_in_sibling_repository() {
        let app_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let archive_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (archive_dir, archive_repository, sibling_commit) = repository(archive_source);
        assert!(app_repository.resolve_snapshot(&sibling_commit).is_err());
        assert!(archive_repository.resolve_snapshot(&sibling_commit).is_ok());

        let archive_closure_commit = write_commit(
            archive_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("app {sibling_commit}\n"),
        );
        let app_historical_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_closure_commit}\n"),
        );
        let archive_current_commit = write_commit(
            archive_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("app {app_historical_commit}\n"),
        );
        let app_current_source = app_source.replace("42", "7");
        write_commit(app_dir.path(), "main.orna", &app_current_source);
        let app_current_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_current_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_current_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), archive_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let archive = root_session.database("archive").unwrap().clone();
        let archive_session = resolver.resolve_for_parent(archive).unwrap();
        let historical_app = archive_session.database("app").unwrap().clone();
        let historical_app_session = resolver.resolve_for_parent(historical_app).unwrap();
        let archive_closure = historical_app_session
            .database("archive")
            .unwrap()
            .clone();

        // The reference requires the exact historical pin but leaves nested
        // closure diagnostics unspecified. V1 resolves each edge only in the
        // repository mapped to its alias, so a sibling OID is unavailable.
        assert!(matches!(
            resolver.resolve_for_parent(archive_closure),
            Err(AttachmentError::PinUnavailable)
        ));
        assert_eq!(
            root_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_current_commit
        );
        assert_eq!(
            archive_session
                .database("app")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            app_historical_commit
        );
        assert_eq!(
            historical_app_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_closure_commit
        );
    }

    #[test]
    fn nested_pin_closure_requires_a_repository_for_the_terminal_alias() {
        let app_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let archive_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (archive_dir, archive_repository, archive_base_commit) = repository(archive_source);

        let archive_terminal_commit = write_commit(
            archive_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("unmapped {archive_base_commit}\n"),
        );
        let app_historical_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_terminal_commit}\n"),
        );
        let archive_current_commit = write_commit(
            archive_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("app {app_historical_commit}\n"),
        );
        let app_current_source = app_source.replace("42", "7");
        write_commit(app_dir.path(), "main.orna", &app_current_source);
        let app_current_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_current_commit}\n"),
        );

        assert!(archive_repository
            .resolve_snapshot(&archive_base_commit)
            .is_ok());
        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_current_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), archive_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let archive = root_session.database("archive").unwrap().clone();
        let archive_session = resolver.resolve_for_parent(archive).unwrap();
        let historical_app = archive_session.database("app").unwrap().clone();
        let historical_app_session = resolver.resolve_for_parent(historical_app).unwrap();
        let archive_terminal = historical_app_session
            .database("archive")
            .unwrap()
            .clone();

        // The reference requires exact pins per selected parent but leaves
        // recursive closure diagnostics unspecified. V1 requires an explicit
        // alias mapping for every edge instead of searching by OID.
        assert!(matches!(
            resolver.resolve_for_parent(archive_terminal),
            Err(AttachmentError::RepositoryUnavailable)
        ));
        assert_eq!(
            root_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_current_commit
        );
        assert_eq!(
            archive_session
                .database("app")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            app_historical_commit
        );
        assert_eq!(
            historical_app_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_terminal_commit
        );
    }

    #[test]
    fn nested_pin_closure_keeps_shared_repository_alias_and_terminal_pin() {
        let app_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let archive_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (archive_dir, archive_repository, archive_base_commit) = repository(archive_source);

        let archive_terminal_commit = write_commit(
            archive_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("mirror {archive_base_commit}\n"),
        );
        let app_historical_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_terminal_commit}\n"),
        );
        let archive_current_commit = write_commit(
            archive_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("app {app_historical_commit}\n"),
        );
        let app_current_source = app_source.replace("42", "7");
        write_commit(app_dir.path(), "main.orna", &app_current_source);
        let app_current_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_current_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_current_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), archive_repository.clone()),
                ("mirror".to_owned(), archive_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let archive = root_session.database("archive").unwrap().clone();
        let archive_session = resolver.resolve_for_parent(archive).unwrap();
        let historical_app = archive_session.database("app").unwrap().clone();
        let historical_app_session = resolver.resolve_for_parent(historical_app).unwrap();
        let archive_terminal = historical_app_session
            .database("archive")
            .unwrap()
            .clone();
        let terminal_session = resolver.resolve_for_parent(archive_terminal).unwrap();

        // The reference fixes exact pins but does not specify aliases sharing
        // one repository. V1 preserves the manifest alias and resolves its OID
        // in that alias's configured repository, even when another alias uses it.
        assert_eq!(
            terminal_session.primary().pin().name(),
            "archive"
        );
        assert_eq!(
            terminal_session.primary().pin().commit().as_str(),
            archive_terminal_commit
        );
        assert_eq!(
            terminal_session
                .database("mirror")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_base_commit
        );
        assert_ne!(archive_terminal_commit, archive_base_commit);
        assert!(terminal_session.units_structurally_equivalent(
            "archive",
            "meter",
            "mirror",
            "meter"
        ));
        assert_eq!(
            root_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_current_commit
        );
    }

    #[test]
    fn nested_pin_closure_keeps_distinct_pins_for_shared_repository_aliases() {
        let app_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let archive_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (archive_dir, archive_repository, archive_base_commit) = repository(archive_source);
        let mirror_commit = write_commit(
            archive_dir.path(),
            "main.orna",
            include_str!("../tests/fixtures/attached-equivalent-main.orna"),
        );
        let archive_terminal_commit = write_commit(
            archive_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("backup {mirror_commit}\nmirror {archive_base_commit}\n"),
        );
        let app_historical_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_terminal_commit}\n"),
        );
        let archive_current_commit = write_commit(
            archive_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("app {app_historical_commit}\n"),
        );
        let app_current_source = app_source.replace("42", "7");
        write_commit(app_dir.path(), "main.orna", &app_current_source);
        let app_current_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_current_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_current_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), archive_repository.clone()),
                ("backup".to_owned(), archive_repository.clone()),
                ("mirror".to_owned(), archive_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let archive = root_session.database("archive").unwrap().clone();
        let archive_session = resolver.resolve_for_parent(archive).unwrap();
        let historical_app = archive_session.database("app").unwrap().clone();
        let historical_app_session = resolver.resolve_for_parent(historical_app).unwrap();
        let archive_terminal = historical_app_session
            .database("archive")
            .unwrap()
            .clone();
        let terminal_session = resolver.resolve_for_parent(archive_terminal).unwrap();

        // The reference fixes exact pins but does not define aliases sharing
        // one repository. V1 keeps each manifest alias and pin distinct.
        assert_eq!(terminal_session.attached().count(), 2);
        assert_eq!(
            terminal_session
                .database("mirror")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_base_commit
        );
        assert_eq!(
            terminal_session
                .database("backup")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            mirror_commit
        );
        assert_ne!(archive_base_commit, mirror_commit);
        assert!(!terminal_session.units_structurally_equivalent(
            "archive",
            "meter",
            "mirror",
            "meter"
        ));
        assert!(terminal_session.units_structurally_equivalent(
            "archive",
            "meter",
            "backup",
            "meter"
        ));
        assert_eq!(
            root_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_current_commit
        );
    }

    #[test]
    fn nested_pin_closure_keeps_equal_pins_distinct_by_shared_repository_alias() {
        let app_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let archive_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (archive_dir, archive_repository, archive_base_commit) = repository(archive_source);
        let archive_terminal_commit = write_commit(
            archive_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("backup {archive_base_commit}\nmirror {archive_base_commit}\n"),
        );
        let app_historical_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_terminal_commit}\n"),
        );
        let archive_current_commit = write_commit(
            archive_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("app {app_historical_commit}\n"),
        );
        let app_current_source = app_source.replace("42", "7");
        write_commit(app_dir.path(), "main.orna", &app_current_source);
        let app_current_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_current_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_current_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), archive_repository.clone()),
                ("backup".to_owned(), archive_repository.clone()),
                ("mirror".to_owned(), archive_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let archive = root_session.database("archive").unwrap().clone();
        let archive_session = resolver.resolve_for_parent(archive).unwrap();
        let historical_app = archive_session.database("app").unwrap().clone();
        let historical_app_session = resolver.resolve_for_parent(historical_app).unwrap();
        let archive_terminal = historical_app_session
            .database("archive")
            .unwrap()
            .clone();
        let terminal_session = resolver.resolve_for_parent(archive_terminal).unwrap();

        // The reference does not define aliases sharing a repository or an
        // object ID. V1 keeps alias identity even when both pins resolve to
        // the same immutable package snapshot.
        assert_eq!(terminal_session.attached().count(), 2);
        let backup = terminal_session.database("backup").unwrap();
        let mirror = terminal_session.database("mirror").unwrap();
        assert_eq!(backup.pin().name(), "backup");
        assert_eq!(mirror.pin().name(), "mirror");
        assert_eq!(backup.pin().commit().as_str(), archive_base_commit);
        assert_eq!(mirror.pin().commit().as_str(), archive_base_commit);
        assert_ne!(backup.pin(), mirror.pin());
        assert!(terminal_session.units_structurally_equivalent(
            "backup",
            "meter",
            "mirror",
            "meter"
        ));
        assert_eq!(
            root_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_current_commit
        );
    }

    #[test]
    fn nested_pin_closure_resolves_equal_shared_repository_aliases_independently() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let shared_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, _) = repository(shared_source);
        let (_leaf_dir, leaf_repository, leaf_commit) = repository(shared_source);
        let shared_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("leaf {leaf_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("backup {shared_commit}\nmirror {shared_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("backup".to_owned(), shared_repository.clone()),
                ("mirror".to_owned(), shared_repository),
                ("leaf".to_owned(), leaf_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        assert_eq!(root_session.attached().count(), 2);
        let backup = root_session.database("backup").unwrap().clone();
        let mirror = root_session.database("mirror").unwrap().clone();
        assert_eq!(backup.pin().commit(), mirror.pin().commit());
        assert_ne!(backup.pin(), mirror.pin());
        // ORNA-UNIT-002 requires structural checks across database boundaries
        // but does not define alias-specific outcomes. V1 compares the pinned
        // unit structures, so equal snapshots stay compatible under each name.
        assert!(root_session.units_structurally_equivalent(
            "backup",
            "meter",
            "mirror",
            "meter"
        ));

        let backup_closure = resolver.resolve_for_parent(backup).unwrap();
        let mirror_closure = resolver.resolve_for_parent(mirror).unwrap();

        // The reference fixes exact pins but leaves recursive alias sharing
        // unspecified. V1 resolves each selected alias as its own parent and
        // follows the shared snapshot's child pin in both closures.
        assert_eq!(backup_closure.primary().pin().name(), "backup");
        assert_eq!(mirror_closure.primary().pin().name(), "mirror");
        assert_ne!(
            backup_closure.primary().pin(),
            mirror_closure.primary().pin()
        );
        assert_eq!(backup_closure.attached().count(), 1);
        assert_eq!(mirror_closure.attached().count(), 1);
        let backup_leaf = backup_closure.database("leaf").unwrap();
        let mirror_leaf = mirror_closure.database("leaf").unwrap();
        assert_eq!(backup_leaf.pin(), mirror_leaf.pin());
        assert_eq!(backup_leaf.pin().commit().as_str(), leaf_commit);
        assert!(backup_closure.units_structurally_equivalent(
            "backup",
            "meter",
            "leaf",
            "meter"
        ));
        assert!(mirror_closure.units_structurally_equivalent(
            "mirror",
            "meter",
            "leaf",
            "meter"
        ));
        assert_eq!(
            root_session
                .database("backup")
                .unwrap()
                .pin()
                .commit(),
            root_session.database("mirror").unwrap().pin().commit()
        );
    }

    #[test]
    fn nested_pin_closure_keeps_equal_alias_edges_distinct_through_two_depths() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let shared_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, _) = repository(shared_source);
        let (leaf_dir, leaf_repository, _) = repository(shared_source);
        let (_end_dir, end_repository, end_commit) = repository(shared_source);

        let leaf_commit = write_commit(
            leaf_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("end {end_commit}\n"),
        );
        let shared_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {leaf_commit}\nterminal {leaf_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("backup {shared_commit}\nmirror {shared_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("backup".to_owned(), shared_repository.clone()),
                ("mirror".to_owned(), shared_repository),
                ("archive".to_owned(), leaf_repository.clone()),
                ("terminal".to_owned(), leaf_repository),
                ("end".to_owned(), end_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let backup_closure = resolver
            .resolve_for_parent(root_session.database("backup").unwrap().clone())
            .unwrap();
        let mirror_closure = resolver
            .resolve_for_parent(root_session.database("mirror").unwrap().clone())
            .unwrap();

        // The reference fixes exact pins but does not specify recursive
        // namespace behavior for aliases that share a repository and OID.
        // V1 retains each alias edge as each selected parent is traversed.
        assert_eq!(backup_closure.primary().pin().name(), "backup");
        assert_eq!(mirror_closure.primary().pin().name(), "mirror");
        for closure in [&backup_closure, &mirror_closure] {
            assert_eq!(closure.attached().count(), 2);
            let archive = closure.database("archive").unwrap();
            let terminal = closure.database("terminal").unwrap();
            assert_eq!(archive.pin().commit().as_str(), leaf_commit);
            assert_eq!(terminal.pin().commit().as_str(), leaf_commit);
            assert_ne!(archive.pin(), terminal.pin());
            assert!(closure.units_structurally_equivalent(
                closure.primary().pin().name(),
                "meter",
                "archive",
                "meter"
            ));
        }

        let archive_tail = resolver
            .resolve_for_parent(backup_closure.database("archive").unwrap().clone())
            .unwrap();
        let terminal_tail = resolver
            .resolve_for_parent(mirror_closure.database("terminal").unwrap().clone())
            .unwrap();
        assert_eq!(archive_tail.primary().pin().name(), "archive");
        assert_eq!(terminal_tail.primary().pin().name(), "terminal");
        assert_ne!(archive_tail.primary().pin(), terminal_tail.primary().pin());
        let archive_end = archive_tail.database("end").unwrap();
        let terminal_end = terminal_tail.database("end").unwrap();
        assert_eq!(archive_end.pin(), terminal_end.pin());
        assert_eq!(archive_end.pin().commit().as_str(), end_commit);
        assert!(archive_tail.units_structurally_equivalent(
            "archive",
            "meter",
            "end",
            "meter"
        ));
        assert!(terminal_tail.units_structurally_equivalent(
            "terminal",
            "meter",
            "end",
            "meter"
        ));
        assert_eq!(
            root_session
                .database("backup")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            shared_commit
        );
        assert_eq!(
            root_session
                .database("mirror")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            shared_commit
        );
    }

    #[test]
    fn nested_pin_closure_resolves_all_equal_alias_edge_routes_independently() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let shared_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, _) = repository(shared_source);
        let (leaf_dir, leaf_repository, _) = repository(shared_source);
        let (_end_dir, end_repository, end_commit) = repository(shared_source);

        let leaf_commit = write_commit(
            leaf_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("end {end_commit}\n"),
        );
        let shared_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {leaf_commit}\nterminal {leaf_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("backup {shared_commit}\nmirror {shared_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("backup".to_owned(), shared_repository.clone()),
                ("mirror".to_owned(), shared_repository),
                ("archive".to_owned(), leaf_repository.clone()),
                ("terminal".to_owned(), leaf_repository),
                ("end".to_owned(), end_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let backup = root_session.database("backup").unwrap();
        let mirror = root_session.database("mirror").unwrap();
        assert_eq!(backup.pin().commit(), mirror.pin().commit());
        assert_ne!(backup.pin(), mirror.pin());

        // The reference fixes exact pins but does not prescribe traversal
        // order for recursive shared-repository aliases. V1 resolves every
        // alias path independently while retaining each manifest edge name.
        let mut route_pins = Vec::new();
        let mut expected_end_pin = None;
        for outer_alias in ["mirror", "backup"] {
            let closure = resolver
                .resolve_for_parent(root_session.database(outer_alias).unwrap().clone())
                .unwrap();
            assert_eq!(closure.primary().pin().name(), outer_alias);

            for child_alias in ["terminal", "archive"] {
                let sibling_alias = if child_alias == "archive" {
                    "terminal"
                } else {
                    "archive"
                };
                let child = closure.database(child_alias).unwrap();
                let sibling = closure.database(sibling_alias).unwrap();
                assert_eq!(child.pin().commit().as_str(), leaf_commit);
                assert_eq!(sibling.pin().commit().as_str(), leaf_commit);
                assert_ne!(child.pin(), sibling.pin());

                let tail = resolver.resolve_for_parent(child.clone()).unwrap();
                assert_eq!(tail.primary().pin().name(), child_alias);
                assert_eq!(tail.primary().pin().commit().as_str(), leaf_commit);
                let end = tail.database("end").unwrap();
                assert_eq!(end.pin().commit().as_str(), end_commit);
                if let Some(expected) = &expected_end_pin {
                    assert_eq!(end.pin(), expected);
                } else {
                    expected_end_pin = Some(end.pin().clone());
                }
                assert!(tail.units_structurally_equivalent(
                    child_alias,
                    "meter",
                    "end",
                    "meter"
                ));
                route_pins.push(tail.primary().pin().clone());
            }
        }

        assert_eq!(route_pins[0], route_pins[2]);
        assert_eq!(route_pins[1], route_pins[3]);
        assert_ne!(route_pins[0], route_pins[1]);
        assert_eq!(route_pins[0].commit(), route_pins[1].commit());
        assert_eq!(
            root_session
                .database("backup")
                .unwrap()
                .pin()
                .commit(),
            root_session.database("mirror").unwrap().pin().commit()
        );
    }

    #[test]
    fn nested_pin_cycle_keeps_equal_alias_routes_distinct_to_historical_terminals() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let package_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, _) = repository(package_source);
        let (leaf_dir, leaf_repository, leaf_base_commit) = repository(package_source);

        let shared_historical_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {leaf_base_commit}\nterminal {leaf_base_commit}\n"),
        );
        let leaf_current_commit = write_commit(
            leaf_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("backup {shared_historical_commit}\nmirror {shared_historical_commit}\n"),
        );
        let shared_current_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {leaf_current_commit}\nterminal {leaf_current_commit}\n"),
        );
        let app_current_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("backup {shared_current_commit}\nmirror {shared_current_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_current_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("backup".to_owned(), shared_repository.clone()),
                ("mirror".to_owned(), shared_repository),
                ("archive".to_owned(), leaf_repository.clone()),
                ("terminal".to_owned(), leaf_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let backup = root_session.database("backup").unwrap();
        let mirror = root_session.database("mirror").unwrap();
        assert_eq!(backup.pin().commit(), mirror.pin().commit());
        assert_ne!(backup.pin(), mirror.pin());
        assert!(root_session.units_structurally_equivalent(
            "backup",
            "meter",
            "mirror",
            "meter"
        ));

        // The reference fixes each parent's exact pins but does not specify
        // recursive equal-alias cycle behavior. V1 follows one manifest edge
        // at a time and preserves the selected alias through historical pins.
        let mut terminal_pins = Vec::new();
        for (outer_alias, child_alias, repeated_alias, terminal_alias) in [
            ("backup", "archive", "mirror", "terminal"),
            ("mirror", "terminal", "backup", "archive"),
        ] {
            let current_closure = resolver
                .resolve_for_parent(root_session.database(outer_alias).unwrap().clone())
                .unwrap();
            assert_eq!(current_closure.primary().pin().name(), outer_alias);
            let child = current_closure.database(child_alias).unwrap();
            let sibling_alias = if child_alias == "archive" {
                "terminal"
            } else {
                "archive"
            };
            let sibling = current_closure.database(sibling_alias).unwrap();
            assert_eq!(child.pin().commit().as_str(), leaf_current_commit);
            assert_eq!(sibling.pin().commit().as_str(), leaf_current_commit);
            assert_ne!(child.pin(), sibling.pin());
            assert!(current_closure.units_structurally_equivalent(
                child_alias,
                "meter",
                sibling_alias,
                "meter"
            ));

            let leaf_closure = resolver.resolve_for_parent(child.clone()).unwrap();
            assert_eq!(leaf_closure.primary().pin().name(), child_alias);
            assert_eq!(leaf_closure.primary().pin().commit().as_str(), leaf_current_commit);
            let historical_shared = leaf_closure.database(repeated_alias).unwrap();
            let repeated_sibling_alias = if repeated_alias == "backup" {
                "mirror"
            } else {
                "backup"
            };
            let repeated_sibling = leaf_closure
                .database(repeated_sibling_alias)
                .unwrap();
            assert_eq!(
                historical_shared.pin().commit().as_str(),
                shared_historical_commit
            );
            assert_eq!(
                repeated_sibling.pin().commit(),
                historical_shared.pin().commit()
            );
            assert_ne!(historical_shared.pin(), repeated_sibling.pin());
            assert!(leaf_closure.units_structurally_equivalent(
                "backup",
                "meter",
                "mirror",
                "meter"
            ));

            let historical_closure =
                resolver.resolve_for_parent(historical_shared.clone()).unwrap();
            assert_eq!(historical_closure.primary().pin().name(), repeated_alias);
            assert_eq!(
                historical_closure.primary().pin().commit().as_str(),
                shared_historical_commit
            );
            let terminal = historical_closure
                .database(terminal_alias)
                .unwrap()
                .clone();
            let terminal_sibling_alias = if terminal_alias == "archive" {
                "terminal"
            } else {
                "archive"
            };
            let terminal_sibling = historical_closure
                .database(terminal_sibling_alias)
                .unwrap();
            assert_eq!(terminal.pin().commit().as_str(), leaf_base_commit);
            assert_eq!(terminal_sibling.pin().commit(), terminal.pin().commit());
            assert_ne!(terminal_sibling.pin(), terminal.pin());
            assert!(historical_closure.units_structurally_equivalent(
                "archive",
                "meter",
                "terminal",
                "meter"
            ));

            let closed = resolver.resolve_for_parent(terminal).unwrap();
            assert_eq!(closed.primary().pin().name(), terminal_alias);
            assert_eq!(closed.primary().pin().commit().as_str(), leaf_base_commit);
            assert_eq!(closed.attached().count(), 0);
            terminal_pins.push(closed.primary().pin().clone());
        }

        assert_ne!(terminal_pins[0], terminal_pins[1]);
        assert_eq!(terminal_pins[0].commit(), terminal_pins[1].commit());
        assert_eq!(
            root_session
                .database("backup")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            shared_current_commit
        );
        assert_eq!(
            root_session
                .database("mirror")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            shared_current_commit
        );
    }

    #[test]
    fn nested_pin_closure_keeps_equal_units_across_distinct_alias_commits() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let package_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (package_dir, package_repository, _) = repository(package_source);
        let (_leaf_dir, leaf_repository, leaf_commit) = repository(package_source);

        let historical_commit = write_commit(
            package_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("leaf {leaf_commit}\n# historical alias pin\n"),
        );
        let current_commit = write_commit(
            package_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("leaf {leaf_commit}\n# current alias pin\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("backup {current_commit}\nmirror {historical_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("backup".to_owned(), package_repository.clone()),
                ("mirror".to_owned(), package_repository),
                ("leaf".to_owned(), leaf_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let backup = root_session.database("backup").unwrap();
        let mirror = root_session.database("mirror").unwrap();
        assert_ne!(backup.pin().commit(), mirror.pin().commit());
        assert_ne!(backup.pin(), mirror.pin());
        // The reference requires exact package pins and structural unit
        // checks, but does not define alias interaction across revisions. V1
        // compares loaded unit structures independently of alias and commit.
        assert!(root_session.units_structurally_equivalent(
            "backup",
            "meter",
            "mirror",
            "meter"
        ));

        let backup_closure = resolver.resolve_for_parent(backup.clone()).unwrap();
        let mirror_closure = resolver.resolve_for_parent(mirror.clone()).unwrap();
        assert_eq!(backup_closure.primary().pin().name(), "backup");
        assert_eq!(mirror_closure.primary().pin().name(), "mirror");
        assert_ne!(
            backup_closure.primary().pin().commit(),
            mirror_closure.primary().pin().commit()
        );
        let backup_leaf = backup_closure.database("leaf").unwrap();
        let mirror_leaf = mirror_closure.database("leaf").unwrap();
        assert_eq!(backup_leaf.pin(), mirror_leaf.pin());
        assert_eq!(backup_leaf.pin().commit().as_str(), leaf_commit);
        assert!(backup_closure.units_structurally_equivalent(
            "backup",
            "meter",
            "leaf",
            "meter"
        ));
        assert!(mirror_closure.units_structurally_equivalent(
            "mirror",
            "meter",
            "leaf",
            "meter"
        ));
        assert_eq!(
            root_session
                .database("backup")
                .unwrap()
                .pin()
                .commit(),
            backup.pin().commit()
        );
        assert_eq!(
            root_session
                .database("mirror")
                .unwrap()
                .pin()
                .commit(),
            mirror.pin().commit()
        );
    }

    #[test]
    fn nested_pin_closure_uses_each_alias_revision_for_unit_equivalence() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let compatible_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let incompatible_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (package_dir, package_repository, _) = repository(compatible_source);
        let (_leaf_dir, leaf_repository, leaf_commit) = repository(compatible_source);

        write_commit(package_dir.path(), "main.orna", incompatible_source);
        let mirror_commit = write_commit(
            package_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("leaf {leaf_commit}\n# incompatible mirror revision\n"),
        );
        write_commit(package_dir.path(), "main.orna", compatible_source);
        let backup_commit = write_commit(
            package_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("leaf {leaf_commit}\n# compatible backup revision\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("backup {backup_commit}\nmirror {mirror_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("backup".to_owned(), package_repository.clone()),
                ("mirror".to_owned(), package_repository),
                ("leaf".to_owned(), leaf_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let backup = root_session.database("backup").unwrap();
        let mirror = root_session.database("mirror").unwrap();
        assert_eq!(backup.pin().commit().as_str(), backup_commit);
        assert_eq!(mirror.pin().commit().as_str(), mirror_commit);
        assert_ne!(backup.pin().commit(), mirror.pin().commit());
        assert!(!root_session.units_structurally_equivalent(
            "backup",
            "meter",
            "mirror",
            "meter"
        ));

        // ORNA-PACKAGE-002 fixes each selected revision, and ORNA-UNIT-002
        // checks structural compatibility. The reference does not specify
        // shared-repository aliases in nested closures; V1 checks each pinned
        // alias revision against the same exact leaf independently.
        let backup_closure = resolver.resolve_for_parent(backup.clone()).unwrap();
        let mirror_closure = resolver.resolve_for_parent(mirror.clone()).unwrap();
        assert_eq!(backup_closure.primary().pin().name(), "backup");
        assert_eq!(mirror_closure.primary().pin().name(), "mirror");
        assert_eq!(
            backup_closure.primary().pin().commit().as_str(),
            backup_commit
        );
        assert_eq!(
            mirror_closure.primary().pin().commit().as_str(),
            mirror_commit
        );
        let backup_leaf = backup_closure.database("leaf").unwrap();
        let mirror_leaf = mirror_closure.database("leaf").unwrap();
        assert_eq!(backup_leaf.pin(), mirror_leaf.pin());
        assert_eq!(backup_leaf.pin().commit().as_str(), leaf_commit);
        assert!(backup_closure.units_structurally_equivalent(
            "backup",
            "meter",
            "leaf",
            "meter"
        ));
        assert!(!mirror_closure.units_structurally_equivalent(
            "mirror",
            "meter",
            "leaf",
            "meter"
        ));
        assert_eq!(
            root_session
                .database("backup")
                .unwrap()
                .pin()
                .commit(),
            backup.pin().commit()
        );
        assert_eq!(
            root_session
                .database("mirror")
                .unwrap()
                .pin()
                .commit(),
            mirror.pin().commit()
        );
    }

    #[test]
    fn nested_pin_closure_resolves_revision_specific_leaf_pins_for_shared_aliases() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let compatible_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let incompatible_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (package_dir, package_repository, _) = repository(compatible_source);
        let (leaf_dir, leaf_repository, leaf_current_commit) = repository(compatible_source);

        let leaf_historical_commit =
            write_commit(leaf_dir.path(), "main.orna", incompatible_source);
        write_commit(package_dir.path(), "main.orna", incompatible_source);
        let package_historical_commit = write_commit(
            package_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("leaf {leaf_historical_commit}\n"),
        );
        write_commit(package_dir.path(), "main.orna", compatible_source);
        let package_current_commit = write_commit(
            package_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("leaf {leaf_current_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {package_current_commit}\nmirror {package_historical_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), package_repository.clone()),
                ("mirror".to_owned(), package_repository),
                ("leaf".to_owned(), leaf_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        assert_eq!(root_session.attached().count(), 2);
        assert_eq!(
            root_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            package_current_commit
        );
        assert_eq!(
            root_session
                .database("mirror")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            package_historical_commit
        );
        assert!(!root_session.units_structurally_equivalent(
            "archive",
            "meter",
            "mirror",
            "meter"
        ));

        // The reference fixes exact parent pins but does not define aliases
        // sharing one repository. V1 resolves each nested OID from its
        // selected revision, even when another alias points to repository HEAD.
        let archive_closure = resolver
            .resolve_for_parent(root_session.database("archive").unwrap().clone())
            .unwrap();
        let mirror_closure = resolver
            .resolve_for_parent(root_session.database("mirror").unwrap().clone())
            .unwrap();
        let archive_leaf = archive_closure.database("leaf").unwrap();
        let mirror_leaf = mirror_closure.database("leaf").unwrap();
        assert_eq!(archive_leaf.pin().commit().as_str(), leaf_current_commit);
        assert_eq!(
            mirror_leaf.pin().commit().as_str(),
            leaf_historical_commit
        );
        assert_ne!(archive_leaf.pin().commit(), mirror_leaf.pin().commit());
        assert_eq!(
            git(leaf_dir.path(), &["rev-parse", "HEAD"]),
            leaf_historical_commit
        );
        assert!(archive_closure.units_structurally_equivalent(
            "archive",
            "meter",
            "leaf",
            "meter"
        ));
        assert!(mirror_closure.units_structurally_equivalent(
            "mirror",
            "meter",
            "leaf",
            "meter"
        ));
        assert_eq!(
            root_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            package_current_commit
        );
        assert_eq!(
            root_session
                .database("mirror")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            package_historical_commit
        );
    }

    #[test]
    fn nested_pin_closure_keeps_revision_specific_pins_through_terminal_edges() {
        let compatible_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let incompatible_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let (app_dir, app_repository, _) = repository(incompatible_source);
        let (package_dir, package_repository, _) = repository(compatible_source);
        let (leaf_dir, leaf_repository, _) = repository(compatible_source);
        let (terminal_dir, terminal_repository, terminal_current_commit) =
            repository(compatible_source);

        let terminal_historical_commit =
            write_commit(terminal_dir.path(), "main.orna", incompatible_source);
        let leaf_current_commit = write_commit(
            leaf_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("terminal {terminal_current_commit}\n"),
        );
        write_commit(leaf_dir.path(), "main.orna", incompatible_source);
        let leaf_historical_commit = write_commit(
            leaf_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("terminal {terminal_historical_commit}\n"),
        );
        let package_current_commit = write_commit(
            package_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("leaf {leaf_current_commit}\n"),
        );
        write_commit(package_dir.path(), "main.orna", incompatible_source);
        let package_historical_commit = write_commit(
            package_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("leaf {leaf_historical_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {package_current_commit}\nmirror {package_historical_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), package_repository.clone()),
                ("mirror".to_owned(), package_repository),
                ("leaf".to_owned(), leaf_repository),
                ("terminal".to_owned(), terminal_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let archive_closure = resolver
            .resolve_for_parent(root_session.database("archive").unwrap().clone())
            .unwrap();
        let mirror_closure = resolver
            .resolve_for_parent(root_session.database("mirror").unwrap().clone())
            .unwrap();
        let archive_leaf = archive_closure.database("leaf").unwrap().clone();
        let mirror_leaf = mirror_closure.database("leaf").unwrap().clone();

        // Exact revision pinning is specified; recursive alias closure across
        // shared repositories is not. V1 follows each selected snapshot's
        // manifest independently at every depth, including the terminal edge.
        assert_eq!(
            root_session.database("archive").unwrap().pin().commit().as_str(),
            package_current_commit
        );
        assert_eq!(
            root_session.database("mirror").unwrap().pin().commit().as_str(),
            package_historical_commit
        );
        assert_eq!(archive_leaf.pin().commit().as_str(), leaf_current_commit);
        assert_eq!(mirror_leaf.pin().commit().as_str(), leaf_historical_commit);
        assert_ne!(archive_leaf.pin().commit(), mirror_leaf.pin().commit());
        assert_eq!(
            git(package_dir.path(), &["rev-parse", "HEAD"]),
            package_historical_commit
        );
        assert_eq!(
            git(leaf_dir.path(), &["rev-parse", "HEAD"]),
            leaf_historical_commit
        );

        let archive_terminal_closure = resolver.resolve_for_parent(archive_leaf).unwrap();
        let mirror_terminal_closure = resolver.resolve_for_parent(mirror_leaf).unwrap();
        let archive_terminal = archive_terminal_closure.database("terminal").unwrap();
        let mirror_terminal = mirror_terminal_closure.database("terminal").unwrap();
        assert_eq!(
            archive_terminal_closure.primary().pin().commit().as_str(),
            leaf_current_commit
        );
        assert_eq!(
            mirror_terminal_closure.primary().pin().commit().as_str(),
            leaf_historical_commit
        );
        assert_eq!(
            archive_terminal.pin().commit().as_str(),
            terminal_current_commit
        );
        assert_eq!(
            mirror_terminal.pin().commit().as_str(),
            terminal_historical_commit
        );
        assert_ne!(archive_terminal.pin().commit(), mirror_terminal.pin().commit());
        assert_eq!(
            git(terminal_dir.path(), &["rev-parse", "HEAD"]),
            terminal_historical_commit
        );
        assert!(archive_closure.units_structurally_equivalent(
            "archive", "meter", "leaf", "meter"
        ));
        assert!(mirror_closure.units_structurally_equivalent(
            "mirror", "meter", "leaf", "meter"
        ));
        assert!(archive_terminal_closure.units_structurally_equivalent(
            "leaf", "meter", "terminal", "meter"
        ));
        assert!(mirror_terminal_closure.units_structurally_equivalent(
            "leaf", "meter", "terminal", "meter"
        ));
        assert_eq!(
            archive_closure.database("leaf").unwrap().pin().commit().as_str(),
            leaf_current_commit
        );
        assert_eq!(
            mirror_closure.database("leaf").unwrap().pin().commit().as_str(),
            leaf_historical_commit
        );
    }

    #[test]
    fn nested_pin_terminal_closure_preserves_swapped_equal_alias_edges() {
        let compatible_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let incompatible_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let (app_dir, app_repository, _) = repository(incompatible_source);
        let (package_dir, package_repository, _) = repository(compatible_source);
        let (leaf_dir, leaf_repository, _) = repository(compatible_source);
        let (end_dir, end_repository, end_current_commit) = repository(compatible_source);

        let end_historical_commit = write_commit(end_dir.path(), "main.orna", incompatible_source);
        let leaf_current_commit = write_commit(
            leaf_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("end {end_current_commit}\n"),
        );
        write_commit(leaf_dir.path(), "main.orna", incompatible_source);
        let leaf_historical_commit = write_commit(
            leaf_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("end {end_historical_commit}\n"),
        );
        let package_current_commit = write_commit(
            package_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("leaf {leaf_current_commit}\nterminal {leaf_historical_commit}\n"),
        );
        write_commit(package_dir.path(), "main.orna", incompatible_source);
        let package_historical_commit = write_commit(
            package_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("leaf {leaf_historical_commit}\nterminal {leaf_current_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {package_current_commit}\nmirror {package_historical_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), package_repository.clone()),
                ("mirror".to_owned(), package_repository),
                ("leaf".to_owned(), leaf_repository.clone()),
                ("terminal".to_owned(), leaf_repository),
                ("end".to_owned(), end_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let archive_closure = resolver
            .resolve_for_parent(root_session.database("archive").unwrap().clone())
            .unwrap();
        let mirror_closure = resolver
            .resolve_for_parent(root_session.database("mirror").unwrap().clone())
            .unwrap();
        let archive_leaf = archive_closure.database("leaf").unwrap();
        let archive_terminal = archive_closure.database("terminal").unwrap();
        let mirror_leaf = mirror_closure.database("leaf").unwrap();
        let mirror_terminal = mirror_closure.database("terminal").unwrap();

        // The reference fixes each exact pin but leaves recursive closure
        // behavior for shared-repository aliases unspecified. V1 retains every
        // alias edge and reads its next pins from that edge's selected revision.
        assert_eq!(
            archive_closure.primary().pin().commit().as_str(),
            package_current_commit
        );
        assert_eq!(
            mirror_closure.primary().pin().commit().as_str(),
            package_historical_commit
        );
        assert_eq!(archive_leaf.pin().commit().as_str(), leaf_current_commit);
        assert_eq!(archive_terminal.pin().commit().as_str(), leaf_historical_commit);
        assert_eq!(mirror_leaf.pin().commit().as_str(), leaf_historical_commit);
        assert_eq!(mirror_terminal.pin().commit().as_str(), leaf_current_commit);
        assert_eq!(archive_leaf.pin().commit(), mirror_terminal.pin().commit());
        assert_ne!(archive_leaf.pin(), mirror_terminal.pin());
        assert_eq!(archive_terminal.pin().commit(), mirror_leaf.pin().commit());
        assert_ne!(archive_terminal.pin(), mirror_leaf.pin());

        for (closure, alias, expected_leaf, expected_end) in [
            (
                &archive_closure,
                "leaf",
                leaf_current_commit.as_str(),
                end_current_commit.as_str(),
            ),
            (
                &archive_closure,
                "terminal",
                leaf_historical_commit.as_str(),
                end_historical_commit.as_str(),
            ),
            (
                &mirror_closure,
                "leaf",
                leaf_historical_commit.as_str(),
                end_historical_commit.as_str(),
            ),
            (
                &mirror_closure,
                "terminal",
                leaf_current_commit.as_str(),
                end_current_commit.as_str(),
            ),
        ] {
            let tail = resolver
                .resolve_for_parent(closure.database(alias).unwrap().clone())
                .unwrap();
            assert_eq!(tail.primary().pin().name(), alias);
            assert_eq!(tail.primary().pin().commit().as_str(), expected_leaf);
            assert_eq!(tail.database("end").unwrap().pin().commit().as_str(), expected_end);
            assert!(tail.units_structurally_equivalent(alias, "meter", "end", "meter"));
        }

        assert_eq!(git(package_dir.path(), &["rev-parse", "HEAD"]), package_historical_commit);
        assert_eq!(git(leaf_dir.path(), &["rev-parse", "HEAD"]), leaf_historical_commit);
        assert_eq!(git(end_dir.path(), &["rev-parse", "HEAD"]), end_historical_commit);
        assert_eq!(
            root_session.database("archive").unwrap().pin().commit().as_str(),
            package_current_commit
        );
        assert_eq!(
            root_session.database("mirror").unwrap().pin().commit().as_str(),
            package_historical_commit
        );
    }

    #[test]
    fn nested_pin_swapped_alias_closure_composes_terminal_pin_swaps() {
        let compatible_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let incompatible_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let (app_dir, app_repository, _) = repository(incompatible_source);
        let (package_dir, package_repository, _) = repository(compatible_source);
        let (leaf_dir, leaf_repository, _) = repository(compatible_source);
        let (end_dir, end_repository, end_current_commit) = repository(compatible_source);

        let end_historical_commit = write_commit(end_dir.path(), "main.orna", incompatible_source);
        let leaf_current_commit = write_commit(
            leaf_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("end {end_current_commit}\ntail {end_historical_commit}\n"),
        );
        write_commit(leaf_dir.path(), "main.orna", incompatible_source);
        let leaf_historical_commit = write_commit(
            leaf_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("end {end_historical_commit}\ntail {end_current_commit}\n"),
        );
        let package_current_commit = write_commit(
            package_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("leaf {leaf_current_commit}\nterminal {leaf_historical_commit}\n"),
        );
        write_commit(package_dir.path(), "main.orna", incompatible_source);
        let package_historical_commit = write_commit(
            package_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("leaf {leaf_historical_commit}\nterminal {leaf_current_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {package_current_commit}\nmirror {package_historical_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), package_repository.clone()),
                ("mirror".to_owned(), package_repository),
                ("leaf".to_owned(), leaf_repository.clone()),
                ("terminal".to_owned(), leaf_repository),
                ("end".to_owned(), end_repository.clone()),
                ("tail".to_owned(), end_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let archive_closure = resolver
            .resolve_for_parent(root_session.database("archive").unwrap().clone())
            .unwrap();
        let mirror_closure = resolver
            .resolve_for_parent(root_session.database("mirror").unwrap().clone())
            .unwrap();

        // The reference fixes exact pins but leaves recursive shared-alias
        // traversal unspecified. V1 applies each selected snapshot's manifest
        // edge by edge while retaining alias identity at both closure levels.
        assert_eq!(
            archive_closure.primary().pin().commit().as_str(),
            package_current_commit
        );
        assert_eq!(
            mirror_closure.primary().pin().commit().as_str(),
            package_historical_commit
        );
        assert_eq!(
            archive_closure.database("leaf").unwrap().pin().commit().as_str(),
            leaf_current_commit
        );
        assert_eq!(
            archive_closure
                .database("terminal")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            leaf_historical_commit
        );
        assert_eq!(
            mirror_closure.database("leaf").unwrap().pin().commit().as_str(),
            leaf_historical_commit
        );
        assert_eq!(
            mirror_closure
                .database("terminal")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            leaf_current_commit
        );
        assert_eq!(
            archive_closure.database("leaf").unwrap().pin().commit(),
            mirror_closure
                .database("terminal")
                .unwrap()
                .pin()
                .commit()
        );
        assert_ne!(
            archive_closure.database("leaf").unwrap().pin(),
            mirror_closure.database("terminal").unwrap().pin()
        );

        let routes = [
            (
                &archive_closure,
                "leaf",
                leaf_current_commit.as_str(),
                end_current_commit.as_str(),
                end_historical_commit.as_str(),
            ),
            (
                &archive_closure,
                "terminal",
                leaf_historical_commit.as_str(),
                end_historical_commit.as_str(),
                end_current_commit.as_str(),
            ),
            (
                &mirror_closure,
                "leaf",
                leaf_historical_commit.as_str(),
                end_historical_commit.as_str(),
                end_current_commit.as_str(),
            ),
            (
                &mirror_closure,
                "terminal",
                leaf_current_commit.as_str(),
                end_current_commit.as_str(),
                end_historical_commit.as_str(),
            ),
        ];
        let mut leaf_closures = Vec::new();
        for (closure, alias, expected_leaf, expected_end, expected_tail) in routes {
            let leaf_closure = resolver
                .resolve_for_parent(closure.database(alias).unwrap().clone())
                .unwrap();
            assert_eq!(leaf_closure.primary().pin().name(), alias);
            assert_eq!(
                leaf_closure.primary().pin().commit().as_str(),
                expected_leaf
            );
            let end = leaf_closure.database("end").unwrap();
            let tail = leaf_closure.database("tail").unwrap();
            assert_eq!(end.pin().commit().as_str(), expected_end);
            assert_eq!(tail.pin().commit().as_str(), expected_tail);
            assert_ne!(end.pin(), tail.pin());
            leaf_closures.push(leaf_closure);
        }

        for (closure, alias, expected_commit) in [
            (&leaf_closures[0], "end", end_current_commit.as_str()),
            (&leaf_closures[0], "tail", end_historical_commit.as_str()),
            (&leaf_closures[1], "end", end_historical_commit.as_str()),
            (&leaf_closures[1], "tail", end_current_commit.as_str()),
        ] {
            let terminal = resolver
                .resolve_for_parent(closure.database(alias).unwrap().clone())
                .unwrap();
            assert_eq!(terminal.primary().pin().name(), alias);
            assert_eq!(terminal.primary().pin().commit().as_str(), expected_commit);
            assert_eq!(terminal.attached().count(), 0);
        }

        assert_eq!(git(package_dir.path(), &["rev-parse", "HEAD"]), package_historical_commit);
        assert_eq!(git(leaf_dir.path(), &["rev-parse", "HEAD"]), leaf_historical_commit);
        assert_eq!(git(end_dir.path(), &["rev-parse", "HEAD"]), end_historical_commit);
        assert_eq!(
            root_session.database("archive").unwrap().pin().commit().as_str(),
            package_current_commit
        );
        assert_eq!(
            root_session.database("mirror").unwrap().pin().commit().as_str(),
            package_historical_commit
        );
    }

    #[test]
    fn nested_pin_closure_rejects_alias_edge_that_reuses_primary_name() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let archive_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (archive_dir, archive_repository, archive_base_commit) = repository(archive_source);

        let archive_terminal_commit = write_commit(
            archive_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_base_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_terminal_commit}\n"),
        );
        assert!(archive_repository
            .resolve_snapshot(&archive_base_commit)
            .is_ok());

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), archive_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let archive = root_session.database("archive").unwrap().clone();
        assert_eq!(archive.pin().commit().as_str(), archive_terminal_commit);

        // The reference fixes the nested OID but does not define a child edge
        // that reuses its primary alias. V1 rejects it to preserve one binding
        // for that alias in each session and leaves the parent's pin unchanged.
        assert!(matches!(
            resolver.resolve_for_parent(archive),
            Err(AttachmentError::DuplicateAttachment)
        ));
        assert_eq!(
            root_session.database("archive").unwrap().pin().commit().as_str(),
            archive_terminal_commit
        );
    }

    #[test]
    fn nested_pin_primary_alias_collision_does_not_block_sibling_closure() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let archive_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let mirror_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (archive_dir, archive_repository, archive_base_commit) = repository(archive_source);
        let (_mirror_dir, mirror_repository, mirror_commit) = repository(mirror_source);

        let archive_closure_commit = write_commit(
            archive_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_base_commit}\nmirror {mirror_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_closure_commit}\nmirror {mirror_commit}\n"),
        );
        assert!(archive_repository
            .resolve_snapshot(&archive_base_commit)
            .is_ok());
        assert!(mirror_repository.resolve_snapshot(&mirror_commit).is_ok());

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), archive_repository),
                ("mirror".to_owned(), mirror_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        assert_eq!(root_session.attached().count(), 2);
        let archive = root_session.database("archive").unwrap().clone();
        let mirror = root_session.database("mirror").unwrap().clone();
        assert_eq!(archive.pin().commit().as_str(), archive_closure_commit);
        assert_eq!(mirror.pin().commit().as_str(), mirror_commit);

        // The reference fixes exact pins but leaves recursive name-collision
        // behavior unspecified. V1 rejects the self-alias closure atomically;
        // the valid sibling alias still resolves in its own parent session.
        assert!(matches!(
            resolver.resolve_for_parent(archive),
            Err(AttachmentError::DuplicateAttachment)
        ));
        let mirror_closure = resolver.resolve_for_parent(mirror).unwrap();
        assert_eq!(mirror_closure.primary().pin().name(), "mirror");
        assert_eq!(mirror_closure.primary().pin().commit().as_str(), mirror_commit);
        assert_eq!(mirror_closure.attached().count(), 0);
        assert_eq!(
            root_session.database("archive").unwrap().pin().commit().as_str(),
            archive_closure_commit
        );
        assert_eq!(
            root_session.database("mirror").unwrap().pin().commit().as_str(),
            mirror_commit
        );
    }

    #[test]
    fn nested_pin_shared_repository_sibling_closure_survives_primary_alias_collision() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let shared_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, shared_base_commit) = repository(shared_source);

        let shared_closure_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {shared_base_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {shared_closure_commit}\nmirror {shared_closure_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), shared_repository.clone()),
                ("mirror".to_owned(), shared_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let archive = root_session.database("archive").unwrap().clone();
        let mirror = root_session.database("mirror").unwrap().clone();
        assert_eq!(archive.pin().commit(), mirror.pin().commit());
        assert_eq!(archive.pin().commit().as_str(), shared_closure_commit);
        assert_ne!(archive.pin(), mirror.pin());

        // The reference fixes the shared exact snapshot but does not define
        // nested alias collisions. V1 rejects the alias matching the primary,
        // while that same manifest edge is valid under the sibling's name.
        assert!(matches!(
            resolver.resolve_for_parent(archive),
            Err(AttachmentError::DuplicateAttachment)
        ));
        let mirror_closure = resolver.resolve_for_parent(mirror).unwrap();
        assert_eq!(mirror_closure.primary().pin().name(), "mirror");
        assert_eq!(
            mirror_closure.primary().pin().commit().as_str(),
            shared_closure_commit
        );
        assert_eq!(mirror_closure.attached().count(), 1);
        let archive_child = mirror_closure.database("archive").unwrap();
        assert_eq!(archive_child.pin().commit().as_str(), shared_base_commit);
        assert_ne!(archive_child.pin(), mirror_closure.primary().pin());
        assert!(mirror_closure.units_structurally_equivalent(
            "mirror", "meter", "archive", "meter"
        ));
        assert_eq!(
            root_session.database("archive").unwrap().pin().commit().as_str(),
            shared_closure_commit
        );
        assert_eq!(
            root_session.database("mirror").unwrap().pin().commit().as_str(),
            shared_closure_commit
        );
    }

    #[test]
    fn nested_pin_prefix_sibling_closure_survives_exact_primary_alias_collision() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let shared_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, shared_base_commit) = repository(shared_source);

        let shared_closure_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {shared_base_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {shared_closure_commit}\narchive_copy {shared_closure_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), shared_repository.clone()),
                ("archive_copy".to_owned(), shared_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let archive = root_session.database("archive").unwrap().clone();
        let archive_copy = root_session.database("archive_copy").unwrap().clone();
        assert_eq!(archive.pin().commit(), archive_copy.pin().commit());
        assert_eq!(archive.pin().commit().as_str(), shared_closure_commit);
        assert_ne!(archive.pin(), archive_copy.pin());

        // The reference fixes the shared exact snapshot but leaves name
        // collision behavior open. V1 compares complete aliases: `archive`
        // collides with that primary while `archive_copy` remains independent.
        assert!(matches!(
            resolver.resolve_for_parent(archive),
            Err(AttachmentError::DuplicateAttachment)
        ));
        let copy_closure = resolver.resolve_for_parent(archive_copy).unwrap();
        assert_eq!(copy_closure.primary().pin().name(), "archive_copy");
        assert_eq!(
            copy_closure.primary().pin().commit().as_str(),
            shared_closure_commit
        );
        assert_eq!(copy_closure.attached().count(), 1);
        let archive_child = copy_closure.database("archive").unwrap();
        assert_eq!(archive_child.pin().commit().as_str(), shared_base_commit);
        assert_ne!(archive_child.pin(), copy_closure.primary().pin());
        assert!(copy_closure.units_structurally_equivalent(
            "archive_copy", "meter", "archive", "meter"
        ));
        assert_eq!(
            root_session.database("archive").unwrap().pin().commit().as_str(),
            shared_closure_commit
        );
        assert_eq!(
            root_session
                .database("archive_copy")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            shared_closure_commit
        );
    }

    #[test]
    fn nested_pin_prefix_chain_keeps_sibling_closures_independent() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let shared_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, shared_base_commit) = repository(shared_source);

        let shared_closure_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {shared_base_commit}\narchive_copy {shared_base_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "archive {shared_closure_commit}\narchive_copy {shared_closure_commit}\narchive_copy_archive {shared_closure_commit}\n"
            ),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), shared_repository.clone()),
                ("archive_copy".to_owned(), shared_repository.clone()),
                ("archive_copy_archive".to_owned(), shared_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let archive = root_session.database("archive").unwrap().clone();
        let archive_copy = root_session.database("archive_copy").unwrap().clone();
        let longest_sibling = root_session
            .database("archive_copy_archive")
            .unwrap()
            .clone();
        for sibling in [&archive, &archive_copy, &longest_sibling] {
            assert_eq!(sibling.pin().commit().as_str(), shared_closure_commit);
        }
        assert_ne!(archive.pin(), archive_copy.pin());
        assert_ne!(archive_copy.pin(), longest_sibling.pin());

        // The reference fixes the OIDs but does not prescribe prefix behavior
        // for nested aliases. V1 collides only on the exact primary name; two
        // failed sibling closures do not prevent the longer alias from owning
        // independent archive and archive_copy child edges.
        assert!(matches!(
            resolver.resolve_for_parent(archive),
            Err(AttachmentError::DuplicateAttachment)
        ));
        assert!(matches!(
            resolver.resolve_for_parent(archive_copy),
            Err(AttachmentError::DuplicateAttachment)
        ));
        let mut longest_closure = resolver.resolve_for_parent(longest_sibling).unwrap();
        assert_eq!(
            longest_closure.primary().pin().name(),
            "archive_copy_archive"
        );
        assert_eq!(longest_closure.attached().count(), 2);
        let archive_child = longest_closure.database("archive").unwrap();
        let archive_copy_child = longest_closure.database("archive_copy").unwrap();
        assert_eq!(archive_child.pin().commit().as_str(), shared_base_commit);
        assert_eq!(archive_copy_child.pin().commit().as_str(), shared_base_commit);
        assert_ne!(archive_child.pin(), archive_copy_child.pin());
        assert!(longest_closure.units_structurally_equivalent(
            "archive_copy_archive",
            "meter",
            "archive",
            "meter"
        ));

        longest_closure.detach_database("archive").unwrap();
        assert!(longest_closure.database("archive").is_none());
        assert_eq!(
            longest_closure
                .database("archive_copy")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            shared_base_commit
        );
        for sibling_name in ["archive", "archive_copy", "archive_copy_archive"] {
            assert_eq!(
                root_session
                    .database(sibling_name)
                    .unwrap()
                    .pin()
                    .commit()
                    .as_str(),
                shared_closure_commit
            );
        }
    }

    #[test]
    fn nested_pin_prefix_sibling_revisions_keep_closures_isolated() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let shared_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, shared_base_commit) = repository(shared_source);

        let archive_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "archive_copy {shared_base_commit}\narchive_copy_archive {shared_base_commit}\n"
            ),
        );
        let archive_copy_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "archive {shared_base_commit}\narchive_copy_archive {shared_base_commit}\n"
            ),
        );
        let longest_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {shared_base_commit}\narchive_copy {shared_base_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "archive {archive_commit}\narchive_copy {archive_copy_commit}\narchive_copy_archive {longest_commit}\n"
            ),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), shared_repository.clone()),
                ("archive_copy".to_owned(), shared_repository.clone()),
                ("archive_copy_archive".to_owned(), shared_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let closure_specs = [
            (
                "archive",
                archive_commit.as_str(),
                &["archive_copy", "archive_copy_archive"][..],
            ),
            (
                "archive_copy",
                archive_copy_commit.as_str(),
                &["archive", "archive_copy_archive"][..],
            ),
            (
                "archive_copy_archive",
                longest_commit.as_str(),
                &["archive", "archive_copy"][..],
            ),
        ];
        let mut closures = closure_specs.map(|(parent_name, parent_commit, children)| {
            let parent = root_session.database(parent_name).unwrap().clone();
            assert_eq!(parent.pin().commit().as_str(), parent_commit);
            let closure = resolver.resolve_for_parent(parent).unwrap();
            assert_eq!(closure.attached().count(), children.len());
            for child_name in children {
                assert_eq!(
                    closure.database(child_name).unwrap().pin().commit().as_str(),
                    shared_base_commit
                );
            }
            closure
        });

        // The reference fixes each exact pin but is silent on closure alias
        // overlap across revisions. V1 uses full alias names independently:
        // detaching a shorter prefix preserves its longer neighbor and does
        // not mutate sibling closures or the root snapshot.
        closures[0].detach_database("archive_copy").unwrap();
        assert!(closures[0].database("archive_copy").is_none());
        assert!(closures[0].database("archive_copy_archive").is_some());
        assert!(closures[1].database("archive_copy_archive").is_some());
        assert!(closures[2].database("archive_copy").is_some());
        assert_eq!(
            root_session
                .database("archive_copy")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_copy_commit
        );
        assert_eq!(
            root_session
                .database("archive_copy_archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            longest_commit
        );
    }

    #[test]
    fn nested_pin_prefix_reverse_detach_keeps_shorter_sibling_routes() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let shared_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, shared_base_commit) = repository(shared_source);

        let archive_copy_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "archive {shared_base_commit}\narchive_copy_archive {shared_base_commit}\n"
            ),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "archive {shared_base_commit}\narchive_copy {archive_copy_commit}\narchive_copy_archive {shared_base_commit}\n"
            ),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), shared_repository.clone()),
                ("archive_copy".to_owned(), shared_repository.clone()),
                ("archive_copy_archive".to_owned(), shared_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let archive_copy = root_session.database("archive_copy").unwrap().clone();
        let mut copy_closure = resolver.resolve_for_parent(archive_copy.clone()).unwrap();
        let sibling_closure = resolver.resolve_for_parent(archive_copy).unwrap();
        for closure in [&copy_closure, &sibling_closure] {
            assert_eq!(closure.primary().pin().name(), "archive_copy");
            assert_eq!(closure.attached().count(), 2);
            assert_eq!(
                closure
                    .database("archive")
                    .unwrap()
                    .pin()
                    .commit()
                    .as_str(),
                shared_base_commit
            );
            assert_eq!(
                closure
                    .database("archive_copy_archive")
                    .unwrap()
                    .pin()
                    .commit()
                    .as_str(),
                shared_base_commit
            );
        }

        // The reference fixes exact pins but is silent on reverse-prefix
        // detach effects. V1 removes only the full longer alias in one closure;
        // its shorter neighbor, a sibling closure, and the root stay pinned.
        copy_closure
            .detach_database("archive_copy_archive")
            .unwrap();
        assert!(copy_closure.database("archive_copy_archive").is_none());
        assert!(copy_closure.database("archive").is_some());
        assert!(sibling_closure.database("archive_copy_archive").is_some());
        assert_eq!(
            root_session
                .database("archive_copy")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_copy_commit
        );
        assert_eq!(
            root_session
                .database("archive_copy_archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            shared_base_commit
        );
    }

    #[test]
    fn nested_pin_reverse_prefix_edges_keep_distinct_closures_isolated() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let shared_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, shared_base_commit) = repository(shared_source);

        let archive_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy {shared_base_commit}\n"),
        );
        let archive_copy_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {shared_base_commit}\n"),
        );
        let longest_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_commit}\narchive_copy {archive_copy_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "archive {archive_commit}\narchive_copy {archive_copy_commit}\narchive_copy_archive {longest_commit}\n"
            ),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), shared_repository.clone()),
                ("archive_copy".to_owned(), shared_repository.clone()),
                ("archive_copy_archive".to_owned(), shared_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let longest = root_session
            .database("archive_copy_archive")
            .unwrap()
            .clone();
        let mut longest_closure = resolver.resolve_for_parent(longest).unwrap();
        let archive_child = longest_closure.database("archive").unwrap().clone();
        let archive_copy_child = longest_closure.database("archive_copy").unwrap().clone();
        assert_eq!(archive_child.pin().commit().as_str(), archive_commit);
        assert_eq!(
            archive_copy_child.pin().commit().as_str(),
            archive_copy_commit
        );
        assert_ne!(archive_child.pin(), archive_copy_child.pin());

        let archive_closure = resolver.resolve_for_parent(archive_child).unwrap();
        let archive_copy_closure = resolver.resolve_for_parent(archive_copy_child).unwrap();
        assert_eq!(archive_closure.primary().pin().name(), "archive");
        assert_eq!(archive_copy_closure.primary().pin().name(), "archive_copy");
        assert_eq!(
            archive_closure
                .database("archive_copy")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            shared_base_commit
        );
        assert_eq!(
            archive_copy_closure
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            shared_base_commit
        );

        // The reference fixes the revisions but leaves reverse prefix closure
        // routing unspecified. V1 follows each exact edge independently, so
        // detaching one sibling does not retarget its reverse-edge closure.
        longest_closure.detach_database("archive_copy").unwrap();
        assert!(longest_closure.database("archive_copy").is_none());
        assert!(longest_closure.database("archive").is_some());
        assert!(archive_closure.database("archive_copy").is_some());
        assert!(archive_copy_closure.database("archive").is_some());
        assert_eq!(
            root_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_commit
        );
        assert_eq!(
            root_session
                .database("archive_copy")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_copy_commit
        );
    }

    #[test]
    fn nested_pin_reverse_prefix_route_keeps_historical_terminal_isolated() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let shared_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, shared_base_commit) = repository(shared_source);

        let archive_copy_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy_archive {shared_base_commit}\n"),
        );
        let archive_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy {archive_copy_commit}\n"),
        );
        let longest_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "archive {archive_commit}\narchive_copy {archive_copy_commit}\narchive_copy_archive {longest_commit}\n"
            ),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), shared_repository.clone()),
                ("archive_copy".to_owned(), shared_repository.clone()),
                ("archive_copy_archive".to_owned(), shared_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let longest_parent = root_session
            .database("archive_copy_archive")
            .unwrap()
            .clone();
        let longest_closure = resolver.resolve_for_parent(longest_parent).unwrap();
        let archive_parent = longest_closure.database("archive").unwrap().clone();
        let archive_closure = resolver.resolve_for_parent(archive_parent).unwrap();
        let archive_copy_parent = archive_closure.database("archive_copy").unwrap().clone();
        let mut middle_closure = resolver
            .resolve_for_parent(archive_copy_parent.clone())
            .unwrap();
        let middle_sibling_closure = resolver
            .resolve_for_parent(archive_copy_parent)
            .unwrap();

        assert_eq!(longest_closure.primary().pin().name(), "archive_copy_archive");
        assert_eq!(longest_closure.primary().pin().commit().as_str(), longest_commit);
        assert_eq!(archive_closure.primary().pin().name(), "archive");
        assert_eq!(archive_closure.primary().pin().commit().as_str(), archive_commit);
        assert_eq!(middle_closure.primary().pin().name(), "archive_copy");
        assert_eq!(middle_closure.primary().pin().commit().as_str(), archive_copy_commit);
        let historical_longest = middle_closure
            .database("archive_copy_archive")
            .unwrap()
            .clone();
        assert_eq!(historical_longest.pin().commit().as_str(), shared_base_commit);

        // The reference fixes each edge's exact OID but is silent on reverse
        // prefix routes that revisit an alias at an older revision. V1 follows
        // one exact edge per parent; a detach in one route leaves its sibling
        // route and the historical terminal pin independent.
        middle_closure
            .detach_database("archive_copy_archive")
            .unwrap();
        assert!(middle_closure.database("archive_copy_archive").is_none());
        assert_eq!(
            middle_sibling_closure
                .database("archive_copy_archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            shared_base_commit
        );
        let terminal_closure = resolver.resolve_for_parent(historical_longest).unwrap();
        assert_eq!(
            terminal_closure.primary().pin().name(),
            "archive_copy_archive"
        );
        assert_eq!(terminal_closure.primary().pin().commit().as_str(), shared_base_commit);
        assert_eq!(terminal_closure.attached().count(), 0);
        for (name, expected_commit) in [
            ("archive", archive_commit.as_str()),
            ("archive_copy", archive_copy_commit.as_str()),
            ("archive_copy_archive", longest_commit.as_str()),
        ] {
            assert_eq!(
                root_session.database(name).unwrap().pin().commit().as_str(),
                expected_commit
            );
        }
    }

    #[test]
    fn nested_pin_reverse_prefix_branches_isolate_shared_terminal_pin() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let shared_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, shared_base_commit) = repository(shared_source);

        let archive_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy_archive {shared_base_commit}\n"),
        );
        let archive_copy_commit = write_commit(
            shared_dir.path(),
            "main.orna",
            &shared_source.replace("42", "7"),
        );
        let longest_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "archive {archive_commit}\narchive_copy {archive_copy_commit}\n"
            ),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "archive {archive_commit}\narchive_copy {archive_copy_commit}\narchive_copy_archive {longest_commit}\n"
            ),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), shared_repository.clone()),
                ("archive_copy".to_owned(), shared_repository.clone()),
                ("archive_copy_archive".to_owned(), shared_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let longest_parent = root_session
            .database("archive_copy_archive")
            .unwrap()
            .clone();
        let longest_closure = resolver.resolve_for_parent(longest_parent).unwrap();
        let archive_parent = longest_closure.database("archive").unwrap().clone();
        let archive_copy_parent = longest_closure
            .database("archive_copy")
            .unwrap()
            .clone();
        let mut archive_terminal = resolver.resolve_for_parent(archive_parent).unwrap();
        let archive_copy_terminal = resolver
            .resolve_for_parent(archive_copy_parent)
            .unwrap();

        assert_eq!(archive_terminal.primary().pin().name(), "archive");
        assert_eq!(archive_copy_terminal.primary().pin().name(), "archive_copy");
        let archive_terminal_pin = archive_terminal
            .database("archive_copy_archive")
            .unwrap()
            .clone();
        let archive_copy_terminal_pin = archive_copy_terminal
            .database("archive_copy_archive")
            .unwrap()
            .clone();
        assert_eq!(archive_terminal_pin.pin(), archive_copy_terminal_pin.pin());
        assert_eq!(
            archive_terminal_pin.pin().commit().as_str(),
            shared_base_commit
        );

        // The reference fixes the terminal OID but is silent on two reverse
        // prefix routes sharing that pin. V1 keeps closure maps per session, so
        // removing the shared terminal from one route preserves the other.
        archive_terminal
            .detach_database("archive_copy_archive")
            .unwrap();
        assert!(archive_terminal
            .database("archive_copy_archive")
            .is_none());
        assert_eq!(
            archive_copy_terminal
                .database("archive_copy_archive")
                .unwrap()
                .pin(),
            archive_terminal_pin.pin()
        );
        let terminal_session = resolver
            .resolve_for_parent(archive_terminal_pin)
            .unwrap();
        assert_eq!(
            terminal_session.primary().pin().name(),
            "archive_copy_archive"
        );
        assert_eq!(terminal_session.attached().count(), 0);
        for (name, expected_commit) in [
            ("archive", archive_commit.as_str()),
            ("archive_copy", archive_copy_commit.as_str()),
            ("archive_copy_archive", longest_commit.as_str()),
        ] {
            assert_eq!(
                root_session.database(name).unwrap().pin().commit().as_str(),
                expected_commit
            );
        }
    }

    #[test]
    fn nested_pin_reverse_prefix_routes_keep_alias_distinct_terminals() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let shared_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, shared_base_commit) = repository(shared_source);

        let archive_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy {shared_base_commit}\n"),
        );
        let archive_copy_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy_archive {shared_base_commit}\n"),
        );
        let longest_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_commit}\narchive_copy {archive_copy_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "archive {archive_commit}\narchive_copy {archive_copy_commit}\narchive_copy_archive {longest_commit}\n"
            ),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), shared_repository.clone()),
                ("archive_copy".to_owned(), shared_repository.clone()),
                ("archive_copy_archive".to_owned(), shared_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let longest_parent = root_session
            .database("archive_copy_archive")
            .unwrap()
            .clone();
        let longest_closure = resolver.resolve_for_parent(longest_parent).unwrap();
        let archive_closure = resolver
            .resolve_for_parent(longest_closure.database("archive").unwrap().clone())
            .unwrap();
        let archive_copy_closure = resolver
            .resolve_for_parent(longest_closure.database("archive_copy").unwrap().clone())
            .unwrap();
        let mut short_terminal = archive_closure;
        let long_terminal = archive_copy_closure;
        let short_alias_pin = short_terminal.database("archive_copy").unwrap().clone();
        let long_alias_pin = long_terminal
            .database("archive_copy_archive")
            .unwrap()
            .clone();

        assert_eq!(short_alias_pin.pin().name(), "archive_copy");
        assert_eq!(long_alias_pin.pin().name(), "archive_copy_archive");
        assert_eq!(short_alias_pin.pin().commit(), long_alias_pin.pin().commit());
        assert_eq!(short_alias_pin.pin().commit().as_str(), shared_base_commit);
        assert_ne!(short_alias_pin.pin(), long_alias_pin.pin());

        // The reference fixes the terminal OID but leaves alias identity open
        // when reverse-prefix routes converge. V1 preserves each full alias
        // and its closure map, even when both terminal edges share one OID.
        short_terminal.detach_database("archive_copy").unwrap();
        assert!(short_terminal.database("archive_copy").is_none());
        assert_eq!(
            long_terminal
                .database("archive_copy_archive")
                .unwrap()
                .pin(),
            long_alias_pin.pin()
        );
        let short_terminal_session = resolver.resolve_for_parent(short_alias_pin).unwrap();
        let long_terminal_session = resolver.resolve_for_parent(long_alias_pin).unwrap();
        assert_eq!(short_terminal_session.primary().pin().name(), "archive_copy");
        assert_eq!(
            long_terminal_session.primary().pin().name(),
            "archive_copy_archive"
        );
        assert_eq!(short_terminal_session.attached().count(), 0);
        assert_eq!(long_terminal_session.attached().count(), 0);
        for (name, expected_commit) in [
            ("archive", archive_commit.as_str()),
            ("archive_copy", archive_copy_commit.as_str()),
            ("archive_copy_archive", longest_commit.as_str()),
        ] {
            assert_eq!(
                root_session.database(name).unwrap().pin().commit().as_str(),
                expected_commit
            );
        }
    }

    #[test]
    fn nested_pin_reverse_prefix_same_terminal_alias_keeps_revisions_distinct() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let shared_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, shared_base_commit) = repository(shared_source);

        let other_terminal_commit = write_commit(
            shared_dir.path(),
            "main.orna",
            &shared_source.replace("42", "7"),
        );
        let archive_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy_archive {shared_base_commit}\n"),
        );
        let archive_copy_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy_archive {other_terminal_commit}\n"),
        );
        let longest_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_commit}\narchive_copy {archive_copy_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "archive {archive_commit}\narchive_copy {archive_copy_commit}\narchive_copy_archive {longest_commit}\n"
            ),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), shared_repository.clone()),
                ("archive_copy".to_owned(), shared_repository.clone()),
                ("archive_copy_archive".to_owned(), shared_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let longest_closure = resolver
            .resolve_for_parent(
                root_session
                    .database("archive_copy_archive")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        let mut archive_terminal = resolver
            .resolve_for_parent(longest_closure.database("archive").unwrap().clone())
            .unwrap();
        let archive_copy_terminal = resolver
            .resolve_for_parent(longest_closure.database("archive_copy").unwrap().clone())
            .unwrap();
        let base_terminal = archive_terminal
            .database("archive_copy_archive")
            .unwrap()
            .clone();
        let later_terminal = archive_copy_terminal
            .database("archive_copy_archive")
            .unwrap()
            .clone();

        assert_eq!(base_terminal.pin().name(), "archive_copy_archive");
        assert_eq!(later_terminal.pin().name(), "archive_copy_archive");
        assert_eq!(base_terminal.pin().commit().as_str(), shared_base_commit);
        assert_eq!(
            later_terminal.pin().commit().as_str(),
            other_terminal_commit
        );
        assert_ne!(base_terminal.pin(), later_terminal.pin());

        // The reference fixes each exact OID but is silent when reverse-prefix
        // routes reach one terminal alias at different revisions. V1 keeps
        // those pins and each route's detachable closure map independent.
        archive_terminal
            .detach_database("archive_copy_archive")
            .unwrap();
        assert!(archive_terminal
            .database("archive_copy_archive")
            .is_none());
        assert_eq!(
            archive_copy_terminal
                .database("archive_copy_archive")
                .unwrap()
                .pin(),
            later_terminal.pin()
        );
        let base_terminal_session = resolver.resolve_for_parent(base_terminal).unwrap();
        let later_terminal_session = resolver.resolve_for_parent(later_terminal).unwrap();
        assert_eq!(base_terminal_session.attached().count(), 0);
        assert_eq!(later_terminal_session.attached().count(), 0);
        assert_eq!(
            base_terminal_session.primary().pin().commit().as_str(),
            shared_base_commit
        );
        assert_eq!(
            later_terminal_session.primary().pin().commit().as_str(),
            other_terminal_commit
        );
        assert_eq!(
            root_session
                .database("archive_copy_archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            longest_commit
        );
    }

    #[test]
    fn nested_pin_reverse_prefix_terminal_keeps_shorter_tail_edge() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let shared_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, shared_base_commit) = repository(shared_source);

        let terminal_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy {shared_base_commit}\n"),
        );
        let archive_copy_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy_archive {terminal_commit}\n"),
        );
        let archive_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy {archive_copy_commit}\n"),
        );
        let longest_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "archive {archive_commit}\narchive_copy {archive_copy_commit}\narchive_copy_archive {longest_commit}\n"
            ),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), shared_repository.clone()),
                ("archive_copy".to_owned(), shared_repository.clone()),
                ("archive_copy_archive".to_owned(), shared_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let longest_closure = resolver
            .resolve_for_parent(
                root_session
                    .database("archive_copy_archive")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        let archive_closure = resolver
            .resolve_for_parent(longest_closure.database("archive").unwrap().clone())
            .unwrap();
        let middle_closure = resolver
            .resolve_for_parent(archive_closure.database("archive_copy").unwrap().clone())
            .unwrap();
        let historical_longest = middle_closure
            .database("archive_copy_archive")
            .unwrap()
            .clone();
        assert_eq!(historical_longest.pin().commit().as_str(), terminal_commit);

        let mut terminal_closure = resolver
            .resolve_for_parent(historical_longest.clone())
            .unwrap();
        let terminal_sibling_closure = resolver
            .resolve_for_parent(historical_longest)
            .unwrap();
        assert_eq!(
            terminal_closure.primary().pin().name(),
            "archive_copy_archive"
        );
        assert_eq!(terminal_closure.primary().pin().commit().as_str(), terminal_commit);
        assert_eq!(terminal_closure.attached().count(), 1);
        assert_eq!(
            terminal_closure
                .database("archive_copy")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            shared_base_commit
        );

        // The reference fixes terminal pins but is silent when a historical
        // longest alias has a shorter-prefix child. V1 keeps that tail edge
        // local to its closure; detaching it does not alter a sibling route.
        terminal_closure.detach_database("archive_copy").unwrap();
        assert!(terminal_closure.database("archive_copy").is_none());
        assert_eq!(
            terminal_sibling_closure
                .database("archive_copy")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            shared_base_commit
        );
        let tail_session = resolver
            .resolve_for_parent(
                terminal_sibling_closure
                    .database("archive_copy")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(tail_session.primary().pin().name(), "archive_copy");
        assert_eq!(tail_session.primary().pin().commit().as_str(), shared_base_commit);
        assert_eq!(tail_session.attached().count(), 0);
        assert_eq!(
            root_session
                .database("archive_copy_archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            longest_commit
        );
    }

    #[test]
    fn nested_pin_reverse_prefix_terminal_revisits_historical_alias() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let shared_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, _) = repository(shared_source);

        let historical_copy_commit = write_commit(
            shared_dir.path(),
            "main.orna",
            &shared_source.replace("42", "7"),
        );
        let longest_terminal_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy {historical_copy_commit}\n"),
        );
        let current_copy_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy_archive {longest_terminal_commit}\n"),
        );
        let archive_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy {current_copy_commit}\n"),
        );
        let longest_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "archive {archive_commit}\narchive_copy {current_copy_commit}\narchive_copy_archive {longest_commit}\n"
            ),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), shared_repository.clone()),
                ("archive_copy".to_owned(), shared_repository.clone()),
                ("archive_copy_archive".to_owned(), shared_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let longest_closure = resolver
            .resolve_for_parent(
                root_session
                    .database("archive_copy_archive")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        let archive_closure = resolver
            .resolve_for_parent(longest_closure.database("archive").unwrap().clone())
            .unwrap();
        let current_copy = archive_closure.database("archive_copy").unwrap().clone();
        let middle_closure = resolver.resolve_for_parent(current_copy.clone()).unwrap();
        let historical_longest = middle_closure
            .database("archive_copy_archive")
            .unwrap()
            .clone();
        let mut terminal_closure = resolver
            .resolve_for_parent(historical_longest.clone())
            .unwrap();
        let terminal_sibling = resolver
            .resolve_for_parent(historical_longest)
            .unwrap();
        let historical_copy = terminal_closure
            .database("archive_copy")
            .unwrap()
            .clone();

        assert_eq!(current_copy.pin().name(), "archive_copy");
        assert_eq!(current_copy.pin().commit().as_str(), current_copy_commit);
        assert_eq!(historical_copy.pin().name(), "archive_copy");
        assert_eq!(
            historical_copy.pin().commit().as_str(),
            historical_copy_commit
        );
        assert_ne!(current_copy.pin(), historical_copy.pin());

        // The reference fixes the exact terminal OIDs but is silent on a
        // reverse-prefix route revisiting `archive_copy` historically. V1
        // follows one exact edge per parent and isolates detaches per session.
        terminal_closure.detach_database("archive_copy").unwrap();
        assert!(terminal_closure.database("archive_copy").is_none());
        assert_eq!(
            terminal_sibling
                .database("archive_copy")
                .unwrap()
                .pin(),
            historical_copy.pin()
        );
        let historical_copy_session = resolver.resolve_for_parent(historical_copy).unwrap();
        assert_eq!(
            historical_copy_session.primary().pin().name(),
            "archive_copy"
        );
        assert_eq!(
            historical_copy_session.primary().pin().commit().as_str(),
            historical_copy_commit
        );
        assert_eq!(historical_copy_session.attached().count(), 0);
        for (name, expected_commit) in [
            ("archive", archive_commit.as_str()),
            ("archive_copy", current_copy_commit.as_str()),
            ("archive_copy_archive", longest_commit.as_str()),
        ] {
            assert_eq!(
                root_session.database(name).unwrap().pin().commit().as_str(),
                expected_commit
            );
        }
    }

    #[test]
    fn nested_pin_reverse_prefix_terminal_reattach_keeps_sibling_pin() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let shared_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, shared_base_commit) = repository(shared_source);

        let alternate_copy_commit = write_commit(
            shared_dir.path(),
            "main.orna",
            &shared_source.replace("42", "7"),
        );
        let terminal_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy {shared_base_commit}\n"),
        );
        let middle_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy_archive {terminal_commit}\n"),
        );
        let archive_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy {middle_commit}\n"),
        );
        let longest_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "archive {archive_commit}\narchive_copy {alternate_copy_commit}\narchive_copy_archive {longest_commit}\n"
            ),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), shared_repository.clone()),
                ("archive_copy".to_owned(), shared_repository.clone()),
                ("archive_copy_archive".to_owned(), shared_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let longest_closure = resolver
            .resolve_for_parent(
                root_session
                    .database("archive_copy_archive")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        let archive_closure = resolver
            .resolve_for_parent(longest_closure.database("archive").unwrap().clone())
            .unwrap();
        let middle_closure = resolver
            .resolve_for_parent(archive_closure.database("archive_copy").unwrap().clone())
            .unwrap();
        let terminal_pin = middle_closure
            .database("archive_copy_archive")
            .unwrap()
            .clone();
        let mut terminal_closure = resolver.resolve_for_parent(terminal_pin.clone()).unwrap();
        let sibling_terminal = resolver.resolve_for_parent(terminal_pin).unwrap();
        let historical_copy = terminal_closure
            .database("archive_copy")
            .unwrap()
            .clone();
        let alternate_copy = root_session.database("archive_copy").unwrap().clone();
        assert_eq!(historical_copy.pin().commit().as_str(), shared_base_commit);
        assert_eq!(alternate_copy.pin().commit().as_str(), alternate_copy_commit);
        assert_ne!(historical_copy.pin(), alternate_copy.pin());

        // The reference fixes the terminal's selected pins but is silent on
        // detaching and rebinding its shorter-prefix tail. V1 replaces only
        // that closure's exact alias; a sibling closure keeps the old revision.
        terminal_closure.detach_database("archive_copy").unwrap();
        terminal_closure.attach_database(alternate_copy.clone()).unwrap();
        assert_eq!(
            terminal_closure
                .database("archive_copy")
                .unwrap()
                .pin(),
            alternate_copy.pin()
        );
        assert_eq!(
            sibling_terminal
                .database("archive_copy")
                .unwrap()
                .pin(),
            historical_copy.pin()
        );
        assert_eq!(
            root_session
                .database("archive_copy")
                .unwrap()
                .pin(),
            alternate_copy.pin()
        );
        let rebound_tail = resolver.resolve_for_parent(alternate_copy).unwrap();
        assert_eq!(rebound_tail.primary().pin().commit().as_str(), alternate_copy_commit);
        assert_eq!(rebound_tail.attached().count(), 0);
        assert_eq!(
            root_session
                .database("archive_copy_archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            longest_commit
        );
    }

    #[test]
    fn nested_pin_reverse_prefix_terminal_keeps_equal_oid_edges_distinct() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let shared_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, shared_base_commit) = repository(shared_source);

        let terminal_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "archive {shared_base_commit}\narchive_copy {shared_base_commit}\n"
            ),
        );
        let middle_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy_archive {terminal_commit}\n"),
        );
        let archive_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy {middle_commit}\n"),
        );
        let longest_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "archive {archive_commit}\narchive_copy {middle_commit}\narchive_copy_archive {longest_commit}\n"
            ),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), shared_repository.clone()),
                ("archive_copy".to_owned(), shared_repository.clone()),
                ("archive_copy_archive".to_owned(), shared_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let longest_closure = resolver
            .resolve_for_parent(
                root_session
                    .database("archive_copy_archive")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        let archive_closure = resolver
            .resolve_for_parent(longest_closure.database("archive").unwrap().clone())
            .unwrap();
        let middle_closure = resolver
            .resolve_for_parent(archive_closure.database("archive_copy").unwrap().clone())
            .unwrap();
        let terminal_pin = middle_closure
            .database("archive_copy_archive")
            .unwrap()
            .clone();
        let mut terminal_closure = resolver.resolve_for_parent(terminal_pin.clone()).unwrap();
        let terminal_sibling = resolver.resolve_for_parent(terminal_pin).unwrap();
        assert_eq!(terminal_closure.primary().pin().name(), "archive_copy_archive");
        assert_eq!(terminal_closure.primary().pin().commit().as_str(), terminal_commit);

        let archive_edge = terminal_closure.database("archive").unwrap().clone();
        let archive_copy_edge = terminal_closure.database("archive_copy").unwrap().clone();
        assert_eq!(archive_edge.pin().commit().as_str(), shared_base_commit);
        assert_eq!(archive_copy_edge.pin().commit().as_str(), shared_base_commit);
        assert_ne!(archive_edge.pin(), archive_copy_edge.pin());

        // The reference fixes the terminal OID but is silent on its pair of
        // prefix-related edges. V1 retains full alias identity at the same OID;
        // detaching one edge affects only that terminal closure instance.
        terminal_closure.detach_database("archive").unwrap();
        assert!(terminal_closure.database("archive").is_none());
        assert_eq!(
            terminal_closure
                .database("archive_copy")
                .unwrap()
                .pin(),
            archive_copy_edge.pin()
        );
        assert_eq!(
            terminal_sibling.database("archive").unwrap().pin(),
            archive_edge.pin()
        );
        assert_eq!(
            terminal_sibling
                .database("archive_copy")
                .unwrap()
                .pin(),
            archive_copy_edge.pin()
        );
        assert_eq!(
            root_session
                .database("archive_copy_archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            longest_commit
        );
        assert_eq!(
            root_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_commit
        );
        assert_eq!(
            root_session
                .database("archive_copy")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            middle_commit
        );
    }

    #[test]
    fn nested_pin_reverse_prefix_historical_edges_keep_exact_tail_revisions() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let shared_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, _) = repository(shared_source);

        let archive_historical_commit = write_commit(
            shared_dir.path(),
            "main.orna",
            &shared_source.replace("42", "7"),
        );
        let archive_copy_historical_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_historical_commit}\n"),
        );
        let longest_historical_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy {archive_copy_historical_commit}\n"),
        );
        let archive_copy_current_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy_archive {longest_historical_commit}\n"),
        );
        let archive_current_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy {archive_copy_current_commit}\n"),
        );
        let longest_current_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_current_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "archive {archive_current_commit}\narchive_copy {archive_copy_current_commit}\narchive_copy_archive {longest_current_commit}\n"
            ),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), shared_repository.clone()),
                ("archive_copy".to_owned(), shared_repository.clone()),
                ("archive_copy_archive".to_owned(), shared_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let current_longest = resolver
            .resolve_for_parent(
                root_session
                    .database("archive_copy_archive")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        let current_archive = current_longest.database("archive").unwrap().clone();
        let current_archive_closure = resolver.resolve_for_parent(current_archive).unwrap();
        let current_copy = current_archive_closure
            .database("archive_copy")
            .unwrap()
            .clone();
        let current_copy_closure = resolver.resolve_for_parent(current_copy).unwrap();
        let historical_longest = current_copy_closure
            .database("archive_copy_archive")
            .unwrap()
            .clone();
        let historical_longest_closure = resolver
            .resolve_for_parent(historical_longest.clone())
            .unwrap();
        let historical_copy = historical_longest_closure
            .database("archive_copy")
            .unwrap()
            .clone();
        let mut historical_copy_closure = resolver
            .resolve_for_parent(historical_copy.clone())
            .unwrap();
        let historical_copy_sibling = resolver
            .resolve_for_parent(historical_copy)
            .unwrap();
        let historical_archive = historical_copy_closure
            .database("archive")
            .unwrap()
            .clone();

        assert_eq!(current_longest.primary().pin().name(), "archive_copy_archive");
        assert_eq!(current_longest.primary().pin().commit().as_str(), longest_current_commit);
        assert_eq!(current_archive_closure.primary().pin().commit().as_str(), archive_current_commit);
        assert_eq!(current_copy_closure.primary().pin().commit().as_str(), archive_copy_current_commit);
        assert_eq!(historical_longest_closure.primary().pin().commit().as_str(), longest_historical_commit);
        assert_eq!(historical_copy_closure.primary().pin().commit().as_str(), archive_copy_historical_commit);
        assert_eq!(historical_archive.pin().commit().as_str(), archive_historical_commit);
        assert_ne!(current_copy_closure.primary().pin(), historical_copy_closure.primary().pin());
        assert_ne!(current_archive_closure.primary().pin(), historical_archive.pin());

        // The reference fixes each exact OID but is silent on nested reverse
        // prefix edges that revisit aliases at older revisions. V1 follows one
        // edge per parent and keeps the historical tail local to its session.
        historical_copy_closure
            .detach_database("archive")
            .unwrap();
        assert!(historical_copy_closure.database("archive").is_none());
        assert_eq!(
            historical_copy_sibling
                .database("archive")
                .unwrap()
                .pin(),
            historical_archive.pin()
        );
        let historical_archive_closure = resolver
            .resolve_for_parent(historical_archive)
            .unwrap();
        assert_eq!(historical_archive_closure.primary().pin().name(), "archive");
        assert_eq!(historical_archive_closure.primary().pin().commit().as_str(), archive_historical_commit);
        assert_eq!(historical_archive_closure.attached().count(), 0);
        for (name, expected_commit) in [
            ("archive", archive_current_commit.as_str()),
            ("archive_copy", archive_copy_current_commit.as_str()),
            ("archive_copy_archive", longest_current_commit.as_str()),
        ] {
            assert_eq!(
                root_session.database(name).unwrap().pin().commit().as_str(),
                expected_commit
            );
        }
    }

    #[test]
    fn nested_pin_reverse_prefix_historical_tail_keeps_older_middle_pin() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let shared_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, _) = repository(shared_source);

        let oldest_copy_commit = write_commit(
            shared_dir.path(),
            "main.orna",
            &shared_source.replace("42", "7"),
        );
        let archive_historical_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy {oldest_copy_commit}\n"),
        );
        let archive_copy_historical_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_historical_commit}\n"),
        );
        let longest_historical_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy {archive_copy_historical_commit}\n"),
        );
        let archive_copy_current_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy_archive {longest_historical_commit}\n"),
        );
        let archive_current_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy {archive_copy_current_commit}\n"),
        );
        let longest_current_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_current_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "archive {archive_current_commit}\narchive_copy {archive_copy_current_commit}\narchive_copy_archive {longest_current_commit}\n"
            ),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), shared_repository.clone()),
                ("archive_copy".to_owned(), shared_repository.clone()),
                ("archive_copy_archive".to_owned(), shared_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let longest_current = resolver
            .resolve_for_parent(
                root_session
                    .database("archive_copy_archive")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        let archive_current = resolver
            .resolve_for_parent(longest_current.database("archive").unwrap().clone())
            .unwrap();
        let copy_current = resolver
            .resolve_for_parent(archive_current.database("archive_copy").unwrap().clone())
            .unwrap();
        let longest_historical = resolver
            .resolve_for_parent(
                copy_current
                    .database("archive_copy_archive")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        let copy_historical = resolver
            .resolve_for_parent(longest_historical.database("archive_copy").unwrap().clone())
            .unwrap();
        let archive_historical_pin = copy_historical.database("archive").unwrap().clone();
        let mut archive_historical = resolver
            .resolve_for_parent(archive_historical_pin.clone())
            .unwrap();
        let archive_historical_sibling = resolver
            .resolve_for_parent(archive_historical_pin)
            .unwrap();
        let oldest_copy = archive_historical
            .database("archive_copy")
            .unwrap()
            .clone();

        assert_eq!(longest_current.primary().pin().commit().as_str(), longest_current_commit);
        assert_eq!(archive_current.primary().pin().commit().as_str(), archive_current_commit);
        assert_eq!(copy_current.primary().pin().commit().as_str(), archive_copy_current_commit);
        assert_eq!(longest_historical.primary().pin().commit().as_str(), longest_historical_commit);
        assert_eq!(copy_historical.primary().pin().commit().as_str(), archive_copy_historical_commit);
        assert_eq!(archive_historical.primary().pin().commit().as_str(), archive_historical_commit);
        assert_eq!(oldest_copy.pin().name(), "archive_copy");
        assert_eq!(oldest_copy.pin().commit().as_str(), oldest_copy_commit);
        assert_ne!(
            root_session.database("archive_copy").unwrap().pin(),
            oldest_copy.pin()
        );

        // The reference fixes the historical OIDs but is silent on a further
        // reverse-prefix edge from an old `archive` pin back to `archive_copy`.
        // V1 follows that exact tail edge and isolates its detach per session.
        archive_historical.detach_database("archive_copy").unwrap();
        assert!(archive_historical.database("archive_copy").is_none());
        assert_eq!(
            archive_historical_sibling
                .database("archive_copy")
                .unwrap()
                .pin(),
            oldest_copy.pin()
        );
        let oldest_copy_session = resolver.resolve_for_parent(oldest_copy).unwrap();
        assert_eq!(oldest_copy_session.primary().pin().name(), "archive_copy");
        assert_eq!(oldest_copy_session.primary().pin().commit().as_str(), oldest_copy_commit);
        assert_eq!(oldest_copy_session.attached().count(), 0);
        for (name, expected_commit) in [
            ("archive", archive_current_commit.as_str()),
            ("archive_copy", archive_copy_current_commit.as_str()),
            ("archive_copy_archive", longest_current_commit.as_str()),
        ] {
            assert_eq!(
                root_session.database(name).unwrap().pin().commit().as_str(),
                expected_commit
            );
        }
    }

    #[test]
    fn nested_pin_reverse_prefix_older_tail_preserves_archive_pin() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let shared_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, _) = repository(shared_source);

        let older_archive_commit = write_commit(
            shared_dir.path(),
            "main.orna",
            &shared_source.replace("42", "7"),
        );
        let oldest_copy_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {older_archive_commit}\n"),
        );
        let archive_historical_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy {oldest_copy_commit}\n"),
        );
        let archive_copy_historical_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_historical_commit}\n"),
        );
        let longest_historical_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy {archive_copy_historical_commit}\n"),
        );
        let archive_copy_current_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy_archive {longest_historical_commit}\n"),
        );
        let archive_current_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy {archive_copy_current_commit}\n"),
        );
        let longest_current_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_current_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "archive {archive_current_commit}\narchive_copy {archive_copy_current_commit}\narchive_copy_archive {longest_current_commit}\n"
            ),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), shared_repository.clone()),
                ("archive_copy".to_owned(), shared_repository.clone()),
                ("archive_copy_archive".to_owned(), shared_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let longest_current = resolver
            .resolve_for_parent(
                root_session
                    .database("archive_copy_archive")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        let archive_current = resolver
            .resolve_for_parent(longest_current.database("archive").unwrap().clone())
            .unwrap();
        let copy_current = resolver
            .resolve_for_parent(archive_current.database("archive_copy").unwrap().clone())
            .unwrap();
        let longest_historical = resolver
            .resolve_for_parent(
                copy_current
                    .database("archive_copy_archive")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        let copy_historical = resolver
            .resolve_for_parent(longest_historical.database("archive_copy").unwrap().clone())
            .unwrap();
        let archive_historical = resolver
            .resolve_for_parent(copy_historical.database("archive").unwrap().clone())
            .unwrap();
        let oldest_copy_pin = archive_historical
            .database("archive_copy")
            .unwrap()
            .clone();
        let mut oldest_copy_closure = resolver
            .resolve_for_parent(oldest_copy_pin.clone())
            .unwrap();
        let oldest_copy_sibling = resolver
            .resolve_for_parent(oldest_copy_pin)
            .unwrap();
        let older_archive = oldest_copy_closure
            .database("archive")
            .unwrap()
            .clone();

        assert_eq!(oldest_copy_closure.primary().pin().name(), "archive_copy");
        assert_eq!(oldest_copy_closure.primary().pin().commit().as_str(), oldest_copy_commit);
        assert_eq!(older_archive.pin().name(), "archive");
        assert_eq!(older_archive.pin().commit().as_str(), older_archive_commit);
        assert_ne!(
            root_session.database("archive").unwrap().pin(),
            older_archive.pin()
        );
        assert_ne!(
            root_session.database("archive_copy").unwrap().pin(),
            oldest_copy_closure.primary().pin()
        );

        // The reference fixes historical OIDs but is silent on the next
        // reverse-prefix edge below an older `archive_copy`. V1 resolves that
        // edge by its exact alias; detaching it stays local to this closure.
        oldest_copy_closure.detach_database("archive").unwrap();
        assert!(oldest_copy_closure.database("archive").is_none());
        assert_eq!(
            oldest_copy_sibling.database("archive").unwrap().pin(),
            older_archive.pin()
        );
        let older_archive_closure = resolver.resolve_for_parent(older_archive).unwrap();
        assert_eq!(older_archive_closure.primary().pin().name(), "archive");
        assert_eq!(older_archive_closure.primary().pin().commit().as_str(), older_archive_commit);
        assert_eq!(older_archive_closure.attached().count(), 0);
        for (name, expected_commit) in [
            ("archive", archive_current_commit.as_str()),
            ("archive_copy", archive_copy_current_commit.as_str()),
            ("archive_copy_archive", longest_current_commit.as_str()),
        ] {
            assert_eq!(
                root_session.database(name).unwrap().pin().commit().as_str(),
                expected_commit
            );
        }
    }

    #[test]
    fn nested_pin_reverse_prefix_older_edges_keep_alias_tail_isolated() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let shared_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, _) = repository(shared_source);

        let ancient_archive_commit = write_commit(
            shared_dir.path(),
            "main.orna",
            &shared_source.replace("42", "8"),
        );
        let ancient_copy_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {ancient_archive_commit}\n"),
        );
        let ancient_longest_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy {ancient_copy_commit}\n"),
        );
        write_commit(
            shared_dir.path(),
            "main.orna",
            &shared_source.replace("42", "7"),
        );
        let older_archive_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "archive_copy {ancient_copy_commit}\narchive_copy_archive {ancient_longest_commit}\n"
            ),
        );
        let oldest_copy_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {older_archive_commit}\n"),
        );
        let archive_historical_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy {oldest_copy_commit}\n"),
        );
        let archive_copy_historical_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_historical_commit}\n"),
        );
        let longest_historical_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy {archive_copy_historical_commit}\n"),
        );
        let archive_copy_current_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy_archive {longest_historical_commit}\n"),
        );
        let archive_current_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy {archive_copy_current_commit}\n"),
        );
        let longest_current_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_current_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "archive {archive_current_commit}\narchive_copy {archive_copy_current_commit}\narchive_copy_archive {longest_current_commit}\n"
            ),
        );

        let loader = ProjectLoader::default();
        let app =
            PinnedDatabase::resolve("app", app_repository.clone(), &app_commit, loader).unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), shared_repository.clone()),
                ("archive_copy".to_owned(), shared_repository.clone()),
                ("archive_copy_archive".to_owned(), shared_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let current_longest = resolver
            .resolve_for_parent(
                root_session
                    .database("archive_copy_archive")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        let archive_current = resolver
            .resolve_for_parent(current_longest.database("archive").unwrap().clone())
            .unwrap();
        let copy_current = resolver
            .resolve_for_parent(archive_current.database("archive_copy").unwrap().clone())
            .unwrap();
        let longest_historical = resolver
            .resolve_for_parent(
                copy_current
                    .database("archive_copy_archive")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        let copy_historical = resolver
            .resolve_for_parent(longest_historical.database("archive_copy").unwrap().clone())
            .unwrap();
        let archive_historical = resolver
            .resolve_for_parent(copy_historical.database("archive").unwrap().clone())
            .unwrap();
        let oldest_copy = resolver
            .resolve_for_parent(archive_historical.database("archive_copy").unwrap().clone())
            .unwrap();
        let older_archive_pin = oldest_copy.database("archive").unwrap().clone();
        let mut older_archive = resolver
            .resolve_for_parent(older_archive_pin.clone())
            .unwrap();
        let older_archive_sibling = resolver.resolve_for_parent(older_archive_pin).unwrap();
        let ancient_copy = older_archive.database("archive_copy").unwrap().clone();
        let ancient_longest = older_archive
            .database("archive_copy_archive")
            .unwrap()
            .clone();

        assert_eq!(
            oldest_copy.primary().pin().commit().as_str(),
            oldest_copy_commit
        );
        assert_eq!(
            older_archive.primary().pin().commit().as_str(),
            older_archive_commit
        );
        assert_eq!(ancient_copy.pin().commit().as_str(), ancient_copy_commit);
        assert_eq!(
            ancient_longest.pin().commit().as_str(),
            ancient_longest_commit
        );
        assert_eq!(ancient_copy.pin().name(), "archive_copy");
        assert_eq!(ancient_longest.pin().name(), "archive_copy_archive");
        assert_ne!(ancient_copy.pin(), ancient_longest.pin());

        // The reference fixes the selected historical OIDs but is silent on
        // these older reverse-prefix siblings. V1 preserves each full alias
        // and applies detach only to the selected closure snapshot.
        older_archive.detach_database("archive_copy").unwrap();
        assert!(older_archive.database("archive_copy").is_none());
        assert_eq!(
            older_archive
                .database("archive_copy_archive")
                .unwrap()
                .pin(),
            ancient_longest.pin()
        );
        assert_eq!(
            older_archive_sibling
                .database("archive_copy")
                .unwrap()
                .pin(),
            ancient_copy.pin()
        );
        let ancient_copy_session = resolver.resolve_for_parent(ancient_copy).unwrap();
        assert_eq!(
            ancient_copy_session.primary().pin().commit().as_str(),
            ancient_copy_commit
        );
        assert_eq!(
            ancient_copy_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            ancient_archive_commit
        );
        assert_eq!(ancient_copy_session.attached().count(), 1);
        for (name, expected_commit) in [
            ("archive", archive_current_commit.as_str()),
            ("archive_copy", archive_copy_current_commit.as_str()),
            ("archive_copy_archive", longest_current_commit.as_str()),
        ] {
            assert_eq!(
                root_session.database(name).unwrap().pin().commit().as_str(),
                expected_commit
            );
        }
    }

    #[test]
    fn nested_pin_reverse_prefix_older_equal_oid_aliases_remain_distinct() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let shared_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, shared_base_commit) = repository(shared_source);

        let older_archive_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "archive_copy {shared_base_commit}\narchive_copy_archive {shared_base_commit}\n"
            ),
        );
        let oldest_copy_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {older_archive_commit}\n"),
        );
        let archive_historical_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy {oldest_copy_commit}\n"),
        );
        let copy_historical_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_historical_commit}\n"),
        );
        let longest_historical_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy {copy_historical_commit}\n"),
        );
        let copy_current_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy_archive {longest_historical_commit}\n"),
        );
        let archive_current_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy {copy_current_commit}\n"),
        );
        let longest_current_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_current_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy_archive {longest_current_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), shared_repository.clone()),
                ("archive_copy".to_owned(), shared_repository.clone()),
                ("archive_copy_archive".to_owned(), shared_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let longest_current = resolver
            .resolve_for_parent(
                root_session
                    .database("archive_copy_archive")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        let archive_current = resolver
            .resolve_for_parent(longest_current.database("archive").unwrap().clone())
            .unwrap();
        let copy_current = resolver
            .resolve_for_parent(archive_current.database("archive_copy").unwrap().clone())
            .unwrap();
        let longest_historical = resolver
            .resolve_for_parent(
                copy_current
                    .database("archive_copy_archive")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        let copy_historical = resolver
            .resolve_for_parent(
                longest_historical
                    .database("archive_copy")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        let archive_historical = resolver
            .resolve_for_parent(copy_historical.database("archive").unwrap().clone())
            .unwrap();
        let oldest_copy = resolver
            .resolve_for_parent(
                archive_historical
                    .database("archive_copy")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        let older_archive_pin = oldest_copy.database("archive").unwrap().clone();
        let mut older_archive = resolver
            .resolve_for_parent(older_archive_pin.clone())
            .unwrap();
        let older_archive_sibling = resolver
            .resolve_for_parent(older_archive_pin)
            .unwrap();
        let short_alias = older_archive.database("archive_copy").unwrap().clone();
        let long_alias = older_archive
            .database("archive_copy_archive")
            .unwrap()
            .clone();

        assert_eq!(oldest_copy.primary().pin().commit().as_str(), oldest_copy_commit);
        assert_eq!(older_archive.primary().pin().commit().as_str(), older_archive_commit);
        assert_eq!(short_alias.pin().commit().as_str(), shared_base_commit);
        assert_eq!(long_alias.pin().commit().as_str(), shared_base_commit);
        assert_eq!(short_alias.pin().commit(), long_alias.pin().commit());
        assert_ne!(short_alias.pin(), long_alias.pin());

        // The reference does not specify equal-OID prefix aliases at this
        // historical closure depth. V1 keys edges by full alias, so removing
        // the shorter edge leaves the longer edge and sibling snapshot intact.
        older_archive.detach_database("archive_copy").unwrap();
        assert!(older_archive.database("archive_copy").is_none());
        assert_eq!(
            older_archive
                .database("archive_copy_archive")
                .unwrap()
                .pin(),
            long_alias.pin()
        );
        assert_eq!(
            older_archive_sibling
                .database("archive_copy")
                .unwrap()
                .pin(),
            short_alias.pin()
        );
        assert_eq!(
            older_archive_sibling
                .database("archive_copy_archive")
                .unwrap()
                .pin(),
            long_alias.pin()
        );
        let long_alias_session = resolver.resolve_for_parent(long_alias).unwrap();
        assert_eq!(
            long_alias_session.primary().pin().name(),
            "archive_copy_archive"
        );
        assert_eq!(
            long_alias_session.primary().pin().commit().as_str(),
            shared_base_commit
        );
        assert_eq!(long_alias_session.attached().count(), 0);
        assert_eq!(
            root_session
                .database("archive_copy_archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            longest_current_commit
        );
    }

    #[test]
    fn nested_pin_reverse_prefix_older_equal_oid_long_alias_detach_keeps_short() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let shared_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, shared_base_commit) = repository(shared_source);

        let older_archive_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "archive_copy {shared_base_commit}\narchive_copy_archive {shared_base_commit}\n"
            ),
        );
        let oldest_copy_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {older_archive_commit}\n"),
        );
        let archive_historical_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy {oldest_copy_commit}\n"),
        );
        let copy_historical_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_historical_commit}\n"),
        );
        let longest_historical_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy {copy_historical_commit}\n"),
        );
        let copy_current_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy_archive {longest_historical_commit}\n"),
        );
        let archive_current_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy {copy_current_commit}\n"),
        );
        let longest_current_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_current_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy_archive {longest_current_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), shared_repository.clone()),
                ("archive_copy".to_owned(), shared_repository.clone()),
                ("archive_copy_archive".to_owned(), shared_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let longest_current = resolver
            .resolve_for_parent(
                root_session
                    .database("archive_copy_archive")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        let archive_current = resolver
            .resolve_for_parent(longest_current.database("archive").unwrap().clone())
            .unwrap();
        let copy_current = resolver
            .resolve_for_parent(archive_current.database("archive_copy").unwrap().clone())
            .unwrap();
        let longest_historical = resolver
            .resolve_for_parent(
                copy_current
                    .database("archive_copy_archive")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        let copy_historical = resolver
            .resolve_for_parent(
                longest_historical
                    .database("archive_copy")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        let archive_historical = resolver
            .resolve_for_parent(copy_historical.database("archive").unwrap().clone())
            .unwrap();
        let oldest_copy = resolver
            .resolve_for_parent(
                archive_historical
                    .database("archive_copy")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        let older_archive_pin = oldest_copy.database("archive").unwrap().clone();
        let mut older_archive = resolver
            .resolve_for_parent(older_archive_pin.clone())
            .unwrap();
        let older_archive_sibling = resolver
            .resolve_for_parent(older_archive_pin)
            .unwrap();
        let short_alias = older_archive.database("archive_copy").unwrap().clone();
        let long_alias = older_archive
            .database("archive_copy_archive")
            .unwrap()
            .clone();

        assert_eq!(oldest_copy.primary().pin().commit().as_str(), oldest_copy_commit);
        assert_eq!(older_archive.primary().pin().commit().as_str(), older_archive_commit);
        assert_eq!(short_alias.pin().commit(), long_alias.pin().commit());
        assert_eq!(short_alias.pin().commit().as_str(), shared_base_commit);
        assert_ne!(short_alias.pin(), long_alias.pin());

        // The reference is silent on detaching the longer equal-OID alias
        // at this historical depth. V1 removes only that full alias; its
        // shorter sibling and independently resolved closure retain identity.
        older_archive
            .detach_database("archive_copy_archive")
            .unwrap();
        assert!(older_archive.database("archive_copy_archive").is_none());
        assert_eq!(
            older_archive
                .database("archive_copy")
                .unwrap()
                .pin(),
            short_alias.pin()
        );
        assert_eq!(
            older_archive_sibling
                .database("archive_copy")
                .unwrap()
                .pin(),
            short_alias.pin()
        );
        assert_eq!(
            older_archive_sibling
                .database("archive_copy_archive")
                .unwrap()
                .pin(),
            long_alias.pin()
        );
        let short_alias_session = resolver.resolve_for_parent(short_alias).unwrap();
        assert_eq!(short_alias_session.primary().pin().name(), "archive_copy");
        assert_eq!(
            short_alias_session.primary().pin().commit().as_str(),
            shared_base_commit
        );
        assert_eq!(short_alias_session.attached().count(), 0);
        assert_eq!(
            root_session
                .database("archive_copy_archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            longest_current_commit
        );
    }

    #[test]
    fn nested_pin_reverse_prefix_older_equal_oid_outer_aliases_stay_distinct() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let shared_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, shared_base_commit) = repository(shared_source);

        let oldest_copy_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "archive {shared_base_commit}\narchive_copy_archive {shared_base_commit}\n"
            ),
        );
        let archive_historical_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy {oldest_copy_commit}\n"),
        );
        let copy_historical_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_historical_commit}\n"),
        );
        let longest_historical_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy {copy_historical_commit}\n"),
        );
        let copy_current_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy_archive {longest_historical_commit}\n"),
        );
        let archive_current_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy {copy_current_commit}\n"),
        );
        let longest_current_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_current_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy_archive {longest_current_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), shared_repository.clone()),
                ("archive_copy".to_owned(), shared_repository.clone()),
                ("archive_copy_archive".to_owned(), shared_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let longest_current = resolver
            .resolve_for_parent(
                root_session
                    .database("archive_copy_archive")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        let archive_current = resolver
            .resolve_for_parent(longest_current.database("archive").unwrap().clone())
            .unwrap();
        let copy_current = resolver
            .resolve_for_parent(archive_current.database("archive_copy").unwrap().clone())
            .unwrap();
        let longest_historical = resolver
            .resolve_for_parent(
                copy_current
                    .database("archive_copy_archive")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        let copy_historical = resolver
            .resolve_for_parent(
                longest_historical
                    .database("archive_copy")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        let archive_historical = resolver
            .resolve_for_parent(copy_historical.database("archive").unwrap().clone())
            .unwrap();
        let oldest_copy_pin = archive_historical
            .database("archive_copy")
            .unwrap()
            .clone();
        let mut oldest_copy = resolver
            .resolve_for_parent(oldest_copy_pin.clone())
            .unwrap();
        let oldest_copy_sibling = resolver
            .resolve_for_parent(oldest_copy_pin)
            .unwrap();
        let short_alias = oldest_copy.database("archive").unwrap().clone();
        let long_alias = oldest_copy
            .database("archive_copy_archive")
            .unwrap()
            .clone();

        assert_eq!(oldest_copy.primary().pin().name(), "archive_copy");
        assert_eq!(oldest_copy.primary().pin().commit().as_str(), oldest_copy_commit);
        assert_eq!(short_alias.pin().commit(), long_alias.pin().commit());
        assert_eq!(short_alias.pin().commit().as_str(), shared_base_commit);
        assert_ne!(short_alias.pin(), long_alias.pin());

        // The reference does not specify this older pair with a skipped
        // prefix alias. V1 compares full names: detaching the longer route
        // keeps archive, including in its independently resolved sibling.
        oldest_copy
            .detach_database("archive_copy_archive")
            .unwrap();
        assert!(oldest_copy.database("archive_copy_archive").is_none());
        assert_eq!(
            oldest_copy.database("archive").unwrap().pin(),
            short_alias.pin()
        );
        assert_eq!(
            oldest_copy_sibling.database("archive").unwrap().pin(),
            short_alias.pin()
        );
        assert_eq!(
            oldest_copy_sibling
                .database("archive_copy_archive")
                .unwrap()
                .pin(),
            long_alias.pin()
        );
        let short_alias_session = resolver.resolve_for_parent(short_alias).unwrap();
        assert_eq!(short_alias_session.primary().pin().name(), "archive");
        assert_eq!(
            short_alias_session.primary().pin().commit().as_str(),
            shared_base_commit
        );
        assert_eq!(short_alias_session.attached().count(), 0);
        assert_eq!(
            root_session
                .database("archive_copy_archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            longest_current_commit
        );
    }

    #[test]
    fn nested_pin_reverse_prefix_older_equal_oid_outer_short_detach_keeps_long() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let shared_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, shared_base_commit) = repository(shared_source);

        let oldest_copy_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "archive {shared_base_commit}\narchive_copy_archive {shared_base_commit}\n"
            ),
        );
        let archive_historical_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy {oldest_copy_commit}\n"),
        );
        let copy_historical_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_historical_commit}\n"),
        );
        let longest_historical_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy {copy_historical_commit}\n"),
        );
        let copy_current_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy_archive {longest_historical_commit}\n"),
        );
        let archive_current_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy {copy_current_commit}\n"),
        );
        let longest_current_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_current_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy_archive {longest_current_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), shared_repository.clone()),
                ("archive_copy".to_owned(), shared_repository.clone()),
                ("archive_copy_archive".to_owned(), shared_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let longest_current = resolver
            .resolve_for_parent(
                root_session
                    .database("archive_copy_archive")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        let archive_current = resolver
            .resolve_for_parent(longest_current.database("archive").unwrap().clone())
            .unwrap();
        let copy_current = resolver
            .resolve_for_parent(archive_current.database("archive_copy").unwrap().clone())
            .unwrap();
        let longest_historical = resolver
            .resolve_for_parent(
                copy_current
                    .database("archive_copy_archive")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        let copy_historical = resolver
            .resolve_for_parent(
                longest_historical
                    .database("archive_copy")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        let archive_historical = resolver
            .resolve_for_parent(copy_historical.database("archive").unwrap().clone())
            .unwrap();
        let oldest_copy_pin = archive_historical
            .database("archive_copy")
            .unwrap()
            .clone();
        let mut oldest_copy = resolver
            .resolve_for_parent(oldest_copy_pin.clone())
            .unwrap();
        let oldest_copy_sibling = resolver
            .resolve_for_parent(oldest_copy_pin)
            .unwrap();
        let short_alias = oldest_copy.database("archive").unwrap().clone();
        let long_alias = oldest_copy
            .database("archive_copy_archive")
            .unwrap()
            .clone();

        assert_eq!(oldest_copy.primary().pin().name(), "archive_copy");
        assert_eq!(oldest_copy.primary().pin().commit().as_str(), oldest_copy_commit);
        assert_eq!(short_alias.pin().commit(), long_alias.pin().commit());
        assert_eq!(short_alias.pin().commit().as_str(), shared_base_commit);
        assert_ne!(short_alias.pin(), long_alias.pin());

        // The reference is silent on detaching the short end of this skipped
        // prefix pair at an older closure. V1 keeps the longer full alias and
        // the same pins in sibling closures.
        oldest_copy.detach_database("archive").unwrap();
        assert!(oldest_copy.database("archive").is_none());
        assert_eq!(
            oldest_copy
                .database("archive_copy_archive")
                .unwrap()
                .pin(),
            long_alias.pin()
        );
        assert_eq!(
            oldest_copy_sibling.database("archive").unwrap().pin(),
            short_alias.pin()
        );
        assert_eq!(
            oldest_copy_sibling
                .database("archive_copy_archive")
                .unwrap()
                .pin(),
            long_alias.pin()
        );
        let long_alias_session = resolver.resolve_for_parent(long_alias).unwrap();
        assert_eq!(
            long_alias_session.primary().pin().name(),
            "archive_copy_archive"
        );
        assert_eq!(
            long_alias_session.primary().pin().commit().as_str(),
            shared_base_commit
        );
        assert_eq!(long_alias_session.attached().count(), 0);
        assert_eq!(
            root_session
                .database("archive_copy_archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            longest_current_commit
        );
    }

    #[test]
    fn nested_pin_reverse_prefix_older_equal_oid_pair_dissolves_per_closure() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let shared_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, shared_base_commit) = repository(shared_source);

        let oldest_copy_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "archive {shared_base_commit}\narchive_copy_archive {shared_base_commit}\n"
            ),
        );
        let archive_historical_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy {oldest_copy_commit}\n"),
        );
        let copy_historical_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_historical_commit}\n"),
        );
        let longest_historical_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy {copy_historical_commit}\n"),
        );
        let copy_current_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy_archive {longest_historical_commit}\n"),
        );
        let archive_current_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy {copy_current_commit}\n"),
        );
        let longest_current_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_current_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy_archive {longest_current_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), shared_repository.clone()),
                ("archive_copy".to_owned(), shared_repository.clone()),
                ("archive_copy_archive".to_owned(), shared_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let longest_current = resolver
            .resolve_for_parent(
                root_session
                    .database("archive_copy_archive")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        let archive_current = resolver
            .resolve_for_parent(longest_current.database("archive").unwrap().clone())
            .unwrap();
        let copy_current = resolver
            .resolve_for_parent(archive_current.database("archive_copy").unwrap().clone())
            .unwrap();
        let longest_historical = resolver
            .resolve_for_parent(
                copy_current
                    .database("archive_copy_archive")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        let copy_historical = resolver
            .resolve_for_parent(
                longest_historical
                    .database("archive_copy")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        let archive_historical = resolver
            .resolve_for_parent(copy_historical.database("archive").unwrap().clone())
            .unwrap();
        let oldest_copy_pin = archive_historical
            .database("archive_copy")
            .unwrap()
            .clone();
        let mut long_first = resolver
            .resolve_for_parent(oldest_copy_pin.clone())
            .unwrap();
        let mut short_first = resolver
            .resolve_for_parent(oldest_copy_pin.clone())
            .unwrap();
        let untouched_sibling = resolver
            .resolve_for_parent(oldest_copy_pin)
            .unwrap();
        let short_alias = long_first.database("archive").unwrap().clone();
        let long_alias = long_first
            .database("archive_copy_archive")
            .unwrap()
            .clone();

        assert_eq!(long_first.primary().pin().name(), "archive_copy");
        assert_eq!(long_first.primary().pin().commit().as_str(), oldest_copy_commit);
        assert_eq!(short_alias.pin().commit(), long_alias.pin().commit());
        assert_eq!(short_alias.pin().commit().as_str(), shared_base_commit);
        assert_ne!(short_alias.pin(), long_alias.pin());

        // The reference fixes these OIDs but is silent on detach ordering
        // across cloned closures. V1 removes exact aliases per closure.
        long_first
            .detach_database("archive_copy_archive")
            .unwrap();
        assert!(long_first.database("archive_copy_archive").is_none());
        assert_eq!(
            long_first.database("archive").unwrap().pin(),
            short_alias.pin()
        );
        long_first.detach_database("archive").unwrap();
        assert!(long_first.database("archive").is_none());

        short_first.detach_database("archive").unwrap();
        assert!(short_first.database("archive").is_none());
        assert_eq!(
            short_first
                .database("archive_copy_archive")
                .unwrap()
                .pin(),
            long_alias.pin()
        );
        short_first
            .detach_database("archive_copy_archive")
            .unwrap();
        assert!(short_first.database("archive_copy_archive").is_none());

        assert_eq!(
            untouched_sibling.database("archive").unwrap().pin(),
            short_alias.pin()
        );
        assert_eq!(
            untouched_sibling
                .database("archive_copy_archive")
                .unwrap()
                .pin(),
            long_alias.pin()
        );
        assert_eq!(untouched_sibling.attached().count(), 2);
        let short_alias_session = resolver.resolve_for_parent(short_alias).unwrap();
        let long_alias_session = resolver.resolve_for_parent(long_alias).unwrap();
        assert_eq!(short_alias_session.primary().pin().name(), "archive");
        assert_eq!(
            long_alias_session.primary().pin().name(),
            "archive_copy_archive"
        );
        assert_eq!(short_alias_session.primary().pin().commit().as_str(), shared_base_commit);
        assert_eq!(long_alias_session.primary().pin().commit().as_str(), shared_base_commit);
        assert_eq!(short_alias_session.attached().count(), 0);
        assert_eq!(long_alias_session.attached().count(), 0);
        assert_eq!(
            root_session
                .database("archive_copy_archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            longest_current_commit
        );
    }

    #[test]
    fn nested_pin_reverse_prefix_older_distinct_oid_pair_dissolves_per_closure() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let shared_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, shared_base_commit) = repository(shared_source);

        let short_revision_commit = write_commit(
            shared_dir.path(),
            "main.orna",
            &shared_source.replace("42", "7"),
        );
        let oldest_copy_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "archive {short_revision_commit}\narchive_copy_archive {shared_base_commit}\n"
            ),
        );
        let archive_historical_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy {oldest_copy_commit}\n"),
        );
        let copy_historical_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_historical_commit}\n"),
        );
        let longest_historical_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy {copy_historical_commit}\n"),
        );
        let copy_current_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy_archive {longest_historical_commit}\n"),
        );
        let archive_current_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy {copy_current_commit}\n"),
        );
        let longest_current_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_current_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy_archive {longest_current_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), shared_repository.clone()),
                ("archive_copy".to_owned(), shared_repository.clone()),
                ("archive_copy_archive".to_owned(), shared_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let longest_current = resolver
            .resolve_for_parent(
                root_session
                    .database("archive_copy_archive")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        let archive_current = resolver
            .resolve_for_parent(longest_current.database("archive").unwrap().clone())
            .unwrap();
        let copy_current = resolver
            .resolve_for_parent(archive_current.database("archive_copy").unwrap().clone())
            .unwrap();
        let longest_historical = resolver
            .resolve_for_parent(
                copy_current
                    .database("archive_copy_archive")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        let copy_historical = resolver
            .resolve_for_parent(
                longest_historical
                    .database("archive_copy")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        let archive_historical = resolver
            .resolve_for_parent(copy_historical.database("archive").unwrap().clone())
            .unwrap();
        let oldest_copy_pin = archive_historical
            .database("archive_copy")
            .unwrap()
            .clone();
        let mut long_first = resolver
            .resolve_for_parent(oldest_copy_pin.clone())
            .unwrap();
        let mut short_first = resolver
            .resolve_for_parent(oldest_copy_pin.clone())
            .unwrap();
        let untouched_sibling = resolver
            .resolve_for_parent(oldest_copy_pin)
            .unwrap();
        let short_alias = long_first.database("archive").unwrap().clone();
        let long_alias = long_first
            .database("archive_copy_archive")
            .unwrap()
            .clone();

        assert_eq!(long_first.primary().pin().name(), "archive_copy");
        assert_eq!(long_first.primary().pin().commit().as_str(), oldest_copy_commit);
        assert_eq!(short_alias.pin().commit().as_str(), short_revision_commit);
        assert_eq!(long_alias.pin().commit().as_str(), shared_base_commit);
        assert_ne!(short_alias.pin().commit(), long_alias.pin().commit());
        assert_ne!(short_alias.pin(), long_alias.pin());

        // The reference fixes the individual revisions but is silent on
        // dissolving this skipped-prefix pair across cloned closures. V1
        // removes each full alias independently in either detach order.
        long_first
            .detach_database("archive_copy_archive")
            .unwrap();
        assert!(long_first.database("archive_copy_archive").is_none());
        assert_eq!(
            long_first.database("archive").unwrap().pin(),
            short_alias.pin()
        );
        long_first.detach_database("archive").unwrap();
        assert!(long_first.database("archive").is_none());

        short_first.detach_database("archive").unwrap();
        assert!(short_first.database("archive").is_none());
        assert_eq!(
            short_first
                .database("archive_copy_archive")
                .unwrap()
                .pin(),
            long_alias.pin()
        );
        short_first
            .detach_database("archive_copy_archive")
            .unwrap();
        assert!(short_first.database("archive_copy_archive").is_none());

        assert_eq!(
            untouched_sibling.database("archive").unwrap().pin(),
            short_alias.pin()
        );
        assert_eq!(
            untouched_sibling
                .database("archive_copy_archive")
                .unwrap()
                .pin(),
            long_alias.pin()
        );
        assert_eq!(untouched_sibling.attached().count(), 2);
        let short_alias_session = resolver.resolve_for_parent(short_alias).unwrap();
        let long_alias_session = resolver.resolve_for_parent(long_alias).unwrap();
        assert_eq!(short_alias_session.primary().pin().name(), "archive");
        assert_eq!(
            long_alias_session.primary().pin().name(),
            "archive_copy_archive"
        );
        assert_eq!(
            short_alias_session.primary().pin().commit().as_str(),
            short_revision_commit
        );
        assert_eq!(
            long_alias_session.primary().pin().commit().as_str(),
            shared_base_commit
        );
        assert_eq!(short_alias_session.attached().count(), 0);
        assert_eq!(long_alias_session.attached().count(), 0);
        assert_eq!(
            root_session
                .database("archive_copy_archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            longest_current_commit
        );
    }

    #[test]
    fn nested_pin_reverse_prefix_distinct_skipped_dissolve_preserves_middle_alias() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let shared_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, _shared_base_commit) = repository(shared_source);

        let short_revision_commit = write_commit(
            shared_dir.path(),
            "main.orna",
            &shared_source.replace("42", "7"),
        );
        let middle_revision_commit = write_commit(
            shared_dir.path(),
            "main.orna",
            &shared_source.replace("42", "8"),
        );
        let long_revision_commit = write_commit(
            shared_dir.path(),
            "main.orna",
            &shared_source.replace("42", "9"),
        );
        let historical_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "archive {short_revision_commit}\narchive_copy {middle_revision_commit}\narchive_copy_archive {long_revision_commit}\n"
            ),
        );
        let current_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy_archive_archive {historical_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy_archive {current_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), shared_repository.clone()),
                ("archive_copy".to_owned(), shared_repository.clone()),
                ("archive_copy_archive".to_owned(), shared_repository.clone()),
                (
                    "archive_copy_archive_archive".to_owned(),
                    shared_repository,
                ),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let current_pin = root_session
            .database("archive_copy_archive")
            .unwrap()
            .clone();
        let current_closure = resolver.resolve_for_parent(current_pin).unwrap();
        assert_eq!(
            current_closure.primary().pin().commit().as_str(),
            current_commit
        );
        let historical_pin = current_closure
            .database("archive_copy_archive_archive")
            .unwrap()
            .clone();
        let mut short_first = resolver
            .resolve_for_parent(historical_pin.clone())
            .unwrap();
        let mut long_first = resolver
            .resolve_for_parent(historical_pin.clone())
            .unwrap();
        let untouched_sibling = resolver
            .resolve_for_parent(historical_pin)
            .unwrap();
        let short_alias = short_first.database("archive").unwrap().clone();
        let middle_alias = short_first.database("archive_copy").unwrap().clone();
        let long_alias = short_first
            .database("archive_copy_archive")
            .unwrap()
            .clone();

        assert_eq!(
            short_first.primary().pin().name(),
            "archive_copy_archive_archive"
        );
        assert_eq!(
            short_first.primary().pin().commit().as_str(),
            historical_commit
        );
        assert_eq!(short_alias.pin().commit().as_str(), short_revision_commit);
        assert_eq!(middle_alias.pin().commit().as_str(), middle_revision_commit);
        assert_eq!(long_alias.pin().commit().as_str(), long_revision_commit);
        assert_ne!(short_alias.pin().commit(), middle_alias.pin().commit());
        assert_ne!(middle_alias.pin().commit(), long_alias.pin().commit());
        assert_ne!(short_alias.pin().commit(), long_alias.pin().commit());

        // The reference fixes these OIDs but is silent on dissolving skipped
        // endpoints around the middle alias. V1 removes each full name in
        // either order and keeps the operation local to each closure.
        short_first.detach_database("archive").unwrap();
        assert!(short_first.database("archive").is_none());
        assert_eq!(
            short_first.database("archive_copy").unwrap().pin(),
            middle_alias.pin()
        );
        assert_eq!(
            short_first
                .database("archive_copy_archive")
                .unwrap()
                .pin(),
            long_alias.pin()
        );
        short_first
            .detach_database("archive_copy_archive")
            .unwrap();
        assert!(short_first.database("archive_copy_archive").is_none());
        assert_eq!(
            short_first.database("archive_copy").unwrap().pin(),
            middle_alias.pin()
        );
        short_first.detach_database("archive_copy").unwrap();
        assert!(short_first.database("archive_copy").is_none());

        long_first
            .detach_database("archive_copy_archive")
            .unwrap();
        assert!(long_first.database("archive_copy_archive").is_none());
        assert_eq!(
            long_first.database("archive").unwrap().pin(),
            short_alias.pin()
        );
        assert_eq!(
            long_first.database("archive_copy").unwrap().pin(),
            middle_alias.pin()
        );
        long_first.detach_database("archive").unwrap();
        assert!(long_first.database("archive").is_none());
        assert_eq!(
            long_first.database("archive_copy").unwrap().pin(),
            middle_alias.pin()
        );
        long_first.detach_database("archive_copy").unwrap();
        assert!(long_first.database("archive_copy").is_none());

        assert_eq!(untouched_sibling.attached().count(), 3);
        assert_eq!(
            untouched_sibling.database("archive").unwrap().pin(),
            short_alias.pin()
        );
        assert_eq!(
            untouched_sibling.database("archive_copy").unwrap().pin(),
            middle_alias.pin()
        );
        assert_eq!(
            untouched_sibling
                .database("archive_copy_archive")
                .unwrap()
                .pin(),
            long_alias.pin()
        );
        for (pin, expected_name, expected_commit) in [
            (&short_alias, "archive", short_revision_commit.as_str()),
            (&middle_alias, "archive_copy", middle_revision_commit.as_str()),
            (&long_alias, "archive_copy_archive", long_revision_commit.as_str()),
        ] {
            let terminal = resolver.resolve_for_parent(pin.clone()).unwrap();
            assert_eq!(terminal.primary().pin().name(), expected_name);
            assert_eq!(terminal.primary().pin().commit().as_str(), expected_commit);
            assert_eq!(terminal.attached().count(), 0);
        }
        assert_eq!(
            root_session
                .database("archive_copy_archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            current_commit
        );
    }

    #[test]
    fn nested_pin_reverse_prefix_distinct_middle_first_dissolve_keeps_endpoints() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let shared_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, _) = repository(shared_source);

        let short_revision_commit = write_commit(
            shared_dir.path(),
            "main.orna",
            &shared_source.replace("42", "7"),
        );
        let middle_revision_commit = write_commit(
            shared_dir.path(),
            "main.orna",
            &shared_source.replace("42", "8"),
        );
        let long_revision_commit = write_commit(
            shared_dir.path(),
            "main.orna",
            &shared_source.replace("42", "9"),
        );
        let historical_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "archive {short_revision_commit}\narchive_copy {middle_revision_commit}\narchive_copy_archive {long_revision_commit}\n"
            ),
        );
        let current_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy_archive_archive {historical_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy_archive {current_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), shared_repository.clone()),
                ("archive_copy".to_owned(), shared_repository.clone()),
                ("archive_copy_archive".to_owned(), shared_repository.clone()),
                (
                    "archive_copy_archive_archive".to_owned(),
                    shared_repository,
                ),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let current_closure = resolver
            .resolve_for_parent(
                root_session
                    .database("archive_copy_archive")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        let historical_pin = current_closure
            .database("archive_copy_archive_archive")
            .unwrap()
            .clone();
        let mut middle_first = resolver
            .resolve_for_parent(historical_pin.clone())
            .unwrap();
        let mut endpoints_first = resolver
            .resolve_for_parent(historical_pin.clone())
            .unwrap();
        let untouched_sibling = resolver
            .resolve_for_parent(historical_pin)
            .unwrap();
        let short_alias = middle_first.database("archive").unwrap().clone();
        let middle_alias = middle_first.database("archive_copy").unwrap().clone();
        let long_alias = middle_first
            .database("archive_copy_archive")
            .unwrap()
            .clone();

        assert_eq!(
            middle_first.primary().pin().name(),
            "archive_copy_archive_archive"
        );
        assert_eq!(
            middle_first.primary().pin().commit().as_str(),
            historical_commit
        );
        assert_eq!(short_alias.pin().commit().as_str(), short_revision_commit);
        assert_eq!(middle_alias.pin().commit().as_str(), middle_revision_commit);
        assert_eq!(long_alias.pin().commit().as_str(), long_revision_commit);
        assert_ne!(short_alias.pin().commit(), middle_alias.pin().commit());
        assert_ne!(middle_alias.pin().commit(), long_alias.pin().commit());
        assert_ne!(short_alias.pin().commit(), long_alias.pin().commit());

        // The reference does not specify middle-first dissolution around
        // skipped prefixes. V1 removes the middle alias by exact name and
        // leaves both differently pinned endpoints available in this closure.
        middle_first.detach_database("archive_copy").unwrap();
        assert!(middle_first.database("archive_copy").is_none());
        assert_eq!(
            middle_first.database("archive").unwrap().pin(),
            short_alias.pin()
        );
        assert_eq!(
            middle_first
                .database("archive_copy_archive")
                .unwrap()
                .pin(),
            long_alias.pin()
        );
        middle_first.detach_database("archive").unwrap();
        middle_first
            .detach_database("archive_copy_archive")
            .unwrap();
        assert_eq!(middle_first.attached().count(), 0);

        endpoints_first.detach_database("archive").unwrap();
        assert_eq!(
            endpoints_first.database("archive_copy").unwrap().pin(),
            middle_alias.pin()
        );
        assert_eq!(
            endpoints_first
                .database("archive_copy_archive")
                .unwrap()
                .pin(),
            long_alias.pin()
        );
        endpoints_first
            .detach_database("archive_copy_archive")
            .unwrap();
        assert_eq!(
            endpoints_first.database("archive_copy").unwrap().pin(),
            middle_alias.pin()
        );
        endpoints_first
            .detach_database("archive_copy")
            .unwrap();
        assert_eq!(endpoints_first.attached().count(), 0);

        assert_eq!(untouched_sibling.attached().count(), 3);
        assert_eq!(
            untouched_sibling.database("archive").unwrap().pin(),
            short_alias.pin()
        );
        assert_eq!(
            untouched_sibling.database("archive_copy").unwrap().pin(),
            middle_alias.pin()
        );
        assert_eq!(
            untouched_sibling
                .database("archive_copy_archive")
                .unwrap()
                .pin(),
            long_alias.pin()
        );
        for (pin, expected_name, expected_commit) in [
            (&short_alias, "archive", short_revision_commit.as_str()),
            (&middle_alias, "archive_copy", middle_revision_commit.as_str()),
            (&long_alias, "archive_copy_archive", long_revision_commit.as_str()),
        ] {
            let terminal = resolver.resolve_for_parent(pin.clone()).unwrap();
            assert_eq!(terminal.primary().pin().name(), expected_name);
            assert_eq!(terminal.primary().pin().commit().as_str(), expected_commit);
            assert_eq!(terminal.attached().count(), 0);
        }
        assert_eq!(
            root_session
                .database("archive_copy_archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            current_commit
        );
    }

    #[test]
    fn nested_pin_reverse_prefix_endpoint_reattach_keeps_middle_revision() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let shared_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, _) = repository(shared_source);

        let short_revision_commit = write_commit(
            shared_dir.path(),
            "main.orna",
            &shared_source.replace("42", "7"),
        );
        let middle_revision_commit = write_commit(
            shared_dir.path(),
            "main.orna",
            &shared_source.replace("42", "8"),
        );
        let long_revision_commit = write_commit(
            shared_dir.path(),
            "main.orna",
            &shared_source.replace("42", "9"),
        );
        let replacement_short_commit = write_commit(
            shared_dir.path(),
            "main.orna",
            &shared_source.replace("42", "10"),
        );
        let replacement_long_commit = write_commit(
            shared_dir.path(),
            "main.orna",
            &shared_source.replace("42", "11"),
        );
        let historical_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "archive {short_revision_commit}\narchive_copy {middle_revision_commit}\narchive_copy_archive {long_revision_commit}\n"
            ),
        );
        let current_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy_archive_archive {historical_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy_archive {current_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), shared_repository.clone()),
                ("archive_copy".to_owned(), shared_repository.clone()),
                ("archive_copy_archive".to_owned(), shared_repository.clone()),
                (
                    "archive_copy_archive_archive".to_owned(),
                    shared_repository.clone(),
                ),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let current_closure = resolver
            .resolve_for_parent(
                root_session
                    .database("archive_copy_archive")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        let historical_pin = current_closure
            .database("archive_copy_archive_archive")
            .unwrap()
            .clone();
        let mut historical = resolver
            .resolve_for_parent(historical_pin.clone())
            .unwrap();
        let historical_sibling = resolver
            .resolve_for_parent(historical_pin)
            .unwrap();
        let short_pin = historical.database("archive").unwrap().clone();
        let middle_pin = historical.database("archive_copy").unwrap().clone();
        let long_pin = historical
            .database("archive_copy_archive")
            .unwrap()
            .clone();
        let replacement_short = PinnedDatabase::resolve(
            "archive",
            shared_repository.clone(),
            &replacement_short_commit,
            loader,
        )
        .unwrap();
        let replacement_long = PinnedDatabase::resolve(
            "archive_copy_archive",
            shared_repository,
            &replacement_long_commit,
            loader,
        )
        .unwrap();

        assert_eq!(historical.primary().pin().name(), "archive_copy_archive_archive");
        assert_eq!(historical.primary().pin().commit().as_str(), historical_commit);
        assert_eq!(short_pin.pin().commit().as_str(), short_revision_commit);
        assert_eq!(middle_pin.pin().commit().as_str(), middle_revision_commit);
        assert_eq!(long_pin.pin().commit().as_str(), long_revision_commit);
        assert_ne!(short_pin.pin().commit(), middle_pin.pin().commit());
        assert_ne!(middle_pin.pin().commit(), long_pin.pin().commit());

        // The reference selects exact historical revisions but is silent on
        // reattaching endpoint aliases around a retained middle prefix. V1
        // replaces each full alias only in the selected closure.
        historical.detach_database("archive").unwrap();
        historical.attach_database(replacement_short.clone()).unwrap();
        assert_eq!(
            historical.database("archive").unwrap().pin(),
            replacement_short.pin()
        );
        assert_eq!(
            historical.database("archive_copy").unwrap().pin(),
            middle_pin.pin()
        );
        assert_eq!(
            historical
                .database("archive_copy_archive")
                .unwrap()
                .pin(),
            long_pin.pin()
        );

        historical
            .detach_database("archive_copy_archive")
            .unwrap();
        historical.attach_database(replacement_long.clone()).unwrap();
        assert_eq!(
            historical
                .database("archive_copy_archive")
                .unwrap()
                .pin(),
            replacement_long.pin()
        );
        assert_eq!(
            historical.database("archive_copy").unwrap().pin(),
            middle_pin.pin()
        );
        assert_eq!(
            historical_sibling.database("archive").unwrap().pin(),
            short_pin.pin()
        );
        assert_eq!(
            historical_sibling
                .database("archive_copy")
                .unwrap()
                .pin(),
            middle_pin.pin()
        );
        assert_eq!(
            historical_sibling
                .database("archive_copy_archive")
                .unwrap()
                .pin(),
            long_pin.pin()
        );
        let short_session = resolver
            .resolve_for_parent(replacement_short)
            .unwrap();
        let long_session = resolver.resolve_for_parent(replacement_long).unwrap();
        assert_eq!(short_session.primary().pin().commit().as_str(), replacement_short_commit);
        assert_eq!(long_session.primary().pin().commit().as_str(), replacement_long_commit);
        assert_eq!(short_session.attached().count(), 0);
        assert_eq!(long_session.attached().count(), 0);
        assert_eq!(
            root_session
                .database("archive_copy_archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            current_commit
        );
    }

    #[test]
    fn nested_pin_reverse_prefix_middle_reattach_keeps_skipped_endpoints() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let shared_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, _) = repository(shared_source);

        let short_revision_commit = write_commit(
            shared_dir.path(),
            "main.orna",
            &shared_source.replace("42", "7"),
        );
        let middle_revision_commit = write_commit(
            shared_dir.path(),
            "main.orna",
            &shared_source.replace("42", "8"),
        );
        let long_revision_commit = write_commit(
            shared_dir.path(),
            "main.orna",
            &shared_source.replace("42", "9"),
        );
        let replacement_middle_commit = write_commit(
            shared_dir.path(),
            "main.orna",
            &shared_source.replace("42", "10"),
        );
        let historical_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "archive {short_revision_commit}\narchive_copy {middle_revision_commit}\narchive_copy_archive {long_revision_commit}\n"
            ),
        );
        let current_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy_archive_archive {historical_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy_archive {current_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), shared_repository.clone()),
                ("archive_copy".to_owned(), shared_repository.clone()),
                ("archive_copy_archive".to_owned(), shared_repository.clone()),
                (
                    "archive_copy_archive_archive".to_owned(),
                    shared_repository.clone(),
                ),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let current_closure = resolver
            .resolve_for_parent(
                root_session
                    .database("archive_copy_archive")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        let historical_pin = current_closure
            .database("archive_copy_archive_archive")
            .unwrap()
            .clone();
        let mut historical = resolver
            .resolve_for_parent(historical_pin.clone())
            .unwrap();
        let historical_sibling = resolver
            .resolve_for_parent(historical_pin)
            .unwrap();
        let short_pin = historical.database("archive").unwrap().clone();
        let middle_pin = historical.database("archive_copy").unwrap().clone();
        let long_pin = historical
            .database("archive_copy_archive")
            .unwrap()
            .clone();
        let replacement_middle = PinnedDatabase::resolve(
            "archive_copy",
            shared_repository,
            &replacement_middle_commit,
            loader,
        )
        .unwrap();

        assert_eq!(historical.primary().pin().name(), "archive_copy_archive_archive");
        assert_eq!(historical.primary().pin().commit().as_str(), historical_commit);
        assert_eq!(short_pin.pin().commit().as_str(), short_revision_commit);
        assert_eq!(middle_pin.pin().commit().as_str(), middle_revision_commit);
        assert_eq!(long_pin.pin().commit().as_str(), long_revision_commit);
        assert_ne!(short_pin.pin().commit(), middle_pin.pin().commit());
        assert_ne!(middle_pin.pin().commit(), long_pin.pin().commit());

        // The reference fixes the three original revisions but is silent on
        // replacing the middle alias. V1 updates only that exact alias in
        // the selected closure and preserves both skipped-prefix endpoints.
        historical.detach_database("archive_copy").unwrap();
        historical.attach_database(replacement_middle.clone()).unwrap();
        assert_eq!(
            historical.database("archive").unwrap().pin(),
            short_pin.pin()
        );
        assert_eq!(
            historical.database("archive_copy").unwrap().pin(),
            replacement_middle.pin()
        );
        assert_eq!(
            historical
                .database("archive_copy_archive")
                .unwrap()
                .pin(),
            long_pin.pin()
        );
        assert_eq!(
            historical_sibling.database("archive").unwrap().pin(),
            short_pin.pin()
        );
        assert_eq!(
            historical_sibling
                .database("archive_copy")
                .unwrap()
                .pin(),
            middle_pin.pin()
        );
        assert_eq!(
            historical_sibling
                .database("archive_copy_archive")
                .unwrap()
                .pin(),
            long_pin.pin()
        );
        let replacement_session = resolver
            .resolve_for_parent(replacement_middle)
            .unwrap();
        assert_eq!(
            replacement_session.primary().pin().name(),
            "archive_copy"
        );
        assert_eq!(
            replacement_session.primary().pin().commit().as_str(),
            replacement_middle_commit
        );
        assert_eq!(replacement_session.attached().count(), 0);
        assert_eq!(
            root_session
                .database("archive_copy_archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            current_commit
        );
    }

    #[test]
    fn nested_pin_reverse_prefix_middle_reattach_keeps_equal_oid_endpoint_distinct() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let shared_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, _) = repository(shared_source);

        let short_revision_commit = write_commit(
            shared_dir.path(),
            "main.orna",
            &shared_source.replace("42", "7"),
        );
        let middle_revision_commit = write_commit(
            shared_dir.path(),
            "main.orna",
            &shared_source.replace("42", "8"),
        );
        let long_revision_commit = write_commit(
            shared_dir.path(),
            "main.orna",
            &shared_source.replace("42", "9"),
        );
        let historical_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "archive {short_revision_commit}\narchive_copy {middle_revision_commit}\narchive_copy_archive {long_revision_commit}\n"
            ),
        );
        let current_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy_archive_archive {historical_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy_archive {current_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), shared_repository.clone()),
                ("archive_copy".to_owned(), shared_repository.clone()),
                ("archive_copy_archive".to_owned(), shared_repository.clone()),
                (
                    "archive_copy_archive_archive".to_owned(),
                    shared_repository.clone(),
                ),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let current_closure = resolver
            .resolve_for_parent(
                root_session
                    .database("archive_copy_archive")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        let historical_pin = current_closure
            .database("archive_copy_archive_archive")
            .unwrap()
            .clone();
        let mut historical = resolver
            .resolve_for_parent(historical_pin.clone())
            .unwrap();
        let historical_sibling = resolver
            .resolve_for_parent(historical_pin)
            .unwrap();
        let short_pin = historical.database("archive").unwrap().clone();
        let middle_pin = historical.database("archive_copy").unwrap().clone();
        let long_pin = historical
            .database("archive_copy_archive")
            .unwrap()
            .clone();
        let replacement_middle = PinnedDatabase::resolve(
            "archive_copy",
            shared_repository,
            &short_revision_commit,
            loader,
        )
        .unwrap();

        assert_eq!(historical.primary().pin().name(), "archive_copy_archive_archive");
        assert_eq!(historical.primary().pin().commit().as_str(), historical_commit);
        assert_eq!(short_pin.pin().commit().as_str(), short_revision_commit);
        assert_eq!(middle_pin.pin().commit().as_str(), middle_revision_commit);
        assert_eq!(long_pin.pin().commit().as_str(), long_revision_commit);
        assert_ne!(short_pin.pin(), middle_pin.pin());

        // The reference is silent on replacing the middle alias with an
        // endpoint's exact OID. V1 preserves full alias identity and changes
        // only the selected closure's middle pin.
        historical.detach_database("archive_copy").unwrap();
        historical.attach_database(replacement_middle.clone()).unwrap();
        let replaced_middle = historical.database("archive_copy").unwrap();
        assert_eq!(replaced_middle.pin().commit(), short_pin.pin().commit());
        assert_ne!(replaced_middle.pin(), short_pin.pin());
        assert_eq!(
            replaced_middle.pin().commit().as_str(),
            short_revision_commit
        );
        assert_eq!(
            historical.database("archive").unwrap().pin(),
            short_pin.pin()
        );
        assert_eq!(
            historical
                .database("archive_copy_archive")
                .unwrap()
                .pin(),
            long_pin.pin()
        );
        assert_eq!(
            historical_sibling.database("archive").unwrap().pin(),
            short_pin.pin()
        );
        assert_eq!(
            historical_sibling
                .database("archive_copy")
                .unwrap()
                .pin(),
            middle_pin.pin()
        );
        assert_eq!(
            historical_sibling
                .database("archive_copy_archive")
                .unwrap()
                .pin(),
            long_pin.pin()
        );
        let replacement_session = resolver
            .resolve_for_parent(replacement_middle)
            .unwrap();
        assert_eq!(
            replacement_session.primary().pin().name(),
            "archive_copy"
        );
        assert_eq!(
            replacement_session.primary().pin().commit().as_str(),
            short_revision_commit
        );
        assert_eq!(replacement_session.attached().count(), 0);
        assert_eq!(
            root_session
                .database("archive_copy_archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            current_commit
        );
    }

    #[test]
    fn nested_pin_reverse_prefix_middle_reattach_matches_long_endpoint_only() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let shared_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, _) = repository(shared_source);

        let short_revision_commit = write_commit(
            shared_dir.path(),
            "main.orna",
            &shared_source.replace("42", "7"),
        );
        let middle_revision_commit = write_commit(
            shared_dir.path(),
            "main.orna",
            &shared_source.replace("42", "8"),
        );
        let long_revision_commit = write_commit(
            shared_dir.path(),
            "main.orna",
            &shared_source.replace("42", "9"),
        );
        let historical_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "archive {short_revision_commit}\narchive_copy {middle_revision_commit}\narchive_copy_archive {long_revision_commit}\n"
            ),
        );
        let current_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy_archive_archive {historical_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy_archive {current_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), shared_repository.clone()),
                ("archive_copy".to_owned(), shared_repository.clone()),
                ("archive_copy_archive".to_owned(), shared_repository.clone()),
                (
                    "archive_copy_archive_archive".to_owned(),
                    shared_repository.clone(),
                ),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let current_closure = resolver
            .resolve_for_parent(
                root_session
                    .database("archive_copy_archive")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        let historical_pin = current_closure
            .database("archive_copy_archive_archive")
            .unwrap()
            .clone();
        let mut historical = resolver
            .resolve_for_parent(historical_pin.clone())
            .unwrap();
        let historical_sibling = resolver
            .resolve_for_parent(historical_pin)
            .unwrap();
        let short_pin = historical.database("archive").unwrap().clone();
        let middle_pin = historical.database("archive_copy").unwrap().clone();
        let long_pin = historical
            .database("archive_copy_archive")
            .unwrap()
            .clone();
        let replacement_middle = PinnedDatabase::resolve(
            "archive_copy",
            shared_repository,
            &long_revision_commit,
            loader,
        )
        .unwrap();

        assert_eq!(historical.primary().pin().name(), "archive_copy_archive_archive");
        assert_eq!(historical.primary().pin().commit().as_str(), historical_commit);
        assert_eq!(short_pin.pin().commit().as_str(), short_revision_commit);
        assert_eq!(middle_pin.pin().commit().as_str(), middle_revision_commit);
        assert_eq!(long_pin.pin().commit().as_str(), long_revision_commit);
        assert_ne!(short_pin.pin().commit(), long_pin.pin().commit());

        // The reference is silent on reattaching the middle alias at the long
        // endpoint's OID. V1 preserves the endpoint distinction by exact alias
        // even when middle and long now share one immutable revision.
        historical.detach_database("archive_copy").unwrap();
        historical.attach_database(replacement_middle.clone()).unwrap();
        let replaced_middle = historical.database("archive_copy").unwrap();
        let unchanged_short = historical.database("archive").unwrap();
        let unchanged_long = historical.database("archive_copy_archive").unwrap();
        assert_eq!(
            replaced_middle.pin().commit(),
            unchanged_long.pin().commit()
        );
        assert_ne!(replaced_middle.pin(), unchanged_long.pin());
        assert_eq!(unchanged_short.pin(), short_pin.pin());
        assert_eq!(unchanged_long.pin(), long_pin.pin());
        assert_ne!(unchanged_short.pin().commit(), unchanged_long.pin().commit());
        assert_eq!(
            replaced_middle.pin().commit().as_str(),
            long_revision_commit
        );
        assert_eq!(
            historical_sibling.database("archive").unwrap().pin(),
            short_pin.pin()
        );
        assert_eq!(
            historical_sibling
                .database("archive_copy")
                .unwrap()
                .pin(),
            middle_pin.pin()
        );
        assert_eq!(
            historical_sibling
                .database("archive_copy_archive")
                .unwrap()
                .pin(),
            long_pin.pin()
        );
        let replacement_session = resolver
            .resolve_for_parent(replacement_middle)
            .unwrap();
        assert_eq!(
            replacement_session.primary().pin().name(),
            "archive_copy"
        );
        assert_eq!(
            replacement_session.primary().pin().commit().as_str(),
            long_revision_commit
        );
        assert_eq!(replacement_session.attached().count(), 0);
        assert_eq!(
            root_session
                .database("archive_copy_archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            current_commit
        );
    }

    #[test]
    fn nested_pin_reverse_prefix_long_reattach_matches_middle_by_oid_only() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let shared_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, _) = repository(shared_source);

        let short_revision_commit = write_commit(
            shared_dir.path(),
            "main.orna",
            &shared_source.replace("42", "7"),
        );
        let middle_revision_commit = write_commit(
            shared_dir.path(),
            "main.orna",
            &shared_source.replace("42", "8"),
        );
        let long_revision_commit = write_commit(
            shared_dir.path(),
            "main.orna",
            &shared_source.replace("42", "9"),
        );
        let historical_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "archive {short_revision_commit}\narchive_copy {middle_revision_commit}\narchive_copy_archive {long_revision_commit}\n"
            ),
        );
        let current_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy_archive_archive {historical_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy_archive {current_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), shared_repository.clone()),
                ("archive_copy".to_owned(), shared_repository.clone()),
                ("archive_copy_archive".to_owned(), shared_repository.clone()),
                (
                    "archive_copy_archive_archive".to_owned(),
                    shared_repository.clone(),
                ),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let current_closure = resolver
            .resolve_for_parent(
                root_session
                    .database("archive_copy_archive")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        let historical_pin = current_closure
            .database("archive_copy_archive_archive")
            .unwrap()
            .clone();
        let mut historical = resolver
            .resolve_for_parent(historical_pin.clone())
            .unwrap();
        let historical_sibling = resolver
            .resolve_for_parent(historical_pin)
            .unwrap();
        let short_pin = historical.database("archive").unwrap().clone();
        let middle_pin = historical.database("archive_copy").unwrap().clone();
        let long_pin = historical
            .database("archive_copy_archive")
            .unwrap()
            .clone();
        let replacement_long = PinnedDatabase::resolve(
            "archive_copy_archive",
            shared_repository,
            &middle_revision_commit,
            loader,
        )
        .unwrap();

        assert_eq!(historical.primary().pin().name(), "archive_copy_archive_archive");
        assert_eq!(historical.primary().pin().commit().as_str(), historical_commit);
        assert_eq!(short_pin.pin().commit().as_str(), short_revision_commit);
        assert_eq!(middle_pin.pin().commit().as_str(), middle_revision_commit);
        assert_eq!(long_pin.pin().commit().as_str(), long_revision_commit);

        // The reference fixes the historical pins but is silent on rebinding
        // the longer alias to the middle alias's revision. V1 keys both by
        // their full names, even when they now select the same commit.
        historical
            .detach_database("archive_copy_archive")
            .unwrap();
        historical.attach_database(replacement_long.clone()).unwrap();
        let retained_middle = historical.database("archive_copy").unwrap();
        let rebound_long = historical
            .database("archive_copy_archive")
            .unwrap();
        assert_eq!(retained_middle.pin().commit(), rebound_long.pin().commit());
        assert_ne!(retained_middle.pin(), rebound_long.pin());
        assert_eq!(
            retained_middle.pin().commit().as_str(),
            middle_revision_commit
        );
        assert_eq!(
            historical.database("archive").unwrap().pin(),
            short_pin.pin()
        );
        assert_eq!(
            historical_sibling.database("archive").unwrap().pin(),
            short_pin.pin()
        );
        assert_eq!(
            historical_sibling
                .database("archive_copy").unwrap().pin(),
            middle_pin.pin()
        );
        assert_eq!(
            historical_sibling
                .database("archive_copy_archive")
                .unwrap()
                .pin(),
            long_pin.pin()
        );
        let rebound_session = resolver.resolve_for_parent(replacement_long).unwrap();
        assert_eq!(
            rebound_session.primary().pin().name(),
            "archive_copy_archive"
        );
        assert_eq!(
            rebound_session.primary().pin().commit().as_str(),
            middle_revision_commit
        );
        assert_eq!(rebound_session.attached().count(), 0);
        assert_eq!(
            root_session
                .database("archive_copy_archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            current_commit
        );
    }

    #[test]
    fn nested_pin_long_rebind_keeps_alias_distinct_at_short_endpoint_oid() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let shared_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, _) = repository(shared_source);

        let short_revision_commit = write_commit(
            shared_dir.path(),
            "main.orna",
            &shared_source.replace("42", "7"),
        );
        let middle_revision_commit = write_commit(
            shared_dir.path(),
            "main.orna",
            &shared_source.replace("42", "8"),
        );
        let long_revision_commit = write_commit(
            shared_dir.path(),
            "main.orna",
            &shared_source.replace("42", "9"),
        );
        let historical_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "archive {short_revision_commit}\narchive_copy {middle_revision_commit}\narchive_copy_archive {long_revision_commit}\n"
            ),
        );
        let current_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy_archive_archive {historical_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy_archive {current_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app =
            PinnedDatabase::resolve("app", app_repository.clone(), &app_commit, loader).unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), shared_repository.clone()),
                ("archive_copy".to_owned(), shared_repository.clone()),
                ("archive_copy_archive".to_owned(), shared_repository.clone()),
                (
                    "archive_copy_archive_archive".to_owned(),
                    shared_repository.clone(),
                ),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let current_closure = resolver
            .resolve_for_parent(
                root_session
                    .database("archive_copy_archive")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        let historical_pin = current_closure
            .database("archive_copy_archive_archive")
            .unwrap()
            .clone();
        let mut historical = resolver.resolve_for_parent(historical_pin.clone()).unwrap();
        let historical_sibling = resolver.resolve_for_parent(historical_pin).unwrap();
        let short_pin = historical.database("archive").unwrap().clone();
        let middle_pin = historical.database("archive_copy").unwrap().clone();
        let long_pin = historical.database("archive_copy_archive").unwrap().clone();
        let replacement_long = PinnedDatabase::resolve(
            "archive_copy_archive",
            shared_repository,
            &short_revision_commit,
            loader,
        )
        .unwrap();

        assert_eq!(short_pin.pin().commit().as_str(), short_revision_commit);
        assert_eq!(middle_pin.pin().commit().as_str(), middle_revision_commit);
        assert_eq!(long_pin.pin().commit().as_str(), long_revision_commit);

        // The reference fixes historical pins but is silent on rebinding a
        // longer alias to the short alias's revision. V1 preserves each name.
        historical.detach_database("archive_copy_archive").unwrap();
        historical
            .attach_database(replacement_long.clone())
            .unwrap();
        let rebound_short = historical.database("archive").unwrap();
        let rebound_long = historical.database("archive_copy_archive").unwrap();
        assert_eq!(rebound_short.pin().commit(), rebound_long.pin().commit());
        assert_ne!(rebound_short.pin(), rebound_long.pin());
        assert_eq!(rebound_short.pin(), short_pin.pin());
        assert_eq!(
            historical.database("archive_copy").unwrap().pin(),
            middle_pin.pin()
        );
        assert_eq!(
            historical_sibling.database("archive").unwrap().pin(),
            short_pin.pin()
        );
        assert_eq!(
            historical_sibling.database("archive_copy").unwrap().pin(),
            middle_pin.pin()
        );
        assert_eq!(
            historical_sibling
                .database("archive_copy_archive")
                .unwrap()
                .pin(),
            long_pin.pin()
        );
        let rebound_session = resolver.resolve_for_parent(replacement_long).unwrap();
        assert_eq!(
            rebound_session.primary().pin().name(),
            "archive_copy_archive"
        );
        assert_eq!(
            rebound_session.primary().pin().commit().as_str(),
            short_revision_commit
        );
        assert_eq!(rebound_session.attached().count(), 0);
        assert_eq!(
            root_session
                .database("archive_copy_archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            current_commit
        );
    }

    #[test]
    fn nested_pin_short_rebind_keeps_alias_distinct_at_long_endpoint_oid() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let shared_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, _) = repository(shared_source);

        let short_revision_commit = write_commit(
            shared_dir.path(),
            "main.orna",
            &shared_source.replace("42", "7"),
        );
        let middle_revision_commit = write_commit(
            shared_dir.path(),
            "main.orna",
            &shared_source.replace("42", "8"),
        );
        let long_revision_commit = write_commit(
            shared_dir.path(),
            "main.orna",
            &shared_source.replace("42", "9"),
        );
        let historical_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "archive {short_revision_commit}\narchive_copy {middle_revision_commit}\narchive_copy_archive {long_revision_commit}\n"
            ),
        );
        let current_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy_archive_archive {historical_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy_archive {current_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve("app", app_repository.clone(), &app_commit, loader)
            .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), shared_repository.clone()),
                ("archive_copy".to_owned(), shared_repository.clone()),
                ("archive_copy_archive".to_owned(), shared_repository.clone()),
                (
                    "archive_copy_archive_archive".to_owned(),
                    shared_repository.clone(),
                ),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let current_closure = resolver
            .resolve_for_parent(
                root_session
                    .database("archive_copy_archive")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        let historical_pin = current_closure
            .database("archive_copy_archive_archive")
            .unwrap()
            .clone();
        let mut historical = resolver.resolve_for_parent(historical_pin.clone()).unwrap();
        let historical_sibling = resolver.resolve_for_parent(historical_pin).unwrap();
        let short_pin = historical.database("archive").unwrap().clone();
        let middle_pin = historical.database("archive_copy").unwrap().clone();
        let long_pin = historical.database("archive_copy_archive").unwrap().clone();
        let replacement_short = PinnedDatabase::resolve(
            "archive",
            shared_repository,
            &long_revision_commit,
            loader,
        )
        .unwrap();

        assert_eq!(short_pin.pin().commit().as_str(), short_revision_commit);
        assert_eq!(middle_pin.pin().commit().as_str(), middle_revision_commit);
        assert_eq!(long_pin.pin().commit().as_str(), long_revision_commit);

        // The reference fixes historical pins but is silent on rebinding the
        // short alias to a longer alias's revision. V1 preserves each name.
        historical.detach_database("archive").unwrap();
        historical.attach_database(replacement_short.clone()).unwrap();
        let rebound_short = historical.database("archive").unwrap();
        let retained_long = historical.database("archive_copy_archive").unwrap();
        assert_eq!(rebound_short.pin().commit(), retained_long.pin().commit());
        assert_ne!(rebound_short.pin(), retained_long.pin());
        assert_ne!(rebound_short.pin(), short_pin.pin());
        assert_eq!(retained_long.pin(), long_pin.pin());
        assert_eq!(
            historical.database("archive_copy").unwrap().pin(),
            middle_pin.pin()
        );
        assert_eq!(
            historical_sibling.database("archive").unwrap().pin(),
            short_pin.pin()
        );
        assert_eq!(
            historical_sibling.database("archive_copy").unwrap().pin(),
            middle_pin.pin()
        );
        assert_eq!(
            historical_sibling
                .database("archive_copy_archive")
                .unwrap()
                .pin(),
            long_pin.pin()
        );
        let rebound_session = resolver.resolve_for_parent(replacement_short).unwrap();
        assert_eq!(rebound_session.primary().pin().name(), "archive");
        assert_eq!(
            rebound_session.primary().pin().commit().as_str(),
            long_revision_commit
        );
        assert_eq!(rebound_session.attached().count(), 0);
        assert_eq!(
            root_session
                .database("archive_copy_archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            current_commit
        );
    }

    #[test]
    fn nested_pin_short_rebind_keeps_long_endpoint_child_closure() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let shared_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, _) = repository(shared_source);

        let short_revision_commit = write_commit(
            shared_dir.path(),
            "main.orna",
            &shared_source.replace("42", "7"),
        );
        write_commit(
            shared_dir.path(),
            "main.orna",
            &shared_source.replace("42", "8"),
        );
        let middle_revision_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {short_revision_commit}\n"),
        );
        write_commit(
            shared_dir.path(),
            "main.orna",
            &shared_source.replace("42", "9"),
        );
        let long_revision_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy {middle_revision_commit}\n"),
        );
        let historical_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "archive {short_revision_commit}\narchive_copy {middle_revision_commit}\narchive_copy_archive {long_revision_commit}\n"
            ),
        );
        let current_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy_archive_archive {historical_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy_archive {current_commit}\n"),
        );

        let nested_archive_extended_closure_tail_alias =
            "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive"
                .to_owned();
        let nested_archive_final_extended_closure_tail_alias =
            format!("{nested_archive_extended_closure_tail_alias}_archive");
        let nested_archive_next_extended_closure_tail_alias =
            format!("{nested_archive_final_extended_closure_tail_alias}_archive");
        let nested_archive_terminal_extended_closure_tail_alias =
            format!("{nested_archive_next_extended_closure_tail_alias}_archive");
        let nested_archive_ultimate_extended_closure_tail_alias =
            format!("{nested_archive_terminal_extended_closure_tail_alias}_archive");
        let nested_archive_tail_22_closure_alias =
            format!("{nested_archive_ultimate_extended_closure_tail_alias}_archive");
        let nested_archive_tail_23_closure_alias =
            format!("{nested_archive_tail_22_closure_alias}_archive");
        let nested_archive_tail_24_closure_alias =
            format!("{nested_archive_tail_23_closure_alias}_archive");
        let nested_archive_tail_25_closure_alias =
            format!("{nested_archive_tail_24_closure_alias}_archive");
        let nested_archive_tail_26_closure_alias =
            format!("{nested_archive_tail_25_closure_alias}_archive");
        let nested_archive_tail_27_closure_alias =
            format!("{nested_archive_tail_26_closure_alias}_archive");
        let nested_archive_tail_28_closure_alias =
            format!("{nested_archive_tail_27_closure_alias}_archive");
        let nested_archive_tail_29_closure_alias =
            format!("{nested_archive_tail_28_closure_alias}_archive");
        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve("app", app_repository.clone(), &app_commit, loader)
            .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), shared_repository.clone()),
                ("archive_copy".to_owned(), shared_repository.clone()),
                ("archive_copy_archive".to_owned(), shared_repository.clone()),
                ("nested_archive".to_owned(), shared_repository.clone()),
                ("nested_archive_copy".to_owned(), shared_repository.clone()),
                (
                    "nested_archive_copy_archive".to_owned(),
                    shared_repository.clone(),
                ),
                (
                    "nested_archive_copy_archive_copy".to_owned(),
                    shared_repository.clone(),
                ),
                (
                    "nested_archive_copy_archive_copy_archive".to_owned(),
                    shared_repository.clone(),
                ),
                (
                    "nested_archive_copy_archive_copy_archive_copy".to_owned(),
                    shared_repository.clone(),
                ),
                (
                    "nested_archive_copy_archive_copy_archive_copy_archive".to_owned(),
                    shared_repository.clone(),
                ),
                (
                    "nested_archive_copy_archive_copy_archive_copy_archive_copy".to_owned(),
                    shared_repository.clone(),
                ),
                (
                    "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive".to_owned(),
                    shared_repository.clone(),
                ),
                (
                    "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy".to_owned(),
                    shared_repository.clone(),
                ),
                (
                    "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive".to_owned(),
                    shared_repository.clone(),
                ),
                (
                    "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy".to_owned(),
                    shared_repository.clone(),
                ),
                (
                    "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive".to_owned(),
                    shared_repository.clone(),
                ),
                (
                    "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy".to_owned(),
                    shared_repository.clone(),
                ),
                (
                    "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive".to_owned(),
                    shared_repository.clone(),
                ),
                (
                    "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy".to_owned(),
                    shared_repository.clone(),
                ),
                (
                    "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive".to_owned(),
                    shared_repository.clone(),
                ),
                (
                    "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy".to_owned(),
                    shared_repository.clone(),
                ),
                (
                    "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive".to_owned(),
                    shared_repository.clone(),
                ),
                (
                    "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy".to_owned(),
                    shared_repository.clone(),
                ),
                (
                    "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive".to_owned(),
                    shared_repository.clone(),
                ),
                (
                    "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy".to_owned(),
                    shared_repository.clone(),
                ),
                (
                    "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive".to_owned(),
                    shared_repository.clone(),
                ),
                (
                    nested_archive_final_extended_closure_tail_alias.clone(),
                    shared_repository.clone(),
                ),
                (
                    nested_archive_next_extended_closure_tail_alias.clone(),
                    shared_repository.clone(),
                ),
                (
                    nested_archive_terminal_extended_closure_tail_alias.clone(),
                    shared_repository.clone(),
                ),
                (
                    nested_archive_ultimate_extended_closure_tail_alias.clone(),
                    shared_repository.clone(),
                ),
                (
                    nested_archive_tail_22_closure_alias.clone(),
                    shared_repository.clone(),
                ),
                (
                    nested_archive_tail_23_closure_alias.clone(),
                    shared_repository.clone(),
                ),
                (
                    nested_archive_tail_24_closure_alias.clone(),
                    shared_repository.clone(),
                ),
                (
                    nested_archive_tail_25_closure_alias.clone(),
                    shared_repository.clone(),
                ),
                (
                    nested_archive_tail_26_closure_alias.clone(),
                    shared_repository.clone(),
                ),
                (
                    nested_archive_tail_27_closure_alias.clone(),
                    shared_repository.clone(),
                ),
                (
                    nested_archive_tail_28_closure_alias.clone(),
                    shared_repository.clone(),
                ),
                (
                    nested_archive_tail_29_closure_alias.clone(),
                    shared_repository.clone(),
                ),
                (
                    "archive_copy_archive_archive".to_owned(),
                    shared_repository.clone(),
                ),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let current_closure = resolver
            .resolve_for_parent(
                root_session
                    .database("archive_copy_archive")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        let historical_pin = current_closure
            .database("archive_copy_archive_archive")
            .unwrap()
            .clone();
        let mut historical = resolver.resolve_for_parent(historical_pin.clone()).unwrap();
        let historical_sibling = resolver.resolve_for_parent(historical_pin).unwrap();
        let short_pin = historical.database("archive").unwrap().clone();
        let middle_pin = historical.database("archive_copy").unwrap().clone();
        let long_pin = historical.database("archive_copy_archive").unwrap().clone();
        let replacement_short = PinnedDatabase::resolve(
            "archive",
            shared_repository.clone(),
            &long_revision_commit,
            loader,
        )
        .unwrap();

        assert_eq!(short_pin.pin().commit().as_str(), short_revision_commit);
        assert_eq!(middle_pin.pin().commit().as_str(), middle_revision_commit);
        assert_eq!(long_pin.pin().commit().as_str(), long_revision_commit);

        let mut equal_pin_nested_session = resolver
            .resolve_for_parent(
                historical_sibling
                    .database("archive_copy")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            equal_pin_nested_session
                .database("archive")
                .unwrap()
                .pin(),
            historical_sibling.database("archive").unwrap().pin()
        );

        // The reference specifies each historical pin but is silent when an
        // equal parent and nested short pin are rebound independently. V1
        // preserves the unmodified parent occurrence and its sibling closure.
        let equal_pin_replacement = PinnedDatabase::resolve(
            "archive",
            shared_repository.clone(),
            &long_revision_commit,
            loader,
        )
        .unwrap();
        equal_pin_nested_session
            .detach_database("archive")
            .unwrap();
        equal_pin_nested_session
            .attach_database(equal_pin_replacement.clone())
            .unwrap();
        assert_eq!(
            equal_pin_nested_session.primary().pin(),
            middle_pin.pin()
        );
        assert_eq!(
            equal_pin_nested_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            long_revision_commit
        );
        assert_eq!(
            historical_sibling.database("archive").unwrap().pin(),
            short_pin.pin()
        );
        assert_eq!(
            historical_sibling
                .database("archive_copy")
                .unwrap()
                .pin(),
            middle_pin.pin()
        );
        let equal_pin_rebound_session = resolver
            .resolve_for_parent(equal_pin_replacement)
            .unwrap();
        assert_eq!(
            equal_pin_rebound_session.primary().pin().name(),
            "archive"
        );
        assert_eq!(
            equal_pin_rebound_session
                .database("archive_copy")
                .unwrap()
                .pin(),
            middle_pin.pin()
        );
        let equal_pin_rebound_child = resolver
            .resolve_for_parent(
                equal_pin_rebound_session
                    .database("archive_copy")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            equal_pin_rebound_child
                .database("archive")
                .unwrap()
                .pin(),
            short_pin.pin()
        );

        // The reference requires historical pins to resolve exactly but does
        // not specify alias identity or rebind behavior. V1 retains both names.
        let long_endpoint_session = resolver.resolve_for_parent(long_pin.clone()).unwrap();
        assert_eq!(
            long_endpoint_session.primary().pin().name(),
            "archive_copy_archive"
        );
        assert_eq!(
            long_endpoint_session
                .database("archive_copy")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            middle_revision_commit
        );
        let long_endpoint_child_session = resolver
            .resolve_for_parent(
                long_endpoint_session
                    .database("archive_copy")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            long_endpoint_child_session.primary().pin().name(),
            "archive_copy"
        );
        assert_eq!(
            long_endpoint_child_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            short_revision_commit
        );

        historical.detach_database("archive").unwrap();
        historical.attach_database(replacement_short.clone()).unwrap();
        let rebound_short = historical.database("archive").unwrap();
        let retained_long = historical.database("archive_copy_archive").unwrap();
        assert_eq!(rebound_short.pin().commit(), retained_long.pin().commit());
        assert_ne!(rebound_short.pin(), retained_long.pin());
        assert_ne!(rebound_short.pin(), short_pin.pin());
        assert_eq!(retained_long.pin(), long_pin.pin());
        assert_eq!(
            historical.database("archive_copy").unwrap().pin(),
            middle_pin.pin()
        );
        assert_eq!(
            historical_sibling.database("archive").unwrap().pin(),
            short_pin.pin()
        );
        assert_eq!(
            historical_sibling.database("archive_copy").unwrap().pin(),
            middle_pin.pin()
        );
        assert_eq!(
            historical_sibling
                .database("archive_copy_archive")
                .unwrap()
                .pin(),
            long_pin.pin()
        );

        let rebound_session = resolver.resolve_for_parent(replacement_short).unwrap();
        assert_eq!(rebound_session.primary().pin().name(), "archive");
        assert_eq!(
            rebound_session.primary().pin().commit().as_str(),
            long_revision_commit
        );
        assert_eq!(
            rebound_session
                .database("archive_copy")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            middle_revision_commit
        );
        assert_eq!(
            long_endpoint_session.database("archive_copy").unwrap().pin(),
            rebound_session.database("archive_copy").unwrap().pin()
        );
        let mut rebound_child_session = resolver
            .resolve_for_parent(
                rebound_session
                    .database("archive_copy")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            rebound_child_session.primary().pin().name(),
            "archive_copy"
        );
        assert_eq!(
            rebound_child_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            short_revision_commit
        );
        assert_eq!(
            long_endpoint_child_session
                .database("archive")
                .unwrap()
                .pin(),
            rebound_child_session.database("archive").unwrap().pin()
        );

        let nested_child_sibling = resolver
            .resolve_for_parent(
                rebound_session
                    .database("archive_copy")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        let nested_replacement_short = PinnedDatabase::resolve(
            "archive",
            shared_repository.clone(),
            &long_revision_commit,
            loader,
        )
        .unwrap();

        // The reference specifies historical pins but is silent on rebinding
        // a nested short alias to a commit that carries the middle alias too.
        // V1 keeps both names and leaves independently resolved siblings intact.
        rebound_child_session.detach_database("archive").unwrap();
        rebound_child_session
            .attach_database(nested_replacement_short.clone())
            .unwrap();
        assert_eq!(
            rebound_child_session.primary().pin().commit().as_str(),
            middle_revision_commit
        );
        assert_eq!(
            rebound_child_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            long_revision_commit
        );
        assert_eq!(
            nested_child_sibling
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            short_revision_commit
        );

        let nested_rebound_session = resolver
            .resolve_for_parent(nested_replacement_short)
            .unwrap();
        assert_eq!(nested_rebound_session.primary().pin().name(), "archive");
        assert_eq!(
            nested_rebound_session
                .database("archive_copy")
                .unwrap()
                .pin(),
            rebound_session.database("archive_copy").unwrap().pin()
        );
        let nested_rebound_child_session = resolver
            .resolve_for_parent(
                nested_rebound_session
                    .database("archive_copy")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            nested_rebound_child_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            short_revision_commit
        );

        let mut expanded_nested_child = resolver
            .resolve_for_parent(
                historical_sibling
                    .database("archive_copy")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            expanded_nested_child
                .database("archive")
                .unwrap()
                .pin(),
            short_pin.pin()
        );
        let expanded_deep_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "nested_archive_copy_archive {long_revision_commit}\n"
            ),
        );
        let expanded_deeper_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "nested_archive_copy_archive_copy {expanded_deep_commit}\n"
            ),
        );
        let expanded_deepest_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "nested_archive_copy_archive_copy_archive {expanded_deeper_commit}\n"
            ),
        );
        let expanded_deeper_deepest_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "nested_archive_copy_archive_copy_archive_copy {expanded_deepest_commit}\n"
            ),
        );
        let expanded_final_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "nested_archive_copy_archive_copy_archive_copy_archive {expanded_deeper_deepest_commit}\n"
            ),
        );
        let expanded_terminal_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "nested_archive_copy_archive_copy_archive_copy_archive_copy {expanded_final_commit}\n"
            ),
        );
        let expanded_tail_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive {expanded_terminal_commit}\n"
            ),
        );
        let expanded_next_tail_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy {expanded_tail_commit}\n"
            ),
        );
        let expanded_further_tail_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive {expanded_next_tail_commit}\n"
            ),
        );
        let expanded_terminal_tail_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy {expanded_further_tail_commit}\n"
            ),
        );
        let expanded_extreme_tail_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive {expanded_terminal_tail_commit}\n"
            ),
        );
        let expanded_terminal_extreme_tail_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy {expanded_extreme_tail_commit}\n"
            ),
        );
        let expanded_supreme_tail_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive {expanded_terminal_extreme_tail_commit}\n"
            ),
        );
        let expanded_ultimate_tail_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy {expanded_supreme_tail_commit}\n"
            ),
        );
        let expanded_penultimate_tail_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive {expanded_ultimate_tail_commit}\n"
            ),
        );
        let expanded_last_tail_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy {expanded_penultimate_tail_commit}\n"
            ),
        );
        let expanded_final_tail_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive {expanded_last_tail_commit}\n"
            ),
        );
        let expanded_terminal_final_tail_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy {expanded_final_tail_commit}\n"
            ),
        );
        let expanded_closure_tail_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive {expanded_terminal_final_tail_commit}\n"
            ),
        );
        let expanded_extended_closure_tail_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy {expanded_closure_tail_commit}\n"
            ),
        );
        let expanded_final_extended_closure_tail_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "{nested_archive_extended_closure_tail_alias} {expanded_extended_closure_tail_commit}\n"
            ),
        );
        let expanded_next_extended_closure_tail_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "{nested_archive_final_extended_closure_tail_alias} {expanded_final_extended_closure_tail_commit}\n"
            ),
        );
        let expanded_terminal_extended_closure_tail_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "{nested_archive_next_extended_closure_tail_alias} {expanded_next_extended_closure_tail_commit}\n"
            ),
        );
        let expanded_ultimate_extended_closure_tail_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "{nested_archive_terminal_extended_closure_tail_alias} {expanded_terminal_extended_closure_tail_commit}\n"
            ),
        );
        let expanded_tail_22_closure_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "{nested_archive_ultimate_extended_closure_tail_alias} {expanded_ultimate_extended_closure_tail_commit}\n"
            ),
        );
        let expanded_tail_23_closure_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "{nested_archive_tail_22_closure_alias} {expanded_tail_22_closure_commit}\n"
            ),
        );
        let expanded_tail_24_closure_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "{nested_archive_tail_23_closure_alias} {expanded_tail_23_closure_commit}\n"
            ),
        );
        let expanded_tail_25_closure_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "{nested_archive_tail_24_closure_alias} {expanded_tail_24_closure_commit}\n"
            ),
        );
        let expanded_tail_26_closure_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "{nested_archive_tail_25_closure_alias} {expanded_tail_25_closure_commit}\n"
            ),
        );
        let expanded_tail_27_closure_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "{nested_archive_tail_26_closure_alias} {expanded_tail_26_closure_commit}\n"
            ),
        );
        let expanded_tail_28_closure_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "{nested_archive_tail_27_closure_alias} {expanded_tail_27_closure_commit}\n"
            ),
        );
        let expanded_tail_29_closure_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "{nested_archive_tail_28_closure_alias} {expanded_tail_28_closure_commit}\n"
            ),
        );
        let expanded_short_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "nested_archive {short_revision_commit}\nnested_archive_copy {middle_revision_commit}\nnested_archive_copy_archive {long_revision_commit}\nnested_archive_copy_archive_copy {expanded_deep_commit}\nnested_archive_copy_archive_copy_archive {expanded_deeper_commit}\nnested_archive_copy_archive_copy_archive_copy {expanded_deepest_commit}\nnested_archive_copy_archive_copy_archive_copy_archive {expanded_deeper_deepest_commit}\nnested_archive_copy_archive_copy_archive_copy_archive_copy {expanded_final_commit}\nnested_archive_copy_archive_copy_archive_copy_archive_copy_archive {expanded_terminal_commit}\nnested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy {expanded_tail_commit}\nnested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive {expanded_next_tail_commit}\nnested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy {expanded_further_tail_commit}\nnested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive {expanded_terminal_tail_commit}\nnested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy {expanded_extreme_tail_commit}\nnested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive {expanded_terminal_extreme_tail_commit}\nnested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy {expanded_supreme_tail_commit}\nnested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive {expanded_ultimate_tail_commit}\nnested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy {expanded_penultimate_tail_commit}\nnested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive {expanded_last_tail_commit}\nnested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy {expanded_final_tail_commit}\nnested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive {expanded_terminal_final_tail_commit}\nnested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy {expanded_closure_tail_commit}\nnested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive {expanded_extended_closure_tail_commit}\n{nested_archive_final_extended_closure_tail_alias} {expanded_final_extended_closure_tail_commit}\n{nested_archive_next_extended_closure_tail_alias} {expanded_next_extended_closure_tail_commit}\n{nested_archive_terminal_extended_closure_tail_alias} {expanded_terminal_extended_closure_tail_commit}\n{nested_archive_ultimate_extended_closure_tail_alias} {expanded_ultimate_extended_closure_tail_commit}\n{nested_archive_tail_22_closure_alias} {expanded_tail_22_closure_commit}\n{nested_archive_tail_23_closure_alias} {expanded_tail_23_closure_commit}\n{nested_archive_tail_24_closure_alias} {expanded_tail_24_closure_commit}\n{nested_archive_tail_25_closure_alias} {expanded_tail_25_closure_commit}\n{nested_archive_tail_26_closure_alias} {expanded_tail_26_closure_commit}\n{nested_archive_tail_27_closure_alias} {expanded_tail_27_closure_commit}\n{nested_archive_tail_28_closure_alias} {expanded_tail_28_closure_commit}\n{nested_archive_tail_29_closure_alias} {expanded_tail_29_closure_commit}\n"
            ),
        );
        let expanded_short_replacement = PinnedDatabase::resolve(
            "archive",
            shared_repository.clone(),
            &expanded_short_commit,
            loader,
        )
        .unwrap();

        // The reference leaves replacement of a nested short pin with a
        // multi-alias historical closure unspecified. V1 keeps its child
        // aliases local while preserving equal short pins in sibling scopes.
        expanded_nested_child.detach_database("archive").unwrap();
        expanded_nested_child
            .attach_database(expanded_short_replacement.clone())
            .unwrap();
        assert_eq!(
            expanded_nested_child.primary().pin(),
            middle_pin.pin()
        );
        assert_eq!(
            expanded_nested_child
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            expanded_short_commit
        );
        assert_eq!(
            historical_sibling.database("archive").unwrap().pin(),
            short_pin.pin()
        );
        assert_eq!(
            nested_child_sibling
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            short_revision_commit
        );

        let expanded_short_session = resolver
            .resolve_for_parent(expanded_short_replacement)
            .unwrap();
        assert_eq!(expanded_short_session.primary().pin().name(), "archive");
        assert_eq!(
            expanded_short_session.primary().pin().commit().as_str(),
            expanded_short_commit
        );
        let nested_archive_pin = expanded_short_session
            .database("nested_archive")
            .unwrap()
            .pin();
        assert_eq!(nested_archive_pin.name(), "nested_archive");
        assert_eq!(nested_archive_pin.commit().as_str(), short_revision_commit);
        assert_ne!(nested_archive_pin, short_pin.pin());
        let nested_archive_copy_pin = expanded_short_session
            .database("nested_archive_copy")
            .unwrap()
            .pin();
        assert_eq!(nested_archive_copy_pin.name(), "nested_archive_copy");
        assert_eq!(
            nested_archive_copy_pin.commit().as_str(),
            middle_revision_commit
        );
        assert_ne!(nested_archive_copy_pin, middle_pin.pin());
        let expanded_short_child = resolver
            .resolve_for_parent(
                expanded_short_session
                    .database("nested_archive_copy")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            expanded_short_child
                .database("archive")
                .unwrap()
                .pin(),
            short_pin.pin()
        );
        let nested_archive_long_pin = expanded_short_session
            .database("nested_archive_copy_archive")
            .unwrap()
            .pin();
        assert_eq!(
            nested_archive_long_pin.name(),
            "nested_archive_copy_archive"
        );
        assert_eq!(
            nested_archive_long_pin.commit().as_str(),
            long_revision_commit
        );
        assert_ne!(nested_archive_long_pin, long_pin.pin());
        let expanded_long_child = resolver
            .resolve_for_parent(
                expanded_short_session
                    .database("nested_archive_copy_archive")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            expanded_long_child.primary().pin().name(),
            "nested_archive_copy_archive"
        );
        assert_eq!(
            expanded_long_child
                .database("archive_copy")
                .unwrap()
                .pin(),
            middle_pin.pin()
        );
        let expanded_long_grandchild = resolver
            .resolve_for_parent(
                expanded_long_child
                    .database("archive_copy")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            expanded_long_grandchild
                .database("archive")
                .unwrap()
                .pin(),
            short_pin.pin()
        );
        // The reference is silent on continuing an expanded alias route
        // through another closure edge. V1 follows each manifest alias while
        // retaining the pin names at every level.
        let nested_archive_deep_pin = expanded_short_session
            .database("nested_archive_copy_archive_copy")
            .unwrap()
            .pin();
        assert_eq!(
            nested_archive_deep_pin.name(),
            "nested_archive_copy_archive_copy"
        );
        assert_eq!(
            nested_archive_deep_pin.commit().as_str(),
            expanded_deep_commit
        );
        let expanded_deep_child = resolver
            .resolve_for_parent(
                expanded_short_session
                    .database("nested_archive_copy_archive_copy")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            expanded_deep_child.primary().pin().name(),
            "nested_archive_copy_archive_copy"
        );
        assert_eq!(
            expanded_deep_child
                .database("nested_archive_copy_archive")
                .unwrap()
                .pin(),
            nested_archive_long_pin
        );
        let expanded_deep_grandchild = resolver
            .resolve_for_parent(
                expanded_deep_child
                    .database("nested_archive_copy_archive")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            expanded_deep_grandchild
                .database("archive_copy")
                .unwrap()
                .pin(),
            middle_pin.pin()
        );
        let expanded_deep_great_grandchild = resolver
            .resolve_for_parent(
                expanded_deep_grandchild
                    .database("archive_copy")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            expanded_deep_great_grandchild
                .database("archive")
                .unwrap()
                .pin(),
            short_pin.pin()
        );
        // A further repeated suffix is unspecified too; v1 preserves it as
        // its own pin and carries the existing deeper closure through it.
        let nested_archive_deeper_pin = expanded_short_session
            .database("nested_archive_copy_archive_copy_archive")
            .unwrap()
            .pin();
        assert_eq!(
            nested_archive_deeper_pin.name(),
            "nested_archive_copy_archive_copy_archive"
        );
        assert_eq!(
            nested_archive_deeper_pin.commit().as_str(),
            expanded_deeper_commit
        );
        let expanded_deeper_child = resolver
            .resolve_for_parent(
                expanded_short_session
                    .database("nested_archive_copy_archive_copy_archive")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            expanded_deeper_child.primary().pin().name(),
            "nested_archive_copy_archive_copy_archive"
        );
        assert_eq!(
            expanded_deeper_child
                .database("nested_archive_copy_archive_copy")
                .unwrap()
                .pin(),
            expanded_deep_child.primary().pin()
        );
        // One more repeated suffix is also unspecified by the reference; v1
        // keeps its name distinct and connects it to the verified deep route.
        let nested_archive_deepest_pin = expanded_short_session
            .database("nested_archive_copy_archive_copy_archive_copy")
            .unwrap()
            .pin();
        assert_eq!(
            nested_archive_deepest_pin.name(),
            "nested_archive_copy_archive_copy_archive_copy"
        );
        assert_eq!(
            nested_archive_deepest_pin.commit().as_str(),
            expanded_deepest_commit
        );
        let expanded_deepest_child = resolver
            .resolve_for_parent(
                expanded_short_session
                    .database("nested_archive_copy_archive_copy_archive_copy")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            expanded_deepest_child.primary().pin().name(),
            "nested_archive_copy_archive_copy_archive_copy"
        );
        assert_eq!(
            expanded_deepest_child
                .database("nested_archive_copy_archive_copy_archive")
                .unwrap()
                .pin(),
            expanded_deeper_child.primary().pin()
        );
        // The reference does not define this further repeated suffix; v1
        // gives it a distinct name and retains the existing deep pin beneath it.
        let nested_archive_deeper_deepest_pin = expanded_short_session
            .database("nested_archive_copy_archive_copy_archive_copy_archive")
            .unwrap()
            .pin();
        assert_eq!(
            nested_archive_deeper_deepest_pin.name(),
            "nested_archive_copy_archive_copy_archive_copy_archive"
        );
        assert_eq!(
            nested_archive_deeper_deepest_pin.commit().as_str(),
            expanded_deeper_deepest_commit
        );
        let expanded_deeper_deepest_child = resolver
            .resolve_for_parent(
                expanded_short_session
                    .database("nested_archive_copy_archive_copy_archive_copy_archive")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            expanded_deeper_deepest_child.primary().pin().name(),
            "nested_archive_copy_archive_copy_archive_copy_archive"
        );
        assert_eq!(
            expanded_deeper_deepest_child
                .database("nested_archive_copy_archive_copy_archive_copy")
                .unwrap()
                .pin(),
            nested_archive_deepest_pin
        );
        // This additional repeated suffix is not specified by the reference;
        // v1 keeps its alias name and carries the verified closure edge below it.
        let nested_archive_final_pin = expanded_short_session
            .database("nested_archive_copy_archive_copy_archive_copy_archive_copy")
            .unwrap()
            .pin();
        assert_eq!(
            nested_archive_final_pin.name(),
            "nested_archive_copy_archive_copy_archive_copy_archive_copy"
        );
        assert_eq!(
            nested_archive_final_pin.commit().as_str(),
            expanded_final_commit
        );
        let expanded_final_child = resolver
            .resolve_for_parent(
                expanded_short_session
                    .database("nested_archive_copy_archive_copy_archive_copy_archive_copy")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            expanded_final_child.primary().pin().name(),
            "nested_archive_copy_archive_copy_archive_copy_archive_copy"
        );
        assert_eq!(
            expanded_final_child
                .database("nested_archive_copy_archive_copy_archive_copy_archive")
                .unwrap()
                .pin(),
            nested_archive_deeper_deepest_pin
        );
        // The reference is silent on another repeated closure edge; v1 keeps
        // the terminal alias distinct and preserves the previous pin beneath it.
        let nested_archive_terminal_pin = expanded_short_session
            .database("nested_archive_copy_archive_copy_archive_copy_archive_copy_archive")
            .unwrap()
            .pin();
        assert_eq!(
            nested_archive_terminal_pin.name(),
            "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive"
        );
        assert_eq!(
            nested_archive_terminal_pin.commit().as_str(),
            expanded_terminal_commit
        );
        let expanded_terminal_child = resolver
            .resolve_for_parent(
                expanded_short_session
                    .database("nested_archive_copy_archive_copy_archive_copy_archive_copy_archive")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            expanded_terminal_child.primary().pin().name(),
            "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive"
        );
        assert_eq!(
            expanded_terminal_child
                .database("nested_archive_copy_archive_copy_archive_copy_archive_copy")
                .unwrap()
                .pin(),
            nested_archive_final_pin
        );
        // Another repeated suffix is unspecified by the reference; v1 keeps
        // its alias distinct and links it to the prior terminal pin.
        let nested_archive_tail_pin = expanded_short_session
            .database("nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy")
            .unwrap()
            .pin();
        assert_eq!(
            nested_archive_tail_pin.name(),
            "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy"
        );
        assert_eq!(
            nested_archive_tail_pin.commit().as_str(),
            expanded_tail_commit
        );
        let expanded_tail_child = resolver
            .resolve_for_parent(
                expanded_short_session
                    .database("nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            expanded_tail_child.primary().pin().name(),
            "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy"
        );
        assert_eq!(
            expanded_tail_child
                .database("nested_archive_copy_archive_copy_archive_copy_archive_copy_archive")
                .unwrap()
                .pin(),
            nested_archive_terminal_pin
        );
        // The reference is silent on this additional repeated suffix; v1
        // keeps the alias distinct and retains the preceding terminal pin.
        let nested_archive_next_tail_pin = expanded_short_session
            .database("nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive")
            .unwrap()
            .pin();
        assert_eq!(
            nested_archive_next_tail_pin.name(),
            "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive"
        );
        assert_eq!(
            nested_archive_next_tail_pin.commit().as_str(),
            expanded_next_tail_commit
        );
        let expanded_next_tail_child = resolver
            .resolve_for_parent(
                expanded_short_session
                    .database("nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            expanded_next_tail_child.primary().pin().name(),
            "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive"
        );
        assert_eq!(
            expanded_next_tail_child
                .database("nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy")
                .unwrap()
                .pin(),
            nested_archive_tail_pin
        );
        // One more repeated suffix is unspecified by the reference; v1 keeps
        // its alias identity and follows the preceding closure edge.
        let nested_archive_further_tail_pin = expanded_short_session
            .database("nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy")
            .unwrap()
            .pin();
        assert_eq!(
            nested_archive_further_tail_pin.name(),
            "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy"
        );
        assert_eq!(
            nested_archive_further_tail_pin.commit().as_str(),
            expanded_further_tail_commit
        );
        let expanded_further_tail_child = resolver
            .resolve_for_parent(
                expanded_short_session
                    .database("nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            expanded_further_tail_child.primary().pin().name(),
            "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy"
        );
        assert_eq!(
            expanded_further_tail_child
                .database("nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive")
                .unwrap()
                .pin(),
            nested_archive_next_tail_pin
        );
        // The reference is silent on this next repeated suffix; v1 retains
        // its distinct alias and follows the prior terminal pin.
        let nested_archive_terminal_tail_pin = expanded_short_session
            .database("nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive")
            .unwrap()
            .pin();
        assert_eq!(
            nested_archive_terminal_tail_pin.name(),
            "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive"
        );
        assert_eq!(
            nested_archive_terminal_tail_pin.commit().as_str(),
            expanded_terminal_tail_commit
        );
        let expanded_terminal_tail_child = resolver
            .resolve_for_parent(
                expanded_short_session
                    .database("nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            expanded_terminal_tail_child.primary().pin().name(),
            "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive"
        );
        assert_eq!(
            expanded_terminal_tail_child
                .database("nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy")
                .unwrap()
                .pin(),
            nested_archive_further_tail_pin
        );
        // The reference does not cover this repeated suffix; v1 retains its
        // alias identity and connects it to the prior terminal closure pin.
        let nested_archive_extreme_tail_pin = expanded_short_session
            .database("nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy")
            .unwrap()
            .pin();
        assert_eq!(
            nested_archive_extreme_tail_pin.name(),
            "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy"
        );
        assert_eq!(
            nested_archive_extreme_tail_pin.commit().as_str(),
            expanded_extreme_tail_commit
        );
        let expanded_extreme_tail_child = resolver
            .resolve_for_parent(
                expanded_short_session
                    .database("nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            expanded_extreme_tail_child.primary().pin().name(),
            "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy"
        );
        assert_eq!(
            expanded_extreme_tail_child
                .database("nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive")
                .unwrap()
                .pin(),
            nested_archive_terminal_tail_pin
        );
        // This further repeated suffix is unspecified by the reference; v1
        // preserves its alias identity and links it to the prior terminal pin.
        let nested_archive_terminal_extreme_tail_pin = expanded_short_session
            .database("nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive")
            .unwrap()
            .pin();
        assert_eq!(
            nested_archive_terminal_extreme_tail_pin.name(),
            "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive"
        );
        assert_eq!(
            nested_archive_terminal_extreme_tail_pin.commit().as_str(),
            expanded_terminal_extreme_tail_commit
        );
        let expanded_terminal_extreme_tail_child = resolver
            .resolve_for_parent(
                expanded_short_session
                    .database("nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            expanded_terminal_extreme_tail_child.primary().pin().name(),
            "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive"
        );
        assert_eq!(
            expanded_terminal_extreme_tail_child
                .database("nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy")
                .unwrap()
                .pin(),
            nested_archive_extreme_tail_pin
        );
        // This next repeated suffix is unspecified by the reference; v1 keeps
        // its identity distinct and links it to the prior terminal alias.
        let nested_archive_supreme_tail_pin = expanded_short_session
            .database("nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy")
            .unwrap()
            .pin();
        assert_eq!(
            nested_archive_supreme_tail_pin.name(),
            "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy"
        );
        assert_eq!(
            nested_archive_supreme_tail_pin.commit().as_str(),
            expanded_supreme_tail_commit
        );
        let expanded_supreme_tail_child = resolver
            .resolve_for_parent(
                expanded_short_session
                    .database("nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            expanded_supreme_tail_child.primary().pin().name(),
            "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy"
        );
        assert_eq!(
            expanded_supreme_tail_child
                .database("nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive")
                .unwrap()
                .pin(),
            nested_archive_terminal_extreme_tail_pin
        );
        // This next repeated suffix is not defined by the reference; v1
        // preserves its distinct alias and follows the prior terminal pin.
        let nested_archive_ultimate_tail_pin = expanded_short_session
            .database("nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive")
            .unwrap()
            .pin();
        assert_eq!(
            nested_archive_ultimate_tail_pin.name(),
            "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive"
        );
        assert_eq!(
            nested_archive_ultimate_tail_pin.commit().as_str(),
            expanded_ultimate_tail_commit
        );
        let expanded_ultimate_tail_child = resolver
            .resolve_for_parent(
                expanded_short_session
                    .database("nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            expanded_ultimate_tail_child.primary().pin().name(),
            "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive"
        );
        assert_eq!(
            expanded_ultimate_tail_child
                .database("nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy")
                .unwrap()
                .pin(),
            nested_archive_supreme_tail_pin
        );
        // The reference is silent on this further repeated suffix; v1 keeps
        // the alias distinct and links it to the previous terminal pin.
        let nested_archive_penultimate_tail_pin = expanded_short_session
            .database("nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy")
            .unwrap()
            .pin();
        assert_eq!(
            nested_archive_penultimate_tail_pin.name(),
            "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy"
        );
        assert_eq!(
            nested_archive_penultimate_tail_pin.commit().as_str(),
            expanded_penultimate_tail_commit
        );
        let expanded_penultimate_tail_child = resolver
            .resolve_for_parent(
                expanded_short_session
                    .database("nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            expanded_penultimate_tail_child.primary().pin().name(),
            "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy"
        );
        assert_eq!(
            expanded_penultimate_tail_child
                .database("nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive")
                .unwrap()
                .pin(),
            nested_archive_ultimate_tail_pin
        );
        // The reference does not define this repeated suffix; v1 preserves
        // its alias identity and links it to the preceding terminal pin.
        let nested_archive_last_tail_pin = expanded_short_session
            .database("nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive")
            .unwrap()
            .pin();
        assert_eq!(
            nested_archive_last_tail_pin.name(),
            "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive"
        );
        assert_eq!(
            nested_archive_last_tail_pin.commit().as_str(),
            expanded_last_tail_commit
        );
        let expanded_last_tail_child = resolver
            .resolve_for_parent(
                expanded_short_session
                    .database("nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            expanded_last_tail_child.primary().pin().name(),
            "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive"
        );
        assert_eq!(
            expanded_last_tail_child
                .database("nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy")
                .unwrap()
                .pin(),
            nested_archive_penultimate_tail_pin
        );
        // The reference is silent on another repeated suffix; v1 retains its
        // distinct alias identity and points it to the previous terminal pin.
        let nested_archive_final_tail_pin = expanded_short_session
            .database("nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy")
            .unwrap()
            .pin();
        assert_eq!(
            nested_archive_final_tail_pin.name(),
            "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy"
        );
        assert_eq!(
            nested_archive_final_tail_pin.commit().as_str(),
            expanded_final_tail_commit
        );
        let expanded_final_tail_child = resolver
            .resolve_for_parent(
                expanded_short_session
                    .database("nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            expanded_final_tail_child.primary().pin().name(),
            "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy"
        );
        assert_eq!(
            expanded_final_tail_child
                .database("nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive")
                .unwrap()
                .pin(),
            nested_archive_last_tail_pin
        );
        // The reference is silent on this repeated suffix; v1 preserves the
        // distinct alias and links it to the previous terminal pin.
        let nested_archive_terminal_final_tail_pin = expanded_short_session
            .database("nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive")
            .unwrap()
            .pin();
        assert_eq!(
            nested_archive_terminal_final_tail_pin.name(),
            "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive"
        );
        assert_eq!(
            nested_archive_terminal_final_tail_pin.commit().as_str(),
            expanded_terminal_final_tail_commit
        );
        let expanded_terminal_final_tail_child = resolver
            .resolve_for_parent(
                expanded_short_session
                    .database("nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            expanded_terminal_final_tail_child.primary().pin().name(),
            "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive"
        );
        assert_eq!(
            expanded_terminal_final_tail_child
                .database("nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy")
                .unwrap()
                .pin(),
            nested_archive_final_tail_pin
        );
        // The reference is silent on this additional suffix; v1 keeps the
        // new alias distinct and links it to the preceding terminal pin.
        let nested_archive_closure_tail_pin = expanded_short_session
            .database("nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy")
            .unwrap()
            .pin();
        assert_eq!(
            nested_archive_closure_tail_pin.name(),
            "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy"
        );
        assert_eq!(
            nested_archive_closure_tail_pin.commit().as_str(),
            expanded_closure_tail_commit
        );
        let expanded_closure_tail_child = resolver
            .resolve_for_parent(
                expanded_short_session
                    .database("nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            expanded_closure_tail_child.primary().pin().name(),
            "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy"
        );
        assert_eq!(
            expanded_closure_tail_child
                .database("nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive")
                .unwrap()
                .pin(),
            nested_archive_terminal_final_tail_pin
        );
        // The reference is silent on this further suffix; v1 retains its
        // alias identity and links it to the previous terminal pin.
        let nested_archive_extended_closure_tail_pin = expanded_short_session
            .database("nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive")
            .unwrap()
            .pin();
        assert_eq!(
            nested_archive_extended_closure_tail_pin.name(),
            "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive"
        );
        assert_eq!(
            nested_archive_extended_closure_tail_pin.commit().as_str(),
            expanded_extended_closure_tail_commit
        );
        let expanded_extended_closure_tail_child = resolver
            .resolve_for_parent(
                expanded_short_session
                    .database("nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            expanded_extended_closure_tail_child.primary().pin().name(),
            "nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive"
        );
        assert_eq!(
            expanded_extended_closure_tail_child
                .database("nested_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy_archive_copy")
                .unwrap()
                .pin(),
            nested_archive_closure_tail_pin
        );
        // The reference is silent on this further suffix; v1 retains its
        // alias identity and links it to the previous terminal pin.
        let nested_archive_final_extended_closure_tail_pin = expanded_short_session
            .database(&nested_archive_final_extended_closure_tail_alias)
            .unwrap()
            .pin();
        assert_eq!(
            nested_archive_final_extended_closure_tail_pin.name(),
            nested_archive_final_extended_closure_tail_alias.as_str()
        );
        assert_eq!(
            nested_archive_final_extended_closure_tail_pin.commit().as_str(),
            expanded_final_extended_closure_tail_commit
        );
        let expanded_final_extended_closure_tail_child = resolver
            .resolve_for_parent(
                expanded_short_session
                    .database(&nested_archive_final_extended_closure_tail_alias)
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            expanded_final_extended_closure_tail_child.primary().pin().name(),
            nested_archive_final_extended_closure_tail_alias.as_str()
        );
        assert_eq!(
            expanded_final_extended_closure_tail_child
                .database(&nested_archive_extended_closure_tail_alias)
                .unwrap()
                .pin(),
            nested_archive_extended_closure_tail_pin
        );
        // The reference is silent on one more suffix; v1 preserves its
        // distinct pin and links the child to the previous terminal pin.
        let nested_archive_next_extended_closure_tail_pin = expanded_short_session
            .database(&nested_archive_next_extended_closure_tail_alias)
            .unwrap()
            .pin();
        assert_eq!(
            nested_archive_next_extended_closure_tail_pin.name(),
            nested_archive_next_extended_closure_tail_alias.as_str()
        );
        assert_eq!(
            nested_archive_next_extended_closure_tail_pin.commit().as_str(),
            expanded_next_extended_closure_tail_commit
        );
        let expanded_next_extended_closure_tail_child = resolver
            .resolve_for_parent(
                expanded_short_session
                    .database(&nested_archive_next_extended_closure_tail_alias)
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            expanded_next_extended_closure_tail_child.primary().pin().name(),
            nested_archive_next_extended_closure_tail_alias.as_str()
        );
        assert_eq!(
            expanded_next_extended_closure_tail_child
                .database(&nested_archive_final_extended_closure_tail_alias)
                .unwrap()
                .pin(),
            nested_archive_final_extended_closure_tail_pin
        );
        // The reference is silent on this next suffix; v1 keeps its
        // alias distinct and closes through the previous terminal pin.
        let nested_archive_terminal_extended_closure_tail_pin = expanded_short_session
            .database(&nested_archive_terminal_extended_closure_tail_alias)
            .unwrap()
            .pin();
        assert_eq!(
            nested_archive_terminal_extended_closure_tail_pin.name(),
            nested_archive_terminal_extended_closure_tail_alias.as_str()
        );
        assert_eq!(
            nested_archive_terminal_extended_closure_tail_pin.commit().as_str(),
            expanded_terminal_extended_closure_tail_commit
        );
        let expanded_terminal_extended_closure_tail_child = resolver
            .resolve_for_parent(
                expanded_short_session
                    .database(&nested_archive_terminal_extended_closure_tail_alias)
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            expanded_terminal_extended_closure_tail_child.primary().pin().name(),
            nested_archive_terminal_extended_closure_tail_alias.as_str()
        );
        assert_eq!(
            expanded_terminal_extended_closure_tail_child
                .database(&nested_archive_next_extended_closure_tail_alias)
                .unwrap()
                .pin(),
            nested_archive_next_extended_closure_tail_pin
        );
        // The reference is silent on this further suffix; v1 keeps its
        // alias distinct and closes through the previous terminal pin.
        let nested_archive_ultimate_extended_closure_tail_pin = expanded_short_session
            .database(&nested_archive_ultimate_extended_closure_tail_alias)
            .unwrap()
            .pin();
        assert_eq!(
            nested_archive_ultimate_extended_closure_tail_pin.name(),
            nested_archive_ultimate_extended_closure_tail_alias.as_str()
        );
        assert_eq!(
            nested_archive_ultimate_extended_closure_tail_pin.commit().as_str(),
            expanded_ultimate_extended_closure_tail_commit
        );
        let expanded_ultimate_extended_closure_tail_child = resolver
            .resolve_for_parent(
                expanded_short_session
                    .database(&nested_archive_ultimate_extended_closure_tail_alias)
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            expanded_ultimate_extended_closure_tail_child.primary().pin().name(),
            nested_archive_ultimate_extended_closure_tail_alias.as_str()
        );
        assert_eq!(
            expanded_ultimate_extended_closure_tail_child
                .database(&nested_archive_terminal_extended_closure_tail_alias)
                .unwrap()
                .pin(),
            nested_archive_terminal_extended_closure_tail_pin
        );
        // The reference is silent on this twenty-second suffix; v1
        // preserves its alias identity and previous-terminal link.
        let nested_archive_tail_22_closure_pin = expanded_short_session
            .database(&nested_archive_tail_22_closure_alias)
            .unwrap()
            .pin();
        assert_eq!(
            nested_archive_tail_22_closure_pin.name(),
            nested_archive_tail_22_closure_alias.as_str()
        );
        assert_eq!(
            nested_archive_tail_22_closure_pin.commit().as_str(),
            expanded_tail_22_closure_commit
        );
        let expanded_tail_22_closure_child = resolver
            .resolve_for_parent(
                expanded_short_session
                    .database(&nested_archive_tail_22_closure_alias)
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            expanded_tail_22_closure_child.primary().pin().name(),
            nested_archive_tail_22_closure_alias.as_str()
        );
        assert_eq!(
            expanded_tail_22_closure_child
                .database(&nested_archive_ultimate_extended_closure_tail_alias)
                .unwrap()
                .pin(),
            nested_archive_ultimate_extended_closure_tail_pin
        );
        // The reference is silent on this twenty-third suffix; v1
        // preserves its alias identity and previous-terminal link.
        let nested_archive_tail_23_closure_pin = expanded_short_session
            .database(&nested_archive_tail_23_closure_alias)
            .unwrap()
            .pin();
        assert_eq!(
            nested_archive_tail_23_closure_pin.name(),
            nested_archive_tail_23_closure_alias.as_str()
        );
        assert_eq!(
            nested_archive_tail_23_closure_pin.commit().as_str(),
            expanded_tail_23_closure_commit
        );
        let expanded_tail_23_closure_child = resolver
            .resolve_for_parent(
                expanded_short_session
                    .database(&nested_archive_tail_23_closure_alias)
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            expanded_tail_23_closure_child.primary().pin().name(),
            nested_archive_tail_23_closure_alias.as_str()
        );
        assert_eq!(
            expanded_tail_23_closure_child
                .database(&nested_archive_tail_22_closure_alias)
                .unwrap()
                .pin(),
            nested_archive_tail_22_closure_pin
        );
        // The reference is silent on the twenty-fourth suffix; v1
        // preserves its alias identity and previous-terminal link.
        let nested_archive_tail_24_closure_pin = expanded_short_session
            .database(&nested_archive_tail_24_closure_alias)
            .unwrap()
            .pin();
        assert_eq!(
            nested_archive_tail_24_closure_pin.name(),
            nested_archive_tail_24_closure_alias.as_str()
        );
        assert_eq!(
            nested_archive_tail_24_closure_pin.commit().as_str(),
            expanded_tail_24_closure_commit
        );
        let expanded_tail_24_closure_child = resolver
            .resolve_for_parent(
                expanded_short_session
                    .database(&nested_archive_tail_24_closure_alias)
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            expanded_tail_24_closure_child.primary().pin().name(),
            nested_archive_tail_24_closure_alias.as_str()
        );
        assert_eq!(
            expanded_tail_24_closure_child
                .database(&nested_archive_tail_23_closure_alias)
                .unwrap()
                .pin(),
            nested_archive_tail_23_closure_pin
        );
        // The reference is silent on the twenty-fifth suffix; v1
        // preserves its alias identity and previous-terminal link.
        let nested_archive_tail_25_closure_pin = expanded_short_session
            .database(&nested_archive_tail_25_closure_alias)
            .unwrap()
            .pin();
        assert_eq!(
            nested_archive_tail_25_closure_pin.name(),
            nested_archive_tail_25_closure_alias.as_str()
        );
        assert_eq!(
            nested_archive_tail_25_closure_pin.commit().as_str(),
            expanded_tail_25_closure_commit
        );
        let expanded_tail_25_closure_child = resolver
            .resolve_for_parent(
                expanded_short_session
                    .database(&nested_archive_tail_25_closure_alias)
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            expanded_tail_25_closure_child.primary().pin().name(),
            nested_archive_tail_25_closure_alias.as_str()
        );
        assert_eq!(
            expanded_tail_25_closure_child
                .database(&nested_archive_tail_24_closure_alias)
                .unwrap()
                .pin(),
            nested_archive_tail_24_closure_pin
        );
        // The reference is silent on the twenty-sixth suffix; v1
        // preserves its alias identity and previous-terminal link.
        let nested_archive_tail_26_closure_pin = expanded_short_session
            .database(&nested_archive_tail_26_closure_alias)
            .unwrap()
            .pin();
        assert_eq!(
            nested_archive_tail_26_closure_pin.name(),
            nested_archive_tail_26_closure_alias.as_str()
        );
        assert_eq!(
            nested_archive_tail_26_closure_pin.commit().as_str(),
            expanded_tail_26_closure_commit
        );
        let expanded_tail_26_closure_child = resolver
            .resolve_for_parent(
                expanded_short_session
                    .database(&nested_archive_tail_26_closure_alias)
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            expanded_tail_26_closure_child.primary().pin().name(),
            nested_archive_tail_26_closure_alias.as_str()
        );
        assert_eq!(
            expanded_tail_26_closure_child
                .database(&nested_archive_tail_25_closure_alias)
                .unwrap()
                .pin(),
            nested_archive_tail_25_closure_pin
        );
        // The reference is silent on the twenty-seventh suffix; v1
        // preserves its alias identity and previous-terminal link.
        let nested_archive_tail_27_closure_pin = expanded_short_session
            .database(&nested_archive_tail_27_closure_alias)
            .unwrap()
            .pin();
        assert_eq!(
            nested_archive_tail_27_closure_pin.name(),
            nested_archive_tail_27_closure_alias.as_str()
        );
        assert_eq!(
            nested_archive_tail_27_closure_pin.commit().as_str(),
            expanded_tail_27_closure_commit
        );
        let expanded_tail_27_closure_child = resolver
            .resolve_for_parent(
                expanded_short_session
                    .database(&nested_archive_tail_27_closure_alias)
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            expanded_tail_27_closure_child.primary().pin().name(),
            nested_archive_tail_27_closure_alias.as_str()
        );
        assert_eq!(
            expanded_tail_27_closure_child
                .database(&nested_archive_tail_26_closure_alias)
                .unwrap()
                .pin(),
            nested_archive_tail_26_closure_pin
        );
        // The reference is silent on the twenty-eighth suffix; v1
        // preserves its alias identity and previous-terminal link.
        let nested_archive_tail_28_closure_pin = expanded_short_session
            .database(&nested_archive_tail_28_closure_alias)
            .unwrap()
            .pin();
        assert_eq!(
            nested_archive_tail_28_closure_pin.name(),
            nested_archive_tail_28_closure_alias.as_str()
        );
        assert_eq!(
            nested_archive_tail_28_closure_pin.commit().as_str(),
            expanded_tail_28_closure_commit
        );
        let expanded_tail_28_closure_child = resolver
            .resolve_for_parent(
                expanded_short_session
                    .database(&nested_archive_tail_28_closure_alias)
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            expanded_tail_28_closure_child.primary().pin().name(),
            nested_archive_tail_28_closure_alias.as_str()
        );
        assert_eq!(
            expanded_tail_28_closure_child
                .database(&nested_archive_tail_27_closure_alias)
                .unwrap()
                .pin(),
            nested_archive_tail_27_closure_pin
        );
        // The reference is silent on the twenty-ninth suffix; v1
        // preserves its alias identity and previous-terminal link.
        let nested_archive_tail_29_closure_pin = expanded_short_session
            .database(&nested_archive_tail_29_closure_alias)
            .unwrap()
            .pin();
        assert_eq!(
            nested_archive_tail_29_closure_pin.name(),
            nested_archive_tail_29_closure_alias.as_str()
        );
        assert_eq!(
            nested_archive_tail_29_closure_pin.commit().as_str(),
            expanded_tail_29_closure_commit
        );
        let expanded_tail_29_closure_child = resolver
            .resolve_for_parent(
                expanded_short_session
                    .database(&nested_archive_tail_29_closure_alias)
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            expanded_tail_29_closure_child.primary().pin().name(),
            nested_archive_tail_29_closure_alias.as_str()
        );
        assert_eq!(
            expanded_tail_29_closure_child
                .database(&nested_archive_tail_28_closure_alias)
                .unwrap()
                .pin(),
            nested_archive_tail_28_closure_pin
        );
        assert_eq!(
            root_session
                .database("archive_copy_archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            current_commit
        );
    }

    #[test]
    fn equivalent_units_interoperate_across_attachments_but_name_match_is_not_enough() {
        let primary_source = include_str!("fixtures/attached-primary-main.orna");
        let (_primary_dir, primary_repository, primary_commit) = repository(&primary_source);
        let (_same_dir, same_repository, same_commit) = repository(
            &include_str!("fixtures/attached-package-main.orna")
                .replace("unit meter:", "pub unit meter :")
                .replace("base;", "base ;"),
        );
        let (_different_dir, different_repository, different_commit) = repository(
            &include_str!("fixtures/attached-package-main.orna")
                .replace("meter: Length", "meter: Time"),
        );
        let loader = ProjectLoader::default();
        let primary = PinnedDatabase::resolve(
            "app",
            primary_repository,
            &primary_commit,
            loader,
        )
        .unwrap();
        let same = PinnedDatabase::resolve(
            "measurements",
            same_repository,
            &same_commit,
            loader,
        )
        .unwrap();
        let different = PinnedDatabase::resolve(
            "archive",
            different_repository,
            &different_commit,
            loader,
        )
        .unwrap();
        let mut session = AttachedDatabaseSession::new(primary).unwrap();
        session.attach_database(same).unwrap();
        session.attach_database(different).unwrap();

        assert!(session.units_structurally_equivalent(
            "app",
            "meter",
            "measurements",
            "meter"
        ));
        assert!(!session.units_structurally_equivalent(
            "app",
            "meter",
            "archive",
            "meter"
        ));
    }

    #[test]
    fn package_manifest_rejects_moving_selectors_and_duplicate_names() {
        let oid = "a".repeat(40);
        assert!(PackagePinManifest::parse(&format!("math {oid}\n")).is_ok());
        assert!(PackagePinManifest::parse("math HEAD\n").is_err());
        assert!(PackagePinManifest::parse(&format!("math {oid}\nmath {oid}\n")).is_err());
        assert!(PackagePinManifest::parse(&format!("sys {oid}\n")).is_err());
    }

    #[test]
    fn std_package_pin_must_match_the_gitlink_when_both_are_committed() {
        let (std_directory, std_repository, first_std_commit) =
            repository(include_str!("fixtures/attached-package-main.orna"));
        let second_std_source = include_str!("fixtures/attached-package-main.orna").replace("42", "43");
        let second_std_commit = write_commit(
            std_directory.path(),
            "main.orna",
            &second_std_source,
        );

        let primary_directory = tempfile::tempdir().unwrap();
        git(primary_directory.path(), &["init", "--quiet"]);
        git(primary_directory.path(), &["config", "user.name", "kierandrewett"]);
        git(
            primary_directory.path(),
            &["config", "user.email", "kieran@drewett.dev"],
        );
        git(primary_directory.path(), &["config", "commit.gpgsign", "false"]);
        fs::write(
            primary_directory.path().join("main.orna"),
            include_str!("fixtures/attached-primary-main.orna"),
        )
        .unwrap();
        fs::create_dir_all(primary_directory.path().join(".orna")).unwrap();
        fs::write(
            primary_directory.path().join(PACKAGE_PIN_MANIFEST_PATH),
            format!("std {first_std_commit}\n"),
        )
        .unwrap();
        git(primary_directory.path(), &["add", "main.orna", ".orna/packages"]);
        let link = format!("160000,{second_std_commit},stdlib/std");
        git(
            primary_directory.path(),
            &["update-index", "--add", "--cacheinfo", &link],
        );
        git(
            primary_directory.path(),
            &["commit", "--quiet", "-m", "conflicting std pins"],
        );
        let primary_repository = Repository::discover(primary_directory.path()).unwrap();
        let parent = git(primary_directory.path(), &["rev-parse", "HEAD"]);
        let loader = ProjectLoader::default();
        let primary = PinnedDatabase::resolve(
            "app",
            primary_repository.clone(),
            &parent,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [("std".to_owned(), std_repository.clone())],
            loader,
        )
        .unwrap();
        assert!(matches!(
            resolver.resolve_for_parent(primary),
            Err(AttachmentError::PinUnavailable)
        ));

        write_commit(
            primary_directory.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("std {second_std_commit}\n"),
        );
        let matching_parent = git(primary_directory.path(), &["rev-parse", "HEAD"]);
        let primary = PinnedDatabase::resolve(
            "app",
            primary_repository,
            &matching_parent,
            loader,
        )
        .unwrap();
        let session = resolver.resolve_for_parent(primary).unwrap();
        assert_eq!(
            session.standard_snapshot().unwrap().as_str(),
            second_std_commit
        );
    }

    #[test]
    fn relation_sources_compose_rows_from_each_pinned_snapshot() {
        fn committed_database(main: &str, row: &str) -> (TempDir, Repository, String) {
            let directory = tempfile::tempdir().unwrap();
            git(directory.path(), &["init", "--quiet"]);
            git(directory.path(), &["config", "user.name", "kierandrewett"]);
            git(
                directory.path(),
                &["config", "user.email", "kieran@drewett.dev"],
            );
            git(directory.path(), &["config", "commit.gpgsign", "false"]);
            write_commit(directory.path(), "main.orna", main);
            write_commit(
                directory.path(),
                "contacts.orna",
                include_str!("fixtures/attached-read-table.orna"),
            );
            let commit = write_commit(directory.path(), "contacts/Contact/1.orna", row);
            let repository = Repository::discover(directory.path()).unwrap();
            (directory, repository, commit)
        }

        let (_primary_dir, primary_repository, primary_commit) = committed_database(
            include_str!("fixtures/attached-read-primary-main.orna"),
            include_str!("fixtures/attached-read-primary-row.orna"),
        );
        let (_archive_dir, archive_repository, archive_commit) = committed_database(
            include_str!("fixtures/attached-read-package-main.orna"),
            include_str!("fixtures/attached-read-package-row.orna"),
        );
        let loader = ProjectLoader::default();
        let primary = PinnedDatabase::resolve(
            "app",
            primary_repository,
            &primary_commit,
            loader,
        )
        .unwrap();
        let archive = PinnedDatabase::resolve(
            "archive",
            archive_repository,
            &archive_commit,
            loader,
        )
        .unwrap();
        let mut session = AttachedDatabaseSession::new(primary).unwrap();
        session.attach_database(archive).unwrap();

        let sources = session.relation_sources("contacts/Contact");
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0].database(), "app");
        assert_eq!(sources[0].commit().as_str(), primary_commit);
        assert!(sources[0].row().source().contains("value: 7"));
        assert_eq!(sources[1].database(), "archive");
        assert_eq!(sources[1].commit().as_str(), archive_commit);
        assert!(sources[1].row().source().contains("value: 42"));
    }
}
