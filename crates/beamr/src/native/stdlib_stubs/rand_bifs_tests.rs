use std::collections::HashSet;

use crate::atom::Atom;
use crate::native::ProcessContext;
use crate::process::Process;
use crate::term::Term;
use crate::term::boxed::Float;

use super::bif_rand_uniform;

fn context(process: &mut Process) -> ProcessContext<'_> {
    let mut context = ProcessContext::new();
    context.attach_process(process, 0);
    context
}

#[test]
fn rand_uniform_returns_float_in_half_open_unit_range() {
    let mut process = Process::new(7, 32);
    let mut context = context(&mut process);
    let result = bif_rand_uniform(&[], &mut context).expect("uniform");
    let value = Float::new(result).expect("float").value();
    assert!((0.0..1.0).contains(&value));
}

#[test]
fn rand_uniform_successive_calls_vary() {
    let mut process = Process::new(7, 32);
    let mut context = context(&mut process);
    let mut values = HashSet::new();
    for _ in 0..8 {
        let result = bif_rand_uniform(&[], &mut context).expect("uniform");
        values.insert(Float::new(result).expect("float").value().to_bits());
    }
    assert!(values.len() > 1);
}

#[test]
fn rand_uniform_rejects_arguments() {
    let mut process = Process::new(7, 32);
    let mut context = context(&mut process);
    assert_eq!(
        bif_rand_uniform(&[Term::small_int(1)], &mut context),
        Err(Term::atom(Atom::BADARG))
    );
}
