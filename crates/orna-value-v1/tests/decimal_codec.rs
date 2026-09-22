use num_bigint::BigInt;
use orna_value_v1::{Decimal, Error};

#[test]
fn canonical_decimal_text_is_exact_and_round_trips() {
    let cases = [
        (0_i64, 0_i64, "0e0.decimal"),
        (123_400, -4, "1234e-2.decimal"),
        (-150, -2, "-15e-1.decimal"),
        (1, 1, "1e1.decimal"),
    ];

    for (coefficient, exponent, expected) in cases {
        let decimal = Decimal::try_new(BigInt::from(coefficient), BigInt::from(exponent))
            .expect("exact decimal construction");
        assert_eq!(decimal.canonical_text(), expected);
        assert_eq!(Decimal::from_canonical_text(expected), Ok(decimal));
    }
}

#[test]
fn canonical_decimal_text_rejects_aliases_and_float_spellings() {
    for input in [
        "01e0.decimal",
        "-0e0.decimal",
        "120e-2.decimal",
        "1e+0.decimal",
        "1e-0.decimal",
        "0e1.decimal",
        "1E0.decimal",
        "1.0.decimal",
    ] {
        assert!(
            matches!(
                Decimal::from_canonical_text(input),
                Err(Error::InvalidValue | Error::NonCanonical)
            ),
            "{input}"
        );
    }
}

#[test]
fn canonical_decimal_text_fails_closed_at_the_exponent_limit() {
    assert_eq!(
        Decimal::from_canonical_text("1e1000001.decimal"),
        Err(Error::DecimalLimit)
    );
    assert_eq!(
        Decimal::from_canonical_text("1e10000000.decimal"),
        Err(Error::DecimalLimit)
    );
}
