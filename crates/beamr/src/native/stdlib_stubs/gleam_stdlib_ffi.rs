//! Gleam stdlib native stubs used by generated Gleam modules.

use crate::atom::{Atom, AtomTable};
use crate::native::ProcessContext;
use crate::term::Term;
use crate::term::binary_ref::BinaryRef;
use crate::term::boxed::Cons;
use crate::term::format::format_term;

pub fn bif_string_replace(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [input, pattern, replacement] = args else {
        return Err(badarg());
    };
    let input = binary_bytes(*input)?;
    let pattern = binary_bytes(*pattern)?;
    let replacement = binary_bytes(*replacement)?;
    if pattern.is_empty() {
        return Err(badarg());
    }

    let mut out = Vec::with_capacity(input.len());
    let mut index = 0;
    while let Some(relative) = find_bytes(&input[index..], &pattern) {
        let match_start = index + relative;
        out.extend_from_slice(&input[index..match_start]);
        out.extend_from_slice(&replacement);
        index = match_start + pattern.len();
    }
    out.extend_from_slice(&input[index..]);
    context.alloc_binary(&out)
}

pub fn bif_less_than(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [left, right] = args else {
        return Err(badarg());
    };
    Ok(bool_term(left < right))
}

pub fn bif_slice(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
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

pub fn bif_crop_string(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [input, length] = args else {
        return Err(badarg());
    };
    let bytes = binary_bytes(*input)?;
    let length = non_negative_usize(*length)?;
    let end = length.min(bytes.len());
    context.alloc_binary(&bytes[..end])
}

pub fn bif_contains_string(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [haystack, needle] = args else {
        return Err(badarg());
    };
    let haystack = binary_bytes(*haystack)?;
    let needle = binary_bytes(*needle)?;
    Ok(bool_term(find_bytes(&haystack, &needle).is_some()))
}

pub fn bif_string_starts_with(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [input, prefix] = args else {
        return Err(badarg());
    };
    let input = binary_bytes(*input)?;
    let prefix = binary_bytes(*prefix)?;
    Ok(bool_term(input.starts_with(&prefix)))
}

pub fn bif_string_ends_with(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [input, suffix] = args else {
        return Err(badarg());
    };
    let input = binary_bytes(*input)?;
    let suffix = binary_bytes(*suffix)?;
    Ok(bool_term(input.ends_with(&suffix)))
}

pub fn bif_string_pop_grapheme(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [input] = args else {
        return Err(badarg());
    };
    let bytes = binary_bytes(*input)?;
    if bytes.is_empty() {
        return Ok(Term::atom(Atom::ERROR));
    }

    let first_len = std::str::from_utf8(&bytes)
        .ok()
        .and_then(|text| text.chars().next().map(char::len_utf8))
        .unwrap_or(1);
    let head_bytes = bytes[..first_len].to_vec();
    let rest_bytes = bytes[first_len..].to_vec();
    let head = context.alloc_binary(&head_bytes)?;
    {
        let process = context.process_mut().ok_or_else(badarg)?;
        process.set_x_reg(0, head);
    }
    let rest = context.alloc_binary(&rest_bytes)?;
    {
        let process = context.process_mut().ok_or_else(badarg)?;
        process.set_x_reg(1, rest);
    }
    context.ensure_heap_space(4)?;
    let (head, rest) = {
        let process = context.process_mut().ok_or_else(badarg)?;
        (process.x_reg(0), process.x_reg(1))
    };
    context.alloc_tuple_prereserved(&[Term::atom(Atom::OK), head, rest])
}

pub fn bif_utf_codepoint_list_to_string(
    args: &[Term],
    context: &mut ProcessContext,
) -> Result<Term, Term> {
    let _ = context;
    let [input] = args else {
        return Err(badarg());
    };
    let mut out = String::new();
    let mut current = *input;
    loop {
        if current.is_nil() {
            return context.alloc_binary(out.as_bytes());
        }
        let cons = Cons::new(current).ok_or_else(badarg)?;
        let codepoint = cons.head().as_small_int().ok_or_else(badarg)?;
        let codepoint = u32::try_from(codepoint).map_err(|_| badarg())?;
        let ch = char::from_u32(codepoint).ok_or_else(badarg)?;
        out.push(ch);
        current = cons.tail();
    }
}

pub fn bif_inspect(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [value] = args else {
        return Err(badarg());
    };
    if let Some(binary) = BinaryRef::new(*value) {
        let bytes = binary.as_bytes().to_vec();
        return context.alloc_binary(&bytes);
    }

    let fallback = AtomTable::with_common_atoms();
    let atom_table = context.atom_table().unwrap_or(&fallback);
    let rendered = format_term(*value, atom_table);
    context.alloc_binary(rendered.as_bytes())
}

pub fn bif_string_remove_prefix(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [input, prefix] = args else {
        return Err(badarg());
    };
    let input = binary_bytes(*input)?;
    let prefix = binary_bytes(*prefix)?;
    if input.starts_with(&prefix) {
        let rest_bytes = input[prefix.len()..].to_vec();
        let rest = context.alloc_binary(&rest_bytes)?;
        {
            let process = context.process_mut().ok_or_else(badarg)?;
            process.set_x_reg(0, rest);
        }
        context.ensure_heap_space(3)?;
        let rest = context.process_mut().ok_or_else(badarg)?.x_reg(0);
        context.alloc_tuple_prereserved(&[Term::atom(Atom::OK), rest])
    } else {
        Ok(Term::atom(Atom::ERROR))
    }
}

pub fn bif_string_remove_suffix(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [input, suffix] = args else {
        return Err(badarg());
    };
    let input = binary_bytes(*input)?;
    let suffix = binary_bytes(*suffix)?;
    if input.ends_with(&suffix) {
        let rest_bytes = input[..input.len() - suffix.len()].to_vec();
        let rest = context.alloc_binary(&rest_bytes)?;
        {
            let process = context.process_mut().ok_or_else(badarg)?;
            process.set_x_reg(0, rest);
        }
        context.ensure_heap_space(3)?;
        let rest = context.process_mut().ok_or_else(badarg)?.x_reg(0);
        context.alloc_tuple_prereserved(&[Term::atom(Atom::OK), rest])
    } else {
        Ok(Term::atom(Atom::ERROR))
    }
}

pub fn bif_iodata_append(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [left, right] = args else {
        return Err(badarg());
    };
    let mut out = Vec::new();
    collect_iodata(*left, &mut out)?;
    collect_iodata(*right, &mut out)?;
    context.alloc_binary(&out)
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

fn binary_bytes(term: Term) -> Result<Vec<u8>, Term> {
    BinaryRef::new(term)
        .map(|binary| binary.as_bytes().to_vec())
        .ok_or_else(badarg)
}

fn non_negative_usize(term: Term) -> Result<usize, Term> {
    term.as_small_int()
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(badarg)
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn bool_term(value: bool) -> Term {
    Term::atom(if value { Atom::TRUE } else { Atom::FALSE })
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}
