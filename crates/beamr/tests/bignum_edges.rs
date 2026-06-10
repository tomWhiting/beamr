//! End-to-end coverage for bignum edge cases: loading modules with
//! larger-than-word compact integer literals, formatting via
//! `integer_to_binary/1`, unary minus/abs promotion, and exact comparison
//! between literal and computed bignums.
//!
//! Fixture source: `tests/fixtures/bignum.erl`.

use std::sync::Arc;

use beamr::atom::AtomTable;
use beamr::ets::copy::OwnedTerm;
use beamr::loader::load_module;
use beamr::module::ModuleRegistry;
use beamr::native::{
    BifRegistryImpl, bifs::register_gate1_bifs, gate3_bifs::register_gate3_bifs,
    process_bifs::register_gate2_bifs, stdlib_stubs::register_stdlib_stubs,
};
use beamr::process::ExitReason;
use beamr::scheduler::{Scheduler, SchedulerConfig};
use beamr::term::binary_ref::BinaryRef;
use beamr::term::boxed::BigInt;
use beamr::term::{Term, compare};

const REPRO_MAGNITUDE: u128 = 100_000_000_000_000_000_000; // 10^20

fn repro_limbs() -> [u64; 2] {
    [REPRO_MAGNITUDE as u64, (REPRO_MAGNITUDE >> 64) as u64]
}

fn start_scheduler(atoms: &Arc<AtomTable>) -> Scheduler {
    let bifs = BifRegistryImpl::new();
    register_gate1_bifs(&bifs, atoms).expect("gate 1 bifs");
    register_gate2_bifs(&bifs, atoms).expect("gate 2 bifs");
    register_gate3_bifs(&bifs, atoms).expect("gate 3 bifs");
    register_stdlib_stubs(&bifs, atoms).expect("stdlib stubs");
    let registry = Arc::new(ModuleRegistry::new());
    let (_module, _report) = load_module(
        include_bytes!("fixtures/bignum.beam"),
        atoms,
        &registry,
        &bifs,
    )
    .expect("bignum fixture loads — oversized compact integers must decode");
    Scheduler::with_code_server(
        SchedulerConfig {
            thread_count: Some(1),
            ..SchedulerConfig::default()
        },
        registry,
        Arc::clone(atoms),
        Arc::new(bifs),
    )
    .expect("scheduler starts")
}

/// Runs `bignum:<function>/0` to completion, returning an owned copy of the
/// result term.
fn call(scheduler: &Scheduler, atoms: &AtomTable, function: &str) -> OwnedTerm {
    let module = atoms.intern("bignum");
    let function = atoms.intern(function);
    let pid = scheduler
        .spawn(module, function, Vec::new())
        .expect("spawn fixture function");
    let (reason, result) = scheduler.run_until_exit(pid);
    assert_eq!(reason, ExitReason::Normal);
    result
}

fn assert_is_repro_bigint(term: Term, negative: bool, context: &str) {
    let bigint = BigInt::new(term).unwrap_or_else(|| panic!("{context}: expected bignum box"));
    assert_eq!(bigint.is_negative(), negative, "{context}: sign");
    assert_eq!(bigint.limbs(), repro_limbs(), "{context}: limbs");
}

#[test]
fn bignum_fixture_runs_all_edge_functions_correctly() {
    let atoms = Arc::new(AtomTable::with_common_atoms());
    let scheduler = start_scheduler(&atoms);

    // tostr() -> integer_to_binary(10^18 * 100)
    let tostr = call(&scheduler, &atoms, "tostr");
    let binary = BinaryRef::new(tostr.root()).expect("binary result");
    assert_eq!(binary.as_bytes(), b"100000000000000000000");

    // cmp() -> 10^18 * 100 =:= 10^20
    let cmp = call(&scheduler, &atoms, "cmp");
    let expected = atoms.lookup("true").expect("true atom");
    assert_eq!(cmp.root(), Term::atom(expected));

    // lit() -> the 10^20 literal itself (oversized compact operand).
    let lit = call(&scheduler, &atoms, "lit");
    assert_is_repro_bigint(lit.root(), false, "lit");

    // neg() -> -(10^18 * 100), abs1() -> abs(neg()).
    let neg = call(&scheduler, &atoms, "neg");
    assert_is_repro_bigint(neg.root(), true, "neg");
    let abs1 = call(&scheduler, &atoms, "abs1");
    assert_is_repro_bigint(abs1.root(), false, "abs1");

    // Literal and computed bignums must compare equal and order correctly.
    assert!(compare::cmp(lit.root(), abs1.root(), &atoms).is_eq());
    assert!(compare::numeric_eq(lit.root(), abs1.root()));
    assert!(compare::cmp(neg.root(), lit.root(), &atoms).is_lt());
    assert!(compare::cmp(lit.root(), Term::small_int(1), &atoms).is_gt());
    assert!(compare::cmp(neg.root(), Term::small_int(-1), &atoms).is_lt());
    assert!(!compare::numeric_eq(lit.root(), neg.root()));

    scheduler.shutdown();
}
