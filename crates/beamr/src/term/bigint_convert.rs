//! Byte-level and radix-string conversions for [`BigIntValue`].
//!
//! These helpers bridge the owned bignum representation with the loader's
//! literal encodings (sign+magnitude bytes, big-endian two's complement) and
//! the `integer_to_binary`/`binary_to_integer` BIF family (radix 2..=36
//! strings, uppercase digits like OTP).

use super::Term;
use super::bigint_math::{BigIntValue, normalized};

/// Smallest radix accepted by the integer/string conversion BIFs.
pub const MIN_RADIX: u32 = 2;
/// Largest radix accepted by the integer/string conversion BIFs.
pub const MAX_RADIX: u32 = 36;

/// Builds a value from a sign flag and little-endian magnitude bytes.
#[must_use]
pub fn from_sign_magnitude_le(negative: bool, magnitude_le: &[u8]) -> BigIntValue {
    let mut limbs = Vec::with_capacity(magnitude_le.len().div_ceil(8));
    for chunk in magnitude_le.chunks(8) {
        let mut limb = [0_u8; 8];
        limb[..chunk.len()].copy_from_slice(chunk);
        limbs.push(u64::from_le_bytes(limb));
    }
    BigIntValue::new(negative, limbs)
}

/// Decomposes a value into a sign flag and minimal little-endian magnitude bytes.
#[must_use]
pub fn to_sign_magnitude_le(value: &BigIntValue) -> (bool, Vec<u8>) {
    let mut bytes = Vec::with_capacity(value.limbs().len() * 8);
    for limb in value.limbs() {
        bytes.extend_from_slice(&limb.to_le_bytes());
    }
    while bytes.last() == Some(&0) {
        bytes.pop();
    }
    (value.is_negative(), bytes)
}

/// Builds a value from arbitrary-width big-endian two's-complement bytes,
/// as used by oversized BEAM compact integer operands.
#[must_use]
pub fn from_twos_complement_be(bytes: &[u8]) -> BigIntValue {
    let negative = bytes.first().is_some_and(|byte| byte & 0x80 != 0);
    let mut magnitude = bytes.to_vec();
    if negative {
        negate_twos_complement_be(&mut magnitude);
    }
    magnitude.reverse();
    from_sign_magnitude_le(negative, &magnitude)
}

/// Formats a value in the given radix using uppercase digits, OTP-style.
///
/// Returns `None` when the radix is outside `2..=36`.
#[must_use]
pub fn to_string_radix(value: &BigIntValue, radix: u32) -> Option<String> {
    if !(MIN_RADIX..=MAX_RADIX).contains(&radix) {
        return None;
    }
    let mut limbs = normalized(value.limbs()).to_vec();
    if limbs.is_empty() {
        return Some("0".to_owned());
    }
    let mut digits = Vec::new();
    while !limbs.is_empty() {
        let remainder = div_small_in_place(&mut limbs, u64::from(radix));
        digits.push(ascii_digit(remainder as u8));
    }
    if value.is_negative() {
        digits.push(b'-');
    }
    digits.reverse();
    // Digits are always ASCII, so this conversion cannot fail.
    String::from_utf8(digits).ok()
}

/// Parses an optionally signed integer string in the given radix.
///
/// Accepts the same inputs as OTP's `list_to_integer/2`: one optional leading
/// `+` or `-` followed by at least one digit (either letter case). Returns
/// `None` on any malformed input or radix outside `2..=36`.
#[must_use]
pub fn from_str_radix(text: &str, radix: u32) -> Option<BigIntValue> {
    if !(MIN_RADIX..=MAX_RADIX).contains(&radix) {
        return None;
    }
    let (negative, digits) = match text.as_bytes() {
        [b'-', rest @ ..] => (true, rest),
        [b'+', rest @ ..] => (false, rest),
        rest => (false, rest),
    };
    if digits.is_empty() {
        return None;
    }
    let mut limbs: Vec<u64> = Vec::new();
    for byte in digits {
        let digit = char::from(*byte).to_digit(radix)?;
        mul_small_add_in_place(&mut limbs, u64::from(radix), u64::from(digit));
    }
    Some(BigIntValue::new(negative, limbs))
}

/// Formats any runtime integer term (small or bignum) in the given radix.
///
/// Returns `None` for non-integer terms or a radix outside `2..=36`.
#[must_use]
pub fn integer_term_to_string_radix(term: Term, radix: u32) -> Option<String> {
    let value = if let Some(small) = term.as_small_int() {
        BigIntValue::from_i64(small)
    } else {
        BigIntValue::from_term(term)?
    };
    to_string_radix(&value, radix)
}

