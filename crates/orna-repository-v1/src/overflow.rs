//! The ROV-3 value-overflow writer of repository format 3.
//!
//! store-format-3 §5: a logical value or row entry whose ROV-3 encoding
//! exceeds 8 KiB, or which holds more than 32 direct Blob references, is
//! stored through tag 60113 `[kind3_tree_oid]`. The kind-3 node keeps the
//! encoded length and the SHA-256 of the value's SOV-3 encoding; the ROV-3
//! bytes sit behind an OGB-2 byte index, and every dependency inside those
//! bytes is named by a canonical kind-7 dependency index, so no reference is
//! hidden. Nested large subvalues are lowered bottom-up: a large field leaves
//! a small reference in its row, and the row entry itself is lowered only when
//! it is still too large after its fields were.
//!
//! The logical subvalues are the positions an Orna value nests other values
//! in: the fields of the row tuple and of records (and enum payload records),
//! List elements, Tuple components, an Option's payload and Range endpoints.
//! Every other encoding is one value. SOV-3 expands each overflow reference
//! back to its value, so where a value is lowered never changes its semantic
//! digest.

use std::collections::{BTreeMap, BTreeSet};

use sha2::{Digest, Sha256};

use crate::{
    blob_store::GearChunker,
    native_graph::{
        CaptureIndexBuilder, CborValue, GraphError, MAX_GRAPH_HEIGHT, NativeGraphContext,
        NativeOid, NodeData, RepositoryReadScope, canonical_value_bytes, collect_row_dependencies,
        decode_canonical_content, direct_blob_reference_count, sov3_value,
    },
    orp::{PageEntry, Partition, cbor_head_len},
    row_store::{
        ROW_CANONICAL_LIMIT, ROW_INLINE_LIMIT, ROW_INLINE_REF_LIMIT, RowDependency, RowValue,
        ValueOverflowRef,
    },
};

/// Whether a stored value may stay inline: at most 8 KiB encoded and at most
/// 32 Blob references of its own (store-3.json `row_inline_limit` and
/// `row_inline_ref_limit`). References inside a nested overflow value belong
/// to that value.
pub(crate) fn fits_inline(stored: &CborValue, encoded_len: usize) -> Result<bool, GraphError> {
    Ok(encoded_len <= ROW_INLINE_LIMIT
        && direct_blob_reference_count(stored)? <= ROW_INLINE_REF_LIMIT)
}

/// Where an overflow graph is written: the capture writer of a row graph
/// ([`CaptureStore`]) or the node writer of a system relation. Both lower rows
/// through [`OverflowWriter`], so a logical row is stored the same way
/// whichever of them writes it.
pub(crate) trait OverflowStore {
    /// The width of the repository's native object IDs.
    fn oid_width(&self) -> usize;

    /// Writes one OGS-1 node envelope and returns its object ID.
    fn write_node(&mut self, node: &NodeData) -> Result<NativeOid, GraphError>;

    /// Writes nonempty `bytes` as their canonical OGB-2 byte index
    /// (store-format-3 §4) and returns the index root.
    fn write_byte_index(&mut self, bytes: &[u8]) -> Result<NativeOid, GraphError>;
}

/// The capture writer of one row graph as an overflow store. Every object
/// written joins `written`, the closure the caller syncs before any ref
/// names it.
pub(crate) struct CaptureStore<'a> {
    graph: &'a NativeGraphContext,
    scope: &'a RepositoryReadScope,
    written: &'a mut BTreeSet<NativeOid>,
    object_limit: u64,
}

impl<'a> CaptureStore<'a> {
    pub(crate) fn new(
        graph: &'a NativeGraphContext,
        scope: &'a RepositoryReadScope,
        written: &'a mut BTreeSet<NativeOid>,
        object_limit: u64,
    ) -> Self {
        Self {
            graph,
            scope,
            written,
            object_limit,
        }
    }
}

