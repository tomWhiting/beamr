//! Integration coverage for BIF/literal allocation stability.

use std::collections::HashMap;
use std::sync::Arc;

use beamr::{
    atom::{Atom, AtomTable},
    constant_pool::materialise_literals,
    gc::collect_major,
    interpreter::{ExecutionResult, NativeServices, run_with_native_services},
    loader::decode::compact::Operand,
    loader::{Instruction, Literal},
    module::{Module, ModuleRegistry, ResolvedImport, ResolvedImportTarget},
    native::{Capability, NativeEntry},
    process::{ExitReason, Process},
};

const ITERATIONS: usize = 10_000;

#[test]
fn bif_and_literal_heavy_workload_does_not_grow_monotonically() {
    let atom_table = Arc::new(AtomTable::new());
    let module = workload_module(&atom_table);
    let registry = ModuleRegistry::new();
    let services = NativeServices {
        atom_table: Some(Arc::clone(&atom_table)),
        timers: None,
        spawn_facility: None,
        link_facility: None,
        supervision_facility: None,
        io_sink: None,
        code_management_facility: None,
    };
    let mut process = Process::new(1, 512);
    process.reset_reductions(500_000);
    process.heap_mut().set_max_capacity(64 * 1024);

    let result = run_with_native_services(&mut process, &module, &registry, &services);
    assert_eq!(result, Ok(ExecutionResult::Exited(ExitReason::Normal)));

    collect_major(&mut process).expect("full GC after workload");
    let used_after_gc = process.heap().total_used();
    assert!(
        used_after_gc <= 512,
        "heap after full GC should be bounded and independent of {ITERATIONS} iterations; used {used_after_gc} words"
    );
}

fn workload_module(atom_table: &AtomTable) -> Module {
    let erlang = atom_table.intern("erlang");
    let rand = atom_table.intern("rand");
    let uniform = atom_table.intern("uniform");
    let self_atom = atom_table.intern("self");
    let send = atom_table.intern("send");
    let literal_atom = atom_table.intern("literal_payload");
    let literals = vec![Literal::Tuple(vec![
        Literal::Atom(literal_atom),
        Literal::List(
            vec![
                Literal::Integer(1),
                Literal::Integer(2),
                Literal::Integer(3),
            ],
            Box::new(Literal::Nil),
        ),
    ])];
    let constant_pool = materialise_literals(&literals, Some(atom_table)).expect("literal pool");
    let mut code = Vec::with_capacity(ITERATIONS * 8 + 2);

    for index in 0..ITERATIONS {
        code.push(Instruction::CallExt {
            arity: Operand::Unsigned(0),
            import: Operand::Unsigned(0),
        });
        code.push(Instruction::Move {
            source: Operand::Literal(0),
            destination: Operand::X(1),
        });
        code.push(Instruction::TestHeap {
            heap_need: Operand::Unsigned(6),
            live: Operand::Unsigned(2),
        });
        code.push(Instruction::PutTuple2 {
            destination: Operand::X(1),
            elements: Operand::List(vec![
                Operand::Integer(index as i64),
                Operand::X(0),
                Operand::X(1),
            ]),
        });
        code.push(Instruction::PutList {
            head: Operand::X(1),
            tail: Operand::Atom(None),
            destination: Operand::X(1),
        });
        code.push(Instruction::CallExt {
            arity: Operand::Unsigned(0),
            import: Operand::Unsigned(1),
        });
        code.push(Instruction::CallExt {
            arity: Operand::Unsigned(2),
            import: Operand::Unsigned(2),
        });
    }

    code.push(Instruction::Move {
        source: Operand::Atom(None),
        destination: Operand::X(0),
    });
    code.push(Instruction::Move {
        source: Operand::Atom(None),
        destination: Operand::X(1),
    });
    code.push(Instruction::Return);

    let label_index = code
        .iter()
        .enumerate()
        .filter_map(|(instruction_pointer, instruction)| match instruction {
            Instruction::Label { label } => Some((*label, instruction_pointer)),
            _ => None,
        })
        .collect();

    Module {
        name: Atom::OK,
        generation: 0,
        exports: HashMap::new(),
        label_index,
        code,
        literals,
        constant_pool,
        resolved_imports: vec![
            native_import(
                rand,
                uniform,
                0,
                beamr::native::stdlib_stubs::bif_rand_uniform,
                Capability::Entropy,
            ),
            native_import(
                erlang,
                self_atom,
                0,
                beamr::native::process_bifs::bif_self,
                Capability::Pure,
            ),
            native_import(
                erlang,
                send,
                2,
                beamr::native::gate3_bifs::bif_send,
                Capability::Pure,
            ),
        ],
        lambdas: Vec::new(),
        string_table: Vec::new(),
        line_info: Vec::new(),
        function_table: Vec::new(),
        line_table: Vec::new(),
    }
}

fn native_import(
    module: Atom,
    function: Atom,
    arity: u8,
    native_function: beamr::native::NativeFn,
    capability: Capability,
) -> ResolvedImport {
    ResolvedImport {
        module,
        function,
        arity,
        target: ResolvedImportTarget::Native(NativeEntry {
            function: native_function,
            is_dirty: false,
            capability,
        }),
    }
}
