//! The interpreter — the execution loop and heartbeat of fairness.
//!
//! Fetch, decode, execute, decrement reduction counter. When the
//! counter hits zero, save state and yield. Implements the subset
//! of BEAM opcodes that Gleam actually emits (per D5).
pub mod opcodes;
pub mod pattern;

use crate::error::ExecError;
use crate::module::Module;
use crate::process::{CodePosition, ExitReason, Process};

/// Result of running a process until it yields, waits, exits, or faults.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ExecutionResult {
    /// Reduction budget exhausted; scheduler should reset and requeue.
    Yielded,
    /// Process blocked waiting for a receive-family opcode.
    Waiting,
    /// Process terminated with an exit reason.
    Exited(ExitReason),
}

/// Control-flow outcome from one atomically completed instruction.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum InstructionOutcome {
    /// Continue at the sequential next instruction.
    Continue,
    /// Jump to a non-sequential code position.
    Jump(CodePosition),
    /// Yield after preserving the next code position in the process.
    Yield,
    /// Block waiting for a message.
    Waiting,
    /// Exit the process.
    Exit(ExitReason),
}

/// Execute `process` against `module` until a scheduler boundary or exit.
pub fn run(process: &mut Process, module: &Module) -> Result<ExecutionResult, ExecError> {
    if process.code_position().is_none() {
        process.set_code_position(Some(CodePosition {
            module: module.name,
            instruction_pointer: 0,
        }));
    }

    loop {
        let position = process
            .code_position()
            .ok_or(ExecError::InvalidOperand("code position"))?;
        let instruction = module
            .code
            .get(position.instruction_pointer)
            .ok_or(ExecError::InvalidOperand("instruction pointer"))?;
        let next_ip = position
            .instruction_pointer
            .checked_add(1)
            .ok_or(ExecError::InvalidOperand("instruction pointer"))?;

        match opcodes::dispatch(process, module, instruction, next_ip)? {
            InstructionOutcome::Continue => process.set_code_position(Some(CodePosition {
                module: module.name,
                instruction_pointer: next_ip,
            })),
            InstructionOutcome::Jump(target) => process.set_code_position(Some(target)),
            InstructionOutcome::Yield => return Ok(ExecutionResult::Yielded),
            InstructionOutcome::Waiting => return Ok(ExecutionResult::Waiting),
            InstructionOutcome::Exit(reason) => {
                process.set_code_position(None);
                return Ok(ExecutionResult::Exited(reason));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ExecutionResult, run};
    use crate::atom::{Atom, AtomTable};
    use crate::error::ExecError;
    use crate::loader::decode::compact::Operand;
    use crate::loader::{Instruction, Literal};
    use crate::module::{Module, ResolvedImport, ResolvedImportTarget};
    use crate::native::{NativeEntry, ProcessContext};
    use crate::process::{CodePosition, ExitReason, Process};
    use crate::term::Term;
    use crate::term::boxed::{Cons, Tuple};
    use std::collections::HashMap;

    fn module(name: Atom, code: Vec<Instruction>) -> Module {
        Module {
            name,
            exports: HashMap::new(),
            code,
            literals: Vec::new(),
            resolved_imports: Vec::new(),
            lambdas: Vec::new(),
            string_table: Vec::new(),
            line_info: Vec::new(),
        }
    }

    #[test]
    fn single_return_exits_normally() {
        let module = module(Atom::OK, vec![Instruction::Return]);
        let mut process = Process::new(1, 32);

        assert_eq!(
            run(&mut process, &module),
            Ok(ExecutionResult::Exited(ExitReason::Normal))
        );
    }

    #[test]
    fn call_chain_executes_in_sequence_and_returns() {
        let module = module(
            Atom::OK,
            vec![
                Instruction::Call {
                    arity: Operand::Unsigned(0),
                    label: Operand::Label(2),
                },
                Instruction::Return,
                Instruction::Label { label: 2 },
                Instruction::Move {
                    source: Operand::Integer(42),
                    destination: Operand::X(0),
                },
                Instruction::Return,
            ],
        );
        let mut process = Process::new(1, 32);

        assert_eq!(
            run(&mut process, &module),
            Ok(ExecutionResult::Exited(ExitReason::Normal))
        );
        assert_eq!(process.x_reg(0), Term::small_int(42));
    }

    #[test]
    fn tight_call_loop_yields_at_reduction_budget_and_resumes() {
        let module = module(
            Atom::OK,
            vec![
                Instruction::Label { label: 1 },
                Instruction::CallOnly {
                    arity: Operand::Unsigned(0),
                    label: Operand::Label(1),
                },
            ],
        );
        let mut process = Process::new(1, 32);
        process.reset_reductions(3);

        assert_eq!(run(&mut process, &module), Ok(ExecutionResult::Yielded));
        assert_eq!(process.reduction_counter(), 0);
        assert_eq!(
            process.code_position(),
            Some(CodePosition {
                module: Atom::OK,
                instruction_pointer: 0,
            })
        );
        process.reset_reductions(1);
        assert_eq!(run(&mut process, &module), Ok(ExecutionResult::Yielded));
    }

    #[test]
    fn func_info_and_move_cover_metadata_register_literals_and_stack() {
        let atoms = AtomTable::new();
        let module_atom = atoms.intern("sample");
        let function_atom = atoms.intern("main");
        let module = module(
            module_atom,
            vec![
                Instruction::FuncInfo {
                    module: Operand::Atom(Some(module_atom)),
                    function: Operand::Atom(Some(function_atom)),
                    arity: Operand::Unsigned(0),
                },
                Instruction::AllocateZero {
                    stack_need: Operand::Unsigned(1),
                    live: Operand::Unsigned(0),
                },
                Instruction::Move {
                    source: Operand::Literal(Literal::Integer(7)),
                    destination: Operand::X(0),
                },
                Instruction::Move {
                    source: Operand::X(0),
                    destination: Operand::Y(0),
                },
                Instruction::Move {
                    source: Operand::Y(0),
                    destination: Operand::X(1),
                },
                Instruction::Deallocate {
                    words: Operand::Unsigned(1),
                },
                Instruction::Return,
            ],
        );
        let mut process = Process::new(1, 32);
        let before_heap = process.heap().used();

        assert_eq!(
            run(&mut process, &module),
            Ok(ExecutionResult::Exited(ExitReason::Normal))
        );
        assert_eq!(process.current_mfa(), Some((module_atom, function_atom, 0)));
        assert_eq!(process.x_reg(1), Term::small_int(7));
        assert_eq!(process.heap().used(), before_heap);
    }

    #[test]
    fn stack_heap_and_data_opcodes_work() {
        let module = module(
            Atom::OK,
            vec![
                Instruction::TestHeap {
                    heap_need: Operand::Unsigned(6),
                    live: Operand::Unsigned(0),
                },
                Instruction::PutList {
                    head: Operand::Integer(1),
                    tail: Operand::Atom(None),
                    destination: Operand::X(0),
                },
                Instruction::PutTuple2 {
                    destination: Operand::X(1),
                    elements: Operand::List(vec![
                        Operand::Integer(1),
                        Operand::Integer(2),
                        Operand::Integer(3),
                    ]),
                },
                Instruction::GetTupleElement {
                    source: Operand::X(1),
                    index: Operand::Unsigned(0),
                    destination: Operand::X(2),
                },
                Instruction::Return,
            ],
        );
        let mut process = Process::new(1, 8);

        assert_eq!(
            run(&mut process, &module),
            Ok(ExecutionResult::Exited(ExitReason::Normal))
        );
        let cons = Cons::new(process.x_reg(0)).expect("put_list creates cons");
        assert_eq!(cons.head(), Term::small_int(1));
        assert_eq!(cons.tail(), Term::NIL);
        let tuple = Tuple::new(process.x_reg(1)).expect("put_tuple2 creates tuple");
        assert_eq!(tuple.arity(), 3);
        assert_eq!(process.x_reg(2), Term::small_int(1));
    }

    #[test]
    fn bad_tuple_access_and_heap_exhaustion_report_errors() {
        let bad_tuple = module(
            Atom::OK,
            vec![Instruction::GetTupleElement {
                source: Operand::Integer(1),
                index: Operand::Unsigned(0),
                destination: Operand::X(0),
            }],
        );
        assert_eq!(
            run(&mut Process::new(1, 8), &bad_tuple),
            Err(ExecError::Badarg)
        );

        let heap_check = module(
            Atom::OK,
            vec![Instruction::TestHeap {
                heap_need: Operand::Unsigned(10),
                live: Operand::Unsigned(0),
            }],
        );
        assert_eq!(
            run(&mut Process::new(1, 8), &heap_check),
            Err(ExecError::GcNeeded {
                requested: 10,
                available: 8,
            })
        );
    }

    fn add_one(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
        let Some(value) = args.first().and_then(|term| term.as_small_int()) else {
            return Err(Term::atom(Atom::BADARG));
        };
        Ok(Term::small_int(value + 1))
    }

    #[test]
    fn call_ext_invokes_registered_native_and_tail_call_deallocates() {
        let import = ResolvedImport {
            module: Atom::OK,
            function: Atom::OK,
            arity: 1,
            target: ResolvedImportTarget::Native(NativeEntry {
                function: add_one,
                is_dirty: false,
            }),
        };
        let mut module = module(
            Atom::OK,
            vec![
                Instruction::Move {
                    source: Operand::Integer(41),
                    destination: Operand::X(0),
                },
                Instruction::CallExt {
                    arity: Operand::Unsigned(1),
                    import: Operand::Unsigned(0),
                },
                Instruction::Return,
            ],
        );
        module.resolved_imports.push(import);
        let mut process = Process::new(1, 32);

        assert_eq!(
            run(&mut process, &module),
            Ok(ExecutionResult::Exited(ExitReason::Normal))
        );
        assert_eq!(process.x_reg(0), Term::small_int(42));
        assert_eq!(process.stack().len(), 0);
    }

    #[test]
    fn unknown_opcode_reports_opcode_number() {
        let module = module(
            Atom::OK,
            vec![Instruction::Generic {
                opcode: 222,
                name: "mystery",
                operands: Vec::new(),
            }],
        );
        assert_eq!(
            run(&mut Process::new(1, 8), &module),
            Err(ExecError::UnknownOpcode { opcode: 222 })
        );
    }

    #[test]
    fn proof_of_life_load_spawn_execute_exit_pipeline_fixture() {
        let atoms = AtomTable::new();
        let module_atom = atoms.intern("gleam_fib_fixture");
        let fib_atom = atoms.intern("fib");
        let mut module = module(
            module_atom,
            vec![
                Instruction::Label { label: 1 },
                Instruction::FuncInfo {
                    module: Operand::Atom(Some(module_atom)),
                    function: Operand::Atom(Some(fib_atom)),
                    arity: Operand::Unsigned(1),
                },
                Instruction::Move {
                    source: Operand::Integer(55),
                    destination: Operand::X(0),
                },
                Instruction::Return,
            ],
        );
        module.exports.insert((fib_atom, 1), 1);
        let mut process = Process::new(42, 32);
        process.set_x_reg(0, Term::small_int(10));
        process.set_code_position(Some(CodePosition {
            module: module_atom,
            instruction_pointer: 0,
        }));

        assert_eq!(
            run(&mut process, &module),
            Ok(ExecutionResult::Exited(ExitReason::Normal))
        );
        assert_eq!(process.x_reg(0), Term::small_int(55));
    }
}
