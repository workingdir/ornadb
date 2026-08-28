use std::error::Error;

use orna_client::capability::{
    LocalCapabilityArgumentSource, LocalCapabilityDeclaration, LocalCapabilityGrant,
    LocalCapabilityGrantSet, LocalCapabilityName, LocalCapabilityScope,
};

fn main() -> Result<(), Box<dyn Error>> {
    const PROJECT_ROOT: &str = "/home/demo/project";
    const CHILD_SOURCE: &str = "/home/demo/project/src/main.orna";
    const SIBLING_SOURCE: &str = "/home/demo/project-other/src/main.orna";

    let read_scope = LocalCapabilityScope::path(PROJECT_ROOT)?;
    let read_grant = LocalCapabilityGrant::new(LocalCapabilityName::StdFsRead, read_scope)?;
    let grants = LocalCapabilityGrantSet::from_grants([read_grant])?;
    assert_eq!(
        grants.len(),
        1,
        "the grant set should contain one unique grant"
    );

    let child_allowed = grants.satisfies(LocalCapabilityName::StdFsRead, CHILD_SOURCE);
    let sibling_allowed = grants.satisfies(LocalCapabilityName::StdFsRead, SIBLING_SOURCE);
    assert!(
        child_allowed,
        "a child source path should satisfy the project grant"
    );
    assert!(
        !sibling_allowed,
        "a component-boundary sibling path must not satisfy the project grant"
    );

    let literal_declaration = LocalCapabilityDeclaration::new(
        LocalCapabilityName::StdFsRead,
        LocalCapabilityArgumentSource::Text(CHILD_SOURCE.to_owned()),
    );
    assert!(
        grants.satisfies_declaration(&literal_declaration, |_| None),
        "a literal capability declaration should use its declared path"
    );

    let parameter_declaration = LocalCapabilityDeclaration::new(
        LocalCapabilityName::StdFsRead,
        LocalCapabilityArgumentSource::Parameter("source".to_owned()),
    );
    assert!(
        grants.satisfies_declaration(&parameter_declaration, |parameter| {
            (parameter == "source").then(|| CHILD_SOURCE.to_owned())
        }),
        "a bound capability parameter should use its invocation path"
    );
    assert!(
        !grants.satisfies_declaration(&parameter_declaration, |_| None),
        "an unresolved capability parameter must fail closed"
    );

    println!("local grant matching: literal declaration allowed");
    println!("local grant matching: parameter declaration allowed");
    println!("local grant matching: unresolved parameter denied");

    println!("local grant matching: child path allowed ({CHILD_SOURCE})");
    println!("local grant matching: sibling path denied ({SIBLING_SOURCE})");
    Ok(())
}
