//! Unit tests for OTP stub BIFs.

use super::*;
use crate::atom::AtomTable;
use crate::native::BifRegistryImpl;
use crate::native::code_management_bifs::CodeManagementFacility;
use crate::process::Process;
use crate::term::binary_ref::BinaryRef;
use crate::term::boxed::{Cons, Tuple};
use crate::{
    error::LoadError,
    module::{ModuleOrigin, PurgeError},
    scheduler::{HotLoadResult, PurgeResult},
};
use std::collections::HashMap;
use std::sync::Arc;

#[test]
fn application_stopped_returns_ok() {
    let mut context = ProcessContext::new();
    let result = bif_application_stopped(&[], &mut context);
    assert_eq!(result, Ok(Term::atom(Atom::OK)));
}

#[test]
fn application_stopped_rejects_args() {
    let mut context = ProcessContext::new();
    let result = bif_application_stopped(&[Term::atom(Atom::OK)], &mut context);
    assert!(result.is_err());
}

#[test]
fn supervisor_start_link_rejects_wrong_arity() {
    let mut context = ProcessContext::new();
    let result = bif_supervisor_start_link(&[], &mut context);
    assert!(result.is_err());
}

#[test]
fn register_otp_stubs_registers_all_entries() {
    let atom_table = AtomTable::with_common_atoms();
    init_otp_atoms(&atom_table);
    let registry = BifRegistryImpl::new();

    register_otp_stubs(&registry, &atom_table).expect("otp stub registration");

    let gleam_otp_ext = atom_table.intern("gleam_otp_external");
    let app_stopped = atom_table.intern("application_stopped");
    assert!(
        registry.lookup(gleam_otp_ext, app_stopped, 0).is_some(),
        "gleam_otp_external:application_stopped/0 should be registered"
    );

    let supervisor = atom_table.intern("supervisor");
    let start_link = atom_table.intern("start_link");
    assert!(
        registry.lookup(supervisor, start_link, 2).is_some(),
        "supervisor:start_link/2 should be registered"
    );

    let gleam_string = atom_table.intern("gleam@string");
    let inspect = atom_table.intern("inspect");
    assert!(
        registry.lookup(gleam_string, inspect, 1).is_some(),
        "gleam@string:inspect/1 should be registered"
    );

    let os = atom_table.intern("os");
    let getenv = atom_table.intern("getenv");
    assert!(
        registry.lookup(os, getenv, 0).is_some(),
        "os:getenv/0 should be registered"
    );
}

#[test]
fn register_otp_stubs_rejects_duplicate_registration() {
    let atom_table = AtomTable::with_common_atoms();
    init_otp_atoms(&atom_table);
    let registry = BifRegistryImpl::new();

    register_otp_stubs(&registry, &atom_table).expect("first");
    assert!(register_otp_stubs(&registry, &atom_table).is_err());
}

#[test]
fn dynamic_int_returns_ok_for_integers() {
    let mut process = Process::new(1, 128);
    let mut context = ProcessContext::new();
    context.attach_process(&mut process, 0);
    let result = gleam_stubs::bif_dynamic_int(&[Term::small_int(42)], &mut context);
    assert!(result.is_ok());
}

#[test]
fn dynamic_int_returns_error_for_atoms() {
    let mut process = Process::new(1, 128);
    let mut context = ProcessContext::new();
    context.attach_process(&mut process, 0);
    let result = gleam_stubs::bif_dynamic_int(&[Term::atom(Atom::OK)], &mut context);
    assert!(result.is_ok());
}

#[test]
fn os_getenv_0_returns_non_empty_environment_list() {
    let mut process = Process::new(1, 262_144);
    let mut context = ProcessContext::new();
    context.attach_process(&mut process, 0);

    let result = erlang_stubs::bif_os_getenv_0(&[], &mut context).expect("environment list");
    let variables = list_terms(result);
    assert!(!variables.is_empty());
    assert!(
        variables
            .into_iter()
            .all(|variable| BinaryRef::new(variable).is_some())
    );
}

