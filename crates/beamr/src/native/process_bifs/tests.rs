use super::*;
use crate::atom::{Atom, AtomTable};
use crate::native::links::{LinkError, LinkFacility, LinkRecord};
use crate::native::spawn::{SpawnError, SpawnFacility, SpawnRecord};
use crate::native::supervision::{
    MonitorResult, SupervisionError, SupervisionFacility, SupervisionRecord,
};
use crate::native::{BifRegistryImpl, ProcessContext};
use crate::process::ExitReason;
use crate::term::Term;
use crate::term::boxed::write_cons;
use std::sync::{Arc, Mutex};

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}

// ---- erlang:self/0 ----

#[test]
fn self_returns_pid() {
    let mut ctx = ProcessContext::new();
    ctx.set_pid(Some(42));
    assert_eq!(bif_self(&[], &mut ctx), Ok(Term::pid(42)));
}

#[test]
fn self_badarg_no_pid() {
    assert_eq!(bif_self(&[], &mut ProcessContext::new()), Err(badarg()));
}

#[test]
fn self_badarg_wrong_arity() {
    let mut ctx = ProcessContext::new();
    ctx.set_pid(Some(1));
    assert_eq!(bif_self(&[Term::small_int(1)], &mut ctx), Err(badarg()));
}

// ---- erlang:spawn/3 ----

#[test]
fn spawn_badarg_without_facility() {
    let mut ctx = ProcessContext::new();
    ctx.set_pid(Some(0));
    assert_eq!(
        bif_spawn(
            &[Term::atom(Atom::OK), Term::atom(Atom::ERROR), Term::NIL],
            &mut ctx
        ),
        Err(badarg()),
    );
}

#[test]
fn spawn_returns_new_pid() {
    let (f, mut ctx) = spawn_ctx(7, 0);
    assert_eq!(
        bif_spawn(
            &[Term::atom(Atom::OK), Term::atom(Atom::ERROR), Term::NIL],
            &mut ctx
        ),
        Ok(Term::pid(7)),
    );
    assert_eq!(f.records().len(), 1);
    assert_eq!(f.records()[0].caller_pid, 0);
    assert_eq!(f.records()[0].link_to, None);
}

#[test]
fn spawn_passes_list_args() {
    let (f, mut ctx) = spawn_ctx(10, 0);
    let mut c2 = [0u64; 2];
    let tail = write_cons(&mut c2, Term::small_int(2), Term::NIL).unwrap();
    let mut c1 = [0u64; 2];
    let list = write_cons(&mut c1, Term::small_int(1), tail).unwrap();
    assert_eq!(
        bif_spawn(
            &[Term::atom(Atom::OK), Term::atom(Atom::ERROR), list],
            &mut ctx
        ),
        Ok(Term::pid(10)),
    );
    assert_eq!(
        f.records()[0].args,
        vec![Term::small_int(1), Term::small_int(2)]
    );
}

#[test]
fn spawn_badarg_non_atom_module() {
    let (_, mut ctx) = spawn_ctx(1, 0);
    assert_eq!(
        bif_spawn(
            &[Term::small_int(42), Term::atom(Atom::OK), Term::NIL],
            &mut ctx
        ),
        Err(badarg()),
    );
}

#[test]
fn spawn_badarg_wrong_arity() {
    assert_eq!(
        bif_spawn(&[Term::atom(Atom::OK)], &mut ProcessContext::new()),
        Err(badarg())
    );
}

#[test]
fn spawn_badarg_facility_fails() {
    let f: Arc<dyn SpawnFacility> = Arc::new(FailingSpawnFacility);
    let mut ctx = ProcessContext::new();
    ctx.set_pid(Some(0));
    ctx.set_spawn_facility(Some(f));
    assert_eq!(
        bif_spawn(
            &[Term::atom(Atom::OK), Term::atom(Atom::ERROR), Term::NIL],
            &mut ctx
        ),
        Err(badarg()),
    );
}

// ---- erlang:spawn_link/3 ----

#[test]
fn spawn_link_sets_link_to_parent() {
    let (f, mut ctx) = spawn_ctx(8, 3);
    assert_eq!(
        bif_spawn_link(
            &[Term::atom(Atom::OK), Term::atom(Atom::ERROR), Term::NIL],
            &mut ctx
        ),
        Ok(Term::pid(8)),
    );
    assert_eq!(f.records()[0].caller_pid, 3);
    assert_eq!(f.records()[0].link_to, Some(3));
}

