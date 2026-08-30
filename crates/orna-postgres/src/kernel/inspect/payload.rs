//! Canonical immutable INSPECT epoch payload codec.

use super::*;

// ---------------------------------------------------------------------------
// The canonical epoch payload envelope
// ---------------------------------------------------------------------------

/// Encodes one immutable epoch as the closed canonical envelope.
///
/// The envelope is deterministic: every length is a fixed-width big-endian
/// integer, every identity is its raw 16 bytes, and every typed value is
/// re-encoded as canonical ORV5 through the pinned registry, so re-encoding
/// a decoded envelope always reproduces the stored bytes.
pub(super) fn encode_epoch_payload(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    epoch: &InspectSnapshotEpoch,
) -> Result<Vec<u8>, PostgresKernelError> {
    let mut writer = PayloadWriter::new();
    writer.bytes.extend_from_slice(INSPECT_EPOCH_MAGIC);
    let observer_context = epoch.observer_context();
    writer.push_u8(if observer_context.is_some() {
        INSPECT_EPOCH_VERSION_V2
    } else {
        INSPECT_EPOCH_VERSION_V1
    });
    if let Some(context) = observer_context {
        writer.push_u8(INSPECT_OBSERVER_CONTEXT_PRESENT);
        writer.push_id(&context.observer_root_invocation_id().to_bytes());
        writer.push_id(&context.observer_parent_invocation_id().to_bytes());
        writer.push_u8(INSPECT_OBSERVER_PURPOSE_INSPECT);
    }
    writer.push_id(&epoch.id().to_bytes());
    writer.push_id(&epoch.invocation_id().to_bytes());
    writer.push_id(&epoch.source_revision_id().to_bytes());
    writer.push_id(&epoch.catalogue_revision_id().to_bytes());
    writer.push_id(&epoch.owner().to_bytes());
    push_system_time(&mut writer, epoch.recorded_at());
    writer.push_id(&epoch.root_target().to_bytes());
    writer.push_u8(outcome_tag(epoch.outcome()));
    push_summary(&mut writer, epoch.summary());
    push_invocation_nodes(&mut writer, epoch.invocation_nodes());
    push_calls(&mut writer, active, registry, epoch.calls())?;
    push_resources(&mut writer, epoch.resources());
    push_state_cells(&mut writer, active, registry, epoch.state_cells())?;
    push_ui_nodes(&mut writer, epoch.ui_nodes());
    push_presentation_candidates(&mut writer, epoch.presentation_candidates());
    push_runtime_bindings(&mut writer, epoch.runtime_bindings());
    push_security_decisions(&mut writer, epoch.security_decisions());
    Ok(writer.into_bytes())
}

