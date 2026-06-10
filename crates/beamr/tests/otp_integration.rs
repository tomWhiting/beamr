//! End-to-end OTP integration tests for B-032.
//!
//! These tests prove that:
//! 1. All gleam_otp .beam files load and resolve imports
//! 2. The new erlang BIFs work correctly (++, length, not, /=, get, pid_to_list)
//! 3. OTP stub BIFs return correct values
//! 4. Cross-module function exports are findable via the module registry
//!
//! The key deliverable: hard evidence that the gleam_otp .beam files can load
//! and the critical import path is resolved.

use beamr::atom::{Atom, AtomTable};
use beamr::loader::load_module;
use beamr::module::ModuleRegistry;
use beamr::native::BifRegistryImpl;
use beamr::native::ProcessContext;
use beamr::native::bifs::register_gate1_bifs;
use beamr::native::gate3_bifs::register_gate3_bifs;
use beamr::native::gleam_ffi::register_gleam_ffi_bifs;
use beamr::native::otp_stubs::{init_otp_atoms, register_otp_stubs};
use beamr::native::process_bifs::register_gate2_bifs;
use beamr::native::selector_ffi::register_selector_bifs;
use beamr::native::stdlib_stubs::register_stdlib_stubs;
use beamr::term::Term;
use beamr::process::Process;
use beamr::term::boxed::Cons;

/// Set up the full BIF registry matching the CLI.
fn full_bif_registry(atom_table: &AtomTable) -> BifRegistryImpl {
    let registry = BifRegistryImpl::new();
    register_gate1_bifs(&registry, atom_table).expect("gate1");
    register_gate2_bifs(&registry, atom_table).expect("gate2");
    register_gate3_bifs(&registry, atom_table).expect("gate3");
    register_stdlib_stubs(&registry, atom_table).expect("stdlib");
    register_selector_bifs(&registry, atom_table).expect("selector");
    register_gleam_ffi_bifs(&registry, atom_table).expect("gleam_ffi");
    init_otp_atoms(atom_table);
    register_otp_stubs(&registry, atom_table).expect("otp_stubs");
    registry
}

/// Load all OTP-related modules into the registry in dependency order.
fn load_all_otp_modules(
    atom_table: &AtomTable,
    bif_registry: &BifRegistryImpl,
    module_registry: &ModuleRegistry,
) {
    let fixtures_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");

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
        let (_module, unresolved) = load_module(&bytes, atom_table, module_registry, bif_registry)
            .unwrap_or_else(|err| panic!("failed to load {relative_path}: {err}"));
        assert!(
            unresolved.is_empty(),
            "{relative_path} has unresolved imports: {unresolved}"
        );
    }
}

// ── End-to-end OTP module loading ─────────────────────────────────────────

/// Core proof: all OTP modules load and resolve every import.
/// The supervisor module's exports are findable via the module registry,
/// completing the import chain from gleam_otp down to the VM.
#[test]
fn otp_supervisor_module_exports_are_resolvable() {
    let atom_table = AtomTable::with_common_atoms();
    let bif_registry = full_bif_registry(&atom_table);
    let module_registry = ModuleRegistry::new();

    load_all_otp_modules(&atom_table, &bif_registry, &module_registry);

    // The supervisor module should be loaded and its exports findable.
    let supervisor = atom_table.intern("gleam@otp@supervisor");
    assert!(
        module_registry.lookup(supervisor).is_some(),
        "gleam@otp@supervisor should be in the module registry"
    );

    // The actor module's start_spec export should be resolvable.
    let actor = atom_table.intern("gleam@otp@actor");
    assert!(
        module_registry.lookup(actor).is_some(),
        "gleam@otp@actor should be in the module registry"
    );

    // Cross-module: gleam@erlang@process exports used by actor/supervisor.
    let process = atom_table.intern("gleam@erlang@process");
    assert!(
        module_registry.lookup(process).is_some(),
        "gleam@erlang@process should be in the module registry"
    );
}

