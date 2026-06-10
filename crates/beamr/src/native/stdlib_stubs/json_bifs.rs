//! OTP 27 `json` module BIFs.
//!
//! `gleam_json`'s Erlang FFI (`gleam_json_ffi`) delegates to
//! `json:decode/1`, `json:encode_integer/1`, `json:encode_float/1`, and
//! `json:encode_binary/1`, so these must exist for any Gleam program that
//! uses JSON. Decode errors follow the OTP `json` error contract —
//! `unexpected_end`, `{invalid_byte, Byte}`, `{unexpected_sequence, Bin}` —
//! because `gleam_json_ffi` pattern-matches those exact reasons.

use crate::atom::Atom;
use crate::native::ProcessContext;
use crate::native::bifs::integer_result;
use crate::term::Term;
use crate::term::bigint_convert;
use crate::term::binary_ref::BinaryRef;
use crate::term::boxed::{Cons, Float, Map};

/// `json:encode_integer/1` — integer (small or bignum) to decimal binary.
pub fn bif_json_encode_integer(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [term] = args else {
        return Err(badarg());
    };
    let text = bigint_convert::integer_term_to_string_radix(*term, 10).ok_or_else(badarg)?;
    context.alloc_binary(text.as_bytes())
}

/// `json:encode_float/1` — finite float to its shortest decimal binary.
pub fn bif_json_encode_float(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [term] = args else {
        return Err(badarg());
    };
    let value = Float::new(*term).ok_or_else(badarg)?.value();
    if !value.is_finite() {
        return Err(badarg());
    }
    context.alloc_binary(format_float_shortest(value).as_bytes())
}

/// `json:encode_binary/1` — UTF-8 binary to a quoted, escaped JSON string.
pub fn bif_json_encode_binary(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [term] = args else {
        return Err(badarg());
    };
    let binary = BinaryRef::new(*term).ok_or_else(badarg)?;
    if std::str::from_utf8(binary.as_bytes()).is_err() {
        return Err(badarg());
    }
    let escaped = escape_json_string(binary.as_bytes());
    context.alloc_binary(&escaped)
}

/// `json:encode/1` — encode a term (maps, lists, binaries, numbers, atoms)
/// as a JSON binary.
pub fn bif_json_encode(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [term] = args else {
        return Err(badarg());
    };
    let mut out = Vec::new();
    encode_term(*term, context, &mut out)?;
    context.alloc_binary(&out)
}

/// `json:decode/1` — parse a JSON binary into terms (objects as maps with
/// binary keys, `true`/`false`/`null` as atoms), rejecting trailing input.
pub fn bif_json_decode(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [term] = args else {
        return Err(badarg());
    };
    let binary = BinaryRef::new(*term).ok_or_else(badarg)?;
    // Own the bytes: parsing allocates, and a collection may move the
    // input binary's heap data while a borrow would still be live.
    let bytes = binary.as_bytes().to_vec();
    let mut parser = Parser {
        bytes: &bytes,
        pos: 0,
    };
    let value = parser.parse_value(context)?;
    parser.skip_whitespace();
    if parser.pos < parser.bytes.len() {
        return Err(parser.invalid_byte_here(context));
    }
    Ok(value)
}

fn format_float_shortest(value: f64) -> String {
    let mut text = format!("{value}");
    if !text.contains(['.', 'e', 'E']) {
        text.push_str(".0");
    }
    text
}

fn escape_json_string(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + 2);
    out.push(b'"');
    for &byte in bytes {
        match byte {
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            0x08 => out.extend_from_slice(b"\\b"),
            0x09 => out.extend_from_slice(b"\\t"),
            0x0A => out.extend_from_slice(b"\\n"),
            0x0C => out.extend_from_slice(b"\\f"),
            0x0D => out.extend_from_slice(b"\\r"),
            byte if byte < 0x20 => {
                out.extend_from_slice(format!("\\u{byte:04X}").as_bytes());
            }
            byte => out.push(byte),
        }
    }
    out.push(b'"');
    out
}

