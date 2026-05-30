//! Pattern match instruction support.
//!
//! BEAM compiles Gleam's `case` and function clause patterns into
//! a sequence of test-and-branch instructions (is_tuple, test_arity,
//! is_eq, etc.). This module supports the interpreter in executing
//! those pattern match sequences, including guard evaluation.

use crate::error::ExecError;
use crate::interpreter::InstructionOutcome;
use crate::interpreter::opcodes::core;
use crate::loader::decode::compact::Operand;
use crate::module::Module;
use crate::process::Process;
use crate::term::boxed::Tuple;
use crate::term::compare;

pub(crate) fn select_val(
    process: &Process,
    module: &Module,
    value: &Operand,
    fail: &Operand,
    list: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    let value = core::read_term(process, value)?;
    for [candidate, label] in select_pairs(list)? {
        let candidate = core::read_term(process, candidate)?;
        if compare::exact_eq(value, candidate) {
            return super::opcodes::guards::jump(module, label);
        }
    }
    super::opcodes::guards::jump(module, fail)
}

pub(crate) fn select_tuple_arity(
    process: &Process,
    module: &Module,
    value: &Operand,
    fail: &Operand,
    list: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    let value = core::read_term(process, value)?;
    let Some(tuple) = Tuple::new(value) else {
        return super::opcodes::guards::jump(module, fail);
    };
    let arity = tuple.arity();
    for [candidate_arity, label] in select_pairs(list)? {
        if core::operand_usize(candidate_arity, "tuple arity")? == arity {
            return super::opcodes::guards::jump(module, label);
        }
    }
    super::opcodes::guards::jump(module, fail)
}

fn select_pairs(list: &Operand) -> Result<&[[Operand; 2]], ExecError> {
    let Operand::List(items) = list else {
        return Err(ExecError::InvalidOperand("select list"));
    };
    let (pairs, remainder) = items.as_chunks::<2>();
    if !remainder.is_empty() {
        return Err(ExecError::InvalidOperand("select list pairs"));
    }
    Ok(pairs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atom::Atom;
    use crate::loader::Instruction;
    use crate::term::Term;
    use crate::term::boxed::write_tuple;
    use std::collections::HashMap;

    fn module() -> Module {
        Module {
            name: Atom::OK,
            exports: HashMap::new(),
            code: vec![
                Instruction::Label { label: 1 },
                Instruction::Label { label: 2 },
                Instruction::Label { label: 3 },
            ],
            literals: Vec::new(),
            resolved_imports: Vec::new(),
            lambdas: Vec::new(),
            string_table: Vec::new(),
            line_info: Vec::new(),
        }
    }

    fn jump_ip(outcome: InstructionOutcome) -> usize {
        let InstructionOutcome::Jump(position) = outcome else {
            panic!("expected jump outcome, got {outcome:?}");
        };
        position.instruction_pointer
    }

    #[test]
    fn select_val_jumps_to_exact_value_match_or_fail() {
        let module = module();
        let process = Process::new(1, 8);
        let list = Operand::List(vec![
            Operand::Atom(Some(Atom::OK)),
            Operand::Label(1),
            Operand::Atom(Some(Atom::ERROR)),
            Operand::Label(2),
        ]);

        assert_eq!(
            jump_ip(
                select_val(
                    &process,
                    &module,
                    &Operand::Atom(Some(Atom::OK)),
                    &Operand::Label(3),
                    &list,
                )
                .expect("select ok")
            ),
            0
        );
        assert_eq!(
            jump_ip(
                select_val(
                    &process,
                    &module,
                    &Operand::Atom(Some(Atom::ERROR)),
                    &Operand::Label(3),
                    &list,
                )
                .expect("select error")
            ),
            1
        );
        assert_eq!(
            jump_ip(
                select_val(
                    &process,
                    &module,
                    &Operand::Atom(Some(Atom::UNDEFINED)),
                    &Operand::Label(3),
                    &list,
                )
                .expect("select fail")
            ),
            2
        );
    }

    #[test]
    fn select_tuple_arity_jumps_to_arity_match_or_fail() {
        let module = module();
        let mut process = Process::new(1, 16);
        let ptr = process.heap_mut().alloc(3).expect("tuple allocation");
        let tuple = write_tuple(
            unsafe { std::slice::from_raw_parts_mut(ptr, 3) },
            &[Term::small_int(1), Term::small_int(2)],
        )
        .expect("tuple term");
        process.set_x_reg(0, tuple);
        let list = Operand::List(vec![
            Operand::Unsigned(2),
            Operand::Label(1),
            Operand::Unsigned(3),
            Operand::Label(2),
        ]);

        assert_eq!(
            jump_ip(
                select_tuple_arity(&process, &module, &Operand::X(0), &Operand::Label(3), &list)
                    .expect("select tuple arity")
            ),
            0
        );
        assert_eq!(
            jump_ip(
                select_tuple_arity(
                    &process,
                    &module,
                    &Operand::Integer(4),
                    &Operand::Label(3),
                    &list,
                )
                .expect("select non tuple")
            ),
            2
        );
    }
}
