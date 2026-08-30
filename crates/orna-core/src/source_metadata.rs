//! Stable, bounded metadata returned by `sys.source.current()`.

use crate::{
    FunctionId, FunctionRevisionId, SourceUnitId, TypeId,
    revision::{DefinitionReferenceTarget, Sha256Digest},
};

const MAGIC: &[u8] = b"ORNA-SOURCE/1\0";
const SIGNATURE_MAGIC: &[u8] = b"ORNA-SOURCE/2\0";
const MAX_STRING: usize = 4096;
const MAX_PARAMETERS: usize = 256;
const MAX_REFERENCES: usize = 4096;

/// A bounded description of the current Orna function.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceFunctionMetadata {
    function: FunctionId,
    function_revision: FunctionRevisionId,
    function_name: String,
    source_unit: SourceUnitId,
    byte_start: u32,
    byte_end: u32,
    declaration_content_hash: Sha256Digest,
    body_kind: SourceBodyKind,
    return_metadata: Option<SourceReturnMetadata>,
    parameters: Vec<SourceParameterMetadata>,
    references: Vec<SourceReferenceMetadata>,
}

impl SourceFunctionMetadata {
    /// Creates metadata after validating its bounded collections and strings.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        function: FunctionId,
        function_revision: FunctionRevisionId,
        function_name: impl Into<String>,
        source_unit: SourceUnitId,
        byte_start: u32,
        byte_end: u32,
        declaration_content_hash: Sha256Digest,
        parameters: Vec<SourceParameterMetadata>,
        references: Vec<SourceReferenceMetadata>,
    ) -> Result<Self, SourceMetadataError> {
        Self::new_with_signature(
            function,
            function_revision,
            function_name,
            source_unit,
            byte_start,
            byte_end,
            declaration_content_hash,
            SourceBodyKind::Unknown,
            None,
            parameters,
            references,
        )
    }

    /// Creates metadata with generic checked body and return information.
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_signature(
        function: FunctionId,
        function_revision: FunctionRevisionId,
        function_name: impl Into<String>,
        source_unit: SourceUnitId,
        byte_start: u32,
        byte_end: u32,
        declaration_content_hash: Sha256Digest,
        body_kind: SourceBodyKind,
        return_metadata: Option<SourceReturnMetadata>,
        parameters: Vec<SourceParameterMetadata>,
        references: Vec<SourceReferenceMetadata>,
    ) -> Result<Self, SourceMetadataError> {
        let function_name = function_name.into();
        validate_string(&function_name)?;
        if byte_start > byte_end
            || parameters.len() > MAX_PARAMETERS
            || references.len() > MAX_REFERENCES
        {
            return Err(SourceMetadataError::InvalidBounds);
        }
        for parameter in &parameters {
            validate_string(parameter.name())?;
        }
        for reference in &references {
            validate_string(reference.target_name())?;
            if reference.byte_start() > reference.byte_end() {
                return Err(SourceMetadataError::InvalidBounds);
            }
        }
        Ok(Self {
            function,
            function_revision,
            function_name,
            source_unit,
            byte_start,
            byte_end,
            declaration_content_hash,
            body_kind,
            return_metadata,
            parameters,
            references,
        })
    }

    pub const fn function(&self) -> FunctionId {
        self.function
    }
    pub const fn function_revision(&self) -> FunctionRevisionId {
        self.function_revision
    }
    pub fn function_name(&self) -> &str {
        &self.function_name
    }
    pub const fn source_unit(&self) -> SourceUnitId {
        self.source_unit
    }
    pub const fn byte_start(&self) -> u32 {
        self.byte_start
    }
    pub const fn byte_end(&self) -> u32 {
        self.byte_end
    }
    pub const fn declaration_content_hash(&self) -> Sha256Digest {
        self.declaration_content_hash
    }
    pub const fn body_kind(&self) -> SourceBodyKind {
        self.body_kind
    }
    pub const fn return_metadata(&self) -> Option<SourceReturnMetadata> {
        self.return_metadata
    }
    pub fn parameters(&self) -> &[SourceParameterMetadata] {
        &self.parameters
    }

    /// Finds a parameter by its declaration ordinal.
    pub fn parameter(&self, ordinal: u32) -> Option<&SourceParameterMetadata> {
        self.parameters
            .iter()
            .find(|parameter| parameter.ordinal() == ordinal)
    }

    pub fn references(&self) -> &[SourceReferenceMetadata] {
        &self.references
    }

    /// Encodes the metadata as a deterministic bounded payload.
    pub fn encode(&self) -> Vec<u8> {
        self.encode_with_magic(MAGIC)
    }

    /// Encodes metadata with the signature-bearing source metadata format.
    pub fn encode_with_signature(&self) -> Vec<u8> {
        self.encode_with_magic(SIGNATURE_MAGIC)
    }

    fn encode_with_magic(&self, magic: &[u8]) -> Vec<u8> {
        let mut output = Vec::new();
        output.extend_from_slice(magic);
        output.extend_from_slice(&self.function.to_bytes());
        output.extend_from_slice(&self.function_revision.to_bytes());
        put_string(&mut output, &self.function_name);
        output.extend_from_slice(&self.source_unit.to_bytes());
        output.extend_from_slice(&self.byte_start.to_be_bytes());
        output.extend_from_slice(&self.byte_end.to_be_bytes());
        output.extend_from_slice(&self.declaration_content_hash.to_bytes());
        if magic == SIGNATURE_MAGIC {
            output.push(self.body_kind.tag());
            match self.return_metadata {
                Some(return_metadata) => {
                    output.push(1);
                    return_metadata.encode_into(&mut output);
                }
                None => output.push(0),
            }
        }
        put_len(&mut output, self.parameters.len());
        for parameter in &self.parameters {
            parameter.encode_into(&mut output);
        }
        put_len(&mut output, self.references.len());
        for reference in &self.references {
            reference.encode_into(&mut output);
        }
        output
    }

    /// Decodes and validates one metadata payload.
    pub fn decode(bytes: &[u8]) -> Result<Self, SourceMetadataError> {
        let mut reader = Reader { bytes, offset: 0 };
        let magic = reader.magic()?;
        let signature = magic == SIGNATURE_MAGIC;
        let function = FunctionId::from_bytes(reader.array()?);
        let function_revision = FunctionRevisionId::from_bytes(reader.array()?);
        let function_name = reader.string()?;
        let source_unit = SourceUnitId::from_bytes(reader.array()?);
        let byte_start = reader.u32()?;
        let byte_end = reader.u32()?;
        let declaration_content_hash = Sha256Digest::from_bytes(reader.array()?);
        let (body_kind, return_metadata) = if signature {
            let body_kind = SourceBodyKind::from_tag(reader.u8()?)?;
            let return_metadata = match reader.u8()? {
                0 => None,
                1 => Some(SourceReturnMetadata::decode_from(&mut reader)?),
                _ => return Err(SourceMetadataError::InvalidReturnMetadata),
            };
            (body_kind, return_metadata)
        } else {
            (SourceBodyKind::Unknown, None)
        };
        let parameter_count = reader.count(MAX_PARAMETERS)?;
        let mut parameters = Vec::with_capacity(parameter_count);
        for _ in 0..parameter_count {
            parameters.push(SourceParameterMetadata::decode_from(&mut reader)?);
        }
        let reference_count = reader.count(MAX_REFERENCES)?;
        let mut references = Vec::with_capacity(reference_count);
        for _ in 0..reference_count {
            references.push(SourceReferenceMetadata::decode_from(&mut reader)?);
        }
        if reader.offset != bytes.len() {
            return Err(SourceMetadataError::TrailingBytes);
        }
        Self::new_with_signature(
            function,
            function_revision,
            function_name,
            source_unit,
            byte_start,
            byte_end,
            declaration_content_hash,
            body_kind,
            return_metadata,
            parameters,
            references,
        )
    }
}

