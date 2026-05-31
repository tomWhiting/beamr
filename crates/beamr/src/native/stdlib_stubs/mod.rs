//! Utility stub BIFs for OTP modules — logger, unicode, sys, gleam_stdlib,
//! maps, lists, and timer.
//!
//! These are simple stubs with correct semantics registered under their
//! respective OTP module names. They satisfy imports from gleam_otp actor
//! and supervisor modules.
//!
//! Non-higher-order collection BIFs (maps, lists, timer) are in the
//! `collection_bifs` submodule to keep each file under 500 lines.

pub mod collection_bifs;
pub mod gleam_stdlib_ffi;
pub mod lists_bifs;
pub mod maps_bifs;
pub mod string_bifs;

use crate::atom::{Atom, AtomTable};
use crate::native::{BifRegistryImpl, NativeFn, NativeRegistrationError, ProcessContext};
use crate::term::Term;
use crate::term::binary::Binary;
use crate::term::boxed::Cons;

use collection_bifs::{
    bif_lists_reverse, bif_maps_from_list, bif_maps_map, bif_maps_merge, bif_maps_remove,
    bif_timer_sleep,
};
use gleam_stdlib_ffi::{
    bif_contains_string, bif_crop_string, bif_inspect, bif_iodata_append, bif_less_than, bif_slice,
    bif_string_ends_with, bif_string_pop_grapheme, bif_string_remove_prefix,
    bif_string_remove_suffix, bif_string_replace, bif_string_starts_with,
    bif_utf_codepoint_list_to_string,
};
use lists_bifs::{
    bif_lists_append_1, bif_lists_append_2, bif_lists_join, bif_lists_map, bif_lists_reverse_2,
    bif_lists_seq,
};
use maps_bifs::{
    bif_maps_filter, bif_maps_find, bif_maps_fold, bif_maps_keys, bif_maps_merge_with,
    bif_maps_put, bif_maps_to_list, bif_maps_update_with, bif_maps_values, bif_maps_with,
    bif_maps_without,
};
use string_bifs::{
    bif_equal as bif_string_equal, bif_is_empty as bif_string_is_empty,
    bif_length as bif_string_length, bif_lowercase as bif_string_lowercase,
    bif_reverse as bif_string_reverse, bif_split as bif_string_split, bif_trim as bif_string_trim,
    bif_uppercase as bif_string_uppercase,
};

/// A stub BIF entry: (module_name, function_name, arity, implementation).
type StubBif = (&'static str, &'static str, u8, NativeFn);