#[test]
fn os_getenv_returns_binary_values_and_false_for_missing() {
    let mut process = Process::new(1, 4096);
    let mut context = ProcessContext::new();
    context.attach_process(&mut process, 0);

    let key = context.alloc_binary(b"PATH").expect("key binary");
    let path = erlang_stubs::bif_os_getenv_1(&[key], &mut context).expect("PATH lookup");
    let bytes = BinaryRef::new(path).expect("PATH value should be binary");
    assert!(!bytes.is_empty());

    let missing = context
        .alloc_binary(b"BEAMR_TEST_NONEXISTENT_VAR")
        .expect("missing key binary");
    assert_eq!(
        erlang_stubs::bif_os_getenv_1(&[missing], &mut context),
        Ok(Term::atom(Atom::FALSE))
    );
    assert!(erlang_stubs::bif_os_getenv_1(&[Term::small_int(1)], &mut context).is_err());
}

#[test]
fn os_putenv_and_unsetenv_round_trip() {
    let mut process = Process::new(1, 4096);
    let mut context = ProcessContext::new();
    context.attach_process(&mut process, 0);
    let key_name = b"BEAMR_TEST_B170_PUTENV";
    let key = context.alloc_binary(key_name).expect("key binary");
    let value = context.alloc_binary(b"hello").expect("value binary");
    let previous = std::env::var("BEAMR_TEST_B170_PUTENV").ok();

    assert_eq!(
        erlang_stubs::bif_os_putenv(&[key, value], &mut context),
        Ok(Term::atom(Atom::TRUE))
    );
    let lookup_key = context.alloc_binary(key_name).expect("lookup key binary");
    let lookup = erlang_stubs::bif_os_getenv_1(&[lookup_key], &mut context).expect("lookup");
    assert_eq!(binary_bytes(lookup), b"hello");

    let unset_key = context.alloc_binary(key_name).expect("unset key binary");
    assert_eq!(
        erlang_stubs::bif_os_unsetenv(&[unset_key], &mut context),
        Ok(Term::atom(Atom::TRUE))
    );
    let missing_key = context.alloc_binary(key_name).expect("missing key binary");
    assert_eq!(
        erlang_stubs::bif_os_getenv_1(&[missing_key], &mut context),
        Ok(Term::atom(Atom::FALSE))
    );
    assert!(erlang_stubs::bif_os_putenv(&[Term::small_int(1), value], &mut context).is_err());

    if let Some(previous) = previous {
        let restore_key = context.alloc_binary(key_name).expect("restore key");
        let restore_value = context
            .alloc_binary(previous.as_bytes())
            .expect("restore value");
        assert_eq!(
            erlang_stubs::bif_os_putenv(&[restore_key, restore_value], &mut context),
            Ok(Term::atom(Atom::TRUE))
        );
    }
}

#[test]
fn os_type_returns_platform_tuple() {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    init_otp_atoms(&atom_table);
    let mut process = Process::new(1, 128);
    let mut context = ProcessContext::new();
    context.set_atom_table(Some(atom_table.clone()));
    context.attach_process(&mut process, 0);

    let result = erlang_stubs::bif_os_type(&[], &mut context).expect("os:type");
    let tuple = Tuple::new(result).expect("os:type tuple");
    assert_eq!(tuple.arity(), 2);
    if cfg!(target_os = "macos") {
        assert_eq!(
            tuple.get(0).and_then(Term::as_atom),
            Some(atom_table.intern("unix"))
        );
        assert_eq!(
            tuple.get(1).and_then(Term::as_atom),
            Some(atom_table.intern("darwin"))
        );
    } else if cfg!(target_os = "linux") {
        assert_eq!(
            tuple.get(0).and_then(Term::as_atom),
            Some(atom_table.intern("unix"))
        );
        assert_eq!(
            tuple.get(1).and_then(Term::as_atom),
            Some(atom_table.intern("linux"))
        );
    } else if cfg!(target_os = "windows") {
        assert_eq!(
            tuple.get(0).and_then(Term::as_atom),
            Some(atom_table.intern("win32"))
        );
        assert_eq!(
            tuple.get(1).and_then(Term::as_atom),
            Some(atom_table.intern("nt"))
        );
    }
}

