//! Additional Gleam stdlib native stubs for data conversion and encoding.

use crate::atom::{Atom, AtomTable};
use crate::native::ProcessContext;
use crate::term::Term;
use crate::term::binary_ref::BinaryRef;
use crate::term::boxed::{Cons, Float, Map};
use crate::term::format::format_term;

use super::encoding_bifs::{
    bif_base64_decode as erlang_base64_decode, bif_base64_encode as erlang_base64_encode,
    bif_binary_decode_hex, bif_binary_encode_hex,
};

pub fn bif_float_to_string(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [value] = args else {
        return Err(badarg());
    };
    let float = Float::new(*value).ok_or_else(badarg)?;
    context.alloc_binary(float.value().to_string().as_bytes())
}

pub fn bif_int_from_base_string(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [string, base] = args else {
        return Err(badarg());
    };
    let base = base
        .as_small_int()
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| (2..=36).contains(value))
        .ok_or_else(badarg)?;
    result_tuple(context, parse_int_with_base(*string, base))
}

pub fn bif_parse_float(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [string] = args else {
        return Err(badarg());
    };
    let result = parse_float_binary(*string).and_then(|value| make_float(context, value));
    result_tuple(context, result)
}

pub fn bif_parse_int(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [string] = args else {
        return Err(badarg());
    };
    result_tuple(context, parse_int_with_base(*string, 10))
}

pub fn bif_map_get(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [map_term, key] = args else {
        return Err(badarg());
    };
    let map = Map::new(*map_term).ok_or_else(badarg)?;
    match map.get(*key) {
        Some(value) => context.alloc_tuple(&[Term::atom(Atom::OK), value]),
        None => context.alloc_tuple(&[Term::atom(Atom::ERROR), Term::NIL]),
    }
}

pub fn bif_print(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    write_print_args(args, context, false)
}

pub fn bif_print_error(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    // There is currently one configured IoSink, so stderr-flavoured Gleam
    // wrappers intentionally write to the same sink as stdout wrappers.
    write_print_args(args, context, false)
}

pub fn bif_println(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    write_print_args(args, context, true)
}

pub fn bif_println_error(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    // There is currently one configured IoSink, so stderr-flavoured Gleam
    // wrappers intentionally write to the same sink as stdout wrappers.
    write_print_args(args, context, true)
}

fn write_print_args(
    args: &[Term],
    context: &mut ProcessContext,
    newline: bool,
) -> Result<Term, Term> {
    let [value] = args else {
        return Err(badarg());
    };
    let mut bytes = print_bytes(*value, context);
    if newline {
        bytes.push(b'\n');
    }
    context.io_sink().write(&bytes);
    Ok(Term::atom(Atom::OK))
}

fn print_bytes(value: Term, context: &ProcessContext) -> Vec<u8> {
    BinaryRef::new(value)
        .map(|binary| binary.as_bytes().to_vec())
        .unwrap_or_else(|| render_term(value, context).into_bytes())
}

pub fn bif_wrap_list(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [value] = args else {
        return Err(badarg());
    };
    if value.is_nil() || Cons::new(*value).is_some() {
        Ok(*value)
    } else {
        context.alloc_cons(*value, Term::NIL)
    }
}

pub fn bif_base16_decode(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    bif_binary_decode_hex(args, context)
}

pub fn bif_base16_encode(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    bif_binary_encode_hex(args, context)
}

pub fn bif_base64_decode(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    erlang_base64_decode(args, context)
}

pub fn bif_base64_encode(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    erlang_base64_encode(args, context)
}

pub fn bif_bit_array_concat(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [parts] = args else {
        return Err(badarg());
    };
    let mut bytes = Vec::new();
    let mut current = *parts;
    while !current.is_nil() {
        let cons = Cons::new(current).ok_or_else(badarg)?;
        let binary = BinaryRef::new(cons.head()).ok_or_else(badarg)?;
        bytes.extend_from_slice(binary.as_bytes());
        current = cons.tail();
    }
    context.alloc_binary(&bytes)
}

pub fn bif_bit_array_pad_to_bytes(
    args: &[Term],
    context: &mut ProcessContext,
) -> Result<Term, Term> {
    let _ = context;
    let [input] = args else {
        return Err(badarg());
    };
    BinaryRef::new(*input).ok_or_else(badarg)?;
    Ok(*input)
}

pub fn bif_bit_array_slice(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [input, offset, length] = args else {
        return Err(badarg());
    };
    let bytes = binary_bytes(*input)?;
    let offset = non_negative_usize(*offset)?;
    let length = non_negative_usize(*length)?;
    let end = offset.checked_add(length).ok_or_else(badarg)?;
    if end > bytes.len() {
        return Err(badarg());
    }
    context.alloc_binary(&bytes[offset..end])
}

pub fn bif_bit_array_to_int_and_size(
    args: &[Term],
    context: &mut ProcessContext,
) -> Result<Term, Term> {
    let [input] = args else {
        return Err(badarg());
    };
    let bytes = binary_bytes(*input)?;
    let mut value = 0i64;
    for byte in bytes {
        value = value
            .checked_mul(256)
            .and_then(|acc| acc.checked_add(i64::from(*byte)))
            .ok_or_else(badarg)?;
    }
    let size = bytes.len().checked_mul(8).ok_or_else(badarg)?;
    let value = Term::try_small_int(value).ok_or_else(badarg)?;
    let size = i64::try_from(size)
        .ok()
        .and_then(Term::try_small_int)
        .ok_or_else(badarg)?;
    context.alloc_tuple(&[value, size])
}

fn parse_int_with_base(binary: Term, base: u32) -> Result<Term, Term> {
    let text = binary_text(binary)?;
    let integer = i64::from_str_radix(text, base).map_err(|_| badarg())?;
    Term::try_small_int(integer).ok_or_else(badarg)
}

fn parse_float_binary(binary: Term) -> Result<f64, Term> {
    let value = binary_text(binary)?.parse::<f64>().map_err(|_| badarg())?;
    if value.is_finite() {
        Ok(value)
    } else {
        Err(badarg())
    }
}

fn render_term(term: Term, context: &ProcessContext) -> String {
    let fallback = AtomTable::with_common_atoms();
    let atom_table = context.atom_table().unwrap_or(&fallback);
    format_term(term, atom_table)
}

fn result_tuple(context: &mut ProcessContext, result: Result<Term, Term>) -> Result<Term, Term> {
    let values = match result {
        Ok(value) => [Term::atom(Atom::OK), value],
        Err(_) => [Term::atom(Atom::ERROR), Term::NIL],
    };
    context.alloc_tuple(&values)
}

fn binary_text(binary: Term) -> Result<&'static str, Term> {
    std::str::from_utf8(binary_bytes(binary)?).map_err(|_| badarg())
}

fn binary_bytes(term: Term) -> Result<&'static [u8], Term> {
    BinaryRef::new(term)
        .map(|binary| binary.as_bytes())
        .ok_or_else(badarg)
}

fn non_negative_usize(term: Term) -> Result<usize, Term> {
    term.as_small_int()
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(badarg)
}

fn make_float(context: &mut ProcessContext, value: f64) -> Result<Term, Term> {
    if !value.is_finite() {
        return Err(badarg());
    }
    context.alloc_float(value)
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}