/// The checked body category returned by source introspection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum SourceBodyKind {
    Unknown = 0,
    Expression = 1,
    Procedural = 2,
    ControlFlow = 3,
    State = 4,
    ExternalContract = 5,
    BooleanLiteral = 6,
}

impl SourceBodyKind {
    const fn tag(self) -> u8 {
        self as u8
    }

    fn from_tag(tag: u8) -> Result<Self, SourceMetadataError> {
        match tag {
            0 => Ok(Self::Unknown),
            1 => Ok(Self::Expression),
            2 => Ok(Self::Procedural),
            3 => Ok(Self::ControlFlow),
            4 => Ok(Self::State),
            5 => Ok(Self::ExternalContract),
            6 => Ok(Self::BooleanLiteral),
            _ => Err(SourceMetadataError::InvalidBodyKind),
        }
    }
}

/// The declared return shape and resolved type of a source function.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceReturnMetadata {
    Single(TypeId),
    Stream(TypeId),
}

impl SourceReturnMetadata {
    fn encode_into(self, output: &mut Vec<u8>) {
        match self {
            Self::Single(type_id) => {
                output.push(1);
                output.extend_from_slice(&type_id.to_bytes());
            }
            Self::Stream(type_id) => {
                output.push(2);
                output.extend_from_slice(&type_id.to_bytes());
            }
        }
    }

