use std::{
    collections::HashSet,
    fmt,
    num::NonZeroU64,
    sync::{Arc, Mutex, OnceLock},
};

/// A non-zero identity for one CLIENT VM invocation.
///
/// Invocation identities are allocated by [`ClientVmInvocationAllocator`].
/// The checked constructor is useful when validating an identity received at a
/// control-plane boundary; it does not grant ownership to an allocator.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ClientVmInvocationId(NonZeroU64);

impl ClientVmInvocationId {
    /// Constructs an invocation identity from a non-zero integer.
    pub const fn new(value: u64) -> Result<Self, ClientVmIdentityError> {
        match NonZeroU64::new(value) {
            Some(value) => Ok(Self(value)),
            None => Err(ClientVmIdentityError::Zero),
        }
    }

    /// Returns the integer representation of this invocation identity.
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

impl TryFrom<u64> for ClientVmInvocationId {
    type Error = ClientVmIdentityError;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<NonZeroU64> for ClientVmInvocationId {
    fn from(value: NonZeroU64) -> Self {
        Self(value)
    }
}

/// Errors raised while allocating CLIENT VM invocation identities.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientVmIdentityError {
    /// No further non-zero identity can be allocated safely.
    Exhausted,
    /// The requested child parent was not allocated by this allocator.
    InvalidParent,
    /// An identity boundary supplied zero instead of a non-zero identity.
    Zero,
    /// The shared registry lock is no longer usable.
    RegistryUnavailable,
    /// The owning host context has been cancelled.
    Cancelled,
    /// The root has no authorised revision/security binding yet.
    UnboundRoot,
}

impl fmt::Display for ClientVmIdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exhausted => formatter.write_str("CLIENT VM invocation identity space exhausted"),
            Self::InvalidParent => formatter.write_str("CLIENT VM invocation parent is invalid"),
            Self::Zero => formatter.write_str("CLIENT VM invocation identity must be non-zero"),
            Self::RegistryUnavailable => {
                formatter.write_str("CLIENT VM invocation identity registry is unavailable")
            }
            Self::Cancelled => formatter.write_str("CLIENT VM host context is cancelled"),
            Self::UnboundRoot => formatter.write_str("CLIENT VM root has no admission binding"),
        }
    }
}

impl std::error::Error for ClientVmIdentityError {}

/// An in-memory allocator for one CLIENT VM invocation identity domain.
///
/// The allocator owns all identities it returns and retains them for its whole
/// lifetime. This makes parent validation deterministic and prevents an issued
/// identity from being reused by this allocator.
#[derive(Debug)]
pub struct ClientVmInvocationAllocator {
    next: Option<NonZeroU64>,
    issued: HashSet<ClientVmInvocationId>,
}

impl ClientVmInvocationAllocator {
    /// Creates an empty allocator whose first identity is one.
    pub fn new() -> Self {
        Self {
            next: NonZeroU64::new(1),
            issued: HashSet::new(),
        }
    }

    /// Allocates a fresh root invocation identity.
    pub fn allocate_root(&mut self) -> Result<ClientVmInvocationId, ClientVmIdentityError> {
        self.allocate()
    }

    /// Allocates a fresh child identity under an identity owned by this allocator.
    pub fn allocate_child(
        &mut self,
        parent: ClientVmInvocationId,
    ) -> Result<ClientVmInvocationId, ClientVmIdentityError> {
        if !self.issued.contains(&parent) {
            return Err(ClientVmIdentityError::InvalidParent);
        }

        self.allocate()
    }

    fn allocate(&mut self) -> Result<ClientVmInvocationId, ClientVmIdentityError> {
        let Some(raw) = self.next.take() else {
            return Err(ClientVmIdentityError::Exhausted);
        };

        let id = ClientVmInvocationId(raw);
        self.next = raw.get().checked_add(1).and_then(NonZeroU64::new);

        // The sequence is strictly increasing, so this cannot collide unless
        // allocator state has been corrupted internally.
        debug_assert!(self.issued.insert(id));
        #[cfg(not(debug_assertions))]
        self.issued.insert(id);
        Ok(id)
    }
}

impl Default for ClientVmInvocationAllocator {
    fn default() -> Self {
        Self::new()
    }
}