/// Divides little-endian limbs in place by a small divisor, returning the
/// remainder and trimming high zero limbs.
fn div_small_in_place(limbs: &mut Vec<u64>, divisor: u64) -> u64 {
    let mut remainder: u128 = 0;
    for limb in limbs.iter_mut().rev() {
        let value = (remainder << u64::BITS) | u128::from(*limb);
        *limb = (value / u128::from(divisor)) as u64;
        remainder = value % u128::from(divisor);
    }
    while limbs.last() == Some(&0) {
        limbs.pop();
    }
    remainder as u64
}

/// Computes `limbs * factor + addend` in place over little-endian limbs.
fn mul_small_add_in_place(limbs: &mut Vec<u64>, factor: u64, addend: u64) {
    let mut carry = u128::from(addend);
    for limb in limbs.iter_mut() {
        let value = u128::from(*limb) * u128::from(factor) + carry;
        *limb = value as u64;
        carry = value >> u64::BITS;
    }
    if carry != 0 {
        limbs.push(carry as u64);
    }
}

/// Negates big-endian two's-complement bytes in place (`!x + 1`).
fn negate_twos_complement_be(bytes: &mut [u8]) {
    for byte in bytes.iter_mut() {
        *byte = !*byte;
    }
    for byte in bytes.iter_mut().rev() {
        let (sum, overflow) = byte.overflowing_add(1);
        *byte = sum;
        if !overflow {
            break;
        }
    }
}

fn ascii_digit(digit: u8) -> u8 {
    if digit < 10 {
        b'0' + digit
    } else {
        b'A' + (digit - 10)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hundred_quintillion_times_hundred() -> BigIntValue {
        // 10^20 = 100000000000000000000, the canonical repro value.
        let bytes = 100_000_000_000_000_000_000_u128.to_le_bytes();
        from_sign_magnitude_le(false, &bytes)
    }

    #[test]
    fn sign_magnitude_round_trips_and_trims_zero_bytes() {
        let value = hundred_quintillion_times_hundred();
        let (negative, bytes) = to_sign_magnitude_le(&value);
        assert!(!negative);
        assert_eq!(bytes.len(), 9);
        assert_eq!(from_sign_magnitude_le(negative, &bytes), value);
    }

    #[test]
    fn twos_complement_decodes_positive_and_negative_values() {
        let positive = 100_000_000_000_000_000_000_i128;
        let be9 = |value: i128| value.to_be_bytes()[7..].to_vec();
        assert_eq!(
            from_twos_complement_be(&be9(positive)),
            hundred_quintillion_times_hundred()
        );
        assert_eq!(
            from_twos_complement_be(&be9(-positive)),
            hundred_quintillion_times_hundred().negate()
        );
        assert_eq!(from_twos_complement_be(&be9(-1)), BigIntValue::from_i64(-1));
        assert_eq!(from_twos_complement_be(&[]), BigIntValue::zero());
    }

    #[test]
    fn to_string_radix_formats_decimal_hex_and_zero() {
        let value = hundred_quintillion_times_hundred();
        assert_eq!(
            to_string_radix(&value, 10).as_deref(),
            Some("100000000000000000000")
        );
        assert_eq!(
            to_string_radix(&value.negate(), 10).as_deref(),
            Some("-100000000000000000000")
        );
        assert_eq!(
            to_string_radix(&BigIntValue::from_i64(255), 16).as_deref(),
            Some("FF")
        );
        assert_eq!(to_string_radix(&BigIntValue::zero(), 2).as_deref(), Some("0"));
        assert_eq!(to_string_radix(&value, 1), None);
        assert_eq!(to_string_radix(&value, 37), None);
    }

    #[test]
    fn from_str_radix_round_trips_and_rejects_malformed_input() {
        let value = hundred_quintillion_times_hundred();
        assert_eq!(
            from_str_radix("100000000000000000000", 10),
            Some(value.clone())
        );
        assert_eq!(
            from_str_radix("-100000000000000000000", 10),
            Some(value.negate())
        );
        assert_eq!(from_str_radix("+ff", 16), Some(BigIntValue::from_i64(255)));
        assert_eq!(from_str_radix("FF", 16), Some(BigIntValue::from_i64(255)));
        assert_eq!(from_str_radix("", 10), None);
        assert_eq!(from_str_radix("-", 10), None);
        assert_eq!(from_str_radix("12a", 10), None);
        assert_eq!(from_str_radix("10", 37), None);
    }

    #[test]
    fn multi_limb_decimal_round_trip() {
        let text = "123456789012345678901234567890123456789012345678901234567890";
        let value = from_str_radix(text, 10).expect("parses");
        assert!(value.limbs().len() > 2);
        assert_eq!(to_string_radix(&value, 10).as_deref(), Some(text));
    }
}
