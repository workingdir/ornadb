//! The `sys.RowRef<T>` of every canonical relation: construction, canonical encoding, strict
//! decoding and agreement with the dedicated constructors that predate the registry.

use std::collections::BTreeSet;

use num_bigint::BigInt;
use orna_value::{
    CanonicalSnapshot, CanonicalValue, GitHash, GitObjectId, KeyPart, KeyPartKind, OvbRaw,
    PathFlavour, RowRef, SysRef, SystemReferenceError, branch_reference, checkpoint_reference,
    commit_reference, failure_reference, function_reference, git_object_reference,
    git_ref_reference, invocation_argument_reference, invocation_reference,
    materialization_reference, object_keyed_reference, object_reference, relations,
    remote_reference, snapshot_reference, stash_reference, tag_reference, type_reference,
};

const DATABASE: [u8; 16] = [1; 16];

fn pin() -> CanonicalSnapshot {
    CanonicalSnapshot::cwd(DATABASE, [2; 16], 3.into()).expect("snapshot")
}

fn oid(fill: u8) -> GitObjectId {
    GitObjectId::new(GitHash::Sha1, vec![fill; 20]).expect("sha1 object id")
}

fn uuid_raw(bytes: [u8; 16]) -> OvbRaw {
    OvbRaw::Tag(37, Box::new(OvbRaw::Bytes(bytes.to_vec())))
}

fn decoded(reference: &SysRef) -> SysRef {
    let bytes = reference.as_row_ref().encode().expect("encodes");
    SysRef::from_row_ref(RowRef::decode(&bytes).expect("decodes")).expect("a checked reference")
}

// Specified: ORNA-SYS-111, ORNA-SYS-112; Orna-1.1.0 sys.Branch, source/29-formats.md tag 60010.
#[test]
fn a_branch_reference_is_the_pinned_row_reference_of_its_name() {
    let reference = branch_reference(DATABASE, pin(), "main").expect("branch reference");
    // The key is the natural key and nothing else: no synthetic identifier is introduced.
    let expected = OvbRaw::Tag(
        60010,
        Box::new(OvbRaw::Array(vec![
            uuid_raw(DATABASE),
            uuid_raw([
                0x1f, 0x13, 0x00, 0x0e, 0x27, 0xd5, 0x45, 0xae, 0x97, 0xcf, 0x8d, 0xbc, 0xf3, 0xd4,
                0xc7, 0x32,
            ]),
            OvbRaw::Text("main".into()),
            pin().raw(),
        ])),
    );
    assert_eq!(
        reference.to_canonical().expect("canonical").raw(),
        &expected
    );
    assert_eq!(reference.relation().name, "sys.Branch");
    assert_eq!(reference.relation().reference_type, "sys.BranchRef");
    assert_eq!(reference.key_text(), Some("main"));
    assert_eq!(decoded(&reference), reference);
}

// Specified: ORNA-SYS-111, ORNA-SYS-112; Orna-1.1.0 sys.Commit and sys.GitOid.
#[test]
fn a_commit_reference_keeps_the_git_object_id_as_its_natural_key() {
    let reference = commit_reference(DATABASE, pin(), oid(0xab)).expect("commit reference");
    let OvbRaw::Array(parts) = &reference.as_row_ref().key else {
        panic!("a sys.GitOid key is [algorithm, bytes]");
    };
    assert_eq!(
        parts,
        &vec![OvbRaw::Text("sha1".into()), OvbRaw::Bytes(vec![0xab; 20])]
    );
    assert_eq!(reference.key_oid(), Some(oid(0xab)));
    assert_eq!(reference.key_text(), None);
    assert_eq!(decoded(&reference), reference);

    let sha256 = GitObjectId::new(GitHash::Sha256, vec![0xcd; 32]).expect("sha256 object id");
    let wide = commit_reference(DATABASE, pin(), sha256.clone()).expect("sha256 commit");
    assert_eq!(wide.key_oid(), Some(sha256));
    assert_ne!(
        wide.as_row_ref().encode().unwrap(),
        reference.as_row_ref().encode().unwrap()
    );
}

