//! Owned signed-magnitude BigInt arithmetic helpers.

use std::cmp::Ordering;

use super::{Term, boxed::BigInt};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BigIntValue {
    negative: bool,
    limbs: Vec<u64>,
}

impl BigIntValue {
    #[must_use]
    pub fn new(negative: bool, limbs: Vec<u64>) -> Self {
        let mut value = Self { negative, limbs };
        value.normalize();
        value
    }

    #[must_use]
    pub fn zero() -> Self {
        Self {
            negative: false,
            limbs: Vec::new(),
        }
    }

    #[must_use]
    pub fn from_i64(value: i64) -> Self {
        let magnitude = value.unsigned_abs();
        if magnitude == 0 {
            Self::zero()
        } else {
            Self {
                negative: value.is_negative(),
                limbs: vec![magnitude],
            }
        }
    }

    #[must_use]
    pub fn from_bigint(bigint: BigInt) -> Self {
        Self::new(bigint.is_negative(), bigint.limbs().to_vec())
    }

    #[must_use]
    pub fn from_term(term: Term) -> Option<Self> {
        if let Some(value) = term.as_small_int() {
            Some(Self::from_i64(value))
        } else {
            BigInt::new(term).map(Self::from_bigint)
        }
    }

    #[must_use]
    pub fn is_zero(&self) -> bool {
        self.limbs.is_empty()
    }

    #[must_use]
    pub fn is_negative(&self) -> bool {
        self.negative && !self.is_zero()
    }

    #[must_use]
    pub fn limbs(&self) -> &[u64] {
        &self.limbs
    }

    #[must_use]
    pub fn to_small_i64(&self) -> Option<i64> {
        match self.limbs.as_slice() {
            [] => Some(0),
            [limb] if self.is_negative() => {
                let limit = (i64::MAX as u64).wrapping_add(1);
                if *limb == limit {
                    Some(i64::MIN)
                } else {
                    i64::try_from(*limb).ok().map(|value| -value)
                }
            }
            [limb] => i64::try_from(*limb).ok(),
            _ => None,
        }
    }

    pub fn normalize(&mut self) {
        normalize_limbs(&mut self.limbs);
        if self.limbs.is_empty() {
            self.negative = false;
        }
    }

    #[must_use]
    pub fn add(&self, other: &Self) -> Self {
        match (self.is_negative(), other.is_negative()) {
            (true, true) => Self::new(true, add_abs(&self.limbs, &other.limbs)),
            (false, false) => Self::new(false, add_abs(&self.limbs, &other.limbs)),
            (false, true) => sub_signed(&self.limbs, &other.limbs),
            (true, false) => sub_signed(&other.limbs, &self.limbs),
        }
    }

    #[must_use]
    pub fn sub(&self, other: &Self) -> Self {
        self.add(&Self::new(!other.is_negative(), other.limbs.clone()))
    }

    #[must_use]
    pub fn mul(&self, other: &Self) -> Self {
        Self::new(
            self.is_negative() ^ other.is_negative(),
            mul_abs(&self.limbs, &other.limbs),
        )
    }

    #[must_use]
    pub fn divmod(&self, other: &Self) -> Option<(Self, Self)> {
        if other.is_zero() {
            return None;
        }
        let (quotient, remainder) = divmod_abs(&self.limbs, &other.limbs);
        Some((
            Self::new(self.is_negative() ^ other.is_negative(), quotient),
            Self::new(self.is_negative(), remainder),
        ))
    }

    #[must_use]
    pub fn shl_bits(&self, shift: usize) -> Self {
        Self::new(self.is_negative(), shl_abs(&self.limbs, shift))
    }

    #[must_use]
    pub fn bit_length(&self) -> usize {
        bit_length_abs(&self.limbs)
    }
}

#[must_use]
pub fn cmp_abs(left: &[u64], right: &[u64]) -> Ordering {
    let left = normalized(left);
    let right = normalized(right);
    match left.len().cmp(&right.len()) {
        Ordering::Equal => left.iter().rev().cmp(right.iter().rev()),
        order => order,
    }
}

