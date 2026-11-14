//! The host a run executes against: everything a program cannot compute for itself.
//!
//! `docs/design/hir-host.md` section A1 defines the trait. A run reads its tables, its clock, its entropy and its
//! `sys` members through one [`Host`]. The program never sees the host; it sees the canonical values the host
//! returns. [`HostManifest`] is the list of `sys` members a host serves, and it is also the admission lowering reads
//! (section A3): a member is admitted iff the manifest registers it.

use std::collections::HashMap;

use orna_hir::{SysAdmission, SysMemberStatus};
use orna_value::{Raw, Snapshot};

use crate::store::{MemoryStore, TableStore};

#[path = "host/catalog.rs"]
mod catalog;
pub use catalog::{HostInfo, HostRuntimeMode, ProgramCatalog};

#[path = "host/runtime_rows.rs"]
mod runtime_rows;
pub use runtime_rows::RuntimeHostRows;

/// The code a host answers for a member, or a capability, it does not provide.
pub(crate) const UNSUPPORTED: &str = "ORNA-EVAL-UNSUPPORTED";

/// The code a host answers for an `as_of` snapshot it does not hold.
pub(crate) const SNAPSHOT_NOT_FOUND: &str = "sys.snapshot.not_found";

/// A host's failure to serve one request. `code` is a canonical code; the machine maps it to a language error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostFailure {
    /// The canonical code the request failed with.
    pub code: &'static str,
}

/// A point in time: whole unix seconds and the nanoseconds past them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct HostInstant {
    /// Whole seconds since the unix epoch.
    pub unix_seconds: i64,
    /// Nanoseconds past `unix_seconds`, below one billion.
    pub nanos: u32,
}

impl HostInstant {
    /// The unix epoch.
    pub const EPOCH: Self = Self {
        unix_seconds: 0,
        nanos: 0,
    };
}

/// The slot a host gives one registered `sys` member. The machine binds each member of the program to its slot once,
/// so a call or a read carries the slot and never a name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SysSlot(pub u32);

/// The source of canonical rows one `sys` relation read produces, one row at a time.
pub trait RowSource {
    /// The next row, or `None` once the relation is exhausted.
    ///
    /// # Errors
    ///
    /// The host's failure while it produces the row.
    fn next(&mut self) -> Result<Option<Raw>, HostFailure>;
}

/// The `sys` members one host serves, by qualified name (`sys.Storage`, `sys.catalog.tables`, ...).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostManifest {
    names: Vec<String>,
    slots: HashMap<String, SysSlot>,
}

impl HostManifest {
    /// A manifest that serves no `sys` member.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers the member `name` and returns its slot. Registering a name twice returns the first slot.
    pub fn register(&mut self, name: &str) -> SysSlot {
        if let Some(slot) = self.slots.get(name) {
            return *slot;
        }
        let slot = SysSlot(
            u32::try_from(self.names.len()).expect("a host registers fewer than 2^32 sys members"),
        );
        self.names.push(name.to_owned());
        self.slots.insert(name.to_owned(), slot);
        slot
    }

    /// The slot of the registered member `name`, if the host serves it.
    #[must_use]
    pub fn slot(&self, name: &str) -> Option<SysSlot> {
        self.slots.get(name).copied()
    }

    /// The qualified name the host registered for `slot`.
    #[must_use]
    pub fn name(&self, slot: SysSlot) -> Option<&str> {
        self.names.get(slot.0 as usize).map(String::as_str)
    }

    /// The number of members the host serves.
    #[must_use]
    pub fn len(&self) -> usize {
        self.names.len()
    }

    /// Whether the host serves no member.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    /// Every registered qualified name, in registration order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.names.iter().map(String::as_str)
    }
}

/// Lowering admits a `sys` path iff the manifest registers its qualified name (section A3). A path the manifest does
/// not register is `Unlinked`, including a path the catalogue does not name; a host that needs the three-way answer
/// keeps its own admission.
impl SysAdmission for HostManifest {
    fn status(&self, path: &[&str]) -> SysMemberStatus {
        if self.slots.contains_key(&path.join(".")) {
            SysMemberStatus::Linked
        } else {
            SysMemberStatus::Unlinked
        }
    }
}

