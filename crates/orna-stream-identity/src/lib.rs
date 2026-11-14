//! Typed stream identity components for Orna 1.0.
//!
//! These types name an ordered provider stream, one delivery on it, and the
//! opaque checkpoint positions that merge across branches. They carry no
//! checkpoint administration, so sibling crates can reuse them without
//! depending on the stream engine in `orna-stream`.

use std::cmp::Ordering;
use std::fmt;

/// A non-empty, separator-free typed identity component.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Component(String);

impl Component {
    pub fn new(value: impl Into<String>) -> Result<Self, IdentityError> {
        let value = value.into();
        if value.is_empty() || value.contains(['|', '\n', '\r']) {
            return Err(IdentityError::InvalidComponent);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Identity validation is deliberately small and stable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdentityError {
    InvalidComponent,
}

impl fmt::Display for IdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid identity component")
    }
}

impl std::error::Error for IdentityError {}

/// The canonical consumer identity is composed from typed, semantic components.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ConsumerIdentity {
    pub principal: Component,
    pub root: Component,
    pub function: Component,
    pub binding: Component,
}

impl ConsumerIdentity {
    pub fn canonical(&self) -> String {
        format!(
            "consumer/v1|{}|{}|{}|{}",
            self.principal.as_str(),
            self.root.as_str(),
            self.function.as_str(),
            self.binding.as_str()
        )
    }
}

/// Identifies one provider delivery and its explicit checkpoint transition.
#[derive(Clone, Debug)]
pub struct DeliveryIdentity {
    pub consumer: ConsumerIdentity,
    pub source_format: Component,
    pub source: Component,
    pub partition_format: Component,
    /// The optional provider partition. `None` is the canonical unpartitioned
    /// stream identity, not an empty partition name.
    pub partition: Option<Component>,
    pub position_format: Component,
    /// The opaque provider position that identifies this delivery.
    pub position: Position,
    /// The provider-selected checkpoint position after this delivery commits.
    pub successor: Position,
}

impl PartialEq for DeliveryIdentity {
    fn eq(&self, other: &Self) -> bool {
        self.consumer == other.consumer
            && self.source_format == other.source_format
            && self.source == other.source
            && self.partition_format == other.partition_format
            && self.partition == other.partition
            && self.position_format == other.position_format
            && self.position == other.position
    }
}

impl Eq for DeliveryIdentity {}

impl PartialOrd for DeliveryIdentity {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for DeliveryIdentity {
    fn cmp(&self, other: &Self) -> Ordering {
        (
            &self.consumer,
            &self.source_format,
            &self.source,
            &self.partition_format,
            &self.partition,
            &self.position_format,
            &self.position.token,
        )
            .cmp(&(
                &other.consumer,
                &other.source_format,
                &other.source,
                &other.partition_format,
                &other.partition,
                &other.position_format,
                &other.position.token,
            ))
    }
}

impl DeliveryIdentity {
    pub fn checkpoint_key(&self) -> CheckpointKey {
        CheckpointKey {
            consumer: self.consumer.clone(),
            source_format: self.source_format.clone(),
            source: self.source.clone(),
            partition_format: self.partition_format.clone(),
            partition: self.partition.clone(),
            position_format: self.position_format.clone(),
        }
    }

    /// Whether this delivery belongs to the ordered stream `key`, compared
    /// field by field so scanning many deliveries builds no keys.
    pub fn is_on_stream(&self, key: &CheckpointKey) -> bool {
        let CheckpointKey {
            consumer,
            source_format,
            source,
            partition_format,
            partition,
            position_format,
        } = key;
        self.consumer == *consumer
            && self.source_format == *source_format
            && self.source == *source
            && self.partition_format == *partition_format
            && self.partition == *partition
            && self.position_format == *position_format
    }

    pub fn canonical(&self) -> String {
        let prefix = if self.partition.is_some() {
            "delivery/v1"
        } else {
            "delivery/v1/null"
        };
        format!(
            "{prefix}|{}|{}|{}|{}|{}|{}|{}",
            self.consumer.canonical(),
            self.source_format.as_str(),
            self.source.as_str(),
            self.partition_format.as_str(),
            self.partition.as_ref().map(Component::as_str).unwrap_or(""),
            self.position_format.as_str(),
            self.position.token.as_str(),
        )
    }
}

/// An opaque provider-defined position.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Position {
    pub token: Component,
}

/// The durable key for one ordered consumer/source/partition stream.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct CheckpointKey {
    pub consumer: ConsumerIdentity,
    pub source_format: Component,
    pub source: Component,
    pub partition_format: Component,
    /// The optional partition component of the durable stream natural key.
    /// Empty components are invalid, so the empty canonical field unambiguously
    /// represents `None` while preserving pre-existing non-null encodings.
    pub partition: Option<Component>,
    pub position_format: Component,
}

/// The result of merging a checkpoint position from two branches.
///
/// ORNA-CONSUMER-008 permits equal positions and a branch that retains the
/// base position to merge. Distinct opaque positions have no defined order
/// and must remain a conflict for the database merge owner to report as
/// `sys.CheckpointConflict`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CheckpointPositionMerge {
    Merged(Option<Position>),
    Conflict(CheckpointPositionConflict),
}

/// The exact positions that the database merge owner needs for a checkpoint
/// conflict record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointPositionConflict {
    pub base: Option<Position>,
    pub left: Option<Position>,
    pub right: Option<Position>,
}

/// Merge three branch positions without treating opaque provider tokens as
/// ordered values.
pub fn merge_checkpoint_position(
    base: Option<&Position>,
    left: Option<&Position>,
    right: Option<&Position>,
) -> CheckpointPositionMerge {
    if left == right {
        CheckpointPositionMerge::Merged(left.cloned())
    } else if left == base {
        CheckpointPositionMerge::Merged(right.cloned())
    } else if right == base {
        CheckpointPositionMerge::Merged(left.cloned())
    } else {
        CheckpointPositionMerge::Conflict(CheckpointPositionConflict {
            base: base.cloned(),
            left: left.cloned(),
            right: right.cloned(),
        })
    }
}
