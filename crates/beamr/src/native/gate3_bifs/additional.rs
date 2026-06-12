//! Additional erlang Gate 3 BIFs.

use crate::atom::Atom;
use crate::native::ProcessContext;
use crate::native::bifs::integer_result;
use crate::term::Term;
use crate::term::bigint_math::BigIntValue;
use crate::term::binary_ref::BinaryRef;
use crate::term::boxed::{Closure, Float, Map};

/// erlang:round/1 — rounds a number to the nearest integer.
pub fn bif_round(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [value] = args else {
        return Err(badarg());
    };
    if value.as_small_int().is_some() {
        return Ok(*value);
    }
    float_to_small_int(*value, f64::round)
}

/// erlang:trunc/1 — truncates a number toward zero.
pub fn bif_trunc(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [value] = args else {
        return Err(badarg());
    };
    if value.as_small_int().is_some() {
        return Ok(*value);
    }
    float_to_small_int(*value, f64::trunc)
}

/// erlang:is_bitstring/1 — true for byte-aligned binaries in beamr.
pub fn bif_is_bitstring(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [value] = args else {
        return Err(badarg());
    };
    Ok(bool_term(BinaryRef::new(*value).is_some()))
}

/// erlang:is_function/1 — true for funs (local closures and exports).
pub fn bif_is_function_1(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [value] = args else {
        return Err(badarg());
    };
    Ok(bool_term(Closure::new(*value).is_some()))
}

/// erlang:is_function/2 — true iff the first argument is a fun with exactly
/// the stated arity.
///
/// OTP semantics: the arity must be a non-negative integer or the call is
/// `badarg`. A non-negative bignum is a valid arity that no fun can have, so
/// it yields `false` rather than an error.
pub fn bif_is_function_2(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [value, arity] = args else {
        return Err(badarg());
    };
    if let Some(expected) = arity.as_small_int() {
        if expected < 0 {
            return Err(badarg());
        }
        return Ok(bool_term(
            Closure::new(*value).is_some_and(|closure| i64::from(closure.arity()) == expected),
        ));
    }
    let big = BigIntValue::from_term(*arity).ok_or_else(badarg)?;
    if big.is_negative() {
        return Err(badarg());
    }
    Ok(bool_term(false))
}

/// erlang:is_map_key/2 — returns true when a map contains key.
pub fn bif_is_map_key(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [key, map_term] = args else {
        return Err(badarg());
    };
    let map = Map::new(*map_term).ok_or_else(badarg)?;
    Ok(bool_term(map.get(*key).is_some()))
}

/// erlang:map_size/1 — returns the number of entries in a map.
pub fn bif_map_size(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [map_term] = args else {
        return Err(badarg());
    };
    let map = Map::new(*map_term).ok_or_else(badarg)?;
    i64::try_from(map.len())
        .ok()
        .and_then(Term::try_small_int)
        .ok_or_else(badarg)
}

/// erlang:binary_part/3 — extracts a sub-binary by offset and length.
pub fn bif_binary_part(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [binary_term, offset_term, length_term] = args else {
        return Err(badarg());
    };
    let binary = BinaryRef::new(*binary_term).ok_or_else(badarg)?;
    let offset = offset_term
        .as_small_int()
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(badarg)?;
    let length = length_term
        .as_small_int()
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(badarg)?;
    let end = offset.checked_add(length).ok_or_else(badarg)?;
    let bytes = binary.as_bytes();
    if end > bytes.len() {
        return Err(badarg());
    }
    let bytes = bytes[offset..end].to_vec();
    context.alloc_binary(&bytes)
}

/// erlang:bit_size/1 — returns the bit length of a binary.
pub fn bif_bit_size(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [binary_term] = args else {
        return Err(badarg());
    };
    let bits = match BinaryRef::new(*binary_term) {
        Some(binary) => binary.len().checked_mul(8).ok_or_else(badarg)?,
        // Compiler-reused match contexts: `bit_size` of a match tail is
        // emitted on the context register (see
        // `match_context_remaining_bits`).
        None => crate::interpreter::opcodes::binary::match_context_remaining_bits(*binary_term)
            .ok_or_else(badarg)?,
    };
    i64::try_from(bits)
        .ok()
        .and_then(Term::try_small_int)
        .ok_or_else(badarg)
}

/// erlang:-/1 — unary numeric negation.
///
/// Integer negation promotes to a bignum when the result leaves the
/// small-integer range and demotes bignum results that fit back into it.
pub fn bif_unary_minus(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [value] = args else {
        return Err(badarg());
    };
    if let Some(integer) = value.as_small_int()
        && let Some(negated) = integer.checked_neg().and_then(Term::try_small_int)
    {
        return Ok(negated);
    }
    // Small integers whose negation overflows fall through here too.
    if let Some(integer) = BigIntValue::from_term(*value) {
        return integer_result(integer.negate(), context);
    }
    let value = Float::new(*value).ok_or_else(badarg)?.value();
    if !value.is_finite() {
        return Err(badarg());
    }
    make_float(context, -value)
}

fn float_to_small_int(term: Term, operation: fn(f64) -> f64) -> Result<Term, Term> {
    let value = Float::new(term).ok_or_else(badarg)?.value();
    if !value.is_finite() {
        return Err(badarg());
    }
    let value = operation(value);
    if !value.is_finite()
        || value < Term::SMALL_INT_MIN as f64
        || value > Term::SMALL_INT_MAX as f64
    {
        return Err(badarg());
    }
    Term::try_small_int(value as i64).ok_or_else(badarg)
}

fn make_float(context: &mut ProcessContext, value: f64) -> Result<Term, Term> {
    if !value.is_finite() {
        return Err(badarg());
    }
    context.alloc_float(value)
}

fn bool_term(value: bool) -> Term {
    Term::atom(if value { Atom::TRUE } else { Atom::FALSE })
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}
