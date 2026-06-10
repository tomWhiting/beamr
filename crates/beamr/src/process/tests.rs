use super::{
    CodePosition, DEFAULT_REDUCTION_BUDGET, Exception, ExitReason, Priority, Process,
    ProcessError, ProcessStatus,
};
use crate::atom::{Atom, AtomTable};
use crate::gc::tests::{alloc_proc_bin, module_pin};
use crate::namespace::NamespaceId;
use crate::term::boxed::{write_cons, write_tuple};
use crate::term::{Term, shared_binary::SharedBinary};

#[test]
fn exception_format_with_atoms_resolves_class_reason_and_stacktrace() {
    let table = AtomTable::with_common_atoms();
    let module = table.intern("sample");
    let function = table.intern("run");

    let mut line_tuple_heap = [0_u64; 3];
    let line_tuple = match write_tuple(
        &mut line_tuple_heap,
        &[Term::atom(Atom::LINE), Term::small_int(123)],
    ) {
        Some(term) => term,
        None => Term::NIL,
    };
    let mut info_heap = [0_u64; 2];
    let info = match write_cons(&mut info_heap, line_tuple, Term::NIL) {
        Some(term) => term,
        None => Term::NIL,
    };
    let mut frame_heap = [0_u64; 5];
    let frame = match write_tuple(
        &mut frame_heap,
        &[
            Term::atom(module),
            Term::atom(function),
            Term::small_int(2),
            info,
        ],
    ) {
        Some(term) => term,
        None => Term::NIL,
    };
    let mut stack_heap = [0_u64; 2];
    let stacktrace = match write_cons(&mut stack_heap, frame, Term::NIL) {
        Some(term) => term,
        None => Term::NIL,
    };
    let exception = Exception {
        class: Term::atom(Atom::ERROR),
        reason: Term::atom(Atom::BADARG),
        stacktrace,
    };

    assert_eq!(
        exception.format_with_atoms(&table),
        "error: badarg\n  at sample:run/2:123"
    );
}

#[test]
fn exception_format_with_atoms_omits_nil_stacktrace() {
    let table = AtomTable::with_common_atoms();
    let exception = Exception {
        class: Term::atom(Atom::ERROR),
        reason: Term::atom(Atom::BADARG),
        stacktrace: Term::NIL,
    };

    assert_eq!(exception.format_with_atoms(&table), "error: badarg");
}

#[test]
fn fresh_process_has_expected_state() {
    let process = Process::new(7, 233);

    assert_eq!(process.pid(), 7);
    assert_eq!(process.status(), ProcessStatus::New);
    assert_eq!(process.priority(), Priority::Normal);
    assert_eq!(process.heap().capacity(), 233);
    assert!(process.stack().is_empty());
    assert!(process.mailbox().is_empty());
    assert!(process.dict_get_all().is_empty());
    assert_eq!(process.reduction_counter(), DEFAULT_REDUCTION_BUDGET);
    assert_eq!(process.namespace_id(), NamespaceId::DEFAULT);
    assert_eq!(process.code_position(), None);
    assert!(process.current_module().is_none());
    assert!(process.links().is_empty());
    assert!(process.monitors().is_empty());
    assert!(!process.trap_exit());
    assert_eq!(process.group_leader(), Term::pid(7));
}

#[test]
fn dictionary_put_get_round_trip() {
    let mut process = Process::new(7, 233);
    let key = Term::atom(Atom::OK);
    let value = Term::small_int(42);

    assert_eq!(process.dict_put(key, value), Term::atom(Atom::UNDEFINED));
    assert_eq!(process.dict_get(key), value);
    assert_eq!(process.dict_get_all(), &[(key, value)]);
}

#[test]
fn dictionary_put_replaces_existing_entry_and_returns_old_value() {
    let mut process = Process::new(7, 233);
    let key = Term::atom(Atom::OK);
    let old_value = Term::small_int(1);
    let new_value = Term::small_int(2);

    assert_eq!(
        process.dict_put(key, old_value),
        Term::atom(Atom::UNDEFINED)
    );
    assert_eq!(process.dict_put(key, new_value), old_value);
    assert_eq!(process.dict_get(key), new_value);
    assert_eq!(process.dict_get_all(), &[(key, new_value)]);
}

