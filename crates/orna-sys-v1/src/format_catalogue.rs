//! Format- and context-scoped selection for the native sys catalogue.
//!
//! This module selects declaration metadata only. It deliberately does not
//! implement Blob values, codecs, repository readers, or runtime execution.
//! Those owners consume the selected typed contract at their respective
//! boundaries.

use std::{collections::BTreeMap, fmt};

use serde::Deserialize;

use super::{SystemEffect, SystemFunctionDescriptor};

const GENERATED_SYSTEM_CATALOGUE: &str =
    include_str!(concat!(env!("OUT_DIR"), "/system_api_catalogue.json"));

/// Repository writer/reader format coordinates recorded by the final
/// publication. Format 3 is the only current writer.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum RepositoryFormat {
    Original100 = 1,
    Previous110Draft = 2,
    Final3 = 3,
}

impl RepositoryFormat {
    pub const fn number(self) -> u16 {
        self as u16
    }

    pub const fn is_final(self) -> bool {
        matches!(self, Self::Final3)
    }

    pub const fn is_historical(self) -> bool {
        !self.is_final()
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Original100 => "1",
            Self::Previous110Draft => "2",
            Self::Final3 => "3",
        }
    }

    pub const fn from_number(number: u16) -> Option<Self> {
        match number {
            1 => Some(Self::Original100),
            2 => Some(Self::Previous110Draft),
            3 => Some(Self::Final3),
            _ => None,
        }
    }
}

impl TryFrom<u16> for RepositoryFormat {
    type Error = FormatContextError;

    fn try_from(number: u16) -> Result<Self, Self::Error> {
        Self::from_number(number).ok_or(FormatContextError::UnsupportedFormat(number))
    }
}

/// Human publication context paired with a recorded repository format.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum RepositoryContext {
    Original100,
    Previous110Draft,
    Final20261005,
}

impl RepositoryContext {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Original100 => "original 1.0.0",
            Self::Previous110Draft => "previous-1.1.0-draft.zip",
            Self::Final20261005 => "final-2026-10-05",
        }
    }

    pub const fn format(self) -> RepositoryFormat {
        match self {
            Self::Original100 => RepositoryFormat::Original100,
            Self::Previous110Draft => RepositoryFormat::Previous110Draft,
            Self::Final20261005 => RepositoryFormat::Final3,
        }
    }

    pub const fn is_historical(self) -> bool {
        !matches!(self, Self::Final20261005)
    }
}

/// A format and publication context recorded by a repository header/reader.
///
/// Constructing this pair validates that the context belongs to the selected
/// format. Callers cannot select a callable from a bare format number or an
/// unrecorded context string.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SystemFormatContext {
    format: RepositoryFormat,
    context: RepositoryContext,
}

/// Public name used by later repository/runtime owners when they carry a
/// recorded format/context pair across their boundary.
pub type RepositoryFormatContext = SystemFormatContext;

/// Compatibility name for callers that model the pair as recorded metadata.
pub type RecordedFormatContext = SystemFormatContext;

impl SystemFormatContext {
    pub const FORMAT_1_ORIGINAL_1_0_0: Self = Self {
        format: RepositoryFormat::Original100,
        context: RepositoryContext::Original100,
    };
    pub const FORMAT_2_PREVIOUS_1_1_0_DRAFT: Self = Self {
        format: RepositoryFormat::Previous110Draft,
        context: RepositoryContext::Previous110Draft,
    };
    pub const FORMAT_3_FINAL_2026_10_05: Self = Self {
        format: RepositoryFormat::Final3,
        context: RepositoryContext::Final20261005,
    };
    pub const FORMAT_1_ORIGINAL: Self = Self::FORMAT_1_ORIGINAL_1_0_0;
    pub const FORMAT_2_PREVIOUS_DRAFT: Self = Self::FORMAT_2_PREVIOUS_1_1_0_DRAFT;
    pub const FORMAT_3_FINAL: Self = Self::FORMAT_3_FINAL_2026_10_05;

    pub const fn format(self) -> RepositoryFormat {
        self.format
    }

    pub const fn repository_format(self) -> RepositoryFormat {
        self.format
    }

    pub const fn context(self) -> RepositoryContext {
        self.context
    }

