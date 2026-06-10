//! External Term Format (ETF) literal decoder for `LitT` chunk entries.
//!
//! Extracted from `chunks.rs` so the recursive decoder and its hardening
//! helpers live in one focused unit. Recursive descent, decoded nodes, and
//! allocations are budgeted so a crafted `.beam` literal table cannot
//! stack-overflow or OOM the loader.

use super::bounded::BoundedCursor;
use super::budget::DecodeBudget;
use super::chunks::Literal;
use crate::atom::AtomTable;
use crate::error::LoadError;

/// Decode one ETF term starting at the cursor.
pub(super) fn decode_external_term(
    cursor: &mut BoundedCursor<'_>,
    atom_table: &AtomTable,
    budget: &mut DecodeBudget,
) -> Result<Literal, LoadError> {
    budget.charge_node()?;
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
            decode_atom_literal(bytes, atom_table, budget)
        }
        119 => {
            let len = usize::from(cursor.read_u8()?);
            let bytes = cursor.read_bytes(len)?;
            decode_atom_literal(bytes, atom_table, budget)
        }
        104 => {
            let arity = usize::from(cursor.read_u8()?);
            decode_tuple(cursor, arity, atom_table, budget)
        }
        105 => {
            let arity = cursor.read_u32()? as usize;
            decode_tuple(cursor, arity, atom_table, budget)
        }
        106 => Ok(Literal::Nil),
        107 => {
            let len = usize::from(cursor.read_u16()?);
            budget.charge_bytes(len)?;
            Ok(Literal::String(cursor.read_bytes(len)?.to_vec()))
        }
        108 => {
            let len = cursor.read_u32()? as usize;
            // Each list element is at least one tag byte, so a count larger
            // than the remaining input is impossible — reject before allocating.
            cursor.ensure_count(len, 1, "ETF list length")?;
            budget.charge_bytes(len.checked_mul(std::mem::size_of::<Literal>()).ok_or_else(
                || LoadError::DecodeError("ETF list allocation size overflow".into()),
            )?)?;
            budget.descend()?;
            let result = (|| {
                let mut elements = Vec::with_capacity(len);
                for _ in 0..len {
                    elements.push(decode_external_term(cursor, atom_table, budget)?);
                }
                let tail = decode_external_term(cursor, atom_table, budget)?;
                Ok(Literal::List(elements, Box::new(tail)))
            })();
            budget.ascend();
            result
        }
        109 => {
            let len = cursor.read_u32()? as usize;
            budget.charge_bytes(len)?;
            Ok(Literal::Binary(cursor.read_bytes(len)?.to_vec()))
        }
        110 | 111 => decode_big_integer(cursor, tag, budget),
        113 => {
            // EXPORT_EXT: fun Module:Function/Arity encoded as
            // tag(113) + Module(atom_ext) + Function(atom_ext) + Arity(small_integer_ext).
            // Decoded as a 3-tuple {Module, Function, Arity}.
            budget.descend()?;
            let result = (|| {
                let module = decode_external_term(cursor, atom_table, budget)?;
                let function = decode_external_term(cursor, atom_table, budget)?;
                let arity = decode_external_term(cursor, atom_table, budget)?;
                budget.charge_bytes(3 * std::mem::size_of::<Literal>())?;
                Ok(Literal::Tuple(vec![module, function, arity]))
            })();
            budget.ascend();
            result
        }
        116 => {
            let len = cursor.read_u32()? as usize;
            // Each map entry is at least two tag bytes (key + value).
            cursor.ensure_count(len, 2, "ETF map size")?;
            budget.charge_bytes(
                len.checked_mul(std::mem::size_of::<(Literal, Literal)>())
                    .ok_or_else(|| {
                        LoadError::DecodeError("ETF map allocation size overflow".into())
                    })?,
            )?;
            budget.descend()?;
            let result = (|| {
                let mut pairs = Vec::with_capacity(len);
                for _ in 0..len {
                    let key = decode_external_term(cursor, atom_table, budget)?;
                    let value = decode_external_term(cursor, atom_table, budget)?;
                    pairs.push((key, value));
                }
                Ok(Literal::Map(pairs))
            })();
            budget.ascend();
            result
        }
        other => Err(LoadError::DecodeError(format!(
            "unsupported ETF literal tag {other}"
        ))),
    }
}

