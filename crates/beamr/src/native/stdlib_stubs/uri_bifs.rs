//! `uri_string` module natives.
//!
//! These serve the real `gleam_stdlib.erl` bytecode (`uri_parse`,
//! `parse_query`), so their contracts must match Erlang/OTP exactly:
//! `parse/1` returns a map containing only the components present in the
//! input (with an integer `port`), and `dissect_query/1` returns a list of
//! `{Key, Value}` pairs with `true` for valueless keys, decoding
//! `application/x-www-form-urlencoded` escapes.

use crate::atom::Atom;
use crate::native::ProcessContext;
use crate::term::Term;
use crate::term::binary_ref::BinaryRef;

/// `uri_string:parse/1` over a UTF-8 binary, RFC 3986 component split.
pub fn bif_uri_string_parse(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [input] = args else {
        return Err(badarg());
    };
    let text = binary_text(*input)?;

    let (rest, fragment) = match text.split_once('#') {
        Some((rest, fragment)) => (rest, Some(fragment)),
        None => (text, None),
    };
    let (rest, query) = match rest.split_once('?') {
        Some((rest, query)) => (rest, Some(query)),
        None => (rest, None),
    };
    let (scheme, rest) = split_scheme(rest);
    let (authority, path) = split_authority(rest);
    let mut userinfo = None;
    let mut host = None;
    let mut port = None;
    if let Some(authority) = authority {
        let (user_part, host_part) = match authority.split_once('@') {
            Some((user, host)) => (Some(user), host),
            None => (None, authority),
        };
        userinfo = user_part;
        let (host_text, port_text) = split_host_port(host_part);
        host = Some(host_text);
        if let Some(port_text) = port_text {
            // OTP keeps an empty port as `port => undefined` and rejects a
            // non-numeric one outright.
            if port_text.is_empty() {
                port = Some(PortComponent::Undefined);
            } else if port_text.chars().all(|ch| ch.is_ascii_digit()) {
                match port_text.parse::<u16>() {
                    Ok(value) => port = Some(PortComponent::Number(value)),
                    Err(_) => return error_tuple(context, "invalid_uri", ":"),
                }
            } else {
                return error_tuple(context, "invalid_uri", ":");
            }
        }
    }

    let mut keys = Vec::new();
    let mut values = Vec::new();
    if let Some(scheme) = scheme {
        keys.push(atom(context, "scheme")?);
        values.push(context.alloc_binary(scheme.as_bytes())?);
    }
    if let Some(userinfo) = userinfo {
        keys.push(atom(context, "userinfo")?);
        values.push(context.alloc_binary(userinfo.as_bytes())?);
    }
    if let Some(host) = host {
        keys.push(atom(context, "host")?);
        values.push(context.alloc_binary(host.as_bytes())?);
    }
    if let Some(port) = port {
        keys.push(atom(context, "port")?);
        values.push(match port {
            PortComponent::Number(value) => {
                Term::try_small_int(i64::from(value)).ok_or_else(badarg)?
            }
            PortComponent::Undefined => atom(context, "undefined")?,
        });
    }
    keys.push(atom(context, "path")?);
    values.push(context.alloc_binary(path.as_bytes())?);
    if let Some(query) = query {
        keys.push(atom(context, "query")?);
        values.push(context.alloc_binary(query.as_bytes())?);
    }
    if let Some(fragment) = fragment {
        keys.push(atom(context, "fragment")?);
        values.push(context.alloc_binary(fragment.as_bytes())?);
    }
    context.alloc_map(&keys, &values)
}

/// `uri_string:dissect_query/1` decoding `application/x-www-form-urlencoded`.
pub fn bif_uri_string_dissect_query(
    args: &[Term],
    context: &mut ProcessContext,
) -> Result<Term, Term> {
    let [input] = args else {
        return Err(badarg());
    };
    let text = binary_text(*input)?;
    if text.is_empty() {
        return Ok(Term::NIL);
    }

    let mut pairs: Vec<(Vec<u8>, Option<Vec<u8>>)> = Vec::new();
    for part in text.split('&') {
        match part.split_once('=') {
            Some((key, value)) => {
                let (Some(key), Some(value)) = (form_decode(key), form_decode(value)) else {
                    return error_tuple(context, "invalid_query", part);
                };
                pairs.push((key, Some(value)));
            }
            None => {
                let Some(key) = form_decode(part) else {
                    return error_tuple(context, "invalid_query", part);
                };
                pairs.push((key, None));
            }
        }
    }

    let mut terms = Vec::with_capacity(pairs.len());
    for (key, value) in pairs {
        let key = context.alloc_binary(&key)?;
        let value = match value {
            Some(bytes) => context.alloc_binary(&bytes)?,
            None => Term::atom(Atom::TRUE),
        };
        terms.push(context.alloc_tuple(&[key, value])?);
    }
    context.alloc_list(&terms)
}

