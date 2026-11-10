//! Definitions (design section 2.2).
//!
//! Every declaration in a program is one [`Def`] in the program's `defs` table,
//! and [`DefKind`] states what it is. A function lives here whoever implements
//! it: [`FnBody::Source`] points at an Orna body, [`FnBody::Native`] records
//! the ABI and key the link step binds to a host implementation.

use orna_syntax::SyntaxSpan as SourceSpan;

use crate::{BodyId, DefId, ModuleId, ModulePath, Name, PatId, Ty};

/// Whether a definition is visible outside its module.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Visibility {
    /// Visible to importing modules.
    Public,
    /// Visible only inside its own module.
    Private,
}

/// One declaration.
#[derive(Clone, Debug)]
pub struct Def {
    /// The module that declares it.
    pub module: ModuleId,
    /// Its declared name.
    pub name: Name,
    /// Its declared visibility.
    pub vis: Visibility,
    /// Where it was written.
    pub span: SourceSpan,
    /// What it declares.
    pub kind: DefKind,
}

/// What one definition is.
#[derive(Clone, Debug)]
pub enum DefKind {
    /// A function, native or with an Orna body.
    Fn(FnDef),
    /// A table.
    Table(TableDef),
    /// A type alias or nominal type, refinements included.
    Type(TypeDef),
    /// An enum; its variants are [`DefKind::Variant`] definitions.
    Enum(EnumDef),
    /// One enum variant.
    Variant(VariantDef),
    /// A protocol.
    Protocol(ProtocolDef),
    /// A protocol implementation on a table or a type.
    Impl(ImplDef),
    /// A dimension declaration, if decision D8 keeps dimensions.
    Dimension(DimensionDef),
    /// A unit declaration, if decision D8 keeps units.
    Unit(UnitDef),
    /// A module-level binding.
    Let(LetDef),
    /// A module- or table-level assertion.
    Assertion(AssertionDef),
    /// A table, nominal or enum-payload field.
    Field(FieldDef),
}

impl DefKind {
    /// Whether this is a table field, nominal field or enum-payload field.
    #[must_use]
    pub fn is_field(&self) -> bool {
        matches!(self, Self::Field(_))
    }

    /// Whether this is an enum variant.
    #[must_use]
    pub fn is_variant(&self) -> bool {
        matches!(self, Self::Variant(_))
    }

    /// Whether this is a protocol.
    #[must_use]
    pub fn is_protocol(&self) -> bool {
        matches!(self, Self::Protocol(_))
    }

    /// Whether this is a unit declaration.
    #[must_use]
    pub fn is_unit(&self) -> bool {
        matches!(self, Self::Unit(_))
    }

    /// Whether this is an assertion.
    #[must_use]
    pub fn is_assertion(&self) -> bool {
        matches!(self, Self::Assertion(_))
    }
}

/// One function.
#[derive(Clone, Debug)]
pub struct FnDef {
    /// Generic parameters with their protocol bounds, resolved to definitions.
    pub generics: Vec<GenericParam>,
    /// Parameters in declaration order. Parameter 0 is the piped input.
    pub params: Vec<ParamDef>,
    /// The declared or inferred result type.
    pub ret: Ty,
    /// The effects the function can perform, computed by the effect pass.
    pub effects: crate::EffectSet,
    /// Where the signature ends: an Orna body or a native declaration.
    pub body: FnBody,
}

/// Where a function's implementation lives.
#[derive(Clone, Debug)]
pub enum FnBody {
    /// An Orna body. Lambdas and initializers use the same [`crate::Body`]
    /// shape and are not `FnDef`s.
    Source(BodyId),
    /// A declaration with no implementation: a protocol member. Callers reach
    /// it through a protocol bound, never through a `DefId` call, so there is
    /// nothing to lower or link.
    Signature,
    /// A `native fn` declaration in `core.orna`, `sys.orna` or the pinned
    /// `std`: no body, so the signature above is its only signature. The link
    /// step binds it to a host implementation keyed by this definition.
    Native(NativeDecl),
}

/// One native declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeDecl {
    /// The engine native ABI the declaration was written against.
    pub abi: NativeAbi,
    /// The identity the link step binds.
    pub key: NativeKey,
    /// The effect kinds the declaration's `effect` clause names. Empty for
    /// `effect none`. The effect pass adds `MayFail` to a non-empty set.
    pub effects: crate::EffectSet,
}

/// The engine native ABI version (design section 4.5).
///
/// Bumped when a native's behaviour or signature changes incompatibly. A
/// program records the ABI it was admitted with, and the link step refuses a
/// program whose recorded ABI the engine does not implement.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct NativeAbi(u32);

