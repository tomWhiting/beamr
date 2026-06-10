use std::collections::HashMap;
use std::sync::Arc;

use super::*;
use crate::atom::{Atom, AtomTable};
use crate::constant_pool::ConstantPool;
use crate::ets::copy_term_to_ets;
use crate::loader::{Instruction, LambdaEntry, LineInfo, Literal};
use crate::module::{Module, ModuleOrigin, ResolvedImport};
use crate::native::{BifRegistryImpl, ProcessContext};
use crate::process::heap::DEFAULT_HEAP_SIZE;
use crate::process::{CodePosition, ExitReason, Process, ProcessStatus, ReceiveTimeout};
use crate::term::Term;
use crate::term::boxed::Tuple;

#[test]
fn wasm_scheduler_starts_empty_and_runs_idle_round() {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let modules = Arc::new(ModuleRegistry::new());
    let bifs = Arc::new(BifRegistryImpl::new());
    let mut scheduler = WasmScheduler::new(atom_table, modules, bifs);

    let summary = scheduler.run_until_idle();

    assert_eq!(summary.executed, 0);
    assert!(summary.exited.is_empty());
}

#[test]
fn receive_after_wait_schedules_and_fires_matching_timer() {
    let (mut scheduler, module) = scheduler_with_test_module();
    let pid = 42;
    let timeout_position = CodePosition {
        module: module.name,
        instruction_pointer: 7,
    };
    let mut process = waiting_process(pid, Arc::clone(&module));
    process.set_receive_timeout(Some(ReceiveTimeout {
        timeout_position,
        milliseconds: 25,
    }));

    scheduler.register_receive_timer(&mut process);
    assert_eq!(process.receive_timer_ref(), Some(1));
    assert_eq!(
        scheduler.take_pending_timer_schedules(),
        vec![WasmScheduledTimer {
            pid,
            timer_id: 1,
            milliseconds: 25,
        }]
    );
    scheduler.processes.insert(pid, process);
    scheduler.waiting.insert(pid);

    assert!(scheduler.timer_fired(pid, 1));
    let resumed = scheduler.processes.get(&pid).expect("process is retained");
    assert_eq!(resumed.receive_timer_ref(), None);
    assert_eq!(resumed.code_position(), Some(timeout_position));
    assert_eq!(resumed.status(), ProcessStatus::Running);
    assert_eq!(scheduler.ready.pop(), Some(pid));
}

#[test]
fn message_before_receive_after_cancels_pending_timer() {
    let (mut scheduler, module) = scheduler_with_test_module();
    let pid = 43;
    let mut process = waiting_process(pid, module);
    process.set_receive_timer_ref(Some(9));
    scheduler.processes.insert(pid, process);
    scheduler.waiting.insert(pid);

    assert!(scheduler.send(pid, Term::small_int(123)));

    assert_eq!(scheduler.take_pending_timer_cancellations(), vec![9]);
    let resumed = scheduler.processes.get(&pid).expect("process is retained");
    assert_eq!(resumed.receive_timer_ref(), None);
    assert_eq!(resumed.status(), ProcessStatus::Running);
    assert_eq!(scheduler.ready.pop(), Some(pid));
}

#[test]
fn host_send_owned_copies_message_into_receiver_heap_and_wakes() {
    let (mut scheduler, module) = scheduler_with_test_module();
    let pid = 47;
    let mut source = Process::new(900, DEFAULT_HEAP_SIZE);
    let mut context = ProcessContext::new();
    context.attach_process(&mut source, 0);
    let tuple = context
        .alloc_tuple(&[Term::small_int(1), Term::small_int(2)])
        .expect("source tuple allocation succeeds");
    context.detach_process();
    let owned = copy_term_to_ets(tuple).expect("tuple copies into host-owned storage");
    let process = waiting_process(pid, module);
    scheduler.processes.insert(pid, process);
    scheduler.waiting.insert(pid);

    scheduler
        .send_owned(pid, &owned)
        .expect("host-owned term sends to local pid");

    let Some(resumed) = scheduler.processes.get_mut(&pid) else {
        panic!("process is retained");
    };
    let Some(delivered) = resumed.mailbox_mut().current_message() else {
        panic!("message is visible through normal receive scan");
    };
    let delivered_tuple = Tuple::new(delivered).expect("delivered message is tuple-shaped");
    assert_eq!(delivered_tuple.get(0), Some(Term::small_int(1)));
    assert_eq!(delivered_tuple.get(1), Some(Term::small_int(2)));
    assert_eq!(resumed.status(), ProcessStatus::Running);
    assert_eq!(scheduler.ready.pop(), Some(pid));
}

