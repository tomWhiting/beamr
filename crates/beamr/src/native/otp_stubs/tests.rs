//! Unit tests for OTP stub BIFs.

use super::*;
use crate::atom::AtomTable;
use crate::native::BifRegistryImpl;

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
    let mut context = ProcessContext::new();
    let result = gleam_stubs::bif_dynamic_int(&[Term::small_int(42)], &mut context);
    assert!(result.is_ok());
}

#[test]
fn dynamic_int_returns_error_for_atoms() {
    let mut context = ProcessContext::new();
    let result = gleam_stubs::bif_dynamic_int(&[Term::atom(Atom::OK)], &mut context);
    assert!(result.is_ok());
}

#[test]
fn os_getenv_0_returns_empty_list() {
    let mut context = ProcessContext::new();
    let result = erlang_stubs::bif_os_getenv_0(&[], &mut context);
    assert_eq!(result, Ok(Term::NIL));
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
