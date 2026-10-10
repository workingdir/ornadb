//! Format- and context-scoped selection for native sys API declarations.
//!
//! This module selects declaration metadata only. Callers must verify recorded
//! repository format/profile before constructing a context; selection does not
//! implement Blob values, codecs, repository readers, runtime execution, or
//! invocation authority.

use std::collections::BTreeMap;

use serde::Deserialize;

use super::{SystemEffect, SystemFunctionDescriptor};

const GENERATED_API_SELECTION: &str =
    include_str!(concat!(env!("OUT_DIR"), "/system_api_selection.json"));

/// Repository writer/reader formats known to this compatibility catalogue.
/// Format 3 is the only current writer.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum RepositoryFormat {
    Original100 = 1,
    Previous110Draft = 2,
    Final3 = 3,
}

impl RepositoryFormat {
    pub const fn is_final(self) -> bool {
        matches!(self, Self::Final3)
    }
}

/// Reader profile associated with persisted repository format metadata.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ReaderContext {
    Original100,
    Previous110Draft,
    Final20261005,
}

impl ReaderContext {
    pub const fn is_historical(self) -> bool {
        !matches!(self, Self::Final20261005)
    }
}

/// Validated format/profile coordinates used to select catalogue metadata.
///
/// Callers must create this value only after checking the repository's
/// recorded format/profile. It selects metadata; it does not grant execution
/// authority or replace repository/runtime admission.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SystemFormatContext {
    format: RepositoryFormat,
    context: ReaderContext,
}

impl SystemFormatContext {
    pub const fn format(self) -> RepositoryFormat {
        self.format
    }

    pub const fn reader_context(self) -> ReaderContext {
        self.context
    }

    pub const fn is_writer(self) -> bool {
        matches!(self.format, RepositoryFormat::Final3)
    }

    pub const fn is_historical_reader(self) -> bool {
        self.context.is_historical()
    }

    /// Creates a coordinate only for a format/profile pair defined by this
    /// catalogue. The caller remains responsible for verifying persisted data.
    pub const fn from_recorded_pair(
        format: RepositoryFormat,
        context: ReaderContext,
    ) -> Option<Self> {
        let matches_recorded_pair = matches!(
            (format, context),
            (RepositoryFormat::Original100, ReaderContext::Original100)
                | (
                    RepositoryFormat::Previous110Draft,
                    ReaderContext::Previous110Draft
                )
                | (RepositoryFormat::Final3, ReaderContext::Final20261005)
        );
        if matches_recorded_pair {
            Some(Self { format, context })
        } else {
            None
        }
    }
}

/// Typed admission class for one callable contract.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SystemCallableAvailability {
    AllRecordedContexts,
    HistoricalReadersOnly,
    FinalFormat3,
    RestrictedFormats {
        original_100: bool,
        previous_110_draft: bool,
        final_3: bool,
    },
}

impl SystemCallableAvailability {
    pub const fn admits(self, context: SystemFormatContext) -> bool {
        match self {
            Self::AllRecordedContexts => true,
            Self::HistoricalReadersOnly => context.is_historical_reader(),
            Self::FinalFormat3 => context.format().is_final(),
            Self::RestrictedFormats {
                original_100,
                previous_110_draft,
                final_3,
            } => match context.format() {
                RepositoryFormat::Original100 => original_100,
                RepositoryFormat::Previous110Draft => previous_110_draft,
                RepositoryFormat::Final3 => final_3,
            },
        }
    }
}

