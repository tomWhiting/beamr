//! External Term Format (ETF) literal decoder for `LitT` chunk entries.
//!
//! Extracted from `chunks.rs` so the recursive decoder and its hardening
//! helpers live in one focused unit. Every recursive descent is depth-bounded
//! (`MAX_ETF_DEPTH`) and every length-prefixed preallocation is validated
//! against the remaining input before any `Vec::with_capacity`, so a crafted
//! `.beam` literal table cannot stack-overflow or OOM the loader.

use super::chunks::{Cursor, Literal};
use super::{MAX_ETF_DEPTH, MAX_TABLE_ENTRIES};
use crate::atom::AtomTable;
use crate::error::LoadError;

/// Decode one ETF term starting at the cursor. `depth` is the current nesting
/// level; recursive arms increment it and reject once `MAX_ETF_DEPTH` is
/// exceeded.
pub(super) fn decode_external_term(
    cursor: &mut Cursor<'_>,
    atom_table: &AtomTable,
    depth: usize,
) -> Result<Literal, LoadError> {
    if depth > MAX_ETF_DEPTH {
        return Err(nesting_limit_error());
    }
    let tag = cursor.read_u8()?;
    match tag {
        70 => {
            let bytes = cursor.read_bytes(8)?;
            Ok(Literal::Float(f64::from_bits(u64::from_be_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            ]))))
        }
        97 => Ok(Literal::Integer(i64::from(cursor.read_u8()?))),
        98 => Ok(Literal::Integer(i64::from(cursor.read_i32()?))),
        100 | 118 => {
            let len = usize::from(cursor.read_u16()?);
            let bytes = cursor.read_bytes(len)?;
            decode_atom_literal(bytes, atom_table)
        }
        119 => {
            let len = usize::from(cursor.read_u8()?);
            let bytes = cursor.read_bytes(len)?;
            decode_atom_literal(bytes, atom_table)
        }
        104 => {
            let arity = usize::from(cursor.read_u8()?);
            decode_tuple(cursor, arity, atom_table, depth)
        }
        105 => {
            let arity = cursor.read_u32()? as usize;
            decode_tuple(cursor, arity, atom_table, depth)
        }
        106 => Ok(Literal::Nil),
        107 => {
            let len = usize::from(cursor.read_u16()?);
            Ok(Literal::String(cursor.read_bytes(len)?.to_vec()))
        }
        108 => {
            let len = cursor.read_u32()? as usize;
            // Each list element is at least one tag byte, so a count larger
            // than the remaining input is impossible — reject before allocating.
            ensure_count(len, cursor.remaining().len(), "ETF list length")?;
            let child_depth = next_depth(depth)?;
            let mut elements = Vec::with_capacity(len);
            for _ in 0..len {
                elements.push(decode_external_term(cursor, atom_table, child_depth)?);
            }
            let tail = decode_external_term(cursor, atom_table, child_depth)?;
            Ok(Literal::List(elements, Box::new(tail)))
        }
        109 => {
            let len = cursor.read_u32()? as usize;
            Ok(Literal::Binary(cursor.read_bytes(len)?.to_vec()))
        }
        110 | 111 => decode_big_integer(cursor, tag),
        113 => {
            // EXPORT_EXT: fun Module:Function/Arity encoded as
            // tag(113) + Module(atom_ext) + Function(atom_ext) + Arity(small_integer_ext).
            // Decoded as a 3-tuple {Module, Function, Arity}.
            let child_depth = next_depth(depth)?;
            let module = decode_external_term(cursor, atom_table, child_depth)?;
            let function = decode_external_term(cursor, atom_table, child_depth)?;
            let arity = decode_external_term(cursor, atom_table, child_depth)?;
            Ok(Literal::Tuple(vec![module, function, arity]))
        }
        116 => {
            let len = cursor.read_u32()? as usize;
            // Each map entry is at least two tag bytes (key + value).
            ensure_count(len, cursor.remaining().len() / 2, "ETF map size")?;
            let child_depth = next_depth(depth)?;
            let mut pairs = Vec::with_capacity(len);
            for _ in 0..len {
                let key = decode_external_term(cursor, atom_table, child_depth)?;
                let value = decode_external_term(cursor, atom_table, child_depth)?;
                pairs.push((key, value));
            }
            Ok(Literal::Map(pairs))
        }
        other => Err(LoadError::DecodeError(format!(
            "unsupported ETF literal tag {other}"
        ))),
    }
}

fn decode_atom_literal(bytes: &[u8], atom_table: &AtomTable) -> Result<Literal, LoadError> {
    let name = std::str::from_utf8(bytes)
        .map_err(|_| LoadError::DecodeError("ETF atom is not valid UTF-8".into()))?;
    Ok(Literal::Atom(atom_table.intern(name)))
}

fn decode_tuple(
    cursor: &mut Cursor<'_>,
    arity: usize,
    atom_table: &AtomTable,
    depth: usize,
) -> Result<Literal, LoadError> {
    // Each element is at least one tag byte.
    ensure_count(arity, cursor.remaining().len(), "ETF tuple arity")?;
    let child_depth = next_depth(depth)?;
    let mut elements = Vec::with_capacity(arity);
    for _ in 0..arity {
        elements.push(decode_external_term(cursor, atom_table, child_depth)?);
    }
    Ok(Literal::Tuple(elements))
}

