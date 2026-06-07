use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::atom::{Atom, AtomTable};
use crate::native::links::{LinkError, LinkFacility, LinkRecord};
use crate::native::registry::{RegistryError, RegistryFacility};
use crate::native::supervision::{
    MonitorResult, SupervisionError, SupervisionFacility, SupervisionRecord,
};
use crate::native::{BifRegistryImpl, ProcessContext};
use crate::process::ExitReason;
use crate::process::Process;
use crate::scheduler::dirty::DirtySchedulerKind;
use crate::term::Term;
use crate::term::boxed::Tuple;

use super::*;

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}

// ---------------------------------------------------------------------------
// R1: trap_exits/1
// ---------------------------------------------------------------------------

#[test]
fn trap_exits_sets_flag_and_returns_nil() {
    let (_, mut ctx) = link_ctx(1);
    let result = bif_trap_exits(&[Term::atom(Atom::TRUE)], &mut ctx);
    assert_eq!(result, Ok(GLEAM_NIL));
}

#[test]
fn trap_exits_accepts_false() {
    let (_, mut ctx) = link_ctx(1);
    let result = bif_trap_exits(&[Term::atom(Atom::FALSE)], &mut ctx);
    assert_eq!(result, Ok(GLEAM_NIL));
}

#[test]
fn trap_exits_badarg_non_bool() {
    let (_, mut ctx) = link_ctx(1);
    assert_eq!(
        bif_trap_exits(&[Term::atom(Atom::OK)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn trap_exits_badarg_no_facility() {
    let _process = Process::new(1, 64);
    let mut ctx = ProcessContext::new();
    ctx.set_pid(Some(1));
    assert_eq!(
        bif_trap_exits(&[Term::atom(Atom::TRUE)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn trap_exits_badarg_wrong_arity() {
    let (_, mut ctx) = link_ctx(1);
    assert_eq!(bif_trap_exits(&[], &mut ctx), Err(badarg()));
}

// ---------------------------------------------------------------------------
// R1: link/1
// ---------------------------------------------------------------------------

#[test]
fn gleam_link_delegates_and_returns_nil() {
    let (f, mut ctx) = link_ctx(1);
    let result = bif_gleam_link(&[Term::pid(2)], &mut ctx);
    assert_eq!(result, Ok(GLEAM_NIL));
    assert_eq!(
        f.records(),
        vec![LinkRecord::Link {
            caller_pid: 1,
            target_pid: 2
        }]
    );
}

#[test]
fn gleam_link_badarg_non_pid() {
    let (_, mut ctx) = link_ctx(1);
    assert_eq!(
        bif_gleam_link(&[Term::small_int(2)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn gleam_link_badarg_no_facility() {
    let _process = Process::new(1, 64);
    let mut ctx = ProcessContext::new();
    ctx.set_pid(Some(1));
    assert_eq!(bif_gleam_link(&[Term::pid(2)], &mut ctx), Err(badarg()));
}

// ---------------------------------------------------------------------------
// R1: demonitor/1
// ---------------------------------------------------------------------------

#[test]
fn gleam_demonitor_delegates_and_returns_nil() {
    let (f, mut ctx) = sup_ctx(0, 1);
    let result = bif_gleam_demonitor(&[Term::small_int(42)], &mut ctx);
    assert_eq!(result, Ok(GLEAM_NIL));
    assert_eq!(
        f.records(),
        vec![SupervisionRecord::Demonitor {
            caller_pid: 1,
            reference: 42
        }]
    );
}

#[test]
fn gleam_demonitor_badarg_negative() {
    let (_, mut ctx) = sup_ctx(0, 1);
    assert_eq!(
        bif_gleam_demonitor(&[Term::small_int(-1)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn gleam_demonitor_badarg_non_integer() {
    let (_, mut ctx) = sup_ctx(0, 1);
    assert_eq!(
        bif_gleam_demonitor(&[Term::atom(Atom::OK)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn gleam_demonitor_badarg_no_facility() {
    let _process = Process::new(1, 64);
    let mut ctx = ProcessContext::new();
    ctx.set_pid(Some(1));
    assert_eq!(
        bif_gleam_demonitor(&[Term::small_int(42)], &mut ctx),
        Err(badarg())
    );
}

// ---------------------------------------------------------------------------
// R2: sleep/1
// ---------------------------------------------------------------------------

#[test]
fn sleep_returns_nil_after_delay() {
    let mut ctx = ProcessContext::new();
    let start = std::time::Instant::now();
    let result = bif_sleep(&[Term::small_int(10)], &mut ctx);
    assert_eq!(result, Ok(GLEAM_NIL));
    assert!(start.elapsed() >= std::time::Duration::from_millis(5));
}

#[test]
fn sleep_zero_returns_immediately() {
    let mut ctx = ProcessContext::new();
    assert_eq!(bif_sleep(&[Term::small_int(0)], &mut ctx), Ok(GLEAM_NIL));
}

#[test]
fn sleep_badarg_negative() {
    let mut ctx = ProcessContext::new();
    assert_eq!(bif_sleep(&[Term::small_int(-1)], &mut ctx), Err(badarg()));
}

#[test]
fn sleep_badarg_non_integer() {
    let mut ctx = ProcessContext::new();
    assert_eq!(bif_sleep(&[Term::atom(Atom::OK)], &mut ctx), Err(badarg()));
}

#[test]
fn sleep_badarg_wrong_arity() {
    let mut ctx = ProcessContext::new();
    assert_eq!(bif_sleep(&[], &mut ctx), Err(badarg()));
}

// ---------------------------------------------------------------------------
// R2: sleep_forever/0 — can't test infinite sleep, just test arity validation
// ---------------------------------------------------------------------------

#[test]
fn sleep_forever_badarg_wrong_arity() {
    let mut ctx = ProcessContext::new();
    assert_eq!(
        bif_sleep_forever(&[Term::small_int(1)], &mut ctx),
        Err(badarg())
    );
}

// ---------------------------------------------------------------------------
// R2: flush_messages/0
// ---------------------------------------------------------------------------

#[test]
fn flush_messages_returns_nil() {
    let mut ctx = ProcessContext::new();
    assert_eq!(bif_flush_messages(&[], &mut ctx), Ok(GLEAM_NIL));
}

#[test]
fn flush_messages_badarg_wrong_arity() {
    let mut ctx = ProcessContext::new();
    assert_eq!(
        bif_flush_messages(&[Term::small_int(1)], &mut ctx),
        Err(badarg())
    );
}

// ---------------------------------------------------------------------------
// R2: register_process/2
// ---------------------------------------------------------------------------

#[test]
fn register_process_delegates_and_returns_nil() {
    let (f, mut ctx) = reg_ctx(1);
    let result = bif_register_process(&[Term::atom(Atom::OK), Term::pid(1)], &mut ctx);
    assert_eq!(result, Ok(GLEAM_NIL));
    assert_eq!(f.whereis(Atom::OK), Some(1));
}

#[test]
fn register_process_badarg_non_atom_name() {
    let (_, mut ctx) = reg_ctx(1);
    assert_eq!(
        bif_register_process(&[Term::small_int(1), Term::pid(1)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn register_process_badarg_non_pid() {
    let (_, mut ctx) = reg_ctx(1);
    assert_eq!(
        bif_register_process(&[Term::atom(Atom::OK), Term::small_int(1)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn register_process_badarg_no_facility() {
    let mut ctx = ProcessContext::new();
    assert_eq!(
        bif_register_process(&[Term::atom(Atom::OK), Term::pid(1)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn register_process_badarg_already_registered() {
    let (f, mut ctx) = reg_ctx(1);
    f.register(Atom::OK, 99).unwrap_or_default();
    assert_eq!(
        bif_register_process(&[Term::atom(Atom::OK), Term::pid(1)], &mut ctx),
        Err(badarg())
    );
}

// ---------------------------------------------------------------------------
// R2: unregister_process/1
// ---------------------------------------------------------------------------

#[test]
fn unregister_process_removes_and_returns_nil() {
    let (f, mut ctx) = reg_ctx(1);
    f.register(Atom::OK, 1).unwrap_or_default();
    let result = bif_unregister_process(&[Term::atom(Atom::OK)], &mut ctx);
    assert_eq!(result, Ok(GLEAM_NIL));
    assert_eq!(f.whereis(Atom::OK), None);
}

#[test]
fn unregister_process_badarg_not_registered() {
    let (_, mut ctx) = reg_ctx(1);
    assert_eq!(
        bif_unregister_process(&[Term::atom(Atom::OK)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn unregister_process_badarg_non_atom() {
    let (_, mut ctx) = reg_ctx(1);
    assert_eq!(
        bif_unregister_process(&[Term::small_int(1)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn unregister_process_badarg_no_facility() {
    let mut ctx = ProcessContext::new();
    assert_eq!(
        bif_unregister_process(&[Term::atom(Atom::OK)], &mut ctx),
        Err(badarg())
    );
}

// ---------------------------------------------------------------------------
// R2: process_named/1
// ---------------------------------------------------------------------------

#[test]
fn process_named_returns_ok_tuple_when_found() {
    let (f, mut ctx) = reg_ctx(1);
    f.register(Atom::OK, 42).unwrap_or_default();
    let result = bif_process_named(&[Term::atom(Atom::OK)], &mut ctx).expect("should succeed");
    let tuple = Tuple::new(result).expect("should be a tuple");
    assert_eq!(tuple.get(0), Some(Term::atom(Atom::OK)));
    assert_eq!(tuple.get(1), Some(Term::pid(42)));
}

#[test]
fn process_named_returns_error_nil_when_not_found() {
    let (_, mut ctx) = reg_ctx(1);
    let result = bif_process_named(&[Term::atom(Atom::OK)], &mut ctx).expect("should succeed");
    let tuple = Tuple::new(result).expect("should be a tuple");
    assert_eq!(tuple.get(0), Some(Term::atom(Atom::ERROR)));
    assert_eq!(tuple.get(1), Some(GLEAM_NIL));
}

#[test]
fn process_named_badarg_non_atom() {
    let (_, mut ctx) = reg_ctx(1);
    assert_eq!(
        bif_process_named(&[Term::small_int(1)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn process_named_badarg_no_facility() {
    let mut ctx = ProcessContext::new();
    assert_eq!(
        bif_process_named(&[Term::atom(Atom::OK)], &mut ctx),
        Err(badarg())
    );
}

// ---------------------------------------------------------------------------
// R3: pid_from_dynamic/1
// ---------------------------------------------------------------------------

#[test]
fn pid_from_dynamic_returns_ok_for_pid() {
    let process = Box::leak(Box::new(Process::new(1, 128)));
    let mut ctx = ProcessContext::new();
    ctx.attach_process(process, 0);
    let pid = Term::pid(42);
    let result = bif_pid_from_dynamic(&[pid], &mut ctx).expect("should succeed");
    let tuple = Tuple::new(result).expect("should be a tuple");
    assert_eq!(tuple.get(0), Some(Term::atom(Atom::OK)));
    assert_eq!(tuple.get(1), Some(pid));
}

#[test]
fn pid_from_dynamic_returns_error_for_non_pid() {
    let process = Box::leak(Box::new(Process::new(1, 128)));
    let mut ctx = ProcessContext::new();
    ctx.attach_process(process, 0);
    let result = bif_pid_from_dynamic(&[Term::small_int(42)], &mut ctx).expect("should succeed");
    let tuple = Tuple::new(result).expect("should be a tuple");
    assert_eq!(tuple.get(0), Some(Term::atom(Atom::ERROR)));
    assert_eq!(tuple.get(1), Some(GLEAM_NIL));
}

#[test]
fn pid_from_dynamic_returns_error_for_atom() {
    let process = Box::leak(Box::new(Process::new(1, 128)));
    let mut ctx = ProcessContext::new();
    ctx.attach_process(process, 0);
    let result = bif_pid_from_dynamic(&[Term::atom(Atom::OK)], &mut ctx).expect("should succeed");
    let tuple = Tuple::new(result).expect("should be a tuple");
    assert_eq!(tuple.get(0), Some(Term::atom(Atom::ERROR)));
    assert_eq!(tuple.get(1), Some(GLEAM_NIL));
}

#[test]
fn pid_from_dynamic_badarg_wrong_arity() {
    let mut ctx = ProcessContext::new();
    assert_eq!(bif_pid_from_dynamic(&[], &mut ctx), Err(badarg()));
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

#[test]
fn register_gleam_ffi_bifs_registers_all_expected() {
    let atom_table = AtomTable::with_common_atoms();
    let registry = BifRegistryImpl::new();
    register_gleam_ffi_bifs(&registry, &atom_table).expect("gleam ffi registration should succeed");

    let module = atom_table.intern("gleam_erlang_ffi");
    for (name, arity) in [
        ("trap_exits", 1),
        ("link", 1),
        ("demonitor", 1),
        ("sleep", 1),
        ("sleep_forever", 0),
        ("flush_messages", 0),
        ("register_process", 2),
        ("unregister_process", 1),
        ("process_named", 1),
        ("pid_from_dynamic", 1),
    ] {
        let function = atom_table.intern(name);
        assert!(
            registry.lookup(module, function, arity).is_some(),
            "missing gleam_erlang_ffi:{name}/{arity}"
        );
    }

    let sleep = atom_table.intern("sleep");
    let sleep_entry = registry
        .lookup(module, sleep, 1)
        .expect("gleam_erlang_ffi:sleep/1");
    assert_eq!(sleep_entry.dirty_kind, Some(DirtySchedulerKind::Io));
}

#[test]
fn register_gleam_ffi_bifs_fails_on_duplicate() {
    let atom_table = AtomTable::with_common_atoms();
    let registry = BifRegistryImpl::new();
    register_gleam_ffi_bifs(&registry, &atom_table).expect("first");
    assert!(register_gleam_ffi_bifs(&registry, &atom_table).is_err());
}

#[test]
fn gleam_ffi_coexists_with_selector_bifs() {
    use crate::native::selector_ffi::register_selector_bifs;

    let atom_table = AtomTable::with_common_atoms();
    let registry = BifRegistryImpl::new();
    register_selector_bifs(&registry, &atom_table).expect("selector");
    register_gleam_ffi_bifs(&registry, &atom_table).expect("gleam ffi");

    let module = atom_table.intern("gleam_erlang_ffi");

    // Selector BIF still present.
    let new_sel = atom_table.intern("new_selector");
    assert!(registry.lookup(module, new_sel, 0).is_some());

    // Gleam FFI BIF present.
    let trap = atom_table.intern("trap_exits");
    assert!(registry.lookup(module, trap, 1).is_some());
}

// ---------------------------------------------------------------------------
// Mock facilities
// ---------------------------------------------------------------------------

fn link_ctx(caller_pid: u64) -> (Arc<MockLinkFacility>, ProcessContext<'static>) {
    let f = Arc::new(MockLinkFacility::new());
    let mut ctx = ProcessContext::new();
    ctx.set_pid(Some(caller_pid));
    ctx.set_link_facility(Some(f.clone()));
    (f, ctx)
}

fn sup_ctx(
    next_ref: u64,
    caller_pid: u64,
) -> (Arc<MockSupervisionFacility>, ProcessContext<'static>) {
    let f = Arc::new(MockSupervisionFacility::new(next_ref));
    let mut ctx = ProcessContext::new();
    ctx.set_pid(Some(caller_pid));
    ctx.set_supervision_facility(Some(f.clone()));
    (f, ctx)
}

fn reg_ctx(caller_pid: u64) -> (Arc<MockRegistryFacility>, ProcessContext<'static>) {
    let f = Arc::new(MockRegistryFacility::new());
    let process = Box::leak(Box::new(Process::new(caller_pid, 128)));
    let mut ctx = ProcessContext::new();
    ctx.set_pid(Some(caller_pid));
    ctx.set_registry_facility(Some(f.clone()));
    ctx.attach_process(process, 0);
    (f, ctx)
}

struct MockLinkFacility {
    records: Mutex<Vec<LinkRecord>>,
}

impl MockLinkFacility {
    fn new() -> Self {
        Self {
            records: Mutex::new(Vec::new()),
        }
    }

    fn records(&self) -> Vec<LinkRecord> {
        self.records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

impl LinkFacility for MockLinkFacility {
    fn link(&self, caller_pid: u64, target_pid: u64) -> Result<(), LinkError> {
        self.records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(LinkRecord::Link {
                caller_pid,
                target_pid,
            });
        Ok(())
    }

    fn unlink(&self, caller_pid: u64, target_pid: u64) -> Result<(), LinkError> {
        self.records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(LinkRecord::Unlink {
                caller_pid,
                target_pid,
            });
        Ok(())
    }

    fn set_trap_exit(&self, _caller_pid: u64, _value: bool) -> Result<bool, LinkError> {
        Ok(false)
    }
}

struct MockSupervisionFacility {
    next_reference: u64,
    records: Mutex<Vec<SupervisionRecord>>,
}

impl MockSupervisionFacility {
    fn new(next_reference: u64) -> Self {
        Self {
            next_reference,
            records: Mutex::new(Vec::new()),
        }
    }

    fn records(&self) -> Vec<SupervisionRecord> {
        self.records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

impl SupervisionFacility for MockSupervisionFacility {
    fn monitor(&self, caller_pid: u64, target_pid: u64) -> Result<MonitorResult, SupervisionError> {
        self.records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(SupervisionRecord::Monitor {
                caller_pid,
                target_pid,
            });
        Ok(MonitorResult {
            reference: self.next_reference,
            immediate_down: false,
        })
    }

    fn demonitor(&self, caller_pid: u64, reference: u64) -> Result<(), SupervisionError> {
        self.records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(SupervisionRecord::Demonitor {
                caller_pid,
                reference,
            });
        Ok(())
    }

    fn exit_signal(
        &self,
        caller_pid: u64,
        target_pid: u64,
        reason: ExitReason,
    ) -> Result<(), SupervisionError> {
        self.records.lock().unwrap_or_else(|e| e.into_inner()).push(
            SupervisionRecord::ExitSignal {
                caller_pid,
                target_pid,
                reason,
            },
        );
        Ok(())
    }
}

struct MockRegistryFacility {
    entries: Mutex<HashMap<u32, u64>>,
    pids: Mutex<HashMap<u64, u32>>,
}

impl MockRegistryFacility {
    fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            pids: Mutex::new(HashMap::new()),
        }
    }
}

impl RegistryFacility for MockRegistryFacility {
    fn register(&self, name: Atom, pid: u64) -> Result<(), RegistryError> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let mut pids = self.pids.lock().unwrap_or_else(|e| e.into_inner());
        if entries.contains_key(&name.index()) {
            return Err(RegistryError::AlreadyRegistered);
        }
        if pids.contains_key(&pid) {
            return Err(RegistryError::PidAlreadyRegistered);
        }
        entries.insert(name.index(), pid);
        pids.insert(pid, name.index());
        Ok(())
    }

    fn unregister(&self, name: Atom) -> Result<(), RegistryError> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let mut pids = self.pids.lock().unwrap_or_else(|e| e.into_inner());
        let pid = entries
            .remove(&name.index())
            .ok_or(RegistryError::NotRegistered)?;
        pids.remove(&pid);
        Ok(())
    }

    fn whereis(&self, name: Atom) -> Option<u64> {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.get(&name.index()).copied()
    }
}
