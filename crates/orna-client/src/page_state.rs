//! Immutable browser-facing page state and safe resumable-session recovery.
//!
//! This module is deliberately transport- and renderer-independent. Typed
//! events transition a page model without allowing server results to replace
//! local edits, and recovery classifies session state without replaying writes.

use std::collections::BTreeMap;

/// Stable identity of an application control.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ControlId(String);

impl ControlId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Typed values exchanged with the application page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PageValue {
    Null,
    Bool(bool),
    Integer(i64),
    Text(String),
    Bytes(Vec<u8>),
}

/// One coherent application source/runtime generation pinned by a page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PageGeneration {
    source: [u8; 32],
    runtime: [u8; 32],
}

impl PageGeneration {
    #[must_use]
    pub const fn new(source: [u8; 32], runtime: [u8; 32]) -> Self {
        Self { source, runtime }
    }

    #[must_use]
    pub const fn source(&self) -> [u8; 32] { self.source }

    #[must_use]
    pub const fn runtime(&self) -> [u8; 32] { self.runtime }
}

/// Local editing state for one structured control.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Draft {
    text: String,
    focused: bool,
    selection_start: usize,
    selection_end: usize,
}

impl Draft {
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let end = text.len();
        Self { text, focused: false, selection_start: end, selection_end: end }
    }

    #[must_use]
    pub fn text(&self) -> &str { &self.text }

    #[must_use]
    pub const fn focused(&self) -> bool { self.focused }

    #[must_use]
    pub const fn selection(&self) -> (usize, usize) { (self.selection_start, self.selection_end) }
}

/// Page-local model. Server results, local drafts and feedback occupy separate
/// maps so an unrelated result cannot overwrite an in-progress edit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClientPageState {
    generation: PageGeneration,
    values: BTreeMap<ControlId, PageValue>,
    drafts: BTreeMap<ControlId, Draft>,
    results: BTreeMap<String, PageValue>,
    feedback: BTreeMap<String, String>,
    scratch: BTreeMap<String, PageValue>,
    invalid_source: bool,
}

/// A typed, explicit page-state transition. Server events carry the generation
/// they came from so stale results cannot enter an upgraded page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PageEvent {
    DraftChanged { control: ControlId, text: String, selection: (usize, usize) },
    FocusChanged { control: ControlId, focused: bool },
    ValueReceived { generation: PageGeneration, control: ControlId, value: PageValue },
    ResultReceived { generation: PageGeneration, request: String, value: PageValue },
    FeedbackReceived { generation: PageGeneration, key: String, message: String },
    GenerationUpgraded(PageGeneration),
    SourceRejected { generation: PageGeneration },
}

impl ClientPageState {
    #[must_use]
    pub fn new(generation: PageGeneration) -> Self {
        Self { generation, values: BTreeMap::new(), drafts: BTreeMap::new(), results: BTreeMap::new(), feedback: BTreeMap::new(), scratch: BTreeMap::new(), invalid_source: false }
    }

    #[must_use]
    pub const fn generation(&self) -> PageGeneration { self.generation }

    #[must_use]
    pub fn value(&self, control: &ControlId) -> Option<&PageValue> { self.values.get(control) }

    #[must_use]
    pub fn draft(&self, control: &ControlId) -> Option<&Draft> { self.drafts.get(control) }

    #[must_use]
    pub fn result(&self, request: &str) -> Option<&PageValue> { self.results.get(request) }

    #[must_use]
    pub fn feedback(&self, key: &str) -> Option<&str> { self.feedback.get(key).map(String::as_str) }

    #[must_use]
    pub fn scratch_value(&self, name: &str) -> Option<&PageValue> { self.scratch.get(name) }

    #[must_use]
    pub const fn invalid_source(&self) -> bool { self.invalid_source }
    /// Returns a new state after exactly one typed event. Generation upgrades
    /// replace both pins atomically; no action is rebound to a partial pair.
    #[must_use]
    pub fn transition(&self, event: PageEvent) -> Self {
        let mut next = self.clone();
        match event {
            PageEvent::DraftChanged { control, text, selection } => {
                let focused = next.drafts.get(&control).is_some_and(|draft| draft.focused);
                next.drafts.insert(control, Draft { text, focused, selection_start: selection.0, selection_end: selection.1 });
            }
            PageEvent::FocusChanged { control, focused } => {
                let draft = next.drafts.entry(control).or_insert_with(|| Draft::new(""));
                draft.focused = focused;
            }
            PageEvent::ValueReceived { generation, control, value } if generation == next.generation => { next.values.insert(control, value); }
            PageEvent::ResultReceived { generation, request, value } if generation == next.generation => { next.results.insert(request, value); }
            PageEvent::FeedbackReceived { generation, key, message } if generation == next.generation => { next.feedback.insert(key, message); }
            PageEvent::SourceRejected { generation } if generation == next.generation => { next.invalid_source = true; }
            PageEvent::ValueReceived { .. } | PageEvent::ResultReceived { .. } | PageEvent::FeedbackReceived { .. } | PageEvent::SourceRejected { .. } => {}
            PageEvent::GenerationUpgraded(generation) => {
                if generation != next.generation {
                    next.generation = generation;
                    next.values.clear();
                    next.results.clear();
                    next.feedback.clear();
                    next.scratch.clear();
                    next.invalid_source = false;
                }
            }
        }
        next
    }

    /// Updates console-only scratch bindings. They are never copied into or
    /// used to mutate the page's pinned environment or page values.
    #[must_use]
    pub fn with_scratch_binding(&self, name: impl Into<String>, value: PageValue) -> Self {
        let mut next = self.clone();
        next.scratch.insert(name.into(), value);
        next
    }
}

/// Whether the authenticated transport can be resumed independently of page restoration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectionRecovery { Resume, Reauthenticate }