fn encode_term(term: Term, context: &mut ProcessContext, out: &mut Vec<u8>) -> Result<(), Term> {
    if let Some(atom) = term.as_atom() {
        match atom {
            Atom::TRUE => out.extend_from_slice(b"true"),
            Atom::FALSE => out.extend_from_slice(b"false"),
            _ => {
                let table = context.atom_table_arc().ok_or_else(badarg)?;
                let name = table.resolve(atom).ok_or_else(badarg)?;
                if name == "null" {
                    out.extend_from_slice(b"null");
                } else {
                    out.extend_from_slice(&escape_json_string(name.as_bytes()));
                }
            }
        }
        return Ok(());
    }
    if let Some(text) = bigint_convert::integer_term_to_string_radix(term, 10) {
        out.extend_from_slice(text.as_bytes());
        return Ok(());
    }
    if let Some(float) = Float::new(term) {
        if !float.value().is_finite() {
            return Err(badarg());
        }
        out.extend_from_slice(format_float_shortest(float.value()).as_bytes());
        return Ok(());
    }
    if let Some(binary) = BinaryRef::new(term) {
        if std::str::from_utf8(binary.as_bytes()).is_err() {
            return Err(badarg());
        }
        out.extend_from_slice(&escape_json_string(binary.as_bytes()));
        return Ok(());
    }
    if term.is_nil() {
        out.extend_from_slice(b"[]");
        return Ok(());
    }
    if Cons::new(term).is_some() {
        out.push(b'[');
        let mut current = term;
        let mut first = true;
        while let Some(cons) = Cons::new(current) {
            if !first {
                out.push(b',');
            }
            first = false;
            encode_term(cons.head(), context, out)?;
            current = cons.tail();
        }
        if !current.is_nil() {
            return Err(badarg());
        }
        out.push(b']');
        return Ok(());
    }
    if let Some(map) = Map::new(term) {
        out.push(b'{');
        for index in 0..map.len() {
            if index > 0 {
                out.push(b',');
            }
            let key = map.key(index).ok_or_else(badarg)?;
            let value = map.value(index).ok_or_else(badarg)?;
            encode_object_key(key, context, out)?;
            out.push(b':');
            encode_term(value, context, out)?;
        }
        out.push(b'}');
        return Ok(());
    }
    Err(badarg())
}

