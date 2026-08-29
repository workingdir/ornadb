use sha2::{Digest, Sha256};
use std::{cmp::Ordering, error::Error, fmt};

const DOMAIN_SEPARATOR: &[u8] = b"orna.runtime-offer.witness/1";
const MAX_TEXT_BYTES: usize = 4096;
const MAX_FEATURES: usize = 16;
const MAX_MEDIA_TYPES: usize = 16;
const MAX_SINKS: usize = 1;
const MAX_CONTRACTS: usize = 8;
const MAX_CANONICAL_BYTES: usize = 16 * 1024 * 1024;
const ACCEPTED_RUNTIME_CONTRACTS: [&str; 8] = [
    "std.ui.window",
    "std.ui.text",
    "std.ui.button",
    "std.ui.panel",
    "std.ui.row",
    "std.ui.column",
    "std.ui.text_input",
    "std.ui.tabs",
];

/// A typed failure while taking an immutable runtime-offer snapshot.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuntimeOfferWitnessError {
    /// A required identity text value is empty.
    EmptyText { field: &'static str },
    /// Text contains an embedded NUL byte.
    TextContainsNul { field: &'static str },
    /// Text exceeds the per-value bound.
    TextTooLong {
        field: &'static str,
        bytes: usize,
        maximum: usize,
    },
    /// A contract feature list exceeds its bound.
    TooManyFeatures { count: usize, maximum: usize },
    /// A sink media-type list exceeds its bound.
    TooManyMediaTypes { count: usize, maximum: usize },
    /// The sink offer count exceeds its bound.
    TooManySinks { count: usize, maximum: usize },
    /// The contract offer count exceeds its bound.
    TooManyContracts { count: usize, maximum: usize },
    /// A feature identity is empty or contains a NUL byte.
    InvalidFeatureIdentity {
        contract_index: usize,
        feature_index: usize,
    },
    /// A sink identity is empty or contains a NUL byte.
    InvalidSinkIdentity { sink_index: usize },
    /// A contract identity is empty or contains a NUL byte.
    InvalidContractIdentity { contract_index: usize },
    /// A feature name occurs more than once in one contract offer.
    DuplicateFeature {
        contract_index: usize,
        feature_index: usize,
    },
    /// A media type occurs more than once in one sink offer.
    DuplicateMediaType {
        sink_index: usize,
        media_index: usize,
    },
    /// Two sink offers are identical after canonicalisation.
    DuplicateSink { sink_index: usize },
    /// Two contract offers are identical after canonicalisation.
    DuplicateContract { contract_index: usize },
    /// Canonical encoding would exceed the aggregate bound.
    CanonicalBytesTooLarge { bytes: usize, maximum: usize },
    /// The descriptor is outside the currently accepted runtime policy.
    UnsupportedDescriptorPolicy,
}

impl fmt::Display for RuntimeOfferWitnessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyText { field } => write!(formatter, "{field} must not be empty"),
            Self::TextContainsNul { field } => {
                write!(formatter, "{field} must not contain NUL bytes")
            }
            Self::TextTooLong {
                field,
                bytes,
                maximum,
            } => write!(
                formatter,
                "{field} is {bytes} bytes, exceeding the {maximum}-byte limit"
            ),
            Self::TooManyFeatures { count, maximum } => write!(
                formatter,
                "contract has {count} features, exceeding the {maximum}-feature limit"
            ),
            Self::TooManyMediaTypes { count, maximum } => write!(
                formatter,
                "sink has {count} media types, exceeding the {maximum}-media-type limit"
            ),
            Self::TooManySinks { count, maximum } => write!(
                formatter,
                "offer has {count} sinks, exceeding the {maximum}-sink limit"
            ),
            Self::TooManyContracts { count, maximum } => write!(
                formatter,
                "offer has {count} contracts, exceeding the {maximum}-contract limit"
            ),
            Self::InvalidFeatureIdentity {
                contract_index,
                feature_index,
            } => write!(
                formatter,
                "contract {contract_index} feature {feature_index} has an invalid identity"
            ),
            Self::InvalidSinkIdentity { sink_index } => {
                write!(formatter, "sink {sink_index} has an invalid identity")
            }
            Self::InvalidContractIdentity { contract_index } => {
                write!(
                    formatter,
                    "contract {contract_index} has an invalid identity"
                )
            }
            Self::DuplicateFeature {
                contract_index,
                feature_index,
            } => write!(
                formatter,
                "contract {contract_index} has a duplicate feature at canonical index {feature_index}"
            ),
            Self::DuplicateMediaType {
                sink_index,
                media_index,
            } => write!(
                formatter,
                "sink {sink_index} has a duplicate media type at canonical index {media_index}"
            ),
            Self::DuplicateSink { sink_index } => {
                write!(
                    formatter,
                    "duplicate sink offer at canonical index {sink_index}"
                )
            }
            Self::DuplicateContract { contract_index } => write!(
                formatter,
                "duplicate contract offer at canonical index {contract_index}"
            ),
            Self::CanonicalBytesTooLarge { bytes, maximum } => write!(
                formatter,
                "canonical witness is {bytes} bytes, exceeding the {maximum}-byte limit"
            ),
            Self::UnsupportedDescriptorPolicy => {
                formatter.write_str("runtime descriptor is outside the accepted policy")
            }
        }
    }
}

