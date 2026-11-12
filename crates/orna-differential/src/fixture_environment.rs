//! The attached fixture environment the reference corpus is admitted against.
//!
//! The old checker's fixture catalogue (`orna-check`'s `fixture_attached_symbols`)
//! attaches tables and nominal types to every reference case without a
//! declaration in the case. The HIR has no catalogue, so the observer supplies
//! the same declarations as Orna source: an unowned module the reference cases
//! reach through `use fixture.attached.*;`, the way they reach the fixture units.
//!
//! Only declarations whose shape the old catalogue states are written here.
//! Names the catalogue gives no shape for, and connector functions and native
//! callables that have no body in source, stay in the environment ratchet and
//! are not invented.

/// The source of the attached fixture tables and nominal types.
///
/// `Contact.full_name` is the spec's computed-field example
/// (`full_name: Str => "{first} {last}"`, ORNA-FIELD-010); the old catalogue
/// records only that the field is computed.
pub const ATTACHED_SUPPORT: &str = "\
pub type Account {}
pub type Customer {}
pub type EmailHeader {
    address: Str,
}
pub table Contact(id: Str = \"generated\") {
    first: Str = \"\",
    last: Str = \"\",
    name: Str,
    emails: [Str] = [],
    full_name: Str => \"{first} {last}\",
}
pub table Note {
    created: Instant,
    text: Str,
}
pub table Order {
    created: Instant,
    customer: Customer,
}
pub table Audit {
    action: Str,
    order: Order,
}
pub table Payment {
    order: Order,
    amount: Int,
}
";