    pub const fn context_label(self) -> &'static str {
        self.context.label()
    }

    pub const fn is_writer(self) -> bool {
        matches!(self, Self::FORMAT_3_FINAL_2026_10_05)
    }

    pub const fn is_historical_reader(self) -> bool {
        self.context.is_historical()
    }

    pub const fn new(format: RepositoryFormat, context: RepositoryContext) -> Option<Self> {
        let matches_recorded_pair = matches!(
            (format, context),
            (
                RepositoryFormat::Original100,
                RepositoryContext::Original100
            ) | (
                RepositoryFormat::Previous110Draft,
                RepositoryContext::Previous110Draft
            ) | (RepositoryFormat::Final3, RepositoryContext::Final20261005)
        );
        if matches_recorded_pair {
            Some(Self { format, context })
        } else {
            None
        }
    }

    pub fn from_recorded(format: u16, context: &str) -> Result<Self, FormatContextError> {
        let format = RepositoryFormat::try_from(format)?;
        let context = match context {
            "original 1.0.0" => RepositoryContext::Original100,
            "previous-1.1.0-draft.zip" => RepositoryContext::Previous110Draft,
            "final-2026-10-05" => RepositoryContext::Final20261005,
            _ => return Err(FormatContextError::UnknownContext(context.to_owned())),
        };
        Self::new(format, context).ok_or(FormatContextError::MismatchedContext { format, context })
    }
}

impl TryFrom<(u16, &str)> for SystemFormatContext {
    type Error = FormatContextError;

    fn try_from(recorded: (u16, &str)) -> Result<Self, Self::Error> {
        Self::from_recorded(recorded.0, recorded.1)
    }
}

/// The only errors possible while turning a recorded header pair into a
/// typed format context.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FormatContextError {
    UnsupportedFormat(u16),
    UnknownContext(String),
    MismatchedContext {
        format: RepositoryFormat,
        context: RepositoryContext,
    },
}

impl fmt::Display for FormatContextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedFormat(format) => {
                write!(formatter, "unsupported repository format {format}")
            }
            Self::UnknownContext(context) => {
                write!(formatter, "unknown repository context `{context}`")
            }
            Self::MismatchedContext { format, context } => write!(
                formatter,
                "repository context `{}` does not belong to format {}",
                context.label(),
                format.as_str()
            ),
        }
    }
}

impl std::error::Error for FormatContextError {}

/// Typed admission class for one callable contract.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SystemCallableAvailability {
    AllRecordedContexts,
    HistoricalReadersOnly,
    FinalFormat3,
}

impl SystemCallableAvailability {
    pub const fn admits(self, context: SystemFormatContext) -> bool {
        match self {
            Self::AllRecordedContexts => true,
            Self::HistoricalReadersOnly => context.is_historical_reader(),
            Self::FinalFormat3 => context.format().is_final(),
        }
    }
}

/// One native callable contract and its bounded format admission class.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SystemCallable {
    descriptor: SystemFunctionDescriptor,
    availability: SystemCallableAvailability,
}

impl SystemCallable {
    pub const fn descriptor_ref(&self) -> &SystemFunctionDescriptor {
        &self.descriptor
    }

    pub const fn descriptor(self) -> SystemFunctionDescriptor {
        self.descriptor
    }

    pub const fn availability(self) -> SystemCallableAvailability {
        self.availability
    }

    pub const fn is_admitted(self, context: SystemFormatContext) -> bool {
        self.availability.admits(context)
    }
}

#[derive(Clone, Debug, Deserialize)]
struct RawCatalogue<'a> {
    #[serde(borrow)]
    functions: Vec<RawFunction<'a>>,
}

#[derive(Clone, Debug, Deserialize)]
struct RawFunction<'a> {
    #[serde(borrow)]
    name: &'a str,
    #[serde(borrow)]
    effect: &'a str,
    #[serde(borrow)]
    signature: &'a str,
    #[serde(borrow)]
    purpose: &'a str,
    #[serde(borrow)]
    since: Option<&'a str>,
    #[serde(borrow)]
    documentation: Option<&'a str>,
    #[serde(borrow)]
    contract: Option<&'a str>,
    #[serde(borrow)]
    preconditions: Option<&'a str>,
    #[serde(borrow)]
    ownership: Option<&'a str>,
    #[serde(borrow)]
    snapshot_rule: Option<&'a str>,
    #[serde(borrow)]
    contexts: Option<Vec<&'a str>>,
}