/// Decodes one canonical envelope back into the immutable epoch.
pub(super) fn decode_epoch_payload(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    payload: &[u8],
    record: &str,
) -> Result<InspectSnapshotEpoch, PostgresKernelError> {
    let mut reader = PayloadReader::new(payload, record);
    if reader.bytes.len() < INSPECT_EPOCH_MAGIC.len()
        || &reader.bytes[..INSPECT_EPOCH_MAGIC.len()] != INSPECT_EPOCH_MAGIC
    {
        return Err(reader.invalid("epoch payload magic is not exact"));
    }
    reader.position = INSPECT_EPOCH_MAGIC.len();
    let version = reader.take_u8("epoch payload version")?;
    let observer_context = match version {
        INSPECT_EPOCH_VERSION_V1 => None,
        INSPECT_EPOCH_VERSION_V2 => {
            if reader.take_u8("observer context presence flag")? != INSPECT_OBSERVER_CONTEXT_PRESENT
            {
                return Err(reader.invalid("observer context presence flag is not canonical"));
            }
            let root = reader.take_id("observer root invocation identity")?;
            let parent = reader.take_id("observer parent invocation identity")?;
            let purpose = reader.take_u8("observer purpose tag")?;
            if root == [0; 16] || parent == [0; 16] {
                return Err(reader.invalid("observer context identities must be non-zero"));
            }
            if purpose != INSPECT_OBSERVER_PURPOSE_INSPECT {
                return Err(reader.invalid("observer purpose tag is outside the closed set"));
            }
            let root = InvocationId::from_bytes(root);
            let parent = InvocationId::from_bytes(parent);
            Some(
                InspectObserverContext::new(root, parent)
                    .map_err(|_| reader.invalid("observer context identities must be valid"))?,
            )
        }
        _ => return Err(reader.invalid("epoch payload version is unsupported")),
    };
    let id = InspectEpochId::from_bytes(reader.take_id("epoch identity")?);
    let invocation_id = InvocationId::from_bytes(reader.take_id("invocation identity")?);
    let source_revision_id =
        SourceRevisionId::from_bytes(reader.take_id("source revision identity")?);
    let catalogue_revision_id =
        CatalogueRevisionId::from_bytes(reader.take_id("catalogue revision identity")?);
    let owner = PrincipalId::from_bytes(reader.take_id("owner principal identity")?);
    let recorded_at = take_system_time(&mut reader)?;
    let root_target = FunctionId::from_bytes(reader.take_id("root target identity")?);
    let outcome = decode_outcome(reader.take_u8("outcome")?, &reader)?;
    let summary = take_summary(&mut reader)?;
    let invocation_nodes = take_invocation_nodes(&mut reader)?;
    let calls = take_calls(&mut reader, active, registry)?;
    let resources = take_resources(&mut reader)?;
    let state_cells = take_state_cells(&mut reader, active, registry)?;
    let ui_nodes = take_ui_nodes(&mut reader)?;
    let presentation_candidates = take_presentation_candidates(&mut reader)?;
    let runtime_bindings = take_runtime_bindings(&mut reader)?;
    let security_decisions = take_security_decisions(&mut reader)?;
    if reader.remaining() != 0 {
        return Err(reader.invalid("epoch payload carries trailing bytes"));
    }
    let epoch = InspectSnapshotEpoch::new(
        id,
        invocation_id,
        source_revision_id,
        catalogue_revision_id,
        owner,
        recorded_at,
        root_target,
        outcome,
        summary,
        &InspectSnapshotOptions::new(true, true, true, true),
        invocation_nodes,
        calls,
        resources,
        state_cells,
        ui_nodes,
        presentation_candidates,
        runtime_bindings,
        security_decisions,
    )
    .map_err(PostgresKernelError::Inspect)?;
    epoch
        .with_observer_context(observer_context)
        .map_err(PostgresKernelError::Inspect)
}

struct PayloadWriter {
    bytes: Vec<u8>,
}

impl PayloadWriter {
    fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    fn push_u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    fn push_u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    fn push_u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    fn push_flag(&mut self, value: bool) {
        self.bytes.push(u8::from(value));
    }

    fn push_bytes(&mut self, bytes: &[u8]) {
        self.push_u64(bytes.len() as u64);
        self.bytes.extend_from_slice(bytes);
    }

    fn push_str(&mut self, value: &str) {
        self.push_bytes(value.as_bytes());
    }

    fn push_id(&mut self, id: &[u8; 16]) {
        self.bytes.extend_from_slice(id);
    }

    fn push_opt_id(&mut self, id: Option<[u8; 16]>) {
        self.push_flag(id.is_some());
        if let Some(id) = id {
            self.push_id(&id);
        }
    }

    fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

struct PayloadReader<'a> {
    bytes: &'a [u8],
    position: usize,
    record: &'a str,
}

impl<'a> PayloadReader<'a> {
    fn new(bytes: &'a [u8], record: &'a str) -> Self {
        Self {
            bytes,
            position: 0,
            record,
        }
    }

    fn remaining(&self) -> usize {
        self.bytes.len() - self.position
    }

    fn take_u8(&mut self, rule: &'static str) -> Result<u8, PostgresKernelError> {
        if self.remaining() < 1 {
            return Err(self.invalid(rule));
        }
        let value = self.bytes[self.position];
        self.position += 1;
        Ok(value)
    }

    fn take_u32(&mut self, rule: &'static str) -> Result<u32, PostgresKernelError> {
        if self.remaining() < 4 {
            return Err(self.invalid(rule));
        }
        let value = u32::from_be_bytes(
            self.bytes[self.position..self.position + 4]
                .try_into()
                .expect("four bytes are available"),
        );
        self.position += 4;
        Ok(value)
    }

    fn take_u64(&mut self, rule: &'static str) -> Result<u64, PostgresKernelError> {
        if self.remaining() < 8 {
            return Err(self.invalid(rule));
        }
        let value = u64::from_be_bytes(
            self.bytes[self.position..self.position + 8]
                .try_into()
                .expect("eight bytes are available"),
        );
        self.position += 8;
        Ok(value)
    }

    fn take_count(
        &mut self,
        rule: &'static str,
        minimum_item_bytes: usize,
        maximum: usize,
    ) -> Result<usize, PostgresKernelError> {
        debug_assert!(minimum_item_bytes > 0);
        let count = usize::try_from(self.take_u64(rule)?).map_err(|_| self.invalid(rule))?;
        if count > maximum || count > self.remaining() / minimum_item_bytes {
            return Err(self.invalid(rule));
        }
        Ok(count)
    }