/// The snapshots a run is pinned to. `cwd` is the working state the activation reads, and `head` the head commit when
/// the repository has one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivationContext {
    /// The working-state snapshot of the activation.
    pub cwd: Snapshot,
    /// The head snapshot, `None` when the repository has no commit.
    pub head: Option<Snapshot>,
    /// The locale profile `format` renders money in (ORNA-MONEY-009), a BCP 47 tag such as `en-GB`.
    pub locale: String,
}

/// Everything a run takes from outside the program (`docs/design/hir-host.md` section A1).
pub trait Host {
    /// The `sys` members this host serves.
    fn manifest(&self) -> &HostManifest;

    /// The current table store. Writes reach it only inside an activation.
    fn store(&mut self) -> &mut dyn TableStore;

    /// A read-only table store as of `at`.
    ///
    /// # Errors
    ///
    /// [`HostFailure`] with `sys.snapshot.not_found` when the host holds no such snapshot.
    fn store_at(&mut self, at: &Snapshot) -> Result<Box<dyn TableStore + '_>, HostFailure>;

    /// The instant of the activation. It is the same for every read within one activation.
    fn now(&self) -> HostInstant;

    /// Fills `out` with entropy.
    ///
    /// # Errors
    ///
    /// [`HostFailure`] when the host supplies no entropy.
    fn entropy(&mut self, out: &mut [u8]) -> Result<(), HostFailure>;

    /// The snapshots this activation is pinned to, when the host has any.
    fn activation(&self) -> Option<&ActivationContext>;

    /// Calls the `sys` callable in `slot` with canonical arguments.
    ///
    /// # Errors
    ///
    /// [`HostFailure`] with the canonical code of the failure.
    fn sys_call(&mut self, slot: SysSlot, args: Vec<Raw>) -> Result<Raw, HostFailure>;

    /// The value of the `sys` static (a singleton such as `sys.rt`) in `slot`.
    ///
    /// # Errors
    ///
    /// [`HostFailure`] with the canonical code of the failure.
    fn sys_static(&mut self, slot: SysSlot) -> Result<Raw, HostFailure>;

    /// A cursor over the rows of the `sys` relation in `slot`, as of `at` when given.
    ///
    /// # Errors
    ///
    /// [`HostFailure`] with the canonical code of the failure.
    fn sys_relation(
        &mut self,
        slot: SysSlot,
        at: Option<&Snapshot>,
    ) -> Result<Box<dyn RowSource>, HostFailure>;

    /// The 16-byte object id of the nominal type named `qualified_name` (module-qualified declaration name). It is the
    /// currency id a `Money` value carries (hir-host.md B4, B6). The default derives it with
    /// `orna_value::enum_type_id`, which needs no application state. The application may override it: the production
    /// scheme is the database-scoped identity of `orna-project`'s `derived_identity`, which the default does not
    /// reproduce.
    fn nominal_type_id(&self, qualified_name: &str) -> [u8; 16] {
        orna_value::enum_type_id(qualified_name)
    }
}

/// The host of a run that needs only tables: a table store, a clock fixed at the epoch, no entropy, no snapshots and
/// no `sys` member. The entry points that take no host build one.
pub struct StoreHost {
    store: Box<dyn TableStore>,
    manifest: HostManifest,
}

impl StoreHost {
    /// A host over `store`.
    #[must_use]
    pub fn new(store: Box<dyn TableStore>) -> Self {
        Self {
            store,
            manifest: HostManifest::new(),
        }
    }

    /// A host over a new empty store.
    #[must_use]
    pub fn empty() -> Self {
        Self::new(Box::new(MemoryStore::new()))
    }

    /// Gives the store back once the run that used this host is over.
    #[must_use]
    pub fn into_store(self) -> Box<dyn TableStore> {
        self.store
    }
}

