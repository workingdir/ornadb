//! Atomic client-side ownership of one live Present watch.

use std::sync::atomic::{AtomicU64, Ordering};

use orna_protocol_v1::{CanonicalSnapshot, Envelope, Limits, Message, PatchList, PresentNode};

static NEXT_LIFECYCLE_NONCE: AtomicU64 = AtomicU64::new(1);

/// A complete visible presentation tree and its server-issued revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublishedPresentation {
    revision: u64,
    present: PresentNode,
    snapshot: CanonicalSnapshot,
}

impl PublishedPresentation {
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    pub const fn present(&self) -> &PresentNode {
        &self.present
    }

    pub const fn snapshot(&self) -> &CanonicalSnapshot {
        &self.snapshot
    }
}

/// The visible-state result of accepting one host presentation message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LivePresentationUpdate {
    SnapshotInstalled,
    DeltaApplied,
    ResyncRequired,
}

/// Identifies one outbound resync request generation. The token is bound to
/// the owning watch and its client-local lifecycle, then borrowed repeatedly
/// until the transport acknowledges that generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResyncRequest {
    watch: [u8; 16],
    lifecycle_nonce: u64,
    generation: u64,
}

impl ResyncRequest {
    pub const fn generation(self) -> u64 {
        self.generation
    }
}

/// A frame that cannot belong to this presentation owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LivePresentationError {
    WrongWatch,
    UnexpectedMessage,
    InvalidLimits,
}

/// Per-watch client state. It retains one complete visible tree only; it does
/// not keep delta history or any mutating request for replay.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WatchPresentation {
    watch: [u8; 16],
    lifecycle_nonce: u64,
    limits: Limits,
    published: Option<PublishedPresentation>,
    resync_intent: Option<ResyncRequest>,
    awaiting_snapshot: bool,
    next_resync_generation: u64,
}

impl WatchPresentation {
    /// Creates presentation state using the limits negotiated for this live
    /// session. Production callers must not substitute local defaults.
    pub fn new(watch: [u8; 16], limits: Limits) -> Result<Self, LivePresentationError> {
        limits
            .validate()
            .map_err(|_| LivePresentationError::InvalidLimits)?;
        Ok(Self::with_valid_limits(watch, limits))
    }

    /// Named compatibility constructor with the same explicit negotiated
    /// limits requirement as [`Self::new`].
    pub fn with_limits(watch: [u8; 16], limits: Limits) -> Result<Self, LivePresentationError> {
        Self::new(watch, limits)
    }

    fn with_valid_limits(watch: [u8; 16], limits: Limits) -> Self {
        Self {
            watch,
            lifecycle_nonce: NEXT_LIFECYCLE_NONCE.fetch_add(1, Ordering::Relaxed),
            limits,
            published: None,
            resync_intent: None,
            awaiting_snapshot: false,
            next_resync_generation: 1,
        }
    }

    pub const fn watch(&self) -> [u8; 16] {
        self.watch
    }

    pub const fn limits(&self) -> Limits {
        self.limits
    }

    pub const fn published(&self) -> Option<&PublishedPresentation> {
        self.published.as_ref()
    }

    pub const fn resync_required(&self) -> bool {
        self.awaiting_snapshot
    }

    pub const fn awaiting_snapshot(&self) -> bool {
        self.awaiting_snapshot
    }

    /// Reports that the caller should schedule the one required `resync`
    /// frame. This is deliberately non-consuming: a failed send must leave
    /// the intent available for another transport attempt.
    pub const fn take_resync_request(&self) -> Option<ResyncRequest> {
        self.resync_intent
    }

    /// Acknowledges that the transport has accepted responsibility for the
    /// supplied resync generation. It must be called only after the outbound
    /// send has succeeded; a send failure leaves the intent pending. The
    /// complete-snapshot barrier remains active until a snapshot is installed.
    pub fn acknowledge_resync_request(&mut self, request: ResyncRequest) {
        if self.resync_intent == Some(request) {
            self.resync_intent = None;
        }
    }

    /// Accepts only host Snapshot or Delta frames for this watch. The complete
    /// frame is revalidated at the negotiated boundary before any state change.
    pub fn receive(
        &mut self,
        frame: &Envelope,
    ) -> Result<LivePresentationUpdate, LivePresentationError> {
        if frame.watch != Some(self.watch) {
            return Err(LivePresentationError::WrongWatch);
        }
        if !matches!(
            &frame.message,
            Message::Snapshot { .. } | Message::Delta { .. }
        ) {
            return Err(LivePresentationError::UnexpectedMessage);
        }
        if frame.encode(self.limits).is_err() {
            return Ok(self.require_resync());
        }
        match &frame.message {
            Message::Snapshot {
                revision,
                present,
                snapshot,
            } => Ok(self.install_snapshot(*revision, present.clone(), snapshot.clone())),
            Message::Delta {
                base_revision,
                new_revision,
                patches,
                snapshot,
            } => Ok(self.apply_delta(*base_revision, *new_revision, patches, snapshot.clone())),
            _ => Err(LivePresentationError::UnexpectedMessage),
        }
    }

