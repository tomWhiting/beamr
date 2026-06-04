//! Integration test: load compiled Erlang stdlib .beam fixtures.
//!
//! Verifies that the higher-order stdlib functions compiled from Erlang
//! source (`lists.erl`) load successfully into the module registry and
//! that their exports resolve so calling modules can find them.

use beamr::atom::AtomTable;
use beamr::loader::{load_beam_chunks, load_module};
use beamr::module::ModuleRegistry;
use beamr::native::BifRegistryImpl;
use beamr::native::bifs::register_gate1_bifs;
use beamr::native::gate3_bifs::register_gate3_bifs;
use beamr::native::process_bifs::register_gate2_bifs;
use beamr::native::selector_ffi::register_selector_bifs;
use beamr::native::stdlib_stubs::register_stdlib_stubs;

/// Helper: set up the full BIF registry matching what the CLI creates.
fn full_bif_registry(atom_table: &AtomTable) -> BifRegistryImpl {
    let registry = BifRegistryImpl::new();
    register_gate1_bifs(&registry, atom_table).expect("gate1");
    register_gate2_bifs(&registry, atom_table).expect("gate2");
    register_gate3_bifs(&registry, atom_table).expect("gate3");
    register_stdlib_stubs(&registry, atom_table).expect("stdlib");
    register_selector_bifs(&registry, atom_table).expect("selector");
    registry
}

/// The compiled lists.beam fixture must parse without errors.
#[test]
fn stdlib_lists_beam_parses_without_errors() {
    let atom_table = AtomTable::with_common_atoms();
    let bytes = include_bytes!("fixtures/stdlib/lists.beam");
    let parsed = load_beam_chunks(bytes, &atom_table).expect("lists.beam should parse");

    assert!(!parsed.atoms.is_empty(), "atom table should not be empty");
    assert!(
        !parsed.instructions.is_empty(),
        "code section should not be empty"
    );
    assert!(
        !parsed.exports.is_empty(),
        "lists module should export functions"
    );
}

/// The lists module exports map/2, foldr/3, reverse/1, foreach/2.
#[test]
fn stdlib_lists_exports_higher_order_functions() {
    let atom_table = AtomTable::with_common_atoms();
    let bytes = include_bytes!("fixtures/stdlib/lists.beam");
    let parsed = load_beam_chunks(bytes, &atom_table).expect("lists.beam should parse");

    let expected_exports = [("map", 2), ("foldr", 3), ("reverse", 1), ("foreach", 2)];

    for (func_name, arity) in &expected_exports {
        let func_atom = atom_table.intern(func_name);
        let found = parsed
            .exports
            .iter()
            .any(|e| e.function == func_atom && e.arity == *arity);
        assert!(found, "missing export lists:{func_name}/{arity}");
    }
}

/// When loaded into the module registry, the lists module resolves its
/// own imports (which are internal recursive calls) plus any erlang:*
/// BIFs it references.
#[test]
fn stdlib_lists_loads_into_module_registry() {
    let atom_table = AtomTable::with_common_atoms();
    let bif_registry = full_bif_registry(&atom_table);
    let module_registry = ModuleRegistry::new();
    let bytes = include_bytes!("fixtures/stdlib/lists.beam");

    let (module, unresolved) = load_module(bytes, &atom_table, &module_registry, &bif_registry)
        .expect("lists.beam should load");

    // The module should be registered under the `lists` atom.
    let lists_atom = atom_table.intern("lists");
    assert_eq!(module.name, lists_atom);
    assert!(
        module_registry.lookup(lists_atom).is_some(),
        "lists module should be in the registry"
    );

    // All imports should be resolved (the module only calls itself
    // recursively and possibly erlang:error/1, which are all registered).
    assert!(
        unresolved.is_empty(),
        "lists module should have no unresolved imports, got: {unresolved}"
    );
}

/// A module that imports lists:map/2 should resolve it via the loaded
/// lists.beam module rather than a native BIF.
#[test]
fn calling_module_resolves_lists_map_via_loaded_module() {
    let atom_table = AtomTable::with_common_atoms();
    let bif_registry = full_bif_registry(&atom_table);
    let module_registry = ModuleRegistry::new();

    // Load the stdlib lists module first.
    let lists_bytes = include_bytes!("fixtures/stdlib/lists.beam");
    let (_lists_module, _) = load_module(lists_bytes, &atom_table, &module_registry, &bif_registry)
        .expect("lists.beam should load");

    // Verify lookup_mfa finds the exported function.
    let lists_atom = atom_table.intern("lists");
    let map_atom = atom_table.intern("map");
    let foldr_atom = atom_table.intern("foldr");
    let foreach_atom = atom_table.intern("foreach");
    let reverse_atom = atom_table.intern("reverse");

    // All higher-order functions should be findable via the module registry.
    assert!(
        module_registry.lookup_mfa(lists_atom, map_atom, 2).is_ok(),
        "lists:map/2 should resolve via loaded module"
    );
    assert!(
        module_registry
            .lookup_mfa(lists_atom, foldr_atom, 3)
            .is_ok(),
        "lists:foldr/3 should resolve via loaded module"
    );
    assert!(
        module_registry
            .lookup_mfa(lists_atom, foreach_atom, 2)
            .is_ok(),
        "lists:foreach/2 should resolve via loaded module"
    );
    assert!(
        module_registry
            .lookup_mfa(lists_atom, reverse_atom, 1)
            .is_ok(),
        "lists:reverse/1 should resolve via loaded module"
    );
}

/// The maps:map/2 native stub is registered as a BIF (returns badarg).
/// Verify it resolves in the BIF registry.
#[test]
fn maps_map_stub_resolves_in_bif_registry() {
    let atom_table = AtomTable::with_common_atoms();
    let bif_registry = full_bif_registry(&atom_table);

    let maps_atom = atom_table.intern("maps");
    let map_atom = atom_table.intern("map");

    assert!(
        bif_registry.lookup(maps_atom, map_atom, 2).is_some(),
        "maps:map/2 should be registered as a BIF stub"
    );
}
