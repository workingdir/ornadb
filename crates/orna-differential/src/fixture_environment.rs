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

/// The source of the attached fixture tables and nominal types that stay under
/// `fixture.attached`. `Contact` is not here: the `contacts` database below owns it,
/// and reference cases reach it by its bare name through `use contacts.*;`.
pub const ATTACHED_SUPPORT: &str = "\
use fixture.currencies.*;
pub type Account {}
pub type Customer {}
pub type EmailHeader {
    address: Str,
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
    amount: Money<GBP>,
}
";

/// The fixture unit namespace the reference cases reach through `use fixture.units.*;`, in the declaration form of
/// units.md section 2. The old checker reads its own legacy declaration (`orna_check::FIXTURE_UNIT_SUPPORT`); the new
/// pipeline reads this one. Temperature is not declared: an affine unit is a library type on HIR (section 8.5).
pub const UNIT_SUPPORT: &str = "\
pub enum Length {
    m,
    mile = 1609.344 m,
    impl Unit {}
}
pub enum Mass {
    kg,
    impl Unit {}
}
pub enum Time {
    s,
    hour = 3600 s,
    impl Unit {}
}
pub enum Energy {
    J = kg * m^2 / s^2,
    kWh = 3600000 J,
    impl Unit {}
}
pub enum Information {
    B,
    MiB = 1048576 B,
    impl Unit {}
}
pub enum Speed {
    mph = 0.44704 m / s,
    impl Unit {}
}
";

/// The fixture currencies the old catalogue supplies to every reference module (`GBP`, `EUR`, `USD`,
/// with `Money` from `core.money`'s root environment). Each is a nominal type implementing `Currency`
/// with its code and minor digits, as ORNA-MONEY-004 requires. Reached through `use fixture.currencies.*;`.
pub const CURRENCY_SUPPORT: &str = "\
pub type GBP {
    impl Currency {
        static code = \"GBP\";
        static minor_digits = 2;
    }
}
pub type EUR {
    impl Currency {
        static code = \"EUR\";
        static minor_digits = 2;
    }
}
pub type USD {
    impl Currency {
        static code = \"USD\";
        static minor_digits = 2;
    }
}
";

/// The attached database modules, as `(logical path, source)`. The old catalogue
/// names each one by its module (`contacts`, `energy`, `mail`). The observer
/// attaches a root only to a reference case that names it (see
/// `observe_new::attached_databases`), so a case reaches `contacts.Contact` with no
/// `use`. A module that needs a unit (`mph`) imports the fixture units.
pub const DATABASE_SUPPORT: [(&str, &str); 3] = [
    ("contacts.orna", CONTACTS_SUPPORT),
    ("energy.orna", ENERGY_SUPPORT),
    ("mail.orna", MAIL_SUPPORT),
];

/// The `contacts` database.
///
/// `Contact.full_name` is the spec's computed-field example
/// (`full_name: Str => "{first} {last}"`, ORNA-FIELD-010); the old catalogue
/// records only that the field is computed.
const CONTACTS_SUPPORT: &str = "\
pub table Contact(id: Str = \"generated\") {
    first: Str = \"\",
    last: Str = \"\",
    name: Str,
    emails: [Str] = [],
    full_name: Str => \"{first} {last}\",
}
";

/// The `energy` database: the shape the old catalogue gives `Reading`.
const ENERGY_SUPPORT: &str = "\
use fixture.units.*;
pub table Reading {
    rpm: Int,
    speed: Float<mph>,
    time: Instant,
}
";

/// The `mail` database: the shape the old catalogue gives `Email`, the message
/// fields plus the provider identity a stored email carries.
const MAIL_SUPPORT: &str = "\
pub table Email {
    body_html: Str,
    body_text: Str,
    from: Str,
    id: Str,
    received: Instant,
    sent: Instant,
    subject: Str,
    to: Str,
    provider: Str,
}
";