impl Error for RuntimeOfferWitnessError {}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SinkSnapshot {
    type_name: String,
    media_types: Vec<String>,
    supports_streaming: bool,
    preference_rank: i32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ContractSnapshot {
    name: String,
    major: u32,
    minor: u32,
    features: Vec<String>,
}

/// An immutable, canonical snapshot of the selected runtime offer.
///
/// The constructor accepts primitive fields and borrowed string slices rather
/// than a runtime-loader descriptor. All text and offer collections are copied,
/// sorted, and validated before the canonical bytes and policy digest are
/// retained. The snapshot has no host effects and does not retain borrowed
/// input data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeOfferWitness {
    abi_major: u32,
    abi_minor: u32,
    runtime_name: String,
    runtime_version: String,
    build_id: String,
    platform: String,
    thread_model: i32,
    features: u64,
    sinks: Vec<SinkSnapshot>,
    contracts: Vec<ContractSnapshot>,
    canonical_bytes: Vec<u8>,
    digest: [u8; 32],
}

impl RuntimeOfferWitness {
    /// Constructs the currently accepted Qt runtime-offer witness.
    ///
    /// The constructor accepts primitive fields so callers cannot retain
    /// native descriptor pointers. It enforces the loader's ABI, platform,
    /// thread, feature, sink, and contract policy before copying data.
    #[expect(
        clippy::too_many_arguments,
        reason = "the public constructor mirrors the native runtime descriptor"
    )]
    pub fn new(
        abi_major: u32,
        abi_minor: u32,
        runtime_name: &str,
        runtime_version: &str,
        build_id: &str,
        platform: &str,
        thread_model: i32,
        features: u64,
        sinks: &[(&str, &[&str], bool, i32)],
        contracts: &[(&str, u32, u32, &[&str])],
    ) -> Result<Self, RuntimeOfferWitnessError> {
        validate_runtime_policy(abi_major, abi_minor, platform, thread_model, features)?;
        validate_accepted_runtime_offer(runtime_name, sinks, contracts)?;
        Self::from_parts(
            abi_major,
            abi_minor,
            runtime_name,
            runtime_version,
            build_id,
            platform,
            thread_model,
            features,
            sinks,
            contracts,
        )
    }

    /// Constructs a bounded canonical snapshot for the trusted bridge and
    /// internal deterministic tests.
    #[expect(
        clippy::too_many_arguments,
        reason = "the internal constructor mirrors the native runtime descriptor"
    )]
    pub(crate) fn from_parts(
        abi_major: u32,
        abi_minor: u32,
        runtime_name: &str,
        runtime_version: &str,
        build_id: &str,
        platform: &str,
        thread_model: i32,
        features: u64,
        sinks: &[(&str, &[&str], bool, i32)],
        contracts: &[(&str, u32, u32, &[&str])],
    ) -> Result<Self, RuntimeOfferWitnessError> {
        validate_required_text("runtime name", runtime_name)?;
        validate_required_text("runtime version", runtime_version)?;
        validate_required_text("build id", build_id)?;
        validate_required_text("platform", platform)?;

        if sinks.len() > MAX_SINKS {
            return Err(RuntimeOfferWitnessError::TooManySinks {
                count: sinks.len(),
                maximum: MAX_SINKS,
            });
        }
        if contracts.len() > MAX_CONTRACTS {
            return Err(RuntimeOfferWitnessError::TooManyContracts {
                count: contracts.len(),
                maximum: MAX_CONTRACTS,
            });
        }

        let mut sink_snapshots = Vec::with_capacity(sinks.len());
        for (sink_index, &(type_name, media_types, supports_streaming, preference_rank)) in
            sinks.iter().enumerate()
        {
            validate_required_text("sink type name", type_name).map_err(|error| {
                if matches!(error, RuntimeOfferWitnessError::EmptyText { .. })
                    || matches!(error, RuntimeOfferWitnessError::TextContainsNul { .. })
                {
                    RuntimeOfferWitnessError::InvalidSinkIdentity { sink_index }
                } else {
                    error
                }
            })?;

            if media_types.len() > MAX_MEDIA_TYPES {
                return Err(RuntimeOfferWitnessError::TooManyMediaTypes {
                    count: media_types.len(),
                    maximum: MAX_MEDIA_TYPES,
                });
            }
            let mut media_type_snapshots = Vec::with_capacity(media_types.len());
            for (media_index, media_type) in media_types.iter().enumerate() {
                validate_nonempty_text("sink media type", media_type).map_err(|error| {
                    if matches!(error, RuntimeOfferWitnessError::EmptyText { .. })
                        || matches!(error, RuntimeOfferWitnessError::TextContainsNul { .. })
                    {
                        RuntimeOfferWitnessError::InvalidSinkIdentity { sink_index }
                    } else {
                        error
                    }
                })?;
                media_type_snapshots.push((*media_type).to_owned());
                if media_index == usize::MAX {
                    return Err(RuntimeOfferWitnessError::CanonicalBytesTooLarge {
                        bytes: usize::MAX,
                        maximum: MAX_CANONICAL_BYTES,
                    });
                }
            }
            media_type_snapshots.sort_unstable();
            if let Some(media_index) = first_duplicate(&media_type_snapshots) {
                return Err(RuntimeOfferWitnessError::DuplicateMediaType {
                    sink_index,
                    media_index,
                });
            }
            sink_snapshots.push(SinkSnapshot {
                type_name: type_name.to_owned(),
                media_types: media_type_snapshots,
                supports_streaming,
                preference_rank,
            });
        }
        sink_snapshots.sort_unstable_by(compare_sinks);
        if let Some(sink_index) = first_duplicate(&sink_snapshots) {
            return Err(RuntimeOfferWitnessError::DuplicateSink { sink_index });
        }

        let mut contract_snapshots = Vec::with_capacity(contracts.len());
        for (contract_index, &(name, major, minor, feature_names)) in contracts.iter().enumerate() {
            validate_required_text("contract name", name).map_err(|error| {
                if matches!(error, RuntimeOfferWitnessError::EmptyText { .. })
                    || matches!(error, RuntimeOfferWitnessError::TextContainsNul { .. })
                {
                    RuntimeOfferWitnessError::InvalidContractIdentity { contract_index }
                } else {
                    error
                }
            })?;

            if feature_names.len() > MAX_FEATURES {
                return Err(RuntimeOfferWitnessError::TooManyFeatures {
                    count: feature_names.len(),
                    maximum: MAX_FEATURES,
                });
            }
            let mut feature_snapshots = Vec::with_capacity(feature_names.len());
            for (feature_index, feature_name) in feature_names.iter().enumerate() {
                validate_nonempty_text("contract feature", feature_name).map_err(|error| {
                    if matches!(error, RuntimeOfferWitnessError::EmptyText { .. })
                        || matches!(error, RuntimeOfferWitnessError::TextContainsNul { .. })
                    {
                        RuntimeOfferWitnessError::InvalidFeatureIdentity {
                            contract_index,
                            feature_index,
                        }
                    } else {
                        error
                    }
                })?;
                feature_snapshots.push((*feature_name).to_owned());
            }
            feature_snapshots.sort_unstable();
            if let Some(feature_index) = first_duplicate(&feature_snapshots) {
                return Err(RuntimeOfferWitnessError::DuplicateFeature {
                    contract_index,
                    feature_index,
                });
            }
            contract_snapshots.push(ContractSnapshot {
                name: name.to_owned(),
                major,
                minor,
                features: feature_snapshots,
            });
        }
        contract_snapshots.sort_unstable_by(compare_contracts);
        if let Some(contract_index) = first_duplicate(&contract_snapshots) {
            return Err(RuntimeOfferWitnessError::DuplicateContract { contract_index });
        }
        validate_runtime_policy(abi_major, abi_minor, platform, thread_model, features)?;

        let mut encoder = CanonicalEncoder::new();
        encoder.push_u32(abi_major)?;
        encoder.push_u32(abi_minor)?;
        encoder.push_text(runtime_name)?;
        encoder.push_text(runtime_version)?;
        encoder.push_text(build_id)?;
        encoder.push_text(platform)?;
        encoder.push_i32(thread_model)?;
        encoder.push_u64(features)?;
        encoder.push_count(sink_snapshots.len())?;
        for sink in &sink_snapshots {
            encoder.push_text(&sink.type_name)?;
            encoder.push_count(sink.media_types.len())?;
            for media_type in &sink.media_types {
                encoder.push_text(media_type)?;
            }
            encoder.push_u8(u8::from(sink.supports_streaming))?;
            encoder.push_i32(sink.preference_rank)?;
        }
        encoder.push_count(contract_snapshots.len())?;
        for contract in &contract_snapshots {
            encoder.push_text(&contract.name)?;
            encoder.push_u32(contract.major)?;
            encoder.push_u32(contract.minor)?;
            encoder.push_count(contract.features.len())?;
            for feature in &contract.features {
                encoder.push_text(feature)?;
            }
        }
        let canonical_bytes = encoder.finish();
        let digest = digest_bytes(&canonical_bytes);

        Ok(Self {
            abi_major,
            abi_minor,
            runtime_name: runtime_name.to_owned(),
            runtime_version: runtime_version.to_owned(),
            build_id: build_id.to_owned(),
            platform: platform.to_owned(),
            thread_model,
            features,
            sinks: sink_snapshots,
            contracts: contract_snapshots,
            canonical_bytes,
            digest,
        })
    }
    /// Copies and validates a descriptor from the trusted runtime loader.
    ///
    /// The descriptor is converted to primitive slices before this witness
    /// stores any data. The witness therefore owns no loader or native memory.
    pub fn from_descriptor(
        descriptor: &crate::runtime_loader::RuntimeDescriptor,
    ) -> Result<Self, RuntimeOfferWitnessError> {
        if descriptor.runtime_name != crate::runtime_loader::QT_RUNTIME_NAME
            || validate_runtime_policy(
                descriptor.abi_major,
                descriptor.abi_minor,
                descriptor.platform.as_str(),
                descriptor.thread_model.0,
                descriptor.features,
            )
            .is_err()
        {
            return Err(RuntimeOfferWitnessError::UnsupportedDescriptorPolicy);
        }
        if descriptor.sinks.len() > MAX_SINKS {
            return Err(RuntimeOfferWitnessError::TooManySinks {
                count: descriptor.sinks.len(),
                maximum: MAX_SINKS,
            });
        }
        if descriptor.contracts.len() > MAX_CONTRACTS {
            return Err(RuntimeOfferWitnessError::TooManyContracts {
                count: descriptor.contracts.len(),
                maximum: MAX_CONTRACTS,
            });
        }
        if descriptor
            .sinks
            .iter()
            .any(|sink| sink.media_types.len() > MAX_MEDIA_TYPES)
        {
            let count = descriptor
                .sinks
                .iter()
                .map(|sink| sink.media_types.len())
                .max()
                .unwrap_or_default();
            return Err(RuntimeOfferWitnessError::TooManyMediaTypes {
                count,
                maximum: MAX_MEDIA_TYPES,
            });
        }
        if descriptor
            .contracts
            .iter()
            .any(|contract| contract.features.len() > MAX_FEATURES)
        {
            let count = descriptor
                .contracts
                .iter()
                .map(|contract| contract.features.len())
                .max()
                .unwrap_or_default();
            return Err(RuntimeOfferWitnessError::TooManyFeatures {
                count,
                maximum: MAX_FEATURES,
            });
        }
        if descriptor.sinks.len() != 1
            || descriptor.sinks[0].type_name != crate::runtime_loader::UI_SINK_NAME
            || !descriptor.sinks[0].media_types.is_empty()
            || descriptor.sinks[0].supports_streaming
            || descriptor.sinks[0].preference_rank != 0
        {
            return Err(RuntimeOfferWitnessError::UnsupportedDescriptorPolicy);
        }
        let mut contract_names = descriptor
            .contracts
            .iter()
            .map(|contract| contract.name.as_str())
            .collect::<Vec<_>>();
        contract_names.sort_unstable();
        let mut expected_contract_names = ACCEPTED_RUNTIME_CONTRACTS.to_vec();
        expected_contract_names.sort_unstable();
        if contract_names != expected_contract_names
            || descriptor.contracts.iter().any(|contract| {
                contract.major != 1 || contract.minor != 0 || !contract.features.is_empty()
            })
        {
            return Err(RuntimeOfferWitnessError::UnsupportedDescriptorPolicy);
        }
        let sink_media_types = descriptor
            .sinks
            .iter()
            .map(|sink| {
                sink.media_types
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let sinks = descriptor
            .sinks
            .iter()
            .zip(&sink_media_types)
            .map(|(sink, media_types)| {
                (
                    sink.type_name.as_str(),
                    media_types.as_slice(),
                    sink.supports_streaming,
                    sink.preference_rank,
                )
            })
            .collect::<Vec<_>>();
        let contract_features = descriptor
            .contracts
            .iter()
            .map(|contract| {
                contract
                    .features
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let contracts = descriptor
            .contracts
            .iter()
            .zip(&contract_features)
            .map(|(contract, features)| {
                (
                    contract.name.as_str(),
                    contract.major,
                    contract.minor,
                    features.as_slice(),
                )
            })
            .collect::<Vec<_>>();
        Self::new(
            descriptor.abi_major,
            descriptor.abi_minor,
            &descriptor.runtime_name,
            &descriptor.runtime_version,
            &descriptor.build_id,
            &descriptor.platform,
            descriptor.thread_model.0,
            descriptor.features,
            &sinks,
            &contracts,
        )
    }

    /// Returns the ABI major version in this snapshot.
    pub const fn abi_major(&self) -> u32 {
        self.abi_major
    }

    /// Returns the ABI minor version in this snapshot.
    pub const fn abi_minor(&self) -> u32 {
        self.abi_minor
    }

    /// Returns the copied runtime name.
    pub fn runtime_name(&self) -> &str {
        &self.runtime_name
    }

    /// Returns the copied runtime version.
    pub fn runtime_version(&self) -> &str {
        &self.runtime_version
    }

    /// Returns the copied runtime build identity.
    pub fn build_id(&self) -> &str {
        &self.build_id
    }

    /// Returns the copied target platform identity.
    pub fn platform(&self) -> &str {
        &self.platform
    }

    /// Returns the ABI thread-model value.
    pub const fn thread_model(&self) -> i32 {
        self.thread_model
    }

    /// Returns the runtime feature bitset.
    pub const fn features(&self) -> u64 {
        self.features
    }

    /// Returns the runtime feature bitset.
    pub const fn feature_bits(&self) -> u64 {
        self.features
    }

    /// Returns the number of canonical sink offers.
    pub const fn sink_count(&self) -> usize {
        self.sinks.len()
    }

    /// Returns the number of canonical contract offers.
    pub const fn contract_count(&self) -> usize {
        self.contracts.len()
    }

    /// Returns canonical sink offers as immutable primitive views.
    pub fn sinks(&self) -> impl ExactSizeIterator<Item = (&str, &[String], bool, i32)> + '_ {
        self.sinks.iter().map(|sink| {
            (
                sink.type_name.as_str(),
                sink.media_types.as_slice(),
                sink.supports_streaming,
                sink.preference_rank,
            )
        })
    }

    /// Returns one canonical sink offer as an immutable primitive view.
    pub fn sink(&self, index: usize) -> Option<(&str, &[String], bool, i32)> {
        self.sinks.get(index).map(|sink| {
            (
                sink.type_name.as_str(),
                sink.media_types.as_slice(),
                sink.supports_streaming,
                sink.preference_rank,
            )
        })
    }

    /// Returns canonical contract offers as immutable primitive views.
    pub fn contracts(&self) -> impl ExactSizeIterator<Item = (&str, u32, u32, &[String])> + '_ {
        self.contracts.iter().map(|contract| {
            (
                contract.name.as_str(),
                contract.major,
                contract.minor,
                contract.features.as_slice(),
            )
        })
    }

    /// Returns one canonical contract offer as an immutable primitive view.
    pub fn contract(&self, index: usize) -> Option<(&str, u32, u32, &[String])> {
        self.contracts.get(index).map(|contract| {
            (
                contract.name.as_str(),
                contract.major,
                contract.minor,
                contract.features.as_slice(),
            )
        })
    }

    /// Returns the canonical v1 witness encoding without the digest domain.
    pub fn canonical_bytes(&self) -> &[u8] {
        &self.canonical_bytes
    }

    /// Returns SHA-256(domain separator || canonical bytes).
    pub const fn digest(&self) -> [u8; 32] {
        self.digest
    }
}

fn validate_required_text(
    field: &'static str,
    value: &str,
) -> Result<(), RuntimeOfferWitnessError> {
    validate_text(field, value, true)
}

fn validate_nonempty_text(
    field: &'static str,
    value: &str,
) -> Result<(), RuntimeOfferWitnessError> {
    validate_text(field, value, true)
}

fn validate_text(
    field: &'static str,
    value: &str,
    require_nonempty: bool,
) -> Result<(), RuntimeOfferWitnessError> {
    let bytes = value.len();
    if bytes > MAX_TEXT_BYTES {
        return Err(RuntimeOfferWitnessError::TextTooLong {
            field,
            bytes,
            maximum: MAX_TEXT_BYTES,
        });
    }
    if require_nonempty && value.is_empty() {
        return Err(RuntimeOfferWitnessError::EmptyText { field });
    }
    if value.as_bytes().contains(&0) {
        return Err(RuntimeOfferWitnessError::TextContainsNul { field });
    }
    Ok(())
}

fn validate_runtime_policy(
    abi_major: u32,
    abi_minor: u32,
    platform: &str,
    thread_model: i32,
    features: u64,
) -> Result<(), RuntimeOfferWitnessError> {
    if abi_major != crate::runtime_loader::ABI_V1_MAJOR
        || abi_minor != crate::runtime_loader::ABI_V1_MINOR
        || platform != crate::runtime_loader::QT_PLATFORM
        || thread_model != crate::runtime_loader::AbiThreadModel::CALLER_PUMPS.0
        || features != crate::runtime_loader::RUNTIME_FEATURE_MULTIPLE_WINDOWS
    {
        return Err(RuntimeOfferWitnessError::UnsupportedDescriptorPolicy);
    }
    Ok(())
}

fn validate_accepted_runtime_offer(
    runtime_name: &str,
    sinks: &[(&str, &[&str], bool, i32)],
    contracts: &[(&str, u32, u32, &[&str])],
) -> Result<(), RuntimeOfferWitnessError> {
    if runtime_name != crate::runtime_loader::QT_RUNTIME_NAME
        || sinks.len() != 1
        || sinks[0].0 != crate::runtime_loader::UI_SINK_NAME
        || !sinks[0].1.is_empty()
        || sinks[0].2
        || sinks[0].3 != 0
        || contracts.len() != ACCEPTED_RUNTIME_CONTRACTS.len()
    {
        return Err(RuntimeOfferWitnessError::UnsupportedDescriptorPolicy);
    }
    let mut contract_names = contracts
        .iter()
        .map(|(name, _, _, _)| *name)
        .collect::<Vec<_>>();
    contract_names.sort_unstable();
    let mut expected_names = ACCEPTED_RUNTIME_CONTRACTS.to_vec();
    expected_names.sort_unstable();
    if contract_names != expected_names
        || contracts
            .iter()
            .any(|(_, major, minor, features)| *major != 1 || *minor != 0 || !features.is_empty())
    {
        return Err(RuntimeOfferWitnessError::UnsupportedDescriptorPolicy);
    }
    Ok(())
}

fn first_duplicate<T: PartialEq>(values: &[T]) -> Option<usize> {
    values
        .windows(2)
        .position(|window| window[0] == window[1])
        .map(|index| index + 1)
}

fn compare_sinks(left: &SinkSnapshot, right: &SinkSnapshot) -> Ordering {
    left.type_name
        .cmp(&right.type_name)
        .then_with(|| left.media_types.cmp(&right.media_types))
        .then_with(|| left.supports_streaming.cmp(&right.supports_streaming))
        .then_with(|| left.preference_rank.cmp(&right.preference_rank))
}

fn compare_contracts(left: &ContractSnapshot, right: &ContractSnapshot) -> Ordering {
    left.name
        .cmp(&right.name)
        .then_with(|| left.major.cmp(&right.major))
        .then_with(|| left.minor.cmp(&right.minor))
        .then_with(|| left.features.cmp(&right.features))
}

struct CanonicalEncoder {
    bytes: Vec<u8>,
}

impl CanonicalEncoder {
    fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    fn append(&mut self, value: &[u8]) -> Result<(), RuntimeOfferWitnessError> {
        let new_len = self.bytes.len().checked_add(value.len()).ok_or(
            RuntimeOfferWitnessError::CanonicalBytesTooLarge {
                bytes: usize::MAX,
                maximum: MAX_CANONICAL_BYTES,
            },
        )?;
        if new_len > MAX_CANONICAL_BYTES {
            return Err(RuntimeOfferWitnessError::CanonicalBytesTooLarge {
                bytes: new_len,
                maximum: MAX_CANONICAL_BYTES,
            });
        }
        self.bytes.extend_from_slice(value);
        Ok(())
    }

    fn push_u8(&mut self, value: u8) -> Result<(), RuntimeOfferWitnessError> {
        self.append(&[value])
    }

    fn push_u32(&mut self, value: u32) -> Result<(), RuntimeOfferWitnessError> {
        self.append(&value.to_be_bytes())
    }

    fn push_i32(&mut self, value: i32) -> Result<(), RuntimeOfferWitnessError> {
        self.append(&value.to_be_bytes())
    }

    fn push_u64(&mut self, value: u64) -> Result<(), RuntimeOfferWitnessError> {
        self.append(&value.to_be_bytes())
    }

    fn push_count(&mut self, value: usize) -> Result<(), RuntimeOfferWitnessError> {
        let value =
            u32::try_from(value).map_err(|_| RuntimeOfferWitnessError::CanonicalBytesTooLarge {
                bytes: usize::MAX,
                maximum: MAX_CANONICAL_BYTES,
            })?;
        self.push_u32(value)
    }

    fn push_text(&mut self, value: &str) -> Result<(), RuntimeOfferWitnessError> {
        let length = u32::try_from(value.len()).map_err(|_| {
            RuntimeOfferWitnessError::CanonicalBytesTooLarge {
                bytes: usize::MAX,
                maximum: MAX_CANONICAL_BYTES,
            }
        })?;
        self.push_u32(length)?;
        self.append(value.as_bytes())
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

fn digest_bytes(canonical_bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(DOMAIN_SEPARATOR);
    hasher.update(canonical_bytes);
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalises_media_features_and_contract_order() {
        let media_first = ["video/raw", "audio/raw"];
        let media_second = ["audio/raw", "video/raw"];
        let features_first = ["zeta", "alpha"];
        let features_first_ordered = ["alpha", "zeta"];
        let features_second = ["beta"];
        let contracts_first = [
            ("z.contract", 2, 1, &features_second[..]),
            ("a.contract", 1, 0, &features_first[..]),
        ];
        let contracts_second = [
            ("a.contract", 1, 0, &features_first_ordered[..]),
            ("z.contract", 2, 1, &features_second[..]),
        ];
        let first = RuntimeOfferWitness::from_parts(
            1,
            0,
            "runtime",
            "1.0",
            "build",
            "linux-x86_64",
            3,
            1,
            &[("sink", &media_first[..], true, -2)],
            &contracts_first,
        )
        .expect("first witness");
        let second = RuntimeOfferWitness::from_parts(
            1,
            0,
            "runtime",
            "1.0",
            "build",
            "linux-x86_64",
            3,
            1,
            &[("sink", &media_second[..], true, -2)],
            &contracts_second,
        )
        .expect("second witness");

        assert_eq!(first.canonical_bytes(), second.canonical_bytes());
        assert_eq!(first.digest(), second.digest());
        assert_eq!(
            first.sink(0).expect("sink").1,
            [String::from("audio/raw"), String::from("video/raw")]
        );
        assert_eq!(
            first.contract(0).expect("contract").3,
            ["alpha".to_owned(), "zeta".to_owned()]
        );
    }

    #[test]
    fn digest_is_stable_and_uses_domain_separator() {
        let witness = RuntimeOfferWitness::from_parts(
            1,
            0,
            "runtime",
            "1.0",
            "build",
            "linux-x86_64",
            3,
            1,
            &[],
            &[],
        )
        .expect("witness");
        let expected = digest_bytes(witness.canonical_bytes());

        assert_eq!(witness.digest(), expected);
        assert_eq!(witness.digest(), witness.digest());
        let digest_without_domain: [u8; 32] = {
            let mut hasher = Sha256::new();
            hasher.update(witness.canonical_bytes());
            hasher.finalize().into()
        };
        assert_ne!(witness.digest(), digest_without_domain);
    }

    #[test]
    fn rejects_unsupported_runtime_policy_before_witness_creation() {
        assert!(matches!(
            RuntimeOfferWitness::from_parts(
                2,
                0,
                "runtime",
                "1.0",
                "build",
                "linux-x86_64",
                3,
                1,
                &[],
                &[],
            ),
            Err(RuntimeOfferWitnessError::UnsupportedDescriptorPolicy)
        ));
        assert!(matches!(
            RuntimeOfferWitness::from_parts(
                1,
                0,
                "runtime",
                "1.0",
                "build",
                "linux-x86_64",
                2,
                1,
                &[],
                &[],
            ),
            Err(RuntimeOfferWitnessError::UnsupportedDescriptorPolicy)
        ));
    }

    #[test]
    fn rejects_invalid_text_and_identities() {
        assert!(matches!(
            RuntimeOfferWitness::from_parts(1, 0, "", "1", "b", "p", 3, 1, &[], &[]),
            Err(RuntimeOfferWitnessError::EmptyText { .. })
        ));
        assert!(matches!(
            RuntimeOfferWitness::from_parts(1, 0, "runtime\0", "1", "b", "p", 3, 1, &[], &[]),
            Err(RuntimeOfferWitnessError::TextContainsNul { .. })
        ));
        let empty_media: &[&str] = &[];
        assert!(matches!(
            RuntimeOfferWitness::from_parts(
                1,
                0,
                "runtime",
                "1",
                "b",
                "p",
                3,
                1,
                &[("", empty_media, false, 0)],
                &[]
            ),
            Err(RuntimeOfferWitnessError::InvalidSinkIdentity { .. })
        ));
        assert!(matches!(
            RuntimeOfferWitness::from_parts(
                1,
                0,
                "runtime",
                "1",
                "b",
                "p",
                3,
                1,
                &[],
                &[("contract", 1, 0, &["\0"][..])]
            ),
            Err(RuntimeOfferWitnessError::InvalidFeatureIdentity { .. })
        ));
        assert!(matches!(
            RuntimeOfferWitness::from_parts(
                1,
                0,
                "runtime",
                "1",
                "b",
                "p",
                3,
                1,
                &[],
                &[("", 1, 0, &[][..])]
            ),
            Err(RuntimeOfferWitnessError::InvalidContractIdentity { .. })
        ));
    }

    #[test]
    fn rejects_text_and_collection_limits() {
        let too_long = "x".repeat(MAX_TEXT_BYTES + 1);
        assert!(matches!(
            RuntimeOfferWitness::from_parts(1, 0, &too_long, "1", "b", "p", 3, 1, &[], &[]),
            Err(RuntimeOfferWitnessError::TextTooLong { .. })
        ));

        let feature_values: Vec<String> = (0..MAX_FEATURES + 1)
            .map(|index| format!("feature-{index}"))
            .collect();
        let feature_refs: Vec<&str> = feature_values.iter().map(String::as_str).collect();
        assert!(matches!(
            RuntimeOfferWitness::from_parts(
                1,
                0,
                "runtime",
                "1",
                "b",
                "p",
                3,
                1,
                &[],
                &[("contract", 1, 0, feature_refs.as_slice())]
            ),
            Err(RuntimeOfferWitnessError::TooManyFeatures { .. })
        ));

        let media_values: Vec<String> = (0..MAX_MEDIA_TYPES + 1)
            .map(|index| format!("media/{index}"))
            .collect();
        let media_refs: Vec<&str> = media_values.iter().map(String::as_str).collect();
        assert!(matches!(
            RuntimeOfferWitness::from_parts(
                1,
                0,
                "runtime",
                "1",
                "b",
                "p",
                3,
                1,
                &[("sink", media_refs.as_slice(), false, 0)],
                &[]
            ),
            Err(RuntimeOfferWitnessError::TooManyMediaTypes { .. })
        ));

        let empty_media: &[&str] = &[];
        assert!(matches!(
            RuntimeOfferWitness::from_parts(
                1,
                0,
                "runtime",
                "1",
                "b",
                "p",
                3,
                1,
                &[("a", empty_media, false, 0), ("b", empty_media, false, 0)],
                &[]
            ),
            Err(RuntimeOfferWitnessError::TooManySinks { .. })
        ));

        let contract_names = [
            "contract-0",
            "contract-1",
            "contract-2",
            "contract-3",
            "contract-4",
            "contract-5",
            "contract-6",
            "contract-7",
            "contract-8",
        ];
        let empty_features: &[&str] = &[];
        let contracts: Vec<(&str, u32, u32, &[&str])> = contract_names
            .into_iter()
            .map(|name| (name, 1, 0, empty_features))
            .collect();
        assert!(matches!(
            RuntimeOfferWitness::from_parts(
                1,
                0,
                "runtime",
                "1",
                "b",
                "p",
                3,
                1,
                &[],
                contracts.as_slice()
            ),
            Err(RuntimeOfferWitnessError::TooManyContracts { .. })
        ));
    }

    #[test]
    fn rejects_duplicate_entries() {
        let duplicate_features = ["same", "same"];
        assert!(matches!(
            RuntimeOfferWitness::from_parts(
                1,
                0,
                "runtime",
                "1",
                "b",
                "p",
                3,
                1,
                &[],
                &[("contract", 1, 0, &duplicate_features[..])]
            ),
            Err(RuntimeOfferWitnessError::DuplicateFeature { .. })
        ));

        let duplicate_media = ["same", "same"];
        assert!(matches!(
            RuntimeOfferWitness::from_parts(
                1,
                0,
                "runtime",
                "1",
                "b",
                "p",
                3,
                1,
                &[("sink", &duplicate_media[..], false, 0)],
                &[]
            ),
            Err(RuntimeOfferWitnessError::DuplicateMediaType { .. })
        ));

        let empty_features: &[&str] = &[];
        assert!(matches!(
            RuntimeOfferWitness::from_parts(
                1,
                0,
                "runtime",
                "1",
                "b",
                "p",
                3,
                1,
                &[],
                &[
                    ("contract", 1, 0, empty_features),
                    ("contract", 1, 0, empty_features)
                ]
            ),
            Err(RuntimeOfferWitnessError::DuplicateContract { .. })
        ));
    }
}
