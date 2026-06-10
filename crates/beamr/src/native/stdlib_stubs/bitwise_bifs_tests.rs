use crate::atom::Atom;
use crate::native::ProcessContext;
use crate::process::Process;
use crate::term::Term;
use crate::term::boxed::BigInt;

use super::bitwise_bifs::*;

fn context(process: &mut Process) -> ProcessContext<'_> {
    let mut context = ProcessContext::new();
    context.attach_process(process, 0);
    context
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}

fn bigint_limb(term: Term) -> u64 {
    let bigint = BigInt::new(term).expect("expected BigInt term");
    bigint.limbs()[0]
}

#[test]
fn bitwise_acceptance_values_match_erlang() {
    let mut process = Process::new(1, 64);
    let mut context = context(&mut process);
    assert_eq!(
        bif_band(&[Term::small_int(5), Term::small_int(3)], &mut context),
        Ok(Term::small_int(1))
    );
    assert_eq!(
        bif_bor(&[Term::small_int(5), Term::small_int(3)], &mut context),
        Ok(Term::small_int(7))
    );
    assert_eq!(
        bif_bxor(&[Term::small_int(5), Term::small_int(3)], &mut context),
        Ok(Term::small_int(6))
    );
    assert_eq!(
        bif_bsl(&[Term::small_int(1), Term::small_int(4)], &mut context),
        Ok(Term::small_int(16))
    );
    assert_eq!(
        bif_bsr(&[Term::small_int(16), Term::small_int(4)], &mut context),
        Ok(Term::small_int(1))
    );
    assert_eq!(
        bif_bnot(&[Term::small_int(0)], &mut context),
        Ok(Term::small_int(-1))
    );
}

#[test]
fn bitwise_bigint_values_use_twos_complement_and_demote() {
    let mut process = Process::new(1, 256);
    let mut context = context(&mut process);

    let large = bif_bsl(&[Term::small_int(1), Term::small_int(70)], &mut context)
        .expect("large shift should allocate BigInt");
    assert_eq!(bigint_limb(large), 0);

    let masked = bif_band(&[large, large], &mut context).expect("BigInt band should work");
    assert!(BigInt::new(masked).is_some());

    assert_eq!(
        bif_bsr(&[large, Term::small_int(70)], &mut context),
        Ok(Term::small_int(1))
    );

    let negative = bif_bnot(&[large], &mut context).expect("BigInt bnot should work");
    let negative = BigInt::new(negative).expect("bnot of large positive should be BigInt");
    assert!(negative.is_negative());
}

#[test]
fn bitwise_rejects_non_integer_arguments() {
    let mut process = Process::new(1, 64);
    process.heap_mut().set_max_capacity(128);
    let mut context = context(&mut process);
    assert_eq!(
        bif_band(&[Term::atom(Atom::OK), Term::small_int(1)], &mut context),
        Err(badarg())
    );
    assert_eq!(
        bif_bnot(&[Term::atom(Atom::OK)], &mut context),
        Err(badarg())
    );
    assert_eq!(
        bif_bor(&[Term::small_int(1), Term::atom(Atom::OK)], &mut context),
        Err(badarg())
    );
    assert_eq!(
        bif_bsl(&[Term::small_int(1), Term::small_int(-1)], &mut context),
        Err(badarg())
    );
    assert_eq!(
        bif_bsr(&[Term::small_int(1), Term::small_int(-1)], &mut context),
        Err(badarg())
    );
    assert_eq!(
        bif_bsl(
            &[Term::small_int(1), Term::small_int(10_000_000)],
            &mut context
        ),
        Err(badarg())
    );
    assert_eq!(
        bif_bxor(&[Term::small_int(1), Term::atom(Atom::OK)], &mut context),
        Err(badarg())
    );
}