impl Host for StoreHost {
    fn manifest(&self) -> &HostManifest {
        &self.manifest
    }

    fn store(&mut self) -> &mut dyn TableStore {
        self.store.as_mut()
    }

    fn store_at(&mut self, _at: &Snapshot) -> Result<Box<dyn TableStore + '_>, HostFailure> {
        Err(HostFailure {
            code: SNAPSHOT_NOT_FOUND,
        })
    }

    fn now(&self) -> HostInstant {
        HostInstant::EPOCH
    }

    fn entropy(&mut self, _out: &mut [u8]) -> Result<(), HostFailure> {
        Err(HostFailure { code: UNSUPPORTED })
    }

    fn activation(&self) -> Option<&ActivationContext> {
        None
    }

    fn sys_call(&mut self, _slot: SysSlot, _args: Vec<Raw>) -> Result<Raw, HostFailure> {
        Err(HostFailure { code: UNSUPPORTED })
    }

    fn sys_static(&mut self, _slot: SysSlot) -> Result<Raw, HostFailure> {
        Err(HostFailure { code: UNSUPPORTED })
    }

    fn sys_relation(
        &mut self,
        _slot: SysSlot,
        _at: Option<&Snapshot>,
    ) -> Result<Box<dyn RowSource>, HostFailure> {
        Err(HostFailure { code: UNSUPPORTED })
    }
}

/// A host for tests: a [`StoreHost`] plus the `sys` members a test registers, each with the rows, static value or call
/// result it answers with. A member the test does not register is unlinked, as on [`StoreHost`].
#[cfg(feature = "test-support")]
pub struct FixtureHost {
    inner: StoreHost,
    relations: HashMap<SysSlot, Vec<Raw>>,
    statics: HashMap<SysSlot, Raw>,
    calls: HashMap<SysSlot, SysCall>,
}

/// A fixture `sys` callable: canonical arguments in, a canonical result or the host's failure out.
#[cfg(feature = "test-support")]
pub type SysCall = fn(Vec<Raw>) -> Result<Raw, HostFailure>;

#[cfg(feature = "test-support")]
impl FixtureHost {
    /// A fixture host over `store`, with no `sys` member registered.
    pub fn new(store: Box<dyn TableStore>) -> Self {
        Self {
            inner: StoreHost::new(store),
            relations: HashMap::new(),
            statics: HashMap::new(),
            calls: HashMap::new(),
        }
    }

    /// Registers the relation `name` with its canonical rows.
    #[must_use]
    pub fn with_relation(mut self, name: &str, rows: Vec<Raw>) -> Self {
        let slot = self.inner.manifest.register(name);
        self.relations.insert(slot, rows);
        self
    }

    /// Registers the static `name` with its canonical value.
    #[must_use]
    pub fn with_static(mut self, name: &str, value: Raw) -> Self {
        let slot = self.inner.manifest.register(name);
        self.statics.insert(slot, value);
        self
    }

    /// Registers the callable `name` with its fixture implementation.
    #[must_use]
    pub fn with_call(mut self, name: &str, call: SysCall) -> Self {
        let slot = self.inner.manifest.register(name);
        self.calls.insert(slot, call);
        self
    }
}

/// The rows of one fixture relation, owned, so the cursor borrows nothing from the host.
#[cfg(feature = "test-support")]
struct VecRows(std::vec::IntoIter<Raw>);

#[cfg(feature = "test-support")]
impl RowSource for VecRows {
    fn next(&mut self) -> Result<Option<Raw>, HostFailure> {
        Ok(self.0.next())
    }
}

#[cfg(feature = "test-support")]
impl Host for FixtureHost {
    fn manifest(&self) -> &HostManifest {
        self.inner.manifest()
    }

    fn store(&mut self) -> &mut dyn TableStore {
        self.inner.store()
    }

