use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::atom::Atom;
use crate::native::registry::{RegistryError, RegistryFacility};
use crate::native::ProcessContext;
use crate::term::Term;

use super::*;

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}

fn reg_ctx(caller_pid: u64) -> (Arc<MockRegistryFacility>, ProcessContext) {
    let f = Arc::new(MockRegistryFacility::new());
    let mut ctx = ProcessContext::new();
    ctx.set_pid(Some(caller_pid));
    ctx.set_registry_facility(Some(f.clone()));
    (f, ctx)
}

// ---- erlang:register/2 ----

#[test]
fn register_associates_name_with_pid() {
    let (f, mut ctx) = reg_ctx(1);
    assert_eq!(
        bif_register(&[Term::atom(Atom::OK), Term::pid(1)], &mut ctx),
        Ok(Term::atom(Atom::TRUE))
    );
    assert_eq!(f.whereis(Atom::OK), Some(1));
}

#[test]
fn register_badarg_name_taken() {
    let (f, mut ctx) = reg_ctx(1);
    f.register(Atom::OK, 99).unwrap();
    assert_eq!(
        bif_register(&[Term::atom(Atom::OK), Term::pid(1)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn register_badarg_non_atom_name() {
    let (_, mut ctx) = reg_ctx(1);
    assert_eq!(
        bif_register(&[Term::small_int(1), Term::pid(1)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn register_badarg_non_pid() {
    let (_, mut ctx) = reg_ctx(1);
    assert_eq!(
        bif_register(&[Term::atom(Atom::OK), Term::small_int(1)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn register_badarg_no_facility() {
    let mut ctx = ProcessContext::new();
    assert_eq!(
        bif_register(&[Term::atom(Atom::OK), Term::pid(1)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn register_badarg_wrong_arity() {
    let (_, mut ctx) = reg_ctx(1);
    assert_eq!(bif_register(&[Term::atom(Atom::OK)], &mut ctx), Err(badarg()));
}

// ---- erlang:unregister/1 ----

#[test]
fn unregister_removes_registration() {
    let (f, mut ctx) = reg_ctx(1);
    f.register(Atom::OK, 1).unwrap();
    assert_eq!(
        bif_unregister(&[Term::atom(Atom::OK)], &mut ctx),
        Ok(Term::atom(Atom::TRUE))
    );
    assert_eq!(f.whereis(Atom::OK), None);
}

#[test]
fn unregister_badarg_not_registered() {
    let (_, mut ctx) = reg_ctx(1);
    assert_eq!(
        bif_unregister(&[Term::atom(Atom::OK)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn unregister_badarg_non_atom() {
    let (_, mut ctx) = reg_ctx(1);
    assert_eq!(
        bif_unregister(&[Term::small_int(1)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn unregister_badarg_no_facility() {
    let mut ctx = ProcessContext::new();
    assert_eq!(
        bif_unregister(&[Term::atom(Atom::OK)], &mut ctx),
        Err(badarg())
    );
}

// ---- erlang:whereis/1 ----

#[test]
fn whereis_returns_pid() {
    let (f, mut ctx) = reg_ctx(1);
    f.register(Atom::OK, 42).unwrap();
    assert_eq!(
        bif_whereis(&[Term::atom(Atom::OK)], &mut ctx),
        Ok(Term::pid(42))
    );
}

#[test]
fn whereis_returns_undefined_when_not_registered() {
    let (_, mut ctx) = reg_ctx(1);
    assert_eq!(
        bif_whereis(&[Term::atom(Atom::OK)], &mut ctx),
        Ok(Term::atom(Atom::UNDEFINED))
    );
}

#[test]
fn whereis_badarg_non_atom() {
    let (_, mut ctx) = reg_ctx(1);
    assert_eq!(
        bif_whereis(&[Term::small_int(1)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn whereis_badarg_no_facility() {
    let mut ctx = ProcessContext::new();
    assert_eq!(
        bif_whereis(&[Term::atom(Atom::OK)], &mut ctx),
        Err(badarg())
    );
}

// ---- Mock registry facility ----

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
        let pid = entries.remove(&name.index()).ok_or(RegistryError::NotRegistered)?;
        pids.remove(&pid);
        Ok(())
    }

    fn whereis(&self, name: Atom) -> Option<u64> {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.get(&name.index()).copied()
    }
}