#[test]
fn a_git_object_id_has_exactly_the_width_of_its_algorithm() {
    assert_eq!(
        GitObjectId::new(GitHash::Sha1, vec![0; 19]),
        Err(SystemReferenceError::InvalidRelationKey)
    );
    assert_eq!(
        GitObjectId::new(GitHash::Sha256, vec![0; 20]),
        Err(SystemReferenceError::InvalidRelationKey)
    );
}

// Specified: ORNA-SYS-113 (a reference is not its row), ORNA-SYS-125 (typed reference per relation).
#[test]
fn references_of_different_relations_never_compare_equal() {
    let branch = branch_reference(DATABASE, pin(), "main").unwrap();
    let tag = tag_reference(DATABASE, pin(), "main").unwrap();
    let remote = remote_reference(DATABASE, pin(), "main").unwrap();
    let git_ref = git_ref_reference(DATABASE, pin(), "main").unwrap();
    let all = [&branch, &tag, &remote, &git_ref];
    for (index, left) in all.iter().enumerate() {
        for right in &all[index + 1..] {
            assert_ne!(left, right);
            assert!(!left.is_of(right.relation()));
        }
    }
    let commit = commit_reference(DATABASE, pin(), oid(1)).unwrap();
    let object = git_object_reference(DATABASE, pin(), oid(1)).unwrap();
    assert_ne!(commit, object);
}

#[test]
fn a_reference_resolves_in_its_pin_so_another_pin_is_another_reference() {
    let first = branch_reference(DATABASE, pin(), "main").unwrap();
    let again = branch_reference(DATABASE, pin(), "main").unwrap();
    let later = CanonicalSnapshot::cwd(DATABASE, [2; 16], 4.into()).unwrap();
    let moved = branch_reference(DATABASE, later, "main").unwrap();
    assert_eq!(first, again);
    assert_eq!(
        first.as_row_ref().encode().unwrap(),
        again.as_row_ref().encode().unwrap()
    );
    assert_ne!(first, moved);
}

#[test]
fn a_snapshot_of_another_database_is_refused() {
    assert_eq!(
        branch_reference([9; 16], pin(), "main"),
        Err(SystemReferenceError::DatabaseMismatch)
    );
}

#[test]
fn the_natural_key_must_have_the_declared_shape() {
    assert_eq!(
        branch_reference(DATABASE, pin(), ""),
        Err(SystemReferenceError::InvalidRelationKey),
        "a branch has a nonempty name"
    );
    // A key of the wrong arity or representation never builds.
    assert_eq!(
        SysRef::new(&relations::BRANCH, DATABASE, pin(), vec![]),
        Err(SystemReferenceError::InvalidRelationKey)
    );
    assert_eq!(
        SysRef::new(
            &relations::BRANCH,
            DATABASE,
            pin(),
            vec![KeyPart::GitOid(oid(1))]
        ),
        Err(SystemReferenceError::InvalidRelationKey)
    );
    assert_eq!(
        SysRef::new(
            &relations::STASH,
            DATABASE,
            pin(),
            vec![KeyPart::Text("0".into())]
        ),
        Err(SystemReferenceError::InvalidRelationKey)
    );
}

#[test]
fn a_stored_reference_is_checked_against_its_relation_on_decode() {
    let good = branch_reference(DATABASE, pin(), "main")
        .unwrap()
        .into_row_ref();
    let mut wrong_key = good.clone();
    wrong_key.key = OvbRaw::Bytes(vec![0; 4]);
    assert_eq!(
        SysRef::from_row_ref(wrong_key),
        Err(SystemReferenceError::InvalidRelationKey)
    );
    let mut empty = good.clone();
    empty.key = OvbRaw::Text(String::new());
    assert_eq!(
        SysRef::from_row_ref(empty),
        Err(SystemReferenceError::InvalidRelationKey)
    );
    let mut unknown = good;
    unknown.table_id = [0xee; 16];
    assert_eq!(
        SysRef::from_row_ref(unknown),
        Err(SystemReferenceError::UnknownRelation)
    );
}

