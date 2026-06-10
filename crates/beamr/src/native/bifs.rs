//! Built-in function implementations.
//!
//! The set of BIFs is demand-driven: only functions that appear in
//! the loader's unresolved-import report are implemented. Gate 1 currently
//! provides the minimum arithmetic, comparison, and utility BIFs required by
//! unresolved imports.

use std::cmp::Ordering;
use std::time::Duration;

use crate::atom::{Atom, AtomTable};
use crate::native::{
    BifRegistryImpl, Capability, ExceptionClass, NativeFn, NativeRegistrationError, ProcessContext,
};
use crate::term::Term;
use crate::term::compare;
use crate::timer::TimerRef;

type Gate1Bif = (&'static str, u8, Capability, NativeFn);

const GATE1_BIFS: &[Gate1Bif] = &[
    ("+", 2, Capability::Pure, add),
    ("-", 2, Capability::Pure, subtract),
    ("*", 2, Capability::Pure, multiply),
    ("div", 2, Capability::Pure, div),
    ("rem", 2, Capability::Pure, rem),
    ("<", 2, Capability::Pure, less_than),
    (">", 2, Capability::Pure, greater_than),
    ("=<", 2, Capability::Pure, less_equal),
    (">=", 2, Capability::Pure, greater_equal),
    ("==", 2, Capability::Pure, numeric_equal),
    ("/=", 2, Capability::Pure, numeric_not_equal),
    ("=:=", 2, Capability::Pure, exact_equal),
    ("=/=", 2, Capability::Pure, exact_not_equal),
    ("error", 1, Capability::Pure, error),
    ("throw", 1, Capability::Pure, throw),
    ("display", 1, Capability::ExternalIo, display),
    ("get_module_info", 1, Capability::Pure, get_module_info_1),
    ("get_module_info", 2, Capability::Pure, get_module_info_2),
    ("send_after", 3, Capability::Clock, send_after),
    ("start_timer", 3, Capability::Clock, start_timer),
    ("cancel_timer", 1, Capability::Clock, cancel_timer),
];

/// Registers all Gate 1 BIFs into the VM-owned BIF registry.
pub fn register_gate1_bifs(
    registry: &BifRegistryImpl,
    atom_table: &AtomTable,
) -> Result<(), NativeRegistrationError> {
    let erlang = atom_table.intern("erlang");

    for &(function_name, arity, capability, native_function) in GATE1_BIFS {
        let function = atom_table.intern(function_name);
        registry.register(erlang, function, arity, native_function, capability)?;
    }

    crate::native::code_management_bifs::register_code_management_bifs(registry, atom_table)?;
    crate::native::dictionary_bifs::register_dictionary_bifs(registry, atom_table)?;
    crate::native::distribution_bifs::register_distribution_bifs(registry, atom_table)?;
    crate::native::etf_bifs::register_etf_bifs(registry, atom_table)?;
    crate::native::ets_bifs::register_ets_bifs(registry, atom_table)?;
    crate::native::exception_bifs::register_exception_bifs(registry, atom_table)?;
    crate::native::file_bifs::register_file_bifs(registry, atom_table)?;
    crate::native::file_meta_bifs::register_file_meta_bifs(registry, atom_table)?;
    crate::native::inet_bifs::register_inet_bifs(registry, atom_table)?;

    crate::native::tcp_bifs::register_tcp_bifs(registry, atom_table)?;
    crate::native::udp_bifs::register_udp_bifs(registry, atom_table)?;
    crate::native::process_info_bifs::register_process_info_bifs(registry, atom_table)?;
    crate::native::system_info_bifs::register_system_info_bifs(registry, atom_table)?;
    crate::distribution::pg::register_pg_bifs(registry, atom_table)?;

    Ok(())
}

/// erlang:+/2 for small integers.
pub fn add(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    arithmetic(args, i64::checked_add)
}

/// erlang:-/2 for small integers.
pub fn subtract(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    arithmetic(args, i64::checked_sub)
}

/// erlang:*/2 for small integers.
pub fn multiply(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    arithmetic(args, i64::checked_mul)
}

/// erlang:div/2 for small integers.
pub fn div(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    arithmetic(args, i64::checked_div)
}

/// erlang:rem/2 for small integers.
pub fn rem(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    arithmetic(args, i64::checked_rem)
}

/// erlang:</2 over the full BEAM term order.
///
/// BEAM: `<` is total over every term type and never raises — it routes
/// through the same `compare::cmp` order as the fused comparison opcode, so
/// `1 < a` is `true` (number < atom). See [`crate::term::compare::cmp`].
pub fn less_than(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let (left, right) = two_terms(args)?;
    let atom_table = context.atom_table().ok_or_else(badarg)?;
    Ok(bool_term(
        compare::cmp(left, right, atom_table) == Ordering::Less,
    ))
}

/// erlang:>/2 over the full BEAM term order.
pub fn greater_than(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let (left, right) = two_terms(args)?;
    let atom_table = context.atom_table().ok_or_else(badarg)?;
    Ok(bool_term(
        compare::cmp(left, right, atom_table) == Ordering::Greater,
    ))
}

/// erlang:=</2 over the full BEAM term order.
pub fn less_equal(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let (left, right) = two_terms(args)?;
    let atom_table = context.atom_table().ok_or_else(badarg)?;
    Ok(bool_term(
        compare::cmp(left, right, atom_table) != Ordering::Greater,
    ))
}

/// erlang:>=/2 over the full BEAM term order.
///
/// BEAM: `>=` is total over every term type and never raises — the inverse of
/// `<` under the same `compare::cmp` order. See [`crate::term::compare::cmp`].
pub fn greater_equal(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let (left, right) = two_terms(args)?;
    let atom_table = context.atom_table().ok_or_else(badarg)?;
    Ok(bool_term(
        compare::cmp(left, right, atom_table) != Ordering::Less,
    ))
}

/// erlang:==/2 numeric equality.
pub fn numeric_equal(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let (left, right) = two_terms(args)?;
    Ok(bool_term(compare::numeric_eq(left, right)))
}

/// erlang:/=/2 numeric inequality.
pub fn numeric_not_equal(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let (left, right) = two_terms(args)?;
    Ok(bool_term(!compare::numeric_eq(left, right)))
}

/// erlang:=:=/2 exact term equality.
pub fn exact_equal(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [left, right] = args else {
        return Err(badarg());
    };

    Ok(bool_term(left == right))
}

/// erlang:=/=/2 exact term inequality.
pub fn exact_not_equal(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [left, right] = args else {
        return Err(badarg());
    };

    Ok(bool_term(left != right))
}

/// erlang:error/1 exits with the supplied reason.
pub fn error(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [reason] = args else {
        return Err(badarg());
    };

    Err(*reason)
}

/// erlang:throw/1 raises a throw-class exception with the supplied reason.
pub fn throw(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [reason] = args else {
        return Err(badarg());
    };

    context.set_exception_class(ExceptionClass::Throw);
    Err(*reason)
}

/// erlang:display/1 prints Debug formatting and returns true.
pub fn display(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [term] = args else {
        return Err(badarg());
    };

    println!("{term:?}");
    Ok(bool_term(true))
}

/// erlang:get_module_info/1 returns loaded module metadata as a property list.
pub fn get_module_info_1(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [module] = args else {
        return Err(badarg());
    };
    let module = module.as_atom().ok_or_else(badarg)?;
    let source = module_source_atom(module, context)?;
    let atom_table = context.atom_table().ok_or_else(badarg)?;
    let source_key = atom_table.intern("source");
    let source_tuple = context.alloc_tuple(&[Term::atom(source_key), Term::atom(source)])?;

    context.alloc_cons(source_tuple, Term::NIL)
}

/// erlang:get_module_info/2 returns a single metadata value.
pub fn get_module_info_2(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [module, key] = args else {
        return Err(badarg());
    };
    let module = module.as_atom().ok_or_else(badarg)?;
    let key = key.as_atom().ok_or_else(badarg)?;
    let atom_table = context.atom_table().ok_or_else(badarg)?;
    let source_key = atom_table.intern("source");
    if key != source_key {
        return Ok(Term::NIL);
    }

    Ok(Term::atom(module_source_atom(module, context)?))
}

fn module_source_atom(module: Atom, context: &mut ProcessContext) -> Result<Atom, Term> {
    let facility = context.code_management_facility().ok_or_else(badarg)?;
    let Some(origin) = facility.module_origin(module) else {
        return Err(badarg());
    };
    let source = origin.source_atom_name();
    let atom_table = context.atom_table().ok_or_else(badarg)?;
    Ok(atom_table.intern(source))
}

/// erlang:send_after/3 schedules `Msg` to be delivered to `Pid` after `Time` ms.
pub fn send_after(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [delay, pid, message] = args else {
        return Err(badarg());
    };
    let delay = duration_from_term(*delay)?;
    let target_pid = pid.as_pid().ok_or_else(badarg)?;
    let reference = context
        .schedule_timer(delay, target_pid, *message)
        .ok_or_else(badarg)?;
    timer_ref_term(reference)
}

/// erlang:start_timer/3 schedules `{timeout, Ref, Msg}` delivery after `Time` ms.
pub fn start_timer(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [delay, pid, message] = args else {
        return Err(badarg());
    };
    let delay = duration_from_term(*delay)?;
    let target_pid = pid.as_pid().ok_or_else(badarg)?;
    let reference = context.reserve_timer_reference().ok_or_else(badarg)?;
    {
        let process = context.process_mut().ok_or_else(badarg)?;
        process.set_x_reg(2, *message);
    }
    context.ensure_heap_space(3)?;
    let message = context.process_mut().ok_or_else(badarg)?.x_reg(2);
    let payload = timeout_tuple_term_prereserved(context, reference, message)?;
    let reference = context
        .schedule_reserved_timer(reference, delay, target_pid, payload)
        .ok_or_else(badarg)?;
    timer_ref_term(reference)
}

/// erlang:cancel_timer/1 cancels a pending timer and returns remaining ms or false.
pub fn cancel_timer(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [reference] = args else {
        return Err(badarg());
    };
    let reference = reference
        .as_small_int()
        .and_then(|id| u64::try_from(id).ok())
        .map(TimerRef::from_id)
        .ok_or_else(badarg)?;
    match context.cancel_timer(reference) {
        Some(remaining) => i64::try_from(remaining.as_millis())
            .ok()
            .and_then(Term::try_small_int)
            .ok_or_else(badarg),
        None => Ok(Term::atom(Atom::FALSE)),
    }
}

fn arithmetic(args: &[Term], operation: fn(i64, i64) -> Option<i64>) -> Result<Term, Term> {
    let (left, right) = two_small_ints(args)?;
    let result = operation(left, right).ok_or_else(badarith)?;
    Term::try_small_int(result).ok_or_else(badarith)
}

/// Extracts exactly two operands for a total-order comparison BIF.
///
/// Comparison BIFs accept any two terms (BEAM total order); only a wrong
/// arity is an error, reported as `badarg`.
fn two_terms(args: &[Term]) -> Result<(Term, Term), Term> {
    let [left, right] = args else {
        return Err(badarg());
    };
    Ok((*left, *right))
}

fn two_small_ints(args: &[Term]) -> Result<(i64, i64), Term> {
    let [left, right] = args else {
        return Err(badarith());
    };

    match (left.as_small_int(), right.as_small_int()) {
        (Some(left), Some(right)) => Ok((left, right)),
        _ => Err(badarith()),
    }
}

fn duration_from_term(term: Term) -> Result<Duration, Term> {
    let milliseconds = term
        .as_small_int()
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(badarg)?;
    Ok(Duration::from_millis(milliseconds))
}

fn timer_ref_term(reference: TimerRef) -> Result<Term, Term> {
    i64::try_from(reference.id())
        .ok()
        .and_then(Term::try_small_int)
        .ok_or_else(badarg)
}

fn timeout_tuple_term_prereserved(
    context: &mut ProcessContext,
    reference: TimerRef,
    message: Term,
) -> Result<Term, Term> {
    context.alloc_tuple_prereserved(&[
        Term::atom(Atom::TIMEOUT),
        timer_ref_term(reference)?,
        message,
    ])
}

fn bool_term(value: bool) -> Term {
    Term::atom(if value { Atom::TRUE } else { Atom::FALSE })
}

fn badarith() -> Term {
    Term::atom(Atom::BADARITH)
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}

#[cfg(test)]
mod tests {
    use super::{
        add, cancel_timer, compare, display, div, error, exact_equal, exact_not_equal,
        get_module_info_2, greater_equal, greater_than, less_equal, less_than, multiply,
        numeric_equal, numeric_not_equal, register_gate1_bifs, rem, send_after, start_timer,
        subtract, throw,
    };
    use crate::atom::{Atom, AtomTable};
    use crate::error::LoadError;
    use crate::module::{ModuleOrigin, PurgeError};
    use crate::native::{
        BifRegistryImpl, Capability, CodeManagementFacility, ExceptionClass, ProcessContext,
    };
    use crate::process::Process;
    use crate::scheduler::{HotLoadResult, PurgeResult};
    use crate::term::Term;
    use crate::term::boxed::{Tuple, write_cons, write_float, write_map, write_tuple};
    use crate::timer::TimerWheel;
    use std::cmp::Ordering;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn context(process: &mut Process) -> ProcessContext<'_> {
        let mut context = ProcessContext::new();
        context.set_atom_table(Some(Arc::new(AtomTable::with_common_atoms())));
        context.attach_process(process, 0);
        context
    }

    fn small_int(value: i64) -> Term {
        Term::small_int(value)
    }

    fn badarith() -> Term {
        Term::atom(Atom::BADARITH)
    }

    fn badarg() -> Term {
        Term::atom(Atom::BADARG)
    }

    struct MetadataFacility {
        module: Atom,
        origin: ModuleOrigin,
    }

    impl CodeManagementFacility for MetadataFacility {
        fn load_module(&self, _bytes: &[u8]) -> Result<HotLoadResult, LoadError> {
            Err(LoadError::DecodeError("unused test facility".into()))
        }

        fn purge_module(&self, module: Atom) -> Result<PurgeResult, PurgeError> {
            Err(PurgeError::NoOldVersion { module })
        }

        fn delete_module(&self, _module: Atom) -> bool {
            false
        }

        fn check_old_code(&self, _module: Atom) -> bool {
            false
        }

        fn check_process_code(&self, _pid: u64, _module: Atom) -> bool {
            false
        }

        fn module_origin(&self, module: Atom) -> Option<ModuleOrigin> {
            (module == self.module).then(|| self.origin.clone())
        }

        fn all_loaded_modules(&self) -> Vec<(Atom, ModuleOrigin)> {
            vec![(self.module, self.origin.clone())]
        }
    }

    #[test]
    fn arithmetic_bifs_return_small_integer_results() {
        let mut process = Process::new(1, 128);
        let mut context = context(&mut process);

        assert_eq!(
            add(&[small_int(3), small_int(4)], &mut context),
            Ok(small_int(7))
        );
        assert_eq!(
            subtract(&[small_int(10), small_int(3)], &mut context),
            Ok(small_int(7))
        );
        assert_eq!(
            multiply(&[small_int(3), small_int(4)], &mut context),
            Ok(small_int(12))
        );
        assert_eq!(
            div(&[small_int(7), small_int(2)], &mut context),
            Ok(small_int(3))
        );
        assert_eq!(
            rem(&[small_int(7), small_int(2)], &mut context),
            Ok(small_int(1))
        );
    }

    #[test]
    fn arithmetic_bifs_return_badarith_for_invalid_inputs() {
        let mut process = Process::new(1, 128);
        let mut context = context(&mut process);

        assert_eq!(
            div(&[small_int(7), small_int(0)], &mut context),
            Err(badarith())
        );
        assert_eq!(
            add(&[Term::atom(Atom::OK), small_int(1)], &mut context),
            Err(badarith())
        );
        assert_eq!(add(&[small_int(1)], &mut context), Err(badarith()));
        assert_eq!(
            add(
                &[small_int(Term::SMALL_INT_MAX), small_int(1)],
                &mut context
            ),
            Err(badarith())
        );
        assert_eq!(
            subtract(
                &[small_int(Term::SMALL_INT_MIN), small_int(1)],
                &mut context
            ),
            Err(badarith())
        );
        assert_eq!(
            multiply(
                &[small_int(Term::SMALL_INT_MAX), small_int(2)],
                &mut context
            ),
            Err(badarith())
        );
        assert_eq!(
            rem(&[small_int(7), small_int(0)], &mut context),
            Err(badarith())
        );
    }

    #[test]
    fn comparison_bifs_return_true_or_false_atoms() {
        let mut process = Process::new(1, 128);
        let mut context = context(&mut process);

        assert_eq!(
            less_than(&[small_int(1), small_int(2)], &mut context),
            Ok(Term::atom(Atom::TRUE))
        );
        assert_eq!(
            less_than(&[small_int(2), small_int(1)], &mut context),
            Ok(Term::atom(Atom::FALSE))
        );
        assert_eq!(
            greater_than(&[small_int(2), small_int(1)], &mut context),
            Ok(Term::atom(Atom::TRUE))
        );
        assert_eq!(
            greater_than(&[small_int(1), small_int(2)], &mut context),
            Ok(Term::atom(Atom::FALSE))
        );
        assert_eq!(
            less_equal(&[small_int(1), small_int(2)], &mut context),
            Ok(Term::atom(Atom::TRUE))
        );
        assert_eq!(
            less_equal(&[small_int(2), small_int(1)], &mut context),
            Ok(Term::atom(Atom::FALSE))
        );
        assert_eq!(
            greater_equal(&[small_int(2), small_int(1)], &mut context),
            Ok(Term::atom(Atom::TRUE))
        );
        assert_eq!(
            greater_equal(&[small_int(1), small_int(2)], &mut context),
            Ok(Term::atom(Atom::FALSE))
        );

        let mut one_float_heap = [0_u64; 2];
        let one_float = write_float(&mut one_float_heap, 1.0).expect("float should fit");
        assert_eq!(
            greater_than(&[one_float, small_int(1)], &mut context),
            Ok(Term::atom(Atom::FALSE))
        );
        assert_eq!(
            less_equal(&[one_float, small_int(1)], &mut context),
            Ok(Term::atom(Atom::TRUE))
        );

        let mut one_float_heap = [0_u64; 2];
        let one_float = write_float(&mut one_float_heap, 1.0).expect("float should fit");
        assert_eq!(
            numeric_equal(&[small_int(1), one_float], &mut context),
            Ok(Term::atom(Atom::TRUE))
        );
        assert_eq!(
            numeric_equal(&[small_int(1), small_int(2)], &mut context),
            Ok(Term::atom(Atom::FALSE))
        );
        let mut one_float_heap = [0_u64; 2];
        let one_float = write_float(&mut one_float_heap, 1.0).expect("float should fit");
        assert_eq!(
            numeric_not_equal(&[small_int(1), one_float], &mut context),
            Ok(Term::atom(Atom::FALSE))
        );
        assert_eq!(
            numeric_not_equal(&[small_int(1), small_int(2)], &mut context),
            Ok(Term::atom(Atom::TRUE))
        );
        assert_eq!(
            exact_equal(&[small_int(1), small_int(1)], &mut context),
            Ok(Term::atom(Atom::TRUE))
        );
        assert_eq!(
            exact_equal(&[small_int(1), small_int(2)], &mut context),
            Ok(Term::atom(Atom::FALSE))
        );
        assert_eq!(
            exact_not_equal(&[small_int(1), small_int(2)], &mut context),
            Ok(Term::atom(Atom::TRUE))
        );
        assert_eq!(
            exact_not_equal(&[small_int(1), small_int(1)], &mut context),
            Ok(Term::atom(Atom::FALSE))
        );
    }

    #[test]
    fn comparison_bifs_return_badarg_only_for_wrong_arity() {
        let mut process = Process::new(1, 128);
        let mut context = context(&mut process);

        // Wrong arity is the sole error condition for total-order comparisons.
        assert_eq!(less_than(&[small_int(1)], &mut context), Err(badarg()));
        assert_eq!(greater_than(&[small_int(1)], &mut context), Err(badarg()));
        assert_eq!(less_equal(&[small_int(1)], &mut context), Err(badarg()));
        assert_eq!(greater_equal(&[], &mut context), Err(badarg()));
        assert_eq!(numeric_equal(&[small_int(1)], &mut context), Err(badarg()));
        assert_eq!(
            numeric_not_equal(&[small_int(1)], &mut context),
            Err(badarg())
        );
        assert_eq!(exact_equal(&[small_int(1)], &mut context), Err(badarg()));
        assert_eq!(
            exact_not_equal(&[small_int(1)], &mut context),
            Err(badarg())
        );
    }

    #[test]
    fn comparison_bifs_use_beam_total_term_order_across_types() {
        let mut process = Process::new(1, 128);
        let mut context = context(&mut process);

        // number < atom: `1 < a` is true; `a >= 1` is true.
        assert_eq!(
            less_than(&[small_int(1), Term::atom(Atom::OK)], &mut context),
            Ok(Term::atom(Atom::TRUE))
        );
        assert_eq!(
            greater_equal(&[Term::atom(Atom::OK), small_int(1)], &mut context),
            Ok(Term::atom(Atom::TRUE))
        );
        // The reverse direction agrees: `a < 1` is false.
        assert_eq!(
            less_than(&[Term::atom(Atom::OK), small_int(1)], &mut context),
            Ok(Term::atom(Atom::FALSE))
        );

        // nil < list: `[] < [1]` is true (rank nil < rank list).
        let mut list_heap = [0_u64; 2];
        let list = write_cons(&mut list_heap, small_int(1), Term::NIL).expect("cons");
        assert_eq!(
            less_than(&[Term::NIL, list], &mut context),
            Ok(Term::atom(Atom::TRUE))
        );

        // tuple < map: rank tuple < rank map.
        let mut tuple_heap = [0_u64; 1];
        let tuple = write_tuple(&mut tuple_heap, &[]).expect("tuple");
        let mut map_heap = [0_u64; 2];
        let map = write_map(&mut map_heap, &[], &[]).expect("map");
        assert_eq!(
            less_than(&[tuple, map], &mut context),
            Ok(Term::atom(Atom::TRUE))
        );
        // Mixed-type `>=`: map >= tuple is true.
        assert_eq!(
            greater_equal(&[map, tuple], &mut context),
            Ok(Term::atom(Atom::TRUE))
        );
    }

    /// Regression: the BIF comparison path must agree with the fused opcode
    /// comparison path (`compare::cmp`) on the same operands — they previously
    /// disagreed (BIF raised `badarith` where the opcode returned a boolean).
    #[test]
    fn comparison_bifs_agree_with_opcode_compare_cmp() {
        let mut process = Process::new(1, 128);
        let mut context = context(&mut process);

        let mut list_heap = [0_u64; 2];
        let list = write_cons(&mut list_heap, small_int(1), Term::NIL).expect("cons");
        let mut tuple_heap = [0_u64; 1];
        let tuple = write_tuple(&mut tuple_heap, &[]).expect("tuple");

        let pairs = [
            (small_int(1), Term::atom(Atom::OK)),
            (Term::atom(Atom::OK), small_int(1)),
            (small_int(1), small_int(2)),
            (Term::NIL, list),
            (tuple, list),
            (Term::atom(Atom::OK), Term::atom(Atom::OK)),
        ];

        for (left, right) in pairs {
            let atom_table = context.atom_table().expect("atom table");
            let opcode_lt = compare::cmp(left, right, atom_table) == Ordering::Less;
            let opcode_ge = compare::cmp(left, right, atom_table) != Ordering::Less;
            assert_eq!(
                less_than(&[left, right], &mut context),
                Ok(Term::atom(if opcode_lt { Atom::TRUE } else { Atom::FALSE })),
                "less_than disagrees with opcode cmp on {left:?} < {right:?}"
            );
            assert_eq!(
                greater_equal(&[left, right], &mut context),
                Ok(Term::atom(if opcode_ge { Atom::TRUE } else { Atom::FALSE })),
                "greater_equal disagrees with opcode cmp on {left:?} >= {right:?}"
            );
        }
    }

    #[test]
    fn utility_bifs_exit_or_return_true() {
        let mut process = Process::new(1, 128);
        let mut context = context(&mut process);

        assert_eq!(
            error(&[Term::atom(Atom::BADARG)], &mut context),
            Err(Term::atom(Atom::BADARG))
        );
        assert_eq!(context.take_exception_class(), ExceptionClass::Error);
        assert_eq!(
            throw(&[Term::atom(Atom::OK)], &mut context),
            Err(Term::atom(Atom::OK))
        );
        assert_eq!(context.take_exception_class(), ExceptionClass::Throw);
        assert_eq!(
            display(&[small_int(42)], &mut context),
            Ok(Term::atom(Atom::TRUE))
        );
    }

    #[test]
    fn module_info_source_reports_embedded_origin() {
        let mut process = Process::new(1, 128);
        let mut context = context(&mut process);
        let module = context
            .atom_table()
            .expect("atom table")
            .intern("embedded_fixture");
        context.set_code_management_facility(Some(Arc::new(MetadataFacility {
            module,
            origin: ModuleOrigin::Embedded,
        })));
        let source = context.atom_table().expect("atom table").intern("source");
        let embedded = context.atom_table().expect("atom table").intern("embedded");

        assert_eq!(
            get_module_info_2(&[Term::atom(module), Term::atom(source)], &mut context),
            Ok(Term::atom(embedded))
        );
    }

    #[test]
    fn utility_bifs_return_badarg_for_wrong_arity() {
        let mut process = Process::new(1, 128);
        let mut context = context(&mut process);

        assert_eq!(error(&[], &mut context), Err(badarg()));
        assert_eq!(throw(&[], &mut context), Err(badarg()));
        assert_eq!(context.take_exception_class(), ExceptionClass::Error);
        assert_eq!(display(&[], &mut context), Err(badarg()));
    }

    #[test]
    fn register_gate1_bifs_registers_all_minimum_mfas() {
        let atom_table = AtomTable::new();
        let registry = BifRegistryImpl::new();

        register_gate1_bifs(&registry, &atom_table).expect("gate 1 BIF registration");

        let erlang = atom_table.intern("erlang");
        for (name, arity, expected_capability) in [
            ("+", 2, Capability::Pure),
            ("-", 2, Capability::Pure),
            ("*", 2, Capability::Pure),
            ("div", 2, Capability::Pure),
            ("rem", 2, Capability::Pure),
            ("<", 2, Capability::Pure),
            (">", 2, Capability::Pure),
            ("=<", 2, Capability::Pure),
            (">=", 2, Capability::Pure),
            ("==", 2, Capability::Pure),
            ("/=", 2, Capability::Pure),
            ("=:=", 2, Capability::Pure),
            ("=/=", 2, Capability::Pure),
            ("error", 1, Capability::Pure),
            ("throw", 1, Capability::Pure),
            ("display", 1, Capability::ExternalIo),
            ("get_module_info", 1, Capability::Pure),
            ("get_module_info", 2, Capability::Pure),
            ("send_after", 3, Capability::Clock),
            ("start_timer", 3, Capability::Clock),
            ("cancel_timer", 1, Capability::Clock),
            ("load_module", 2, Capability::ExternalIo),
            ("purge_module", 1, Capability::ExternalIo),
            ("delete_module", 1, Capability::ExternalIo),
            ("check_old_code", 1, Capability::ExternalIo),
            ("check_process_code", 2, Capability::ExternalIo),
            ("put", 2, Capability::ProcessLocal),
            ("get", 1, Capability::ProcessLocal),
            ("get", 0, Capability::ProcessLocal),
            ("erase", 1, Capability::ProcessLocal),
            ("erase", 0, Capability::ProcessLocal),
            ("get_keys", 1, Capability::ProcessLocal),
            ("term_to_binary", 1, Capability::Pure),
            ("term_to_iovec", 1, Capability::Pure),
            ("term_to_iovec", 2, Capability::Pure),
            ("raise", 3, Capability::Pure),
            ("open_file", 2, Capability::ExternalIo),
            ("close_file", 1, Capability::ExternalIo),
            ("read_file", 2, Capability::ExternalIo),
            ("write_file", 2, Capability::ExternalIo),
            ("inet_setopts", 2, Capability::ExternalIo),
            ("inet_getopts", 2, Capability::ExternalIo),
            ("inet_peername", 1, Capability::ExternalIo),
            ("inet_sockname", 1, Capability::ExternalIo),
            ("inet_port", 1, Capability::ExternalIo),
            ("inet_close", 1, Capability::ExternalIo),
            ("tcp_listen", 2, Capability::ExternalIo),
            ("tcp_accept", 1, Capability::ExternalIo),
            ("tcp_accept", 2, Capability::ExternalIo),
            ("process_info", 1, Capability::Pure),
            ("process_info", 2, Capability::Pure),
            ("system_info", 1, Capability::Pure),
        ] {
            let function = atom_table.intern(name);
            let entry = registry
                .lookup(erlang, function, arity)
                .expect("registered Gate 1 BIF should be present");
            assert_eq!(
                entry.capability, expected_capability,
                "wrong capability for erlang:{name}/{arity}"
            );
        }

        let ets = atom_table.intern("ets");
        for (name, arity) in [
            ("new", 2),
            ("insert", 2),
            ("lookup", 2),
            ("delete", 1),
            ("delete", 2),
            ("member", 2),
            ("info", 1),
            ("info", 2),
        ] {
            let function = atom_table.intern(name);
            let entry = registry
                .lookup(ets, function, arity)
                .expect("registered ETS BIF should be present");
            assert_eq!(entry.capability, Capability::ProcessLocal);
        }

        let global = atom_table.intern("global");
        for (name, arity) in [
            ("register_name", 2),
            ("register_name", 3),
            ("whereis_name", 1),
            ("unregister_name", 1),
        ] {
            let function = atom_table.intern(name);
            let entry = registry
                .lookup(global, function, arity)
                .expect("registered global BIF should be present");
            assert_eq!(entry.capability, Capability::Pure);
        }
    }

    #[test]
    fn register_gate1_bifs_fails_when_called_twice() {
        let atom_table = AtomTable::new();
        let registry = BifRegistryImpl::new();

        register_gate1_bifs(&registry, &atom_table).expect("first registration");

        assert!(register_gate1_bifs(&registry, &atom_table).is_err());
    }

    #[test]
    fn timer_bifs_schedule_start_and_cancel_round_trips() {
        let timers = Arc::new(Mutex::new(TimerWheel::new()));
        let mut process = Process::new(7, 32);
        let mut context = ProcessContext::with_timer_services(7, Arc::clone(&timers));
        context.attach_process(&mut process, 0);

        let send_ref = send_after(
            &[small_int(100), Term::pid(9), Term::atom(Atom::OK)],
            &mut context,
        )
        .expect("send_after schedules");
        assert!(send_ref.as_small_int().is_some());

        let start_ref = start_timer(
            &[small_int(100), Term::pid(9), Term::atom(Atom::OK)],
            &mut context,
        )
        .expect("start_timer schedules");
        assert!(start_ref.as_small_int().is_some());

        let remaining = cancel_timer(&[send_ref], &mut context).expect("cancel pending timer");
        assert!(remaining.as_small_int().is_some());
        assert_eq!(
            cancel_timer(&[send_ref], &mut context),
            Ok(Term::atom(Atom::FALSE))
        );
        context.detach_process();

        let expired = timers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .tick_at(std::time::Instant::now() + Duration::from_millis(101));
        assert_eq!(expired.len(), 1);
        assert_eq!(
            expired[0].reference.id(),
            start_ref.as_small_int().unwrap_or_default() as u64
        );
        let message_ptr = expired[0]
            .message
            .heap_ptr()
            .expect("timeout tuple has a heap pointer");
        assert!(process.heap().contains(message_ptr));
        let tuple = Tuple::new(expired[0].message).expect("timeout tuple");
        assert_eq!(tuple.get(0), Some(Term::atom(Atom::TIMEOUT)));
        assert_eq!(tuple.get(1), Some(start_ref));
        assert_eq!(tuple.get(2), Some(Term::atom(Atom::OK)));
    }
}