    fn decode_from(reader: &mut Reader<'_>) -> Result<Self, SourceMetadataError> {
        let tag = reader.u8()?;
        let type_id = TypeId::from_bytes(reader.array()?);
        match tag {
            1 => Ok(Self::Single(type_id)),
            2 => Ok(Self::Stream(type_id)),
            _ => Err(SourceMetadataError::InvalidReturnMetadata),
        }
    }
}

/// One function parameter declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceParameterMetadata {
    id: crate::ParameterId,
    name: String,
    ordinal: u32,
    resolved_type: TypeId,
}

impl SourceParameterMetadata {
    pub fn new(
        id: crate::ParameterId,
        name: impl Into<String>,
        ordinal: u32,
        resolved_type: TypeId,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            ordinal,
            resolved_type,
        }
    }
    pub const fn id(&self) -> crate::ParameterId {
        self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub const fn ordinal(&self) -> u32 {
        self.ordinal
    }
    pub const fn resolved_type(&self) -> TypeId {
        self.resolved_type
    }
    fn encode_into(&self, output: &mut Vec<u8>) {
        output.extend_from_slice(&self.id.to_bytes());
        put_string(output, &self.name);
        output.extend_from_slice(&self.ordinal.to_be_bytes());
        output.extend_from_slice(&self.resolved_type.to_bytes());
    }
    fn decode_from(reader: &mut Reader<'_>) -> Result<Self, SourceMetadataError> {
        Ok(Self::new(
            crate::ParameterId::from_bytes(reader.array()?),
            reader.string()?,
            reader.u32()?,
            TypeId::from_bytes(reader.array()?),
        ))
    }
}

/// One resolved reference from the current function body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceReferenceMetadata {
    ordinal: u32,
    target: DefinitionReferenceTarget,
    target_name: String,
    source_unit: SourceUnitId,
    byte_start: u32,
    byte_end: u32,
}