#[test]
fn spawn_link_badarg_without_pid() {
    let f: Arc<dyn SpawnFacility> = Arc::new(MockSpawnFacility::new(8));
    let mut ctx = ProcessContext::new();
    ctx.set_spawn_facility(Some(f));
    assert_eq!(
        bif_spawn_link(
            &[Term::atom(Atom::OK), Term::atom(Atom::ERROR), Term::NIL],
            &mut ctx
        ),
        Err(badarg()),
    );
}

// ---- erlang:link/1 ----

#[test]
fn link_establishes_bidirectional_link() {
    let (f, mut ctx) = link_ctx(1);
    assert_eq!(
        bif_link(&[Term::pid(2)], &mut ctx),
        Ok(Term::atom(Atom::TRUE))
    );
    assert_eq!(
        f.records(),
        vec![LinkRecord::Link {
            caller_pid: 1,
            target_pid: 2
        }]
    );
}

#[test]
fn link_self_is_noop() {
    let (f, mut ctx) = link_ctx(1);
    assert_eq!(
        bif_link(&[Term::pid(1)], &mut ctx),
        Ok(Term::atom(Atom::TRUE))
    );
    assert!(f.records().is_empty());
}

#[test]
fn link_noproc_for_dead_target() {
    let f: Arc<dyn LinkFacility> = Arc::new(NoprocLinkFacility);
    let mut ctx = ProcessContext::new();
    ctx.set_pid(Some(1));
    ctx.set_link_facility(Some(f));
    assert_eq!(
        bif_link(&[Term::pid(2)], &mut ctx),
        Err(Term::atom(Atom::NOPROC))
    );
}

#[test]
fn link_badarg_no_pid() {
    let mut ctx = ProcessContext::new();
    assert_eq!(bif_link(&[Term::pid(2)], &mut ctx), Err(badarg()));
}

#[test]
fn link_badarg_non_pid_arg() {
    let (_, mut ctx) = link_ctx(1);
    assert_eq!(bif_link(&[Term::small_int(2)], &mut ctx), Err(badarg()));
}

// ---- erlang:unlink/1 ----

#[test]
fn unlink_removes_link() {
    let (f, mut ctx) = link_ctx(1);
    assert_eq!(
        bif_unlink(&[Term::pid(2)], &mut ctx),
        Ok(Term::atom(Atom::TRUE))
    );
    assert_eq!(
        f.records(),
        vec![LinkRecord::Unlink {
            caller_pid: 1,
            target_pid: 2
        }]
    );
}

#[test]
fn unlink_self_is_noop() {
    let (f, mut ctx) = link_ctx(1);
    assert_eq!(
        bif_unlink(&[Term::pid(1)], &mut ctx),
        Ok(Term::atom(Atom::TRUE))
    );
    assert!(f.records().is_empty());
}

// ---- erlang:process_flag/2 ----

#[test]
fn process_flag_trap_exit_returns_old_value() {
    let (_, mut ctx) = link_ctx(1);
    assert_eq!(
        bif_process_flag(
            &[Term::atom(Atom::TRAP_EXIT), Term::atom(Atom::TRUE)],
            &mut ctx
        ),
        Ok(Term::atom(Atom::FALSE)),
    );
}

#[test]
fn process_flag_badarg_unknown_flag() {
    let (_, mut ctx) = link_ctx(1);
    assert_eq!(
        bif_process_flag(&[Term::atom(Atom::OK), Term::atom(Atom::TRUE)], &mut ctx),
        Err(badarg()),
    );
}

#[test]
fn process_flag_badarg_non_bool_value() {
    let (_, mut ctx) = link_ctx(1);
    assert_eq!(
        bif_process_flag(
            &[Term::atom(Atom::TRAP_EXIT), Term::atom(Atom::OK)],
            &mut ctx
        ),
        Err(badarg()),
    );
}

// ---- list_to_vec ----

#[test]
fn list_to_vec_empty() {
    assert!(list_to_vec(Term::NIL).unwrap().is_empty());
}

#[test]
fn list_to_vec_proper() {
    let mut c2 = [0u64; 2];
    let tail = write_cons(&mut c2, Term::small_int(2), Term::NIL).unwrap();
    let mut c1 = [0u64; 2];
    let list = write_cons(&mut c1, Term::small_int(1), tail).unwrap();
    assert_eq!(
        list_to_vec(list).unwrap(),
        vec![Term::small_int(1), Term::small_int(2)]
    );
}