#[test]
fn dictionary_get_and_erase_missing_return_undefined() {
    let mut process = Process::new(7, 233);
    let key = Term::atom(Atom::OK);

    assert_eq!(process.dict_get(key), Term::atom(Atom::UNDEFINED));
    assert_eq!(process.dict_erase(key), Term::atom(Atom::UNDEFINED));
}

#[test]
fn dictionary_erase_removes_entry_with_swap_remove() {
    let mut process = Process::new(7, 233);
    let key = Term::atom(Atom::OK);
    let value = Term::small_int(42);
    process.dict_put(key, value);

    assert_eq!(process.dict_erase(key), value);
    assert_eq!(process.dict_get(key), Term::atom(Atom::UNDEFINED));
    assert!(process.dict_get_all().is_empty());
}

#[test]
fn dictionary_erase_all_drains_entries() {
    let mut process = Process::new(7, 233);
    process.dict_put(Term::atom(Atom::OK), Term::small_int(1));
    process.dict_put(Term::atom(Atom::ERROR), Term::small_int(2));

    assert_eq!(
        process.dict_erase_all(),
        vec![
            (Term::atom(Atom::OK), Term::small_int(1)),
            (Term::atom(Atom::ERROR), Term::small_int(2)),
        ]
    );
    assert!(process.dict_get_all().is_empty());
}

#[test]
fn dictionary_get_keys_returns_exact_value_matches() {
    let mut process = Process::new(7, 233);
    process.dict_put(Term::atom(Atom::OK), Term::small_int(1));
    process.dict_put(Term::atom(Atom::ERROR), Term::small_int(1));
    process.dict_put(Term::atom(Atom::UNDEFINED), Term::small_int(2));

    assert_eq!(
        process.dict_get_keys(Term::small_int(1)),
        vec![Term::atom(Atom::OK), Term::atom(Atom::ERROR)]
    );
}

#[test]
fn links_preserve_insertion_order_and_deduplicate() {
    let mut process = Process::new(7, 233);

    assert!(process.add_link(11));
    assert!(process.add_link(13));
    assert!(process.add_link(17));
    assert!(!process.add_link(13));
    assert!(!process.add_link(7));

    assert_eq!(process.links(), &[11, 13, 17]);
}

#[test]
fn remove_link_preserves_remaining_order() {
    let mut process = Process::new(7, 233);
    process.add_link(11);
    process.add_link(13);
    process.add_link(17);
    process.add_link(19);

    assert!(process.remove_link(13));
    assert!(!process.remove_link(23));

    assert_eq!(process.links(), &[11, 17, 19]);
}

#[test]
fn take_links_returns_ordered_links_and_clears_storage() {
    let mut process = Process::new(7, 233);
    process.add_link(11);
    process.add_link(13);
    process.add_link(17);

    assert_eq!(process.take_links(), vec![11, 13, 17]);
    assert!(process.links().is_empty());
}

#[test]
fn terminate_clears_current_module_pin() {
    let mut process = Process::new(0, 233);
    process.set_code_position(Some(CodePosition {
        module: Atom::OK,
        instruction_pointer: 0,
    }));
    process.set_current_module(module_pin(Atom::OK));

    process.terminate(ExitReason::Normal);

    assert!(process.current_module().is_none());
    assert_eq!(process.code_position(), None);
}

#[test]
fn terminate_releases_heap_proc_bins_and_resets_virtual_binary_heap() {
    let shared = SharedBinary::new(vec![0xAB; 256 * 1024]);
    let mut process = Process::new(0, 233);
    let proc_bin = alloc_proc_bin(&mut process, &shared);
    process.set_x_reg(0, proc_bin);
    assert_eq!(shared.ref_count(), 2);
    assert_eq!(process.virtual_binary_heap(), 256 * 1024);

    process.terminate(ExitReason::Normal);

    assert_eq!(shared.ref_count(), 1);
    assert_eq!(process.virtual_binary_heap(), 0);
    assert_eq!(process.heap().total_used(), 0);
    assert_eq!(process.x_reg(0), Term::NIL);
}

