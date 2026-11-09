//! The generated `EngineFunction` enum must match the sealed engine catalogue.

use orna_sys_id::{ENGINE_FUNCTION_COUNT, ENGINE_FUNCTIONS, EngineFunction};

#[test]
fn every_sealed_engine_function_has_one_generated_variant() {
    let sealed: Vec<_> = ENGINE_FUNCTIONS
        .iter()
        .filter(|entry| entry.sealed)
        .collect();
    assert_eq!(sealed.len(), ENGINE_FUNCTION_COUNT);
    assert_eq!(EngineFunction::ALL.len(), ENGINE_FUNCTION_COUNT);
    for (index, entry) in sealed.iter().enumerate() {
        let generated = EngineFunction::ALL[index];
        assert_eq!(generated.label(), entry.name);
        assert_eq!(generated.name_parts(), entry.name_parts);
        assert_eq!(Some(generated.function_id()), entry.function_id);
        assert_eq!(EngineFunction::from_label(entry.name), Some(generated));
    }
}