#[test]
fn list_to_vec_rejects_non_list() {
    assert_eq!(list_to_vec(Term::small_int(42)), Err(badarg()));
}

// ---- Registration ----

#[test]
fn register_gate2_bifs_registers_all() {
    let at = AtomTable::new();
    let reg = BifRegistryImpl::new();
    register_gate2_bifs(&reg, &at).expect("gate 2 registration");
    let erlang = at.intern("erlang");
    for (name, arity) in [
        ("self", 0),
        ("spawn", 3),
        ("spawn_link", 3),
        ("link", 1),
        ("unlink", 1),
        ("process_flag", 2),
        ("monitor", 2),
        ("demonitor", 1),
        ("exit", 2),
    ] {
        assert!(
            reg.lookup(erlang, at.intern(name), arity).is_some(),
            "missing erlang:{name}/{arity}"
        );
    }
}

#[test]
fn register_gate2_bifs_fails_twice() {
    let at = AtomTable::new();
    let reg = BifRegistryImpl::new();
    register_gate2_bifs(&reg, &at).expect("first");
    assert!(register_gate2_bifs(&reg, &at).is_err());
}

#[test]
fn gate1_and_gate2_coexist() {
    let at = AtomTable::new();
    let reg = BifRegistryImpl::new();
    crate::native::bifs::register_gate1_bifs(&reg, &at).expect("gate 1");
    register_gate2_bifs(&reg, &at).expect("gate 2");
    let erlang = at.intern("erlang");
    assert!(reg.lookup(erlang, at.intern("+"), 2).is_some());
    assert!(reg.lookup(erlang, at.intern("self"), 0).is_some());
    assert!(reg.lookup(erlang, at.intern("monitor"), 2).is_some());
}

// ---- erlang:monitor/2 ----

#[test]
fn monitor_returns_reference() {
    let (f, mut ctx) = sup_ctx(42, 1);
    let result = bif_monitor(&[Term::atom(Atom::PROCESS), Term::pid(2)], &mut ctx);
    assert_eq!(result, Ok(Term::small_int(42)));
    assert_eq!(
        f.records(),
        vec![SupervisionRecord::Monitor {
            caller_pid: 1,
            target_pid: 2
        }]
    );
}