/// Verify that the gleam@otp@actor module has key exports that the
/// supervisor depends on.
#[test]
fn actor_module_exports_key_functions() {
    let atom_table = AtomTable::with_common_atoms();
    let bif_registry = full_bif_registry(&atom_table);
    let module_registry = ModuleRegistry::new();

    load_all_otp_modules(&atom_table, &bif_registry, &module_registry);

    let actor_atom = atom_table.intern("gleam@otp@actor");

    // These are the functions that gleam@otp@supervisor imports from actor.
    let expected_exports = [
        ("continue", 1),
        ("start_spec", 1),
        ("to_erlang_start_result", 1),
    ];

    for (func_name, arity) in &expected_exports {
        let func_atom = atom_table.intern(func_name);
        let result = module_registry.lookup_mfa(actor_atom, func_atom, *arity);
        assert!(
            result.is_ok(),
            "gleam@otp@actor:{func_name}/{arity} should be resolvable, got: {result:?}"
        );
    }
}

/// Verify that gleam@erlang@process exports the functions that
/// both actor and supervisor depend on.
#[test]
fn process_module_exports_key_functions() {
    let atom_table = AtomTable::with_common_atoms();
    let bif_registry = full_bif_registry(&atom_table);
    let module_registry = ModuleRegistry::new();

    load_all_otp_modules(&atom_table, &bif_registry, &module_registry);

    let process_atom = atom_table.intern("gleam@erlang@process");

    let expected_exports = [
        ("new_subject", 0),
        ("send", 2),
        ("subject_owner", 1),
        ("start", 2),
        ("selecting", 3),
        ("selecting_anything", 2),
        ("monitor_process", 1),
        ("kill", 1),
        ("call", 3),
    ];

    for (func_name, arity) in &expected_exports {
        let func_atom = atom_table.intern(func_name);
        let result = module_registry.lookup_mfa(process_atom, func_atom, *arity);
        assert!(
            result.is_ok(),
            "gleam@erlang@process:{func_name}/{arity} should be resolvable, got: {result:?}"
        );
    }
}

// ── Erlang BIF integration tests ──────────────────────────────────────────

/// erlang:get/0 returns an empty list (no process dictionary).
#[test]
fn erlang_get_returns_empty_list() {
    let atom_table = AtomTable::with_common_atoms();
    let bif_registry = full_bif_registry(&atom_table);

    let erlang = atom_table.intern("erlang");
    let get = atom_table.intern("get");
    let entry = bif_registry
        .lookup(erlang, get, 0)
        .expect("erlang:get/0 should be registered");

    let mut process = Process::new(1, 128);
    let mut context = ProcessContext::new();
    context.attach_process(&mut process, 0);
    let result = (entry.function)(&[], &mut context);
    assert_eq!(result, Ok(Term::NIL));
}

/// erlang:length/1 counts list elements correctly.
#[test]
fn erlang_length_counts_list() {
    let atom_table = AtomTable::with_common_atoms();
    let bif_registry = full_bif_registry(&atom_table);

    let erlang = atom_table.intern("erlang");
    let length = atom_table.intern("length");
    let entry = bif_registry
        .lookup(erlang, length, 1)
        .expect("erlang:length/1 should be registered");

    let mut context = ProcessContext::new();

    // Empty list → 0
    let result = (entry.function)(&[Term::NIL], &mut context);
    assert_eq!(result, Ok(Term::small_int(0)));

    // Build [1, 2, 3] and check length = 3
    let cell3 = Box::leak(Box::new([0u64; 2]));
    let list = beamr::term::boxed::write_cons(cell3, Term::small_int(3), Term::NIL).expect("cons");
    let cell2 = Box::leak(Box::new([0u64; 2]));
    let list = beamr::term::boxed::write_cons(cell2, Term::small_int(2), list).expect("cons");
    let cell1 = Box::leak(Box::new([0u64; 2]));
    let list = beamr::term::boxed::write_cons(cell1, Term::small_int(1), list).expect("cons");

    let result = (entry.function)(&[list], &mut context);
    assert_eq!(result, Ok(Term::small_int(3)));
}