const STDLIB_STUBS: &[StubBif] = &[
    ("logger", "warning", 2, bif_logger_warning),
    ("unicode", "characters_to_list", 1, bif_characters_to_list),
    (
        "unicode",
        "characters_to_binary",
        1,
        bif_characters_to_binary,
    ),
    ("sys", "debug_options", 1, bif_debug_options),
    ("gleam_stdlib", "identity", 1, bif_identity),
    ("gleam_stdlib", "string_replace", 3, bif_string_replace),
    ("gleam_stdlib", "less_than", 2, bif_less_than),
    ("gleam_stdlib", "slice", 3, bif_slice),
    ("gleam_stdlib", "crop_string", 2, bif_crop_string),
    ("gleam_stdlib", "contains_string", 2, bif_contains_string),
    (
        "gleam_stdlib",
        "string_starts_with",
        2,
        bif_string_starts_with,
    ),
    ("gleam_stdlib", "string_ends_with", 2, bif_string_ends_with),
    (
        "gleam_stdlib",
        "string_pop_grapheme",
        1,
        bif_string_pop_grapheme,
    ),
    (
        "gleam_stdlib",
        "utf_codepoint_list_to_string",
        1,
        bif_utf_codepoint_list_to_string,
    ),
    ("gleam_stdlib", "inspect", 1, bif_inspect),
    (
        "gleam_stdlib",
        "string_remove_prefix",
        2,
        bif_string_remove_prefix,
    ),
    (
        "gleam_stdlib",
        "string_remove_suffix",
        2,
        bif_string_remove_suffix,
    ),
    ("gleam_stdlib", "iodata_append", 2, bif_iodata_append),
    ("string", "length", 1, bif_string_length),
    ("string", "reverse", 1, bif_string_reverse),
    ("string", "lowercase", 1, bif_string_lowercase),
    ("string", "uppercase", 1, bif_string_uppercase),
    ("string", "trim", 2, bif_string_trim),
    ("string", "split", 3, bif_string_split),
    ("string", "equal", 2, bif_string_equal),
    ("string", "is_empty", 1, bif_string_is_empty),
    ("binary", "part", 3, bif_binary_part),
    // Non-higher-order collection BIFs (B-028a):
    ("maps", "from_list", 1, bif_maps_from_list),
    ("maps", "merge", 2, bif_maps_merge),
    ("maps", "remove", 2, bif_maps_remove),
    // maps:map/2 is a stub — requires interpreter re-entry for closures.
    // The real implementation needs compiled BEAM bytecode; see B-028b.
    ("maps", "map", 2, bif_maps_map),
    ("maps", "put", 3, bif_maps_put),
    ("maps", "find", 2, bif_maps_find),
    ("maps", "keys", 1, bif_maps_keys),
    ("maps", "values", 1, bif_maps_values),
    ("maps", "to_list", 1, bif_maps_to_list),
    ("maps", "fold", 3, bif_maps_fold),
    ("maps", "filter", 2, bif_maps_filter),
    ("maps", "merge_with", 3, bif_maps_merge_with),
    ("maps", "update_with", 4, bif_maps_update_with),
    ("maps", "with", 2, bif_maps_with),
    ("maps", "without", 2, bif_maps_without),
    ("lists", "reverse", 1, bif_lists_reverse),
    ("lists", "append", 1, bif_lists_append_1),
    ("lists", "append", 2, bif_lists_append_2),
    ("lists", "join", 2, bif_lists_join),
    ("lists", "map", 2, bif_lists_map),
    ("lists", "reverse", 2, bif_lists_reverse_2),
    ("lists", "seq", 2, bif_lists_seq),
    ("timer", "sleep", 1, bif_timer_sleep),
];

/// Registers all stdlib stub BIFs under their OTP module names.
pub fn register_stdlib_stubs(
    registry: &mut BifRegistryImpl,
    atom_table: &AtomTable,
) -> Result<(), NativeRegistrationError> {
    for &(module_name, function_name, arity, native_function) in STDLIB_STUBS {
        let module = atom_table.intern(module_name);
        let function = atom_table.intern(function_name);
        registry.register(module, function, arity, native_function)?;
    }

    Ok(())
}

/// logger:warning/2 — prints format string and args to stderr, returns `ok`.
///
/// Accepts (Format, Args) where Format is a binary/string and Args is a list.
/// Prints in a debug-friendly way using eprintln.
pub fn bif_logger_warning(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [format_term, args_term] = args else {
        return Err(badarg());
    };

    // Try to extract the format string from a binary term.
    if let Some(binary) = Binary::new(*format_term) {
        let format_str = String::from_utf8_lossy(binary.as_bytes());
        eprintln!("[warning] {format_str} {args_term:?}");
    } else {
        // Fall back to debug formatting for non-binary format terms.
        eprintln!("[warning] {format_term:?} {args_term:?}");
    }

    Ok(Term::atom(Atom::OK))
}