#[test]
fn all_x_registers_start_as_nil() {
    let process = Process::new(0, 233);

    for register in u16::MIN..=u8::MAX as u16 {
        assert_eq!(process.x_reg(register), Term::NIL);
    }
}

#[test]
fn x_registers_are_independently_addressable() {
    let mut process = Process::new(0, 233);

    process.set_x_reg(0, Term::small_int(10));
    process.set_x_reg(255, Term::small_int(20));

    assert_eq!(process.x_reg(0), Term::small_int(10));
    assert_eq!(process.x_reg(255), Term::small_int(20));
    assert_eq!(process.x_reg(1), Term::NIL);
}

#[test]
fn float_registers_start_at_zero_and_are_independent() {
    let mut process = Process::new(0, 233);

    assert_eq!(process.get_float_reg(0), Ok(0.0));
    assert_eq!(process.get_float_reg(15), Ok(0.0));
    process.set_x_reg(0, Term::small_int(314));
    assert_eq!(process.set_float_reg(0, 2.75), Ok(()));

    assert_eq!(process.get_float_reg(0), Ok(2.75));
    assert_eq!(process.get_float_reg(1), Ok(0.0));
    assert_eq!(process.x_reg(0), Term::small_int(314));
}

#[test]
fn float_register_bounds_return_errors() {
    let mut process = Process::new(0, 233);

    assert_eq!(
        process.get_float_reg(16),
        Err(ProcessError::InvalidFloatRegister { index: 16 })
    );
    assert_eq!(
        process.set_float_reg(16, 1.0),
        Err(ProcessError::InvalidFloatRegister { index: 16 })
    );
}

#[test]
fn terminate_clears_float_registers() {
    let mut process = Process::new(0, 233);
    assert_eq!(process.set_float_reg(0, 2.75), Ok(()));

    process.terminate(ExitReason::Normal);

    assert_eq!(process.get_float_reg(0), Ok(0.0));
}

#[test]
fn valid_status_transitions_succeed() {
    let mut process = Process::new(0, 233);

    assert_eq!(process.transition_to(ProcessStatus::Running), Ok(()));
    assert_eq!(process.transition_to(ProcessStatus::Yielded), Ok(()));
    assert_eq!(process.transition_to(ProcessStatus::Running), Ok(()));
    assert_eq!(process.transition_to(ProcessStatus::Waiting), Ok(()));
    assert_eq!(process.transition_to(ProcessStatus::Running), Ok(()));
    assert_eq!(
        process.transition_to(ProcessStatus::Exited(ExitReason::Normal)),
        Ok(())
    );
}

#[test]
fn new_to_exited_transition_fails() {
    let mut process = Process::new(0, 233);

    assert_eq!(
        process.transition_to(ProcessStatus::Exited(ExitReason::Error)),
        Err(ProcessError::InvalidStatusTransition {
            from: ProcessStatus::New,
            to: ProcessStatus::Exited(ExitReason::Error),
        })
    );
    assert_eq!(process.status(), ProcessStatus::New);
}

#[test]
fn exited_state_is_terminal() {
    let mut process = Process::new(0, 233);

    process
        .transition_to(ProcessStatus::Running)
        .expect("new process can start running");
    process
        .transition_to(ProcessStatus::Exited(ExitReason::Kill))
        .expect("running process can exit");

    assert_eq!(
        process.transition_to(ProcessStatus::Running),
        Err(ProcessError::InvalidStatusTransition {
            from: ProcessStatus::Exited(ExitReason::Kill),
            to: ProcessStatus::Running,
        })
    );
}

#[test]
fn reductions_decrement_saturate_and_reset() {
    let mut process = Process::new(0, 233);

    assert_eq!(process.reduction_counter(), DEFAULT_REDUCTION_BUDGET);
    process.decrement_reductions(1);
    assert_eq!(process.reduction_counter(), DEFAULT_REDUCTION_BUDGET - 1);
    process.decrement_reductions(DEFAULT_REDUCTION_BUDGET);
    assert_eq!(process.reduction_counter(), 0);
    assert!(process.reductions_exhausted());
    process.reset_reductions(DEFAULT_REDUCTION_BUDGET);
    assert_eq!(process.reduction_counter(), DEFAULT_REDUCTION_BUDGET);
    assert!(!process.reductions_exhausted());
}
