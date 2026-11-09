//! Opaque identity types and descriptive references.

use serde::Serialize;

macro_rules! identity {
    ($name:ident) => {
        #[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
        pub struct $name(String);
        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

identity!(FunctionId);
identity!(SnapshotId);
identity!(RuntimeId);
identity!(InvocationId);
identity!(TypeId);

/// Runtime-issued descriptive reference to an invocation.
///
/// This is deliberately not a durable `sys.RowRef<sys.Invocation>`: this
/// local supervisor has neither a catalogue nor row-authority evidence. It
/// can describe an invocation without granting the ability to operate on it.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
pub struct InvocationRef(InvocationId);

impl InvocationRef {
    pub(crate) fn from_id(id: InvocationId) -> Self {
        Self(id)
    }

    pub(crate) fn id(&self) -> &InvocationId {
        &self.0
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

/// Runtime-issued descriptive reference to a static result type.
///
/// As with [`InvocationRef`], this is a nominal description only, not a
/// catalogue-authorized durable type-row reference.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
pub struct TypeRef(TypeId);

impl TypeRef {
    pub(crate) fn from_id(id: TypeId) -> Self {
        Self(id)
    }

    pub(crate) fn id(&self) -> &TypeId {
        &self.0
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

/// Opaque identity for one immutable semantic revision.
///
/// Its 32 bytes are supplied by the authoritative revision producer. This
/// type intentionally neither derives those bytes nor treats them as another
/// identifier or digest type.
///
/// The derived `Serialize` implementation is an internal, non-normative
/// projection, including when a caller selects JSON. It is not a portable wire
/// encoding. Portable `sys.RevisionId` values use the separate OVB tag-60023
/// `[qualified_type_name, representation]` boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
pub struct RevisionId([u8; 32]);

impl RevisionId {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub const fn into_bytes(self) -> [u8; 32] {
        self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FunctionIdentity {
    pub function: FunctionId,
    pub revision: RevisionId,
    pub snapshot: SnapshotId,
}
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
pub struct ObjectRef(String);

impl ObjectRef {
    pub fn descriptive(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A descriptive plan reference in an explanation.
///
/// A plan is optional because an explanation may be produced before planning;
/// this type does not make an unverified plan executable.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
pub struct PlanRef(String);

impl PlanRef {
    pub fn descriptive(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}
