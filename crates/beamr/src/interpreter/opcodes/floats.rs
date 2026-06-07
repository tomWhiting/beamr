//! Float arithmetic BEAM opcode handlers.

use crate::error::ExecError;
use crate::gc::GcError;
use crate::interpreter::InstructionOutcome;
use crate::loader::decode::Operand;
use crate::module::Module;
use crate::process::{Process, ProcessError};
use crate::term::boxed::{Float, write_float};

use super::core;

const BOXED_FLOAT_WORDS: usize = 2;

pub fn fmove(
    process: &mut Process,
    module: &Module,
    source: &Operand,
    dest: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    match (source, dest) {
        (Operand::FloatRegister(_), Operand::FloatRegister(_)) => {
            let value = read_float_register(process, source)?;
            write_float_register(process, dest, value)?;
        }
        (Operand::FloatRegister(_), _) => {
            let value = read_float_register(process, source)?;
            let term = allocate_boxed_float(process, value)?;
            core::write_term(process, dest, term)?;
        }
        (_, Operand::FloatRegister(_)) => {
            let value = read_boxed_float(process, module, source)?;
            write_float_register(process, dest, value)?;
        }
        _ => return Err(ExecError::InvalidOperand("fmove")),
    }
    Ok(InstructionOutcome::Continue)
}

pub fn fconv(
    process: &mut Process,
    module: &Module,
    source: &Operand,
    dest: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    let value = read_number_as_float(process, module, source)?;
    write_float_register(process, dest, value)?;
    Ok(InstructionOutcome::Continue)
}

pub fn fadd(
    process: &mut Process,
    left: &Operand,
    right: &Operand,
    dest: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    arithmetic(process, left, right, dest, |left, right| Ok(left + right))
}

pub fn fsub(
    process: &mut Process,
    left: &Operand,
    right: &Operand,
    dest: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    arithmetic(process, left, right, dest, |left, right| Ok(left - right))
}

pub fn fmul(
    process: &mut Process,
    left: &Operand,
    right: &Operand,
    dest: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    arithmetic(process, left, right, dest, |left, right| Ok(left * right))
}

pub fn fdiv(
    process: &mut Process,
    left: &Operand,
    right: &Operand,
    dest: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    arithmetic(process, left, right, dest, |left, right| {
        if is_zero(right) {
            Err(ExecError::Badarith)
        } else {
            Ok(left / right)
        }
    })
}

fn is_zero(value: f64) -> bool {
    value.to_bits() & 0x7fff_ffff_ffff_ffff == 0
}

pub fn fnegate(
    process: &mut Process,
    source: &Operand,
    dest: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    let value = read_float_register(process, source)?;
    write_float_register(process, dest, -value)?;
    Ok(InstructionOutcome::Continue)
}

fn arithmetic(
    process: &mut Process,
    left: &Operand,
    right: &Operand,
    dest: &Operand,
    op: impl FnOnce(f64, f64) -> Result<f64, ExecError>,
) -> Result<InstructionOutcome, ExecError> {
    let left = read_float_register(process, left)?;
    let right = read_float_register(process, right)?;
    let value = op(left, right)?;
    write_float_register(process, dest, value)?;
    Ok(InstructionOutcome::Continue)
}

fn read_float_register(process: &Process, operand: &Operand) -> Result<f64, ExecError> {
    let Operand::FloatRegister(index) = operand else {
        return Err(ExecError::InvalidOperand("float register source"));
    };
    let index = u16::try_from(*index).map_err(|_| ExecError::InvalidOperand("float register"))?;
    process.get_float_reg(index).map_err(process_error_to_exec)
}

fn write_float_register(
    process: &mut Process,
    operand: &Operand,
    value: f64,
) -> Result<(), ExecError> {
    let Operand::FloatRegister(index) = operand else {
        return Err(ExecError::InvalidOperand("float register destination"));
    };
    let index = u16::try_from(*index).map_err(|_| ExecError::InvalidOperand("float register"))?;
    process
        .set_float_reg(index, value)
        .map_err(process_error_to_exec)
}

fn read_boxed_float(
    process: &Process,
    module: &Module,
    operand: &Operand,
) -> Result<f64, ExecError> {
    let term = core::read_term(process, module, operand)?;
    Float::new(term)
        .map(Float::value)
        .ok_or(ExecError::Badarith)
}

fn read_number_as_float(
    process: &Process,
    module: &Module,
    operand: &Operand,
) -> Result<f64, ExecError> {
    let term = core::read_term(process, module, operand)?;
    if let Some(integer) = term.as_small_int() {
        return Ok(integer as f64);
    }
    Float::new(term)
        .map(Float::value)
        .ok_or(ExecError::Badarith)
}

