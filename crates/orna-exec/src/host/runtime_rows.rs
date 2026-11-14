//! The `RuntimeHostRows` host: the `sys.rt.*` relations and statics an embedder supplies as canonical rows (hir-host.md A5).
//!
//! The embedder encodes each runtime observation as a canonical row and registers it under its `sys` member name, for
//! example `sys.rt.runs`. Every registered member answers from those rows; a member the embedder does not register is
//! answered by the wrapped [`StoreHost`], which serves none, so it stays unlinked. A snapshot request is refused, since
//! the rows are a single current capture.

use std::collections::HashMap;

use orna_value::{Raw, Snapshot};

use super::{
    ActivationContext, Host, HostFailure, HostInstant, HostManifest, RowSource, SNAPSHOT_NOT_FOUND,
    StoreHost, SysSlot,
};
use crate::store::TableStore;

/// A host whose `sys.rt.*` members are the rows and statics the embedder registered.
pub struct RuntimeHostRows {
    inner: StoreHost,
    relations: HashMap<SysSlot, Vec<Raw>>,
    statics: HashMap<SysSlot, Raw>,
}

impl RuntimeHostRows {
    /// A host over `store`, with no `sys` member registered.
    pub fn new(store: Box<dyn TableStore>) -> Self {
        Self {
            inner: StoreHost::new(store),
            relations: HashMap::new(),
            statics: HashMap::new(),
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
}

/// The rows of one runtime relation, owned, so the cursor borrows nothing from the host.
struct RuntimeRows(std::vec::IntoIter<Raw>);

impl RowSource for RuntimeRows {
    fn next(&mut self) -> Result<Option<Raw>, HostFailure> {
        Ok(self.0.next())
    }
}

impl Host for RuntimeHostRows {
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
        self.inner.sys_call(slot, args)
    }

    fn sys_static(&mut self, slot: SysSlot) -> Result<Raw, HostFailure> {
        match self.statics.get(&slot) {
            Some(value) => Ok(value.clone()),
            None => self.inner.sys_static(slot),
        }
    }

    fn sys_relation(
        &mut self,
        slot: SysSlot,
        at: Option<&Snapshot>,
    ) -> Result<Box<dyn RowSource>, HostFailure> {
        let Some(rows) = self.relations.get(&slot) else {
            return self.inner.sys_relation(slot, at);
        };
        if at.is_some() {
            return Err(HostFailure {
                code: SNAPSHOT_NOT_FOUND,
            });
        }
        Ok(Box::new(RuntimeRows(rows.clone().into_iter())))
    }
}
