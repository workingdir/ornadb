//! The H0 environment ratchet.
//!
//! The reference corpus is admitted against an environment the HIR does not
//! carry: fixture tables and modules, connector and host names, and the fixture
//! unit names. Lowering reports each use of such a name as "not in scope" or "not
//! a declared type". Those uses are counted here by category, and the count is
//! ratcheted through `[[environment]]` tables in
//! `tests/differential/expected-differences.toml`.
//!
//! A count may only shrink as real declarations arrive. The test fails when a
//! count moves in either direction, so a shrink must be recorded in the same
//! change, and a name in no class is an explicit category rather than silence.

use std::collections::BTreeMap;

use crate::{differential_dir, read_file, toml_lite};

/// Connector and host names. Documented as a not-in-scope class: the HIR does
/// not invent declarations for them.
pub const CONNECTOR_AND_HOST: &str = "connector-and-host-names";

/// Fixture tables and modules. Lowered from the old Rust `Type` objects only
/// once real declarations exist; until then they are an H0 environment class.
pub const FIXTURE_TABLES: &str = "fixture-tables-and-modules";

/// Currency and fixture unit names. Currencies exist only as a legacy parser
/// construct, and the fixture units are supplied by the observer.
pub const CURRENCY_AND_UNIT: &str = "currency-and-unit-names";

/// Legacy 1.0.0 programs. The HIR does not reject them with the published
/// code, so each use is an expected difference. The environment test asserts
/// the published code against the old checker instead.
pub const LEGACY_REJECTION: &str = "legacy-rejection-names";

/// Harness bindings: names the expression and example harness supplies and the
/// case does not declare.
pub const HARNESS_ENVIRONMENT: &str = "harness-environment-names";

/// Stdlib names used without the import that declares them.
pub const MISSING_IMPORT: &str = "missing-import-names";

/// Names that await a later phase of the typed pipeline (phase 3b).
pub const PENDING_P3B: &str = "pending-p3b-names";

/// Names the HIR must declare before they resolve: core enums, `Self`, and the
/// relational predicate constructors.
pub const IMPLEMENT: &str = "implement-declaration-names";

/// Names no class above covers. Not accepted as environment: each needs a
/// per-name decision, either a HIR declaration or a resolver defect.
pub const UNCLASSIFIED: &str = "unclassified-names";

/// Every category a ratchet entry may name, in report order.
pub const CATEGORIES: [&str; 9] = [
    CONNECTOR_AND_HOST,
    FIXTURE_TABLES,
    CURRENCY_AND_UNIT,
    LEGACY_REJECTION,
    HARNESS_ENVIRONMENT,
    MISSING_IMPORT,
    PENDING_P3B,
    IMPLEMENT,
    UNCLASSIFIED,
];

/// The name a lowering diagnostic reports as unresolved, if it reports one.
fn unresolved_name(diagnostic: &str) -> Option<&str> {
    const MARKERS: [&str; 3] = [
        "is not in scope",
        "is not a declared type",
        "is not an export of the module",
    ];
    if !MARKERS.iter().any(|marker| diagnostic.contains(marker)) {
        return None;
    }
    let start = diagnostic.find('`')? + 1;
    let end = start + diagnostic[start..].find('`')?;
    Some(&diagnostic[start..end])
}

/// Whether a case is a legacy 1.0.0 rejection program, by its file name.
#[must_use]
pub fn is_legacy_case(case_id: &str) -> bool {
    case_id
        .rsplit('/')
        .next()
        .is_some_and(|file| file.starts_with("legacy-"))
}

/// The category of one lowering diagnostic of one case, or `None` when it is
/// not an unresolved name at all. The name classes come first, then the legacy
/// case class, then the per-name classes.
#[must_use]
pub fn classify(case_id: &str, diagnostic: &str) -> Option<&'static str> {
    let name = unresolved_name(diagnostic)?;
    Some(match name {
        "mail" | "google" | "finance" | "energy" | "HEAD" | "CWD" | "work" => CONNECTOR_AND_HOST,
        "kWh" | "mph" | "A" | "C" => CURRENCY_AND_UNIT,
        "Contact" | "contacts" | "Message" | "Audit" | "Order" | "Note" | "Page" | "Customer"
        | "Account" | "Payment" | "Reading" | "Status" | "Locale" | "DisplayContext" | "Europe"
        | "vehicle" | "openbanking" | "bank" => FIXTURE_TABLES,
        _ if is_legacy_case(case_id) => LEGACY_REJECTION,
        "log" | "process" | "read" | "rows" | "get" | "valid_email" | "fallback" | "left"
        | "middle" | "missing" | "EmailHeader" => HARNESS_ENVIRONMENT,
        "for_each" | "mean" | "ref" | "open" => MISSING_IMPORT,
        "Str" if case_id.ends_with("secret-display.orna") => PENDING_P3B,
        "Ordering" | "List" => IMPLEMENT,
        _ => UNCLASSIFIED,
    })
}

/// One recorded environment category and its measured count.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnvironmentCategory {
    /// The category, one of [`CATEGORIES`].
    pub category: String,
    /// The measured number of lowering diagnostics in the category.
    pub count: usize,
    /// Why the category is accepted.
    pub reason: String,
    /// The bead that closes it.
    pub bead: String,
}

/// Reads the `[[environment]]` tables from `expected-differences.toml`.
pub fn load_environment_categories() -> Result<Vec<EnvironmentCategory>, String> {
    let path = differential_dir().join("expected-differences.toml");
    parse_environment_categories(&read_file(&path)?)
}