    fn take_flag(&mut self, rule: &'static str) -> Result<bool, PostgresKernelError> {
        Ok(self.take_u8(rule)? != 0)
    }

    fn take_bytes(&mut self, rule: &'static str) -> Result<Vec<u8>, PostgresKernelError> {
        let length = self.take_u64(rule)?;
        let length = usize::try_from(length).map_err(|_| self.invalid(rule))?;
        if self.remaining() < length {
            return Err(self.invalid(rule));
        }
        let value = self.bytes[self.position..self.position + length].to_vec();
        self.position += length;
        Ok(value)
    }

    fn take_str(&mut self, rule: &'static str) -> Result<String, PostgresKernelError> {
        let bytes = self.take_bytes(rule)?;
        String::from_utf8(bytes).map_err(|_| self.invalid(rule))
    }

    fn take_id(&mut self, rule: &'static str) -> Result<[u8; 16], PostgresKernelError> {
        if self.remaining() < 16 {
            return Err(self.invalid(rule));
        }
        let bytes: [u8; 16] = self.bytes[self.position..self.position + 16]
            .try_into()
            .map_err(|_| self.invalid(rule))?;
        self.position += 16;
        Ok(bytes)
    }

    fn take_opt_id(&mut self, rule: &'static str) -> Result<Option<[u8; 16]>, PostgresKernelError> {
        if self.take_flag(rule)? {
            Ok(Some(self.take_id(rule)?))
        } else {
            Ok(None)
        }
    }

    fn invalid(&self, rule: &'static str) -> PostgresKernelError {
        PostgresKernelError::DurableInvariant {
            relation: INSPECT_SNAPSHOT_RELATION,
            record: self.record.to_owned(),
            rule,
        }
    }
}

fn push_system_time(writer: &mut PayloadWriter, time: SystemTime) {
    let duration = time
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    writer.push_u64(duration.as_secs());
    writer.push_u32(duration.subsec_nanos());
}

fn take_system_time(reader: &mut PayloadReader<'_>) -> Result<SystemTime, PostgresKernelError> {
    let seconds = reader.take_u64("recording time seconds")?;
    let nanoseconds = reader.take_u32("recording time nanoseconds")?;
    Ok(SystemTime::UNIX_EPOCH + Duration::new(seconds, nanoseconds))
}

fn outcome_tag(outcome: InspectOutcomeKind) -> u8 {
    match outcome {
        InspectOutcomeKind::Allowed => 0,
        InspectOutcomeKind::Denied => 1,
        InspectOutcomeKind::Failed => 2,
        InspectOutcomeKind::Cancelled => 3,
    }
}

fn decode_outcome(
    tag: u8,
    reader: &PayloadReader<'_>,
) -> Result<InspectOutcomeKind, PostgresKernelError> {
    match tag {
        0 => Ok(InspectOutcomeKind::Allowed),
        1 => Ok(InspectOutcomeKind::Denied),
        2 => Ok(InspectOutcomeKind::Failed),
        3 => Ok(InspectOutcomeKind::Cancelled),
        _ => Err(reader.invalid("outcome tag is outside the closed set")),
    }
}

fn push_summary(writer: &mut PayloadWriter, summary: InspectSnapshotSummary) {
    writer.push_u64(summary.event_count());
    match summary.result() {
        InspectResultSummary::NoValues => writer.push_flag(false),
        InspectResultSummary::ValueBatch { value_count } => {
            writer.push_flag(true);
            writer.push_u64(value_count);
        }
    }
    match summary.duration_nanoseconds() {
        Some(duration) => {
            writer.push_flag(true);
            writer.push_u64(duration);
        }
        None => writer.push_flag(false),
    }
}

fn take_summary(
    reader: &mut PayloadReader<'_>,
) -> Result<InspectSnapshotSummary, PostgresKernelError> {
    let event_count = reader.take_u64("summary event count")?;
    let result = if reader.take_flag("summary result flag")? {
        InspectResultSummary::ValueBatch {
            value_count: reader.take_u64("summary value count")?,
        }
    } else {
        InspectResultSummary::NoValues
    };
    let duration_nanoseconds = if reader.take_flag("summary duration flag")? {
        Some(reader.take_u64("summary duration")?)
    } else {
        None
    };
    InspectSnapshotSummary::new(event_count, result, duration_nanoseconds)
        .map_err(|_| reader.invalid("summary is not canonical"))
}