fn decode_big_integer(cursor: &mut Cursor<'_>, tag: u8) -> Result<Literal, LoadError> {
    let len = if tag == 110 {
        usize::from(cursor.read_u8()?)
    } else {
        cursor.read_u32()? as usize
    };
    let sign = cursor.read_u8()?;
    let bytes = cursor.read_bytes(len)?;
    if len <= 8 {
        let mut value: i128 = 0;
        for (shift, byte) in bytes.iter().enumerate() {
            value += i128::from(*byte) << (shift * 8);
        }
        if sign == 1 {
            value = -value;
        } else if sign != 0 {
            return Err(LoadError::DecodeError(format!(
                "invalid bignum sign {sign}"
            )));
        }
        i64::try_from(value)
            .map(Literal::Integer)
            .map_err(|_| LoadError::DecodeError(format!("ETF bignum {value} is outside i64 range")))
    } else {
        let mut out = Vec::with_capacity(len + 1);
        out.push(sign);
        out.extend_from_slice(bytes);
        Ok(Literal::BigInteger(out))
    }
}

/// Increment a recursion depth, refusing once it would exceed `MAX_ETF_DEPTH`.
fn next_depth(depth: usize) -> Result<usize, LoadError> {
    if depth >= MAX_ETF_DEPTH {
        return Err(nesting_limit_error());
    }
    Ok(depth + 1)
}

/// Validate a length-prefixed count read from untrusted bytes before it is used
/// to preallocate. `feasible` is the maximum number of elements the remaining
/// input could possibly contain (remaining bytes / min bytes-per-element).
fn ensure_count(count: usize, feasible: usize, label: &str) -> Result<(), LoadError> {
    if count > MAX_TABLE_ENTRIES {
        return Err(LoadError::DecodeError(format!("{label} exceeds limit")));
    }
    if count > feasible {
        return Err(LoadError::DecodeError(format!(
            "{label} impossible for payload size"
        )));
    }
    Ok(())
}

fn nesting_limit_error() -> LoadError {
    LoadError::DecodeError("ETF nesting exceeds limit".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(bytes: &[u8]) -> Result<Literal, LoadError> {
        let atoms = AtomTable::with_common_atoms();
        let mut cursor = Cursor::new(bytes);
        decode_external_term(&mut cursor, &atoms, 0)
    }

    /// Build a term that nests single-element lists `levels` deep, terminated
    /// by NIL tails. Each level is `108` + a 4-byte length of `1`.
    fn nested_single_element_lists(levels: usize) -> Vec<u8> {
        let mut bytes = Vec::new();
        for _ in 0..levels {
            bytes.push(108);
            bytes.extend_from_slice(&1u32.to_be_bytes());
        }
        // innermost element + one tail per opened list
        bytes.push(106);
        bytes.resize(bytes.len() + levels, 106);
        bytes
    }

    #[test]
    fn deeply_nested_list_rejected_not_stack_overflow() {
        // Far past the depth limit: a 2-byte-per-level overflow attempt.
        let bytes = nested_single_element_lists(MAX_ETF_DEPTH + 16);
        match decode(&bytes) {
            Err(LoadError::DecodeError(message)) => {
                assert!(
                    message.contains("nesting exceeds limit"),
                    "unexpected error: {message}"
                );
            }
            other => panic!("expected nesting-limit DecodeError, got {other:?}"),
        }
    }

    #[test]
    fn shallow_nesting_still_decodes() {
        // A list one level deep must still round-trip.
        let bytes = [108u8, 0, 0, 0, 1, 97, 7, 106];
        assert_eq!(
            decode(&bytes),
            Ok(Literal::List(
                vec![Literal::Integer(7)],
                Box::new(Literal::Nil)
            ))
        );
    }

    #[test]
    fn list_count_impossible_for_payload_rejected_before_alloc() {
        // tag 108 (LIST) claims 0xFFFF_FFFF elements but body is empty.
        let bytes = [108u8, 0xFF, 0xFF, 0xFF, 0xFF];
        match decode(&bytes) {
            Err(LoadError::DecodeError(message)) => {
                assert!(
                    message.contains("exceeds limit") || message.contains("impossible"),
                    "unexpected error: {message}"
                );
            }
            other => panic!("expected count-limit DecodeError, got {other:?}"),
        }
    }

    #[test]
    fn tuple_arity_impossible_for_payload_rejected_before_alloc() {
        // tag 105 (LARGE_TUPLE) claims a huge arity with no body.
        let bytes = [105u8, 0xFF, 0xFF, 0xFF, 0xFF];
        assert!(matches!(decode(&bytes), Err(LoadError::DecodeError(_))));
    }

    #[test]
    fn map_size_impossible_for_payload_rejected_before_alloc() {
        let bytes = [116u8, 0xFF, 0xFF, 0xFF, 0xFF];
        assert!(matches!(decode(&bytes), Err(LoadError::DecodeError(_))));
    }
}