impl OverflowStore for CaptureStore<'_> {
    fn oid_width(&self) -> usize {
        self.graph.algorithm().width()
    }

    fn write_node(&mut self, node: &NodeData) -> Result<NativeOid, GraphError> {
        self.graph
            .write_capture_node(node, self.written, self.object_limit)
    }

    /// The chunks are structured row bytes, not media, so they are not
    /// charged as payload written.
    fn write_byte_index(&mut self, bytes: &[u8]) -> Result<NativeOid, GraphError> {
        let spans = GearChunker::new()
            .spans(bytes)
            .map_err(|_| GraphError::InvalidByteIndex)?;
        let mut chunks = Vec::with_capacity(spans.len());
        for span in spans {
            let start = usize::try_from(span.offset()).map_err(|_| GraphError::InvalidByteIndex)?;
            let end = start
                .checked_add(
                    usize::try_from(span.length()).map_err(|_| GraphError::InvalidByteIndex)?,
                )
                .ok_or(GraphError::InvalidByteIndex)?;
            let chunk = bytes.get(start..end).ok_or(GraphError::InvalidByteIndex)?;
            let target =
                self.graph
                    .write_capture_object("blob", chunk, self.written, self.object_limit)?;
            chunks.push((span.length(), target, span.sha256()));
        }
        let mut index =
            CaptureIndexBuilder::new(self.graph, self.written, self.object_limit, self.scope);
        for (length, target, sha256) in chunks {
            index.push_chunk(length, target, sha256)?;
        }
        index.finish()?.ok_or(GraphError::InvalidByteIndex)
    }
}

/// Lowers row values into the shared overflow graph of one store.
pub(crate) struct OverflowWriter<'a, S: OverflowStore> {
    store: &'a mut S,
    oid_width: usize,
}

/// A value as stored (ROV-3, possibly holding overflow references) and its
/// SOV-3 semantic form, which never does.
type Lowered = (CborValue, CborValue);

impl<'a, S: OverflowStore> OverflowWriter<'a, S> {
    pub(crate) fn new(store: &'a mut S) -> Self {
        let oid_width = store.oid_width();
        Self { store, oid_width }
    }

    /// Lowers one canonical ROV-3 stored-field tuple and returns the value
    /// its row page stores. Large fields are lowered first; the tuple stays
    /// inline unless it still exceeds 8 KiB or 32 direct Blob references, in
    /// which case the page names its kind-3 node. The tuple must be a logical
    /// value: it may not already hold an overflow reference, and it is bounded
    /// to 1 GiB (store-format-3 §5).
    pub(crate) fn lower_row(&mut self, fields: &[u8]) -> Result<RowValue, GraphError> {
        if fields.len() > ROW_CANONICAL_LIMIT {
            return Err(GraphError::InvalidLength(fields.len() as u64));
        }
        let tuple = decode_canonical_content(fields)?;
        if !matches!(tuple, CborValue::Array(_)) || holds_overflow_reference(&tuple) {
            return Err(GraphError::NonCanonicalData);
        }
        let (stored, semantic) = self.lower_children(tuple)?;
        let encoded = canonical_value_bytes(&stored);
        if fits_inline(&stored, encoded.len())? {
            let mut dependencies = BTreeMap::new();
            collect_row_dependencies(&stored, &mut dependencies)?;
            let dependencies = dependencies
                .into_iter()
                .map(|(oid, kind)| RowDependency::new(oid, kind))
                .collect();
            return RowValue::inline(encoded, dependencies)
                .map_err(|_| GraphError::NonCanonicalData);
        }
        Ok(RowValue::overflow(
            self.write_overflow(&stored, encoded, &semantic)?,
        ))
    }