fn push_invocation_nodes(writer: &mut PayloadWriter, rows: &[InvocationNodeRow]) {
    writer.push_u64(rows.len() as u64);
    for row in rows {
        writer.push_id(&row.id().to_bytes());
        writer.push_opt_id(row.parent_id().map(|id| id.to_bytes()));
        writer.push_u8(match row.kind() {
            InspectInvocationNodeKind::Root => 0,
            InspectInvocationNodeKind::Nested => 1,
        });
        writer.push_u8(match row.phase() {
            InspectInvocationPhase::Started => 0,
            InspectInvocationPhase::Executing => 1,
            InspectInvocationPhase::Completed => 2,
            InspectInvocationPhase::Failed => 3,
            InspectInvocationPhase::Cancelled => 4,
        });
        writer.push_id(&row.target().to_bytes());
        writer.push_u64(row.sequence());
    }
}

fn take_invocation_nodes(
    reader: &mut PayloadReader<'_>,
) -> Result<Vec<InvocationNodeRow>, PostgresKernelError> {
    let count = reader.take_count(
        "invocation node count",
        INVOCATION_NODE_MIN_BYTES,
        MAX_PERSISTED_COLLECTION_ITEMS,
    )?;
    let mut rows = Vec::with_capacity(count);
    for _ in 0..count {
        let id = InvocationId::from_bytes(reader.take_id("invocation node identity")?);
        let parent_id = reader
            .take_opt_id("invocation node parent identity")?
            .map(InvocationId::from_bytes);
        let kind = match reader.take_u8("invocation node kind")? {
            0 => InspectInvocationNodeKind::Root,
            1 => InspectInvocationNodeKind::Nested,
            _ => return Err(reader.invalid("invocation node kind is outside the closed set")),
        };
        let phase = match reader.take_u8("invocation node phase")? {
            0 => InspectInvocationPhase::Started,
            1 => InspectInvocationPhase::Executing,
            2 => InspectInvocationPhase::Completed,
            3 => InspectInvocationPhase::Failed,
            4 => InspectInvocationPhase::Cancelled,
            _ => return Err(reader.invalid("invocation node phase is outside the closed set")),
        };
        let target = FunctionId::from_bytes(reader.take_id("invocation node target identity")?);
        let sequence = reader.take_u64("invocation node sequence")?;
        rows.push(
            InvocationNodeRow::new(id, parent_id, kind, phase, target, sequence)
                .map_err(|_| reader.invalid("invocation node row is not canonical"))?,
        );
    }
    Ok(rows)
}

fn push_calls(
    writer: &mut PayloadWriter,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    rows: &[CallRow],
) -> Result<(), PostgresKernelError> {
    writer.push_u64(rows.len() as u64);
    for row in rows {
        writer.push_id(&row.invocation_id().to_bytes());
        push_optional_invoke_value(writer, active, registry, row.schema())?;
        writer.push_u64(row.value_count());
        writer.push_u64(row.duration_nanoseconds());
    }
    Ok(())
}

fn take_calls(
    reader: &mut PayloadReader<'_>,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
) -> Result<Vec<CallRow>, PostgresKernelError> {
    let count = reader.take_count(
        "call row count",
        CALL_ROW_MIN_BYTES,
        MAX_PERSISTED_COLLECTION_ITEMS,
    )?;
    let mut rows = Vec::with_capacity(count);
    for _ in 0..count {
        let invocation_id = InvocationId::from_bytes(reader.take_id("call invocation identity")?);
        let schema = take_optional_invoke_value(reader, active, registry)?;
        let value_count = reader.take_u64("call value count")?;
        let duration_nanoseconds = reader.take_u64("call duration")?;
        rows.push(
            CallRow::new(invocation_id, schema, value_count, duration_nanoseconds)
                .map_err(|_| reader.invalid("call row is not canonical"))?,
        );
    }
    Ok(rows)
}

fn push_resources(writer: &mut PayloadWriter, rows: &[ResourceRow]) {
    writer.push_u64(rows.len() as u64);
    for row in rows {
        writer.push_u8(match row.kind() {
            orna_core::inspect::InspectResourceKind::State => 0,
            orna_core::inspect::InspectResourceKind::Catalog => 1,
            orna_core::inspect::InspectResourceKind::Standard => 2,
            orna_core::inspect::InspectResourceKind::Runtime => 3,
        });
        writer.push_u8(match row.status() {
            orna_core::inspect::InspectResourceStatus::Active => 0,
            orna_core::inspect::InspectResourceStatus::Invalidated => 1,
            orna_core::inspect::InspectResourceStatus::Released => 2,
        });
    }
}

