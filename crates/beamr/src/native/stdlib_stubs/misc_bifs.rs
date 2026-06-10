//! Miscellaneous OTP stub BIFs — logger, unicode, rand, init, fun
//! introspection, and float formatting.

use crate::atom::Atom;
use crate::native::ProcessContext;
use crate::term::Term;
use crate::term::binary_ref::BinaryRef;
use crate::term::boxed::Cons;
use rand::RngExt;

/// logger:warning/2 — writes format string and args to the configured I/O sink, returns `ok`.
///
/// Accepts (Format, Args) where Format is a binary/string and Args is a list.
pub fn bif_logger_warning(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [format_term, args_term] = args else {
        return Err(badarg());
    };

    let message = if let Some(binary) = BinaryRef::new(*format_term) {
        let format_str = String::from_utf8_lossy(binary.as_bytes());
        format!("[warning] {format_str} {args_term:?}\n")
    } else {
        format!("[warning] {format_term:?} {args_term:?}\n")
    };
    context.io_sink().write(message.as_bytes());

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
    let mut text = format!("{f}");
    if !text.contains(['.', 'e', 'E']) {
        text.push_str(".0");
    }
    context.alloc_binary(text.as_bytes())
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}
