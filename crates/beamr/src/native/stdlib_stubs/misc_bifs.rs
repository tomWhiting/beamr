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

    // Chardata: an arbitrarily nested list of codepoint integers, UTF-8
    // binaries, and further chardata, where any tail may improperly end in a
    // binary. Collect it depth-first.
    if input.is_list() {
        let mut bytes = Vec::new();
        collect_chardata(*input, &mut bytes)?;
        return context.alloc_binary(&bytes);
    }

    Err(badarg())
}

/// Collects Erlang chardata into UTF-8 bytes.
///
/// Heads may be codepoint integers, binaries, or nested chardata lists; tails
/// may be further chardata or an improper binary tail, matching
/// `unicode:chardata()`.
fn collect_chardata(term: Term, bytes: &mut Vec<u8>) -> Result<(), Term> {
    if term.is_nil() {
        return Ok(());
    }
    if let Some(code_point) = term.as_small_int() {
        let cp = u32::try_from(code_point).map_err(|_| badarg())?;
        let ch = char::from_u32(cp).ok_or_else(badarg)?;
        let mut buf = [0u8; 4];
        bytes.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
        return Ok(());
    }
    if let Some(binary) = BinaryRef::new(term) {
        bytes.extend_from_slice(binary.as_bytes());
        return Ok(());
    }
    let cons = Cons::new(term).ok_or_else(badarg)?;
    collect_chardata(cons.head(), bytes)?;
    collect_chardata(cons.tail(), bytes)
}

/// unicode:characters_to_list/1 — converts a binary to a list of code points.
///
/// Accepts a binary and returns a list of integer code points.
pub fn bif_characters_to_list(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [input] = args else {
        return Err(badarg());
    };

    let mut bytes = Vec::new();
    if let Some(binary) = BinaryRef::new(*input) {
        bytes.extend_from_slice(binary.as_bytes());
    } else if input.is_nil() || input.is_list() {
        collect_chardata(*input, &mut bytes)?;
    } else {
        return Err(badarg());
    }

    let text = std::str::from_utf8(&bytes).map_err(|_| badarg())?;
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
