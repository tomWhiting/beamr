//! Utility stub BIFs for OTP modules — logger, unicode, sys, gleam_stdlib,
//! maps, lists, and timer.
//!
//! These are simple stubs with correct semantics registered under their
//! respective OTP module names. They satisfy imports from gleam_otp actor
//! and supervisor modules.
//!
//! Non-higher-order collection BIFs (maps, lists, timer) are in the
//! `collection_bifs` submodule to keep each file under 500 lines.

pub mod bitwise_bifs;
pub mod collection_bifs;
pub mod encoding_bifs;
pub mod gleam_stdlib_ffi;
pub mod gleam_stdlib_ffi2;
pub mod io_bifs;
pub mod lists_bifs;
pub mod maps_bifs;
pub mod math_bifs;
pub mod sample_support_bifs;
pub mod string_bifs;
pub mod type_conversion_bifs;
pub mod uri_bifs;

use crate::atom::{Atom, AtomTable};
use crate::native::{
    BifRegistryImpl, Capability, NativeFn, NativeRegistrationError, ProcessContext,
};
use crate::term::Term;
use crate::term::binary_ref::BinaryRef;
use crate::term::boxed::Cons;
use rand::RngExt;

use bitwise_bifs::{bif_band, bif_bnot, bif_bor, bif_bsl, bif_bsr, bif_bxor};
use collection_bifs::{
    bif_lists_reverse, bif_maps_from_list, bif_maps_map, bif_maps_merge, bif_maps_remove,
    bif_timer_sleep,
};
use encoding_bifs::{
    bif_base64_decode, bif_base64_encode, bif_binary_decode_hex, bif_binary_encode_hex,
};
use gleam_stdlib_ffi::{
    bif_contains_string, bif_crop_string, bif_inspect, bif_iodata_append, bif_less_than, bif_slice,
    bif_string_ends_with, bif_string_pop_grapheme, bif_string_remove_prefix,
    bif_string_remove_suffix, bif_string_replace, bif_string_starts_with,
    bif_utf_codepoint_list_to_string,
};
use gleam_stdlib_ffi2::{
    bif_base16_decode as bif_gleam_base16_decode, bif_base16_encode as bif_gleam_base16_encode,
    bif_base64_decode as bif_gleam_base64_decode, bif_base64_encode as bif_gleam_base64_encode,
    bif_bit_array as bif_gleam_bit_array, bif_bit_array_concat as bif_gleam_bit_array_concat,
    bif_bit_array_pad_to_bytes as bif_gleam_bit_array_pad_to_bytes,
    bif_bit_array_slice as bif_gleam_bit_array_slice,
    bif_bit_array_to_int_and_size as bif_gleam_bit_array_to_int_and_size, bif_classify_dynamic,
    bif_dict, bif_float as bif_gleam_float, bif_float_to_string, bif_index, bif_int,
    bif_int_from_base_string, bif_is_null, bif_list as bif_gleam_list, bif_map_get,
    bif_parse_float, bif_parse_int, bif_print, bif_print_error, bif_println, bif_println_error,
    bif_wrap_list,
};
use io_bifs::{
    bif_io_format_3, bif_io_lib_format_2, bif_io_put_chars_1, bif_io_put_chars_2, bif_io_setopts_2,
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
use math_bifs::{bif_ceil, bif_exp, bif_floor, bif_log, bif_pow};
use sample_support_bifs::{
    bif_gleam_list_map, bif_gleam_result_try, bif_gleam_string_repeat, bif_gleam_string_replace,
    bif_gleam_string_tree_split, bif_gleeunit_main,
};
use string_bifs::{
    bif_equal as bif_string_equal, bif_find as bif_string_find,
    bif_is_empty as bif_string_is_empty, bif_length as bif_string_length,
    bif_lowercase as bif_string_lowercase, bif_next_grapheme as bif_string_next_grapheme,
    bif_pad as bif_string_pad, bif_replace as bif_string_replace4,
    bif_reverse as bif_string_reverse, bif_slice as bif_string_slice,
    bif_split as bif_string_split, bif_trim as bif_string_trim,
    bif_uppercase as bif_string_uppercase,
};
use type_conversion_bifs::{
    bif_atom_to_binary, bif_binary_to_float, bif_binary_to_integer, bif_binary_to_integer_radix,
    bif_float, bif_integer_to_binary, bif_integer_to_binary_radix, bif_integer_to_list,
    bif_iolist_to_binary, bif_list_to_bitstring, bif_list_to_tuple, bif_tuple_to_list,
};
use uri_bifs::{
    bif_parse_query, bif_percent_decode, bif_percent_encode, bif_uri_parse,
    bif_uri_string_dissect_query, bif_uri_string_parse,
};

/// A stub BIF entry: (module_name, function_name, arity, implementation).
type StubBif = (&'static str, &'static str, u8, Capability, NativeFn);

const STDLIB_STUBS: &[StubBif] = &[
    (
        "erlang",
        "atom_to_binary",
        1,
        Capability::Pure,
        bif_atom_to_binary,
    ),
    (
        "erlang",
        "binary_to_float",
        1,
        Capability::Pure,
        bif_binary_to_float,
    ),
    (
        "erlang",
        "binary_to_integer",
        1,
        Capability::Pure,
        bif_binary_to_integer,
    ),
    (
        "erlang",
        "binary_to_integer",
        2,
        Capability::Pure,
        bif_binary_to_integer_radix,
    ),
    ("erlang", "float", 1, Capability::Pure, bif_float),
    (
        "erlang",
        "integer_to_binary",
        1,
        Capability::Pure,
        bif_integer_to_binary,
    ),
    (
        "erlang",
        "integer_to_binary",
        2,
        Capability::Pure,
        bif_integer_to_binary_radix,
    ),
    (
        "erlang",
        "integer_to_list",
        1,
        Capability::Pure,
        bif_integer_to_list,
    ),
    (
        "erlang",
        "iolist_to_binary",
        1,
        Capability::Pure,
        bif_iolist_to_binary,
    ),
    (
        "erlang",
        "list_to_bitstring",
        1,
        Capability::Pure,
        bif_list_to_bitstring,
    ),
    (
        "erlang",
        "list_to_tuple",
        1,
        Capability::Pure,
        bif_list_to_tuple,
    ),
    (
        "erlang",
        "tuple_to_list",
        1,
        Capability::Pure,
        bif_tuple_to_list,
    ),
    ("erlang", "band", 2, Capability::Pure, bif_band),
    ("erlang", "bnot", 1, Capability::Pure, bif_bnot),
    ("erlang", "bor", 2, Capability::Pure, bif_bor),
    ("erlang", "bsl", 2, Capability::Pure, bif_bsl),
    ("erlang", "bsr", 2, Capability::Pure, bif_bsr),
    ("erlang", "bxor", 2, Capability::Pure, bif_bxor),
    ("math", "ceil", 1, Capability::Pure, bif_ceil),
    ("math", "floor", 1, Capability::Pure, bif_floor),
    ("math", "exp", 1, Capability::Pure, bif_exp),
    ("math", "log", 1, Capability::Pure, bif_log),
    ("math", "pow", 2, Capability::Pure, bif_pow),
    ("rand", "uniform", 0, Capability::Entropy, bif_rand_uniform),
    (
        "logger",
        "warning",
        2,
        Capability::ExternalIo,
        bif_logger_warning,
    ),
    (
        "unicode",
        "characters_to_list",
        1,
        Capability::Pure,
        bif_characters_to_list,
    ),
    (
        "unicode",
        "characters_to_binary",
        1,
        Capability::Pure,
        bif_characters_to_binary,
    ),
    (
        "sys",
        "debug_options",
        1,
        Capability::Pure,
        bif_debug_options,
    ),
    (
        "gleam_stdlib",
        "identity",
        1,
        Capability::Pure,
        bif_identity,
    ),
    (
        "gleam_stdlib",
        "classify_dynamic",
        1,
        Capability::Pure,
        bif_classify_dynamic,
    ),
    ("gleam_stdlib", "dict", 1, Capability::Pure, bif_dict),
    (
        "gleam_stdlib",
        "float",
        1,
        Capability::Pure,
        bif_gleam_float,
    ),
    (
        "gleam_stdlib",
        "float_to_string",
        1,
        Capability::Pure,
        bif_float_to_string,
    ),
    ("gleam_stdlib", "index", 2, Capability::Pure, bif_index),
    ("gleam_stdlib", "int", 1, Capability::Pure, bif_int),
    (
        "gleam_stdlib",
        "int_from_base_string",
        2,
        Capability::Pure,
        bif_int_from_base_string,
    ),
    (
        "gleam_stdlib",
        "parse_float",
        1,
        Capability::Pure,
        bif_parse_float,
    ),
    (
        "gleam_stdlib",
        "parse_int",
        1,
        Capability::Pure,
        bif_parse_int,
    ),
    ("gleam_stdlib", "is_null", 1, Capability::Pure, bif_is_null),
    ("gleam_stdlib", "list", 5, Capability::Pure, bif_gleam_list),
    ("gleam_stdlib", "map_get", 2, Capability::Pure, bif_map_get),
    (
        "gleam_stdlib",
        "print",
        1,
        Capability::ExternalIo,
        bif_print,
    ),
    (
        "gleam_stdlib",
        "print_error",
        1,
        Capability::ExternalIo,
        bif_print_error,
    ),
    (
        "gleam_stdlib",
        "println",
        1,
        Capability::ExternalIo,
        bif_println,
    ),
    (
        "gleam_stdlib",
        "println_error",
        1,
        Capability::ExternalIo,
        bif_println_error,
    ),
    (
        "gleam_stdlib",
        "wrap_list",
        1,
        Capability::Pure,
        bif_wrap_list,
    ),
    (
        "gleam_stdlib",
        "parse_query",
        1,
        Capability::Pure,
        bif_parse_query,
    ),
    (
        "gleam_stdlib",
        "percent_decode",
        1,
        Capability::Pure,
        bif_percent_decode,
    ),
    (
        "gleam_stdlib",
        "percent_encode",
        1,
        Capability::Pure,
        bif_percent_encode,
    ),
    (
        "gleam_stdlib",
        "uri_parse",
        1,
        Capability::Pure,
        bif_uri_parse,
    ),
    (
        "gleam_stdlib",
        "base16_decode",
        1,
        Capability::Pure,
        bif_gleam_base16_decode,
    ),
    (
        "gleam_stdlib",
        "base16_encode",
        1,
        Capability::Pure,
        bif_gleam_base16_encode,
    ),
    (
        "gleam_stdlib",
        "base64_decode",
        1,
        Capability::Pure,
        bif_gleam_base64_decode,
    ),
    (
        "gleam_stdlib",
        "base64_encode",
        2,
        Capability::Pure,
        bif_gleam_base64_encode,
    ),
    (
        "gleam_stdlib",
        "bit_array",
        1,
        Capability::Pure,
        bif_gleam_bit_array,
    ),
    (
        "gleam_stdlib",
        "bit_array_concat",
        1,
        Capability::Pure,
        bif_gleam_bit_array_concat,
    ),
    (
        "gleam_stdlib",
        "bit_array_pad_to_bytes",
        1,
        Capability::Pure,
        bif_gleam_bit_array_pad_to_bytes,
    ),
    (
        "gleam_stdlib",
        "bit_array_slice",
        3,
        Capability::Pure,
        bif_gleam_bit_array_slice,
    ),
    (
        "gleam_stdlib",
        "bit_array_to_int_and_size",
        1,
        Capability::Pure,
        bif_gleam_bit_array_to_int_and_size,
    ),
    (
        "gleam_stdlib",
        "string_replace",
        3,
        Capability::Pure,
        bif_string_replace,
    ),
    (
        "gleam_stdlib",
        "less_than",
        2,
        Capability::Pure,
        bif_less_than,
    ),
    ("gleam_stdlib", "slice", 3, Capability::Pure, bif_slice),
    (
        "gleam_stdlib",
        "crop_string",
        2,
        Capability::Pure,
        bif_crop_string,
    ),
    (
        "gleam_stdlib",
        "contains_string",
        2,
        Capability::Pure,
        bif_contains_string,
    ),
    (
        "gleam_stdlib",
        "string_starts_with",
        2,
        Capability::Pure,
        bif_string_starts_with,
    ),
    (
        "gleam_stdlib",
        "string_ends_with",
        2,
        Capability::Pure,
        bif_string_ends_with,
    ),
    (
        "gleam_stdlib",
        "string_pop_grapheme",
        1,
        Capability::Pure,
        bif_string_pop_grapheme,
    ),
    (
        "gleam_stdlib",
        "utf_codepoint_list_to_string",
        1,
        Capability::Pure,
        bif_utf_codepoint_list_to_string,
    ),
    ("gleam_stdlib", "inspect", 1, Capability::Pure, bif_inspect),
    (
        "gleam_stdlib",
        "string_remove_prefix",
        2,
        Capability::Pure,
        bif_string_remove_prefix,
    ),
    (
        "gleam_stdlib",
        "string_remove_suffix",
        2,
        Capability::Pure,
        bif_string_remove_suffix,
    ),
    (
        "gleam_stdlib",
        "iodata_append",
        2,
        Capability::Pure,
        bif_iodata_append,
    ),
    (
        "uri_string",
        "parse",
        1,
        Capability::Pure,
        bif_uri_string_parse,
    ),
    (
        "uri_string",
        "dissect_query",
        1,
        Capability::Pure,
        bif_uri_string_dissect_query,
    ),
    ("string", "length", 1, Capability::Pure, bif_string_length),
    ("string", "reverse", 1, Capability::Pure, bif_string_reverse),
    (
        "string",
        "lowercase",
        1,
        Capability::Pure,
        bif_string_lowercase,
    ),
    (
        "string",
        "uppercase",
        1,
        Capability::Pure,
        bif_string_uppercase,
    ),
    ("string", "trim", 2, Capability::Pure, bif_string_trim),
    ("string", "split", 3, Capability::Pure, bif_string_split),
    ("string", "find", 2, Capability::Pure, bif_string_find),
    (
        "string",
        "next_grapheme",
        1,
        Capability::Pure,
        bif_string_next_grapheme,
    ),
    ("string", "pad", 4, Capability::Pure, bif_string_pad),
    (
        "string",
        "replace",
        4,
        Capability::Pure,
        bif_string_replace4,
    ),
    ("string", "slice", 3, Capability::Pure, bif_string_slice),
    ("string", "equal", 2, Capability::Pure, bif_string_equal),
    (
        "string",
        "is_empty",
        1,
        Capability::Pure,
        bif_string_is_empty,
    ),
    ("binary", "part", 3, Capability::Pure, bif_binary_part),
    (
        "binary",
        "encode_hex",
        1,
        Capability::Pure,
        bif_binary_encode_hex,
    ),
    (
        "binary",
        "decode_hex",
        1,
        Capability::Pure,
        bif_binary_decode_hex,
    ),
    ("base64", "encode", 2, Capability::Pure, bif_base64_encode),
    ("base64", "decode", 1, Capability::Pure, bif_base64_decode),
    (
        "io",
        "put_chars",
        1,
        Capability::ExternalIo,
        bif_io_put_chars_1,
    ),
    (
        "io",
        "put_chars",
        2,
        Capability::ExternalIo,
        bif_io_put_chars_2,
    ),
    ("io", "format", 3, Capability::ExternalIo, bif_io_format_3),
    ("io", "setopts", 2, Capability::ExternalIo, bif_io_setopts_2),
    (
        "io_lib",
        "format",
        2,
        Capability::ExternalIo,
        bif_io_lib_format_2,
    ),
    ("init", "stop", 1, Capability::ExternalIo, bif_init_stop),
    // Non-higher-order collection BIFs (B-028a):
    ("maps", "from_list", 1, Capability::Pure, bif_maps_from_list),
    ("maps", "merge", 2, Capability::Pure, bif_maps_merge),
    ("maps", "remove", 2, Capability::Pure, bif_maps_remove),
    // maps:map/2 is a stub — requires interpreter re-entry for closures.
    // The real implementation needs compiled BEAM bytecode; see B-028b.
    ("maps", "map", 2, Capability::Pure, bif_maps_map),
    ("maps", "put", 3, Capability::Pure, bif_maps_put),
    ("maps", "find", 2, Capability::Pure, bif_maps_find),
    ("maps", "keys", 1, Capability::Pure, bif_maps_keys),
    ("maps", "values", 1, Capability::Pure, bif_maps_values),
    ("maps", "to_list", 1, Capability::Pure, bif_maps_to_list),
    ("maps", "fold", 3, Capability::Pure, bif_maps_fold),
    ("maps", "filter", 2, Capability::Pure, bif_maps_filter),
    (
        "maps",
        "merge_with",
        3,
        Capability::Pure,
        bif_maps_merge_with,
    ),
    (
        "maps",
        "update_with",
        4,
        Capability::Pure,
        bif_maps_update_with,
    ),
    ("maps", "with", 2, Capability::Pure, bif_maps_with),
    ("maps", "without", 2, Capability::Pure, bif_maps_without),
    ("lists", "reverse", 1, Capability::Pure, bif_lists_reverse),
    ("lists", "append", 1, Capability::Pure, bif_lists_append_1),
    ("lists", "append", 2, Capability::Pure, bif_lists_append_2),
    ("lists", "join", 2, Capability::Pure, bif_lists_join),
    ("lists", "map", 2, Capability::Pure, bif_lists_map),
    ("lists", "reverse", 2, Capability::Pure, bif_lists_reverse_2),
    ("lists", "seq", 2, Capability::Pure, bif_lists_seq),
    ("timer", "sleep", 1, Capability::Clock, bif_timer_sleep),
    ("gleam@list", "map", 2, Capability::Pure, bif_gleam_list_map),
    (
        "gleam@string",
        "repeat",
        2,
        Capability::Pure,
        bif_gleam_string_repeat,
    ),
    (
        "gleam@string",
        "replace",
        3,
        Capability::Pure,
        bif_gleam_string_replace,
    ),
    (
        "gleam@string_tree",
        "split",
        2,
        Capability::Pure,
        bif_gleam_string_tree_split,
    ),
    (
        "gleam@result",
        "try",
        2,
        Capability::Pure,
        bif_gleam_result_try,
    ),
    (
        "gleeunit",
        "main",
        0,
        Capability::ExternalIo,
        bif_gleeunit_main,
    ),
    ("erlang", "fun_info", 2, Capability::Pure, bif_fun_info),
    (
        "io_lib_format",
        "fwrite_g",
        1,
        Capability::Pure,
        bif_fwrite_g,
    ),
];

#[cfg(feature = "json")]
const JSON_STUBS: &[StubBif] = &[
    ("json", "decode", 1, Capability::Pure, bif_json_decode),
    ("json", "encode", 1, Capability::Pure, bif_json_encode),
];

/// Registers all stdlib stub BIFs under their OTP module names.
pub fn register_stdlib_stubs(
    registry: &BifRegistryImpl,
    atom_table: &AtomTable,
) -> Result<(), NativeRegistrationError> {
    for &(module_name, function_name, arity, capability, native_function) in STDLIB_STUBS {
        let module = atom_table.intern(module_name);
        let function = atom_table.intern(function_name);
        registry.register(module, function, arity, native_function, capability)?;
    }

    #[cfg(feature = "json")]
    for &(module_name, function_name, arity, capability, native_function) in JSON_STUBS {
        let module = atom_table.intern(module_name);
        let function = atom_table.intern(function_name);
        registry.register(module, function, arity, native_function, capability)?;
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
    if let Some(binary) = BinaryRef::new(*format_term) {
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
pub fn bif_characters_to_binary(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [input] = args else {
        return Err(badarg());
    };

    // If already a binary, return unchanged.
    if BinaryRef::new(*input).is_some() {
        return Ok(*input);
    }

    // If it's an empty list, return an empty binary.
    if input.is_nil() {
        return context.alloc_binary(&[]);
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
            } else if let Some(binary) = BinaryRef::new(head) {
                bytes.extend_from_slice(binary.as_bytes());
            } else {
                return Err(badarg());
            }

            current = cons.tail();
        }

        return context.alloc_binary(&bytes);
    }

    Err(badarg())
}

/// unicode:characters_to_list/1 — converts a binary to a list of code points.
///
/// Accepts a binary and returns a list of integer code points.
pub fn bif_characters_to_list(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [input] = args else {
        return Err(badarg());
    };

    let binary = BinaryRef::new(*input).ok_or_else(badarg)?;
    let bytes = binary.as_bytes();

    let text = std::str::from_utf8(bytes).map_err(|_| badarg())?;
    let elements: Vec<_> = text
        .chars()
        .map(|ch| Term::try_small_int(i64::from(ch as u32)).ok_or_else(badarg))
        .collect::<Result<_, _>>()?;

    context.alloc_list(&elements)
}

/// binary:part/3 — extracts a sub-binary by offset and length.
pub fn bif_binary_part(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
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
    context.alloc_binary(&bytes[offset..end])
}

/// rand:uniform/0 — returns a random float in [0.0, 1.0).
pub fn bif_rand_uniform(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    if !args.is_empty() {
        return Err(badarg());
    }
    let value = rand::rng().random_range(0.0..1.0);
    context.alloc_float(value)
}

/// init:stop/1 — request runtime shutdown and return `ok`.
pub fn bif_init_stop(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [exit_code] = args else {
        return Err(badarg());
    };
    let _code = exit_code.as_small_int().ok_or_else(badarg)?;
    context.request_shutdown();
    Ok(Term::atom(Atom::OK))
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

/// erlang:fun_info/2 — return metadata about a closure.
pub fn bif_fun_info(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [fun, item] = args else {
        return Err(badarg());
    };
    let item_atom = item.as_atom().ok_or_else(badarg)?;
    let at = context.atom_table_arc().ok_or_else(badarg)?;
    let item_name = at.resolve(item_atom).unwrap_or("");
    let value = match item_name {
        "arity" => {
            let arity = crate::term::boxed::Closure::new(*fun).map_or(0, |c| i64::from(c.arity()));
            Term::small_int(arity)
        }
        "module" | "name" | "type" => context.alloc_binary(item_name.as_bytes())?,
        "env" => Term::NIL,
        _ => Term::atom(Atom::UNDEFINED),
    };
    context.alloc_tuple(&[*item, value])
}

/// io_lib_format:fwrite_g/1 — format a float to its shortest representation.
pub fn bif_fwrite_g(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [float_term] = args else {
        return Err(badarg());
    };
    let f = if let Some(v) = float_term.as_small_int() {
        v as f64
    } else if let Some(fl) = crate::term::boxed::Float::new(*float_term) {
        fl.value()
    } else {
        return Err(badarg());
    };
    context.alloc_binary(format!("{f}").as_bytes())
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}

#[cfg(test)]
mod b033_tests;
#[cfg(test)]
mod b038_tests;
#[cfg(test)]
mod bitwise_bifs_tests;
#[cfg(test)]
mod collection_bifs_tests;
#[cfg(feature = "json")]
fn bif_json_decode(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [input] = args else {
        return Err(badarg());
    };
    let binary = BinaryRef::new(*input).ok_or_else(badarg)?;
    let json_value: serde_json::Value =
        serde_json::from_slice(binary.as_bytes()).map_err(|_| badarg())?;
    crate::term::json::value_to_term(&json_value, context).map_err(|_| badarg())
}

#[cfg(feature = "json")]
fn bif_json_encode(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [input] = args else {
        return Err(badarg());
    };
    let atom_table = context.atom_table().ok_or_else(badarg)?;
    let json_value = crate::term::json::term_to_value(*input, atom_table).map_err(|_| badarg())?;
    let json_bytes = serde_json::to_vec(&json_value).map_err(|_| badarg())?;
    context.alloc_binary(&json_bytes)
}

#[cfg(test)]
mod gleam_stdlib_ffi2_tests;
#[cfg(test)]
mod math_bifs_tests;
#[cfg(test)]
mod rand_bifs_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod type_conversion_bifs_tests;
#[cfg(test)]
mod uri_bifs_tests;