fn allocate_boxed_float(process: &mut Process, value: f64) -> Result<crate::term::Term, ExecError> {
    let ptr = crate::gc::alloc(process, BOXED_FLOAT_WORDS).map_err(gc_error_to_exec)?;
    let heap = core::heap_slice(ptr, BOXED_FLOAT_WORDS);
    write_float(heap, value).ok_or(ExecError::Badarg)
}

fn process_error_to_exec(error: ProcessError) -> ExecError {
    match error {
        ProcessError::InvalidFloatRegister { .. } => ExecError::InvalidOperand("float register"),
        ProcessError::InvalidStatusTransition { .. } => ExecError::Badarg,
    }
}

fn gc_error_to_exec(error: GcError) -> ExecError {
    match error {
        GcError::HeapFull(error) => ExecError::from(error),
        GcError::InvalidObjectHeader(_) => ExecError::Badarg,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::atom::Atom;
    use crate::loader::Instruction;
    use crate::module::Module;
    use crate::term::Term;
    use crate::term::boxed::{Float, write_float};

    use super::*;

    fn module() -> Module {
        Module {
            name: Atom::OK,
            generation: 0,
            exports: HashMap::new(),
            label_index: HashMap::new(),
            code: Vec::new(),
            literals: Vec::new(),
            constant_pool: Default::default(),
            resolved_imports: Vec::new(),
            lambdas: Vec::new(),
            string_table: Vec::new(),
            line_info: Vec::new(),
        }
    }

    fn set_x_float(process: &mut Process, register: u16, value: f64) {
        let ptr = process
            .heap_mut()
            .alloc(BOXED_FLOAT_WORDS)
            .expect("test process has room for float");
        let heap = core::heap_slice(ptr, BOXED_FLOAT_WORDS);
        let term = write_float(heap, value).expect("test float fits in two words");
        process.set_x_reg(register, term);
    }

    #[test]
    fn fmove_moves_boxed_float_to_float_register() {
        let module = module();
        let mut process = Process::new(0, 16);
        set_x_float(&mut process, 0, 3.14);

        assert_eq!(
            fmove(
                &mut process,
                &module,
                &Operand::X(0),
                &Operand::FloatRegister(0),
            ),
            Ok(InstructionOutcome::Continue)
        );
        assert_eq!(process.get_float_reg(0), Ok(3.14));
    }

    #[test]
    fn fmove_moves_float_register_to_boxed_float_term() {
        let module = module();
        let mut process = Process::new(0, 16);
        assert_eq!(process.set_float_reg(0, 3.14), Ok(()));

        assert_eq!(
            fmove(
                &mut process,
                &module,
                &Operand::FloatRegister(0),
                &Operand::X(0),
            ),
            Ok(InstructionOutcome::Continue)
        );

        let float = Float::new(process.x_reg(0)).expect("boxed float");
        assert_eq!(float.value(), 3.14);
    }

    #[test]
    fn fconv_converts_small_integer_to_float_register() {
        let module = module();
        let mut process = Process::new(0, 16);
        process.set_x_reg(0, Term::small_int(42));

        assert_eq!(
            fconv(
                &mut process,
                &module,
                &Operand::X(0),
                &Operand::FloatRegister(0),
            ),
            Ok(InstructionOutcome::Continue)
        );
        assert_eq!(process.get_float_reg(0), Ok(42.0));
    }

    #[test]
    fn fconv_accepts_boxed_float_and_rejects_non_number() {
        let module = module();
        let mut process = Process::new(0, 16);
        set_x_float(&mut process, 0, 2.5);
        process.set_x_reg(1, Term::atom(Atom::OK));

        assert_eq!(
            fconv(
                &mut process,
                &module,
                &Operand::X(0),
                &Operand::FloatRegister(0),
            ),
            Ok(InstructionOutcome::Continue)
        );
        assert_eq!(process.get_float_reg(0), Ok(2.5));
        assert_eq!(
            fconv(
                &mut process,
                &module,
                &Operand::X(1),
                &Operand::FloatRegister(0),
            ),
            Err(ExecError::Badarith)
        );
    }

    #[test]
    fn float_arithmetic_handles_normal_values() {
        let mut process = Process::new(0, 16);
        assert_eq!(process.set_float_reg(0, 1.5), Ok(()));
        assert_eq!(process.set_float_reg(1, 2.5), Ok(()));

        assert_eq!(
            fadd(
                &mut process,
                &Operand::FloatRegister(0),
                &Operand::FloatRegister(1),
                &Operand::FloatRegister(2),
            ),
            Ok(InstructionOutcome::Continue)
        );
        assert_eq!(process.get_float_reg(2), Ok(4.0));

        assert_eq!(
            fsub(
                &mut process,
                &Operand::FloatRegister(1),
                &Operand::FloatRegister(0),
                &Operand::FloatRegister(3),
            ),
            Ok(InstructionOutcome::Continue)
        );
        assert_eq!(process.get_float_reg(3), Ok(1.0));

        assert_eq!(
            fmul(
                &mut process,
                &Operand::FloatRegister(0),
                &Operand::FloatRegister(1),
                &Operand::FloatRegister(4),
            ),
            Ok(InstructionOutcome::Continue)
        );
        assert_eq!(process.get_float_reg(4), Ok(3.75));
    }

    #[test]
    fn fdiv_by_zero_is_badarith() {
        let mut process = Process::new(0, 16);
        assert_eq!(process.set_float_reg(0, 10.0), Ok(()));
        assert_eq!(process.set_float_reg(1, 0.0), Ok(()));

        assert_eq!(
            fdiv(
                &mut process,
                &Operand::FloatRegister(0),
                &Operand::FloatRegister(1),
                &Operand::FloatRegister(2),
            ),
            Err(ExecError::Badarith)
        );
    }

    #[test]
    fn fdiv_and_fnegate_handle_float_values() {
        let mut process = Process::new(0, 16);
        assert_eq!(process.set_float_reg(0, 10.0), Ok(()));
        assert_eq!(process.set_float_reg(1, 2.0), Ok(()));
        assert_eq!(process.set_float_reg(3, 3.14), Ok(()));

        assert_eq!(
            fdiv(
                &mut process,
                &Operand::FloatRegister(0),
                &Operand::FloatRegister(1),
                &Operand::FloatRegister(2),
            ),
            Ok(InstructionOutcome::Continue)
        );
        assert_eq!(process.get_float_reg(2), Ok(5.0));

        assert_eq!(
            fnegate(
                &mut process,
                &Operand::FloatRegister(3),
                &Operand::FloatRegister(4),
            ),
            Ok(InstructionOutcome::Continue)
        );
        assert_eq!(process.get_float_reg(4), Ok(-3.14));
    }

    #[test]
    fn arithmetic_preserves_nan_and_infinity() {
        let mut process = Process::new(0, 16);
        assert_eq!(process.set_float_reg(0, f64::NAN), Ok(()));
        assert_eq!(process.set_float_reg(1, 1.0), Ok(()));
        assert_eq!(process.set_float_reg(2, f64::INFINITY), Ok(()));

        assert_eq!(
            fadd(
                &mut process,
                &Operand::FloatRegister(0),
                &Operand::FloatRegister(1),
                &Operand::FloatRegister(3),
            ),
            Ok(InstructionOutcome::Continue)
        );
        assert!(process.get_float_reg(3).is_ok_and(f64::is_nan));

        assert_eq!(
            fmul(
                &mut process,
                &Operand::FloatRegister(2),
                &Operand::FloatRegister(1),
                &Operand::FloatRegister(4),
            ),
            Ok(InstructionOutcome::Continue)
        );
        assert_eq!(process.get_float_reg(4), Ok(f64::INFINITY));
    }

    #[test]
    fn fmove_to_x_register_allocates_through_gc() {
        let module = module();
        let mut process = Process::new(0, 1);
        assert_eq!(process.set_float_reg(0, 6.25), Ok(()));

        assert_eq!(
            fmove(
                &mut process,
                &module,
                &Operand::FloatRegister(0),
                &Operand::X(0),
            ),
            Ok(InstructionOutcome::Continue)
        );

        let float = Float::new(process.x_reg(0)).expect("boxed float after GC allocation");
        assert_eq!(float.value(), 6.25);
    }

    #[test]
    fn float_dispatch_style_instruction_sequence_adds_one_point_zero() {
        let module = module();
        let mut process = Process::new(0, 16);
        process.set_x_reg(0, Term::small_int(41));
        assert_eq!(process.set_float_reg(1, 1.0), Ok(()));
        let instructions = [
            Instruction::Fconv {
                source: Operand::X(0),
                dest: Operand::FloatRegister(0),
            },
            Instruction::Fadd {
                fail: Operand::Label(0),
                left: Operand::FloatRegister(0),
                right: Operand::FloatRegister(1),
                dest: Operand::FloatRegister(2),
            },
            Instruction::Fmove {
                source: Operand::FloatRegister(2),
                dest: Operand::X(0),
            },
        ];

        for instruction in instructions {
            match instruction {
                Instruction::Fconv { source, dest } => {
                    assert_eq!(
                        fconv(&mut process, &module, &source, &dest),
                        Ok(InstructionOutcome::Continue)
                    );
                }
                Instruction::Fadd {
                    left, right, dest, ..
                } => {
                    assert_eq!(
                        fadd(&mut process, &left, &right, &dest),
                        Ok(InstructionOutcome::Continue)
                    );
                }
                Instruction::Fmove { source, dest } => {
                    assert_eq!(
                        fmove(&mut process, &module, &source, &dest),
                        Ok(InstructionOutcome::Continue)
                    );
                }
                _ => unreachable!("test only builds float instructions"),
            }
        }

        let float = Float::new(process.x_reg(0)).expect("boxed float result");
        assert_eq!(float.value(), 42.0);
    }
}