fn take_resources(reader: &mut PayloadReader<'_>) -> Result<Vec<ResourceRow>, PostgresKernelError> {
    use orna_core::inspect::{InspectResourceKind, InspectResourceStatus};
    let count = reader.take_count(
        "resource row count",
        RESOURCE_ROW_MIN_BYTES,
        MAX_PERSISTED_COLLECTION_ITEMS,
    )?;
    let mut rows = Vec::with_capacity(count);
    for _ in 0..count {
        let kind = match reader.take_u8("resource kind")? {
            0 => InspectResourceKind::State,
            1 => InspectResourceKind::Catalog,
            2 => InspectResourceKind::Standard,
            3 => InspectResourceKind::Runtime,
            _ => return Err(reader.invalid("resource kind is outside the closed set")),
        };
        let status = match reader.take_u8("resource status")? {
            0 => InspectResourceStatus::Active,
            1 => InspectResourceStatus::Invalidated,
            2 => InspectResourceStatus::Released,
            _ => return Err(reader.invalid("resource status is outside the closed set")),
        };
        rows.push(ResourceRow::new(kind, status));
    }
    Ok(rows)
}

fn push_state_cells(
    writer: &mut PayloadWriter,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    rows: &[StateCellRow],
) -> Result<(), PostgresKernelError> {
    writer.push_u64(rows.len() as u64);
    for row in rows {
        writer.push_id(&row.key().root_function().to_bytes());
        writer.push_str(row.key().state_profile());
        writer.push_id(&row.key().function().to_bytes());
        writer.push_str(row.key().instance_key());
        writer.push_id(&row.key().state_slot().to_bytes());
        writer.push_id(&row.value_type().to_bytes());
        writer.push_u64(row.revision());
        push_system_time(writer, row.updated_at());
        push_optional_invoke_value(writer, active, registry, row.value())?;
    }
    Ok(())
}

fn take_state_cells(
    reader: &mut PayloadReader<'_>,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
) -> Result<Vec<StateCellRow>, PostgresKernelError> {
    let count = reader.take_count(
        "state cell row count",
        STATE_CELL_ROW_MIN_BYTES,
        MAX_PERSISTED_COLLECTION_ITEMS,
    )?;
    let mut rows = Vec::with_capacity(count);
    for _ in 0..count {
        let root_function =
            FunctionId::from_bytes(reader.take_id("state cell root function identity")?);
        let state_profile = reader.take_str("state cell state profile")?;
        let function = FunctionId::from_bytes(reader.take_id("state cell function identity")?);
        let instance_key = reader.take_str("state cell instance key")?;
        let state_slot = StateSlotId::from_bytes(reader.take_id("state cell state slot identity")?);
        let value_type = TypeId::from_bytes(reader.take_id("state cell value type identity")?);
        let revision = reader.take_u64("state cell revision")?;
        let updated_at = take_system_time(reader)?;
        let value = take_optional_invoke_value(reader, active, registry)?;
        let key = UserStateKeyWithoutPrincipal::new(
            root_function,
            state_profile,
            function,
            instance_key,
            state_slot,
        )
        .map_err(|_| reader.invalid("state cell key is not canonical"))?;
        rows.push(StateCellRow::new(
            key, value_type, revision, updated_at, value,
        ));
    }
    Ok(rows)
}

fn push_ui_nodes(writer: &mut PayloadWriter, rows: &[UiNodeRow]) {
    writer.push_u64(rows.len() as u64);
    for row in rows {
        writer.push_id(&row.function().to_bytes());
        writer.push_str(row.call_site());
        writer.push_str(row.runtime_contract());
    }
}

fn take_ui_nodes(reader: &mut PayloadReader<'_>) -> Result<Vec<UiNodeRow>, PostgresKernelError> {
    let count = reader.take_count(
        "UI node row count",
        UI_NODE_ROW_MIN_BYTES,
        MAX_PERSISTED_COLLECTION_ITEMS,
    )?;
    let mut rows = Vec::with_capacity(count);
    for _ in 0..count {
        let function = FunctionId::from_bytes(reader.take_id("UI node function identity")?);
        let call_site = reader.take_str("UI node call site")?;
        let runtime_contract = reader.take_str("UI node runtime contract")?;
        rows.push(
            UiNodeRow::new(function, call_site, runtime_contract)
                .map_err(|_| reader.invalid("UI node row is not canonical"))?,
        );
    }
    Ok(rows)
}