/// erlang:++/2 appends two lists.
#[test]
fn erlang_list_append_concatenates() {
    let atom_table = AtomTable::with_common_atoms();
    let bif_registry = full_bif_registry(&atom_table);

    let erlang = atom_table.intern("erlang");
    let append = atom_table.intern("++");
    let entry = bif_registry
        .lookup(erlang, append, 2)
        .expect("erlang:++/2 should be registered");

    let process = Box::leak(Box::new(Process::new(1, 256)));
    let mut context = ProcessContext::new();
    context.attach_process(process, 0);

    // [] ++ [3] = [3]
    let cell = Box::leak(Box::new([0u64; 2]));
    let list_b = beamr::term::boxed::write_cons(cell, Term::small_int(3), Term::NIL).expect("cons");

    let result =
        (entry.function)(&[Term::NIL, list_b], &mut context).expect("append should succeed");
    let cons = Cons::new(result).expect("result is a list");
    assert_eq!(cons.head(), Term::small_int(3));
    assert!(cons.tail().is_nil());

    // [1, 2] ++ [3]
    let c2 = Box::leak(Box::new([0u64; 2]));
    let list_a = beamr::term::boxed::write_cons(c2, Term::small_int(2), Term::NIL).expect("cons");
    let c1 = Box::leak(Box::new([0u64; 2]));
    let list_a = beamr::term::boxed::write_cons(c1, Term::small_int(1), list_a).expect("cons");

    let result = (entry.function)(&[list_a, list_b], &mut context).expect("append should succeed");

    // Verify [1, 2, 3]
    let cons1 = Cons::new(result).expect("list");
    assert_eq!(cons1.head(), Term::small_int(1));
    let cons2 = Cons::new(cons1.tail()).expect("list");
    assert_eq!(cons2.head(), Term::small_int(2));
    let cons3 = Cons::new(cons2.tail()).expect("list");
    assert_eq!(cons3.head(), Term::small_int(3));
    assert!(cons3.tail().is_nil());
}

/// erlang:not/1 negates booleans.
#[test]
fn erlang_not_negates() {
    let atom_table = AtomTable::with_common_atoms();
    let bif_registry = full_bif_registry(&atom_table);

    let erlang = atom_table.intern("erlang");
    let not = atom_table.intern("not");
    let entry = bif_registry
        .lookup(erlang, not, 1)
        .expect("erlang:not/1 should be registered");

    let mut context = ProcessContext::new();

    assert_eq!(
        (entry.function)(&[Term::atom(Atom::TRUE)], &mut context),
        Ok(Term::atom(Atom::FALSE))
    );
    assert_eq!(
        (entry.function)(&[Term::atom(Atom::FALSE)], &mut context),
        Ok(Term::atom(Atom::TRUE))
    );
    // Non-boolean should error.
    assert!((entry.function)(&[Term::atom(Atom::OK)], &mut context).is_err());
}

/// erlang:/=/2 structural inequality.
#[test]
fn erlang_not_equal_compares() {
    let atom_table = AtomTable::with_common_atoms();
    let bif_registry = full_bif_registry(&atom_table);

    let erlang = atom_table.intern("erlang");
    let ne = atom_table.intern("/=");
    let entry = bif_registry
        .lookup(erlang, ne, 2)
        .expect("erlang:/=/2 should be registered");

    let mut context = ProcessContext::new();

    assert_eq!(
        (entry.function)(&[Term::small_int(1), Term::small_int(2)], &mut context),
        Ok(Term::atom(Atom::TRUE))
    );
    assert_eq!(
        (entry.function)(&[Term::small_int(1), Term::small_int(1)], &mut context),
        Ok(Term::atom(Atom::FALSE))
    );
}

/// erlang:pid_to_list/1 converts a PID to a string list.
#[test]
fn erlang_pid_to_list_formats_correctly() {
    let atom_table = AtomTable::with_common_atoms();
    let bif_registry = full_bif_registry(&atom_table);

    let erlang = atom_table.intern("erlang");
    let ptl = atom_table.intern("pid_to_list");
    let entry = bif_registry
        .lookup(erlang, ptl, 1)
        .expect("erlang:pid_to_list/1 should be registered");

    let process = Box::leak(Box::new(Process::new(1, 256)));
    let mut context = ProcessContext::new();
    context.attach_process(process, 0);
    let pid_term = Term::pid(7);

    let result = (entry.function)(&[pid_term], &mut context).expect("pid_to_list should succeed");

    // The result should be a list of integers representing "<0.7.0>".
    let expected = "<0.7.0>";
    let mut chars = Vec::new();
    let mut current = result;
    loop {
        if current.is_nil() {
            break;
        }
        let cons = Cons::new(current).expect("proper list");
        let ch = cons.head().as_small_int().expect("integer code point");
        chars.push(ch as u8 as char);
        current = cons.tail();
    }
    let actual: String = chars.into_iter().collect();
    assert_eq!(actual, expected);
}