    /// Lowers one logical subvalue: its own subvalues first, then the value
    /// itself when it still does not fit inline.
    fn lower_value(&mut self, value: CborValue) -> Result<Lowered, GraphError> {
        let (stored, semantic) = self.lower_children(value)?;
        let encoded = canonical_value_bytes(&stored);
        if fits_inline(&stored, encoded.len())? {
            return Ok((stored, semantic));
        }
        let reference = self.write_overflow(&stored, encoded, &semantic)?;
        let root = reference
            .native_root()
            .ok_or(GraphError::NonCanonicalData)?;
        Ok((overflow_reference(root), semantic))
    }

    /// Lowers the logical subvalues of `value`, rebuilding it as stored and in
    /// its semantic form. A value with no nested positions is one value.
    fn lower_children(&mut self, value: CborValue) -> Result<Lowered, GraphError> {
        match value {
            CborValue::Array(items) => {
                let (stored, semantic) = self.lower_each(items)?;
                Ok((CborValue::Array(stored), CborValue::Array(semantic)))
            }
            CborValue::Tag(60_013, payload) => match *payload {
                CborValue::Array(mut fields)
                    if fields.len() == 2 && fields[0] == CborValue::Unsigned(1) =>
                {
                    let (stored, semantic) =
                        self.lower_value(fields.pop().ok_or(GraphError::NonCanonicalData)?)?;
                    Ok((option_some(stored), option_some(semantic)))
                }
                payload => self.single_value(CborValue::Tag(60_013, Box::new(payload))),
            },
            CborValue::Tag(60_015, payload) => match *payload {
                CborValue::Array(items) => {
                    let (stored, semantic) = self.lower_each(items)?;
                    Ok((tag_array(60_015, stored), tag_array(60_015, semantic)))
                }
                payload => self.single_value(CborValue::Tag(60_015, Box::new(payload))),
            },
            CborValue::Tag(60_009, payload) => {
                let (stored, semantic) = self.lower_record(*payload)?;
                Ok((
                    CborValue::Tag(60_009, Box::new(stored)),
                    CborValue::Tag(60_009, Box::new(semantic)),
                ))
            }
            CborValue::Tag(60_008, payload) => match *payload {
                CborValue::Array(fields) if fields.len() == 3 => {
                    let mut fields = fields.into_iter();
                    let (Some(type_id), Some(variant), Some(record)) =
                        (fields.next(), fields.next(), fields.next())
                    else {
                        return Err(GraphError::NonCanonicalData);
                    };
                    let (stored_record, semantic_record) = match record {
                        CborValue::Tag(60_009, record) => {
                            let (stored, semantic) = self.lower_record(*record)?;
                            (
                                CborValue::Tag(60_009, Box::new(stored)),
                                CborValue::Tag(60_009, Box::new(semantic)),
                            )
                        }
                        record => self.single_value(record)?,
                    };
                    let semantic_type = self.semantic_of(&type_id)?;
                    let semantic_variant = self.semantic_of(&variant)?;
                    Ok((
                        tag_array(60_008, vec![type_id, variant, stored_record]),
                        tag_array(
                            60_008,
                            vec![semantic_type, semantic_variant, semantic_record],
                        ),
                    ))
                }
                payload => self.single_value(CborValue::Tag(60_008, Box::new(payload))),
            },
            CborValue::Tag(60_019, payload) => match *payload {
                CborValue::Array(fields) if fields.len() == 3 => {
                    let mut fields = fields.into_iter();
                    let (Some(lower), Some(upper), Some(inclusive)) =
                        (fields.next(), fields.next(), fields.next())
                    else {
                        return Err(GraphError::NonCanonicalData);
                    };
                    let (stored_lower, semantic_lower) = self.lower_value(lower)?;
                    let (stored_upper, semantic_upper) = self.lower_value(upper)?;
                    let semantic_inclusive = self.semantic_of(&inclusive)?;
                    Ok((
                        tag_array(60_019, vec![stored_lower, stored_upper, inclusive]),
                        tag_array(
                            60_019,
                            vec![semantic_lower, semantic_upper, semantic_inclusive],
                        ),
                    ))
                }
                payload => self.single_value(CborValue::Tag(60_019, Box::new(payload))),
            },
            value => self.single_value(value),
        }
    }