fn push_presentation_candidates(writer: &mut PayloadWriter, rows: &[PresentationCandidateRow]) {
    writer.push_u64(rows.len() as u64);
    for row in rows {
        writer.push_str(row.presenter());
        writer.push_flag(row.accepted());
        writer.push_str(row.reason());
        writer.push_flag(row.selected_sink().is_some());
        if let Some(sink) = row.selected_sink() {
            push_descriptor(writer, sink);
        }
        writer.push_flag(row.runtime().is_some());
        if let Some(runtime) = row.runtime() {
            writer.push_str(runtime);
        }
    }
}

fn take_presentation_candidates(
    reader: &mut PayloadReader<'_>,
) -> Result<Vec<PresentationCandidateRow>, PostgresKernelError> {
    let count = reader.take_count(
        "presentation candidate row count",
        PRESENTATION_CANDIDATE_ROW_MIN_BYTES,
        MAX_PERSISTED_COLLECTION_ITEMS,
    )?;
    let mut rows = Vec::with_capacity(count);
    for _ in 0..count {
        let presenter = reader.take_str("presentation candidate presenter")?;
        let accepted = reader.take_flag("presentation candidate acceptance")?;
        let reason = reader.take_str("presentation candidate reason")?;
        let selected_sink = if reader.take_flag("presentation candidate sink flag")? {
            Some(take_descriptor(reader)?)
        } else {
            None
        };
        let runtime = if reader.take_flag("presentation candidate runtime flag")? {
            Some(reader.take_str("presentation candidate runtime")?)
        } else {
            None
        };
        rows.push(
            PresentationCandidateRow::new(presenter, accepted, reason, selected_sink, runtime)
                .map_err(|_| reader.invalid("presentation candidate row is not canonical"))?,
        );
    }
    Ok(rows)
}

fn push_runtime_bindings(writer: &mut PayloadWriter, rows: &[RuntimeBindingRow]) {
    writer.push_u64(rows.len() as u64);
    for row in rows {
        writer.push_str(row.runtime_name());
        writer.push_str(row.version());
        writer.push_u64(row.consumed_descriptors().len() as u64);
        for descriptor in row.consumed_descriptors() {
            push_descriptor(writer, descriptor);
        }
        writer.push_u64(row.contracts().len() as u64);
        for (name, version, features) in row.contracts() {
            writer.push_str(name);
            writer.push_str(version);
            writer.push_u64(features.len() as u64);
            for feature in features {
                writer.push_str(feature);
            }
        }
        writer.push_flag(row.trusted());
        writer.push_u32(row.preference_rank());
    }
}

fn take_runtime_bindings(
    reader: &mut PayloadReader<'_>,
) -> Result<Vec<RuntimeBindingRow>, PostgresKernelError> {
    let count = reader.take_count(
        "runtime binding row count",
        RUNTIME_BINDING_ROW_MIN_BYTES,
        MAX_PERSISTED_COLLECTION_ITEMS,
    )?;
    let mut rows = Vec::with_capacity(count);
    for _ in 0..count {
        let runtime_name = reader.take_str("runtime binding name")?;
        let version = reader.take_str("runtime binding version")?;
        let descriptor_count = reader.take_count(
            "runtime binding descriptor count",
            TYPE_DESCRIPTOR_MIN_BYTES,
            MAX_PERSISTED_COLLECTION_ITEMS,
        )?;
        let mut consumed_descriptors = Vec::with_capacity(descriptor_count);
        for _ in 0..descriptor_count {
            consumed_descriptors.push(take_descriptor(reader)?);
        }
        let contract_count = reader.take_count(
            "runtime binding contract count",
            RUNTIME_CONTRACT_MIN_BYTES,
            MAX_PERSISTED_COLLECTION_ITEMS,
        )?;
        let mut contracts = Vec::with_capacity(contract_count);
        for _ in 0..contract_count {
            let name = reader.take_str("runtime binding contract name")?;
            let version = reader.take_str("runtime binding contract version")?;
            let feature_count = reader.take_count(
                "runtime binding contract feature count",
                RUNTIME_FEATURE_MIN_BYTES,
                MAX_PERSISTED_COLLECTION_ITEMS,
            )?;
            let mut features = Vec::with_capacity(feature_count);
            for _ in 0..feature_count {
                features.push(reader.take_str("runtime binding contract feature")?);
            }
            contracts.push((name, version, features));
        }
        let trusted = reader.take_flag("runtime binding trust")?;
        let preference_rank = reader.take_u32("runtime binding preference rank")?;
        rows.push(
            RuntimeBindingRow::new(
                runtime_name,
                version,
                consumed_descriptors,
                contracts,
                trusted,
                preference_rank,
            )
            .map_err(|_| reader.invalid("runtime binding row is not canonical"))?,
        );
    }
    Ok(rows)
}