#[test]
fn a_nested_reference_must_name_a_row_of_the_field_relation() {
    let object = object_reference(DATABASE, [5; 16], pin()).unwrap();
    let column = object_keyed_reference(&relations::COLUMN, object.clone()).unwrap();
    // `sys.Reference.from_column` is a `sys.ColumnRef`.
    let reference = SysRef::new(
        &relations::REFERENCE,
        DATABASE,
        pin(),
        vec![KeyPart::Reference(column.as_row_ref().clone())],
    )
    .expect("a reference keyed by its column");
    assert_eq!(decoded(&reference), reference);
    // A `sys.TableRef` is not a `sys.ColumnRef`, even for the same object.
    let table = object_keyed_reference(&relations::TABLE, object.clone()).unwrap();
    assert_eq!(
        SysRef::new(
            &relations::REFERENCE,
            DATABASE,
            pin(),
            vec![KeyPart::Reference(table.into_row_ref())]
        ),
        Err(SystemReferenceError::InvalidRelationKey)
    );
    // A reference of another database is refused.
    let foreign_pin = CanonicalSnapshot::cwd([8; 16], [2; 16], 3.into()).unwrap();
    let foreign = object_reference([8; 16], [5; 16], foreign_pin).unwrap();
    let foreign_column = object_keyed_reference(&relations::COLUMN, foreign).unwrap();
    assert_eq!(
        SysRef::new(
            &relations::REFERENCE,
            DATABASE,
            pin(),
            vec![KeyPart::Reference(foreign_column.into_row_ref())]
        ),
        Err(SystemReferenceError::InvalidRelationKey)
    );
}

#[test]
fn composite_keys_hold_their_components_in_declaration_order() {
    let object = object_reference(DATABASE, [5; 16], pin()).unwrap();
    let function = function_reference(object.clone()).unwrap();
    // `sys.Effect` is keyed by `function + kind + target?`.
    let without_target = SysRef::new(
        &relations::EFFECT,
        DATABASE,
        pin(),
        vec![
            KeyPart::Reference(function.as_row_ref().clone()),
            KeyPart::Variant("write".into()),
            KeyPart::Null,
        ],
    )
    .expect("an effect without a target");
    let with_target = SysRef::new(
        &relations::EFFECT,
        DATABASE,
        pin(),
        vec![
            KeyPart::Reference(function.as_row_ref().clone()),
            KeyPart::Variant("write".into()),
            KeyPart::Reference(object.as_row_ref().clone()),
        ],
    )
    .expect("an effect with a target");
    assert_ne!(without_target, with_target);
    assert_eq!(decoded(&without_target), without_target);
    assert_eq!(decoded(&with_target), with_target);
    let OvbRaw::Array(parts) = &without_target.as_row_ref().key else {
        panic!("a composite key is an array");
    };
    assert_eq!(parts.len(), 3);
    assert_eq!(parts[2], OvbRaw::Null);
    // The variant is bound to its enum: another enum's variant is not an `EffectKind`.
    let mut forged = without_target.into_row_ref();
    let OvbRaw::Array(parts) = &mut forged.key else {
        unreachable!();
    };
    parts[1] = OvbRaw::Tag(
        60008,
        Box::new(OvbRaw::Array(vec![
            uuid_raw([0x11; 16]),
            uuid_raw([0x22; 16]),
            OvbRaw::Null,
        ])),
    );
    assert_eq!(
        SysRef::from_row_ref(forged),
        Err(SystemReferenceError::InvalidRelationKey)
    );
}

#[test]
fn text_and_path_and_index_keys_round_trip() {
    let stash = stash_reference(DATABASE, pin(), 7).unwrap();
    assert_eq!(decoded(&stash), stash);
    assert_eq!(
        stash.as_row_ref().key,
        OvbRaw::Int(BigInt::from(7)),
        "the stash index is its own key"
    );
    let tree = git_object_reference(DATABASE, pin(), oid(3)).unwrap();
    let entry = SysRef::new(
        &relations::TREE_ENTRY,
        DATABASE,
        pin(),
        vec![
            KeyPart::Reference(tree.into_row_ref()),
            KeyPart::Path {
                flavour: PathFlavour::Repo,
                text: "src/lib.orna".into(),
            },
        ],
    )
    .expect("a tree entry");
    assert_eq!(decoded(&entry), entry);
}