impl SourceReferenceMetadata {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        ordinal: u32,
        target: DefinitionReferenceTarget,
        target_name: impl Into<String>,
        source_unit: SourceUnitId,
        byte_start: u32,
        byte_end: u32,
    ) -> Self {
        Self {
            ordinal,
            target,
            target_name: target_name.into(),
            source_unit,
            byte_start,
            byte_end,
        }
    }
    pub const fn ordinal(&self) -> u32 {
        self.ordinal
    }
    pub const fn target(&self) -> DefinitionReferenceTarget {
        self.target
    }
    pub fn target_name(&self) -> &str {
        &self.target_name
    }
    pub const fn source_unit(&self) -> SourceUnitId {
        self.source_unit
    }
    pub const fn byte_start(&self) -> u32 {
        self.byte_start
    }
    pub const fn byte_end(&self) -> u32 {
        self.byte_end
    }
    fn encode_into(&self, output: &mut Vec<u8>) {
        output.extend_from_slice(&self.ordinal.to_be_bytes());
        output.push(target_tag(self.target));
        encode_target(output, self.target);
        put_string(output, &self.target_name);
        output.extend_from_slice(&self.source_unit.to_bytes());
        output.extend_from_slice(&self.byte_start.to_be_bytes());
        output.extend_from_slice(&self.byte_end.to_be_bytes());
    }
    fn decode_from(reader: &mut Reader<'_>) -> Result<Self, SourceMetadataError> {
        Ok(Self::new(
            reader.u32()?,
            decode_target(reader)?,
            reader.string()?,
            SourceUnitId::from_bytes(reader.array()?),
            reader.u32()?,
            reader.u32()?,
        ))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceMetadataError {
    InvalidMagic,
    InvalidBodyKind,
    InvalidReturnMetadata,
    InvalidBounds,
    InvalidString,
    Truncated,
    InvalidTarget,
    TrailingBytes,
    CollectionTooLarge,
}

fn validate_string(value: &str) -> Result<(), SourceMetadataError> {
    if value.is_empty() || value.len() > MAX_STRING {
        Err(SourceMetadataError::InvalidString)
    } else {
        Ok(())
    }
}

fn put_len(output: &mut Vec<u8>, length: usize) {
    output.extend_from_slice(&(length as u32).to_be_bytes());
}

fn put_string(output: &mut Vec<u8>, value: &str) {
    put_len(output, value.len());
    output.extend_from_slice(value.as_bytes());
}

fn target_tag(target: DefinitionReferenceTarget) -> u8 {
    match target {
        DefinitionReferenceTarget::ObjectType(_) => 1,
        DefinitionReferenceTarget::ValueType(_) => 2,
        DefinitionReferenceTarget::Field { .. } => 3,
        DefinitionReferenceTarget::Function(_) => 4,
        DefinitionReferenceTarget::Parameter { .. } => 5,
        DefinitionReferenceTarget::Expression(_) => 6,
    }
}

fn encode_target(output: &mut Vec<u8>, target: DefinitionReferenceTarget) {
    match target {
        DefinitionReferenceTarget::ObjectType(id) | DefinitionReferenceTarget::ValueType(id) => {
            output.extend_from_slice(&id.to_bytes())
        }
        DefinitionReferenceTarget::Expression(id) => output.extend_from_slice(&id.to_bytes()),
        DefinitionReferenceTarget::Function(id) => output.extend_from_slice(&id.to_bytes()),
        DefinitionReferenceTarget::Field { owner, field } => {
            output.extend_from_slice(&owner.to_bytes());
            output.extend_from_slice(&field.to_bytes());
        }
        DefinitionReferenceTarget::Parameter { owner, parameter } => {
            output.extend_from_slice(&owner.to_bytes());
            output.extend_from_slice(&parameter.to_bytes());
        }
    }
}

fn decode_target(
    reader: &mut Reader<'_>,
) -> Result<DefinitionReferenceTarget, SourceMetadataError> {
    match reader.u8()? {
        1 => Ok(DefinitionReferenceTarget::ObjectType(TypeId::from_bytes(
            reader.array()?,
        ))),
        2 => Ok(DefinitionReferenceTarget::ValueType(TypeId::from_bytes(
            reader.array()?,
        ))),
        3 => Ok(DefinitionReferenceTarget::Field {
            owner: TypeId::from_bytes(reader.array()?),
            field: crate::FieldId::from_bytes(reader.array()?),
        }),
        4 => Ok(DefinitionReferenceTarget::Function(FunctionId::from_bytes(
            reader.array()?,
        ))),
        5 => Ok(DefinitionReferenceTarget::Parameter {
            owner: FunctionId::from_bytes(reader.array()?),
            parameter: crate::ParameterId::from_bytes(reader.array()?),
        }),
        6 => Ok(DefinitionReferenceTarget::Expression(
            crate::ExpressionId::from_bytes(reader.array()?),
        )),
        _ => Err(SourceMetadataError::InvalidTarget),
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl Reader<'_> {
    fn magic(&mut self) -> Result<&[u8], SourceMetadataError> {
        if self.bytes.get(..SIGNATURE_MAGIC.len()) == Some(SIGNATURE_MAGIC) {
            self.offset = SIGNATURE_MAGIC.len();
            return Ok(SIGNATURE_MAGIC);
        }
        if self.bytes.get(..MAGIC.len()) == Some(MAGIC) {
            self.offset = MAGIC.len();
            return Ok(MAGIC);
        }
        Err(SourceMetadataError::InvalidMagic)
    }

    fn take(&mut self, length: usize) -> Result<&[u8], SourceMetadataError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(SourceMetadataError::Truncated)?;
        let output = self
            .bytes
            .get(self.offset..end)
            .ok_or(SourceMetadataError::Truncated)?;
        self.offset = end;
        Ok(output)
    }
    fn u8(&mut self) -> Result<u8, SourceMetadataError> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, SourceMetadataError> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N], SourceMetadataError> {
        Ok(self.take(N)?.try_into().unwrap())
    }
    fn count(&mut self, maximum: usize) -> Result<usize, SourceMetadataError> {
        let count = self.u32()? as usize;
        if count > maximum {
            Err(SourceMetadataError::CollectionTooLarge)
        } else {
            Ok(count)
        }
    }
    fn string(&mut self) -> Result<String, SourceMetadataError> {
        let length = self.u32()? as usize;
        let bytes = self.take(length)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| SourceMetadataError::InvalidString)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_round_trips_deterministically() {
        let value = SourceFunctionMetadata::new(
            FunctionId::from_bytes([1; 16]),
            FunctionRevisionId::from_bytes([2; 16]),
            "app.f",
            SourceUnitId::from_bytes([3; 16]),
            4,
            8,
            Sha256Digest::from_bytes([4; 32]),
            vec![SourceParameterMetadata::new(
                crate::ParameterId::from_bytes([5; 16]),
                "p",
                0,
                TypeId::from_bytes([6; 16]),
            )],
            vec![],
        )
        .unwrap();
        let bytes = value.encode();
        assert_eq!(SourceFunctionMetadata::decode(&bytes).unwrap(), value);
        assert_eq!(bytes, value.encode());
    }

    #[test]
    fn signature_metadata_round_trips_every_body_kind() {
        let body_kinds = [
            SourceBodyKind::Unknown,
            SourceBodyKind::Expression,
            SourceBodyKind::Procedural,
            SourceBodyKind::ControlFlow,
            SourceBodyKind::State,
            SourceBodyKind::ExternalContract,
            SourceBodyKind::BooleanLiteral,
        ];

        for body_kind in body_kinds {
            let value = SourceFunctionMetadata::new_with_signature(
                FunctionId::from_bytes([1; 16]),
                FunctionRevisionId::from_bytes([2; 16]),
                "app.f",
                SourceUnitId::from_bytes([3; 16]),
                4,
                8,
                Sha256Digest::from_bytes([4; 32]),
                body_kind,
                Some(SourceReturnMetadata::Stream(TypeId::from_bytes([6; 16]))),
                vec![],
                vec![],
            )
            .unwrap();
            let bytes = value.encode_with_signature();
            let decoded = SourceFunctionMetadata::decode(&bytes).unwrap();
            assert_eq!(decoded, value);
            assert_eq!(decoded.body_kind(), body_kind);
            assert_eq!(decoded.return_metadata(), value.return_metadata());
        }
    }

    #[test]
    fn signature_metadata_rejects_unknown_body_kind() {
        let mut bytes = SourceFunctionMetadata::new_with_signature(
            FunctionId::from_bytes([1; 16]),
            FunctionRevisionId::from_bytes([2; 16]),
            "app.f",
            SourceUnitId::from_bytes([3; 16]),
            4,
            8,
            Sha256Digest::from_bytes([4; 32]),
            SourceBodyKind::Expression,
            None,
            vec![],
            vec![],
        )
        .unwrap()
        .encode_with_signature();
        let body_kind_offset = b"ORNA-SOURCE/2\0".len() + 16 + 16 + 4 + 5 + 16 + 4 + 4 + 32;
        bytes[body_kind_offset] = 7;
        assert_eq!(
            SourceFunctionMetadata::decode(&bytes),
            Err(SourceMetadataError::InvalidBodyKind)
        );
    }

    #[test]
    fn signature_metadata_rejects_unknown_return_shape() {
        let mut bytes = SourceFunctionMetadata::new_with_signature(
            FunctionId::from_bytes([1; 16]),
            FunctionRevisionId::from_bytes([2; 16]),
            "app.f",
            SourceUnitId::from_bytes([3; 16]),
            4,
            8,
            Sha256Digest::from_bytes([4; 32]),
            SourceBodyKind::Expression,
            Some(SourceReturnMetadata::Single(TypeId::from_bytes([6; 16]))),
            vec![],
            vec![],
        )
        .unwrap()
        .encode_with_signature();
        let return_shape_offset = b"ORNA-SOURCE/2\0".len() + 16 + 16 + 4 + 5 + 16 + 4 + 4 + 32 + 1;
        bytes[return_shape_offset] = 3;
        assert_eq!(
            SourceFunctionMetadata::decode(&bytes),
            Err(SourceMetadataError::InvalidReturnMetadata)
        );
    }

    #[test]
    fn legacy_metadata_keeps_unknown_signature_fields() {
        let value = SourceFunctionMetadata::new(
            FunctionId::from_bytes([1; 16]),
            FunctionRevisionId::from_bytes([2; 16]),
            "app.f",
            SourceUnitId::from_bytes([3; 16]),
            4,
            8,
            Sha256Digest::from_bytes([4; 32]),
            vec![],
            vec![],
        )
        .unwrap();
        let decoded = SourceFunctionMetadata::decode(&value.encode()).unwrap();
        assert_eq!(decoded.body_kind(), SourceBodyKind::Unknown);
        assert_eq!(decoded.return_metadata(), None);
    }

    #[test]
    fn metadata_accepts_utf8_names() {
        let value = SourceFunctionMetadata::new(
            FunctionId::from_bytes([1; 16]),
            FunctionRevisionId::from_bytes([2; 16]),
            "app.café",
            SourceUnitId::from_bytes([3; 16]),
            0,
            1,
            Sha256Digest::from_bytes([4; 32]),
            vec![],
            vec![],
        )
        .unwrap();
        assert_eq!(
            SourceFunctionMetadata::decode(&value.encode())
                .unwrap()
                .function_name(),
            "app.café"
        );
    }

    #[test]
    fn metadata_rejects_trailing_bytes() {
        let value = SourceFunctionMetadata::new(
            FunctionId::from_bytes([1; 16]),
            FunctionRevisionId::from_bytes([2; 16]),
            "app.f",
            SourceUnitId::from_bytes([3; 16]),
            0,
            1,
            Sha256Digest::from_bytes([4; 32]),
            vec![],
            vec![],
        )
        .unwrap();
        let mut bytes = value.encode();
        bytes.push(0);
        assert_eq!(
            SourceFunctionMetadata::decode(&bytes),
            Err(SourceMetadataError::TrailingBytes)
        );
    }
}
