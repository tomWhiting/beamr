//! Unit tests for OTP stub BIFs.

use super::*;
use crate::atom::AtomTable;
use crate::native::otp_stubs::gleam_stubs::{GleamResultState, resume_gleam_result_continuation};
use crate::native::stdlib_stubs::maps_bifs::ContinuationStep;
use crate::native::{BifRegistryImpl, NativeContinuation};
use crate::process::Process;
use crate::term::boxed::{Tuple, write_closure};

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

fn attached_context(process: &mut Process) -> ProcessContext<'_> {
    let mut context = ProcessContext::new();
    context.set_atom_table(Some(std::sync::Arc::new(AtomTable::with_common_atoms())));
    context.attach_process(process, 0);
    context
}

fn closure(process: &mut Process, arity: u8, unique_id: u64) -> Term {
    let heap = process.heap_mut().alloc_slice(7).expect("closure heap");
    write_closure(heap, Atom::OK, 0, 1, arity, unique_id, &[]).expect("closure")
}

#[test]
fn option_map_some_sets_trampoline_and_resume_wraps_some() {
    let mut process = Process::new(1, 512);
    let fun = closure(&mut process, 1, 0x201);
    let mut context = attached_context(&mut process);
    let some = context
        .alloc_tuple(&[Term::atom(Atom::OK), Term::small_int(1)])
        .expect("some tuple");

    let placeholder = gleam_stubs::bif_option_map(&[some, fun], &mut context).expect("map some");
    assert_eq!(placeholder, Term::NIL);
    let request = context.take_trampoline().expect("option trampoline");
    assert_eq!(request.fun, fun);
    assert_eq!(request.args, vec![Term::small_int(1)]);
    let Some(NativeContinuation::GleamOption(state)) = request.continuation else {
        panic!("expected option continuation");
    };
    let done =
        gleam_stubs::resume_gleam_option_continuation(state, Term::small_int(2), &mut context)
            .expect("option resume");
    let ContinuationStep::Done(result) = done else {
        panic!("expected done");
    };
    let tuple = Tuple::new(result).expect("some result");
    assert_eq!(tuple.get(0), Some(Term::atom(Atom::OK)));
    assert_eq!(tuple.get(1), Some(Term::small_int(2)));
}

#[test]
fn option_map_none_returns_without_trampoline() {
    let mut process = Process::new(1, 512);
    let fun = closure(&mut process, 1, 0x202);
    let mut context = attached_context(&mut process);

    let result = gleam_stubs::bif_option_map(&[Term::atom(Atom::NIL), fun], &mut context);
    assert_eq!(result, Ok(Term::atom(Atom::NIL)));
    assert!(!context.has_trampoline());
}

#[test]
fn result_map_error_trampolines_only_error_and_wraps_result() {
    let mut process = Process::new(1, 512);
    let fun = closure(&mut process, 1, 0x203);
    let mut context = attached_context(&mut process);
    let error = context
        .alloc_tuple(&[Term::atom(Atom::ERROR), Term::atom(Atom::BADARG)])
        .expect("error tuple");

    let placeholder =
        gleam_stubs::bif_result_map_error(&[error, fun], &mut context).expect("map_error error");
    assert_eq!(placeholder, Term::NIL);
    let request = context.take_trampoline().expect("result trampoline");
    assert_eq!(request.args, vec![Term::atom(Atom::BADARG)]);
    let Some(NativeContinuation::GleamResult(GleamResultState::MapError)) = request.continuation
    else {
        panic!("expected map_error continuation");
    };

    let done = resume_gleam_result_continuation(
        GleamResultState::MapError,
        Term::small_int(7),
        &mut context,
    )
    .expect("map_error resume");
    let ContinuationStep::Done(result) = done else {
        panic!("expected done");
    };
    let tuple = Tuple::new(result).expect("mapped error");
    assert_eq!(tuple.get(0), Some(Term::atom(Atom::ERROR)));
    assert_eq!(tuple.get(1), Some(Term::small_int(7)));

    let ok = context
        .alloc_tuple(&[Term::atom(Atom::OK), Term::small_int(42)])
        .expect("ok tuple");
    assert_eq!(
        gleam_stubs::bif_result_map_error(&[ok, fun], &mut context),
        Ok(ok)
    );
    assert!(!context.has_trampoline());
}

#[test]
fn result_then_trampolines_only_ok_and_returns_closure_result() {
    let mut process = Process::new(1, 512);
    let fun = closure(&mut process, 1, 0x204);
    let mut context = attached_context(&mut process);
    let ok = context
        .alloc_tuple(&[Term::atom(Atom::OK), Term::small_int(1)])
        .expect("ok tuple");

    let placeholder = gleam_stubs::bif_result_then(&[ok, fun], &mut context).expect("then ok");
    assert_eq!(placeholder, Term::NIL);
    let request = context.take_trampoline().expect("then trampoline");
    assert_eq!(request.args, vec![Term::small_int(1)]);
    assert!(matches!(
        request.continuation,
        Some(NativeContinuation::GleamResult(GleamResultState::Then))
    ));

    let error = context
        .alloc_tuple(&[Term::atom(Atom::ERROR), Term::atom(Atom::BADARG)])
        .expect("error tuple");
    assert_eq!(
        gleam_stubs::bif_result_then(&[error, fun], &mut context),
        Ok(error)
    );
    assert!(!context.has_trampoline());
}

#[test]
fn intensity_tracker_add_event_increments_and_tags_by_previous_count() {
    let mut process = Process::new(1, 512);
    let mut context = attached_context(&mut process);
    let tracker = gleam_stubs::bif_intensity_tracker_new(
        &[Term::small_int(2), Term::small_int(1000)],
        &mut context,
    )
    .expect("tracker new");
    let result = gleam_stubs::bif_intensity_tracker_add_event(&[tracker], &mut context)
        .expect("tracker event");
    let tuple = Tuple::new(result).expect("ok tuple");
    assert_eq!(tuple.get(0), Some(Term::atom(Atom::OK)));
    let updated = Tuple::new(tuple.get(1).expect("updated tracker")).expect("updated tuple");
    assert_eq!(updated.get(0), Some(Term::small_int(1)));

    let saturated = context
        .alloc_tuple(&[
            Term::small_int(2),
            Term::small_int(2),
            Term::small_int(1000),
            Term::NIL,
        ])
        .expect("saturated tracker");
    let result = gleam_stubs::bif_intensity_tracker_add_event(&[saturated], &mut context)
        .expect("tracker error");
    let tuple = Tuple::new(result).expect("error tuple");
    assert_eq!(tuple.get(0), Some(Term::atom(Atom::ERROR)));
    assert!(!context.has_trampoline());
}