fn decode_atom_literal(
    bytes: &[u8],
    atom_table: &AtomTable,
    budget: &mut DecodeBudget,
) -> Result<Literal, LoadError> {
    let name = std::str::from_utf8(bytes)
        .map_err(|_| LoadError::DecodeError("ETF atom is not valid UTF-8".into()))?;
    let atom = match atom_table.lookup(name) {
        Some(atom) => atom,
        None => {
            budget.charge_atom()?;
            atom_table.intern(name)
        }
    };
    Ok(Literal::Atom(atom))
}

fn decode_tuple(
    cursor: &mut BoundedCursor<'_>,
    arity: usize,
    atom_table: &AtomTable,
    budget: &mut DecodeBudget,
) -> Result<Literal, LoadError> {
    // Each element is at least one tag byte.
    cursor.ensure_count(arity, 1, "ETF tuple arity")?;
    budget.charge_bytes(
        arity
            .checked_mul(std::mem::size_of::<Literal>())
            .ok_or_else(|| LoadError::DecodeError("ETF tuple allocation size overflow".into()))?,
    )?;
    budget.descend()?;
    let result = (|| {
        let mut elements = Vec::with_capacity(arity);
        for _ in 0..arity {
            elements.push(decode_external_term(cursor, atom_table, budget)?);
        }
        Ok(Literal::Tuple(elements))
    })();
    budget.ascend();
    result
}

