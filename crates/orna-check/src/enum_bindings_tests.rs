use super::*;

/// The scan `add_qualified_enum_bindings` replaced: every source module of the
/// project, kept when its namespace starts with `target`.
fn full_scan(
    binding: &str,
    target: &Namespace,
    source_enums: &BTreeMap<Namespace, BTreeMap<String, EnumVariantRows>>,
    modules: &BTreeMap<Namespace, ModuleHeader>,
) -> BTreeMap<String, EnumVariantRows> {
    let mut bindings = BTreeMap::new();
    for (namespace, enums) in source_enums {
        if namespace.0.len() < target.0.len() || namespace.0[..target.0.len()] != target.0[..] {
            continue;
        }
        let Some(module) = modules.get(namespace) else {
            continue;
        };
        let mut prefix = vec![binding.to_owned()];
        prefix.extend(namespace.0[target.0.len()..].iter().cloned());
        for (name, variants) in enums {
            if is_exported_enum(module, name) {
                let mut qualified = prefix.clone();
                qualified.push(name.clone());
                bindings.insert(qualified.join("."), variants.clone());
            }
        }
    }
    bindings
}

fn namespace(segments: &[&str]) -> Namespace {
    Namespace(
        segments
            .iter()
            .map(|segment| (*segment).to_owned())
            .collect(),
    )
}

/// Nested modules, namespaces that share a string prefix without sharing a
/// segment (`a0`, `aa`), a private enum and a payload enum.
fn nested_project() -> Analysis {
    analyze(&[
        ModuleInput::new("main.orna", "pub enum RootKind { a, }\n"),
        ModuleInput::new("a/main.orna", "pub enum Alpha { x, }\n"),
        ModuleInput::new(
            "a/b.orna",
            "pub enum AlphaBeta { y, z { count: Int }, }\nenum Hidden { h, }\n",
        ),
        ModuleInput::new("a/b/c.orna", "pub enum Deep { d, }\n"),
        ModuleInput::new("a0.orna", "pub enum Near { n, }\n"),
        ModuleInput::new("aa.orna", "pub enum Same { s, }\n"),
        ModuleInput::new("b.orna", "pub enum Beta { p, }\n"),
    ])
}

#[test]
fn range_scan_binds_what_a_full_scan_binds() {
    let analysis = nested_project();
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let mut targets = analysis.modules.keys().cloned().collect::<Vec<_>>();
    targets.extend([
        namespace(&["a", "c"]),
        namespace(&["a", "b", "c", "d"]),
        namespace(&["z"]),
    ]);
    for target in &targets {
        let mut scanned = BTreeMap::new();
        add_qualified_enum_bindings(
            &mut scanned,
            "alias",
            target,
            &analysis.source_enums,
            &analysis.modules,
        );
        assert_eq!(
            scanned,
            full_scan("alias", target, &analysis.source_enums, &analysis.modules),
            "target {target:?}"
        );
    }
}

#[test]
fn an_import_binds_the_enums_of_its_target_and_the_modules_below_it() {
    let analysis = nested_project();
    let mut bindings = BTreeMap::new();
    add_qualified_enum_bindings(
        &mut bindings,
        "alias",
        &namespace(&["a"]),
        &analysis.source_enums,
        &analysis.modules,
    );
    // `a0` and `aa` start with the same characters as `a` but are other
    // modules, and the private `Hidden` is not exported.
    assert_eq!(
        bindings.keys().map(String::as_str).collect::<Vec<_>>(),
        ["alias.Alpha", "alias.b.AlphaBeta", "alias.b.c.Deep"]
    );
}

/// `count` modules that each import every other module and export one enum.
fn all_importing_project(count: usize) -> Vec<ModuleInput> {
    (0..count)
        .map(|index| {
            let imports = (0..count)
                .filter(|other| *other != index)
                .map(|other| format!("use m{other};\n"))
                .collect::<String>();
            ModuleInput::new(
                format!("m{index}.orna"),
                format!("{imports}pub enum E{index} {{ a, b, }}\n"),
            )
        })
        .collect()
}

#[test]
fn an_import_visits_only_the_modules_under_its_target() {
    const COUNT: usize = 24;
    reset_enum_scan_visits();
    let analysis = analyze(&all_importing_project(COUNT));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    // Each of the `COUNT * (COUNT - 1)` imports names one module, which has no
    // modules below it. Scanning every module per import would visit `COUNT`
    // times as many.
    assert_eq!(enum_scan_visits(), COUNT * (COUNT - 1));
}

#[test]
fn nested_enums_construct_through_a_module_alias() {
    let shop = [
        ModuleInput::new(
            "shop/main.orna",
            "use shop.orders;\npub enum Region { north, south, }\n",
        ),
        ModuleInput::new(
            "shop/orders.orna",
            "pub enum Status { paid, open { due: Int }, }\nenum Private { p, }\n",
        ),
    ];
    let analyze_consumer = |consumer: &str| {
        let mut inputs = shop.to_vec();
        inputs.push(ModuleInput::new("main.orna", consumer));
        analyze(&inputs)
    };

    let valid = analyze_consumer(
        "use shop as s;\n\
         pub fn region() = s.Region.north;\n\
         pub fn paid() = s.orders.Status.paid;\n\
         pub fn open() = s.orders.Status.open { due: 3 };\n",
    );
    assert!(valid.is_ok(), "{:?}", valid.diagnostics);

    let private = analyze_consumer("use shop as s;\npub fn hidden() = s.orders.Private.p;\n");
    assert!(
        private
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_UNRESOLVED),
        "{:?}",
        private.diagnostics
    );
}