fn push_security_decisions(writer: &mut PayloadWriter, rows: &[SecurityDecisionRow]) {
    writer.push_u64(rows.len() as u64);
    for row in rows {
        writer.push_u8(match row.kind() {
            InspectSecurityDecisionKind::Execute => 0,
            InspectSecurityDecisionKind::Capability => 1,
            InspectSecurityDecisionKind::UserState => 2,
            InspectSecurityDecisionKind::Inspect => 3,
        });
        writer.push_u8(match row.outcome() {
            InspectSecurityDecisionOutcome::Allowed => 0,
            InspectSecurityDecisionOutcome::Denied => 1,
        });
        writer.push_u64(row.principals().len() as u64);
        for principal in row.principals() {
            writer.push_id(&principal.to_bytes());
        }
        writer.push_opt_id(row.target().map(|target| target.to_bytes()));
        writer.push_flag(row.denial_reason().is_some());
        if let Some(reason) = row.denial_reason() {
            writer.push_str(reason);
        }
        writer.push_u64(row.audit_refs().len() as u64);
        for reference in row.audit_refs() {
            writer.push_id(&reference.to_bytes());
        }
    }
}

fn take_security_decisions(
    reader: &mut PayloadReader<'_>,
) -> Result<Vec<SecurityDecisionRow>, PostgresKernelError> {
    let count = reader.take_count(
        "security decision row count",
        SECURITY_DECISION_ROW_MIN_BYTES,
        MAX_PERSISTED_COLLECTION_ITEMS,
    )?;
    let mut rows = Vec::with_capacity(count);
    for _ in 0..count {
        let kind = match reader.take_u8("security decision kind")? {
            0 => InspectSecurityDecisionKind::Execute,
            1 => InspectSecurityDecisionKind::Capability,
            2 => InspectSecurityDecisionKind::UserState,
            3 => InspectSecurityDecisionKind::Inspect,
            _ => return Err(reader.invalid("security decision kind is outside the closed set")),
        };
        let outcome = match reader.take_u8("security decision outcome")? {
            0 => InspectSecurityDecisionOutcome::Allowed,
            1 => InspectSecurityDecisionOutcome::Denied,
            _ => return Err(reader.invalid("security decision outcome is outside the closed set")),
        };
        let principal_count = reader.take_count(
            "security decision principal count",
            PAYLOAD_ID_BYTES,
            MAX_PERSISTED_COLLECTION_ITEMS,
        )?;
        let mut principals = Vec::with_capacity(principal_count);
        for _ in 0..principal_count {
            principals.push(PrincipalId::from_bytes(
                reader.take_id("security decision principal identity")?,
            ));
        }
        let target = reader
            .take_opt_id("security decision target identity")?
            .map(FunctionId::from_bytes);
        let denial_reason = if reader.take_flag("security decision denial flag")? {
            Some(reader.take_str("security decision denial reason")?)
        } else {
            None
        };
        let reference_count = reader.take_count(
            "security decision audit reference count",
            PAYLOAD_ID_BYTES,
            MAX_PERSISTED_COLLECTION_ITEMS,
        )?;
        let mut audit_refs = Vec::with_capacity(reference_count);
        for _ in 0..reference_count {
            audit_refs.push(SecurityAuditEventId::from_bytes(
                reader.take_id("security decision audit reference identity")?,
            ));
        }
        rows.push(
            SecurityDecisionRow::new(kind, outcome, principals, target, denial_reason, audit_refs)
                .map_err(|_| reader.invalid("security decision row is not canonical"))?,
        );
    }
    Ok(rows)
}
fn push_optional_invoke_value(
    writer: &mut PayloadWriter,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    value: Option<&InvokeValue>,
) -> Result<(), PostgresKernelError> {
    writer.push_flag(value.is_some());
    if let Some(value) = value {
        let bytes =
            encode_constructed_value(active, registry, &RuntimeValue::InvokeValue(value.clone()))
                .map_err(PostgresKernelError::InspectValueCodec)?;
        writer.push_bytes(&bytes);
    }
    Ok(())
}