    /// The semantic form of a value that holds no overflow reference.
    fn semantic_of(&self, value: &CborValue) -> Result<CborValue, GraphError> {
        sov3_value(value.clone(), self.oid_width, &mut |_| {
            Err(GraphError::NonCanonicalData)
        })
    }

    /// A value with no logical subvalues of its own.
    fn single_value(&self, value: CborValue) -> Result<Lowered, GraphError> {
        let semantic = self.semantic_of(&value)?;
        Ok((value, semantic))
    }

    fn lower_each(
        &mut self,
        items: Vec<CborValue>,
    ) -> Result<(Vec<CborValue>, Vec<CborValue>), GraphError> {
        let mut stored = Vec::with_capacity(items.len());
        let mut semantic = Vec::with_capacity(items.len());
        for item in items {
            let (stored_item, semantic_item) = self.lower_value(item)?;
            stored.push(stored_item);
            semantic.push(semantic_item);
        }
        Ok((stored, semantic))
    }

    /// A record payload `[type_or_null, [[field_id, value], ...]]`, whose
    /// field values are logical subvalues.
    fn lower_record(&mut self, payload: CborValue) -> Result<Lowered, GraphError> {
        let CborValue::Array(parts) = payload else {
            return self.single_value(payload);
        };
        let [type_id, CborValue::Array(pairs)] =
            <[CborValue; 2]>::try_from(parts).map_err(|_| GraphError::NonCanonicalData)?
        else {
            return Err(GraphError::NonCanonicalData);
        };
        let semantic_type = self.semantic_of(&type_id)?;
        let mut stored_pairs = Vec::with_capacity(pairs.len());
        let mut semantic_pairs = Vec::with_capacity(pairs.len());
        for pair in pairs {
            let CborValue::Array(pair) = pair else {
                return Err(GraphError::NonCanonicalData);
            };
            let [field_id, value] =
                <[CborValue; 2]>::try_from(pair).map_err(|_| GraphError::NonCanonicalData)?;
            let semantic_field_id = self.semantic_of(&field_id)?;
            let (stored, semantic) = self.lower_value(value)?;
            stored_pairs.push(CborValue::Array(vec![field_id, stored]));
            semantic_pairs.push(CborValue::Array(vec![semantic_field_id, semantic]));
        }
        Ok((
            CborValue::Array(vec![type_id, CborValue::Array(stored_pairs)]),
            CborValue::Array(vec![semantic_type, CborValue::Array(semantic_pairs)]),
        ))
    }

    /// Writes one value's overflow graph: the OGB-2 byte index of its ROV-3
    /// bytes, the kind-7 index of the dependencies those bytes name, and the
    /// kind-3 node binding both to the length and SOV-3 digest.
    fn write_overflow(
        &mut self,
        stored: &CborValue,
        encoded: Vec<u8>,
        semantic: &CborValue,
    ) -> Result<ValueOverflowRef, GraphError> {
        if encoded.len() > ROW_CANONICAL_LIMIT {
            return Err(GraphError::InvalidLength(encoded.len() as u64));
        }
        let encoded_length = encoded.len() as u64;
        let semantic_digest: [u8; 32] = Sha256::digest(canonical_value_bytes(semantic)).into();
        let mut dependencies = BTreeMap::new();
        collect_row_dependencies(stored, &mut dependencies)?;
        let dependency_root = self.write_dependency_index(dependencies.into_keys().collect())?;
        let byte_root = self.store.write_byte_index(&encoded)?;
        let root = self.store.write_node(&NodeData::ValueOverflow {
            encoded_length,
            semantic_digest,
            byte_root: byte_root.clone(),
            dependency_root: dependency_root.clone(),
        })?;
        ValueOverflowRef::from_native_node(
            root,
            encoded_length,
            semantic_digest,
            byte_root,
            dependency_root,
        )
        .map_err(|_| GraphError::NonCanonicalData)
    }

