use super::*;
use crate::atom::{Atom, AtomTable};
use crate::native::spawn::{SpawnError, SpawnFacility, SpawnRecord};
use crate::native::supervision::{
    MonitorResult, SupervisionError, SupervisionFacility, SupervisionRecord,
};
use crate::native::{BifRegistryImpl, ProcessContext};
use crate::process::ExitReason;
use crate::term::Term;
use crate::term::binary;
use crate::term::boxed::{write_closure, write_tuple};
use std::sync::{Arc, Mutex};

fn context() -> ProcessContext {
    ProcessContext::new()
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}

// ---- erlang:element/2 ----

#[test]
fn element_returns_first_element() {
    let mut ctx = context();
    let elements = [
        Term::small_int(10),
        Term::small_int(20),
        Term::small_int(30),
    ];
    let mut heap = [0u64; 4];
    let tuple = write_tuple(&mut heap, &elements).expect("tuple");
    assert_eq!(
        bif_element(&[Term::small_int(1), tuple], &mut ctx),
        Ok(Term::small_int(10))
    );
}

#[test]
fn element_returns_last_element() {
    let mut ctx = context();
    let elements = [
        Term::small_int(10),
        Term::small_int(20),
        Term::small_int(30),
    ];
    let mut heap = [0u64; 4];
    let tuple = write_tuple(&mut heap, &elements).expect("tuple");
    assert_eq!(
        bif_element(&[Term::small_int(3), tuple], &mut ctx),
        Ok(Term::small_int(30))
    );
}