impl NativeAbi {
    /// Wraps an ABI number.
    #[must_use]
    pub const fn new(number: u32) -> Self {
        Self(number)
    }

    /// The ABI number.
    #[must_use]
    pub const fn number(self) -> u32 {
        self.0
    }
}

/// The stable identity of one native implementation.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct NativeKey {
    /// The declaring module path.
    pub module: ModulePath,
    /// The declared name inside that module.
    pub name: Name,
}

/// One generic parameter.
#[derive(Clone, Debug)]
pub struct GenericParam {
    /// Its name.
    pub name: Name,
    /// Its protocol bounds, resolved to [`DefKind::Protocol`] definitions.
    pub bounds: Vec<Bound>,
}

/// One protocol bound on a generic parameter, such as `T: From<Str>`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Bound {
    /// The bounding protocol.
    pub protocol: DefId,
    /// The protocol's type arguments, in the protocol's parameter order.
    pub args: Box<[Ty]>,
}

/// One parameter.
#[derive(Clone, Debug)]
pub struct ParamDef {
    /// Its name, used by named arguments.
    pub name: Name,
    /// Its type.
    pub ty: Ty,
    /// Whether the declaration gives the parameter a type: a written annotation (even one that fails to resolve,
    /// which is reported by lowering) or a method's receiver. `false` means the author omitted it (decision D1).
    pub written: bool,
    /// The default expression, as its own body, when the declaration has one.
    pub default: Option<BodyId>,
    /// The binder pattern, when the declaration destructures the argument.
    pub binder: Option<PatId>,
}

/// A table.
#[derive(Clone, Debug)]
pub struct TableDef {
    /// Its fields, in declaration order.
    pub fields: Vec<DefId>,
    /// The key fields.
    pub key: Vec<DefId>,
    /// Its assertions.
    pub assertions: Vec<DefId>,
}

/// A type alias or nominal type.
#[derive(Clone, Debug)]
pub struct TypeDef {
    /// Its generic parameters.
    pub params: Vec<GenericParam>,
    /// Its declared fields, refinements and nested implementations. A field
    /// is a [`DefKind::Field`]; an implementation is a [`DefKind::Impl`].
    pub members: Vec<DefId>,
}

/// An enum.
#[derive(Clone, Debug)]
pub struct EnumDef {
    /// Its variants, in declaration order.
    pub variants: Vec<DefId>,
    /// Its nested implementations, in declaration order. Each is a [`DefKind::Impl`].
    pub members: Vec<DefId>,
}

/// One enum variant.
#[derive(Clone, Debug)]
pub struct VariantDef {
    /// Its payload fields, in declaration order.
    pub fields: Vec<DefId>,
}

/// A protocol.
///
/// A member's generic parameters are the protocol's parameters, then `Self`
/// (the implementing type, at index `params.len()`), then the member's own.
#[derive(Clone, Debug)]
pub struct ProtocolDef {
    /// Its type parameters, in declaration order.
    pub params: Vec<GenericParam>,
    /// Its members, in declaration order.
    pub members: Vec<DefId>,
}

/// One protocol implementation.
#[derive(Clone, Debug)]
pub struct ImplDef {
    /// The type the implementation is declared on.
    pub owner: DefId,
    /// The protocol implemented, when this is a protocol implementation
    /// rather than a type's own member block.
    pub protocol: Option<DefId>,
    /// The protocol's type arguments, in the protocol's parameter order.
    pub protocol_args: Box<[Ty]>,
    /// The implementations it contributes.
    pub members: Vec<DefId>,
}

/// A dimension declaration.
#[derive(Clone, Debug)]
pub struct DimensionDef {
    /// The unit it coerces to, when it declares one.
    pub unit: Option<DefId>,
}

/// A unit declaration.
#[derive(Clone, Debug, Default)]
pub struct UnitDef;

/// A module-level binding.
#[derive(Clone, Debug)]
pub struct LetDef {
    /// The initializer body.
    pub body: BodyId,
}

/// An assertion.
#[derive(Clone, Debug)]
pub struct AssertionDef {
    /// The assertion body.
    pub body: BodyId,
    /// The row the assertion belongs to: its table, or the assertion itself for a
    /// module assertion. The body is owned by the assertion, so this is not `body.owner`.
    pub owner: DefId,
}

/// A field of a table, a nominal type or an enum payload.
#[derive(Clone, Debug)]
pub struct FieldDef {
    /// The declared type.
    pub ty: Ty,
}