/// unicode:characters_to_binary/1 — converts input to a binary.
///
/// If the input is already a binary, returns it unchanged. If it is a list
/// of integers, converts code points to UTF-8 bytes and returns a binary.
/// Returns `{error, Binary, Rest}` on failure via badarg for now.
pub fn bif_characters_to_binary(
    args: &[Term],
    _context: &mut ProcessContext,
) -> Result<Term, Term> {
    let [input] = args else {
        return Err(badarg());
    };

    // If already a binary, return unchanged.
    if Binary::new(*input).is_some() {
        return Ok(*input);
    }

    // If it's an empty list, return an empty binary.
    if input.is_nil() {
        return Ok(make_empty_binary());
    }

    // If it's a list, try to collect integer code points into UTF-8 bytes.
    if input.is_list() {
        let mut bytes = Vec::new();
        let mut current = *input;

        loop {
            if current.is_nil() {
                break;
            }
            let cons = Cons::new(current).ok_or_else(badarg)?;
            let head = cons.head();

            // Head could be a small integer (code point) or a binary chunk.
            if let Some(code_point) = head.as_small_int() {
                let cp = u32::try_from(code_point).map_err(|_| badarg())?;
                let ch = char::from_u32(cp).ok_or_else(badarg)?;
                let mut buf = [0u8; 4];
                let encoded = ch.encode_utf8(&mut buf);
                bytes.extend_from_slice(encoded.as_bytes());
            } else if let Some(binary) = Binary::new(head) {
                bytes.extend_from_slice(binary.as_bytes());
            } else {
                return Err(badarg());
            }

            current = cons.tail();
        }

        return Ok(make_leaked_binary(&bytes));
    }

    Err(badarg())
}

/// unicode:characters_to_list/1 — converts a binary to a list of code points.
///
/// Accepts a binary and returns a list of integer code points. Since BIFs
/// lack heap access, cons cells are allocated via leaked boxes.
pub fn bif_characters_to_list(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [input] = args else {
        return Err(badarg());
    };

    let binary = Binary::new(*input).ok_or_else(badarg)?;
    let bytes = binary.as_bytes();

    if bytes.is_empty() {
        return Ok(Term::NIL);
    }

    // Decode UTF-8 bytes to code points, build a proper list in reverse.
    let text = std::str::from_utf8(bytes).map_err(|_| badarg())?;
    let code_points: Vec<i64> = text.chars().map(|ch| i64::from(ch as u32)).collect();

    // Build the list from the end (last element first).
    let mut tail = Term::NIL;
    for &cp in code_points.iter().rev() {
        let int_term = Term::try_small_int(cp).ok_or_else(badarg)?;
        let cell = Box::leak(Box::new([0u64; 2]));
        tail = crate::term::boxed::write_cons(cell, int_term, tail).ok_or_else(badarg)?;
    }

    Ok(tail)
}

/// binary:part/3 — extracts a sub-binary by offset and length.
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
    Ok(make_leaked_binary(&bytes[offset..end]))
}

/// sys:debug_options/1 — no-op stub returning empty list.
///
/// Accepts any list argument and returns `[]`.
pub fn bif_debug_options(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [_options] = args else {
        return Err(badarg());
    };

    Ok(Term::NIL)
}

/// gleam_stdlib:identity/1 — returns its argument unchanged.
pub fn bif_identity(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [value] = args else {
        return Err(badarg());
    };

    Ok(*value)
}

/// Creates an empty binary term using a leaked heap allocation.
fn make_empty_binary() -> Term {
    let heap = Box::leak(Box::new([0u64; 2]));
    crate::term::binary::write_binary(heap, &[]).unwrap_or(Term::NIL)
}

/// Creates a binary term from bytes using a leaked heap allocation.
fn make_leaked_binary(bytes: &[u8]) -> Term {
    let data_words = crate::term::binary::packed_word_count(bytes.len());
    let total_words = 2 + data_words;
    let heap: &mut [u64] = Box::leak(vec![0u64; total_words].into_boxed_slice());
    crate::term::binary::write_binary(heap, bytes).unwrap_or(Term::NIL)
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}

#[cfg(test)]
mod b033_tests;
#[cfg(test)]
mod collection_bifs_tests;
#[cfg(test)]
mod tests;
