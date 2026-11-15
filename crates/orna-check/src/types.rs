//! The semantic value types: types, symbols, tables and modules.

use super::*;
use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Type {
    Int,
    Decimal,
    Float,
    Date,
    Instant,
    Text,
    Bool,
    Null,
    /// A completed no-value expression; distinct from the portable null value.
    Unit,
    List(Box<Type>),
    /// A bounded numeric range. This semantic-only value retains its bound
    /// type without constructing an iterator or admitting generic ranges.
    Range(Box<Type>),
    /// A composable table/query value. This is a semantic summary only; it
    /// neither enumerates rows nor constructs a runtime query plan.
    Relation(Box<Type>),
    /// A checkpointable source summary. This crate never starts or resumes it.
    Stream(Box<Type>),
    Record(BTreeMap<String, Type>),
    Tuple(Vec<Type>),
    Optional(Box<Type>),
    /// A named type constructor with its statically resolved arguments. This
    /// retains enough identity for closed collection rules without treating a
    /// parameterized type as an untyped nominal name.
    Applied {
        base: String,
        arguments: Vec<Type>,
    },
    /// A numeric quantity resolved from a source unit; its structural
    /// dimensions and exact conversion metadata survive dimensional algebra.
    Quantity {
        numeric: Box<Type>,
        dimension_basis: BTreeMap<SourceDimensionIdentity, i64>,
        scale: SourceUnitRational,
        offset: SourceUnitRational,
        affine: bool,
    },
    /// The one rate shape needed by the exact money rule. This is not a
    /// general dimensional-algebra representation: unsupported products still
    /// remain Error and are never treated as dynamic values.
    MoneyPerUnit {
        currency: Box<Type>,
        unit: Box<Type>,
    },
    Function {
        parameters: Vec<Type>,
        /// Source parameter names when every declaration pattern is a name.
        /// Function type expressions and catalogue summaries do not invent
        /// names, so named invocation of those summaries fails closed.
        parameter_names: Option<Vec<String>>,
        /// Positions with a declaration-provided default; omitted arguments
        /// are legal only at these positions.
        default_parameters: BTreeSet<usize>,
        result: Box<Type>,
    },
    Named(String),
    /// An expression that cannot complete successfully. This is a valid
    /// successful-result subtype for contextual checking, unlike Error,
    /// which records that semantic analysis already emitted a diagnostic.
    Bottom,
    /// A diagnostic has already been emitted; this is never a dynamic Any.
    Error,
}

/// The REPL-facing spelling of a static type, as shown after a result
/// (`3 : Int`, `"a" : Str`, `main.Contact : Relation<Contact>`).
impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Int => f.write_str("Int"),
            Self::Decimal => f.write_str("Decimal"),
            Self::Float => f.write_str("Float"),
            Self::Date => f.write_str("Date"),
            Self::Instant => f.write_str("Instant"),
            Self::Text => f.write_str("Str"),
            Self::Bool => f.write_str("Bool"),
            Self::Null => f.write_str("Null"),
            Self::Unit => f.write_str("Unit"),
            Self::List(element) => write!(f, "[{element}]"),
            Self::Range(element) => write!(f, "Range<{element}>"),
            Self::Relation(element) => write!(f, "Relation<{element}>"),
            Self::Stream(element) => write!(f, "Stream<{element}>"),
            Self::Record(fields) => {
                f.write_str("{")?;
                for (index, (name, ty)) in fields.iter().enumerate() {
                    if index > 0 {
                        f.write_str(",")?;
                    }
                    write!(f, " {name}: {ty}")?;
                }
                if !fields.is_empty() {
                    f.write_str(" ")?;
                }
                f.write_str("}")
            }
            Self::Tuple(elements) => {
                f.write_str("(")?;
                write_separated(f, elements)?;
                f.write_str(")")
            }
            Self::Optional(element) => write!(f, "{element}?"),
            Self::Applied { base, arguments } => {
                write!(f, "{base}<")?;
                write_separated(f, arguments)?;
                f.write_str(">")
            }
            Self::Quantity {
                numeric,
                dimension_basis,
                ..
            } => {
                write!(f, "{numeric}")?;
                if dimension_basis.is_empty() {
                    return Ok(());
                }
                f.write_str("<")?;
                for (index, (dimension, exponent)) in dimension_basis.iter().enumerate() {
                    if index > 0 {
                        f.write_str("·")?;
                    }
                    f.write_str(&dimension.display())?;
                    if *exponent != 1 {
                        write!(f, "^{exponent}")?;
                    }
                }
                f.write_str(">")
            }
            Self::MoneyPerUnit { currency, unit } => write!(f, "Money<{currency}>/{unit}"),
            Self::Function {
                parameters,
                parameter_names,
                result,
                ..
            } => {
                f.write_str("fn(")?;
                for (index, parameter) in parameters.iter().enumerate() {
                    if index > 0 {
                        f.write_str(", ")?;
                    }
                    match parameter_names.as_ref().and_then(|names| names.get(index)) {
                        Some(name) => write!(f, "{name}: {parameter}")?,
                        None => write!(f, "{parameter}")?,
                    }
                }
                write!(f, "): {result}")
            }
            Self::Named(name) => f.write_str(name),
            Self::Bottom => f.write_str("Never"),
            Self::Error => f.write_str("<error>"),
        }
    }
}

