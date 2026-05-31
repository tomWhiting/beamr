//! Additional Gleam stdlib native stubs for data conversion and encoding.

use crate::atom::Atom;
use crate::native::ProcessContext;
use crate::term::Term;
use crate::term::binary::{Binary, packed_word_count, write_binary};
use crate::term::boxed::{Cons, Float, Map, Tuple, write_float, write_map, write_tuple};

use super::encoding_bifs::{
    bif_base64_decode as erlang_base64_decode, bif_base64_encode as erlang_base64_encode,
    bif_binary_decode_hex, bif_binary_encode_hex,
};

pub fn bif_classify_dynamic(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [value] = args else {
        return Err(badarg());
    };
    Ok(Term::atom(intern_atom(context, classify(*value))?))
}

pub fn bif_dict(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [pairs] = args else {
        return Err(badarg());
    };
    let mut entries = Vec::new();
    let mut current = *pairs;
    while !current.is_nil() {
        let cons = Cons::new(current).ok_or_else(badarg)?;
        let pair = Tuple::new(cons.head()).ok_or_else(badarg)?;
        if pair.arity() != 2 {
            return Err(badarg());
        }
        set_entry(
            &mut entries,
            pair.get(0).ok_or_else(badarg)?,
            pair.get(1).ok_or_else(badarg)?,
        );
        current = cons.tail();
    }
    make_sorted_map(&entries)
}

pub fn bif_float(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [value] = args else {
        return Err(badarg());
    };
    if let Some(integer) = value.as_small_int() {
        return make_float(integer as f64);
    }
    if Float::new(*value).is_some() {
        return Ok(*value);
    }
    Err(badarg())
}

pub fn bif_float_to_string(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [value] = args else {
        return Err(badarg());
    };
    let float = Float::new(*value).ok_or_else(badarg)?;
    make_binary(float.value().to_string().as_bytes())
}

pub fn bif_index(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [collection, index] = args else {
        return Err(badarg());
    };
    let index = non_negative_usize(*index)?;
    if let Some(tuple) = Tuple::new(*collection) {
        return tuple.get(index).ok_or_else(badarg);
    }
    list_index(*collection, index)
}

pub fn bif_int(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [value] = args else {
        return Err(badarg());
    };
    if value.as_small_int().is_some() {
        Ok(*value)
    } else {
        Err(badarg())
    }
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
    result_tuple(parse_int_with_base(*string, base))
}

pub fn bif_parse_float(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [string] = args else {
        return Err(badarg());
    };
    let result = parse_float_binary(*string).and_then(make_float);
    result_tuple(result)
}

pub fn bif_parse_int(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [string] = args else {
        return Err(badarg());
    };
    result_tuple(parse_int_with_base(*string, 10))
}

pub fn bif_is_null(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [value] = args else {
        return Err(badarg());
    };
    Ok(Term::atom(if value.is_nil() {
        Atom::TRUE
    } else {
        Atom::FALSE
    }))
}

pub fn bif_list(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [_decoder, _type_name, _path, value, _decode_errors] = args else {
        return Err(badarg());
    };
    // Approximate gleam@dynamic@decode support: accept already-list values and
    // return them unchanged until closure re-entry is available for full decode.
    if value.is_nil() || Cons::new(*value).is_some() {
        Ok(*value)
    } else {
        Err(badarg())
    }
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
    Binary::new(value)
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

pub fn bif_bit_array(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [value] = args else {
        return Err(badarg());
    };
    Binary::new(*value).ok_or_else(badarg)?;
    Ok(*value)
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
        let binary = Binary::new(cons.head()).ok_or_else(badarg)?;
        bytes.extend_from_slice(binary.as_bytes());
        current = cons.tail();
    }
    make_binary(&bytes)
}

