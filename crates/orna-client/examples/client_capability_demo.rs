use std::error::Error;

use orna_client::capability::{
    LocalCapabilityGrant, LocalCapabilityGrantSet, LocalCapabilityName, LocalCapabilityScope,
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

    println!("local grant matching: child path allowed ({CHILD_SOURCE})");
    println!("local grant matching: sibling path denied ({SIBLING_SOURCE})");
    Ok(())
}