fn write_separated(f: &mut fmt::Formatter<'_>, types: &[Type]) -> fmt::Result {
    for (index, ty) in types.iter().enumerate() {
        if index > 0 {
            f.write_str(", ")?;
        }
        write!(f, "{ty}")?;
    }
    Ok(())
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EffectSummary {
    /// Host effects of the `sys.EffectKind` vocabulary.
    pub effects: BTreeSet<EffectKind>,
    /// Effectful operation with no `sys.EffectKind`, such as `sys.invoke` or
    /// an admin operation. Fails closed: it counts as effectful everywhere.
    pub opaque: bool,
    pub may_fail: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SymbolKind {
    Function,
    Parameter,
    Table,
    Type,
    Enum,
    Protocol,
    Dimension,
    Unit,
    Let,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GenericParameterMetadata {
    pub name: String,
    /// Protocol identities retained beside exported generic declarations.
    pub bounds: Vec<Type>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Symbol {
    pub kind: SymbolKind,
    pub ty: Type,
    pub public: bool,
    pub effects: EffectSummary,
    pub generic_parameters: Vec<GenericParameterMetadata>,
    /// Closed enum variants retained for schema-directed key reconstruction.
    pub enum_variants: BTreeSet<String>,
    /// Declaration-backed write admission, retained through module exports.
    /// None means the caller supplied no field-admission metadata.
    pub table_schema: Option<TableSchema>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableSchema {
    pub fields: BTreeMap<String, Type>,
    /// Stored references from this table's columns to other tables.
    /// Populated once every table in the module is declared, because a
    /// reference is resolved against the module's table symbols.
    pub references: Vec<TableReference>,
    /// Visible field names in the order the declaration lists them. `fields`
    /// is a map, so it cannot carry declaration order; `sys.Table.columns`
    /// needs it (ORNA-SYS table columns, Gate G).
    pub declaration_order: Vec<String>,
    /// None preserves a shape-only catalogue without inventing declaration rules.
    pub admission: Option<TableAdmission>,
    /// Nested nominal From implementations exported with the target type.
    pub conversions: Vec<(Type, EffectSummary)>,
    /// The nominal type implements the core `Currency` protocol, so it can
    /// parameterize `Money` and select a currency literal (ORNA-MONEY-004,
    /// ORNA-LIT-007). Exported so importers never infer it from a name.
    pub currency: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableAdmission {
    pub required: BTreeSet<String>,
    /// True when at least one required declaration field is hidden from the
    /// exported representation. The field name and type remain private.
    pub private_required: bool,
    pub computed: BTreeSet<String>,
    /// Ordered explicit key columns, or the implicit automatic id column.
    pub keys: Vec<(String, Type)>,
    pub automatic_key: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleHeader {
    pub namespace: Namespace,
    pub exports: BTreeMap<String, Symbol>,
    pub symbols: BTreeMap<String, Symbol>,
    pub generic_functions: BTreeMap<String, Vec<GenericParameterMetadata>>,
    /// Public non-generic protocol member surfaces captured from source-backed
    /// modules so importers can validate implementations without loading files.
    pub protocols: BTreeMap<String, Vec<ProtocolMember>>,
    pub prelude_exports: BTreeSet<String>,
    /// Attached catalogue modules may be available without a source `use`.
    /// Source modules remain explicit by default.
    pub implicit: bool,
}