    /// A complete snapshot is the recovery boundary after reconnect or resync.
    pub fn install_snapshot(
        &mut self,
        revision: u64,
        present: PresentNode,
        snapshot: CanonicalSnapshot,
    ) -> LivePresentationUpdate {
        if self
            .published
            .as_ref()
            .is_some_and(|current| revision < current.revision)
        {
            return self.require_resync();
        }
        if self.published.as_ref().is_some_and(|current| {
            revision == current.revision
                && (present != current.present || snapshot != current.snapshot)
        }) {
            return self.require_resync();
        }
        if present.validate_with_limits(self.limits).is_err() {
            return self.require_resync();
        }
        self.published = Some(PublishedPresentation {
            revision,
            present,
            snapshot,
        });
        self.resync_intent = None;
        self.awaiting_snapshot = false;
        LivePresentationUpdate::SnapshotInstalled
    }

    fn apply_delta(
        &mut self,
        base_revision: u64,
        new_revision: u64,
        patches: &PatchList,
        snapshot: CanonicalSnapshot,
    ) -> LivePresentationUpdate {
        if self.awaiting_snapshot {
            return self.require_resync();
        }
        let Some(current) = self.published.as_ref() else {
            return self.require_resync();
        };
        if current.revision != base_revision || new_revision <= base_revision {
            return self.require_resync();
        }
        let Ok(present) = current.present.apply_patches(patches, self.limits) else {
            return self.require_resync();
        };
        if present.validate_with_limits(self.limits).is_err() {
            return self.require_resync();
        }
        self.published = Some(PublishedPresentation {
            revision: new_revision,
            present,
            snapshot,
        });
        LivePresentationUpdate::DeltaApplied
    }

