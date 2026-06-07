//! Type conversion BIFs — atom_to_binary, binary_to_existing_atom,
//! binary_to_list, list_to_binary, map_get.

use crate::atom::Atom;
use crate::native::ProcessContext;
use crate::term::Term;
use crate::term::binary::Binary;
use crate::term::boxed::{Cons, Map};

/// erlang:atom_to_binary/2 — converts an atom to a binary using the given
/// encoding. Both `utf8` and `latin1` produce the same result for ASCII atom
/// names.
pub fn bif_atom_to_binary(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [atom_term, encoding_term] = args else {
        return Err(badarg());
    };
    let atom = atom_term.as_atom().ok_or_else(badarg)?;
    let encoding = encoding_term.as_atom().ok_or_else(badarg)?;
    if encoding != Atom::UTF8 && encoding != Atom::LATIN1 {
        return Err(badarg());
    }
    let table = context.atom_table_arc().ok_or_else(badarg)?;
    let name = table.resolve(atom).ok_or_else(badarg)?;
    let bytes = name.as_bytes();

    context.alloc_binary(bytes)
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
    let binary = Binary::new(*binary_term).ok_or_else(badarg)?;
    let name = std::str::from_utf8(binary.as_bytes()).map_err(|_| badarg())?;
    let table = context.atom_table().ok_or_else(badarg)?;
    let atom = table.lookup(name).ok_or_else(badarg)?;
    Ok(Term::atom(atom))
}

/// erlang:binary_to_list/1 — converts binary bytes to a list of integer terms.
pub fn bif_binary_to_list(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [binary_term] = args else {
        return Err(badarg());
    };
    let binary = Binary::new(*binary_term).ok_or_else(badarg)?;
    let bytes = binary.as_bytes();

    let elements: Vec<_> = bytes
        .iter()
        .copied()
        .map(|byte| Term::small_int(i64::from(byte)))
        .collect();
    context.alloc_list(&elements)
}

/// erlang:list_to_binary/1 — converts a list of integers (0-255) to a binary.
pub fn bif_list_to_binary(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [list_term] = args else {
        return Err(badarg());
    };
    if !list_term.is_nil() && Cons::new(*list_term).is_none() && Binary::new(*list_term).is_none() {
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
    if let Some(binary) = Binary::new(term) {
        bytes.extend_from_slice(binary.as_bytes());
        return Ok(());
    }
    if let Some(cons) = Cons::new(term) {
        collect_iodata(cons.head(), bytes)?;
        return collect_iodata(cons.tail(), bytes);
    }
    Err(badarg())
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
        Err(badkey(context, *key_term)?)
    }
}

/// Builds a `{badkey, Key}` error tuple on the process heap.
fn badkey(context: &mut ProcessContext, key: Term) -> Result<Term, Term> {
    context.alloc_tuple(&[Term::atom(Atom::BADKEY), key])
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}

#[cfg(test)]
#[path = "type_conversion_tests.rs"]
mod tests;
