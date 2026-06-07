//! Erlang `string` module native stubs for binary string inputs.

use crate::atom::Atom;
use crate::native::ProcessContext;
use crate::term::Term;
use crate::term::binary::Binary;

pub fn bif_length(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [input] = args else {
        return Err(badarg());
    };
    let len = binary_bytes(*input)?.len();
    i64::try_from(len)
        .ok()
        .and_then(Term::try_small_int)
        .ok_or_else(badarg)
}

pub fn bif_reverse(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [input] = args else {
        return Err(badarg());
    };
    let mut bytes = binary_bytes(*input)?.to_vec();
    bytes.reverse();
    context.alloc_binary(&bytes)
}

pub fn bif_lowercase(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [input] = args else {
        return Err(badarg());
    };
    let bytes: Vec<u8> = binary_bytes(*input)?
        .iter()
        .map(|byte| byte.to_ascii_lowercase())
        .collect();
    context.alloc_binary(&bytes)
}

pub fn bif_uppercase(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [input] = args else {
        return Err(badarg());
    };
    let bytes: Vec<u8> = binary_bytes(*input)?
        .iter()
        .map(|byte| byte.to_ascii_uppercase())
        .collect();
    context.alloc_binary(&bytes)
}

pub fn bif_trim(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [input, direction] = args else {
        return Err(badarg());
    };
    let bytes = binary_bytes(*input)?;
    let direction = atom_name(*direction, context)?;
    let (mut start, mut end) = (0, bytes.len());

    if direction == "leading" || direction == "both" {
        while start < end && bytes[start].is_ascii_whitespace() {
            start += 1;
        }
    } else if direction != "trailing" {
        return Err(badarg());
    }

    if direction == "trailing" || direction == "both" {
        while end > start && bytes[end - 1].is_ascii_whitespace() {
            end -= 1;
        }
    }

    context.alloc_binary(&bytes[start..end])
}

pub fn bif_split(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [input, pattern, option] = args else {
        return Err(badarg());
    };
    let input = binary_bytes(*input)?;
    let pattern = binary_bytes(*pattern)?;
    if pattern.is_empty() {
        return Err(badarg());
    }
    let option = atom_name(*option, context)?;
    let parts = match option {
        "all" => split_all(input, pattern),
        "leading" => split_once(input, pattern, false),
        "trailing" => split_once(input, pattern, true),
        _ => return Err(badarg()),
    };

    let mut terms = Vec::with_capacity(parts.len());
    for part in parts {
        terms.push(context.alloc_binary(part)?);
    }
    context.alloc_list(&terms)
}

pub fn bif_find(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [input, pattern] = args else {
        return Err(badarg());
    };
    let input = binary_bytes(*input)?;
    let pattern = binary_bytes(*pattern)?;
    if let Some(index) = find_bytes(input, pattern) {
        context.alloc_binary(&input[index..])
    } else {
        atom_term("nomatch", context)
    }
}

pub fn bif_next_grapheme(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [input] = args else {
        return Err(badarg());
    };
    let bytes = binary_bytes(*input)?;
    if bytes.is_empty() {
        return Ok(Term::atom(Atom::ERROR));
    }

    let first_len = std::str::from_utf8(bytes)
        .ok()
        .and_then(|text| text.chars().next().map(char::len_utf8))
        .unwrap_or(1);
    let head = context.alloc_binary(&bytes[..first_len])?;
    let rest = context.alloc_binary(&bytes[first_len..])?;
    context.alloc_tuple(&[Term::atom(Atom::OK), head, rest])
}

pub fn bif_pad(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [input, length, direction, pad] = args else {
        return Err(badarg());
    };
    let input = binary_bytes(*input)?;
    let target_len = non_negative_usize(*length)?;
    let direction = atom_name(*direction, context)?;
    let pad = binary_bytes(*pad)?;
    if pad.is_empty() {
        return Err(badarg());
    }
    if input.len() >= target_len {
        return context.alloc_binary(input);
    }

    let needed = target_len - input.len();
    let (leading, trailing) = match direction {
        "leading" => (needed, 0),
        "trailing" => (0, needed),
        "both" => (needed / 2, needed - (needed / 2)),
        _ => return Err(badarg()),
    };
    let mut out = Vec::with_capacity(target_len);
    append_pad(&mut out, pad, leading);
    out.extend_from_slice(input);
    append_pad(&mut out, pad, trailing);
    context.alloc_binary(&out)
}

