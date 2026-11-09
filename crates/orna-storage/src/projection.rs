use std::borrow::Cow;

use orna_foundation::{OvbRaw, SchemaDescriptor};
use orna_repository::TypedKey;
use orna_runtime::TableMutation;
use orna_value::{ContextValue, Format3Context, Raw, ValueFormat};

use crate::Error;

/// A committed schema descriptor and the two facts row projection needs from
/// it. A caller that holds only a descriptor reference borrows it; the
/// publication path owns the one it read from the graph.
pub(crate) struct CommittedProjectionSchema<'a> {
    descriptor: Cow<'a, SchemaDescriptor>,
    text_primary_key: bool,
    stored_field_count: usize,
}

pub struct ProjectedRuntimeMutation {
    pub key: Vec<u8>,
    pub rekey_to: Option<Vec<u8>>,
    pub value: Option<Vec<u8>>,
}

fn schema_descriptor_entry(schema: &SchemaDescriptor, number: i64) -> Result<&OvbRaw, Error> {
    let OvbRaw::Map(entries) = schema.raw() else {
        return Err(Error::RepositoryUnavailable);
    };
    let key = OvbRaw::Int(number.into());
    entries
        .iter()
        .find(|(candidate, _)| candidate == &key)
        .map(|(_, value)| value)
        .ok_or(Error::RepositoryUnavailable)
}

pub(crate) fn committed_projection_schema(
    descriptor: Cow<'_, SchemaDescriptor>,
) -> Result<CommittedProjectionSchema<'_>, Error> {
    let OvbRaw::Array(key_ids) = schema_descriptor_entry(&descriptor, 2)? else {
        return Err(Error::RepositoryUnavailable);
    };
    let OvbRaw::Array(declared_fields) = schema_descriptor_entry(&descriptor, 3)? else {
        return Err(Error::RepositoryUnavailable);
    };
    let text_primary_key = if key_ids.len() == 1 {
        let field = declared_fields
            .iter()
            .find(|candidate| {
                matches!(candidate, OvbRaw::Array(parts) if parts.first() == key_ids.first())
            })
            .ok_or(Error::RepositoryUnavailable)?;
        let OvbRaw::Array(parts) = field else {
            return Err(Error::RepositoryUnavailable);
        };
        let OvbRaw::Array(type_node) = parts.get(2).ok_or(Error::RepositoryUnavailable)? else {
            return Err(Error::RepositoryUnavailable);
        };
        matches!(
            type_node.as_slice(),
            [OvbRaw::Int(code), OvbRaw::Text(name)]
                if code == &0.into() && name == "Str"
        )
    } else {
        false
    };
    let stored_field_count = declared_fields.iter().try_fold(0usize, |count, field| {
        let OvbRaw::Array(parts) = field else {
            return Err(Error::RepositoryUnavailable);
        };
        let role = parts.get(3).ok_or(Error::RepositoryUnavailable)?;
        Ok(count + usize::from(matches!(role, OvbRaw::Int(role) if role == &1.into())))
    })?;
    Ok(CommittedProjectionSchema {
        descriptor,
        text_primary_key,
        stored_field_count,
    })
}

/// Projects the runtime row carrier only at the publication boundary. Text
/// primary keys arrive as UTF-8 and become canonical typed keys; every other
/// key must already be canonical. Stored-field arrays pass through byte-exact,
/// while a scalar is wrapped byte-exactly only for a one-field committed
/// schema. The descriptor comes from the row map at the pinned expected head.
fn project_runtime_key(
    schema: &CommittedProjectionSchema,
    relation_id: [u8; 16],
    bytes: &[u8],
) -> Result<Vec<u8>, Error> {
    let key = if schema.text_primary_key {
        let text = std::str::from_utf8(bytes).map_err(|_| Error::RepositoryUnavailable)?;
        TypedKey::Text(text.to_owned())
            .canonical_bytes()
            .map_err(|_| Error::RepositoryUnavailable)?
    } else {
        bytes.to_vec()
    };
    schema
        .descriptor
        .validate_stored_key(relation_id, &key)
        .map_err(|_| Error::RepositoryUnavailable)?;
    Ok(key)
}

/// Converts runtime keys to the committed store-key representation.
/// Single-`Str` keys arrive as UTF-8; other shapes arrive canonical.
pub fn canonicalize_runtime_key(
    descriptor: &SchemaDescriptor,
    relation_id: [u8; 16],
    bytes: &[u8],
) -> Result<Vec<u8>, Error> {
    let schema = committed_projection_schema(Cow::Borrowed(descriptor))?;
    project_runtime_key(&schema, relation_id, bytes)
}

pub(crate) fn project_runtime_mutation(
    schema: &CommittedProjectionSchema,
    relation_id: [u8; 16],
    mutation: &TableMutation,
    value_context: &Format3Context,
) -> Result<ProjectedRuntimeMutation, Error> {
    let project_key = |bytes: &[u8]| project_runtime_key(schema, relation_id, bytes);
    let key = project_key(mutation.key())?;
    let rekey_to = mutation.rekey_to().map(project_key).transpose()?;
    let value = mutation
        .value()
        .map(|bytes| {
            let row_key = rekey_to.as_deref().unwrap_or(key.as_slice());
            let decoded = ContextValue::decode_in_context(bytes, ValueFormat::Rov3, value_context)
                .map_err(|_| Error::RepositoryUnavailable)?;
            let fields = if schema
                .descriptor
                .validate_stored_row(relation_id, row_key, bytes)
                .is_ok()
            {
                bytes.to_vec()
            } else if schema.stored_field_count == 1 {
                ContextValue::new_in_context(
                    value_context,
                    ValueFormat::Rov3,
                    Raw::Array(vec![decoded.raw().clone()]),
                )
                .and_then(|value| value.encode())
                .map_err(|_| Error::RepositoryUnavailable)?
            } else {
                bytes.to_vec()
            };
            schema
                .descriptor
                .validate_stored_row(relation_id, row_key, &fields)
                .map_err(|_| Error::RepositoryUnavailable)?;
            Ok(fields)
        })
        .transpose()?;
    Ok(ProjectedRuntimeMutation {
        key,
        rekey_to,
        value,
    })
}
/// Projects a runtime mutation with the committed schema and format context.
/// This is the same key and row normalization used by publication.
pub fn project_runtime_mutation_for_descriptor(
    descriptor: &SchemaDescriptor,
    relation_id: [u8; 16],
    mutation: &TableMutation,
    value_context: &Format3Context,
) -> Result<ProjectedRuntimeMutation, Error> {
    let schema = committed_projection_schema(Cow::Borrowed(descriptor))?;
    project_runtime_mutation(&schema, relation_id, mutation, value_context)
}
