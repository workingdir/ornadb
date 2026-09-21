use orna_foundation_v1::{CanonicalValue, OvbRaw, SchemaDescriptor};
use orna_runtime_v1::{
    Checkpoint, PublicationEquivalenceWitness, PublicationFreeze, PublicationFreezeMutation,
    PublicationMutationState, PublicationRowEncoding, PublicationValueEncoding,
};
use orna_storage_v1::{
    lower_publication_freeze, CompactKeyError, CompactLoweringError, CompactOvbProfile,
    CompactWriterMutationState,
};
use sha2::{Digest, Sha256};

const TABLE: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
const KEY_FIELD: [u8; 16] = [0x10; 16];
const VALUE_FIELD: [u8; 16] = [0x20; 16];
const GENERATION: u64 = 27;

fn uuid_raw(value: [u8; 16]) -> OvbRaw {
    OvbRaw::Tag(37, Box::new(OvbRaw::Bytes(value.to_vec())))
}

fn primitive(name: &str) -> OvbRaw {
    OvbRaw::Array(vec![OvbRaw::Int(0.into()), OvbRaw::Text(name.into())])
}

fn field(id: [u8; 16], name: &str, ty: OvbRaw, role: u64) -> OvbRaw {
    OvbRaw::Array(vec![
        uuid_raw(id),
        OvbRaw::Text(name.into()),
        ty,
        OvbRaw::Int(role.into()),
        OvbRaw::Array(vec![OvbRaw::Int(0.into())]),
    ])
}

fn profile() -> CompactOvbProfile {
    let schema = OvbRaw::Map(vec![
        (OvbRaw::Int(0.into()), OvbRaw::Int(1.into())),
        (OvbRaw::Int(1.into()), uuid_raw(TABLE)),
        (
            OvbRaw::Int(2.into()),
            OvbRaw::Array(vec![uuid_raw(KEY_FIELD)]),
        ),
        (
            OvbRaw::Int(3.into()),
            OvbRaw::Array(vec![
                field(KEY_FIELD, "id", primitive("Int"), 0),
                field(VALUE_FIELD, "name", primitive("Str"), 1),
            ]),
        ),
        (OvbRaw::Int(4.into()), OvbRaw::Array(Vec::new())),
    ]);
    CompactOvbProfile::new(SchemaDescriptor::new(schema).unwrap()).unwrap()
}

fn ovb(value: OvbRaw) -> Vec<u8> {
    CanonicalValue::new(value).unwrap().encode().unwrap()
}

fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn candidate_digest(generation: u64, mutations: &[(u64, &[u8], Option<&[u8]>)]) -> [u8; 32] {
    let mut bytes = Vec::from(&b"ORNA-COMPACT-CANDIDATE-1\0"[..]);
    bytes.extend_from_slice(&generation.to_be_bytes());
    for (sequence, key, value) in mutations {
        bytes.extend_from_slice(&sequence.to_be_bytes());
        bytes.extend_from_slice(&(key.len() as u64).to_be_bytes());
        bytes.extend_from_slice(key);
        bytes.push(if value.is_some() { 1 } else { 2 });
        let value = value.unwrap_or_default();
        bytes.extend_from_slice(&(value.len() as u64).to_be_bytes());
        bytes.extend_from_slice(value);
    }
    digest(&bytes)
}

fn freeze_fixture() -> (CompactOvbProfile, PublicationFreeze, Vec<u8>, [u8; 32]) {
    let profile = profile();
    let key_one = ovb(OvbRaw::Int(7.into()));
    let key_two = ovb(OvbRaw::Int(8.into()));
    let value_one = ovb(OvbRaw::Text("Ada".into()));
    let mutations = vec![
        (1, key_one.as_slice(), Some(value_one.as_slice())),
        (2, key_two.as_slice(), None),
    ];
    let candidate = candidate_digest(GENERATION, &mutations);
    let freeze = PublicationFreeze {
        intent_id: [0x41; 16],
        checkpoint: Checkpoint {
            generation: GENERATION,
            digest: [0x42; 32],
            mutation_sequence: 2,
        },
        candidate_digest: candidate,
        mutations: vec![
            PublicationFreezeMutation {
                table_id: TABLE,
                sequence: 1,
                mutation_id: [0x51; 16],
                table: "accounts".into(),
                logical_key: key_one.clone(),
                state: PublicationMutationState::Replacement {
                    value: value_one.clone(),
                },
                schema_fingerprint: profile.schema_fingerprint(),
                row_encoding_identity: PublicationRowEncoding::CompactOvb1,
                value_encoding_identity: PublicationValueEncoding::Ovb1,
                candidate_generation: GENERATION,
                equivalence_witness: PublicationEquivalenceWitness {
                    key_digest: digest(&key_one),
                    value_digest: Some(digest(&value_one)),
                    candidate_digest: candidate,
                },
            },
            PublicationFreezeMutation {
                table_id: TABLE,
                sequence: 2,
                mutation_id: [0x52; 16],
                table: "accounts".into(),
                logical_key: key_two.clone(),
                state: PublicationMutationState::Deletion,
                schema_fingerprint: profile.schema_fingerprint(),
                row_encoding_identity: PublicationRowEncoding::CompactOvb1,
                value_encoding_identity: PublicationValueEncoding::Ovb1,
                candidate_generation: GENERATION,
                equivalence_witness: PublicationEquivalenceWitness {
                    key_digest: digest(&key_two),
                    value_digest: None,
                    candidate_digest: candidate,
                },
            },
        ],
    };
    (profile, freeze, value_one, candidate)
}