#[must_use]
pub fn add_abs(left: &[u64], right: &[u64]) -> Vec<u64> {
    let len = left.len().max(right.len());
    let mut out = Vec::with_capacity(len + 1);
    let mut carry = 0_u128;
    for index in 0..len {
        let sum = u128::from(*left.get(index).unwrap_or(&0))
            + u128::from(*right.get(index).unwrap_or(&0))
            + carry;
        out.push(sum as u64);
        carry = sum >> 64;
    }
    if carry != 0 {
        out.push(carry as u64);
    }
    normalize_limbs(&mut out);
    out
}

#[must_use]
pub fn sub_abs(left: &[u64], right: &[u64]) -> Vec<u64> {
    debug_assert!(cmp_abs(left, right) != Ordering::Less);
    let mut out = Vec::with_capacity(left.len());
    let mut borrow = 0_u128;
    for (index, left_limb) in left.iter().enumerate() {
        let right_limb = u128::from(*right.get(index).unwrap_or(&0));
        let subtrahend = right_limb + borrow;
        let minuend = u128::from(*left_limb);
        if minuend >= subtrahend {
            out.push((minuend - subtrahend) as u64);
            borrow = 0;
        } else {
            out.push(((1_u128 << 64) + minuend - subtrahend) as u64);
            borrow = 1;
        }
    }
    normalize_limbs(&mut out);
    out
}

#[must_use]
pub fn mul_abs(left: &[u64], right: &[u64]) -> Vec<u64> {
    if normalized(left).is_empty() || normalized(right).is_empty() {
        return Vec::new();
    }
    let mut out = vec![0_u64; left.len() + right.len()];
    for (i, left_limb) in left.iter().enumerate() {
        let mut carry = 0_u128;
        for (j, right_limb) in right.iter().enumerate() {
            let index = i + j;
            let product =
                u128::from(*left_limb) * u128::from(*right_limb) + u128::from(out[index]) + carry;
            out[index] = product as u64;
            carry = product >> 64;
        }
        let mut index = i + right.len();
        while carry != 0 {
            let sum = u128::from(out[index]) + carry;
            out[index] = sum as u64;
            carry = sum >> 64;
            index += 1;
        }
    }
    normalize_limbs(&mut out);
    out
}

#[must_use]
pub fn divmod_abs(left: &[u64], right: &[u64]) -> (Vec<u64>, Vec<u64>) {
    let left = normalized(left);
    let right = normalized(right);
    if right.is_empty() {
        return (Vec::new(), Vec::new());
    }
    if cmp_abs(left, right) == Ordering::Less {
        return (Vec::new(), left.to_vec());
    }
    let bit_len = bit_length_abs(left);
    let mut quotient = Vec::new();
    let mut remainder = Vec::new();
    for bit in (0..bit_len).rev() {
        remainder = shl_abs(&remainder, 1);
        if test_bit(left, bit) {
            set_bit(&mut remainder, 0);
        }
        if cmp_abs(&remainder, right) != Ordering::Less {
            remainder = sub_abs(&remainder, right);
            set_bit(&mut quotient, bit);
        }
    }
    normalize_limbs(&mut quotient);
    normalize_limbs(&mut remainder);
    (quotient, remainder)
}

#[must_use]
pub fn normalized(limbs: &[u64]) -> &[u64] {
    let len = limbs
        .iter()
        .rposition(|limb| *limb != 0)
        .map_or(0, |index| index + 1);
    &limbs[..len]
}

fn sub_signed(positive: &[u64], negative_magnitude: &[u64]) -> BigIntValue {
    match cmp_abs(positive, negative_magnitude) {
        Ordering::Greater => BigIntValue::new(false, sub_abs(positive, negative_magnitude)),
        Ordering::Equal => BigIntValue::zero(),
        Ordering::Less => BigIntValue::new(true, sub_abs(negative_magnitude, positive)),
    }
}

