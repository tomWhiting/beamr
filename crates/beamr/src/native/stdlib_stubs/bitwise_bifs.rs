//! Erlang bitwise BIFs.

use crate::atom::Atom;
use crate::native::ProcessContext;
use crate::term::Term;
use crate::term::bigint_math::{BigIntValue, bitand_abs, low_bits_mask, set_bit};

pub fn bif_band(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    two_ints(args, context, |left, right| left & right)
}

pub fn bif_bnot(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [value] = args else {
        return Err(badarg());
    };
    let value = integer_value(*value)?;
    integer_result(BigIntValue::from_i64(-1).sub(&value), context)
}

pub fn bif_bor(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    two_ints(args, context, |left, right| left | right)
}

pub fn bif_bsl(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [value, shift] = args else {
        return Err(badarg());
    };
    let value = integer_value(*value)?;
    let shift = shift
        .as_small_int()
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(badarg)?;
    ensure_shifted_result_fits_context(&value, shift, context)?;
    integer_result(value.shl_bits(shift), context)
}

pub fn bif_bsr(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [value, shift] = args else {
        return Err(badarg());
    };
    let value = integer_value(*value)?;
    let shift = shift
        .as_small_int()
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(badarg)?;
    integer_result(arithmetic_shift_right(&value, shift), context)
}

pub fn bif_bxor(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    two_ints(args, context, |left, right| left ^ right)
}

fn two_ints(
    args: &[Term],
    context: &mut ProcessContext,
    operation: fn(u64, u64) -> u64,
) -> Result<Term, Term> {
    let [left, right] = args else {
        return Err(badarg());
    };
    let left = integer_value(*left)?;
    let right = integer_value(*right)?;
    let width = left.bit_length().max(right.bit_length()).saturating_add(1);
    let left_twos = to_twos_complement(&left, width);
    let right_twos = to_twos_complement(&right, width);
    let len = left_twos.len().max(right_twos.len());
    let mut out = Vec::with_capacity(len);
    for index in 0..len {
        out.push(operation(
            *left_twos.get(index).unwrap_or(&0),
            *right_twos.get(index).unwrap_or(&0),
        ));
    }
    integer_result(from_twos_complement(&out, width), context)
}

fn ensure_shifted_result_fits_context(
    value: &BigIntValue,
    shift: usize,
    context: &ProcessContext<'_>,
) -> Result<(), Term> {
    if value.is_zero() {
        return Ok(());
    }
    let bit_len = value.bit_length().checked_add(shift).ok_or_else(badarg)?;
    let limb_count = bit_len.div_ceil(u64::BITS as usize);
    let words = limb_count.checked_add(3).ok_or_else(badarg)?;
    let Some(heap) = context.process_heap() else {
        return Ok(());
    };
    if words > heap.max_capacity() {
        return Err(badarg());
    }
    Ok(())
}

fn arithmetic_shift_right(value: &BigIntValue, shift: usize) -> BigIntValue {
    if value.is_zero() {
        return BigIntValue::zero();
    }
    let width = value.bit_length().saturating_add(shift).saturating_add(1);
    let shifted_width = width.saturating_sub(shift);
    if shifted_width == 0 {
        return if value.is_negative() {
            BigIntValue::from_i64(-1)
        } else {
            BigIntValue::zero()
        };
    }
    let limbs = to_twos_complement(value, width);
    let mut shifted = Vec::new();
    for bit in shift..width {
        if test_bit(&limbs, bit) {
            set_bit(&mut shifted, bit - shift);
        }
    }
    if value.is_negative() {
        for bit in shifted_width..width {
            set_bit(&mut shifted, bit);
        }
    }
    from_twos_complement(&shifted, shifted_width)
}

fn to_twos_complement(value: &BigIntValue, width: usize) -> Vec<u64> {
    let mut limbs = value.limbs().to_vec();
    let mask = low_bits_mask(width);
    if !value.is_negative() {
        return bitand_abs(&limbs, &mask);
    }
    let len = mask.len();
    limbs.resize(len, 0);
    let mut out = Vec::with_capacity(len);
    for limb in limbs.iter().take(len) {
        out.push(!*limb);
    }
    let mut carry = 1_u128;
    for limb in &mut out {
        let sum = u128::from(*limb) + carry;
        *limb = sum as u64;
        carry = sum >> 64;
    }
    bitand_abs(&out, &mask)
}

fn from_twos_complement(limbs: &[u64], width: usize) -> BigIntValue {
    if width == 0 || !test_bit(limbs, width - 1) {
        return BigIntValue::new(false, bitand_abs(limbs, &low_bits_mask(width)));
    }
    let mask = low_bits_mask(width);
    let len = mask.len();
    let mut magnitude = Vec::with_capacity(len);
    for (index, mask_limb) in mask.iter().enumerate().take(len) {
        magnitude.push(!(*limbs.get(index).unwrap_or(&0)) & *mask_limb);
    }
    let mut carry = 1_u128;
    for limb in &mut magnitude {
        let sum = u128::from(*limb) + carry;
        *limb = sum as u64;
        carry = sum >> 64;
    }
    BigIntValue::new(true, bitand_abs(&magnitude, &mask))
}

fn test_bit(limbs: &[u64], bit: usize) -> bool {
    let limb = bit / 64;
    let offset = bit % 64;
    limbs
        .get(limb)
        .is_some_and(|value| ((value >> offset) & 1) == 1)
}

fn integer_value(term: Term) -> Result<BigIntValue, Term> {
    BigIntValue::from_term(term).ok_or_else(badarg)
}

fn integer_result(value: BigIntValue, context: &mut ProcessContext) -> Result<Term, Term> {
    if let Some(value) = value.to_small_i64().and_then(Term::try_small_int) {
        Ok(value)
    } else {
        context.alloc_bigint(value.is_negative(), value.limbs())
    }
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}