fn encode_object_key(
    key: Term,
    context: &mut ProcessContext,
    out: &mut Vec<u8>,
) -> Result<(), Term> {
    if let Some(binary) = BinaryRef::new(key) {
        out.extend_from_slice(&escape_json_string(binary.as_bytes()));
        return Ok(());
    }
    if let Some(atom) = key.as_atom() {
        let table = context.atom_table_arc().ok_or_else(badarg)?;
        let name = table.resolve(atom).ok_or_else(badarg)?.to_owned();
        out.extend_from_slice(&escape_json_string(name.as_bytes()));
        return Ok(());
    }
    if let Some(text) = bigint_convert::integer_term_to_string_radix(key, 10) {
        out.extend_from_slice(&escape_json_string(text.as_bytes()));
        return Ok(());
    }
    Err(badarg())
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn skip_whitespace(&mut self) {
        while let Some(&byte) = self.bytes.get(self.pos) {
            if matches!(byte, b' ' | b'\t' | b'\n' | b'\r') {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn unexpected_end(&self, context: &mut ProcessContext) -> Term {
        let Some(table) = context.atom_table_arc() else {
            return badarg();
        };
        Term::atom(table.intern("unexpected_end"))
    }

    fn invalid_byte_here(&self, context: &mut ProcessContext) -> Term {
        let byte = self.bytes.get(self.pos).copied().unwrap_or(0);
        let Some(table) = context.atom_table_arc() else {
            return badarg();
        };
        let tag = Term::atom(table.intern("invalid_byte"));
        context
            .alloc_tuple(&[tag, Term::small_int(i64::from(byte))])
            .unwrap_or_else(|error| error)
    }

    fn unexpected_sequence(&self, context: &mut ProcessContext, sequence: &[u8]) -> Term {
        let Some(table) = context.atom_table_arc() else {
            return badarg();
        };
        let tag = Term::atom(table.intern("unexpected_sequence"));
        let binary = match context.alloc_binary(sequence) {
            Ok(binary) => binary,
            Err(error) => return error,
        };
        context
            .alloc_tuple(&[tag, binary])
            .unwrap_or_else(|error| error)
    }

    fn parse_value(&mut self, context: &mut ProcessContext) -> Result<Term, Term> {
        self.skip_whitespace();
        let Some(&byte) = self.bytes.get(self.pos) else {
            return Err(self.unexpected_end(context));
        };
        match byte {
            b'{' => self.parse_object(context),
            b'[' => self.parse_array(context),
            b'"' => self.parse_string(context),
            b'-' | b'0'..=b'9' => self.parse_number(context),
            b't' => self.parse_literal(context, b"true", Term::atom(Atom::TRUE)),
            b'f' => self.parse_literal(context, b"false", Term::atom(Atom::FALSE)),
            b'n' => {
                let table = context.atom_table_arc().ok_or_else(badarg)?;
                let null = Term::atom(table.intern("null"));
                self.parse_literal(context, b"null", null)
            }
            _ => Err(self.invalid_byte_here(context)),
        }
    }

    fn parse_literal(
        &mut self,
        context: &mut ProcessContext,
        literal: &[u8],
        value: Term,
    ) -> Result<Term, Term> {
        if self.bytes[self.pos..].starts_with(literal) {
            self.pos += literal.len();
            Ok(value)
        } else if self.bytes.len() - self.pos < literal.len() {
            Err(self.unexpected_end(context))
        } else {
            Err(self.invalid_byte_here(context))
        }
    }

    fn parse_array(&mut self, context: &mut ProcessContext) -> Result<Term, Term> {
        self.pos += 1;
        self.skip_whitespace();
        if self.bytes.get(self.pos) == Some(&b']') {
            self.pos += 1;
            return Ok(Term::NIL);
        }
        context.with_rooted(&[], |context, roots| {
            loop {
                let value = self.parse_value(context)?;
                context.rooted_push(roots, value)?;
                self.skip_whitespace();
                match self.bytes.get(self.pos) {
                    Some(b',') => {
                        self.pos += 1;
                    }
                    Some(b']') => {
                        self.pos += 1;
                        break;
                    }
                    Some(_) => return Err(self.invalid_byte_here(context)),
                    None => return Err(self.unexpected_end(context)),
                }
            }
            let count = context.rooted_len(roots);
            context.ensure_heap_space(count * 2)?;
            let mut tail = Term::NIL;
            for index in (0..count).rev() {
                let element = context.rooted(roots, index)?;
                tail = context.alloc_cons_prereserved(element, tail)?;
            }
            Ok(tail)
        })
    }

    fn parse_object(&mut self, context: &mut ProcessContext) -> Result<Term, Term> {
        self.pos += 1;
        self.skip_whitespace();
        if self.bytes.get(self.pos) == Some(&b'}') {
            self.pos += 1;
            return context.alloc_map(&[], &[]);
        }
        context.with_rooted(&[], |context, roots| {
            loop {
                self.skip_whitespace();
                if self.bytes.get(self.pos) != Some(&b'"') {
                    return Err(match self.bytes.get(self.pos) {
                        Some(_) => self.invalid_byte_here(context),
                        None => self.unexpected_end(context),
                    });
                }
                let key = self.parse_string(context)?;
                context.rooted_push(roots, key)?;
                self.skip_whitespace();
                if self.bytes.get(self.pos) != Some(&b':') {
                    return Err(match self.bytes.get(self.pos) {
                        Some(_) => self.invalid_byte_here(context),
                        None => self.unexpected_end(context),
                    });
                }
                self.pos += 1;
                let value = self.parse_value(context)?;
                context.rooted_push(roots, value)?;
                self.skip_whitespace();
                match self.bytes.get(self.pos) {
                    Some(b',') => {
                        self.pos += 1;
                    }
                    Some(b'}') => {
                        self.pos += 1;
                        break;
                    }
                    Some(_) => return Err(self.invalid_byte_here(context)),
                    None => return Err(self.unexpected_end(context)),
                }
            }
            let pair_count = context.rooted_len(roots) / 2;
            let mut keys = Vec::with_capacity(pair_count);
            let mut values = Vec::with_capacity(pair_count);
            for index in 0..pair_count {
                keys.push(context.rooted(roots, index * 2)?);
                values.push(context.rooted(roots, index * 2 + 1)?);
            }
            context.alloc_map(&keys, &values)
        })
    }

    fn parse_string(&mut self, context: &mut ProcessContext) -> Result<Term, Term> {
        let start = self.pos;
        self.pos += 1;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let Some(&byte) = self.bytes.get(self.pos) else {
                return Err(self.unexpected_end(context));
            };
            match byte {
                b'"' => {
                    self.pos += 1;
                    if std::str::from_utf8(&out).is_err() {
                        let sequence = self.bytes[start..self.pos].to_vec();
                        return Err(self.unexpected_sequence(context, &sequence));
                    }
                    return context.alloc_binary(&out);
                }
                b'\\' => {
                    self.pos += 1;
                    let Some(&escape) = self.bytes.get(self.pos) else {
                        return Err(self.unexpected_end(context));
                    };
                    match escape {
                        b'"' => out.push(b'"'),
                        b'\\' => out.push(b'\\'),
                        b'/' => out.push(b'/'),
                        b'b' => out.push(0x08),
                        b'f' => out.push(0x0C),
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'u' => {
                            let ch = self.parse_unicode_escape(context)?;
                            let mut buffer = [0u8; 4];
                            out.extend_from_slice(ch.encode_utf8(&mut buffer).as_bytes());
                            continue;
                        }
                        _ => return Err(self.invalid_byte_here(context)),
                    }
                    self.pos += 1;
                }
                byte if byte < 0x20 => return Err(self.invalid_byte_here(context)),
                byte => {
                    out.push(byte);
                    self.pos += 1;
                }
            }
        }
    }

    /// Parses the `XXXX` of a `\uXXXX` escape with `self.pos` on the `u`,
    /// combining UTF-16 surrogate pairs; leaves `self.pos` past the escape.
    fn parse_unicode_escape(&mut self, context: &mut ProcessContext) -> Result<char, Term> {
        let escape_start = self.pos - 1;
        let high = self.parse_hex4(context)?;
        if (0xD800..=0xDBFF).contains(&high) {
            if self.bytes.get(self.pos) == Some(&b'\\')
                && self.bytes.get(self.pos + 1) == Some(&b'u')
            {
                self.pos += 1;
                let low = self.parse_hex4(context)?;
                if (0xDC00..=0xDFFF).contains(&low) {
                    let combined =
                        0x10000 + ((u32::from(high) - 0xD800) << 10) + (u32::from(low) - 0xDC00);
                    if let Some(ch) = char::from_u32(combined) {
                        return Ok(ch);
                    }
                }
            }
            let sequence = self.bytes[escape_start..self.pos.min(self.bytes.len())].to_vec();
            return Err(self.unexpected_sequence(context, &sequence));
        }
        if (0xDC00..=0xDFFF).contains(&high) {
            let sequence = self.bytes[escape_start..self.pos.min(self.bytes.len())].to_vec();
            return Err(self.unexpected_sequence(context, &sequence));
        }
        char::from_u32(u32::from(high)).ok_or_else(|| {
            let sequence = self.bytes[escape_start..self.pos.min(self.bytes.len())].to_vec();
            self.unexpected_sequence(context, &sequence)
        })
    }

    /// Parses four hex digits with `self.pos` on the `u`; leaves `self.pos`
    /// past the last digit.
    fn parse_hex4(&mut self, context: &mut ProcessContext) -> Result<u16, Term> {
        self.pos += 1;
        if self.pos + 4 > self.bytes.len() {
            self.pos = self.bytes.len();
            return Err(self.unexpected_end(context));
        }
        let mut value: u16 = 0;
        for _ in 0..4 {
            let byte = self.bytes[self.pos];
            let digit = char::from(byte)
                .to_digit(16)
                .ok_or_else(|| self.invalid_byte_here(context))?;
            value = (value << 4) | u16::try_from(digit).expect("hex digit fits in u16");
            self.pos += 1;
        }
        Ok(value)
    }

    fn parse_number(&mut self, context: &mut ProcessContext) -> Result<Term, Term> {
        let start = self.pos;
        if self.bytes.get(self.pos) == Some(&b'-') {
            self.pos += 1;
        }
        match self.bytes.get(self.pos) {
            Some(b'0') => self.pos += 1,
            Some(b'1'..=b'9') => {
                while matches!(self.bytes.get(self.pos), Some(b'0'..=b'9')) {
                    self.pos += 1;
                }
            }
            Some(_) => return Err(self.invalid_byte_here(context)),
            None => return Err(self.unexpected_end(context)),
        }
        let mut is_float = false;
        if self.bytes.get(self.pos) == Some(&b'.') {
            is_float = true;
            self.pos += 1;
            if !matches!(self.bytes.get(self.pos), Some(b'0'..=b'9')) {
                return Err(match self.bytes.get(self.pos) {
                    Some(_) => self.invalid_byte_here(context),
                    None => self.unexpected_end(context),
                });
            }
            while matches!(self.bytes.get(self.pos), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        if matches!(self.bytes.get(self.pos), Some(b'e' | b'E')) {
            is_float = true;
            self.pos += 1;
            if matches!(self.bytes.get(self.pos), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if !matches!(self.bytes.get(self.pos), Some(b'0'..=b'9')) {
                return Err(match self.bytes.get(self.pos) {
                    Some(_) => self.invalid_byte_here(context),
                    None => self.unexpected_end(context),
                });
            }
            while matches!(self.bytes.get(self.pos), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        let text = std::str::from_utf8(&self.bytes[start..self.pos]).map_err(|_| badarg())?;
        if is_float {
            let value: f64 = text.parse().map_err(|_| badarg())?;
            return context.alloc_float(value);
        }
        let integer = bigint_convert::from_str_radix(text, 10).ok_or_else(badarg)?;
        integer_result(integer, context)
    }
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}
