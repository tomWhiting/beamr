//! Additional erlang Gate 3 BIFs.

use crate::atom::Atom;
use crate::native::ProcessContext;
use crate::term::Term;
use crate::term::binary::Binary;
use crate::term::boxed::{Float, Map};

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
    Ok(bool_term(Binary::new(*value).is_some()))
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
    let binary = Binary::new(*binary_term).ok_or_else(badarg)?;
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
    context.alloc_binary(&bytes[offset..end])
}

/// erlang:bit_size/1 — returns the bit length of a binary.
pub fn bif_bit_size(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [binary_term] = args else {
        return Err(badarg());
    };
    let binary = Binary::new(*binary_term).ok_or_else(badarg)?;
    let bits = binary.len().checked_mul(8).ok_or_else(badarg)?;
    i64::try_from(bits)
        .ok()
        .and_then(Term::try_small_int)
        .ok_or_else(badarg)
}

/// erlang:-/1 — unary numeric negation.
pub fn bif_unary_minus(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [value] = args else {
        return Err(badarg());
    };
    if let Some(integer) = value.as_small_int() {
        return integer
            .checked_neg()
            .and_then(Term::try_small_int)
            .ok_or_else(badarg);
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
