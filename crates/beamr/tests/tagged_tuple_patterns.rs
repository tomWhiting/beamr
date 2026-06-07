use beamr::atom::{Atom, AtomTable};
use beamr::interpreter::{ExecutionResult, InstructionOutcome, run};
use beamr::loader::decode::Operand;
use beamr::loader::{Instruction, load_beam_chunks, prepare_module};
use beamr::module::{Module, ModuleRegistry};
use beamr::native::BifRegistryImpl;
use beamr::process::{CodePosition, ExitReason, Process};
use beamr::term::Term;
use beamr::term::boxed::write_tuple;

fn load_fixture(atoms: &AtomTable) -> Module {
    let registry = ModuleRegistry::new();
    let bifs = BifRegistryImpl::new();
    let (module, _report) = prepare_module(
        include_bytes!("fixtures/tagged_tuple_patterns.beam"),
        atoms,
        &registry,
        &bifs,
    )
    .expect("tagged tuple fixture prepares");
    module
}

fn call(module: &Module, atoms: &AtomTable, function: &str, args: &[Term]) -> Term {
    let function = atoms.intern(function);
    let arity = args.len() as u8;
    let label = *module
        .exports
        .get(&(function, arity))
        .expect("export exists");
    let entry_ip = module
        .code
        .iter()
        .position(|instruction| matches!(instruction, Instruction::Label { label: candidate } if *candidate == label))
        .expect("export label exists");
    let mut process = Process::new(1, 4096);
    for (index, arg) in args.iter().enumerate() {
        process.set_x_reg(index as u16, *arg);
    }
    process.set_code_position(Some(CodePosition {
        module: module.name,
        instruction_pointer: entry_ip,
    }));

    assert_eq!(
        run(&mut process, module),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    process.x_reg(0)
}

fn dispatch_is_tagged_tuple(
    module: &Module,
    input: Term,
    expected_arity: u64,
    tag: Atom,
) -> InstructionOutcome {
    let mut process = Process::new(1, 64);
    process.set_x_reg(0, input);
    let instruction = Instruction::IsTaggedTuple {
        fail: Operand::Label(7),
        value: Operand::X(0),
        arity: Operand::Unsigned(expected_arity),
        tag: Operand::Atom(Some(tag)),
    };
    beamr::interpreter::opcodes::dispatch(&mut process, module, &instruction, 1, None)
        .expect("is_tagged_tuple dispatch")
}

#[test]
fn decoded_is_tagged_tuple_handler_checks_tuple_arity_and_first_atom() {
    let module = Module {
        name: Atom::OK,
        generation: 0,
        exports: Default::default(),
        label_index: [(7, 0)].into_iter().collect(),
        code: vec![Instruction::Label { label: 7 }],
        literals: Vec::new(),
        constant_pool: Default::default(),
        resolved_imports: Vec::new(),
        lambdas: Vec::new(),
        string_table: Vec::new(),
        line_info: Vec::new(),
    };
    let mut ok_words = [0_u64; 3];
    let mut error_words = [0_u64; 3];
    let mut wrong_arity_words = [0_u64; 4];
    let mut empty_words = [0_u64; 1];
    let ok =
        write_tuple(&mut ok_words, &[Term::atom(Atom::OK), Term::small_int(42)]).expect("ok tuple");
    let error = write_tuple(
        &mut error_words,
        &[Term::atom(Atom::ERROR), Term::small_int(7)],
    )
    .expect("error tuple");
    let wrong_arity = write_tuple(
        &mut wrong_arity_words,
        &[Term::atom(Atom::OK), Term::small_int(1), Term::small_int(2)],
    )
    .expect("wrong arity tuple");
    let empty = write_tuple(&mut empty_words, &[]).expect("empty tuple");

    assert_eq!(
        dispatch_is_tagged_tuple(&module, ok, 2, Atom::OK),
        InstructionOutcome::Continue
    );
    for (input, expected_arity) in [
        (error, 2),
        (wrong_arity, 2),
        (Term::small_int(7), 2),
        (empty, 0),
    ] {
        assert!(matches!(
            dispatch_is_tagged_tuple(&module, input, expected_arity, Atom::OK),
            InstructionOutcome::Jump(CodePosition {
                instruction_pointer: 0,
                ..
            })
        ));
    }
}

#[test]
fn compiled_record_pattern_fixture_uses_is_tagged_tuple() {
    let atoms = AtomTable::new();
    let parsed = load_beam_chunks(
        include_bytes!("fixtures/tagged_tuple_patterns.beam"),
        &atoms,
    )
    .expect("tagged tuple fixture parses");

    assert!(
        parsed
            .instructions
            .iter()
            .any(|instruction| matches!(instruction, Instruction::IsTaggedTuple { .. })),
        "fixture should contain opcode 159 decoded as Instruction::IsTaggedTuple"
    );
}

#[test]
fn compiled_record_pattern_branches_execute_correctly() {
    let atoms = AtomTable::new();
    let module = load_fixture(&atoms);
    let mut ok_words = [0_u64; 3];
    let mut error_words = [0_u64; 3];
    let ok =
        write_tuple(&mut ok_words, &[Term::atom(Atom::OK), Term::small_int(42)]).expect("ok tuple");
    let error = write_tuple(
        &mut error_words,
        &[Term::atom(Atom::ERROR), Term::small_int(7)],
    )
    .expect("error tuple");

    assert_eq!(call(&module, &atoms, "match", &[ok]), Term::small_int(42));
    assert_eq!(call(&module, &atoms, "match", &[error]), Term::small_int(7));
}

#[test]
fn compiled_nested_record_patterns_execute_correctly() {
    let atoms = AtomTable::new();
    let module = load_fixture(&atoms);
    let mut inner_ok_words = [0_u64; 3];
    let mut outer_ok_words = [0_u64; 3];
    let mut inner_error_words = [0_u64; 3];
    let mut outer_error_words = [0_u64; 3];
    let inner_ok = write_tuple(
        &mut inner_ok_words,
        &[Term::atom(Atom::OK), Term::small_int(11)],
    )
    .expect("inner ok tuple");
    let outer_ok = write_tuple(
        &mut outer_ok_words,
        &[Term::atom(atoms.intern("outer")), inner_ok],
    )
    .expect("outer ok tuple");
    let inner_error = write_tuple(
        &mut inner_error_words,
        &[Term::atom(Atom::ERROR), Term::small_int(13)],
    )
    .expect("inner error tuple");
    let outer_error = write_tuple(
        &mut outer_error_words,
        &[Term::atom(atoms.intern("outer")), inner_error],
    )
    .expect("outer error tuple");

    assert_eq!(
        call(&module, &atoms, "nested", &[outer_ok]),
        Term::small_int(11)
    );
    assert_eq!(
        call(&module, &atoms, "nested", &[outer_error]),
        Term::small_int(13)
    );
}
