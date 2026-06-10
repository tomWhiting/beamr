//! Integration tests for `bs_create_bin` against a real compiler-emitted
//! fixture (`tests/fixtures/bs_create_bin_fixture.erl`, compiled with
//! erlc/OTP 28). Each exported function exercises one segment shape; the
//! expected bytes were produced by running the same functions on the BEAM.

use beamr::atom::AtomTable;
use beamr::interpreter::{ExecutionResult, run};
use beamr::loader::{Instruction, prepare_module};
use beamr::module::{Module, ModuleRegistry};
use beamr::native::BifRegistryImpl;
use beamr::process::{CodePosition, ExitReason, Process};
use beamr::term::binary_ref::BinaryRef;

fn load_fixture(atoms: &AtomTable) -> Module {
    let registry = ModuleRegistry::new();
    let bifs = BifRegistryImpl::new();
    let (module, _report) = prepare_module(
        include_bytes!("fixtures/bs_create_bin_fixture.beam"),
        atoms,
        &registry,
        &bifs,
    )
    .expect("bs_create_bin fixture prepares");
    module
}

fn call(module: &Module, atoms: &AtomTable, function: &str) -> Vec<u8> {
    let function = atoms.intern(function);
    let label = *module.exports.get(&(function, 0)).expect("export exists");
    let entry_ip = module
        .code
        .iter()
        .position(|instruction| {
            matches!(instruction, Instruction::Label { label: candidate } if *candidate == label)
        })
        .expect("export label exists");
    let mut process = Process::new(1, 4096);
    process.set_code_position(Some(CodePosition {
        module: module.name,
        instruction_pointer: entry_ip,
    }));
    assert_eq!(
        run(&mut process, module),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    BinaryRef::new(process.x_reg(0))
        .expect("binary result")
        .as_bytes()
        .to_vec()
}

#[test]
fn bs_create_bin_fixture_matches_beam_results() {
    let atoms = AtomTable::with_common_atoms();
    let module = load_fixture(&atoms);

    let expectations: &[(&str, &[u8])] = &[
        ("concat_regs", b"hello world"),
        ("lit_prefix", b"littail"),
        ("int8", &[65]),
        ("int16_little", &[0x42, 0x41]),
        ("utf8_seg", &[0xe2, 0x82, 0xac, b'!']),
        ("join", b"a,b,c"),
        ("case_concat", b"one-tail"),
        ("signed_little", &[0xfe, 0xff]),
        ("sized_bin", b"abc"),
        ("big16", &[2, 1]),
        ("native16", &[1, 2]),
        ("big_signed", &[253, 255]),
        // <<3.14:64/float>> and <<3.14:64/float-little>> per the BEAM.
        ("float64", &[64, 9, 30, 184, 81, 235, 133, 31]),
        ("float32", &1.5_f32.to_bits().to_be_bytes()),
        ("float_little", &[31, 133, 235, 81, 184, 30, 9, 64]),
        ("priv_append", &[1, 2, 3]),
        ("bit_seg", &[0b1010_0000]),
    ];
    for (function, expected) in expectations {
        assert_eq!(
            call(&module, &atoms, function),
            expected.to_vec(),
            "{function} result mismatch"
        );
    }
}