/// A host-wide in-memory invocation identity registry.
///
/// Every registry instance in one process shares the same allocator. This
/// prevents a second VM host from restarting the sequence at one and reusing
/// a root identity. Issued identities remain reserved until process shutdown.
#[derive(Clone, Debug)]
pub struct ClientVmInvocationRegistry {
    allocator: Arc<Mutex<ClientVmInvocationAllocator>>,
}

impl ClientVmInvocationRegistry {
    /// Creates a handle to the process-wide host registry.
    pub fn new() -> Self {
        static ALLOCATOR: OnceLock<Arc<Mutex<ClientVmInvocationAllocator>>> = OnceLock::new();
        Self {
            allocator: ALLOCATOR
                .get_or_init(|| Arc::new(Mutex::new(ClientVmInvocationAllocator::new())))
                .clone(),
        }
    }

    /// Allocates a fresh root identity.
    pub fn allocate_root(&self) -> Result<ClientVmInvocationId, ClientVmIdentityError> {
        self.allocator
            .lock()
            .map_err(|_| ClientVmIdentityError::RegistryUnavailable)?
            .allocate_root()
    }

    /// Allocates a fresh child identity under an issued parent.
    pub fn allocate_child(
        &self,
        parent: ClientVmInvocationId,
    ) -> Result<ClientVmInvocationId, ClientVmIdentityError> {
        self.allocator
            .lock()
            .map_err(|_| ClientVmIdentityError::RegistryUnavailable)?
            .allocate_child(parent)
    }
}

impl Default for ClientVmInvocationRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_and_child_ids_are_unique_and_monotonic() {
        let mut allocator = ClientVmInvocationAllocator::new();
        let root = allocator.allocate_root().expect("root allocation");
        let first_child = allocator
            .allocate_child(root)
            .expect("first child allocation");
        let second_child = allocator
            .allocate_child(root)
            .expect("second child allocation");
        let grandchild = allocator
            .allocate_child(first_child)
            .expect("grandchild allocation");

        let ids = [root, first_child, second_child, grandchild];
        for window in ids.windows(2) {
            assert!(window[0] < window[1]);
        }
        let unique_count = ids.iter().collect::<HashSet<_>>().len();
        assert_eq!(unique_count, ids.len());
        assert!(ids.iter().all(|id| id.get() != 0));
    }

    #[test]
    fn allocated_parents_are_retained_for_later_children() {
        let mut allocator = ClientVmInvocationAllocator::new();
        let root = allocator.allocate_root().expect("root allocation");
        let child = allocator.allocate_child(root).expect("child allocation");

        assert!(allocator.allocate_child(root).is_ok());
        assert!(allocator.allocate_child(child).is_ok());
    }

    #[test]
    fn zero_identity_is_rejected() {
        assert_eq!(
            ClientVmInvocationId::new(0),
            Err(ClientVmIdentityError::Zero)
        );
        assert_eq!(
            ClientVmInvocationId::try_from(0),
            Err(ClientVmIdentityError::Zero)
        );
    }

    #[test]
    fn unknown_parent_is_rejected() {
        let mut allocator = ClientVmInvocationAllocator::new();
        let unknown = ClientVmInvocationId::new(42).expect("non-zero test identity");

        assert_eq!(
            allocator.allocate_child(unknown),
            Err(ClientVmIdentityError::InvalidParent)
        );
    }

    #[test]
    fn allocation_reports_exhaustion_without_wrapping() {
        let mut allocator = ClientVmInvocationAllocator {
            next: NonZeroU64::new(u64::MAX),
            issued: HashSet::new(),
        };

        let maximum = allocator.allocate_root().expect("maximum identity");
        assert_eq!(maximum.get(), u64::MAX);
        assert_eq!(
            allocator.allocate_root(),
            Err(ClientVmIdentityError::Exhausted)
        );
    }

    #[test]
    fn registry_instances_share_process_identity_space() {
        let first = ClientVmInvocationRegistry::new();
        let second = ClientVmInvocationRegistry::new();
        let first_root = first.allocate_root().expect("first registry root");
        let second_root = second.allocate_root().expect("second registry root");
        assert_ne!(first_root, second_root);
    }
}