fn decode_big_integer(
    cursor: &mut BoundedCursor<'_>,
    tag: u8,
    budget: &mut DecodeBudget,
) -> Result<Literal, LoadError> {
    let len = if tag == 110 {
        usize::from(cursor.read_u8()?)
    } else {
        cursor.read_u32()? as usize
    };
    let sign = cursor.read_u8()?;
    if sign > 1 {
        return Err(LoadError::DecodeError(format!(
            "invalid bignum sign {sign}"
        )));
    }
    let bytes = cursor.read_bytes(len)?;
    if len <= 8 {
        let mut value: i128 = 0;
        for (shift, byte) in bytes.iter().enumerate() {
            value += i128::from(*byte) << (shift * 8);
        }
        if sign == 1 {
            value = -value;
        }
        // Word-sized values that exceed i64 (e.g. an 8-byte magnitude with the
        // top bit set) fall through to the big-integer literal encoding.
        if let Ok(value) = i64::try_from(value) {
            return Ok(Literal::Integer(value));
        }
    }
    let capacity = len
        .checked_add(1)
        .ok_or_else(|| LoadError::DecodeError("ETF bignum allocation size overflow".into()))?;
    budget.charge_bytes(capacity)?;
    let mut out = Vec::with_capacity(capacity);
    out.push(sign);
    out.extend_from_slice(bytes);
    Ok(Literal::BigInteger(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(bytes: &[u8]) -> Result<Literal, LoadError> {
        let mut budget = DecodeBudget::default();
        decode_with_budget(bytes, &mut budget)
    }

    fn decode_with_budget(bytes: &[u8], budget: &mut DecodeBudget) -> Result<Literal, LoadError> {
        let atoms = AtomTable::with_common_atoms();
        decode_with_atom_table(bytes, &atoms, budget)
    }

    fn decode_with_atom_table(
        bytes: &[u8],
        atoms: &AtomTable,
        budget: &mut DecodeBudget,
    ) -> Result<Literal, LoadError> {
        let mut cursor = BoundedCursor::new(bytes);
        decode_external_term(&mut cursor, atoms, budget)
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
        let bytes = nested_single_element_lists(super::super::budget::MAX_ETF_DEPTH + 16);
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

    #[test]
    fn wide_flat_list_rejected_by_node_budget() {
        let bytes = [108u8, 0, 0, 0, 3, 97, 1, 97, 2, 97, 3, 106];
        let mut budget = DecodeBudget::new(16, 3, 4096, 16);
        match decode_with_budget(&bytes, &mut budget) {
            Err(LoadError::DecodeError(message)) => {
                assert!(
                    message.contains("node budget"),
                    "unexpected error: {message}"
                );
            }
            other => panic!("expected node-budget DecodeError, got {other:?}"),
        }
    }

    #[test]
    fn deeply_nested_list_rejected_by_small_custom_budget() {
        let bytes = nested_single_element_lists(3);
        let mut budget = DecodeBudget::new(1, 32, 4096, 16);
        match decode_with_budget(&bytes, &mut budget) {
            Err(LoadError::DecodeError(message)) => {
                assert!(
                    message.contains("nesting exceeds limit"),
                    "unexpected error: {message}"
                );
            }
            other => panic!("expected nesting DecodeError, got {other:?}"),
        }
    }

    #[test]
    fn literal_atom_ext_charges_atom_budget_only_for_new_atoms() {
        let atoms = AtomTable::with_common_atoms();
        let mut new_atom_bytes = vec![119u8, 1, b'x'];
        let mut budget = DecodeBudget::new(16, 16, 4096, 0);
        match decode_with_atom_table(&new_atom_bytes, &atoms, &mut budget) {
            Err(LoadError::DecodeError(message)) => assert!(
                message.contains("atom budget"),
                "unexpected error: {message}"
            ),
            other => panic!("expected atom-budget DecodeError, got {other:?}"),
        }

        new_atom_bytes.clear();
        new_atom_bytes.extend_from_slice(&[119u8, 2, b'o', b'k']);
        let mut budget = DecodeBudget::new(16, 16, 4096, 0);
        assert_eq!(
            decode_with_atom_table(&new_atom_bytes, &atoms, &mut budget),
            Ok(Literal::Atom(atoms.lookup("ok").expect("common ok atom")))
        );
    }

    /// SMALL_BIG_EXT (tag 110): one length byte, one sign byte, then
    /// little-endian magnitude bytes.
    fn small_big(sign: u8, magnitude_le: &[u8]) -> Vec<u8> {
        let mut bytes = vec![110, magnitude_le.len() as u8, sign];
        bytes.extend_from_slice(magnitude_le);
        bytes
    }

    #[test]
    fn small_big_within_i64_decodes_to_integer_literal() {
        assert_eq!(
            decode(&small_big(0, &42_u64.to_le_bytes())),
            Ok(Literal::Integer(42))
        );
        assert_eq!(
            decode(&small_big(1, &42_u64.to_le_bytes())),
            Ok(Literal::Integer(-42))
        );
    }

    #[test]
    fn word_sized_big_beyond_i64_decodes_to_big_integer_literal() {
        // Magnitude 2^63 with positive sign exceeds i64::MAX.
        let magnitude = (1_u64 << 63).to_le_bytes();
        let mut expected = vec![0_u8];
        expected.extend_from_slice(&magnitude);
        assert_eq!(
            decode(&small_big(0, &magnitude)),
            Ok(Literal::BigInteger(expected))
        );
        // ... while -(2^63) is exactly i64::MIN and stays an integer literal.
        assert_eq!(
            decode(&small_big(1, &magnitude)),
            Ok(Literal::Integer(i64::MIN))
        );
    }

    #[test]
    fn nine_byte_big_decodes_to_big_integer_literal_with_sign() {
        let magnitude = &100_000_000_000_000_000_000_u128.to_le_bytes()[..9];
        let mut expected = vec![1_u8];
        expected.extend_from_slice(magnitude);
        assert_eq!(
            decode(&small_big(1, magnitude)),
            Ok(Literal::BigInteger(expected))
        );
    }

    #[test]
    fn big_with_invalid_sign_is_rejected() {
        match decode(&small_big(2, &42_u64.to_le_bytes())) {
            Err(LoadError::DecodeError(message)) => {
                assert!(message.contains("sign"), "unexpected error: {message}");
            }
            other => panic!("expected invalid-sign DecodeError, got {other:?}"),
        }
    }
}
