//! Type conversion BIFs — atom_to_binary, binary_to_existing_atom,
//! binary_to_list, list_to_binary, map_get.

use crate::atom::Atom;
use crate::native::ProcessContext;
use crate::term::Term;
use crate::term::binary::{Binary, write_binary};
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
    let table = context.atom_table().ok_or_else(badarg)?;
    let name = table.resolve(atom).ok_or_else(badarg)?;
    let bytes = name.as_bytes();

    // Allocate binary on a thread-local heap buffer. BIFs currently cannot
    // allocate on the process heap, so we use a leaked allocation. This is
    // consistent with the AtomTable's own leak strategy and will be replaced
    // when process heap allocation is wired through ProcessContext.
    let word_count = 2 + crate::term::binary::packed_word_count(bytes.len());
    let heap = Box::leak(vec![0u64; word_count].into_boxed_slice());
    write_binary(heap, bytes).ok_or_else(badarg)
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
pub fn bif_binary_to_list(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [binary_term] = args else {
        return Err(badarg());
    };
    let binary = Binary::new(*binary_term).ok_or_else(badarg)?;
    let bytes = binary.as_bytes();

    if bytes.is_empty() {
        return Ok(Term::NIL);
    }

    // Build the list from back to front, each cons cell is 2 words.
    let cell_count = bytes.len();
    let heap = Box::leak(vec![0u64; cell_count * 2].into_boxed_slice());

    let mut tail = Term::NIL;
    for (i, &byte) in bytes.iter().enumerate().rev() {
        let head = Term::small_int(i64::from(byte));
        let cell = &mut heap[i * 2..i * 2 + 2];
        tail = crate::term::boxed::write_cons(cell, head, tail).ok_or_else(badarg)?;
    }

    Ok(tail)
}

/// erlang:list_to_binary/1 — converts a list of integers (0-255) to a binary.
pub fn bif_list_to_binary(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [list_term] = args else {
        return Err(badarg());
    };

    let bytes = list_to_bytes(*list_term)?;
    let word_count = 2 + crate::term::binary::packed_word_count(bytes.len());
    let heap = Box::leak(vec![0u64; word_count].into_boxed_slice());
    write_binary(heap, &bytes).ok_or_else(badarg)
}

/// erlang:map_get/2 — returns the value for `key` in `map`, or raises
/// `{badkey, Key}`.
pub fn bif_map_get(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [key_term, map_term] = args else {
        return Err(badarg());
    };
    let map = Map::new(*map_term).ok_or_else(badarg)?;
    map.get(*key_term).ok_or_else(|| badkey(*key_term))
}

/// Builds a `{badkey, Key}` error tuple on a leaked heap allocation.
fn badkey(key: Term) -> Term {
    let heap = Box::leak(vec![0u64; 3].into_boxed_slice());
    crate::term::boxed::write_tuple(heap, &[Term::atom(Atom::BADKEY), key])
        .unwrap_or_else(|| Term::atom(Atom::BADKEY))
}

/// Walks a proper list collecting byte values (0-255).
fn list_to_bytes(term: Term) -> Result<Vec<u8>, Term> {
    let mut bytes = Vec::new();
    let mut current = term;
    loop {
        if current.is_nil() {
            return Ok(bytes);
        }
        let cons = Cons::new(current).ok_or_else(badarg)?;
        let value = cons.head().as_small_int().ok_or_else(badarg)?;
        if !(0..=255).contains(&value) {
            return Err(badarg());
        }
        bytes.push(value as u8);
        current = cons.tail();
    }
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}

#[cfg(test)]
#[path = "type_conversion_tests.rs"]
mod tests;
