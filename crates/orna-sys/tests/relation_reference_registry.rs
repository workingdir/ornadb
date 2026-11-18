//! The registry of row-reference identities (`orna_value::relations`) names exactly the
//! relations, reference aliases and natural-key fields of the generated `sys` catalogue
//! (`api/sys.json`), in catalogue order.

use orna_sys::SysRelation;
use orna_value::relations;

// Specified: ORNA-SYS-111, ORNA-SYS-125, ORNA-SYS-141; Orna-1.1.0 api/sys.json relations.
#[test]
fn the_reference_registry_matches_the_generated_catalogue() {
    assert_eq!(
        relations::ALL.len(),
        SysRelation::ALL.len(),
        "one registry entry per catalogue relation"
    );
    for (relation, spec) in SysRelation::ALL.into_iter().zip(relations::ALL) {
        let descriptor = relation.descriptor();
        assert_eq!(spec.name, descriptor.name, "catalogue order");
        assert_eq!(
            spec.reference_type, descriptor.reference_type,
            "{}",
            descriptor.name
        );
        let key = spec
            .key
            .iter()
            .map(|field| field.name)
            .collect::<Vec<_>>()
            .join(" + ");
        assert_eq!(key, descriptor.key, "{}", descriptor.name);
        for field in spec.key {
            // A field of a nested record (`change.area`) is declared on the nested record's relation.
            let (owner, name) = match field.name.split_once('.') {
                Some((_, nested)) => (SysRelation::Change.descriptor(), nested),
                None => (descriptor, field.name),
            };
            let declared = owner
                .fields
                .iter()
                .find(|declared| declared.field.name() == name)
                .unwrap_or_else(|| panic!("{} declares key field {name}", descriptor.name));
            assert_eq!(
                field.ty, declared.type_name,
                "{} key field {}",
                descriptor.name, field.name
            );
        }
    }
}