// The dedicated constructors predate the registry and their encodings are stored: the registry
// must reproduce them bit for bit.
#[test]
fn the_registry_reproduces_every_dedicated_constructor_bit_for_bit() {
    let snapshot = pin();
    let object = object_reference(DATABASE, [5; 16], snapshot.clone()).unwrap();
    let object_row = object.as_row_ref().clone();
    let generic = |relation: &'static orna_value::RelationSpec, key: Vec<KeyPart>| {
        SysRef::new(relation, DATABASE, snapshot.clone(), key).unwrap()
    };

    assert_eq!(
        generic(
            &relations::OBJECT,
            vec![KeyPart::Uuid([5; 16]), KeyPart::Pin]
        )
        .as_row_ref(),
        object.as_row_ref()
    );
    assert_eq!(
        generic(
            &relations::TYPE,
            vec![KeyPart::Reference(object_row.clone())]
        )
        .as_row_ref(),
        type_reference(object.clone()).unwrap().as_row_ref()
    );
    assert_eq!(
        generic(
            &relations::FUNCTION,
            vec![KeyPart::Reference(object_row.clone())]
        )
        .as_row_ref(),
        function_reference(object.clone()).unwrap().as_row_ref()
    );
    assert_eq!(
        generic(&relations::SNAPSHOT, vec![KeyPart::PinId]).as_row_ref(),
        snapshot_reference(DATABASE, snapshot.clone())
            .unwrap()
            .as_row_ref()
    );
    let invocation = invocation_reference(DATABASE, snapshot.clone(), [6; 16]).unwrap();
    assert_eq!(
        generic(&relations::INVOCATION, vec![KeyPart::Uuid([6; 16])]).as_row_ref(),
        invocation.as_row_ref()
    );
    assert_eq!(
        generic(
            &relations::INVOCATION_ARGUMENT,
            vec![
                KeyPart::Reference(invocation.as_row_ref().clone()),
                KeyPart::Int(2)
            ]
        )
        .as_row_ref(),
        invocation_argument_reference(DATABASE, snapshot.clone(), [6; 16], BigInt::from(2))
            .unwrap()
            .as_row_ref()
    );
    assert_eq!(
        generic(
            &relations::CHECKPOINT,
            vec![
                KeyPart::Text("consumer".into()),
                KeyPart::Text("source".into()),
                KeyPart::Text("part".into())
            ]
        )
        .as_row_ref(),
        checkpoint_reference(
            DATABASE,
            snapshot.clone(),
            "consumer".into(),
            "source".into(),
            Some("part".into())
        )
        .unwrap()
        .as_row_ref()
    );
    assert_eq!(
        generic(
            &relations::FAILURE,
            vec![
                KeyPart::Text("consumer".into()),
                KeyPart::Text("source".into()),
                KeyPart::Null,
                KeyPart::Text("format".into()),
                KeyPart::Text("position".into())
            ]
        )
        .as_row_ref(),
        failure_reference(
            DATABASE,
            snapshot.clone(),
            "consumer".into(),
            "source".into(),
            None,
            "format".into(),
            "position".into()
        )
        .unwrap()
        .as_row_ref()
    );
    assert_eq!(
        generic(
            &relations::MATERIALIZATION,
            vec![KeyPart::Digest([1; 32]), KeyPart::Digest([2; 32])]
        )
        .as_row_ref(),
        materialization_reference(DATABASE, snapshot, [1; 32], [2; 32])
            .unwrap()
            .as_row_ref()
    );
}