/// Parses the `[[environment]]` tables.
pub fn parse_environment_categories(source: &str) -> Result<Vec<EnvironmentCategory>, String> {
    let document = toml_lite::parse(source)?;
    let mut categories: Vec<EnvironmentCategory> = Vec::new();
    for table in document.tables_named("environment") {
        let category = table.string("category")?.to_owned();
        if !CATEGORIES.contains(&category.as_str()) {
            return Err(format!("environment: unknown category `{category}`"));
        }
        if categories.iter().any(|entry| entry.category == category) {
            return Err(format!(
                "environment: category `{category}` is recorded twice"
            ));
        }
        let count = usize::try_from(table.integer("count")?)
            .map_err(|_| format!("environment: `{category}` count must be non-negative"))?;
        let reason = table.string("reason")?.to_owned();
        let bead = table.string("bead")?.to_owned();
        if reason.trim().is_empty() || bead.trim().is_empty() {
            return Err(format!(
                "environment: `{category}` needs a reason and a bead"
            ));
        }
        categories.push(EnvironmentCategory {
            category,
            count,
            reason,
            bead,
        });
    }
    Ok(categories)
}

/// Applies the ratchet: every category's recorded count must equal the observed
/// count. A category missing from the file records zero.
///
/// Returns the failure messages; an empty vector is a clean run.
#[must_use]
pub fn ratchet(
    recorded: &[EnvironmentCategory],
    observed: &BTreeMap<&'static str, usize>,
) -> Vec<String> {
    let mut failures = Vec::new();
    for category in CATEGORIES {
        let have = observed.get(category).copied().unwrap_or(0);
        let want = recorded
            .iter()
            .find(|entry| entry.category == category)
            .map_or(0, |entry| entry.count);
        if have != want {
            failures.push(format!(
                "environment category `{category}`: recorded {want}, observed {have}; record the measured \
                 count in expected-differences.toml in the change that moved it"
            ));
        }
    }
    failures
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_by_the_unresolved_name() {
        let case = "valid/lambda.orna";
        assert_eq!(
            classify(
                case,
                "examples.valid.x: ORNA-S012-UNRESOLVED: `mail` is not in scope"
            ),
            Some(CONNECTOR_AND_HOST)
        );
        assert_eq!(
            classify(case, "m: ORNA-S012-UNRESOLVED: `kWh` is not in scope"),
            Some(CURRENCY_AND_UNIT)
        );
        assert_eq!(
            classify(case, "m: ORNA-S012-UNRESOLVED: `Contact` is not in scope"),
            Some(FIXTURE_TABLES)
        );
        assert_eq!(
            classify(case, "m: ORNA-S012-UNRESOLVED: `mean` is not in scope"),
            Some(MISSING_IMPORT)
        );
        assert_eq!(
            classify(
                case,
                "m: ORNA-INFER-007: the type of this expression is ambiguous"
            ),
            None
        );
    }

    #[test]
    fn classifies_a_name_by_its_case_when_no_name_class_applies() {
        let legacy = "invalid/legacy-result.orna";
        assert!(is_legacy_case(legacy));
        assert_eq!(
            classify(legacy, "m: ORNA-S012-UNRESOLVED: `Result` is not in scope"),
            Some(LEGACY_REJECTION)
        );
        assert_eq!(
            classify(
                "valid/secret-display.orna",
                "m: ORNA-S012-UNRESOLVED: `Str` is not in scope"
            ),
            Some(PENDING_P3B)
        );
        assert_eq!(
            classify(
                "valid/generic-protocol.orna",
                "m: ORNA-S012-UNRESOLVED: `Ordering` is not in scope"
            ),
            Some(IMPLEMENT)
        );
        assert_eq!(
            classify(
                "valid/x.orna",
                "m: ORNA-S012-UNRESOLVED: `Mystery` is not in scope"
            ),
            Some(UNCLASSIFIED)
        );
    }

    #[test]
    fn parses_and_ratchets_each_category() {
        let source = "[[environment]]\ncategory = \"fixture-tables-and-modules\"\ncount = 3\nreason = \"r\"\nbead = \"b\"\n";
        let recorded = parse_environment_categories(source).expect("parses");
        assert_eq!(recorded[0].count, 3);
        let mut observed = BTreeMap::new();
        observed.insert(FIXTURE_TABLES, 3);
        assert!(
            ratchet(&recorded, &observed).is_empty(),
            "the other categories record zero"
        );
        observed.insert(FIXTURE_TABLES, 2);
        let failures = ratchet(&recorded, &observed);
        assert!(
            failures
                .iter()
                .any(|line| line.contains("recorded 3, observed 2"))
        );
        observed.insert(UNCLASSIFIED, 1);
        assert!(
            ratchet(&recorded, &observed)
                .iter()
                .any(|line| line.contains("unclassified-names"))
        );
    }

    #[test]
    fn rejects_an_unknown_or_duplicate_category() {
        let unknown =
            "[[environment]]\ncategory = \"other\"\ncount = 1\nreason = \"r\"\nbead = \"b\"\n";
        assert!(parse_environment_categories(unknown).is_err());
        let entry = "[[environment]]\ncategory = \"unclassified-names\"\ncount = 1\nreason = \"r\"\nbead = \"b\"\n";
        assert!(parse_environment_categories(&format!("{entry}\n{entry}")).is_err());
    }
}