fn callable_availability(contexts: Option<&[&str]>) -> SystemCallableAvailability {
    let Some(contexts) = contexts else {
        return SystemCallableAvailability::AllRecordedContexts;
    };
    let mut formats = [false; 3];
    for context in contexts {
        let index = match *context {
            "format-1" => 0,
            "format-2" => 1,
            "format-3" => 2,
            _ => panic!("invalid generated sys callable context: {context}"),
        };
        formats[index] = true;
    }
    match formats {
        [true, true, true] => SystemCallableAvailability::AllRecordedContexts,
        [true, true, false] => SystemCallableAvailability::HistoricalReadersOnly,
        [false, false, true] => SystemCallableAvailability::FinalFormat3,
        [original_100, previous_110_draft, final_3] => {
            SystemCallableAvailability::RestrictedFormats {
                original_100,
                previous_110_draft,
                final_3,
            }
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
struct RawApiSelection<'a> {
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

/// Immutable native API selection keyed by callable name, then selected through
/// a caller-verified recorded [`SystemFormatContext`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SystemApiSelection {
    callables: BTreeMap<String, SystemCallable>,
}

impl SystemApiSelection {
    fn from_generated() -> Self {
        let raw: RawApiSelection<'static> = serde_json::from_str(GENERATED_API_SELECTION)
            .expect("build-validated native sys API selection");
        let callables = raw
            .functions
            .into_iter()
            .map(|function| {
                let availability = callable_availability(function.contexts.as_deref());
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

/// The native typed format/context API selection.
pub fn system_api_selection() -> &'static SystemApiSelection {
    static API_SELECTION: std::sync::LazyLock<SystemApiSelection> =
        std::sync::LazyLock::new(SystemApiSelection::from_generated);
    &API_SELECTION
}

/// Context-aware descriptor selection for callers that only need metadata.
pub fn system_function_descriptor_for(
    context: SystemFormatContext,
    name: &str,
) -> Option<&'static SystemFunctionDescriptor> {
    system_api_selection().descriptor(context, name)
}

/// Context-aware callable selection for admission code.
pub fn system_callable_for(
    context: SystemFormatContext,
    name: &str,
) -> Result<&'static SystemCallable, SystemDispatchError> {
    system_api_selection().dispatch(context, name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn final_context() -> SystemFormatContext {
        SystemFormatContext::from_recorded_pair(
            RepositoryFormat::Final3,
            ReaderContext::Final20261005,
        )
        .expect("internal final selector context")
    }

    fn historical_contexts() -> (SystemFormatContext, SystemFormatContext) {
        (
            SystemFormatContext::from_recorded_pair(
                RepositoryFormat::Original100,
                ReaderContext::Original100,
            )
            .expect("explicit original reader context"),
            SystemFormatContext::from_recorded_pair(
                RepositoryFormat::Previous110Draft,
                ReaderContext::Previous110Draft,
            )
            .expect("explicit draft reader context"),
        )
    }

    #[test]
    fn recorded_format_profile_pair_is_validated() {
        let context = final_context();
        assert_eq!(context.format(), RepositoryFormat::Final3);
        assert_eq!(context.reader_context(), ReaderContext::Final20261005);
        assert!(context.is_writer());
        assert!(SystemFormatContext::from_recorded_pair(
            RepositoryFormat::Final3,
            ReaderContext::Original100
        )
        .is_none());
        assert!(SystemFormatContext::from_recorded_pair(
            RepositoryFormat::Original100,
            ReaderContext::Final20261005
        )
        .is_none());
    }

    #[test]
    fn explicit_context_sets_admit_only_the_recorded_formats() {
        let (original, draft) = historical_contexts();
        let final_context = final_context();

        let original_only = callable_availability(Some(&["format-1"]));
        assert!(original_only.admits(original));
        assert!(!original_only.admits(draft));
        assert!(!original_only.admits(final_context));

        let original_and_final = callable_availability(Some(&["format-1", "format-3"]));
        assert!(original_and_final.admits(original));
        assert!(!original_and_final.admits(draft));
        assert!(original_and_final.admits(final_context));

        let all_formats = callable_availability(Some(&["format-1", "format-2", "format-3"]));
        assert!(all_formats.admits(original));
        assert!(all_formats.admits(draft));
        assert!(all_formats.admits(final_context));
    }

    #[test]
    fn final_blob_inventory_and_legacy_reader_admission_are_fail_closed() {
        let final_context = final_context();
        let (original_context, draft_context) = historical_contexts();
        for name in [
            "sys.blob.length",
            "sys.blob.digest",
            "sys.blob.read",
            "sys.blob.verify",
            "sys.blob.from_bytes",
            "sys.blob.to_bytes",
            "sys.blob.begin",
            "sys.blob.append",
            "sys.blob.finish",
            "sys.blob.abort",
            "sys.blob.capture_file",
            "sys.blob.resource",
            "sys.blob.media_type",
            "sys.blob.suffix",
            "sys.blob.annotate",
            "sys.blob.same_content",
        ] {
            assert_eq!(
                system_callable_for(final_context, name)
                    .expect("format 3 admits final Blob metadata")
                    .availability(),
                SystemCallableAvailability::FinalFormat3
            );
            assert!(matches!(
                system_callable_for(original_context, name),
                Err(SystemDispatchError::Unavailable { .. })
            ));
            assert!(matches!(
                system_callable_for(draft_context, name),
                Err(SystemDispatchError::Unavailable { .. })
            ));
        }
        for name in [
            "sys.admin.set_storage_preference",
            "sys.admin.rewrite_storage",
        ] {
            assert_eq!(
                system_callable_for(original_context, name)
                    .expect("original reader retains retired placement metadata")
                    .availability(),
                SystemCallableAvailability::HistoricalReadersOnly
            );
            assert_eq!(
                system_callable_for(draft_context, name)
                    .expect("draft reader retains retired placement metadata")
                    .availability(),
                SystemCallableAvailability::HistoricalReadersOnly
            );
            assert!(matches!(
                system_callable_for(final_context, name),
                Err(SystemDispatchError::Unavailable { .. })
            ));
            assert!(system_function_descriptor_for(final_context, name).is_none());
            assert!(crate::system_function_descriptor(name).is_none());
        }
    }

    #[test]
    fn every_admitted_final_callable_matches_its_generated_binding() {
        let final_context = final_context();
        let abi = crate::system_provider_abi();
        let mut checked = 0usize;
        for callable in system_api_selection().available(final_context) {
            let name = callable.descriptor().name;
            let operation = abi.operation(name).unwrap_or_else(|| {
                panic!("admitted final callable `{name}` has no generated binding")
            });
            assert_eq!(
                operation.signature.source,
                callable.descriptor().signature,
                "generated binding signature for `{name}` matches the catalogue read"
            );
            assert!(
                operation
                    .effects
                    .iter()
                    .any(|effect| *effect == callable.descriptor().effect),
                "generated binding effects for `{name}` include the catalogue read effect"
            );
            checked += 1;
        }
        assert_eq!(
            checked,
            system_api_selection().available(final_context).count()
        );
        assert!(checked > 0, "final catalogue admits at least one callable");
    }

    #[test]
    fn generated_selection_exposes_final_catalogue() {
        let final_context = final_context();
        let selection = system_api_selection();
        assert_eq!(selection.callables().count(), 84);
        assert_eq!(selection.available(final_context).count(), 82);
        assert!(selection
            .descriptor(final_context, "sys.blob.length")
            .is_some());
    }
}