pub fn bif_bit_array_pad_to_bytes(
    args: &[Term],
    context: &mut ProcessContext,
) -> Result<Term, Term> {
    let _ = context;
    let [input] = args else {
        return Err(badarg());
    };
    Binary::new(*input).ok_or_else(badarg)?;
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
    make_binary(&bytes[offset..end])
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

fn classify(value: Term) -> &'static str {
    if value.is_small_int() {
        "int"
    } else if value.is_atom() {
        "atom"
    } else if value.is_pid() {
        "pid"
    } else if value.is_nil() || Cons::new(value).is_some() {
        "list"
    } else if Binary::new(value).is_some() {
        "binary"
    } else if Float::new(value).is_some() {
        "float"
    } else if Tuple::new(value).is_some() {
        "tuple"
    } else if Map::new(value).is_some() {
        "map"
    } else {
        "unknown"
    }
}

fn intern_atom(context: &mut ProcessContext, name: &str) -> Result<Atom, Term> {
    context
        .atom_table()
        .map(|table| table.intern(name))
        .ok_or_else(badarg)
}

fn set_entry(entries: &mut Vec<(Term, Term)>, key: Term, value: Term) {
    if let Some((_, existing_value)) = entries.iter_mut().find(|(entry_key, _)| *entry_key == key) {
        *existing_value = value;
    } else {
        entries.push((key, value));
    }
}

fn make_sorted_map(entries: &[(Term, Term)]) -> Result<Term, Term> {
    let mut sorted = entries.to_vec();
    sorted.sort_by(|(left, _), (right, _)| left.cmp(right));
    let keys: Vec<_> = sorted.iter().map(|(key, _)| *key).collect();
    let values: Vec<_> = sorted.iter().map(|(_, value)| *value).collect();
    let heap = Box::leak(vec![0u64; 2 + keys.len() + values.len()].into_boxed_slice());
    write_map(heap, &keys, &values).ok_or_else(badarg)
}

fn list_index(list: Term, index: usize) -> Result<Term, Term> {
    let mut current = list;
    let mut remaining = index;
    loop {
        let cons = Cons::new(current).ok_or_else(badarg)?;
        if remaining == 0 {
            return Ok(cons.head());
        }
        remaining -= 1;
        current = cons.tail();
    }
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
    if let Some(integer) = term.as_small_int() {
        return integer.to_string();
    }
    if let Some(atom) = term.as_atom() {
        return context
            .atom_table()
            .and_then(|table| table.resolve(atom))
            .map(str::to_owned)
            .unwrap_or_else(|| format!("Atom({atom:?})"));
    }
    if term.is_nil() {
        return "[]".to_owned();
    }
    if let Some(binary) = Binary::new(term) {
        return match std::str::from_utf8(binary.as_bytes()) {
            Ok(text) => text.to_owned(),
            Err(_) => format!("<<{} bytes>>", binary.len()),
        };
    }
    if let Some(tuple) = Tuple::new(term) {
        let mut elements = Vec::with_capacity(tuple.arity());
        for index in 0..tuple.arity() {
            if let Some(element) = tuple.get(index) {
                elements.push(render_term(element, context));
            }
        }
        return format!("{{{}}}", elements.join(", "));
    }
    format!("{term:?}")
}

fn result_tuple(result: Result<Term, Term>) -> Result<Term, Term> {
    let values = match result {
        Ok(value) => [Term::atom(Atom::OK), value],
        Err(_) => [Term::atom(Atom::ERROR), Term::NIL],
    };
    let heap = Box::leak(Box::new([0u64; 3]));
    write_tuple(heap, &values).ok_or_else(badarg)
}

fn binary_text(binary: Term) -> Result<&'static str, Term> {
    std::str::from_utf8(binary_bytes(binary)?).map_err(|_| badarg())
}

fn binary_bytes(term: Term) -> Result<&'static [u8], Term> {
    Binary::new(term)
        .map(|binary| binary.as_bytes())
        .ok_or_else(badarg)
}

fn non_negative_usize(term: Term) -> Result<usize, Term> {
    term.as_small_int()
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(badarg)
}

fn make_binary(bytes: &[u8]) -> Result<Term, Term> {
    let heap = Box::leak(vec![0u64; 2 + packed_word_count(bytes.len())].into_boxed_slice());
    write_binary(heap, bytes).ok_or_else(badarg)
}

fn make_float(value: f64) -> Result<Term, Term> {
    if !value.is_finite() {
        return Err(badarg());
    }
    let heap = Box::leak(Box::new([0u64; 2]));
    write_float(heap, value).ok_or_else(badarg)
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}
