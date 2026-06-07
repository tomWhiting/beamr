//! Approximate URI-related native stubs for Gleam stdlib and `uri_string`.

use crate::atom::{Atom, AtomTable};
use crate::native::ProcessContext;
use crate::term::Term;
use crate::term::binary_ref::BinaryRef;
use crate::term::compare;

pub fn bif_percent_encode(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [input] = args else {
        return Err(badarg());
    };
    let mut out = Vec::new();
    for byte in binary_bytes(*input)? {
        if is_unreserved(*byte) {
            out.push(*byte);
        } else {
            out.push(b'%');
            out.push(hex_digit(*byte >> 4));
            out.push(hex_digit(*byte & 0x0f));
        }
    }
    context.alloc_binary(&out)
}

pub fn bif_percent_decode(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [input] = args else {
        return Err(badarg());
    };
    let bytes = binary_bytes(*input)?;
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            out.push(bytes[index]);
            index += 1;
            continue;
        }
        let high = bytes.get(index + 1).and_then(|byte| hex_value(*byte));
        let low = bytes.get(index + 2).and_then(|byte| hex_value(*byte));
        let (Some(high), Some(low)) = (high, low) else {
            return Err(badarg());
        };
        out.push((high << 4) | low);
        index += 3;
    }
    context.alloc_binary(&out)
}

pub fn bif_uri_parse(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [input] = args else {
        return Err(badarg());
    };
    parse_uri_map(*input, context)
}

pub fn bif_uri_string_parse(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    bif_uri_parse(args, context)
}

pub fn bif_parse_query(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [input] = args else {
        return Err(badarg());
    };
    parse_query_map(*input, context)
}

pub fn bif_uri_string_dissect_query(
    args: &[Term],
    context: &mut ProcessContext,
) -> Result<Term, Term> {
    bif_parse_query(args, context)
}

fn parse_uri_map(input: Term, context: &mut ProcessContext) -> Result<Term, Term> {
    let text = binary_text(input)?;
    // Approximate RFC 3986 support for sample workflows: split only the common
    // scheme://host/path?query shape. Full URI parsing is intentionally deferred.
    let (scheme, after_scheme) = text.split_once("://").unwrap_or(("", text));
    let (authority_and_path, query) = after_scheme.split_once('?').unwrap_or((after_scheme, ""));
    let (host, path) = authority_and_path
        .split_once('/')
        .map(|(host, rest)| (host, format!("/{rest}")))
        .unwrap_or((authority_and_path, String::new()));

    let entries = [
        (
            atom(context, "scheme")?,
            context.alloc_binary(scheme.as_bytes())?,
        ),
        (
            atom(context, "host")?,
            context.alloc_binary(host.as_bytes())?,
        ),
        (
            atom(context, "path")?,
            context.alloc_binary(path.as_bytes())?,
        ),
        (
            atom(context, "query")?,
            context.alloc_binary(query.as_bytes())?,
        ),
    ];
    make_sorted_map(&entries, context)
}

fn parse_query_map(input: Term, context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let text = binary_text(input)?;
    // Approximate query support: split `a=b&c=d` into a deterministic flatmap
    // of binary keys to binary values; duplicate keys keep the last value.
    let mut entries: Vec<(Term, Term)> = Vec::new();
    for pair in text.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = context.alloc_binary(key.as_bytes())?;
        let value = context.alloc_binary(value.as_bytes())?;
        if let Some((_, existing)) = entries.iter_mut().find(|(entry_key, _)| *entry_key == key) {
            *existing = value;
        } else {
            entries.push((key, value));
        }
    }
    make_sorted_map(&entries, context)
}

fn is_unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~')
}

fn hex_digit(value: u8) -> u8 {
    match value {
        0..=9 => b'0' + value,
        10..=15 => b'A' + (value - 10),
        _ => b'0',
    }
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn atom(context: &mut ProcessContext, name: &str) -> Result<Term, Term> {
    if let Some(table) = context.atom_table() {
        return Ok(Term::atom(table.intern(name)));
    }
    let table = AtomTable::with_common_atoms();
    Ok(Term::atom(table.intern(name)))
}

fn make_sorted_map(entries: &[(Term, Term)], context: &mut ProcessContext) -> Result<Term, Term> {
    let atom_table = context.atom_table().ok_or_else(badarg)?;
    let mut sorted = entries.to_vec();
    sorted.sort_by(|(left, _), (right, _)| compare::cmp(*left, *right, atom_table));
    let keys: Vec<_> = sorted.iter().map(|(key, _)| *key).collect();
    let values: Vec<_> = sorted.iter().map(|(_, value)| *value).collect();
    context.alloc_map(&keys, &values)
}

fn binary_text(binary: Term) -> Result<&'static str, Term> {
    std::str::from_utf8(binary_bytes(binary)?).map_err(|_| badarg())
}

fn binary_bytes(term: Term) -> Result<&'static [u8], Term> {
    BinaryRef::new(term)
        .map(|binary| binary.as_bytes())
        .ok_or_else(badarg)
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}