#[test]
fn lowers_runtime_freeze_to_ordered_writer_input() {
    let (profile, freeze, value_one, candidate) = freeze_fixture();
    let input = lower_publication_freeze(&profile, &freeze).unwrap();

    assert_eq!(input.table_id, TABLE);
    assert_eq!(input.schema_fingerprint, profile.schema_fingerprint());
    assert_eq!(input.candidate_generation, GENERATION);
    assert_eq!(input.row_encoding_identity, PublicationRowEncoding::CompactOvb1);
    assert_eq!(input.value_encoding_identity, PublicationValueEncoding::Ovb1);
    assert_eq!(input.candidate_digest, candidate);
    assert_eq!(input.mutations.len(), 2);
    assert_eq!(input.mutations[0].sequence, 1);
    assert_eq!(input.mutations[0].mutation_id, [0x51; 16]);
    assert_eq!(input.mutations[0].key.encoded(), freeze.mutations[0].logical_key);
    assert!(matches!(
        &input.mutations[0].state,
        CompactWriterMutationState::Replacement { value } if value == &value_one
    ));
    assert_eq!(input.mutations[1].sequence, 2);
    assert_eq!(input.mutations[1].mutation_id, [0x52; 16]);
    assert_eq!(input.mutations[1].key.encoded(), freeze.mutations[1].logical_key);
    assert!(matches!(input.mutations[1].state, CompactWriterMutationState::Deletion));
}

#[test]
fn rejects_malformed_or_stale_typed_freezes() {
    let (profile, freeze, _, _) = freeze_fixture();

    let mut duplicate_sequence = freeze.clone();
    duplicate_sequence.mutations[1].sequence = 1;
    assert!(matches!(
        lower_publication_freeze(&profile, &duplicate_sequence),
        Err(CompactLoweringError::SequenceOutOfOrder)
    ));

    let mut duplicate_id = freeze.clone();
    duplicate_id.mutations[1].mutation_id = duplicate_id.mutations[0].mutation_id;
    assert!(matches!(
        lower_publication_freeze(&profile, &duplicate_id),
        Err(CompactLoweringError::DuplicateMutationId)
    ));

    let mut wrong_schema = freeze.clone();
    wrong_schema.mutations[0].schema_fingerprint[0] ^= 1;
    assert!(matches!(
        lower_publication_freeze(&profile, &wrong_schema),
        Err(CompactLoweringError::WrongSchema)
    ));

    let mut wrong_table = freeze.clone();
    wrong_table.mutations[0].table_id[0] ^= 1;
    assert!(matches!(
        lower_publication_freeze(&profile, &wrong_table),
        Err(CompactLoweringError::WrongTable)
    ));

    let mut wrong_generation = freeze.clone();
    wrong_generation.mutations[0].candidate_generation += 1;
    assert!(matches!(
        lower_publication_freeze(&profile, &wrong_generation),
        Err(CompactLoweringError::StaleGeneration)
    ));

    let mut malformed_key = freeze.clone();
    malformed_key.mutations[0].logical_key = vec![0xff];
    assert!(matches!(
        lower_publication_freeze(&profile, &malformed_key),
        Err(CompactLoweringError::InvalidKey(CompactKeyError::InvalidOvb))
    ));

    let mut wrong_key_witness = freeze.clone();
    wrong_key_witness.mutations[0].equivalence_witness.key_digest[0] ^= 1;
    assert!(matches!(
        lower_publication_freeze(&profile, &wrong_key_witness),
        Err(CompactLoweringError::KeyWitnessMismatch)
    ));

    let mut wrong_value_witness = freeze.clone();
    wrong_value_witness.mutations[0]
        .equivalence_witness
        .value_digest = Some([0x99; 32]);
    assert!(matches!(
        lower_publication_freeze(&profile, &wrong_value_witness),
        Err(CompactLoweringError::ValueWitnessMismatch)
    ));

    let mut wrong_candidate_witness = freeze.clone();
    wrong_candidate_witness.mutations[1]
        .equivalence_witness
        .candidate_digest[0] ^= 1;
    assert!(matches!(
        lower_publication_freeze(&profile, &wrong_candidate_witness),
        Err(CompactLoweringError::CandidateWitnessMismatch)
    ));

    let mut wrong_candidate_digest = freeze;
    for mutation in &mut wrong_candidate_digest.mutations {
        mutation.equivalence_witness.candidate_digest = [0xaa; 32];
    }
    wrong_candidate_digest.candidate_digest = [0xaa; 32];
    assert!(matches!(
        lower_publication_freeze(&profile, &wrong_candidate_digest),
        Err(CompactLoweringError::CandidateDigestMismatch)
    ));
}
