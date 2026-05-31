//! Integration test: all gleam_otp modules load with zero unresolved imports.
//!
//! This is the "no import gaps" proof for B-032. It loads ALL .beam files
//! from stdlib and OTP fixtures into a shared module registry with all BIF
//! gates registered, then verifies that every module has zero unresolved
//! imports.
//!
//! If any import is missing, this test fails and prints exactly what is
//! unresolved, making it trivial to identify the next stub to implement.

use beamr::atom::AtomTable;
use beamr::loader::load_module;
use beamr::module::ModuleRegistry;
use beamr::native::BifRegistryImpl;
use beamr::native::bifs::register_gate1_bifs;
use beamr::native::gate3_bifs::register_gate3_bifs;
use beamr::native::gleam_ffi::register_gleam_ffi_bifs;
use beamr::native::otp_stubs::{init_otp_atoms, register_otp_stubs};
use beamr::native::process_bifs::register_gate2_bifs;
use beamr::native::selector_ffi::register_selector_bifs;
use beamr::native::stdlib_stubs::register_stdlib_stubs;

/// Set up the full BIF registry matching what the CLI creates.
fn full_bif_registry(atom_table: &AtomTable) -> BifRegistryImpl {
    let mut registry = BifRegistryImpl::new();
    register_gate1_bifs(&mut registry, atom_table).expect("gate1");
    register_gate2_bifs(&mut registry, atom_table).expect("gate2");
    register_gate3_bifs(&mut registry, atom_table).expect("gate3");
    register_stdlib_stubs(&mut registry, atom_table).expect("stdlib");
    register_selector_bifs(&mut registry, atom_table).expect("selector");
    register_gleam_ffi_bifs(&mut registry, atom_table).expect("gleam_ffi");
    init_otp_atoms(atom_table);
    register_otp_stubs(&mut registry, atom_table).expect("otp_stubs");
    registry
}

/// Helper: load a .beam file and return (module_name, unresolved_count, unresolved_details).
fn load_beam(
    path: &std::path::Path,
    atom_table: &AtomTable,
    module_registry: &ModuleRegistry,
    bif_registry: &BifRegistryImpl,
) -> (String, usize, String) {
    let filename = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string());

    let bytes =
        std::fs::read(path).unwrap_or_else(|err| panic!("failed to read {filename}: {err}"));

    let (_module, unresolved) = load_module(&bytes, atom_table, module_registry, bif_registry)
        .unwrap_or_else(|err| panic!("failed to load {filename}: {err}"));

    let count = unresolved.imports().len();
    let details = if count > 0 {
        let imports: Vec<String> = unresolved
            .imports()
            .into_iter()
            .map(|imp| {
                let module_name = atom_table.resolve(imp.module).unwrap_or("?");
                let function_name = atom_table.resolve(imp.function).unwrap_or("?");
                format!("  {module_name}:{function_name}/{}", imp.arity)
            })
            .collect();
        imports.join("\n")
    } else {
        String::new()
    };

    (filename, count, details)
}

/// All OTP modules load with zero unresolved imports when all fixtures
/// and BIF registries are available.
///
/// Loading order matters: stdlib modules first, then gleam_erlang modules,
/// then gleam_otp modules. Each loaded module registers its exports in the
/// shared module registry, making them available to later modules.
#[test]
fn all_otp_modules_have_zero_unresolved_imports() {
    let atom_table = AtomTable::with_common_atoms();
    let bif_registry = full_bif_registry(&atom_table);
    let module_registry = ModuleRegistry::new();

    let fixtures_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");

    // Load order: stdlib first, then erlang layer, then OTP layer.
    // This ensures cross-module imports resolve correctly.
    let load_order = [
        // stdlib
        "stdlib/lists.beam",
        // gleam_erlang layer
        "otp/gleam_erlang_ffi.beam",
        "otp/gleam@erlang@process.beam",
        // gleam_otp layer
        "otp/gleam_otp_external.beam",
        "otp/gleam@otp@actor.beam",
        "otp/gleam@otp@supervisor.beam",
    ];

    let mut failures: Vec<String> = Vec::new();

    for relative_path in &load_order {
        let path = fixtures_dir.join(relative_path);
        let (filename, count, details) =
            load_beam(&path, &atom_table, &module_registry, &bif_registry);

        if count > 0 {
            failures.push(format!(
                "{filename}: {count} unresolved import(s):\n{details}"
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "modules with unresolved imports:\n{}",
        failures.join("\n\n")
    );
}

/// Each individual OTP module loads without errors (decode/parse succeeds).
#[test]
fn all_otp_modules_load_without_decode_errors() {
    let atom_table = AtomTable::with_common_atoms();
    let bif_registry = full_bif_registry(&atom_table);
    let module_registry = ModuleRegistry::new();

    let fixtures_dir =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/otp");

    let expected_files = [
        "gleam_erlang_ffi.beam",
        "gleam_otp_external.beam",
        "gleam@erlang@process.beam",
        "gleam@otp@actor.beam",
        "gleam@otp@supervisor.beam",
    ];

    for filename in &expected_files {
        let path = fixtures_dir.join(filename);
        let bytes =
            std::fs::read(&path).unwrap_or_else(|err| panic!("failed to read {filename}: {err}"));

        // Loading should not panic or return an error.
        let result = load_module(&bytes, &atom_table, &module_registry, &bif_registry);
        assert!(
            result.is_ok(),
            "{filename} failed to load: {:?}",
            result.err()
        );
    }
}

/// After loading all modules, the module registry contains entries for
/// every OTP module, and cross-module MFA lookups succeed.
#[test]
fn module_registry_contains_all_otp_modules_after_loading() {
    let atom_table = AtomTable::with_common_atoms();
    let bif_registry = full_bif_registry(&atom_table);
    let module_registry = ModuleRegistry::new();

    let fixtures_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");

    // Load all modules.
    let load_order = [
        "stdlib/lists.beam",
        "otp/gleam_erlang_ffi.beam",
        "otp/gleam@erlang@process.beam",
        "otp/gleam_otp_external.beam",
        "otp/gleam@otp@actor.beam",
        "otp/gleam@otp@supervisor.beam",
    ];

    for relative_path in &load_order {
        let path = fixtures_dir.join(relative_path);
        let bytes = std::fs::read(&path)
            .unwrap_or_else(|err| panic!("failed to read {relative_path}: {err}"));
        let _ = load_module(&bytes, &atom_table, &module_registry, &bif_registry)
            .unwrap_or_else(|err| panic!("failed to load {relative_path}: {err}"));
    }

    // Verify all OTP modules are in the registry.
    let expected_modules = [
        "lists",
        "gleam_erlang_ffi",
        "gleam@erlang@process",
        "gleam_otp_external",
        "gleam@otp@actor",
        "gleam@otp@supervisor",
    ];

    for module_name in &expected_modules {
        let atom = atom_table.intern(module_name);
        assert!(
            module_registry.lookup(atom).is_some(),
            "module '{module_name}' should be in the registry"
        );
    }
}