/// How local page state should be reconstructed after transport loss.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PageRestoration { KeepPinnedPage, RestoreLocalPage }

/// Independent connection, durable-request, and page-state recovery outcomes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecoveryPlan {
    connection: ConnectionRecovery,
    request: RequestRecovery,
    page: PageRestoration,
}

impl RecoveryPlan {
    #[must_use]
    pub const fn connection(self) -> ConnectionRecovery { self.connection }

    #[must_use]
    pub const fn request(self) -> RequestRecovery { self.request }

    #[must_use]
    pub const fn page(self) -> PageRestoration { self.page }
}

/// Whether one durable request may be queried or must remain outcome-unknown.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestRecovery { QueryStatus, OutcomeUnknown, DoNotReplay }

/// Classifies recovery promises independently; no mutation transcript is replayed.
#[must_use]
pub const fn classify_recovery(session_valid: bool, durable: bool, mutation: bool) -> RecoveryPlan {
    RecoveryPlan {
        connection: if session_valid { ConnectionRecovery::Resume } else { ConnectionRecovery::Reauthenticate },
        request: if mutation && !session_valid { RequestRecovery::DoNotReplay } else if durable { RequestRecovery::QueryStatus } else { RequestRecovery::OutcomeUnknown },
        page: if session_valid { PageRestoration::KeepPinnedPage } else { PageRestoration::RestoreLocalPage },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn generation(source: u8, runtime: u8) -> PageGeneration { PageGeneration::new([source; 32], [runtime; 32]) }

    /// ORNA-APP-018: unrelated typed results preserve unsent draft text, focus, and caret.
    #[test]
    fn unrelated_typed_results_preserve_unsent_draft_focus_and_caret() {
        let control = ControlId::new("query");
        let page = ClientPageState::new(generation(1, 1))
            .transition(PageEvent::DraftChanged { control: control.clone(), text: "table.rows".into(), selection: (5, 5) })
            .transition(PageEvent::FocusChanged { control: control.clone(), focused: true })
            .transition(PageEvent::ResultReceived { generation: generation(1, 1), request: "older-run".into(), value: PageValue::Integer(7) });
        let draft = page.draft(&control).expect("draft is retained");
        assert_eq!(draft.text(), "table.rows");
        assert!(draft.focused());
        assert_eq!(draft.selection(), (5, 5));
        assert_eq!(page.result("older-run"), Some(&PageValue::Integer(7)));
    }

    /// ORNA-APP-017: scratch bindings cannot change page values or the pinned generation.
    #[test]
    fn scratch_bindings_cannot_change_page_values_or_pinned_generation() {
        let control = ControlId::new("query");
        let page = ClientPageState::new(generation(1, 2)).transition(PageEvent::ValueReceived { generation: generation(1, 2), control: control.clone(), value: PageValue::Text("page".into()) });
        let scratch = page.with_scratch_binding("query", PageValue::Text("scratch".into()));
        assert_eq!(scratch.generation(), generation(1, 2));
        assert_eq!(scratch.value(&control), Some(&PageValue::Text("page".into())));
        assert_eq!(scratch.scratch_value("query"), Some(&PageValue::Text("scratch".into())));
        assert_eq!(page.scratch_value("query"), None);
    }

    /// ORNA-APP-020: a source or runtime upgrade is atomic and keeps local recovery state.
    #[test]
    fn source_and_runtime_upgrade_is_atomic_and_preserves_local_recovery() {
        let control = ControlId::new("query");
        let rejected = ClientPageState::new(generation(1, 1))
            .transition(PageEvent::DraftChanged { control: control.clone(), text: "unsent".into(), selection: (2, 2) })
            .transition(PageEvent::SourceRejected { generation: generation(1, 1) });
        assert!(rejected.invalid_source());
        assert_eq!(rejected.draft(&control).expect("local draft survives rejection").text(), "unsent");
        let page = rejected.transition(PageEvent::GenerationUpgraded(generation(2, 3)));
        assert_eq!(page.generation(), generation(2, 3));
        assert!(!page.invalid_source());
        let stale = page.transition(PageEvent::ResultReceived { generation: generation(1, 1), request: "old-request".into(), value: PageValue::Integer(99) });
        assert_eq!(stale.result("old-request"), None);
        for (request, pair) in [("mixed-source", generation(1, 3)), ("mixed-runtime", generation(2, 1))] {
            let mixed = page.transition(PageEvent::ResultReceived { generation: pair, request: request.into(), value: PageValue::Integer(5) });
            assert_eq!(mixed.result(request), None);
        }
        let current = page.transition(PageEvent::ResultReceived { generation: generation(2, 3), request: "current-request".into(), value: PageValue::Integer(8) });
        assert_eq!(current.result("current-request"), Some(&PageValue::Integer(8)));
        assert!(stale.draft(&control).expect("local draft survives").text() == "unsent");
    }

    /// ORNA-APP-019: expired sessions restore local state and never replay a mutation.
    #[test]
    fn expired_sessions_restore_locally_without_replaying_mutations() {
        let plan = classify_recovery(false, true, true);
        assert_eq!(plan.connection(), ConnectionRecovery::Reauthenticate);
        assert_eq!(plan.request(), RequestRecovery::DoNotReplay);
        assert_eq!(plan.page(), PageRestoration::RestoreLocalPage);
        let resumed = classify_recovery(true, true, true);
        assert_eq!(resumed.connection(), ConnectionRecovery::Resume);
        assert_eq!(resumed.request(), RequestRecovery::QueryStatus);
        assert_eq!(resumed.page(), PageRestoration::KeepPinnedPage);
        assert_eq!(classify_recovery(true, false, true).request(), RequestRecovery::OutcomeUnknown);
    }
}