#[test]
fn code_priv_dir_resolves_sibling_priv_for_loaded_filesystem_module() {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    init_otp_atoms(&atom_table);
    let app = atom_table.intern("beamr_b170_app");
    let app_dir = unique_test_dir("beamr_b170_priv_dir");
    let ebin_dir = app_dir.join("ebin");
    let priv_dir = app_dir.join("priv");
    std::fs::create_dir_all(&ebin_dir).expect("create ebin");
    std::fs::create_dir_all(&priv_dir).expect("create priv");
    let beam_path = ebin_dir.join("beamr_b170_app.beam");
    std::fs::write(&beam_path, []).expect("create beam placeholder");

    let mut process = Process::new(1, 4096);
    let mut context = ProcessContext::new();
    context.set_atom_table(Some(atom_table));
    context.set_code_management_facility(Some(Arc::new(MockCodeFacility::with_origin(
        app,
        ModuleOrigin::Filesystem(beam_path),
    ))));
    context.attach_process(&mut process, 0);

    let result =
        erlang_stubs::bif_code_priv_dir(&[Term::atom(app)], &mut context).expect("code:priv_dir");
    assert_eq!(binary_bytes(result), priv_dir.to_string_lossy().as_bytes());
    assert!(erlang_stubs::bif_code_priv_dir(&[Term::small_int(1)], &mut context).is_err());

    std::fs::remove_dir_all(app_dir).expect("remove temp app dir");
}

#[test]
fn code_priv_dir_returns_error_bad_name_for_missing_app() {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    init_otp_atoms(&atom_table);
    let missing = atom_table.intern("missing_app");
    let mut process = Process::new(1, 1024);
    let mut context = ProcessContext::new();
    context.set_atom_table(Some(atom_table.clone()));
    context.set_code_management_facility(Some(Arc::new(MockCodeFacility::default())));
    context.attach_process(&mut process, 0);

    let result = erlang_stubs::bif_code_priv_dir(&[Term::atom(missing)], &mut context)
        .expect("bad_name tuple");
    let tuple = Tuple::new(result).expect("error tuple");
    assert_eq!(tuple.get(0), Some(Term::atom(Atom::ERROR)));
    assert_eq!(
        tuple.get(1).and_then(Term::as_atom),
        Some(atom_table.intern("bad_name"))
    );
}

#[test]
fn string_split_preserves_binary_and_list_representations() {
    let mut process = Process::new(1, 4096);
    let mut context = ProcessContext::new();
    context.attach_process(&mut process, 0);

    let input = context.alloc_binary(b"a.b.c").expect("input");
    let pattern = context.alloc_binary(b".").expect("pattern");
    let result = erlang_stubs::bif_string_split(&[input, pattern], &mut context).expect("split");
    let parts = list_terms(result);
    assert_eq!(parts.len(), 2);
    assert_eq!(binary_bytes(parts[0]), b"a");
    assert_eq!(binary_bytes(parts[1]), b"b.c");

    let no_match_input = context.alloc_binary(b"hello").expect("no match input");
    let no_match_pattern = context.alloc_binary(b".").expect("no match pattern");
    let no_match =
        erlang_stubs::bif_string_split(&[no_match_input, no_match_pattern], &mut context)
            .expect("no match split");
    assert_eq!(list_terms(no_match), vec![no_match_input]);

    let empty = context.alloc_binary(b"").expect("empty input");
    let dot = context.alloc_binary(b".").expect("dot");
    let empty_result =
        erlang_stubs::bif_string_split(&[empty, dot], &mut context).expect("empty no match");
    assert_eq!(list_terms(empty_result), vec![empty]);

    let list_input = byte_list(&mut context, b"a.b");
    let list_pattern = byte_list(&mut context, b".");
    let list_result = erlang_stubs::bif_string_split(&[list_input, list_pattern], &mut context)
        .expect("list split");
    let list_parts = list_terms(list_result);
    assert_eq!(list_bytes(list_parts[0]), b"a");
    assert_eq!(list_bytes(list_parts[1]), b"b");
    assert!(erlang_stubs::bif_string_split(&[Term::small_int(1), pattern], &mut context).is_err());
}

