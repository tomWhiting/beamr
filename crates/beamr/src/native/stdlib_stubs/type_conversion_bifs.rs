//! Additional erlang type conversion BIFs.

use crate::atom::Atom;
use crate::native::ProcessContext;
use crate::native::bifs::integer_result;
use crate::term::Term;
use crate::term::bigint_convert;
use crate::term::binary_ref::BinaryRef;
use crate::term::boxed::{Cons, Float, Tuple};

pub fn bif_atom_to_binary(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [atom_term] = args else {
        return Err(badarg());
    };
    let atom = atom_term.as_atom().ok_or_else(badarg)?;
    let table = context.atom_table_arc().ok_or_else(badarg)?;
    let name = table.resolve(atom).ok_or_else(badarg)?;
    context.alloc_binary(name.as_bytes())
}

pub fn bif_binary_to_float(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [binary_term] = args else {
        return Err(badarg());
    };
    let binary = BinaryRef::new(*binary_term).ok_or_else(badarg)?;
    let text = std::str::from_utf8(binary.as_bytes()).map_err(|_| badarg())?;
    let value = text.parse::<f64>().map_err(|_| badarg())?;
    make_float(context, value)
}

pub fn bif_binary_to_integer(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [binary_term] = args else {
        return Err(badarg());
    };
    binary_to_integer(*binary_term, 10, context)
}

pub fn bif_binary_to_integer_radix(
    args: &[Term],
    context: &mut ProcessContext,
) -> Result<Term, Term> {
    let [binary_term, radix_term] = args else {
        return Err(badarg());
    };
    let radix = parse_radix(*radix_term)?;
    binary_to_integer(*binary_term, radix, context)
}

pub fn bif_float(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [value] = args else {
        return Err(badarg());
    };
    if let Some(integer) = value.as_small_int() {
        return make_float(context, integer as f64);
    }
    if Float::new(*value).is_some() {
        return Ok(*value);
    }
    Err(badarg())
}

pub fn bif_integer_to_binary(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [integer_term] = args else {
        return Err(badarg());
    };
    let text = format_integer_term(*integer_term, 10)?;
    context.alloc_binary(text.as_bytes())
}

pub fn bif_integer_to_binary_radix(
    args: &[Term],
    context: &mut ProcessContext,
) -> Result<Term, Term> {
    let [integer_term, radix_term] = args else {
        return Err(badarg());
    };
    let radix = parse_radix(*radix_term)?;
    let text = format_integer_term(*integer_term, radix)?;
    context.alloc_binary(text.as_bytes())
}

pub fn bif_integer_to_list(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [integer_term] = args else {
        return Err(badarg());
    };
    let text = format_integer_term(*integer_term, 10)?;
    make_list(context, text.bytes().map(i64::from))
}

pub fn bif_integer_to_list_radix(
    args: &[Term],
    context: &mut ProcessContext,
) -> Result<Term, Term> {
    let [integer_term, radix_term] = args else {
        return Err(badarg());
    };
    let radix = parse_radix(*radix_term)?;
    let text = format_integer_term(*integer_term, radix)?;
    make_list(context, text.bytes().map(i64::from))
}

pub fn bif_iolist_to_binary(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [iodata] = args else {
        return Err(badarg());
    };
    let mut bytes = Vec::new();
    collect_iodata(*iodata, &mut bytes)?;
    context.alloc_binary(&bytes)
}

pub fn bif_list_to_bitstring(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    bif_iolist_to_binary(args, context)
}

pub fn bif_list_to_tuple(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [list_term] = args else {
        return Err(badarg());
    };
    context.alloc_tuple(&list_to_vec(*list_term)?)
}

pub fn bif_tuple_to_list(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [tuple_term] = args else {
        return Err(badarg());
    };
    let tuple = Tuple::new(*tuple_term).ok_or_else(badarg)?;
    let mut values = Vec::with_capacity(tuple.arity());
    for index in 0..tuple.arity() {
        values.push(tuple.get(index).ok_or_else(badarg)?);
    }
    context.alloc_list(&values)
}

/// Parses integer text in the given radix, allocating a bignum when the value
/// does not fit a small-integer immediate.
fn binary_to_integer(
    binary_term: Term,
    radix: u32,
    context: &mut ProcessContext,
) -> Result<Term, Term> {
    let binary = BinaryRef::new(binary_term).ok_or_else(badarg)?;
    let text = std::str::from_utf8(binary.as_bytes()).map_err(|_| badarg())?;
    let integer = bigint_convert::from_str_radix(text, radix).ok_or_else(badarg)?;
    integer_result(integer, context)
}

/// Formats a small or bignum integer term in the given radix.
fn format_integer_term(integer_term: Term, radix: u32) -> Result<String, Term> {
    bigint_convert::integer_term_to_string_radix(integer_term, radix).ok_or_else(badarg)
}

fn parse_radix(radix_term: Term) -> Result<u32, Term> {
    radix_term
        .as_small_int()
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| (bigint_convert::MIN_RADIX..=bigint_convert::MAX_RADIX).contains(value))
        .ok_or_else(badarg)
}

fn collect_iodata(term: Term, bytes: &mut Vec<u8>) -> Result<(), Term> {
    if term.is_nil() {
        return Ok(());
    }
    if let Some(byte) = term.as_small_int() {
        let byte = u8::try_from(byte).map_err(|_| badarg())?;
        bytes.push(byte);
        return Ok(());
    }
    if let Some(binary) = BinaryRef::new(term) {
        bytes.extend_from_slice(binary.as_bytes());
        return Ok(());
    }
    let cons = Cons::new(term).ok_or_else(badarg)?;
    collect_iodata(cons.head(), bytes)?;
    collect_iodata(cons.tail(), bytes)
}

fn list_to_vec(term: Term) -> Result<Vec<Term>, Term> {
    let mut values = Vec::new();
    let mut current = term;
    loop {
        if current.is_nil() {
            return Ok(values);
        }
        let cons = Cons::new(current).ok_or_else(badarg)?;
        values.push(cons.head());
        current = cons.tail();
    }
}

fn make_float(context: &mut ProcessContext, value: f64) -> Result<Term, Term> {
    if !value.is_finite() {
        return Err(badarg());
    }
    context.alloc_float(value)
}

fn make_list(
    context: &mut ProcessContext,
    values: impl DoubleEndedIterator<Item = i64>,
) -> Result<Term, Term> {
    let terms: Result<Vec<Term>, Term> = values
        .map(|value| Term::try_small_int(value).ok_or_else(badarg))
        .collect();
    context.alloc_list(&terms?)
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}