#[test]
fn element_badarg_index_zero() {
    let mut ctx = context();
    let mut heap = [0u64; 2];
    let tuple = write_tuple(&mut heap, &[Term::small_int(1)]).expect("tuple");
    assert_eq!(
        bif_element(&[Term::small_int(0), tuple], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn element_badarg_index_out_of_range() {
    let mut ctx = context();
    let mut heap = [0u64; 2];
    let tuple = write_tuple(&mut heap, &[Term::small_int(1)]).expect("tuple");
    assert_eq!(
        bif_element(&[Term::small_int(2), tuple], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn element_badarg_negative_index() {
    let mut ctx = context();
    let mut heap = [0u64; 2];
    let tuple = write_tuple(&mut heap, &[Term::small_int(1)]).expect("tuple");
    assert_eq!(
        bif_element(&[Term::small_int(-1), tuple], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn element_badarg_non_tuple() {
    let mut ctx = context();
    assert_eq!(
        bif_element(&[Term::small_int(1), Term::small_int(42)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn element_badarg_non_integer_index() {
    let mut ctx = context();
    let mut heap = [0u64; 2];
    let tuple = write_tuple(&mut heap, &[Term::small_int(1)]).expect("tuple");
    assert_eq!(
        bif_element(&[Term::atom(Atom::OK), tuple], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn element_badarg_wrong_arity() {
    let mut ctx = context();
    assert_eq!(bif_element(&[Term::small_int(1)], &mut ctx), Err(badarg()));
}

// ---- erlang:send/2 ----

#[test]
fn send_returns_message() {
    let mut ctx = context();
    let message = Term::atom(Atom::OK);
    assert_eq!(bif_send(&[Term::pid(1), message], &mut ctx), Ok(message));
}

#[test]
fn send_badarg_non_pid() {
    let mut ctx = context();
    assert_eq!(
        bif_send(&[Term::small_int(1), Term::atom(Atom::OK)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn send_badarg_wrong_arity() {
    let mut ctx = context();
    assert_eq!(bif_send(&[Term::pid(1)], &mut ctx), Err(badarg()));
}

// ---- erlang:tuple_size/1 ----

#[test]
fn tuple_size_returns_arity() {
    let mut ctx = context();
    let mut heap = [0u64; 4];
    let tuple = write_tuple(
        &mut heap,
        &[Term::small_int(1), Term::small_int(2), Term::small_int(3)],
    )
    .expect("tuple");
    assert_eq!(bif_tuple_size(&[tuple], &mut ctx), Ok(Term::small_int(3)));
}

#[test]
fn tuple_size_empty_tuple() {
    let mut ctx = context();
    let mut heap = [0u64; 1];
    let tuple = write_tuple(&mut heap, &[]).expect("empty tuple");
    assert_eq!(bif_tuple_size(&[tuple], &mut ctx), Ok(Term::small_int(0)));
}

#[test]
fn tuple_size_badarg_non_tuple() {
    let mut ctx = context();
    assert_eq!(
        bif_tuple_size(&[Term::small_int(42)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn tuple_size_badarg_wrong_arity() {
    let mut ctx = context();
    assert_eq!(bif_tuple_size(&[], &mut ctx), Err(badarg()));
}

// ---- erlang:make_ref/0 ----

#[test]
fn make_ref_returns_small_int() {
    let mut ctx = context();
    let result = bif_make_ref(&[], &mut ctx).expect("make_ref");
    assert!(result.as_small_int().is_some());
}

#[test]
fn make_ref_returns_unique_values() {
    let mut ctx = context();
    let ref1 = bif_make_ref(&[], &mut ctx).expect("make_ref 1");
    let ref2 = bif_make_ref(&[], &mut ctx).expect("make_ref 2");
    assert_ne!(ref1, ref2);
}

#[test]
fn make_ref_badarg_wrong_arity() {
    let mut ctx = context();
    assert_eq!(bif_make_ref(&[Term::small_int(1)], &mut ctx), Err(badarg()));
}

// ---- erlang:is_process_alive/1 ----

#[test]
fn is_process_alive_self_is_true() {
    let mut ctx = ProcessContext::new();
    ctx.set_pid(Some(5));
    assert_eq!(
        bif_is_process_alive(&[Term::pid(5)], &mut ctx),
        Ok(Term::atom(Atom::TRUE))
    );
}

#[test]
fn is_process_alive_false_without_facility() {
    let mut ctx = ProcessContext::new();
    ctx.set_pid(Some(1));
    assert_eq!(
        bif_is_process_alive(&[Term::pid(99)], &mut ctx),
        Ok(Term::atom(Atom::FALSE))
    );
}

#[test]
fn is_process_alive_true_with_facility() {
    let (_, mut ctx) = sup_ctx(100, 1, true);
    assert_eq!(
        bif_is_process_alive(&[Term::pid(2)], &mut ctx),
        Ok(Term::atom(Atom::TRUE))
    );
}

#[test]
fn is_process_alive_false_dead_process() {
    let (_, mut ctx) = sup_ctx(100, 1, false);
    assert_eq!(
        bif_is_process_alive(&[Term::pid(2)], &mut ctx),
        Ok(Term::atom(Atom::FALSE))
    );
}

#[test]
fn is_process_alive_badarg_non_pid() {
    let mut ctx = ProcessContext::new();
    ctx.set_pid(Some(1));
    assert_eq!(
        bif_is_process_alive(&[Term::small_int(1)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn is_process_alive_badarg_wrong_arity() {
    let mut ctx = context();
    assert_eq!(bif_is_process_alive(&[], &mut ctx), Err(badarg()));
}

// ---- erlang:spawn/1 ----

#[test]
fn spawn_1_with_zero_arity_closure() {
    let (f, mut ctx) = spawn_ctx(42, 1);
    let mut heap = [0u64; 7];
    let fun = write_closure(&mut heap, Atom::OK, 0, 0, 1, 0, &[]).expect("closure");
    assert_eq!(bif_spawn_1(&[fun], &mut ctx), Ok(Term::pid(42)));
    let records = f.lambda_records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].caller_pid, 1);
    assert_eq!(records[0].link_to, None);
}

#[test]
fn spawn_1_badarg_non_zero_arity() {
    let (_, mut ctx) = spawn_ctx(42, 1);
    let mut heap = [0u64; 7];
    let fun = write_closure(&mut heap, Atom::OK, 0, 2, 1, 0, &[]).expect("closure");
    assert_eq!(bif_spawn_1(&[fun], &mut ctx), Err(badarg()));
}

#[test]
fn spawn_1_badarg_with_captures() {
    let (_, mut ctx) = spawn_ctx(42, 1);
    let free_vars = [Term::small_int(1)];
    let mut heap = [0u64; 8];
    let fun = write_closure(&mut heap, Atom::OK, 0, 0, 1, 0, &free_vars).expect("closure");
    assert_eq!(bif_spawn_1(&[fun], &mut ctx), Err(badarg()));
}

#[test]
fn spawn_1_badarg_non_closure() {
    let (_, mut ctx) = spawn_ctx(42, 1);
    assert_eq!(bif_spawn_1(&[Term::small_int(42)], &mut ctx), Err(badarg()));
}

#[test]
fn spawn_1_badarg_no_facility() {
    let mut ctx = ProcessContext::new();
    ctx.set_pid(Some(1));
    let mut heap = [0u64; 7];
    let fun = write_closure(&mut heap, Atom::OK, 0, 0, 1, 0, &[]).expect("closure");
    assert_eq!(bif_spawn_1(&[fun], &mut ctx), Err(badarg()));
}

#[test]
fn spawn_1_badarg_wrong_arity() {
    let mut ctx = context();
    assert_eq!(bif_spawn_1(&[], &mut ctx), Err(badarg()));
}

// ---- erlang:spawn_link/1 ----

#[test]
fn spawn_link_1_sets_link_to_parent() {
    let (f, mut ctx) = spawn_ctx(42, 3);
    let mut heap = [0u64; 7];
    let fun = write_closure(&mut heap, Atom::OK, 0, 0, 1, 0, &[]).expect("closure");
    assert_eq!(bif_spawn_link_1(&[fun], &mut ctx), Ok(Term::pid(42)));
    let records = f.lambda_records();
    assert_eq!(records[0].caller_pid, 3);
    assert_eq!(records[0].link_to, Some(3));
}

#[test]
fn spawn_link_1_badarg_without_pid() {
    let f: Arc<dyn SpawnFacility> = Arc::new(MockSpawnFacility::new(42));
    let mut ctx = ProcessContext::new();
    ctx.set_spawn_facility(Some(f));
    let mut heap = [0u64; 7];
    let fun = write_closure(&mut heap, Atom::OK, 0, 0, 1, 0, &[]).expect("closure");
    assert_eq!(bif_spawn_link_1(&[fun], &mut ctx), Err(badarg()));
}

// ---- erlang:byte_size/1 and erlang:iolist_size/1 ----

#[test]
fn byte_size_returns_binary_length() {
    let mut ctx = context();
    let mut heap = [0u64; 3];
    let bin = binary::write_binary(&mut heap, b"hello").expect("binary");
    assert_eq!(bif_byte_size(&[bin], &mut ctx), Ok(Term::small_int(5)));
}

#[test]
fn byte_size_rejects_non_binary() {
    let mut ctx = context();
    assert_eq!(
        bif_byte_size(&[Term::small_int(5)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn iolist_size_returns_binary_length() {
    let mut ctx = context();
    let mut heap = [0u64; 3];
    let bin = binary::write_binary(&mut heap, b"hello").expect("binary");
    assert_eq!(bif_iolist_size(&[bin], &mut ctx), Ok(Term::small_int(5)));
}

#[test]
fn iolist_size_rejects_complex_iolist_stub() {
    let mut ctx = context();
    let mut cell = [0u64; 2];
    let list =
        crate::term::boxed::write_cons(&mut cell, Term::small_int(65), Term::NIL).expect("list");
    assert_eq!(bif_iolist_size(&[list], &mut ctx), Err(badarg()));
}

// ---- Registration ----

#[test]
fn register_gate3_bifs_registers_all() {
    let at = AtomTable::new();
    let reg = BifRegistryImpl::new();
    register_gate3_bifs(&reg, &at).expect("gate 3 registration");
    let erlang = at.intern("erlang");
    for (name, arity) in [
        ("element", 2),
        ("send", 2),
        ("tuple_size", 1),
        ("make_ref", 0),
        ("is_process_alive", 1),
        ("spawn", 1),
        ("spawn_link", 1),
        // Type conversion BIFs (R1)
        ("atom_to_binary", 2),
        ("binary_to_existing_atom", 1),
        ("binary_to_list", 1),
        ("list_to_binary", 1),
        ("map_get", 2),
        // Process registry BIFs (R2)
        ("register", 2),
        ("unregister", 1),
        ("whereis", 1),
        // demonitor/2 (R3)
        ("demonitor", 2),
        // Gleam stdlib support (B-033)
        ("byte_size", 1),
        ("iolist_size", 1),
        // Additional erlang BIFs (B-037)
        ("round", 1),
        ("trunc", 1),
        ("is_bitstring", 1),
        ("is_map_key", 2),
        ("map_size", 1),
        ("binary_part", 3),
        ("bit_size", 1),
        ("-", 1),
    ] {
        assert!(
            reg.lookup(erlang, at.intern(name), arity).is_some(),
            "missing erlang:{name}/{arity}"
        );
    }
}

#[test]
fn register_gate3_bifs_fails_twice() {
    let at = AtomTable::new();
    let reg = BifRegistryImpl::new();
    register_gate3_bifs(&reg, &at).expect("first");
    assert!(register_gate3_bifs(&reg, &at).is_err());
}

#[test]
fn all_three_gates_coexist() {
    let at = AtomTable::new();
    let reg = BifRegistryImpl::new();
    crate::native::bifs::register_gate1_bifs(&reg, &at).expect("gate 1");
    crate::native::process_bifs::register_gate2_bifs(&reg, &at).expect("gate 2");
    register_gate3_bifs(&reg, &at).expect("gate 3");
    let erlang = at.intern("erlang");
    // Gate 1
    assert!(reg.lookup(erlang, at.intern("+"), 2).is_some());
    // Gate 2
    assert!(reg.lookup(erlang, at.intern("self"), 0).is_some());
    // Gate 3
    assert!(reg.lookup(erlang, at.intern("element"), 2).is_some());
    assert!(reg.lookup(erlang, at.intern("make_ref"), 0).is_some());
}

// ---- Helpers ----

fn spawn_ctx(next_pid: u64, caller_pid: u64) -> (Arc<MockSpawnFacility>, ProcessContext) {
    let f = Arc::new(MockSpawnFacility::new(next_pid));
    let mut ctx = ProcessContext::new();
    ctx.set_pid(Some(caller_pid));
    ctx.set_spawn_facility(Some(f.clone()));
    (f, ctx)
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

// ---- Mock spawn facility ----

struct LambdaSpawnRecord {
    caller_pid: u64,
    link_to: Option<u64>,
}

struct MockSpawnFacility {
    next_pid: u64,
    records: Mutex<Vec<SpawnRecord>>,
    lambda_records: Mutex<Vec<LambdaSpawnRecord>>,
}

impl MockSpawnFacility {
    fn new(next_pid: u64) -> Self {
        Self {
            next_pid,
            records: Mutex::new(Vec::new()),
            lambda_records: Mutex::new(Vec::new()),
        }
    }
    fn lambda_records(&self) -> Vec<LambdaSpawnRecord> {
        self.lambda_records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
            .collect()
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

    fn spawn_lambda(
        &self,
        caller_pid: u64,
        _module: Atom,
        _lambda_index: u32,
        link_to: Option<u64>,
    ) -> Result<u64, SpawnError> {
        self.lambda_records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(LambdaSpawnRecord {
                caller_pid,
                link_to,
            });
        Ok(self.next_pid)
    }
}

// ---- Mock supervision facility ----

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
}

impl SupervisionFacility for MockSupervisionFacility {
    fn monitor(&self, caller_pid: u64, target_pid: u64) -> Result<MonitorResult, SupervisionError> {
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
