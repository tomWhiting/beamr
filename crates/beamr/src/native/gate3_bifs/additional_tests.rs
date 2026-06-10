use super::*;
use crate::atom::Atom;
use crate::native::ProcessContext;
use crate::process::Process;
use crate::term::Term;
use crate::term::binary;
use crate::term::boxed::{BigInt, Float, write_float, write_map};

fn context(process: &mut Process) -> ProcessContext<'_> {
    let mut context = ProcessContext::new();
    context.attach_process(process, 0);
    context
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}

fn float(value: f64) -> Term {
    let heap = Box::leak(Box::new([0u64; 2]));
    write_float(heap, value).expect("float")
}

fn binary_term(bytes: &[u8]) -> Term {
    let heap = Box::leak(vec![0u64; 2 + binary::packed_word_count(bytes.len())].into_boxed_slice());
    binary::write_binary(heap, bytes).expect("binary")
}

#[test]
fn round_and_trunc_convert_floats_to_integers() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    assert_eq!(bif_round(&[float(3.7)], &mut ctx), Ok(Term::small_int(4)));
    assert_eq!(bif_trunc(&[float(3.7)], &mut ctx), Ok(Term::small_int(3)));
}

#[test]
fn type_and_map_helpers_return_expected_values() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    let key = Term::atom(Atom::OK);
    let val = Term::small_int(9);
    let map_heap = Box::leak(vec![0u64; 4].into_boxed_slice());
    let map = write_map(map_heap, &[key], &[val]).expect("map");

    assert_eq!(
        bif_is_bitstring(&[binary_term(b"abc")], &mut ctx),
        Ok(Term::atom(Atom::TRUE))
    );
    assert_eq!(
        bif_is_bitstring(&[Term::small_int(1)], &mut ctx),
        Ok(Term::atom(Atom::FALSE))
    );
    assert_eq!(
        bif_is_map_key(&[key, map], &mut ctx),
        Ok(Term::atom(Atom::TRUE))
    );
    assert_eq!(
        bif_is_map_key(&[Term::atom(Atom::ERROR), map], &mut ctx),
        Ok(Term::atom(Atom::FALSE))
    );
    assert_eq!(bif_map_size(&[map], &mut ctx), Ok(Term::small_int(1)));
}

#[test]
fn binary_part_and_bit_size_operate_on_binaries() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    let result = bif_binary_part(
        &[
            binary_term(b"abcdef"),
            Term::small_int(2),
            Term::small_int(3),
        ],
        &mut ctx,
    )
    .expect("part");
    assert_eq!(
        binary::Binary::new(result).expect("binary").as_bytes(),
        b"cde"
    );
    assert_eq!(
        bif_bit_size(&[binary_term(b"abc")], &mut ctx),
        Ok(Term::small_int(24))
    );
}

#[test]
fn unary_minus_negates_integers_and_floats() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    assert_eq!(
        bif_unary_minus(&[Term::small_int(5)], &mut ctx),
        Ok(Term::small_int(-5))
    );
    let result = bif_unary_minus(&[float(2.5)], &mut ctx).expect("float");
    assert_eq!(Float::new(result).expect("float").value(), -2.5);
}

#[test]
fn unary_minus_promotes_small_overflow_to_bignum() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    let result =
        bif_unary_minus(&[Term::small_int(Term::SMALL_INT_MIN)], &mut ctx).expect("promotes");
    let bigint = BigInt::new(result).expect("bignum box");
    assert!(!bigint.is_negative());
    assert_eq!(bigint.limbs(), [Term::SMALL_INT_MIN.unsigned_abs()]);
}

#[test]
fn unary_minus_negates_bignum_arguments_and_demotes_small_results() {
    let mut process = Process::new(1, 256);
    let mut ctx = context(&mut process);

    // -(10^20) -> -100000000000000000000 stays a bignum with flipped sign.
    let magnitude = 100_000_000_000_000_000_000_u128;
    let limbs = [magnitude as u64, (magnitude >> 64) as u64];
    let big = ctx.alloc_bigint(false, &limbs).expect("bignum");
    let negated = bif_unary_minus(&[big], &mut ctx).expect("negates");
    let bigint = BigInt::new(negated).expect("bignum box");
    assert!(bigint.is_negative());
    assert_eq!(bigint.limbs(), limbs);

    // A non-canonical bignum holding -5 negates to the small immediate 5.
    let small_magnitude = ctx.alloc_bigint(true, &[5]).expect("bignum");
    assert_eq!(
        bif_unary_minus(&[small_magnitude], &mut ctx),
        Ok(Term::small_int(5))
    );
}

#[test]
fn additional_bifs_reject_bad_arguments() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    assert_eq!(bif_round(&[Term::atom(Atom::OK)], &mut ctx), Err(badarg()));
    assert_eq!(
        bif_map_size(&[Term::atom(Atom::OK)], &mut ctx),
        Err(badarg())
    );
    assert_eq!(
        bif_binary_part(
            &[Term::atom(Atom::OK), Term::small_int(0), Term::small_int(1)],
            &mut ctx
        ),
        Err(badarg())
    );
    assert_eq!(
        bif_bit_size(&[Term::atom(Atom::OK)], &mut ctx),
        Err(badarg())
    );
    assert_eq!(
        bif_unary_minus(&[Term::atom(Atom::OK)], &mut ctx),
        Err(badarg())
    );
}