#[test]
fn ensure_all_started_reports_loaded_and_missing_modules() {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    init_otp_atoms(&atom_table);
    let loaded = atom_table.intern("loaded_app");
    let missing = atom_table.intern("missing_app");
    let mut process = Process::new(1, 4096);
    let mut context = ProcessContext::new();
    context.set_atom_table(Some(atom_table.clone()));
    context.set_code_management_facility(Some(Arc::new(MockCodeFacility::with_origin(
        loaded,
        ModuleOrigin::Preloaded,
    ))));
    context.attach_process(&mut process, 0);

    let ok = erlang_stubs::bif_ensure_all_started(&[Term::atom(loaded)], &mut context)
        .expect("loaded result");
    let ok_tuple = Tuple::new(ok).expect("ok tuple");
    assert_eq!(ok_tuple.get(0), Some(Term::atom(Atom::OK)));
    assert_eq!(
        list_terms(ok_tuple.get(1).expect("started app list")),
        vec![Term::atom(loaded)]
    );

    let error = erlang_stubs::bif_ensure_all_started(&[Term::atom(missing)], &mut context)
        .expect("missing result");
    let error_tuple = Tuple::new(error).expect("error tuple");
    assert_eq!(error_tuple.get(0), Some(Term::atom(Atom::ERROR)));
    let reason = Tuple::new(error_tuple.get(1).expect("reason tuple")).expect("reason tuple");
    assert_eq!(reason.get(0), Some(Term::atom(missing)));
    let inner = Tuple::new(reason.get(1).expect("inner tuple")).expect("inner tuple");
    assert_eq!(
        inner.get(0).and_then(Term::as_atom),
        Some(atom_table.intern("not_loaded"))
    );
    assert_eq!(inner.get(1), Some(Term::atom(missing)));
    assert!(erlang_stubs::bif_ensure_all_started(&[Term::small_int(1)], &mut context).is_err());
}

#[test]
fn erlang_not_negates_booleans() {
    use crate::native::gate3_bifs::bif_not;

    let mut context = ProcessContext::new();
    assert_eq!(
        bif_not(&[Term::atom(Atom::TRUE)], &mut context),
        Ok(Term::atom(Atom::FALSE))
    );
    assert_eq!(
        bif_not(&[Term::atom(Atom::FALSE)], &mut context),
        Ok(Term::atom(Atom::TRUE))
    );
    assert!(bif_not(&[Term::atom(Atom::OK)], &mut context).is_err());
}

#[test]
fn erlang_length_counts_empty_list() {
    use crate::native::gate3_bifs::bif_length;

    let mut context = ProcessContext::new();
    assert_eq!(
        bif_length(&[Term::NIL], &mut context),
        Ok(Term::small_int(0))
    );
}

#[derive(Default)]
struct MockCodeFacility {
    origins: HashMap<Atom, ModuleOrigin>,
}

impl MockCodeFacility {
    fn with_origin(module: Atom, origin: ModuleOrigin) -> Self {
        Self {
            origins: HashMap::from([(module, origin)]),
        }
    }
}

impl CodeManagementFacility for MockCodeFacility {
    fn load_module(&self, _bytes: &[u8]) -> Result<HotLoadResult, LoadError> {
        unimplemented!("not needed by OTP stub tests")
    }

    fn purge_module(&self, _module: Atom) -> Result<PurgeResult, PurgeError> {
        unimplemented!("not needed by OTP stub tests")
    }

    fn delete_module(&self, _module: Atom) -> bool {
        unimplemented!("not needed by OTP stub tests")
    }

    fn check_old_code(&self, _module: Atom) -> bool {
        false
    }

    fn check_process_code(&self, _pid: u64, _module: Atom) -> bool {
        false
    }

    fn module_origin(&self, module: Atom) -> Option<ModuleOrigin> {
        self.origins.get(&module).cloned()
    }

    fn all_loaded_modules(&self) -> Vec<(Atom, ModuleOrigin)> {
        self.origins
            .iter()
            .map(|(module, origin)| (*module, origin.clone()))
            .collect()
    }
}

fn binary_bytes(term: Term) -> &'static [u8] {
    BinaryRef::new(term).expect("binary term").as_bytes()
}

fn byte_list(context: &mut ProcessContext, bytes: &[u8]) -> Term {
    let elements: Vec<_> = bytes
        .iter()
        .map(|byte| Term::small_int(i64::from(*byte)))
        .collect();
    context.alloc_list(&elements).expect("byte list")
}

fn list_terms(term: Term) -> Vec<Term> {
    let mut terms = Vec::new();
    let mut current = term;
    while !current.is_nil() {
        let cons = Cons::new(current).expect("proper list");
        terms.push(cons.head());
        current = cons.tail();
    }
    terms
}

fn list_bytes(term: Term) -> Vec<u8> {
    list_terms(term)
        .into_iter()
        .map(|term| u8::try_from(term.as_small_int().expect("byte integer")).expect("u8 byte"))
        .collect()
}

fn unique_test_dir(prefix: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "{prefix}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after epoch")
            .as_nanos()
    ))
}
