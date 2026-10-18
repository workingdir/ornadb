use std::collections::BTreeMap;

use orna_semantic_v1::{Analysis, ModuleInput, SourceUnitMetadata, Type, analyze};

const SOURCE: &str = r#"
pub dim Length;
pub dim Time;
pub dim Temperature;
pub dim Speed = Length / Time;

pub unit m : Length base;
pub unit metre : Length = 1.m;
pub unit km : Length = 1000.m;
pub unit s : Time base;
pub unit hour : Time = 3600.s;
pub unit mps : Speed = m / s;
pub unit kmph : Speed = km / hour;
pub unit K : Temperature base;
pub unit celsius : Temperature = K offset 273.15 affine;
pub unit deltaC : Temperature = K;

pub fn total_speed(values: Relation<Float<kmph>>) = std.stats.sum(rows: values);
pub fn mean_speed(values: Relation<Float<kmph>>) = std.stats.mean(rows: values);
pub fn travelled(points: Relation<(Instant, Float<kmph>)>) = std.stats.integrate(points: points);
"#;

fn analysis() -> Analysis {
    analyze(&[ModuleInput::new("unit-algebra.orna", SOURCE)])
}

fn source_unit<'a>(analysis: &'a Analysis, name: &str) -> &'a SourceUnitMetadata {
    analysis
        .source_units
        .values()
        .find(|unit| unit.identity.name == name)
        .unwrap_or_else(|| panic!("source unit {name} was not resolved; available: {:?}", analysis.source_units.keys().map(|identity| identity.name.clone()).collect::<Vec<_>>()))
}

fn function<'a>(analysis: &'a Analysis, name: &str) -> &'a Type {
    analysis
        .modules
        .values()
        .find_map(|module| module.symbols.get(name).map(|symbol| &symbol.ty))
        .unwrap_or_else(|| panic!("function {name} was not exposed"))
}

// Specified: ORNA-UNIT-001; source-unit compatibility metadata is reduced from declared dimensions, exact rational scales, and affine behavior rather than unit spelling.
#[test]
fn declared_unit_metadata_uses_reduced_dimensions_scale_and_affinity() {
    let analysis = analysis();
    assert!(analysis.is_ok(), "diagnostics={:?}; total_speed={:?}", analysis.diagnostics.iter().map(|diagnostic| diagnostic.message()).collect::<Vec<_>>(), function(&analysis, "total_speed"));

    let length_axis = source_unit(&analysis, "m").dimension_basis.keys().next().unwrap().clone();
    let time_axis = source_unit(&analysis, "s").dimension_basis.keys().next().unwrap().clone();
    assert_eq!(
        source_unit(&analysis, "kmph").dimension_basis,
        BTreeMap::from([(length_axis.clone(), 1), (time_axis, -1)])
    );
    assert_eq!(
        source_unit(&analysis, "kmph").scale.numerator,
        "5"
    );
    assert_eq!(source_unit(&analysis, "kmph").scale.denominator, "18");

    let metre = source_unit(&analysis, "m");
    let alias = source_unit(&analysis, "metre");
    assert_ne!(metre.identity, alias.identity);
    assert_eq!(metre.dimension_basis, alias.dimension_basis);
    assert_eq!(metre.scale, alias.scale);
    assert_eq!(metre.offset, alias.offset);
    assert_eq!(metre.affine, alias.affine);

    let absolute = source_unit(&analysis, "celsius");
    let delta = source_unit(&analysis, "deltaC");
    assert_eq!(absolute.dimension_basis, delta.dimension_basis);
    assert_ne!(absolute.offset, delta.offset);
    assert!(absolute.affine);
    assert!(!delta.affine);
}

// Specified: ORNA-UNIT-003; a production statistics aggregate preserves the input quantity dimension.
#[test]
fn relation_sum_and_mean_preserve_quantity_dimensions() {
    let analysis = analysis();
    assert!(analysis.is_ok(), "{:?}", analysis.diagnostics.iter().map(|diagnostic| diagnostic.message()).collect::<Vec<_>>());

    let Type::Function {
        parameters: sum_parameters,
        result: sum_result,
        ..
    } = function(&analysis, "total_speed")
    else {
        panic!("total_speed has a function type");
    };
    let Type::Relation(_) = &sum_parameters[0] else {
        panic!("total_speed input is a relation");
    };
    let Type::Quantity {
        numeric,
        dimension_basis,
        scale,
        affine,
        ..
    } = sum_result.as_ref()
    else {
        panic!("sum retains source-unit quantity metadata");
    };
    assert_eq!(numeric.as_ref(), &Type::Float);
    let kmph = source_unit(&analysis, "kmph");
    assert_eq!(dimension_basis, &kmph.dimension_basis);
    assert_eq!(scale, &kmph.scale);
    assert!(!affine);

    let Type::Function {
        parameters: mean_parameters,
        result: mean_result,
        ..
    } = function(&analysis, "mean_speed")
    else {
        panic!("mean_speed has a function type");
    };
    let Type::Relation(_) = &mean_parameters[0] else {
        panic!("mean_speed input is a relation");
    };
    let Type::Optional(mean) = mean_result.as_ref() else {
        panic!("mean returns an optional quantity");
    };
    assert_eq!(mean.as_ref(), sum_result.as_ref());
}

// Specified: ORNA-UNIT-006; integrating speed over elapsed time reduces the time exponent and returns a length quantity.
#[test]
fn integrating_speed_over_time_reduces_to_length() {
    let analysis = analysis();
    assert!(analysis.is_ok(), "{:?}", analysis.diagnostics.iter().map(|diagnostic| diagnostic.message()).collect::<Vec<_>>());

    let Type::Function {
        parameters,
        result,
        ..
    } = function(&analysis, "travelled")
    else {
        panic!("travelled has a function type");
    };
    let Type::Relation(points) = &parameters[0] else {
        panic!("travelled input is a relation");
    };
    let Type::Tuple(parts) = points.as_ref() else {
        panic!("integration rows are timestamp/value pairs");
    };
    assert_eq!(parts.len(), 2);
    let Type::Optional(integral) = result.as_ref() else {
        panic!("integration result is optional");
    };
    let Type::Quantity {
        numeric,
        dimension_basis,
        scale,
        ..
    } = integral.as_ref()
    else {
        panic!("integration returns a source-unit quantity");
    };
    assert_eq!(numeric.as_ref(), &Type::Float);
    assert_eq!(dimension_basis, &source_unit(&analysis, "m").dimension_basis);
    assert_eq!(scale, &source_unit(&analysis, "kmph").scale);
}
