//! End-to-end GC pressure tests for interpreter heap checks.

use std::{collections::HashMap, time::Instant};

use beamr::{
    atom::Atom,
    interpreter::{ExecutionResult, run},
    loader::Instruction,
    loader::decode::compact::Operand,
    module::{Module, ModuleOrigin},
    process::{ExitReason, Process},
    term::{Term, boxed::Cons},
};

fn module(code: Vec<Instruction>) -> Module {
    let label_index = code
        .iter()
        .enumerate()
        .filter_map(|(i, instr)| {
            if let Instruction::Label { label } = instr {
                Some((*label, i))
            } else {
                None
            }
        })
        .collect();
    Module {
        name: Atom::OK,
        generation: 0,
        origin: ModuleOrigin::Preloaded,
        exports: HashMap::new(),
        label_index,
        code,
        literals: Vec::new(),
        constant_pool: Default::default(),
        resolved_imports: Vec::new(),
        lambdas: Vec::new(),
        string_table: Vec::new(),
        line_info: Vec::new(),
        function_table: Vec::new(),
        line_table: Vec::new(),
    }
}

#[test]
fn interpreter_survives_10_000_allocations_under_gc_pressure() {
    let mut code = vec![Instruction::Move {
        source: Operand::Atom(None),
        destination: Operand::X(0),
    }];
    for value in 0..10_000 {
        code.push(Instruction::TestHeap {
            heap_need: Operand::Unsigned(2),
            live: Operand::Unsigned(1),
        });
        code.push(Instruction::PutList {
            head: Operand::Integer(value),
            tail: Operand::X(0),
            destination: Operand::X(0),
        });
    }
    code.push(Instruction::Return);
    let module = module(code);
    let mut process = Process::new(1, 2_048);
    process.reset_reductions(100_000);
    process.heap_mut().set_max_capacity(32_768);

    let start = Instant::now();
    let result = run(&mut process, &module);

    assert_eq!(result, Ok(ExecutionResult::Exited(ExitReason::Normal)));
    assert!(start.elapsed().as_secs_f64() < 5.0);
    assert_eq!(list_len(process.x_reg(0)), 10_000);
    assert!(process.heap().old_used() > 0);
}

fn list_len(mut term: Term) -> usize {
    let mut len = 0;
    while term.is_list() {
        let cons = Cons::new(term).expect("valid cons cell");
        len += 1;
        term = cons.tail();
    }
    assert!(term.is_nil());
    len
}
