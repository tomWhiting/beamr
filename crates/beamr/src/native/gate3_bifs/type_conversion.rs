//! Type conversion BIFs — atom/binary/list/number conversions and map_get.

use crate::atom::Atom;
use crate::native::ProcessContext;
use crate::native::bifs::integer_result;
use crate::term::Term;
use crate::term::bigint_convert;
use crate::term::binary_ref::BinaryRef;
use crate::term::boxed::{Cons, Float, Map, Tuple};

/// erlang:atom_to_binary/1 — converts an atom to a UTF-8 binary.
pub fn bif_atom_to_binary_1(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [atom_term] = args else {
        return Err(badarg());
    };
    atom_to_binary(*atom_term, context)
}

/// erlang:atom_to_binary/2 — converts an atom to a binary using the given
/// encoding. Both `utf8` and `latin1` produce the same result for ASCII atom
/// names.
pub fn bif_atom_to_binary_2(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [atom_term, encoding_term] = args else {
        return Err(badarg());
    };
    validate_text_encoding(*encoding_term, context)?;
    atom_to_binary(*atom_term, context)
}

fn atom_to_binary(atom_term: Term, context: &mut ProcessContext) -> Result<Term, Term> {
    let atom = atom_term.as_atom().ok_or_else(badarg)?;
    let table = context.atom_table_arc().ok_or_else(badarg)?;
    let name = table.resolve(atom).ok_or_else(badarg)?;
    let bytes = name.as_bytes();

    context.alloc_binary(bytes)
}

/// Backwards-compatible symbol for existing direct callers of the /2 BIF.
pub fn bif_atom_to_binary(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    bif_atom_to_binary_2(args, context)
}

/// erlang:binary_to_atom/1 — interns a UTF-8 binary as an atom.
pub fn bif_binary_to_atom(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [binary_term] = args else {
        return Err(badarg());
    };
    let name = binary_to_utf8(*binary_term)?;
    let table = context.atom_table_arc().ok_or_else(badarg)?;
    Ok(Term::atom(table.intern(name)))
}

/// erlang:binary_to_existing_atom/1 — looks up a binary string in the atom
/// table. Returns the atom if found, badarg if not interned.
pub fn bif_binary_to_existing_atom(
    args: &[Term],
    context: &mut ProcessContext,
) -> Result<Term, Term> {
    let [binary_term] = args else {
        return Err(badarg());
    };
    binary_to_existing_atom(*binary_term, context)
}

/// erlang:binary_to_existing_atom/2 — looks up a UTF-8/latin1-labelled binary
/// string in the atom table. Latin1 transcoding is intentionally not performed.
pub fn bif_binary_to_existing_atom_2(
    args: &[Term],
    context: &mut ProcessContext,
) -> Result<Term, Term> {
    let [binary_term, encoding_term] = args else {
        return Err(badarg());
    };
    validate_text_encoding(*encoding_term, context)?;
    binary_to_existing_atom(*binary_term, context)
}

fn binary_to_existing_atom(binary_term: Term, context: &mut ProcessContext) -> Result<Term, Term> {
    let name = binary_to_utf8(binary_term)?;
    let table = context.atom_table().ok_or_else(badarg)?;
    let atom = table.lookup(name).ok_or_else(badarg)?;
    Ok(Term::atom(atom))
}

fn validate_text_encoding(encoding_term: Term, context: &ProcessContext<'_>) -> Result<(), Term> {
    let encoding = encoding_term.as_atom().ok_or_else(badarg)?;
    let table = context.atom_table().ok_or_else(badarg)?;
    match table.resolve(encoding) {
        Some("utf8" | "latin1") => Ok(()),
        _ => Err(badarg()),
    }
}

