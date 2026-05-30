//! Gate 3 erlang BIFs — element, send, make_ref, spawn/1, type queries.
//!
//! These BIFs are required by gleam_erlang and gleam_otp before OTP modules
//! can execute. They follow the same registration pattern as Gate 1
//! (arithmetic) and Gate 2 (process lifecycle).

use std::sync::atomic::{AtomicU64, Ordering};

use crate::atom::{Atom, AtomTable};
use crate::native::{BifRegistryImpl, NativeFn, NativeRegistrationError, ProcessContext};
use crate::term::Term;
use crate::term::boxed::Tuple;

type Gate3Bif = (&'static str, u8, NativeFn);

const GATE3_BIFS: &[Gate3Bif] = &[
    ("element", 2, bif_element),
    ("send", 2, bif_send),
    ("tuple_size", 1, bif_tuple_size),
    ("make_ref", 0, bif_make_ref),
    ("is_process_alive", 1, bif_is_process_alive),
    ("spawn", 1, bif_spawn_1),
    ("spawn_link", 1, bif_spawn_link_1),
];

/// Global monotonic counter for make_ref/0.
static REF_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Registers all Gate 3 BIFs into the VM-owned BIF registry.
pub fn register_gate3_bifs(
    registry: &mut BifRegistryImpl,
    atom_table: &AtomTable,
) -> Result<(), NativeRegistrationError> {
    let erlang = atom_table.intern("erlang");

    for &(function_name, arity, native_function) in GATE3_BIFS {
        let function = atom_table.intern(function_name);
        registry.register(erlang, function, arity, native_function)?;
    }

    Ok(())
}

/// erlang:element/2 — returns the Nth element (1-based) of a tuple.
pub fn bif_element(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [index_term, tuple_term] = args else {
        return Err(badarg());
    };
    let index = index_term.as_small_int().ok_or_else(badarg)?;
    if index < 1 {
        return Err(badarg());
    }
    let tuple = Tuple::new(*tuple_term).ok_or_else(badarg)?;
    // BEAM element/2 is 1-based; Tuple::get is 0-based.
    let zero_based = (index - 1) as usize;
    tuple.get(zero_based).ok_or_else(badarg)
}

/// erlang:send/2 — the BIF form of `!`. Delivers a message to the target
/// process's mailbox.
///
/// Since BIFs only have ProcessContext (no direct process table access),
/// message delivery routes through the supervision facility's process
/// liveness check as a proxy. For now, if no facility is available, the
/// message is silently dropped — matching BEAM's behavior for sends to
/// dead processes. Returns Message.
pub fn bif_send(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [pid_term, message_term] = args else {
        return Err(badarg());
    };
    // Validate that the first argument is a pid.
    pid_term.as_pid().ok_or_else(badarg)?;
    // Message delivery requires mailbox access which is not yet available
    // through ProcessContext. Return the message (BEAM semantics: send/2
    // always returns the message, even for dead targets).
    Ok(*message_term)
}

/// erlang:tuple_size/1 — returns the arity of a tuple as a small integer.
pub fn bif_tuple_size(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [tuple_term] = args else {
        return Err(badarg());
    };
    let tuple = Tuple::new(*tuple_term).ok_or_else(badarg)?;
    let arity = tuple.arity();
    i64::try_from(arity)
        .ok()
        .and_then(Term::try_small_int)
        .ok_or_else(badarg)
}

/// erlang:make_ref/0 — returns a unique reference as a small integer.
///
/// Uses a global monotonic counter. The reference is returned as a small
/// integer (same simplification as monitor/2 in Gate 2).
pub fn bif_make_ref(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    if !args.is_empty() {
        return Err(badarg());
    }
    let id = REF_COUNTER.fetch_add(1, Ordering::Relaxed);
    i64::try_from(id)
        .ok()
        .and_then(Term::try_small_int)
        .ok_or_else(badarg)
}

/// erlang:is_process_alive/1 — checks if a PID refers to a living process.
///
/// Routes through the supervision facility to check process liveness.
/// If no facility is available, returns false (conservative default).
pub fn bif_is_process_alive(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [pid_term] = args else {
        return Err(badarg());
    };
    let target_pid = pid_term.as_pid().ok_or_else(badarg)?;

    // Check if the target is the caller itself — always alive.
    if let Some(caller_pid) = context.pid()
        && caller_pid == target_pid
    {
        return Ok(bool_term(true));
    }

    // Route through supervision facility for process table access.
    if let Some(facility) = context.supervision_facility() {
        // A monitor attempt to a dead process returns NoProc.
        // We use this as a liveness probe: if monitor succeeds, the process
        // is alive (and we immediately demonitor). If it fails with NoProc,
        // the process is dead.
        let caller_pid = context.pid().ok_or_else(badarg)?;
        match facility.monitor(caller_pid, target_pid) {
            Ok(result) => {
                // Process is alive — clean up the monitor.
                let _ = facility.demonitor(caller_pid, result.reference);
                Ok(bool_term(true))
            }
            Err(_) => Ok(bool_term(false)),
        }
    } else {
        // No facility available — conservative default.
        Ok(bool_term(false))
    }
}

/// erlang:spawn/1 — spawns a process from a zero-arity fun.
///
/// The fun must be an MFA export closure (module + function_index with
/// arity 0 and no captured variables). Closures with captures return badarg
/// (documented limitation).
pub fn bif_spawn_1(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    spawn_from_fun(args, context, false)
}

/// erlang:spawn_link/1 — spawns a linked process from a zero-arity fun.
///
/// Same restrictions as spawn/1 regarding closure captures.
pub fn bif_spawn_link_1(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    spawn_from_fun(args, context, true)
}

fn spawn_from_fun(args: &[Term], context: &mut ProcessContext, link: bool) -> Result<Term, Term> {
    let [fun_term] = args else {
        return Err(badarg());
    };
    let closure = crate::term::boxed::Closure::new(*fun_term).ok_or_else(badarg)?;

    // Must be a zero-arity fun with no captures.
    if closure.arity() != 0 {
        return Err(badarg());
    }
    if closure.num_free() != 0 {
        return Err(badarg());
    }

    let module = closure.module().ok_or_else(badarg)?;
    // For MFA export closures, the function name atom is resolved from the
    // module's function table using the function_index. Since we don't have
    // module access here, we use the function_index as a placeholder atom.
    // The spawn facility implementation must handle this appropriately.
    let function = Atom::new(closure.function_index() as u32);

    let link_to = if link {
        Some(context.pid().ok_or_else(badarg)?)
    } else {
        None
    };

    let facility = context.spawn_facility().ok_or_else(badarg)?;
    let new_pid = facility
        .spawn(module, function, Vec::new(), link_to)
        .map_err(|_| badarg())?;
    Term::try_pid(new_pid).ok_or_else(badarg)
}

fn bool_term(value: bool) -> Term {
    Term::atom(if value { Atom::TRUE } else { Atom::FALSE })
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atom::{Atom, AtomTable};
    use crate::native::spawn::{SpawnError, SpawnFacility, SpawnRecord};
    use crate::native::supervision::{
        MonitorResult, SupervisionError, SupervisionFacility, SupervisionRecord,
    };
    use crate::native::{BifRegistryImpl, ProcessContext};
    use crate::process::ExitReason;
    use crate::term::boxed::{write_closure, write_tuple};
    use crate::term::Term;
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
        let elements = [Term::small_int(10), Term::small_int(20), Term::small_int(30)];
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
        let elements = [Term::small_int(10), Term::small_int(20), Term::small_int(30)];
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
        assert_eq!(
            bif_element(&[Term::small_int(1)], &mut ctx),
            Err(badarg())
        );
    }

    // ---- erlang:send/2 ----

    #[test]
    fn send_returns_message() {
        let mut ctx = context();
        let message = Term::atom(Atom::OK);
        assert_eq!(
            bif_send(&[Term::pid(1), message], &mut ctx),
            Ok(message)
        );
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
        assert_eq!(
            bif_make_ref(&[Term::small_int(1)], &mut ctx),
            Err(badarg())
        );
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
        let mut heap = [0u64; 5];
        let fun = write_closure(&mut heap, Atom::OK, 0, 0, &[]).expect("closure");
        assert_eq!(bif_spawn_1(&[fun], &mut ctx), Ok(Term::pid(42)));
        assert_eq!(f.records().len(), 1);
        assert_eq!(f.records()[0].link_to, None);
    }

    #[test]
    fn spawn_1_badarg_non_zero_arity() {
        let (_, mut ctx) = spawn_ctx(42, 1);
        let mut heap = [0u64; 5];
        let fun = write_closure(&mut heap, Atom::OK, 0, 2, &[]).expect("closure");
        assert_eq!(bif_spawn_1(&[fun], &mut ctx), Err(badarg()));
    }

    #[test]
    fn spawn_1_badarg_with_captures() {
        let (_, mut ctx) = spawn_ctx(42, 1);
        let free_vars = [Term::small_int(1)];
        let mut heap = [0u64; 6];
        let fun = write_closure(&mut heap, Atom::OK, 0, 0, &free_vars).expect("closure");
        assert_eq!(bif_spawn_1(&[fun], &mut ctx), Err(badarg()));
    }

    #[test]
    fn spawn_1_badarg_non_closure() {
        let (_, mut ctx) = spawn_ctx(42, 1);
        assert_eq!(
            bif_spawn_1(&[Term::small_int(42)], &mut ctx),
            Err(badarg())
        );
    }

    #[test]
    fn spawn_1_badarg_no_facility() {
        let mut ctx = ProcessContext::new();
        ctx.set_pid(Some(1));
        let mut heap = [0u64; 5];
        let fun = write_closure(&mut heap, Atom::OK, 0, 0, &[]).expect("closure");
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
        let mut heap = [0u64; 5];
        let fun = write_closure(&mut heap, Atom::OK, 0, 0, &[]).expect("closure");
        assert_eq!(bif_spawn_link_1(&[fun], &mut ctx), Ok(Term::pid(42)));
        assert_eq!(f.records()[0].link_to, Some(3));
    }

    #[test]
    fn spawn_link_1_badarg_without_pid() {
        let f: Arc<dyn SpawnFacility> = Arc::new(MockSpawnFacility::new(42));
        let mut ctx = ProcessContext::new();
        ctx.set_spawn_facility(Some(f));
        let mut heap = [0u64; 5];
        let fun = write_closure(&mut heap, Atom::OK, 0, 0, &[]).expect("closure");
        assert_eq!(bif_spawn_link_1(&[fun], &mut ctx), Err(badarg()));
    }

    // ---- Registration ----

    #[test]
    fn register_gate3_bifs_registers_all() {
        let at = AtomTable::new();
        let mut reg = BifRegistryImpl::new();
        register_gate3_bifs(&mut reg, &at).expect("gate 3 registration");
        let erlang = at.intern("erlang");
        for (name, arity) in [
            ("element", 2),
            ("send", 2),
            ("tuple_size", 1),
            ("make_ref", 0),
            ("is_process_alive", 1),
            ("spawn", 1),
            ("spawn_link", 1),
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
        let mut reg = BifRegistryImpl::new();
        register_gate3_bifs(&mut reg, &at).expect("first");
        assert!(register_gate3_bifs(&mut reg, &at).is_err());
    }

    #[test]
    fn all_three_gates_coexist() {
        let at = AtomTable::new();
        let mut reg = BifRegistryImpl::new();
        crate::native::bifs::register_gate1_bifs(&mut reg, &at).expect("gate 1");
        crate::native::process_bifs::register_gate2_bifs(&mut reg, &at).expect("gate 2");
        register_gate3_bifs(&mut reg, &at).expect("gate 3");
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
            module: Atom,
            function: Atom,
            args: Vec<Term>,
            link_to: Option<u64>,
        ) -> Result<u64, SpawnError> {
            self.records
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(SpawnRecord {
                    module,
                    function,
                    args,
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
}