/// `maps:get/2` raising `{badkey, Key}` for missing keys, matching the BIF.
pub fn bif_maps_get_2(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [key, map_term] = args else {
        return Err(badarg());
    };
    let map = crate::term::boxed::Map::new(*map_term).ok_or_else(badarg)?;
    match map.get(*key) {
        Some(value) => Ok(value),
        None => {
            let badkey = atom(context, "badkey")?;
            Err(context.alloc_tuple(&[badkey, *key])?)
        }
    }
}

/// `maps:get/3` returning the default for missing keys.
pub fn bif_maps_get_3(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [key, map_term, default] = args else {
        return Err(badarg());
    };
    let map = crate::term::boxed::Map::new(*map_term).ok_or_else(badarg)?;
    Ok(map.get(*key).unwrap_or(*default))
}

/// A parsed authority port: numeric, or present-but-empty (`undefined`).
enum PortComponent {
    Number(u16),
    Undefined,
}

/// Splits a leading `scheme:` when the scheme is RFC 3986-valid.
fn split_scheme(text: &str) -> (Option<&str>, &str) {
    let Some(colon) = text.find(':') else {
        return (None, text);
    };
    let candidate = &text[..colon];
    if candidate.is_empty() {
        return (None, text);
    }
    let mut chars = candidate.chars();
    let valid_first = chars.next().is_some_and(|ch| ch.is_ascii_alphabetic());
    let valid_rest =
        chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '+' || ch == '-' || ch == '.');
    // A '/' or '?' before the colon means it belongs to the path, not a scheme.
    if valid_first && valid_rest && !candidate.contains('/') {
        (Some(candidate), &text[colon + 1..])
    } else {
        (None, text)
    }
}

/// Splits `//authority` from the path remainder.
fn split_authority(text: &str) -> (Option<&str>, &str) {
    let Some(after) = text.strip_prefix("//") else {
        return (None, text);
    };
    match after.find('/') {
        Some(slash) => (Some(&after[..slash]), &after[slash..]),
        None => (Some(after), ""),
    }
}

/// Splits `host[:port]`, honouring IPv6 bracket notation.
fn split_host_port(text: &str) -> (&str, Option<&str>) {
    if let Some(rest) = text.strip_prefix('[')
        && let Some(close) = rest.find(']')
    {
        let host = &rest[..close];
        let remainder = &rest[close + 1..];
        return match remainder.strip_prefix(':') {
            Some(port) => (host, Some(port)),
            None => (host, None),
        };
    }
    match text.rsplit_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (text, None),
    }
}

/// Decodes a form-urlencoded component (`+` as space, `%XX` escapes).
fn form_decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            b'%' => {
                let high = hex_value(*bytes.get(index + 1)?)?;
                let low = hex_value(*bytes.get(index + 2)?)?;
                out.push(high * 16 + low);
                index += 3;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    Some(out)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn error_tuple(context: &mut ProcessContext, reason: &str, detail: &str) -> Result<Term, Term> {
    let error = Term::atom(Atom::ERROR);
    let reason = atom(context, reason)?;
    let detail = context.alloc_binary(detail.as_bytes())?;
    context.alloc_tuple(&[error, reason, detail])
}

fn binary_text(term: Term) -> Result<&'static str, Term> {
    let binary = BinaryRef::new(term).ok_or_else(badarg)?;
    std::str::from_utf8(binary.as_bytes()).map_err(|_| badarg())
}

fn atom(context: &mut ProcessContext, name: &str) -> Result<Term, Term> {
    let table = context.atom_table().ok_or_else(badarg)?;
    Ok(Term::atom(table.intern(name)))
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}