/// erlang:binary_to_list/1 — converts binary bytes to a list of integer terms.
pub fn bif_binary_to_list(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [binary_term] = args else {
        return Err(badarg());
    };
    let binary = BinaryRef::new(*binary_term).ok_or_else(badarg)?;
    let bytes = binary.as_bytes();

    let elements: Vec<_> = bytes
        .iter()
        .copied()
        .map(|byte| Term::small_int(i64::from(byte)))
        .collect();
    context.alloc_list(&elements)
}

/// erlang:list_to_atom/1 — interns a proper UTF-8 character list as an atom.
pub fn bif_list_to_atom(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [list_term] = args else {
        return Err(badarg());
    };
    let bytes = proper_byte_list(*list_term)?;
    let name = std::str::from_utf8(&bytes).map_err(|_| badarg())?;
    let table = context.atom_table_arc().ok_or_else(badarg)?;
    Ok(Term::atom(table.intern(name)))
}

/// erlang:list_to_existing_atom/1 — resolves an already-interned atom from a
/// proper UTF-8 character list.
pub fn bif_list_to_existing_atom(
    args: &[Term],
    context: &mut ProcessContext,
) -> Result<Term, Term> {
    let [list_term] = args else {
        return Err(badarg());
    };
    let bytes = proper_byte_list(*list_term)?;
    let name = std::str::from_utf8(&bytes).map_err(|_| badarg())?;
    let table = context.atom_table().ok_or_else(badarg)?;
    let atom = table.lookup(name).ok_or_else(badarg)?;
    Ok(Term::atom(atom))
}

/// erlang:atom_to_list/1 — converts an atom name to a character list.
pub fn bif_atom_to_list(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [atom_term] = args else {
        return Err(badarg());
    };
    let atom = atom_term.as_atom().ok_or_else(badarg)?;
    let table = context.atom_table_arc().ok_or_else(badarg)?;
    let name = table.resolve(atom).ok_or_else(badarg)?;
    make_byte_list(context, name.as_bytes())
}

/// erlang:list_to_integer/1 — parses a base-10 integer from a character list.
///
/// Values beyond the small-integer range allocate a bignum.
pub fn bif_list_to_integer(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [list_term] = args else {
        return Err(badarg());
    };
    let bytes = proper_byte_list(*list_term)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| badarg())?;
    let integer = bigint_convert::from_str_radix(text, 10).ok_or_else(badarg)?;
    integer_result(integer, context)
}

/// erlang:list_to_float/1 — parses a finite float from a character list.
pub fn bif_list_to_float(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [list_term] = args else {
        return Err(badarg());
    };
    let bytes = proper_byte_list(*list_term)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| badarg())?;
    let value = text.parse::<f64>().map_err(|_| badarg())?;
    make_float(context, value)
}

/// erlang:float_to_list/1 — formats a finite float as a character list.
pub fn bif_float_to_list(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [float_term] = args else {
        return Err(badarg());
    };
    let text = format_float(*float_term, None)?;
    make_byte_list(context, text.as_bytes())
}

/// erlang:float_to_binary/2 — formats a finite float as a binary. Supports
/// `{decimals, N}` options in a proper options list.
pub fn bif_float_to_binary_2(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [float_term, options_term] = args else {
        return Err(badarg());
    };
    let decimals = parse_float_format_options(*options_term, context)?;
    let text = format_float(*float_term, decimals)?;
    context.alloc_binary(text.as_bytes())
}

/// erlang:list_to_binary/1 — converts a list of integers (0-255) to a binary.
pub fn bif_list_to_binary(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [list_term] = args else {
        return Err(badarg());
    };
    if !list_term.is_nil()
        && Cons::new(*list_term).is_none()
        && BinaryRef::new(*list_term).is_none()
    {
        return Err(badarg());
    }

    let mut bytes = Vec::new();
    collect_iodata(*list_term, &mut bytes)?;
    context.alloc_binary(&bytes)
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
    if let Some(cons) = Cons::new(term) {
        collect_iodata(cons.head(), bytes)?;
        return collect_iodata(cons.tail(), bytes);
    }
    Err(badarg())
}

