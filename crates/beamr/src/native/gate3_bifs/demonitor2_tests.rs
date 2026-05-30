use std::sync::{Arc, Mutex};

use crate::atom::Atom;
use crate::native::supervision::{
    MonitorResult, SupervisionError, SupervisionFacility, SupervisionRecord,
};
use crate::native::ProcessContext;
use crate::process::ExitReason;
use crate::term::Term;
use crate::term::boxed::write_cons;

use super::bif_demonitor_2;

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}

fn sup_ctx(
    next_ref: u64,
    caller_pid: u64,
    alive: bool,
) -> (Arc<MockSupervisionFacility>, ProcessContext) {
    let f = Arc::new(MockSupervisionFacility::new(next_ref, alive));
    let mut ctx = ProcessContext::new();
    ctx.set_pid(Some(caller_pid));
    ctx.set_supervision_facility(Some(f.clone()));
    (f, ctx)
}

// ---- erlang:demonitor/2 ----

#[test]
fn demonitor2_empty_options_behaves_like_demonitor1() {
    let (f, mut ctx) = sup_ctx(0, 1, true);
    assert_eq!(
        bif_demonitor_2(&[Term::small_int(42), Term::NIL], &mut ctx),
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
fn demonitor2_flush_option() {
    let (f, mut ctx) = sup_ctx(0, 1, true);
    let mut cell = [0u64; 2];
    let opts = write_cons(&mut cell, Term::atom(Atom::FLUSH), Term::NIL).unwrap();
    assert_eq!(
        bif_demonitor_2(&[Term::small_int(42), opts], &mut ctx),
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
fn demonitor2_info_option_returns_true_when_active() {
    let (_, mut ctx) = sup_ctx(0, 1, true);
    let mut cell = [0u64; 2];
    let opts = write_cons(&mut cell, Term::atom(Atom::INFO), Term::NIL).unwrap();
    assert_eq!(
        bif_demonitor_2(&[Term::small_int(42), opts], &mut ctx),
        Ok(Term::atom(Atom::TRUE))
    );
}

#[test]
fn demonitor2_info_option_returns_false_when_not_active() {
    let f = Arc::new(FailingDemonitorFacility);
    let mut ctx = ProcessContext::new();
    ctx.set_pid(Some(1));
    ctx.set_supervision_facility(Some(f));
    let mut cell = [0u64; 2];
    let opts = write_cons(&mut cell, Term::atom(Atom::INFO), Term::NIL).unwrap();
    assert_eq!(
        bif_demonitor_2(&[Term::small_int(42), opts], &mut ctx),
        Ok(Term::atom(Atom::FALSE))
    );
}

#[test]
fn demonitor2_flush_and_info_combined() {
    let (_, mut ctx) = sup_ctx(0, 1, true);
    let mut cell2 = [0u64; 2];
    let tail = write_cons(&mut cell2, Term::atom(Atom::INFO), Term::NIL).unwrap();
    let mut cell1 = [0u64; 2];
    let opts = write_cons(&mut cell1, Term::atom(Atom::FLUSH), tail).unwrap();
    assert_eq!(
        bif_demonitor_2(&[Term::small_int(42), opts], &mut ctx),
        Ok(Term::atom(Atom::TRUE))
    );
}

#[test]
fn demonitor2_badarg_invalid_option() {
    let (_, mut ctx) = sup_ctx(0, 1, true);
    let mut cell = [0u64; 2];
    let opts = write_cons(&mut cell, Term::atom(Atom::OK), Term::NIL).unwrap();
    assert_eq!(
        bif_demonitor_2(&[Term::small_int(42), opts], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn demonitor2_badarg_negative_ref() {
    let (_, mut ctx) = sup_ctx(0, 1, true);
    assert_eq!(
        bif_demonitor_2(&[Term::small_int(-1), Term::NIL], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn demonitor2_badarg_non_integer_ref() {
    let (_, mut ctx) = sup_ctx(0, 1, true);
    assert_eq!(
        bif_demonitor_2(&[Term::atom(Atom::OK), Term::NIL], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn demonitor2_badarg_wrong_arity() {
    let (_, mut ctx) = sup_ctx(0, 1, true);
    assert_eq!(
        bif_demonitor_2(&[Term::small_int(42)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn demonitor2_badarg_no_facility() {
    let mut ctx = ProcessContext::new();
    ctx.set_pid(Some(1));
    assert_eq!(
        bif_demonitor_2(&[Term::small_int(42), Term::NIL], &mut ctx),
        Err(badarg())
    );
}

// ---- Mock facilities ----

struct MockSupervisionFacility {
    next_reference: u64,
    target_alive: bool,
    records: Mutex<Vec<SupervisionRecord>>,
}

impl MockSupervisionFacility {
    fn new(next_reference: u64, target_alive: bool) -> Self {
        Self {
            next_reference,
            target_alive,
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
    fn monitor(
        &self,
        caller_pid: u64,
        target_pid: u64,
    ) -> Result<MonitorResult, SupervisionError> {
        if !self.target_alive {
            return Err(SupervisionError::NoProc);
        }
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
        self.records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(SupervisionRecord::ExitSignal {
                caller_pid,
                target_pid,
                reason,
            });
        Ok(())
    }
}

struct FailingDemonitorFacility;

impl SupervisionFacility for FailingDemonitorFacility {
    fn monitor(&self, _: u64, _: u64) -> Result<MonitorResult, SupervisionError> {
        Err(SupervisionError::NoProc)
    }

    fn demonitor(&self, _: u64, _: u64) -> Result<(), SupervisionError> {
        Err(SupervisionError::NoProc)
    }

    fn exit_signal(&self, _: u64, _: u64, _: ExitReason) -> Result<(), SupervisionError> {
        Err(SupervisionError::NoProc)
    }
}
