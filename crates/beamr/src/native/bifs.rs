//! Built-in function implementations.
//!
//! The set of BIFs is demand-driven: only functions that appear in
//! the loader's unresolved-import report are implemented. Gate 1 currently
//! provides the minimum arithmetic, comparison, and utility BIFs required by
//! unresolved imports.

use std::time::Duration;

use crate::atom::{Atom, AtomTable};
use crate::native::{BifRegistryImpl, NativeFn, NativeRegistrationError, ProcessContext};
use crate::term::Term;
use crate::term::boxed::write_tuple;
use crate::timer::TimerRef;

type Gate1Bif = (&'static str, u8, NativeFn);

const GATE1_BIFS: &[Gate1Bif] = &[
    ("+", 2, add),
    ("-", 2, subtract),
    ("*", 2, multiply),
    ("div", 2, div),
    ("rem", 2, rem),
    ("<", 2, less_than),
    (">=", 2, greater_equal),
    ("=:=", 2, exact_equal),
    ("=/=", 2, exact_not_equal),
    ("error", 1, error),
    ("display", 1, display),
    ("send_after", 3, send_after),
    ("start_timer", 3, start_timer),
    ("cancel_timer", 1, cancel_timer),
];

/// Registers all Gate 1 BIFs into the VM-owned BIF registry.
pub fn register_gate1_bifs(
    registry: &mut BifRegistryImpl,
    atom_table: &AtomTable,
) -> Result<(), NativeRegistrationError> {
    let erlang = atom_table.intern("erlang");

    for &(function_name, arity, native_function) in GATE1_BIFS {
        let function = atom_table.intern(function_name);
        registry.register(erlang, function, arity, native_function)?;
    }

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

/// erlang:</2 for small integers.
pub fn less_than(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let (left, right) = two_small_ints(args)?;
    Ok(bool_term(left < right))
}

/// erlang:>=/2 for small integers.
pub fn greater_equal(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let (left, right) = two_small_ints(args)?;
    Ok(bool_term(left >= right))
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

/// erlang:display/1 prints Debug formatting and returns true.
pub fn display(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [term] = args else {
        return Err(badarg());
    };

    println!("{term:?}");
    Ok(bool_term(true))
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
    let payload = *message;
    let reference = context
        .schedule_timer_with_reference(delay, target_pid, |reference| {
            timeout_tuple_term(reference, payload).unwrap_or(payload)
        })
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

fn timeout_tuple_term(reference: TimerRef, message: Term) -> Result<Term, Term> {
    let words = Box::leak(Box::new([0_u64; 4]));
    write_tuple(
        words,
        &[
            Term::atom(Atom::TIMEOUT),
            timer_ref_term(reference)?,
            message,
        ],
    )
    .ok_or_else(badarg)
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
        add, display, div, error, exact_equal, exact_not_equal, greater_equal, less_than, multiply,
        register_gate1_bifs, rem, subtract,
    };
    use crate::atom::{Atom, AtomTable};
    use crate::native::{BifRegistryImpl, ProcessContext};
    use crate::term::Term;

    fn context() -> ProcessContext {
        ProcessContext::new()
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

    #[test]
    fn arithmetic_bifs_return_small_integer_results() {
        let mut context = context();

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
        let mut context = context();

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
        let mut context = context();

        assert_eq!(
            less_than(&[small_int(1), small_int(2)], &mut context),
            Ok(Term::atom(Atom::TRUE))
        );
        assert_eq!(
            less_than(&[small_int(2), small_int(1)], &mut context),
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
    fn comparison_bifs_return_errors_for_invalid_inputs() {
        let mut context = context();

        assert_eq!(
            less_than(&[Term::atom(Atom::OK), small_int(2)], &mut context),
            Err(badarith())
        );
        assert_eq!(less_than(&[small_int(1)], &mut context), Err(badarith()));
        assert_eq!(
            greater_equal(&[small_int(1), Term::atom(Atom::OK)], &mut context),
            Err(badarith())
        );
        assert_eq!(exact_equal(&[small_int(1)], &mut context), Err(badarg()));
        assert_eq!(
            exact_not_equal(&[small_int(1)], &mut context),
            Err(badarg())
        );
    }

    #[test]
    fn utility_bifs_exit_or_return_true() {
        let mut context = context();

        assert_eq!(
            error(&[Term::atom(Atom::BADARG)], &mut context),
            Err(Term::atom(Atom::BADARG))
        );
        assert_eq!(
            display(&[small_int(42)], &mut context),
            Ok(Term::atom(Atom::TRUE))
        );
    }

    #[test]
    fn utility_bifs_return_badarg_for_wrong_arity() {
        let mut context = context();

        assert_eq!(error(&[], &mut context), Err(badarg()));
        assert_eq!(display(&[], &mut context), Err(badarg()));
    }

    #[test]
    fn register_gate1_bifs_registers_all_minimum_mfas() {
        let atom_table = AtomTable::new();
        let mut registry = BifRegistryImpl::new();

        register_gate1_bifs(&mut registry, &atom_table).expect("gate 1 BIF registration");

        let erlang = atom_table.intern("erlang");
        for (name, arity) in [
            ("+", 2),
            ("-", 2),
            ("*", 2),
            ("div", 2),
            ("rem", 2),
            ("<", 2),
            (">=", 2),
            ("=:=", 2),
            ("=/=", 2),
            ("error", 1),
            ("display", 1),
        ] {
            let function = atom_table.intern(name);
            assert!(
                registry.lookup(erlang, function, arity).is_some(),
                "missing erlang:{name}/{arity}"
            );
        }
    }

    #[test]
    fn register_gate1_bifs_fails_when_called_twice() {
        let atom_table = AtomTable::new();
        let mut registry = BifRegistryImpl::new();

        register_gate1_bifs(&mut registry, &atom_table).expect("first registration");

        assert!(register_gate1_bifs(&mut registry, &atom_table).is_err());
    }
}
