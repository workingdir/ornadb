use num_bigint::BigInt;
use orna_value_v1::{Raw, argument_identity};

fn scalar_type(name: &str) -> Raw {
    Raw::Array(vec![Raw::Int(0.into()), Raw::Text(name.to_owned())])
}

#[test]
fn matching_typed_arguments_have_a_stable_identity() {
    let bool_type = scalar_type("Bool");
    let arguments = vec![
        ("enabled".to_owned(), bool_type.clone(), Raw::Bool(true)),
        (
            "count".to_owned(),
            scalar_type("Int"),
            Raw::Int(BigInt::from(3)),
        ),
    ];
    let reordered = vec![arguments[1].clone(), arguments[0].clone()];

    assert_eq!(
        argument_identity(arguments).expect("matching typed arguments are valid"),
        argument_identity(reordered).expect("argument order does not affect identity"),
    );
}

#[test]
fn argument_identity_rejects_descriptor_value_mismatches() {
    let mismatch = argument_identity(vec![(
        "enabled".to_owned(),
        scalar_type("Bool"),
        Raw::Int(BigInt::from(1)),
    )]);

    assert!(mismatch.is_err());
}

#[test]
fn argument_identity_normalizes_names_before_order_and_collision_checks() {
    let bool_type = scalar_type("Bool");
    let composed_name = "é".to_owned();
    let decomposed_name = "e\u{301}".to_owned();
    let composed = argument_identity(vec![(
        composed_name.clone(),
        bool_type.clone(),
        Raw::Bool(true),
    )])
    .expect("composed argument name is valid");
    let decomposed = argument_identity(vec![(
        decomposed_name.clone(),
        bool_type.clone(),
        Raw::Bool(true),
    )])
    .expect("decomposed argument name is valid");
    assert_eq!(composed, decomposed);

    assert!(
        argument_identity(vec![
            (composed_name, bool_type.clone(), Raw::Bool(true)),
            (decomposed_name, bool_type, Raw::Bool(false)),
        ])
        .is_err()
    );
}

#[test]
fn argument_identity_rejects_invalid_type_descriptors() {
    let invalid_type = Raw::Array(vec![Raw::Int(0.into()), Raw::Text("Unknown".to_owned())]);

    assert!(argument_identity(vec![("value".to_owned(), invalid_type, Raw::Null,)]).is_err());
}