pub fn bif_replace(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [input, pattern, replacement, where_atom] = args else {
        return Err(badarg());
    };
    let input = binary_bytes(*input)?;
    let pattern = binary_bytes(*pattern)?;
    let replacement = binary_bytes(*replacement)?;
    if pattern.is_empty() {
        return Err(badarg());
    }
    let where_name = atom_name(*where_atom, context)?;
    let out = match where_name {
        "all" => replace_all(input, pattern, replacement),
        "leading" => replace_once(input, pattern, replacement, false),
        "trailing" => replace_once(input, pattern, replacement, true),
        _ => return Err(badarg()),
    };
    context.alloc_binary(&out)
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

pub fn bif_equal(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [left, right] = args else {
        return Err(badarg());
    };
    Ok(bool_term(binary_bytes(*left)? == binary_bytes(*right)?))
}

pub fn bif_is_empty(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [input] = args else {
        return Err(badarg());
    };
    Ok(bool_term(binary_bytes(*input)?.is_empty()))
}

fn split_all<'a>(input: &'a [u8], pattern: &[u8]) -> Vec<&'a [u8]> {
    let mut parts = Vec::new();
    let mut index = 0;
    while let Some(relative) = find_bytes(&input[index..], pattern) {
        let match_start = index + relative;
        parts.push(&input[index..match_start]);
        index = match_start + pattern.len();
    }
    parts.push(&input[index..]);
    parts
}

fn split_once<'a>(input: &'a [u8], pattern: &[u8], trailing: bool) -> Vec<&'a [u8]> {
    let found = if trailing {
        rfind_bytes(input, pattern)
    } else {
        find_bytes(input, pattern)
    };
    if let Some(index) = found {
        vec![&input[..index], &input[index + pattern.len()..]]
    } else {
        vec![input]
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn rfind_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(haystack.len());
    }
    haystack
        .windows(needle.len())
        .rposition(|window| window == needle)
}

fn replace_all(input: &[u8], pattern: &[u8], replacement: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut index = 0;
    while let Some(relative) = find_bytes(&input[index..], pattern) {
        let match_start = index + relative;
        out.extend_from_slice(&input[index..match_start]);
        out.extend_from_slice(replacement);
        index = match_start + pattern.len();
    }
    out.extend_from_slice(&input[index..]);
    out
}

fn replace_once(input: &[u8], pattern: &[u8], replacement: &[u8], trailing: bool) -> Vec<u8> {
    let found = if trailing {
        rfind_bytes(input, pattern)
    } else {
        find_bytes(input, pattern)
    };
    if let Some(index) = found {
        let mut out = Vec::with_capacity(input.len() - pattern.len() + replacement.len());
        out.extend_from_slice(&input[..index]);
        out.extend_from_slice(replacement);
        out.extend_from_slice(&input[index + pattern.len()..]);
        out
    } else {
        input.to_vec()
    }
}

fn append_pad(out: &mut Vec<u8>, pad: &[u8], count: usize) {
    for index in 0..count {
        out.push(pad[index % pad.len()]);
    }
}

fn non_negative_usize(term: Term) -> Result<usize, Term> {
    term.as_small_int()
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(badarg)
}

fn atom_term(name: &str, context: &mut ProcessContext) -> Result<Term, Term> {
    let table = context.atom_table().ok_or_else(badarg)?;
    Ok(Term::atom(table.intern(name)))
}

fn atom_name<'a>(term: Term, context: &'a ProcessContext<'_>) -> Result<&'a str, Term> {
    let atom = term.as_atom().ok_or_else(badarg)?;
    if let Some(name) = context.atom_table().and_then(|table| table.resolve(atom)) {
        return Ok(name);
    }
    if atom == Atom::OK {
        Ok("ok")
    } else if atom == Atom::ERROR {
        Ok("error")
    } else if atom == Atom::TRUE {
        Ok("true")
    } else if atom == Atom::FALSE {
        Ok("false")
    } else {
        Err(badarg())
    }
}

fn binary_bytes(term: Term) -> Result<&'static [u8], Term> {
    Binary::new(term).map(Binary::as_bytes).ok_or_else(badarg)
}

fn bool_term(value: bool) -> Term {
    Term::atom(if value { Atom::TRUE } else { Atom::FALSE })
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}