// ── OTP stub integration tests ────────────────────────────────────────────

/// gleam_otp_external:application_stopped/0 is registered and returns ok.
#[test]
fn otp_external_application_stopped_returns_ok() {
    let atom_table = AtomTable::with_common_atoms();
    let bif_registry = full_bif_registry(&atom_table);

    let module = atom_table.intern("gleam_otp_external");
    let function = atom_table.intern("application_stopped");
    let entry = bif_registry
        .lookup(module, function, 0)
        .expect("gleam_otp_external:application_stopped/0 should be registered");

    let mut context = ProcessContext::new();
    let result = (entry.function)(&[], &mut context);
    assert_eq!(result, Ok(Term::atom(Atom::OK)));
}

/// gleam@* modules ship as compiled bytecode in every Gleam build and must
/// never be shadowed by native stubs: a native entry overrides the loaded
/// module, so any drift from the real implementation silently corrupts
/// behaviour that the real module would serve correctly.
#[test]
fn gleam_modules_are_not_shadowed_by_native_stubs() {
    let atom_table = AtomTable::with_common_atoms();
    let bif_registry = full_bif_registry(&atom_table);

    let forbidden = [
        ("gleam@dynamic", "classify", 1u8),
        ("gleam@dynamic", "int", 1),
        ("gleam@dynamic", "string", 1),
        ("gleam@string", "inspect", 1),
        ("gleam@string", "append", 2),
        ("gleam@option", "map", 2),
        ("gleam@option", "unwrap", 2),
        ("gleam@result", "map_error", 2),
        ("gleam@result", "then", 2),
        ("gleam@otp@intensity_tracker", "new", 2),
        ("gleam@otp@intensity_tracker", "add_event", 1),
    ];
    for (module_name, function_name, arity) in &forbidden {
        let module = atom_table.intern(module_name);
        let function = atom_table.intern(function_name);
        assert!(
            bif_registry.lookup(module, function, *arity).is_none(),
            "{module_name}:{function_name}/{arity} must come from loaded bytecode, not a stub"
        );
    }
}

/// The complete BIF coverage check: every import that the CLI would need
/// to resolve is registered in the BIF registry.
#[test]
fn bif_registry_covers_all_required_otp_external_imports() {
    let atom_table = AtomTable::with_common_atoms();
    let bif_registry = full_bif_registry(&atom_table);

    // These are the BIF-level imports from gleam_otp_external.beam.
    let required_bifs = [
        ("erlang", "get", 0),
        ("erlang", "pid_to_list", 1),
        ("erlang", "++", 2),
        ("supervisor", "start_link", 2),
    ];

    for (module_name, function_name, arity) in &required_bifs {
        let module = atom_table.intern(module_name);
        let function = atom_table.intern(function_name);
        assert!(
            bif_registry.lookup(module, function, *arity).is_some(),
            "{module_name}:{function_name}/{arity} should be registered"
        );
    }
}

/// Full BIF coverage for all non-module-level imports across all OTP fixtures.
#[test]
fn bif_registry_covers_all_non_module_level_otp_imports() {
    let atom_table = AtomTable::with_common_atoms();
    let bif_registry = full_bif_registry(&atom_table);

    // BIF-level imports from across all OTP fixtures that must be registered.
    // Module-level imports (gleam@erlang@process:*, gleam@otp@actor:*) are
    // resolved via the module registry, not BIFs.
    let required_bifs = [
        // From gleam_otp_external.beam
        ("erlang", "get", 0u8),
        ("erlang", "pid_to_list", 1),
        ("erlang", "++", 2),
        ("supervisor", "start_link", 2),
        ("os", "getenv", 0),
        ("os", "getenv", 1),
        ("os", "putenv", 2),
        ("os", "unsetenv", 1),
        ("os", "type", 0),
        ("application", "ensure_all_started", 1),
        ("io", "get_line", 1),
        ("code", "priv_dir", 1),
        ("net_kernel", "connect_node", 1),
        ("string", "split", 2),
        ("gleam_otp_external", "application_stopped", 0),
    ];

    for (module_name, function_name, arity) in &required_bifs {
        let module = atom_table.intern(module_name);
        let function = atom_table.intern(function_name);
        assert!(
            bif_registry.lookup(module, function, *arity).is_some(),
            "BIF missing: {module_name}:{function_name}/{arity}"
        );
    }
}