#[test]
fn host_send_owned_rejects_missing_pid() {
    let (mut scheduler, _module) = scheduler_with_test_module();
    let owned = copy_term_to_ets(Term::small_int(5)).expect("immediate copies into owned storage");

    assert_eq!(scheduler.send_owned(99, &owned), Err(ExecError::Badarg));
}

#[test]
fn stale_timer_callback_is_ignored() {
    let (mut scheduler, module) = scheduler_with_test_module();
    let pid = 44;
    let mut process = waiting_process(pid, module);
    process.set_receive_timer_ref(Some(10));
    process.set_code_position(Some(CodePosition {
        module: Atom::NIL,
        instruction_pointer: 3,
    }));
    scheduler.processes.insert(pid, process);
    scheduler.waiting.insert(pid);

    assert!(!scheduler.timer_fired(pid, 11));

    let still_waiting = scheduler.processes.get(&pid).expect("process is retained");
    assert_eq!(still_waiting.receive_timer_ref(), Some(10));
    assert_eq!(still_waiting.status(), ProcessStatus::Waiting);
    assert!(scheduler.ready.pop().is_none());
}

#[test]
fn async_completion_rejects_missing_pid_without_recording_result() {
    let (mut scheduler, _module) = scheduler_with_test_module();

    assert!(!scheduler.complete_async(
        404,
        WasmAsyncCompletion::Ok(crate::ets::OwnedTerm::immediate(Term::small_int(1)))
    ));
    assert!(scheduler.async_results.is_empty());
}

#[test]
fn async_completion_injects_result_and_advances_call() {
    let (mut scheduler, module) = scheduler_with_test_module();
    let mut process = running_process(45, module);
    process.set_code_position(Some(CodePosition {
        module: Atom::NIL,
        instruction_pointer: 12,
    }));
    scheduler.async_results.insert(
        process.pid(),
        WasmAsyncCompletion::Ok(crate::ets::OwnedTerm::immediate(Term::small_int(987))),
    );

    assert_eq!(scheduler.apply_async_completion(&mut process), None);

    assert_eq!(process.x_reg(0), Term::small_int(987));
    assert_eq!(
        process.code_position(),
        Some(CodePosition {
            module: Atom::NIL,
            instruction_pointer: 13,
        })
    );
}

#[test]
fn async_rejection_maps_to_error_exit() {
    let (mut scheduler, module) = scheduler_with_test_module();
    let mut process = running_process(46, module);
    scheduler.async_results.insert(
        process.pid(),
        WasmAsyncCompletion::Error(crate::ets::OwnedTerm::immediate(Term::atom(Atom::BADARG))),
    );

    assert_eq!(
        scheduler.apply_async_completion(&mut process),
        Some(ExitReason::Error)
    );
    assert_eq!(process.x_reg(0), Term::atom(Atom::BADARG));
}

fn scheduler_with_test_module() -> (WasmScheduler, Arc<Module>) {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let modules = Arc::new(ModuleRegistry::new());
    let bifs = Arc::new(BifRegistryImpl::new());
    let module = Arc::new(dummy_module(Atom::NIL));
    (WasmScheduler::new(atom_table, modules, bifs), module)
}

fn waiting_process(pid: u64, module: Arc<Module>) -> Process {
    let mut process = running_process(pid, module);
    process
        .transition_to(ProcessStatus::Waiting)
        .expect("running process can wait");
    process
}

fn running_process(pid: u64, module: Arc<Module>) -> Process {
    let mut process = Process::new(pid, DEFAULT_HEAP_SIZE);
    process.set_current_module(module);
    process
        .transition_to(ProcessStatus::Running)
        .expect("new process can run");
    process
}

fn dummy_module(name: Atom) -> Module {
    Module {
        name,
        generation: 0,
        origin: ModuleOrigin::Preloaded,
        exports: HashMap::new(),
        label_index: HashMap::new(),
        code: Vec::<Instruction>::new(),
        function_table: Vec::new(),
        line_table: Vec::new(),
        literals: Vec::<Literal>::new(),
        constant_pool: ConstantPool::new(),
        resolved_imports: Vec::<ResolvedImport>::new(),
        lambdas: Vec::<LambdaEntry>::new(),
        string_table: Vec::new(),
        line_info: Vec::<LineInfo>::new(),
    }
}