fn take_optional_invoke_value(
    reader: &mut PayloadReader<'_>,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
) -> Result<Option<InvokeValue>, PostgresKernelError> {
    if !reader.take_flag("typed value flag")? {
        return Ok(None);
    }
    let bytes = reader.take_bytes("typed value payload")?;
    let RuntimeValue::InvokeValue(value) = decode_constructed_value(active, registry, &bytes)
        .map_err(PostgresKernelError::InspectValueCodec)?
    else {
        return Err(reader.invalid("typed value payload must decode as one invoke value"));
    };
    Ok(Some(value))
}

fn push_descriptor(writer: &mut PayloadWriter, descriptor: &TypeDescriptor) {
    match descriptor.kind() {
        orna_core::types::TypeDescriptorKind::Named(id) => {
            writer.push_u8(0);
            writer.push_id(&id.to_bytes());
        }
        orna_core::types::TypeDescriptorKind::Reference(target) => {
            writer.push_u8(1);
            writer.push_id(&target.to_bytes());
        }
        orna_core::types::TypeDescriptorKind::List(element) => {
            writer.push_u8(2);
            push_descriptor(writer, element);
        }
        orna_core::types::TypeDescriptorKind::Set(element) => {
            writer.push_u8(3);
            push_descriptor(writer, element);
        }
        orna_core::types::TypeDescriptorKind::Map { key, value } => {
            writer.push_u8(4);
            push_descriptor(writer, key);
            push_descriptor(writer, value);
        }
        orna_core::types::TypeDescriptorKind::Option(value) => {
            writer.push_u8(5);
            push_descriptor(writer, value);
        }
        orna_core::types::TypeDescriptorKind::Stream(element) => {
            writer.push_u8(6);
            push_descriptor(writer, element);
        }
    }
}

fn take_descriptor(reader: &mut PayloadReader<'_>) -> Result<TypeDescriptor, PostgresKernelError> {
    let tag = reader.take_u8("type descriptor tag")?;
    match tag {
        0 => Ok(TypeDescriptor::named(TypeId::from_bytes(
            reader.take_id("type descriptor identity")?,
        ))),
        1 => Ok(TypeDescriptor::reference(TypeId::from_bytes(
            reader.take_id("type descriptor reference identity")?,
        ))),
        2 => TypeDescriptor::list(take_descriptor(reader)?)
            .map_err(|_| reader.invalid("type descriptor is not canonical")),
        3 => TypeDescriptor::set(take_descriptor(reader)?)
            .map_err(|_| reader.invalid("type descriptor is not canonical")),
        4 => {
            let key = take_descriptor(reader)?;
            let value = take_descriptor(reader)?;
            TypeDescriptor::map(key, value)
                .map_err(|_| reader.invalid("type descriptor is not canonical"))
        }
        5 => TypeDescriptor::option(take_descriptor(reader)?)
            .map_err(|_| reader.invalid("type descriptor is not canonical")),
        6 => TypeDescriptor::stream(take_descriptor(reader)?)
            .map_err(|_| reader.invalid("type descriptor is not canonical")),
        _ => Err(reader.invalid("type descriptor tag is outside the closed set")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_count_near_u64_max_fails_before_allocation() {
        let payload = u64::MAX.to_be_bytes();
        let mut reader = PayloadReader::new(&payload, "malformed count");
        assert!(matches!(
            reader.take_count("row count is too large", 1, MAX_PERSISTED_COLLECTION_ITEMS),
            Err(PostgresKernelError::DurableInvariant {
                rule: "row count is too large",
                ..
            })
        ));
    }

    #[test]
    fn persisted_count_must_fit_remaining_payload_bytes() {
        let mut payload = 2_u64.to_be_bytes().to_vec();
        payload.extend_from_slice(&[0; 4]);
        let mut reader = PayloadReader::new(&payload, "bounded count");
        assert_eq!(
            reader
                .take_count("row count", 2, MAX_PERSISTED_COLLECTION_ITEMS)
                .expect("two items fit in four bytes"),
            2
        );

        let mut payload = 2_u64.to_be_bytes().to_vec();
        payload.push(0);
        let mut reader = PayloadReader::new(&payload, "truncated count");
        assert!(
            reader
                .take_count("row count", 2, MAX_PERSISTED_COLLECTION_ITEMS)
                .is_err()
        );
    }

    #[test]
    fn persisted_empty_count_is_valid_without_remaining_items() {
        let payload = 0_u64.to_be_bytes();
        let mut reader = PayloadReader::new(&payload, "empty collection");
        assert_eq!(
            reader
                .take_count("row count", 16, MAX_PERSISTED_COLLECTION_ITEMS)
                .expect("empty collection has no item bytes"),
            0
        );
    }
}