    /// Writes the canonical kind-7 inventory of `dependencies`: sorted by
    /// native OID bytes, deduplicated, and partitioned by the ORP-1 rule with
    /// the OID as token (store-format-3 §5). Each level up names its children
    /// in inventory order and takes each child's last inventory OID as its
    /// token, as ORP-1 branch levels take each child's last key. An empty
    /// inventory has no index.
    fn write_dependency_index(
        &mut self,
        dependencies: BTreeSet<NativeOid>,
    ) -> Result<Option<NativeOid>, GraphError> {
        let mut level: Vec<DependencyEntry> = dependencies
            .into_iter()
            .map(|oid| DependencyEntry {
                last: oid.clone(),
                child: oid,
            })
            .collect();
        if level.is_empty() {
            return Ok(None);
        }
        let mut height = 0u8;
        loop {
            if height > MAX_GRAPH_HEIGHT {
                return Err(GraphError::HeightExceeded(height));
            }
            let children = level.len();
            let mut next = Vec::new();
            {
                let store = &mut *self.store;
                let mut emit = |page: Vec<DependencyEntry>| -> Result<(), GraphError> {
                    let last = page
                        .last()
                        .map(|entry| entry.last.clone())
                        .ok_or(GraphError::InvalidCount(0))?;
                    let oid = store.write_node(&NodeData::DependencyIndex {
                        height,
                        children: page.into_iter().map(|entry| entry.child).collect(),
                    })?;
                    next.push(DependencyEntry { child: oid, last });
                    Ok(())
                };
                let mut partition = Partition::new(3 + cbor_head_len(u64::from(height)), height);
                for entry in level {
                    let width = entry.child.as_bytes().len();
                    let size = cbor_head_len(width as u64) + width;
                    let refs = vec![entry.child.clone()];
                    partition.push(entry, size, refs, &mut emit)?;
                }
                partition.finish(&mut emit)?;
            }
            if next.len() == 1 {
                return Ok(next.pop().map(|entry| entry.child));
            }
            if next.len() >= children {
                return Err(GraphError::FanoutExceeded(children));
            }
            level = next;
            height += 1;
        }
    }
}

/// One entry of a kind-7 level: the node or dependency it names and the last
/// inventory OID it covers.
struct DependencyEntry {
    child: NativeOid,
    last: NativeOid,
}

impl PageEntry for DependencyEntry {
    fn token(&self) -> &[u8] {
        self.last.as_bytes()
    }
}

/// The stored reference to a kind-3 node: tag 60113 `[kind3_tree_oid]`.
pub(crate) fn overflow_reference(root: &NativeOid) -> CborValue {
    CborValue::Tag(
        60_113,
        Box::new(CborValue::Array(vec![CborValue::Bytes(
            root.as_bytes().to_vec(),
        )])),
    )
}

/// Whether a value already holds an overflow reference anywhere.
fn holds_overflow_reference(value: &CborValue) -> bool {
    match value {
        CborValue::Tag(60_113, _) => true,
        CborValue::Tag(_, payload) => holds_overflow_reference(payload),
        CborValue::Array(items) => items.iter().any(holds_overflow_reference),
        CborValue::Map(entries) => entries
            .iter()
            .any(|(key, value)| holds_overflow_reference(key) || holds_overflow_reference(value)),
        _ => false,
    }
}

fn option_some(value: CborValue) -> CborValue {
    tag_array(60_013, vec![CborValue::Unsigned(1), value])
}

fn tag_array(tag: u64, items: Vec<CborValue>) -> CborValue {
    CborValue::Tag(tag, Box::new(CborValue::Array(items)))
}