/// Immutable native catalogue keyed by callable name, then selected through a
/// recorded [`SystemFormatContext`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SystemCatalogue {
    callables: BTreeMap<String, SystemCallable>,
}

impl SystemCatalogue {
    fn from_generated() -> Self {
        let raw: RawCatalogue<'static> = serde_json::from_str(GENERATED_SYSTEM_CATALOGUE)
            .expect("build-validated native sys format catalogue");
        let callables = raw
            .functions
            .into_iter()
            .map(|function| {
                let availability = match function.contexts.as_deref() {
                    None => SystemCallableAvailability::AllRecordedContexts,
                    Some(contexts)
                        if contexts
                            .iter()
                            .all(|context| matches!(*context, "format-1" | "format-2")) =>
                    {
                        SystemCallableAvailability::HistoricalReadersOnly
                    }
                    Some(contexts) if contexts.len() == 1 && contexts[0] == "format-3" => {
                        SystemCallableAvailability::FinalFormat3
                    }
                    Some(contexts) => panic!(
                        "invalid generated sys callable contexts for {}: {contexts:?}",
                        function.name
                    ),
                };
                let effect = match function.effect {
                    "read" => SystemEffect::Read,
                    "invoke" => SystemEffect::Invoke,
                    "admin" => SystemEffect::Admin,
                    _ => panic!("invalid generated sys effect for {}", function.name),
                };
                let descriptor = SystemFunctionDescriptor {
                    name: function.name,
                    effect,
                    signature: function.signature,
                    purpose: function.purpose,
                    since: function.since,
                    documentation: function.documentation,
                    contract: function.contract,
                    preconditions: function.preconditions,
                    ownership: function.ownership,
                    snapshot_rule: function.snapshot_rule,
                };
                (
                    function.name.to_owned(),
                    SystemCallable {
                        descriptor,
                        availability,
                    },
                )
            })
            .collect();
        Self { callables }
    }

    /// Select a callable using the recorded format/context pair.
    pub fn dispatch(
        &self,
        context: SystemFormatContext,
        name: &str,
    ) -> Result<&SystemCallable, SystemDispatchError> {
        let callable = self
            .callables
            .get(name)
            .ok_or_else(|| SystemDispatchError::UnknownCallable(name.to_owned()))?;
        if !callable.is_admitted(context) {
            return Err(SystemDispatchError::Unavailable {
                name: name.to_owned(),
                context,
            });
        }
        Ok(callable)
    }

    pub fn select(&self, context: SystemFormatContext, name: &str) -> Option<&SystemCallable> {
        self.dispatch(context, name).ok()
    }

    pub fn descriptor(
        &self,
        context: SystemFormatContext,
        name: &str,
    ) -> Option<&SystemFunctionDescriptor> {
        self.select(context, name)
            .map(|callable| &callable.descriptor)
    }

    pub fn callables(&self) -> impl Iterator<Item = &SystemCallable> {
        self.callables.values()
    }

    pub fn available(&self, context: SystemFormatContext) -> impl Iterator<Item = &SystemCallable> {
        self.callables
            .values()
            .filter(move |callable| callable.is_admitted(context))
    }
}

/// A context-aware dispatch failure. This layer only chooses metadata; it does
/// not attempt to execute an admitted operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SystemDispatchError {
    UnknownCallable(String),
    Unavailable {
        name: String,
        context: SystemFormatContext,
    },
}

/// The native typed format/context catalogue.
pub fn system_catalogue() -> &'static SystemCatalogue {
    static CATALOGUE: std::sync::LazyLock<SystemCatalogue> =
        std::sync::LazyLock::new(SystemCatalogue::from_generated);
    &CATALOGUE
}

/// Context-aware descriptor selection for callers that only need metadata.
pub fn system_function_descriptor_for(
    context: SystemFormatContext,
    name: &str,
) -> Option<&'static SystemFunctionDescriptor> {
    system_catalogue().descriptor(context, name)
}

/// Compatibility alias for the context-aware selector.
pub fn system_callable_for(
    context: SystemFormatContext,
    name: &str,
) -> Result<&'static SystemCallable, SystemDispatchError> {
    system_catalogue().dispatch(context, name)
}

/// Exact generated native catalogue bytes used to construct the typed table.
pub fn system_api_catalogue_json() -> &'static str {
    GENERATED_SYSTEM_CATALOGUE
}