fn binary_to_utf8(binary_term: Term) -> Result<&'static str, Term> {
    let binary = BinaryRef::new(binary_term).ok_or_else(badarg)?;
    std::str::from_utf8(binary.as_bytes()).map_err(|_| badarg())
}

fn proper_byte_list(term: Term) -> Result<Vec<u8>, Term> {
    let mut bytes = Vec::new();
    let mut current = term;
    loop {
        if current.is_nil() {
            return Ok(bytes);
        }
        let cons = Cons::new(current).ok_or_else(badarg)?;
        let byte = cons
            .head()
            .as_small_int()
            .and_then(|value| u8::try_from(value).ok())
            .ok_or_else(badarg)?;
        bytes.push(byte);
        current = cons.tail();
    }
}

fn make_byte_list(context: &mut ProcessContext, bytes: &[u8]) -> Result<Term, Term> {
    let elements: Vec<_> = bytes
        .iter()
        .copied()
        .map(|byte| Term::small_int(i64::from(byte)))
        .collect();
    context.alloc_list(&elements)
}

fn parse_float_format_options(
    options_term: Term,
    context: &ProcessContext<'_>,
) -> Result<Option<usize>, Term> {
    let table = context.atom_table().ok_or_else(badarg)?;
    let mut decimals = None;
    let mut current = options_term;
    loop {
        if current.is_nil() {
            return Ok(decimals);
        }
        let cons = Cons::new(current).ok_or_else(badarg)?;
        let option = Tuple::new(cons.head()).ok_or_else(badarg)?;
        if option.arity() != 2 {
            return Err(badarg());
        }
        let name = option
            .get(0)
            .and_then(Term::as_atom)
            .and_then(|atom| table.resolve(atom))
            .ok_or_else(badarg)?;
        if name != "decimals" {
            return Err(badarg());
        }
        let value = option
            .get(1)
            .and_then(Term::as_small_int)
            .and_then(|value| usize::try_from(value).ok())
            .filter(|value| *value <= 253)
            .ok_or_else(badarg)?;
        decimals = Some(value);
        current = cons.tail();
    }
}

fn format_float(float_term: Term, decimals: Option<usize>) -> Result<String, Term> {
    let value = Float::new(float_term).ok_or_else(badarg)?.value();
    if !value.is_finite() {
        return Err(badarg());
    }
    if let Some(decimals) = decimals {
        Ok(format!("{value:.prec$}", prec = decimals))
    } else {
        let mut text = value.to_string();
        if !text.contains(['.', 'e', 'E']) {
            text.push_str(".0");
        }
        Ok(text)
    }
}

fn make_float(context: &mut ProcessContext, value: f64) -> Result<Term, Term> {
    if !value.is_finite() {
        return Err(badarg());
    }
    context.alloc_float(value)
}

/// erlang:map_get/2 — returns the value for `key` in `map`, or raises
/// `{badkey, Key}`.
pub fn bif_map_get(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [key_term, map_term] = args else {
        return Err(badarg());
    };
    let map = Map::new(*map_term).ok_or_else(badarg)?;
    if let Some(value) = map.get(*key_term) {
        Ok(value)
    } else {
        {
            let process = context.process_mut().ok_or_else(badarg)?;
            process.set_x_reg(0, *key_term);
        }
        context.ensure_heap_space(3)?;
        let key = context.process_mut().ok_or_else(badarg)?.x_reg(0);
        Err(badkey_prereserved(context, key)?)
    }
}

/// Builds a `{badkey, Key}` error tuple using pre-reserved heap space.
fn badkey_prereserved(context: &mut ProcessContext, key: Term) -> Result<Term, Term> {
    context.alloc_tuple_prereserved(&[Term::atom(Atom::BADKEY), key])
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}

#[cfg(test)]
#[path = "type_conversion_tests.rs"]
mod tests;