    fn store_at(&mut self, at: &Snapshot) -> Result<Box<dyn TableStore + '_>, HostFailure> {
        self.inner.store_at(at)
    }

    fn now(&self) -> HostInstant {
        self.inner.now()
    }

    fn entropy(&mut self, out: &mut [u8]) -> Result<(), HostFailure> {
        self.inner.entropy(out)
    }

    fn activation(&self) -> Option<&ActivationContext> {
        self.inner.activation()
    }

    fn sys_call(&mut self, slot: SysSlot, args: Vec<Raw>) -> Result<Raw, HostFailure> {
        match self.calls.get(&slot) {
            Some(call) => call(args),
            None => Err(HostFailure { code: UNSUPPORTED }),
        }
    }

    fn sys_static(&mut self, slot: SysSlot) -> Result<Raw, HostFailure> {
        self.statics
            .get(&slot)
            .cloned()
            .ok_or(HostFailure { code: UNSUPPORTED })
    }

    fn sys_relation(
        &mut self,
        slot: SysSlot,
        at: Option<&Snapshot>,
    ) -> Result<Box<dyn RowSource>, HostFailure> {
        if at.is_some() {
            return Err(HostFailure {
                code: SNAPSHOT_NOT_FOUND,
            });
        }
        let rows = self
            .relations
            .get(&slot)
            .ok_or(HostFailure { code: UNSUPPORTED })?;
        Ok(Box::new(VecRows(rows.clone().into_iter())))
    }
}

#[cfg(test)]
mod tests {
    use orna_hir::{SysAdmission, SysMemberStatus};

    use super::*;

    #[test]
    fn store_host_clock_is_the_epoch_and_never_moves() {
        let host = StoreHost::empty();
        assert_eq!(host.now(), HostInstant::EPOCH);
        assert_eq!(host.now(), host.now());
    }

    #[test]
    fn store_host_refuses_entropy() {
        let mut host = StoreHost::empty();
        let mut bytes = [0_u8; 16];
        assert_eq!(
            host.entropy(&mut bytes),
            Err(HostFailure { code: UNSUPPORTED })
        );
        assert_eq!(bytes, [0_u8; 16]);
    }

    #[test]
    fn store_host_has_no_sys_members_and_no_snapshots() {
        let mut host = StoreHost::empty();
        assert!(host.manifest().is_empty());
        assert!(host.activation().is_none());
        assert!(host.sys_static(SysSlot(0)).is_err());
        assert!(host.sys_call(SysSlot(0), Vec::new()).is_err());
        assert!(host.sys_relation(SysSlot(0), None).is_err());
        let snapshot = Snapshot::Commit {
            database: [0; 16],
            algorithm: orna_value::GitHash::Sha1,
            oid: vec![0; 20],
        };
        let Err(failure) = host.store_at(&snapshot) else {
            panic!("a store host holds no snapshot");
        };
        assert_eq!(failure.code, SNAPSHOT_NOT_FOUND);
    }

    #[test]
    fn manifest_registers_each_name_once_with_a_stable_slot() {
        let mut manifest = HostManifest::new();
        let storage = manifest.register("sys.Storage");
        let tables = manifest.register("sys.catalog.tables");
        assert_ne!(storage, tables);
        assert_eq!(manifest.register("sys.Storage"), storage);
        assert_eq!(manifest.slot("sys.catalog.tables"), Some(tables));
        assert_eq!(manifest.name(storage), Some("sys.Storage"));
        assert_eq!(manifest.len(), 2);
        assert_eq!(
            manifest.names().collect::<Vec<_>>(),
            ["sys.Storage", "sys.catalog.tables"]
        );
    }

    #[test]
    fn manifest_admits_exactly_its_registered_paths() {
        let mut manifest = HostManifest::new();
        manifest.register("sys.Run");
        assert_eq!(manifest.status(&["sys", "Run"]), SysMemberStatus::Linked);
        assert_eq!(
            manifest.status(&["sys", "Storage"]),
            SysMemberStatus::Unlinked
        );
        assert_eq!(
            manifest.status(&["sys", "Run", "as_of"]),
            SysMemberStatus::Unlinked
        );
    }
}