fn normalize_limbs(limbs: &mut Vec<u64>) {
    if let Some(index) = limbs.iter().rposition(|limb| *limb != 0) {
        limbs.truncate(index + 1);
    } else {
        limbs.clear();
    }
}

fn bit_length_abs(limbs: &[u64]) -> usize {
    let limbs = normalized(limbs);
    match limbs.last() {
        Some(last) => (limbs.len() - 1) * 64 + (64 - last.leading_zeros() as usize),
        None => 0,
    }
}

fn test_bit(limbs: &[u64], bit: usize) -> bool {
    let limb = bit / 64;
    let offset = bit % 64;
    limbs
        .get(limb)
        .is_some_and(|value| ((value >> offset) & 1) == 1)
}

pub fn set_bit(limbs: &mut Vec<u64>, bit: usize) {
    let limb = bit / 64;
    let offset = bit % 64;
    if limbs.len() <= limb {
        limbs.resize(limb + 1, 0);
    }
    limbs[limb] |= 1_u64 << offset;
}

#[must_use]
pub fn shl_abs(limbs: &[u64], shift: usize) -> Vec<u64> {
    let limbs = normalized(limbs);
    if limbs.is_empty() {
        return Vec::new();
    }
    let limb_shift = shift / 64;
    let bit_shift = shift % 64;
    let mut out = vec![0_u64; limb_shift + limbs.len() + 1];
    let mut carry = 0_u64;
    for (index, limb) in limbs.iter().enumerate() {
        out[index + limb_shift] = (*limb << bit_shift) | carry;
        carry = if bit_shift == 0 {
            0
        } else {
            *limb >> (64 - bit_shift)
        };
    }
    out[limb_shift + limbs.len()] = carry;
    normalize_limbs(&mut out);
    out
}

#[must_use]
pub fn low_bits_mask(bits: usize) -> Vec<u64> {
    if bits == 0 {
        return Vec::new();
    }
    let limbs = bits.div_ceil(64);
    let mut out = vec![u64::MAX; limbs];
    let used = bits % 64;
    if used != 0
        && let Some(last) = out.last_mut()
    {
        *last = (1_u64 << used) - 1;
    }
    out
}

#[must_use]
pub fn bitand_abs(left: &[u64], right: &[u64]) -> Vec<u64> {
    let len = left.len().min(right.len());
    let mut out = Vec::with_capacity(len);
    for index in 0..len {
        out.push(left[index] & right[index]);
    }
    normalize_limbs(&mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_carries_across_limbs() {
        assert_eq!(add_abs(&[u64::MAX], &[1]), vec![0, 1]);
    }

    #[test]
    fn subtract_borrows_across_limbs() {
        assert_eq!(sub_abs(&[0, 1], &[1]), vec![u64::MAX]);
    }

    #[test]
    fn multiply_schoolbook_multiple_limbs() {
        assert_eq!(
            mul_abs(&[u64::MAX, u64::MAX], &[2]),
            vec![u64::MAX - 1, u64::MAX, 1]
        );
    }

    #[test]
    fn divmod_returns_quotient_and_remainder() {
        let dividend = BigIntValue::new(false, vec![0, 10]);
        let divisor = BigIntValue::from_i64(3);
        let Some((quotient, remainder)) = dividend.divmod(&divisor) else {
            panic!("non-zero divisor should divide");
        };
        assert_eq!(quotient.mul(&divisor).add(&remainder), dividend);
        assert_eq!(remainder, BigIntValue::from_i64(1));
    }

    #[test]
    fn sign_and_zero_normalization_are_canonical() {
        assert_eq!(
            BigIntValue::from_i64(-7).mul(&BigIntValue::from_i64(-6)),
            BigIntValue::from_i64(42)
        );
        assert_eq!(BigIntValue::new(true, vec![0, 0]), BigIntValue::zero());
    }
}