#[test]
fn a_snapshot_reference_is_keyed_by_the_identity_of_its_own_pin() {
    let reference = snapshot_reference(DATABASE, pin()).unwrap();
    let mut forged = reference.into_row_ref();
    forged.key = OvbRaw::Bytes(vec![0; 32]);
    assert_eq!(
        SysRef::from_row_ref(forged),
        Err(SystemReferenceError::InvalidRelationKey),
        "a snapshot reference never accepts a rebindable identity"
    );
}

// A key component of every kind, so every relation's natural key can be built.
fn sample(kind: KeyPartKind, depth: usize) -> KeyPart {
    match kind {
        KeyPartKind::Uuid => KeyPart::Uuid([7; 16]),
        KeyPartKind::Digest => KeyPart::Digest([8; 32]),
        KeyPartKind::GitOid => KeyPart::GitOid(oid(9)),
        KeyPartKind::Text => KeyPart::Text("name".into()),
        KeyPartKind::Int => KeyPart::Int(5),
        KeyPartKind::Path => KeyPart::Path {
            flavour: PathFlavour::Repo,
            text: "a/b".into(),
        },
        KeyPartKind::Pin => KeyPart::Pin,
        KeyPartKind::PinId => KeyPart::PinId,
        KeyPartKind::Reference(target) => {
            let relation = relations::by_name(target).expect("a registered target");
            KeyPart::Reference(sample_reference(relation, depth + 1).into_row_ref())
        }
        KeyPartKind::Enum(_) => KeyPart::Variant("variant".into()),
        KeyPartKind::Canonical => {
            KeyPart::Canonical(CanonicalValue::new(OvbRaw::Text("target".into())).unwrap())
        }
        KeyPartKind::OptionalText
        | KeyPartKind::OptionalReference(_)
        | KeyPartKind::OptionalCanonical => KeyPart::Null,
    }
}

fn sample_reference(relation: &'static orna_value::RelationSpec, depth: usize) -> SysRef {
    assert!(depth < 8, "reference keys nest finitely");
    let key = relation
        .key
        .iter()
        .map(|field| sample(field.kind, depth))
        .collect();
    SysRef::new(relation, DATABASE, pin(), key)
        .unwrap_or_else(|error| panic!("{} is constructible: {error}", relation.name))
}

// Specified: ORNA-SYS-111, ORNA-SYS-125: every canonical relation has a constructible, typed reference.
#[test]
fn every_canonical_relation_has_a_constructible_reference() {
    assert_eq!(relations::ALL.len(), 78);
    for relation in relations::ALL {
        let reference = sample_reference(relation, 0);
        assert_eq!(reference.relation(), relation);
        assert_eq!(decoded(&reference), reference, "{}", relation.name);
        assert_eq!(
            relations::by_table_id(relation.table_id),
            Some(relation),
            "{}",
            relation.name
        );
        assert_eq!(relations::by_name(relation.name), Some(relation));
    }
}

#[test]
fn the_registry_names_each_relation_and_physical_identity_once() {
    let mut names = BTreeSet::new();
    let mut tables = BTreeSet::new();
    for relation in relations::ALL {
        assert!(names.insert(relation.name), "{}", relation.name);
        assert!(tables.insert(relation.table_id), "{}", relation.name);
        assert_eq!(
            relation.reference_type,
            format!("{}Ref", relation.name),
            "{}",
            relation.name
        );
        assert!(!relation.key.is_empty(), "{}", relation.name);
    }
}

#[test]
fn an_object_keyed_reference_is_pinned_where_its_object_is() {
    let later = CanonicalSnapshot::cwd(DATABASE, [2; 16], 9.into()).unwrap();
    let object = object_reference(DATABASE, [5; 16], later.clone()).unwrap();
    let column = object_keyed_reference(&relations::COLUMN, object.clone()).unwrap();
    assert_eq!(column.as_row_ref().snapshot, later);
    assert_eq!(column.as_row_ref().database_id, DATABASE);
    // `sys.Branch` is not keyed by an object.
    assert_eq!(
        object_keyed_reference(&relations::BRANCH, object),
        Err(SystemReferenceError::InvalidRelationKey)
    );
}