    fn require_resync(&mut self) -> LivePresentationUpdate {
        self.awaiting_snapshot = true;
        if self.resync_intent.is_none() {
            let request = ResyncRequest {
                watch: self.watch,
                lifecycle_nonce: self.lifecycle_nonce,
                generation: self.next_resync_generation,
            };
            self.next_resync_generation = self.next_resync_generation.saturating_add(1);
            self.resync_intent = Some(request);
        }
        LivePresentationUpdate::ResyncRequired
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orna_protocol_v1::{Error as ProtocolError, Limits};

    fn default_presentation(watch: [u8; 16]) -> WatchPresentation {
        WatchPresentation::new(watch, Limits::default()).expect("default limits are valid")
    }

    #[test]
    fn construction_requires_valid_explicit_negotiated_limits() {
        let limits = Limits {
            max_message_bytes: 0,
            ..Limits::default()
        };
        assert_eq!(
            WatchPresentation::new([7; 16], limits),
            Err(LivePresentationError::InvalidLimits)
        );
    }

    fn try_frame(code: u8, watch: [u8; 16], body: Vec<u8>) -> Result<Envelope, ProtocolError> {
        let mut bytes = vec![0xa5, 0x00, 0x01, 0x01, code, 0x02, 0xf6, 0x03, 0x50];
        bytes.extend(watch);
        bytes.extend([0x04]);
        bytes.extend(body);
        Envelope::decode(&bytes, Limits::default())
    }

    fn frame(code: u8, watch: [u8; 16], body: Vec<u8>) -> Envelope {
        try_frame(code, watch, body).unwrap()
    }

    fn snapshot(revision: u8, property: &str) -> Vec<u8> {
        snapshot_tree(revision, present(property))
    }

    fn snapshot_tree(revision: u8, tree: Vec<u8>) -> Vec<u8> {
        snapshot_tree_with_snapshot(revision, tree, pinned_snapshot())
    }

    fn snapshot_tree_with_snapshot(revision: u8, tree: Vec<u8>, snapshot: Vec<u8>) -> Vec<u8> {
        let mut body = vec![0xa3, 0x00, revision, 0x01];
        body.extend(tree);
        body.extend([0x02]);
        body.extend(snapshot);
        body
    }

    fn delta(base: u8, next: u8, operations: Vec<Vec<u8>>) -> Vec<u8> {
        let mut body = vec![0xa4, 0x00, base, 0x01, next, 0x02];
        body.extend(array(operations));
        body.extend([0x03]);
        body.extend(pinned_snapshot());
        body
    }

    fn present(property: &str) -> Vec<u8> {
        present_node(vec![0xf6], vec![(text("text"), text(property))], vec![])
    }

    fn present_node(
        key: Vec<u8>,
        properties: Vec<(Vec<u8>, Vec<u8>)>,
        children: Vec<Vec<u8>>,
    ) -> Vec<u8> {
        let mut node = vec![0xd9, 0xea, 0x6c, 0x84, 0x64];
        node.extend(b"text");
        node.extend(key);
        node.extend(map(properties));
        node.extend(array(children));
        node
    }

    fn text(value: &str) -> Vec<u8> {
        assert!(value.len() < 24);
        let mut encoded = vec![0x60 + value.len() as u8];
        encoded.extend(value.as_bytes());
        encoded
    }

    fn uint(value: u8) -> Vec<u8> {
        vec![value]
    }

    fn map(entries: Vec<(Vec<u8>, Vec<u8>)>) -> Vec<u8> {
        assert!(entries.len() < 24);
        let mut encoded = vec![0xa0 + entries.len() as u8];
        for (key, value) in entries {
            encoded.extend(key);
            encoded.extend(value);
        }
        encoded
    }

    fn uuid(value: u8) -> Vec<u8> {
        let mut encoded = vec![0xd8, 0x25, 0x50];
        encoded.extend([value; 16]);
        encoded
    }

    fn bytes(value: usize) -> Vec<u8> {
        assert!(value < 256);
        let mut encoded = vec![0x58, value as u8];
        encoded.extend(vec![3; value]);
        encoded
    }

    fn malformed_uuid(value: u8) -> Vec<u8> {
        let mut encoded = vec![0xd8, 0x25, 0x4f];
        encoded.extend([value; 15]);
        encoded
    }

    fn explicit_key(value: &str) -> Vec<u8> {
        array(vec![uint(3), text(value)])
    }

    fn relation_key(table: u8, primary_key: &str) -> Vec<u8> {
        array(vec![uint(1), uuid(table), text(primary_key)])
    }

    fn property_path(value: &str) -> Vec<u8> {
        path(vec![array(vec![uint(0), text(value)])])
    }

    fn record_field_path(value: &str) -> Vec<u8> {
        path(vec![array(vec![uint(0), text(value)])])
    }

    fn nested_record_field_path(field: &str, property: &str) -> Vec<u8> {
        path(vec![
            array(vec![uint(0), text(field)]),
            array(vec![uint(0), text(property)]),
        ])
    }

    fn positional_path(index: u8) -> Vec<u8> {
        path(vec![array(vec![uint(2), uint(index)])])
    }

    fn keyed_component(value: &str) -> Vec<u8> {
        array(vec![uint(3), text(value)])
    }

    fn relation_component(table: u8, primary_key: &str) -> Vec<u8> {
        array(vec![uint(1), uuid(table), text(primary_key)])
    }

    fn nested_keyed_property_path(key: &str, property: &str) -> Vec<u8> {
        path(vec![
            keyed_component(key),
            array(vec![uint(0), text(property)]),
        ])
    }

    fn nested_relation_property_path(table: u8, key: &str, property: &str) -> Vec<u8> {
        path(vec![
            relation_component(table, key),
            array(vec![uint(0), text(property)]),
        ])
    }

    fn nested_positional_property_path(index: u8, property: &str) -> Vec<u8> {
        path(vec![
            array(vec![uint(2), uint(index)]),
            array(vec![uint(0), text(property)]),
        ])
    }

    fn path(components: Vec<Vec<u8>>) -> Vec<u8> {
        array(components)
    }

    fn add(path: Vec<u8>, value: Vec<u8>) -> Vec<u8> {
        array(vec![uint(0), path, value])
    }

    fn remove(path: Vec<u8>) -> Vec<u8> {
        array(vec![uint(1), path])
    }

    fn replace(path: Vec<u8>, value: Vec<u8>) -> Vec<u8> {
        array(vec![uint(2), path, value])
    }

    fn move_node(from: Vec<u8>, to: Vec<u8>) -> Vec<u8> {
        array(vec![uint(3), from, to])
    }

    fn pinned_snapshot() -> Vec<u8> {
        pinned_snapshot_with(2)
    }

    fn pinned_snapshot_with(digest: u8) -> Vec<u8> {
        let mut snapshot = vec![0x84, 0x01, 0xd8, 0x25, 0x50];
        snapshot.extend([1; 16]);
        snapshot.extend([0x66]);
        snapshot.extend(b"sha256");
        snapshot.push(0x58);
        snapshot.push(32);
        snapshot.extend([digest; 32]);
        snapshot
    }

    fn array(values: Vec<Vec<u8>>) -> Vec<u8> {
        let mut out = vec![0x80 + values.len() as u8];
        for value in values {
            out.extend(value);
        }
        out
    }

    fn replace_text(value: &str) -> Vec<u8> {
        replace(property_path("text"), text(value))
    }

    fn malformed_after_replace() -> Vec<u8> {
        vec![0x82, 0x01, 0x80]
    }

    fn snapshot_present(frame: &Envelope) -> PresentNode {
        let Message::Snapshot { present, .. } = &frame.message else {
            panic!("test frame must be a snapshot");
        };
        present.clone()
    }

    fn child(key: Vec<u8>, label: &str) -> Vec<u8> {
        present_node(key, vec![(text("label"), text(label))], vec![])
    }

    #[test]
    fn sequential_deltas_publish_each_complete_revision() {
        let watch = [7; 16];
        let mut state = default_presentation(watch);
        assert_eq!(
            state
                .receive(&frame(16, watch, snapshot(0, "zero")))
                .unwrap(),
            LivePresentationUpdate::SnapshotInstalled
        );
        assert_eq!(
            state
                .receive(&frame(17, watch, delta(0, 1, vec![replace_text("one")])))
                .unwrap(),
            LivePresentationUpdate::DeltaApplied
        );
        assert_eq!(
            state
                .receive(&frame(17, watch, delta(1, 2, vec![replace_text("two")])))
                .unwrap(),
            LivePresentationUpdate::DeltaApplied
        );
        let expected = frame(16, watch, snapshot(2, "two"));
        assert_eq!(state.published().unwrap().revision(), 2);
        assert_eq!(
            state.published().unwrap().present(),
            &snapshot_present(&expected)
        );
    }

    #[test]
    fn delta_before_initial_snapshot_is_discarded_and_requests_resync() {
        let watch = [7; 16];
        let mut state = default_presentation(watch);

        assert_eq!(
            state
                .receive(&frame(17, watch, delta(0, 1, vec![replace_text("one")])))
                .unwrap(),
            LivePresentationUpdate::ResyncRequired
        );
        assert!(state.published().is_none());
        assert!(state.awaiting_snapshot());
        assert!(state.take_resync_request().is_some());
    }

    #[test]
    fn missing_base_preserves_visible_tree_and_requests_resync() {
        let watch = [7; 16];
        let mut state = default_presentation(watch);
        state
            .receive(&frame(16, watch, snapshot(0, "zero")))
            .unwrap();
        let visible = state.published().cloned();
        assert_eq!(
            state
                .receive(&frame(17, watch, delta(1, 2, vec![replace_text("two")])))
                .unwrap(),
            LivePresentationUpdate::ResyncRequired
        );
        assert_eq!(state.published().cloned(), visible);
        assert!(state.take_resync_request().is_some());
    }

    #[test]
    fn malformed_or_mid_list_failure_is_atomic() {
        let watch = [7; 16];
        let mut state = default_presentation(watch);
        state
            .receive(&frame(16, watch, snapshot(0, "zero")))
            .unwrap();
        let visible = state.published().cloned();
        assert_eq!(
            state
                .receive(&frame(
                    17,
                    watch,
                    delta(0, 1, vec![replace_text("one"), malformed_after_replace()])
                ))
                .unwrap(),
            LivePresentationUpdate::ResyncRequired
        );
        assert_eq!(state.published().cloned(), visible);
        assert!(state.resync_required());
    }

    #[test]
    fn reconnect_snapshot_replaces_the_complete_tree() {
        let watch = [7; 16];
        let mut state = default_presentation(watch);
        state
            .receive(&frame(16, watch, snapshot(0, "old")))
            .unwrap();
        state
            .receive(&frame(17, watch, delta(2, 3, vec![replace_text("lost")])))
            .unwrap();
        assert_eq!(
            state
                .receive(&frame(16, watch, snapshot(3, "current")))
                .unwrap(),
            LivePresentationUpdate::SnapshotInstalled
        );
        assert_eq!(state.published().unwrap().revision(), 3);
        assert_eq!(
            state.published().unwrap().present(),
            &snapshot_present(&frame(16, watch, snapshot(3, "current")))
        );
        assert!(!state.resync_required());
    }

    #[test]
    fn latest_snapshot_coalesces_without_retaining_deltas() {
        let watch = [7; 16];
        let mut state = default_presentation(watch);
        state
            .receive(&frame(16, watch, snapshot(0, "old")))
            .unwrap();
        assert_eq!(
            state
                .receive(&frame(16, watch, snapshot(4, "latest")))
                .unwrap(),
            LivePresentationUpdate::SnapshotInstalled
        );
        assert_eq!(state.published().unwrap().revision(), 4);
        assert_eq!(
            state.published().unwrap().present(),
            &snapshot_present(&frame(16, watch, snapshot(4, "latest")))
        );
        assert!(!state.resync_required());
    }

    #[test]
    fn equal_revision_snapshot_requires_matching_content_and_pin() {
        let watch = [7; 16];
        let mut state = default_presentation(watch);
        let matching = frame(16, watch, snapshot(2, "current"));
        assert_eq!(
            state.receive(&matching).unwrap(),
            LivePresentationUpdate::SnapshotInstalled
        );
        let visible = state.published().cloned();
        assert_eq!(
            state
                .receive(&frame(16, watch, snapshot(2, "current")))
                .unwrap(),
            LivePresentationUpdate::SnapshotInstalled
        );
        assert_eq!(state.published().cloned(), visible);

        assert_eq!(
            state
                .receive(&frame(16, watch, snapshot(2, "conflicting-content")))
                .unwrap(),
            LivePresentationUpdate::ResyncRequired
        );
        assert_eq!(state.published().cloned(), visible);
        assert_eq!(
            state.receive(&matching).unwrap(),
            LivePresentationUpdate::SnapshotInstalled
        );
        assert!(!state.awaiting_snapshot());

        let conflicting_pin = frame(
            16,
            watch,
            snapshot_tree_with_snapshot(2, present("current"), pinned_snapshot_with(3)),
        );
        assert_eq!(
            state.receive(&conflicting_pin).unwrap(),
            LivePresentationUpdate::ResyncRequired
        );
        assert_eq!(state.published().cloned(), visible);
        assert!(state.take_resync_request().is_some());
    }

    #[test]
    fn property_additions_are_inserted_by_encoded_key_order() {
        let watch = [7; 16];
        let initial = present_node(vec![0xf6], vec![(text("z"), text("last"))], vec![]);
        let expected = present_node(
            vec![0xf6],
            vec![(text("a"), text("first")), (text("z"), text("last"))],
            vec![],
        );
        let mut state = default_presentation(watch);
        state
            .receive(&frame(16, watch, snapshot_tree(0, initial)))
            .unwrap();
        assert_eq!(
            state
                .receive(&frame(
                    17,
                    watch,
                    delta(0, 1, vec![add(property_path("a"), text("first"))])
                ))
                .unwrap(),
            LivePresentationUpdate::DeltaApplied
        );
        assert_eq!(
            state.published().unwrap().present(),
            &snapshot_present(&frame(16, watch, snapshot_tree(1, expected)))
        );
    }

    #[test]
    fn root_replace_publishes_a_complete_replacement() {
        let watch = [7; 16];
        let mut state = default_presentation(watch);
        state
            .receive(&frame(16, watch, snapshot(0, "old")))
            .unwrap();
        let replacement = present("new");
        assert_eq!(
            state
                .receive(&frame(
                    17,
                    watch,
                    delta(0, 1, vec![replace(vec![0x80], replacement.clone())])
                ))
                .unwrap(),
            LivePresentationUpdate::DeltaApplied
        );
        assert_eq!(
            state.published().unwrap().present(),
            &snapshot_present(&frame(16, watch, snapshot_tree(1, replacement)))
        );
    }

    #[test]
    fn add_and_remove_are_ordered_and_use_the_temporary_tree() {
        let watch = [7; 16];
        let initial = present_node(
            vec![0xf6],
            vec![],
            vec![child(vec![0xf6], "a"), child(vec![0xf6], "b")],
        );
        let expected = present_node(
            vec![0xf6],
            vec![],
            vec![child(vec![0xf6], "c"), child(vec![0xf6], "b")],
        );
        let mut state = default_presentation(watch);
        state
            .receive(&frame(16, watch, snapshot_tree(0, initial)))
            .unwrap();
        assert_eq!(
            state
                .receive(&frame(
                    17,
                    watch,
                    delta(
                        0,
                        1,
                        vec![
                            add(positional_path(1), child(vec![0xf6], "c")),
                            remove(positional_path(0)),
                        ]
                    )
                ))
                .unwrap(),
            LivePresentationUpdate::DeltaApplied
        );
        assert_eq!(
            state.published().unwrap().present(),
            &snapshot_present(&frame(16, watch, snapshot_tree(1, expected)))
        );
    }

    #[test]
    fn move_removes_before_resolving_destination_and_rejects_descendants() {
        let watch = [7; 16];
        let initial = present_node(
            vec![0xf6],
            vec![],
            vec![
                child(vec![0xf6], "a"),
                child(vec![0xf6], "b"),
                child(vec![0xf6], "c"),
            ],
        );
        let expected = present_node(
            vec![0xf6],
            vec![],
            vec![
                child(vec![0xf6], "b"),
                child(vec![0xf6], "a"),
                child(vec![0xf6], "c"),
            ],
        );
        let mut state = default_presentation(watch);
        state
            .receive(&frame(16, watch, snapshot_tree(0, initial)))
            .unwrap();
        assert_eq!(
            state
                .receive(&frame(
                    17,
                    watch,
                    delta(
                        0,
                        1,
                        vec![move_node(positional_path(0), positional_path(1))]
                    )
                ))
                .unwrap(),
            LivePresentationUpdate::DeltaApplied
        );
        let visible = state.published().cloned();
        assert_eq!(
            state
                .receive(&frame(
                    17,
                    watch,
                    delta(
                        1,
                        2,
                        vec![move_node(
                            positional_path(0),
                            path(vec![
                                array(vec![uint(2), uint(0)]),
                                array(vec![uint(2), uint(0)]),
                            ])
                        )]
                    )
                ))
                .unwrap(),
            LivePresentationUpdate::ResyncRequired
        );
        assert_eq!(state.published().cloned(), visible);
        assert_eq!(
            state.published().unwrap().present(),
            &snapshot_present(&frame(16, watch, snapshot_tree(1, expected)))
        );
    }

    #[test]
    fn keyed_relation_and_positional_paths_select_their_declared_children() {
        let watch = [7; 16];
        let keyed = explicit_key("keyed");
        let relation = relation_key(9, "row");
        let initial = present_node(
            vec![0xf6],
            vec![],
            vec![
                child(keyed.clone(), "old-keyed"),
                child(relation.clone(), "old-relation"),
                child(vec![0xf6], "old-positional"),
            ],
        );
        let expected = present_node(
            vec![0xf6],
            vec![],
            vec![
                child(keyed.clone(), "new-keyed"),
                child(relation.clone(), "new-relation"),
                child(vec![0xf6], "new-positional"),
            ],
        );
        let mut state = default_presentation(watch);
        state
            .receive(&frame(16, watch, snapshot_tree(0, initial)))
            .unwrap();
        let operations = vec![
            replace(
                nested_keyed_property_path("keyed", "label"),
                text("new-keyed"),
            ),
            replace(
                nested_relation_property_path(9, "row", "label"),
                text("new-relation"),
            ),
            replace(
                nested_positional_property_path(2, "label"),
                text("new-positional"),
            ),
        ];
        assert_eq!(
            state
                .receive(&frame(17, watch, delta(0, 1, operations)))
                .unwrap(),
            LivePresentationUpdate::DeltaApplied
        );
        assert_eq!(
            state.published().unwrap().present(),
            &snapshot_present(&frame(16, watch, snapshot_tree(1, expected)))
        );
    }

    #[test]
    fn record_field_paths_resolve_children_contextually_without_breaking_properties() {
        let watch = [7; 16];
        let first = array(vec![uint(0), text("first")]);
        let second = array(vec![uint(0), text("second")]);
        let third = array(vec![uint(0), text("third")]);
        let initial = present_node(
            vec![0xf6],
            vec![(text("ordinary"), text("property"))],
            vec![child(first.clone(), "a"), child(second, "b")],
        );
        let expected = present_node(
            vec![0xf6],
            vec![(text("ordinary"), text("property-new"))],
            vec![child(third, "c"), child(first, "a-new")],
        );
        let mut state = default_presentation(watch);
        state
            .receive(&frame(16, watch, snapshot_tree(0, initial)))
            .unwrap();
        assert_eq!(
            state
                .receive(&frame(
                    17,
                    watch,
                    delta(
                        0,
                        1,
                        vec![
                            replace(nested_record_field_path("first", "label"), text("a-new"),),
                            add(
                                record_field_path("third"),
                                child(array(vec![uint(0), text("third")]), "c"),
                            ),
                            move_node(record_field_path("third"), positional_path(0)),
                            remove(record_field_path("second")),
                            replace(property_path("ordinary"), text("property-new")),
                        ],
                    ),
                ))
                .unwrap(),
            LivePresentationUpdate::DeltaApplied
        );
        assert_eq!(
            state.published().unwrap().present(),
            &snapshot_present(&frame(16, watch, snapshot_tree(1, expected)))
        );
    }

    #[test]
    fn record_field_move_cannot_change_the_child_stable_identity() {
        let watch = [7; 16];
        let initial = present_node(
            vec![0xf6],
            vec![],
            vec![child(array(vec![uint(0), text("first")]), "a")],
        );
        let mut state = default_presentation(watch);
        state
            .receive(&frame(16, watch, snapshot_tree(0, initial)))
            .unwrap();
        let visible = state.published().cloned();
        assert_eq!(
            state
                .receive(&frame(
                    17,
                    watch,
                    delta(
                        0,
                        1,
                        vec![move_node(
                            record_field_path("first"),
                            record_field_path("second"),
                        )],
                    ),
                ))
                .unwrap(),
            LivePresentationUpdate::ResyncRequired
        );
        assert_eq!(state.published().cloned(), visible);
    }

    #[test]
    fn duplicate_stable_keys_fail_atomically() {
        let watch = [7; 16];
        let initial = present_node(
            vec![0xf6],
            vec![],
            vec![child(explicit_key("same"), "first")],
        );
        let mut state = default_presentation(watch);
        state
            .receive(&frame(16, watch, snapshot_tree(0, initial)))
            .unwrap();
        let visible = state.published().cloned();
        assert_eq!(
            state
                .receive(&frame(
                    17,
                    watch,
                    delta(
                        0,
                        1,
                        vec![add(
                            positional_path(1),
                            child(explicit_key("same"), "duplicate")
                        )]
                    )
                ))
                .unwrap(),
            LivePresentationUpdate::ResyncRequired
        );
        assert_eq!(state.published().cloned(), visible);
    }

    #[test]
    fn duplicate_then_remove_is_rejected_at_the_first_invalid_intermediate_tree() {
        let watch = [7; 16];
        let initial = present_node(
            vec![0xf6],
            vec![],
            vec![child(explicit_key("same"), "first")],
        );
        let mut state = default_presentation(watch);
        state
            .receive(&frame(16, watch, snapshot_tree(0, initial)))
            .unwrap();
        let visible = state.published().cloned();
        assert_eq!(
            state
                .receive(&frame(
                    17,
                    watch,
                    delta(
                        0,
                        1,
                        vec![
                            add(positional_path(1), child(explicit_key("same"), "duplicate")),
                            remove(positional_path(1)),
                        ]
                    )
                ))
                .unwrap(),
            LivePresentationUpdate::ResyncRequired
        );
        assert_eq!(state.published().cloned(), visible);
    }

    #[test]
    fn malformed_property_uuid_path_is_rejected_before_client_mutation() {
        let watch = [7; 16];
        let mut state = default_presentation(watch);
        state
            .receive(&frame(16, watch, snapshot(0, "current")))
            .unwrap();
        let visible = state.clone();
        let malformed_path = path(vec![array(vec![uint(0), malformed_uuid(8)])]);
        let malformed = try_frame(
            17,
            watch,
            delta(0, 1, vec![replace(malformed_path, text("bad"))]),
        );
        assert!(malformed.is_err());
        assert_eq!(state, visible);
    }

    #[test]
    fn negotiated_collection_limits_reject_repeated_small_adds_atomically() {
        let watch = [7; 16];
        let limits = Limits {
            max_message_bytes: 1024,
            max_depth: 64,
            max_nodes: 100,
            max_collection_items: 5,
        };
        let mut state = WatchPresentation::with_limits(watch, limits).unwrap();
        state
            .receive(&frame(
                16,
                watch,
                snapshot_tree(0, present_node(vec![0xf6], vec![], vec![])),
            ))
            .unwrap();
        for (base, next, label) in [
            (0_u8, 1_u8, "one"),
            (1_u8, 2_u8, "two"),
            (2_u8, 3_u8, "three"),
            (3_u8, 4_u8, "four"),
            (4_u8, 5_u8, "five"),
        ] {
            assert_eq!(
                state
                    .receive(&frame(
                        17,
                        watch,
                        delta(
                            base,
                            next,
                            vec![add(positional_path(base), child(vec![0xf6], label))]
                        ),
                    ))
                    .unwrap(),
                LivePresentationUpdate::DeltaApplied
            );
        }
        let visible = state.published().cloned();
        assert_eq!(
            state
                .receive(&frame(
                    17,
                    watch,
                    delta(
                        5,
                        6,
                        vec![add(positional_path(5), child(vec![0xf6], "six"))]
                    ),
                ))
                .unwrap(),
            LivePresentationUpdate::ResyncRequired
        );
        assert_eq!(state.published().cloned(), visible);
    }

    #[test]
    fn transient_over_limit_add_then_remove_is_rejected_atomically() {
        let watch = [7; 16];
        let limits = Limits {
            max_message_bytes: 2048,
            max_depth: 64,
            max_nodes: 1_000,
            max_collection_items: 5,
        };
        let initial = present_node(
            vec![0xf6],
            vec![],
            vec![
                child(vec![0xf6], "one"),
                child(vec![0xf6], "two"),
                child(vec![0xf6], "three"),
                child(vec![0xf6], "four"),
                child(vec![0xf6], "five"),
            ],
        );
        let mut state = WatchPresentation::with_limits(watch, limits).unwrap();
        state
            .receive(&frame(16, watch, snapshot_tree(0, initial)))
            .unwrap();
        let visible = state.published().cloned();
        assert_eq!(
            state
                .receive(&frame(
                    17,
                    watch,
                    delta(
                        0,
                        1,
                        vec![
                            add(positional_path(5), child(vec![0xf6], "transient")),
                            remove(positional_path(5)),
                        ],
                    ),
                ))
                .unwrap(),
            LivePresentationUpdate::ResyncRequired
        );
        assert_eq!(state.published().cloned(), visible);
        assert!(state.take_resync_request().is_some());
    }

    #[test]
    fn negotiated_receive_boundary_rejects_an_oversized_frame_before_patching() {
        let watch = [7; 16];
        let initial = frame(16, watch, snapshot(0, "current"));
        let initial_bytes = initial.encode(Limits::default()).unwrap().len();
        let limits = Limits {
            max_message_bytes: initial_bytes + 1,
            max_depth: 64,
            max_nodes: 1_000,
            max_collection_items: 100,
        };
        let mut state = WatchPresentation::with_limits(watch, limits).unwrap();
        assert_eq!(
            state.receive(&initial).unwrap(),
            LivePresentationUpdate::SnapshotInstalled
        );
        let visible = state.published().cloned();
        let oversized = frame(
            17,
            watch,
            delta(0, 1, vec![replace(property_path("text"), bytes(255))]),
        );
        assert!(oversized.encode(Limits::default()).unwrap().len() > limits.max_message_bytes);
        assert_eq!(
            state.receive(&oversized).unwrap(),
            LivePresentationUpdate::ResyncRequired
        );
        assert_eq!(state.published().cloned(), visible);
        assert!(state.resync_required());
    }

    #[test]
    fn lower_snapshot_revision_is_rejected_without_mutating_state() {
        let watch = [7; 16];
        let mut state = default_presentation(watch);
        state
            .receive(&frame(16, watch, snapshot(3, "current")))
            .unwrap();
        let visible = state.published().cloned();
        assert_eq!(
            state
                .receive(&frame(16, watch, snapshot(2, "stale")))
                .unwrap(),
            LivePresentationUpdate::ResyncRequired
        );
        assert_eq!(state.published().cloned(), visible);
        assert!(state.resync_required());
    }

    #[test]
    fn wrong_watch_is_rejected_without_mutating_state() {
        let watch = [7; 16];
        let mut state = default_presentation(watch);
        state
            .receive(&frame(16, watch, snapshot(0, "current")))
            .unwrap();
        let visible = state.clone();
        assert_eq!(
            state.receive(&frame(16, [8; 16], snapshot(1, "foreign"))),
            Err(LivePresentationError::WrongWatch)
        );
        assert_eq!(state, visible);
    }

    #[test]
    fn resync_intent_repeats_until_transport_acknowledges_it() {
        let watch = [7; 16];
        let mut state = default_presentation(watch);
        state
            .receive(&frame(16, watch, snapshot(0, "current")))
            .unwrap();
        state
            .receive(&frame(17, watch, delta(4, 5, vec![replace_text("stale")])))
            .unwrap();
        assert!(state.take_resync_request().is_some());
        assert!(state.take_resync_request().is_some());
        let visible = state.published().cloned();
        assert_eq!(
            state
                .receive(&frame(17, watch, delta(0, 1, vec![replace_text("stale")])))
                .unwrap(),
            LivePresentationUpdate::ResyncRequired
        );
        assert_eq!(state.published().cloned(), visible);
        // Simulate a transport send failure: without acknowledgement, the
        // next scheduling pass must still observe the same intent.
        assert!(state.take_resync_request().is_some());
        let first = state.take_resync_request().unwrap();
        state.acknowledge_resync_request(first);
        assert!(state.take_resync_request().is_none());
        state
            .receive(&frame(17, watch, delta(9, 10, vec![replace_text("again")])))
            .unwrap();
        assert!(state.take_resync_request().is_some());
    }

    #[test]
    fn resync_acknowledgement_is_bound_to_the_original_watch() {
        let first_watch = [7; 16];
        let second_watch = [8; 16];
        let mut first = default_presentation(first_watch);
        first
            .receive(&frame(16, first_watch, snapshot(0, "first")))
            .unwrap();
        first
            .receive(&frame(
                17,
                first_watch,
                delta(4, 5, vec![replace_text("missing-base")]),
            ))
            .unwrap();
        let first_request = first.take_resync_request().unwrap();

        let mut second = default_presentation(second_watch);
        second
            .receive(&frame(16, second_watch, snapshot(0, "second")))
            .unwrap();
        second
            .receive(&frame(
                17,
                second_watch,
                delta(4, 5, vec![replace_text("missing-base")]),
            ))
            .unwrap();
        let second_request = second.take_resync_request().unwrap();
        assert_ne!(first_request, second_request);

        second.acknowledge_resync_request(first_request);
        assert_eq!(second.take_resync_request(), Some(second_request));
        first.acknowledge_resync_request(second_request);
        assert_eq!(first.take_resync_request(), Some(first_request));
    }

    #[test]
    fn resync_acknowledgement_is_bound_to_a_recreated_state_lifecycle() {
        let watch = [7; 16];
        let mut old_state = default_presentation(watch);
        old_state
            .receive(&frame(16, watch, snapshot(0, "old")))
            .unwrap();
        old_state
            .receive(&frame(17, watch, delta(4, 5, vec![replace_text("stale")])))
            .unwrap();
        let old_request = old_state.take_resync_request().unwrap();

        let mut recreated_state = default_presentation(watch);
        recreated_state
            .receive(&frame(16, watch, snapshot(0, "new")))
            .unwrap();
        recreated_state
            .receive(&frame(17, watch, delta(4, 5, vec![replace_text("stale")])))
            .unwrap();
        let new_request = recreated_state.take_resync_request().unwrap();
        assert_ne!(old_request, new_request);

        recreated_state.acknowledge_resync_request(old_request);
        assert_eq!(recreated_state.take_resync_request(), Some(new_request));
        assert!(recreated_state.awaiting_snapshot());
    }

    #[test]
    fn acknowledging_resync_intent_does_not_release_snapshot_barrier() {
        let watch = [7; 16];
        let mut state = default_presentation(watch);
        state
            .receive(&frame(16, watch, snapshot(0, "current")))
            .unwrap();
        state
            .receive(&frame(
                17,
                watch,
                delta(4, 5, vec![replace_text("missing-base")]),
            ))
            .unwrap();
        let request = state.take_resync_request().unwrap();
        state.acknowledge_resync_request(request);
        assert!(state.awaiting_snapshot());
        assert!(state.take_resync_request().is_none());
        let visible = state.published().cloned();
        assert_eq!(
            state
                .receive(&frame(
                    17,
                    watch,
                    delta(0, 1, vec![replace_text("blocked")])
                ))
                .unwrap(),
            LivePresentationUpdate::ResyncRequired
        );
        assert_eq!(state.published().cloned(), visible);
        assert!(state.take_resync_request().is_some());
    }

    #[test]
    fn stale_resync_acknowledgement_cannot_clear_a_newer_generation() {
        let watch = [7; 16];
        let mut state = default_presentation(watch);
        state
            .receive(&frame(16, watch, snapshot(0, "current")))
            .unwrap();
        state
            .receive(&frame(
                17,
                watch,
                delta(4, 5, vec![replace_text("missing-base")]),
            ))
            .unwrap();
        let first = state.take_resync_request().unwrap();
        state.acknowledge_resync_request(first);
        state
            .receive(&frame(
                17,
                watch,
                delta(0, 1, vec![replace_text("blocked")]),
            ))
            .unwrap();
        let second = state.take_resync_request().unwrap();
        assert_ne!(first.generation(), second.generation());
        state.acknowledge_resync_request(first);
        assert_eq!(state.take_resync_request(), Some(second));
        state.acknowledge_resync_request(second);
        assert!(state.take_resync_request().is_none());
        state
            .receive(&frame(16, watch, snapshot(1, "recovered")))
            .unwrap();
        assert!(!state.awaiting_snapshot());
    }
}