#[test]
fn monitor_badarg_non_process_type() {
    let (_, mut ctx) = sup_ctx(1, 1);
    assert_eq!(
        bif_monitor(&[Term::atom(Atom::OK), Term::pid(2)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn monitor_badarg_non_pid_target() {
    let (_, mut ctx) = sup_ctx(1, 1);
    assert_eq!(
        bif_monitor(&[Term::atom(Atom::PROCESS), Term::small_int(2)], &mut ctx),
        Err(badarg()),
    );
}

#[test]
fn monitor_badarg_no_facility() {
    let mut ctx = ProcessContext::new();
    ctx.set_pid(Some(1));
    assert_eq!(
        bif_monitor(&[Term::atom(Atom::PROCESS), Term::pid(2)], &mut ctx),
        Err(badarg()),
    );
}

// ---- erlang:demonitor/1 ----

#[test]
fn demonitor_returns_true() {
    let (f, mut ctx) = sup_ctx(0, 1);
    assert_eq!(
        bif_demonitor(&[Term::small_int(42)], &mut ctx),
        Ok(Term::atom(Atom::TRUE))
    );
    assert_eq!(
        f.records(),
        vec![SupervisionRecord::Demonitor {
            caller_pid: 1,
            reference: 42
        }]
    );
}

#[test]
fn demonitor_badarg_negative_ref() {
    let (_, mut ctx) = sup_ctx(0, 1);
    assert_eq!(
        bif_demonitor(&[Term::small_int(-1)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn demonitor_badarg_non_integer() {
    let (_, mut ctx) = sup_ctx(0, 1);
    assert_eq!(
        bif_demonitor(&[Term::atom(Atom::OK)], &mut ctx),
        Err(badarg())
    );
}

// ---- erlang:exit/2 ----

#[test]
fn exit_sends_signal_and_returns_true() {
    let (f, mut ctx) = sup_ctx(0, 1);
    assert_eq!(
        bif_exit(&[Term::pid(2), Term::atom(Atom::KILL)], &mut ctx),
        Ok(Term::atom(Atom::TRUE))
    );
    assert_eq!(
        f.records(),
        vec![SupervisionRecord::ExitSignal {
            caller_pid: 1,
            target_pid: 2,
            reason: ExitReason::Kill
        }]
    );
}

#[test]
fn exit_normal_reason() {
    let (f, mut ctx) = sup_ctx(0, 1);
    assert_eq!(
        bif_exit(&[Term::pid(2), Term::atom(Atom::NORMAL)], &mut ctx),
        Ok(Term::atom(Atom::TRUE))
    );
    assert_eq!(
        f.records()[0],
        SupervisionRecord::ExitSignal {
            caller_pid: 1,
            target_pid: 2,
            reason: ExitReason::Normal
        }
    );
}

#[test]
fn exit_badarg_non_pid() {
    let (_, mut ctx) = sup_ctx(0, 1);
    assert_eq!(
        bif_exit(&[Term::small_int(2), Term::atom(Atom::KILL)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn exit_badarg_unknown_reason_atom() {
    let (_, mut ctx) = sup_ctx(0, 1);
    assert_eq!(
        bif_exit(&[Term::pid(2), Term::atom(Atom::OK)], &mut ctx),
        Err(badarg())
    );
}

// ---- Helpers ----

fn spawn_ctx(next_pid: u64, caller_pid: u64) -> (Arc<MockSpawnFacility>, ProcessContext) {
    let f = Arc::new(MockSpawnFacility::new(next_pid));
    let mut ctx = ProcessContext::new();
    ctx.set_pid(Some(caller_pid));
    ctx.set_spawn_facility(Some(f.clone()));
    (f, ctx)
}

fn link_ctx(caller_pid: u64) -> (Arc<MockLinkFacility>, ProcessContext) {
    let f = Arc::new(MockLinkFacility::new());
    let mut ctx = ProcessContext::new();
    ctx.set_pid(Some(caller_pid));
    ctx.set_link_facility(Some(f.clone()));
    (f, ctx)
}

fn sup_ctx(next_ref: u64, caller_pid: u64) -> (Arc<MockSupervisionFacility>, ProcessContext) {
    let f = Arc::new(MockSupervisionFacility::new(next_ref));
    let mut ctx = ProcessContext::new();
    ctx.set_pid(Some(caller_pid));
    ctx.set_supervision_facility(Some(f.clone()));
    (f, ctx)
}

// ---- Mock spawn facility ----

struct MockSpawnFacility {
    next_pid: u64,
    records: Mutex<Vec<SpawnRecord>>,
}

impl MockSpawnFacility {
    fn new(next_pid: u64) -> Self {
        Self {
            next_pid,
            records: Mutex::new(Vec::new()),
        }
    }
    fn records(&self) -> Vec<SpawnRecord> {
        self.records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

impl SpawnFacility for MockSpawnFacility {
    fn spawn(
        &self,
        caller_pid: u64,
        module: Atom,
        function: Atom,
        args: Vec<Term>,
        link_to: Option<u64>,
    ) -> Result<u64, SpawnError> {
        self.records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(SpawnRecord {
                caller_pid,
                module,
                function,
                args,
                link_to,
            });
        Ok(self.next_pid)
    }

    fn spawn_lambda(&self, _: u64, _: Atom, _: u32, _: Option<u64>) -> Result<u64, SpawnError> {
        Ok(self.next_pid)
    }
}

struct FailingSpawnFacility;

impl SpawnFacility for FailingSpawnFacility {
    fn spawn(
        &self,
        _: u64,
        _: Atom,
        _: Atom,
        _: Vec<Term>,
        _: Option<u64>,
    ) -> Result<u64, SpawnError> {
        Err(SpawnError::UnresolvedMfa)
    }

    fn spawn_lambda(&self, _: u64, _: Atom, _: u32, _: Option<u64>) -> Result<u64, SpawnError> {
        Err(SpawnError::UnresolvedMfa)
    }
}

// ---- Mock link facility ----

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

struct NoprocLinkFacility;

impl LinkFacility for NoprocLinkFacility {
    fn link(&self, _: u64, _: u64) -> Result<(), LinkError> {
        Err(LinkError::NoProc)
    }
    fn unlink(&self, _: u64, _: u64) -> Result<(), LinkError> {
        Err(LinkError::NoProc)
    }
    fn set_trap_exit(&self, _: u64, _: bool) -> Result<bool, LinkError> {
        Err(LinkError::NoProc)
    }
}

// ---- Mock supervision facility ----

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
