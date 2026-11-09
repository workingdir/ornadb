//! Closed signature language for the intrinsic catalogue.
//!
//! `orna-intrinsics` must not depend on `orna-check`, where the checker's
//! `Type` lives, so this module defines the smallest closed type language the
//! catalogue needs. The semantic checker maps every `SigType` to its own `Type`
//! with a total function.

/// A type in a registry signature.
///
/// The variants are the closed set named by the design's section 3.2. Container
/// variants borrow their element types from `'static` storage because every
/// descriptor is a `const` item.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SigType {
    Unit,
    Bool,
    Int,
    Float,
    Decimal,
    Str,
    Blob,
    Path,
    Digest,
    Uuid,
    Date,
    Instant,
    LocalDateTime,
    TimeOfDay,
    Duration,
    TimeZone,
    ZonedDateTime,
    Error,
    JsonValue,
    Ui,
    Money,
    List(&'static SigType),
    Relation(&'static SigType),
    Query(&'static SigType),
    Stream(&'static SigType),
    Option(&'static SigType),
    Range(&'static SigType),
    Tuple(&'static [SigType]),
    Record(&'static [(&'static str, SigType)]),
    Nominal(&'static str),
    Fn(&'static [SigType], &'static SigType),
    /// A type parameter, by index into [`Signature::type_params`].
    Param(u8),
}

impl SigType {
    /// Renders the type in the catalogue's compact, deterministic notation.
    ///
    /// This notation is the `std_intrinsics.json` wire form. It is deliberately
    /// close to Orna source notation but keeps type parameters explicit as
    /// `Param(n)` so two signatures cannot compare equal by accident.
    pub fn render(&self) -> String {
        match self {
            SigType::Unit => "Unit".to_owned(),
            SigType::Bool => "Bool".to_owned(),
            SigType::Int => "Int".to_owned(),
            SigType::Float => "Float".to_owned(),
            SigType::Decimal => "Decimal".to_owned(),
            SigType::Str => "Str".to_owned(),
            SigType::Blob => "Blob".to_owned(),
            SigType::Path => "Path".to_owned(),
            SigType::Digest => "Digest".to_owned(),
            SigType::Uuid => "Uuid".to_owned(),
            SigType::Date => "Date".to_owned(),
            SigType::Instant => "Instant".to_owned(),
            SigType::LocalDateTime => "LocalDateTime".to_owned(),
            SigType::TimeOfDay => "TimeOfDay".to_owned(),
            SigType::Duration => "Duration".to_owned(),
            SigType::TimeZone => "TimeZone".to_owned(),
            SigType::ZonedDateTime => "ZonedDateTime".to_owned(),
            SigType::Error => "Error".to_owned(),
            SigType::JsonValue => "JsonValue".to_owned(),
            SigType::Ui => "Ui".to_owned(),
            SigType::Money => "Money".to_owned(),
            SigType::List(element) => format!("[{}]", element.render()),
            SigType::Relation(element) => format!("Relation<{}>", element.render()),
            SigType::Query(element) => format!("Query<{}>", element.render()),
            SigType::Stream(element) => format!("Stream<{}>", element.render()),
            SigType::Option(element) => format!("{}?", element.render()),
            SigType::Range(element) => format!("Range<{}>", element.render()),
            SigType::Tuple(elements) => {
                let rendered = elements.iter().map(SigType::render).collect::<Vec<_>>();
                format!("({})", rendered.join(", "))
            }
            SigType::Record(fields) => {
                let rendered = fields
                    .iter()
                    .map(|(name, ty)| format!("{name}: {}", ty.render()))
                    .collect::<Vec<_>>();
                format!("{{ {} }}", rendered.join(", "))
            }
            SigType::Nominal(name) => (*name).to_owned(),
            SigType::Fn(parameters, result) => {
                let rendered = parameters.iter().map(SigType::render).collect::<Vec<_>>();
                format!("fn({}): {}", rendered.join(", "), result.render())
            }
            SigType::Param(index) => format!("Param({index})"),
        }
    }
}

/// One declared parameter of an intrinsic.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Param {
    pub name: &'static str,
    pub ty: SigType,
    pub has_default: bool,
    /// True for argument one, which a pipeline may supply (`spec 9:21`).
    pub pipeline: bool,
}

/// The complete declared signature of one intrinsic.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Signature {
    pub type_params: &'static [&'static str],
    pub params: &'static [Param],
    pub result: SigType,
    /// Accepted total value counts after the pipeline input. Empty means the
    /// required/default prefix of `params` decides.
    pub arities: &'static [usize],
}

impl Signature {
    /// Returns the index of the named parameter, when one exists.
    pub fn parameter_index(&self, name: &str) -> Option<usize> {
        self.params.iter().position(|param| param.name == name)
    }

    /// Checks that every `Param` index is bound by `type_params`.
    ///
    /// The catalogue test uses this to catch a transcription error before the
    /// semantic checker ever maps the signature.
    pub fn is_well_formed(&self) -> bool {
        let arity = u8::try_from(self.type_params.len()).ok();
        let Some(arity) = arity else {
            return false;
        };
        self.params
            .iter()
            .all(|param| sig_type_is_bound(param.ty, arity))
            && sig_type_is_bound(self.result, arity)
    }
}

fn sig_type_is_bound(ty: SigType, arity: u8) -> bool {
    match ty {
        SigType::List(inner)
        | SigType::Relation(inner)
        | SigType::Query(inner)
        | SigType::Stream(inner)
        | SigType::Option(inner)
        | SigType::Range(inner) => sig_type_is_bound(*inner, arity),
        SigType::Tuple(elements) => elements
            .iter()
            .all(|element| sig_type_is_bound(*element, arity)),
        SigType::Record(fields) => fields
            .iter()
            .all(|(_, field)| sig_type_is_bound(*field, arity)),
        SigType::Fn(parameters, result) => {
            parameters
                .iter()
                .all(|parameter| sig_type_is_bound(*parameter, arity))
                && sig_type_is_bound(*result, arity)
        }
        SigType::Param(index) => index < arity,
        SigType::Unit
        | SigType::Bool
        | SigType::Int
        | SigType::Float
        | SigType::Decimal
        | SigType::Str
        | SigType::Blob
        | SigType::Path
        | SigType::Digest
        | SigType::Uuid
        | SigType::Date
        | SigType::Instant
        | SigType::LocalDateTime
        | SigType::TimeOfDay
        | SigType::Duration
        | SigType::TimeZone
        | SigType::ZonedDateTime
        | SigType::Error
        | SigType::JsonValue
        | SigType::Ui
        | SigType::Money
        | SigType::Nominal(_) => true,
    }
}
