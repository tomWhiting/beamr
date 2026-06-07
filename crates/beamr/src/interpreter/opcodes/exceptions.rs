//! Exception opcode handlers.

use crate::atom::Atom;
use crate::error::ExecError;
use crate::interpreter::InstructionOutcome;
use crate::interpreter::opcodes::core;
use crate::loader::decode::compact::Operand;
use crate::module::Module;
use crate::process::{
    CodePosition, Exception, ExceptionHandler, ExitReason, HandlerKind, Process, Register,
};
use crate::term::Term;
use crate::term::boxed::write_tuple;

pub fn try_(
    process: &mut Process,
    module: &Module,
    destination: &Operand,
    label: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    push_handler(process, module, destination, label, HandlerKind::Try)
}

pub fn catch_(
    process: &mut Process,
    module: &Module,
    destination: &Operand,
    label: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    push_handler(process, module, destination, label, HandlerKind::Catch)
}

pub fn try_end(process: &mut Process, source: &Operand) -> Result<InstructionOutcome, ExecError> {
    let _ = register(source)?;
    let _ = process.pop_exception_handler();
    process.set_current_exception(None);
    Ok(InstructionOutcome::Continue)
}

pub fn catch_end(process: &mut Process, source: &Operand) -> Result<InstructionOutcome, ExecError> {
    let destination = y_register(source)?;
    if process.current_exception().is_none() {
        let _ = process.pop_exception_handler();
    }
    process.set_current_exception(None);
    process
        .stack_mut()
        .set_y_reg(destination, Term::NIL)
        .map_err(ExecError::from)?;
    Ok(InstructionOutcome::Continue)
}

pub fn try_case(process: &mut Process, source: &Operand) -> Result<InstructionOutcome, ExecError> {
    core::write_term(process, source, Term::NIL)?;
    if let Some(exception) = process.current_exception() {
        process.set_x_reg(0, exception.class);
        process.set_x_reg(1, exception.reason);
        process.set_x_reg(2, exception.stacktrace);
    }
    Ok(InstructionOutcome::Continue)
}

pub fn try_case_end(
    process: &mut Process,
    module: &Module,
    source: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    let value = core::read_term(process, module, source)?;

    let reason = two_tuple(process, Term::atom(Atom::BADMATCH), value)?;
    raise_exception(process, Exception::error(reason, Term::NIL))
}

pub fn raise(
    process: &mut Process,
    module: &Module,
    stacktrace: &Operand,
    reason: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    let stacktrace = core::read_term(process, module, stacktrace)?;
    let reason = core::read_term(process, module, reason)?;

    let class = process
        .current_exception()
        .map_or(Term::atom(Atom::ERROR), |exception| exception.class);
    raise_exception(
        process,
        Exception {
            class,
            reason,
            stacktrace,
        },
    )
}

pub fn badmatch(
    process: &mut Process,
    module: &Module,
    value: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    let value = core::read_term(process, module, value)?;

    let reason = two_tuple(process, Term::atom(Atom::BADMATCH), value)?;
    raise_exception(process, Exception::error(reason, Term::NIL))
}

pub fn case_end(
    process: &mut Process,
    module: &Module,
    value: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    let value = core::read_term(process, module, value)?;

    let reason = two_tuple(process, Term::atom(Atom::CASE_CLAUSE), value)?;
    raise_exception(process, Exception::error(reason, Term::NIL))
}

pub fn if_end(process: &mut Process) -> Result<InstructionOutcome, ExecError> {
    let reason = two_tuple(process, Term::atom(Atom::IF_CLAUSE), Term::NIL)?;
    raise_exception(process, Exception::error(reason, Term::NIL))
}

pub fn raise_exception(
    process: &mut Process,
    exception: Exception,
) -> Result<InstructionOutcome, ExecError> {
    if let Some(handler) = process.pop_exception_handler() {
        process.stack_mut().truncate(handler.stack_depth);
        process.set_current_exception(Some(exception));
        match handler.kind {
            HandlerKind::Try => {
                write_register(process, handler.destination, exception.reason)?;
            }
            HandlerKind::Catch => {
                let caught = catch_value(process, exception)?;
                process.set_x_reg(0, caught);
            }
        }
        Ok(InstructionOutcome::Jump(handler.catch_position))
    } else {
        process.set_current_exception(Some(exception));
        Ok(InstructionOutcome::Exit(ExitReason::Error))
    }
}

impl Exception {
    fn error(reason: Term, stacktrace: Term) -> Self {
        Self {
            class: Term::atom(Atom::ERROR),
            reason,
            stacktrace,
        }
    }
}

fn push_handler(
    process: &mut Process,
    module: &Module,
    destination: &Operand,
    label: &Operand,
    kind: HandlerKind,
) -> Result<InstructionOutcome, ExecError> {
    let destination = register(destination)?;
    let catch_position = label_position(module, label)?;
    process.push_exception_handler(ExceptionHandler {
        kind,
        stack_depth: process.stack().len(),
        catch_position,
        destination,
    });
    Ok(InstructionOutcome::Continue)
}

fn catch_value(process: &mut Process, exception: Exception) -> Result<Term, ExecError> {
    if exception.class == Term::atom(Atom::THROW) {
        Ok(exception.reason)
    } else if exception.class == Term::atom(Atom::EXIT_CLASS) {
        two_tuple(process, Term::atom(Atom::EXIT), exception.reason)
    } else {
        let payload = two_tuple(process, exception.reason, exception.stacktrace)?;
        two_tuple(process, Term::atom(Atom::EXIT), payload)
    }
}

fn label_position(module: &Module, label: &Operand) -> Result<CodePosition, ExecError> {
    Ok(CodePosition {
        module: module.name,
        instruction_pointer: core::label_ip(module, core::operand_label(label)?)?,
    })
}

fn register(operand: &Operand) -> Result<Register, ExecError> {
    match operand {
        Operand::X(index) => u16::try_from(*index)
            .map(Register::X)
            .map_err(|_| ExecError::InvalidOperand("X register")),
        Operand::Y(index) => u16::try_from(*index)
            .map(Register::Y)
            .map_err(|_| ExecError::InvalidOperand("Y register")),
        Operand::TypedRegister { register, .. } => self::register(register),
        _ => Err(ExecError::InvalidOperand("register")),
    }
}

fn y_register(operand: &Operand) -> Result<u16, ExecError> {
    match operand {
        Operand::Y(index) => {
            u16::try_from(*index).map_err(|_| ExecError::InvalidOperand("Y register"))
        }
        Operand::TypedRegister { register, .. } => self::y_register(register),
        _ => Err(ExecError::InvalidOperand("Y register")),
    }
}

fn write_register(
    process: &mut Process,
    destination: Register,
    value: Term,
) -> Result<(), ExecError> {
    match destination {
        Register::X(index) => {
            process.set_x_reg(index, value);
            Ok(())
        }
        Register::Y(index) => process
            .stack_mut()
            .set_y_reg(index, value)
            .map_err(ExecError::from),
    }
}

fn two_tuple(process: &mut Process, left: Term, right: Term) -> Result<Term, ExecError> {
    let ptr = process.heap_mut().alloc(3).map_err(ExecError::from)?;
    let words = core::heap_slice(ptr, 3);
    write_tuple(words, &[left, right]).ok_or(ExecError::Badarg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interpreter::opcodes::dispatch;
    use crate::loader::Instruction;
    use crate::term::boxed::Tuple;
    use std::collections::HashMap;
    use std::sync::Arc;

    fn module(code: Vec<Instruction>) -> Module {
        let label_index = code
            .iter()
            .enumerate()
            .filter_map(|(ip, instruction)| match instruction {
                Instruction::Label { label } => Some((*label, ip)),
                _ => None,
            })
            .collect();
        Module {
            name: Atom::OK,
            generation: 0,
            exports: HashMap::new(),
            label_index,
            code,
            literals: Vec::new(),
            constant_pool: Default::default(),
            resolved_imports: Vec::new(),
            lambdas: Vec::new(),
            string_table: Vec::new(),
            line_info: Vec::new(),
        }
    }

    fn label20_module() -> Module {
        module(vec![Instruction::Label { label: 20 }])
    }

    fn jump_to_label20() -> InstructionOutcome {
        InstructionOutcome::Jump(CodePosition {
            module: Atom::OK,
            instruction_pointer: 0,
        })
    }

    fn exception(class: Atom, reason: Term, stacktrace: Term) -> Exception {
        Exception {
            class: Term::atom(class),
            reason,
            stacktrace,
        }
    }

    fn push_frame(process: &mut Process, module: &Module) -> Arc<Module> {
        let module_version = Arc::new(module.clone());
        process
            .stack_mut()
            .push_frame(Atom::OK, 0, Arc::clone(&module_version), 1)
            .expect("frame");
        module_version
    }

    #[test]
    fn try_badmatch_captures_class_reason_and_stacktrace() {
        let code = label20_module();
        let mut process = Process::new(1, 32);
        assert_eq!(
            try_(&mut process, &code, &Operand::X(0), &Operand::Label(20)),
            Ok(InstructionOutcome::Continue)
        );
        assert_eq!(
            badmatch(&mut process, &code, &Operand::Integer(42)),
            Ok(jump_to_label20())
        );
        assert_eq!(
            try_case(&mut process, &Operand::X(0)),
            Ok(InstructionOutcome::Continue)
        );
        assert_eq!(process.x_reg(0), Term::atom(Atom::ERROR));
        let reason = Tuple::new(process.x_reg(1)).expect("badmatch reason tuple");
        assert_eq!(reason.get(0), Some(Term::atom(Atom::BADMATCH)));
        assert_eq!(reason.get(1), Some(Term::small_int(42)));
        assert_eq!(process.x_reg(2), Term::NIL);
    }

    #[test]
    fn nested_try_uses_inner_handler_first_and_raise_preserves_stacktrace() {
        let code = module(vec![
            Instruction::Label { label: 10 },
            Instruction::Label { label: 20 },
        ]);
        let mut process = Process::new(1, 64);
        try_(&mut process, &code, &Operand::X(10), &Operand::Label(10)).expect("outer try");
        try_(&mut process, &code, &Operand::X(20), &Operand::Label(20)).expect("inner try");
        assert_eq!(
            raise(
                &mut process,
                &code,
                &Operand::Integer(777),
                &Operand::Atom(Some(Atom::BADARG))
            ),
            Ok(InstructionOutcome::Jump(CodePosition {
                module: Atom::OK,
                instruction_pointer: 1
            }))
        );
        try_case(&mut process, &Operand::X(0)).expect("expose exception");
        assert_eq!(process.exception_handler_count(), 1);
        assert_eq!(process.x_reg(0), Term::atom(Atom::ERROR));
        assert_eq!(process.x_reg(1), Term::atom(Atom::BADARG));
        assert_eq!(process.x_reg(2), Term::small_int(777));
    }

    #[test]
    fn case_if_and_try_case_end_build_catchable_error_reasons() {
        let code = label20_module();
        let mut process = Process::new(1, 64);
        try_(&mut process, &code, &Operand::X(0), &Operand::Label(20)).expect("try");
        assert_eq!(
            case_end(&mut process, &code, &Operand::Atom(Some(Atom::OK))),
            Ok(jump_to_label20())
        );
        try_case(&mut process, &Operand::X(0)).expect("expose");
        let reason = Tuple::new(process.x_reg(1)).expect("case_clause tuple");
        assert_eq!(reason.get(0), Some(Term::atom(Atom::CASE_CLAUSE)));
        assert_eq!(reason.get(1), Some(Term::atom(Atom::OK)));
        try_(&mut process, &code, &Operand::X(0), &Operand::Label(20)).expect("try");
        assert_eq!(if_end(&mut process), Ok(jump_to_label20()));
        try_case(&mut process, &Operand::X(0)).expect("expose");
        let reason = Tuple::new(process.x_reg(1)).expect("if_clause tuple");
        assert_eq!(reason.get(0), Some(Term::atom(Atom::IF_CLAUSE)));
        assert_eq!(reason.get(1), Some(Term::NIL));
    }

    #[test]
    fn try_handler_truncates_stack_before_writing_destination() {
        let code = label20_module();
        let mut process = Process::new(1, 64);
        let module_version = push_frame(&mut process, &code);
        try_(&mut process, &code, &Operand::Y(0), &Operand::Label(20)).expect("try");
        process
            .stack_mut()
            .push_frame(Atom::OK, 1, module_version, 1)
            .expect("intermediate frame");
        assert_eq!(
            raise_exception(
                &mut process,
                exception(Atom::ERROR, Term::atom(Atom::BADARG), Term::NIL)
            ),
            Ok(jump_to_label20())
        );
        assert_eq!(process.stack().len(), 1);
        assert_eq!(process.stack().y_reg(0), Ok(Term::atom(Atom::BADARG)));
    }

    #[test]
    fn catch_handler_records_kind_and_dispatch_does_not_report_unsupported() {
        let code = label20_module();
        let mut process = Process::new(1, 32);
        let instruction = Instruction::Catch {
            destination: Operand::Y(0),
            label: Operand::Label(20),
        };
        assert_eq!(
            dispatch(&mut process, &code, &instruction, 1, None),
            Ok(InstructionOutcome::Continue)
        );
        assert_eq!(process.exception_handler_count(), 1);
        let handler = process.pop_exception_handler().expect("catch handler");
        assert_eq!(handler.kind, HandlerKind::Catch);
        assert_eq!(handler.stack_depth, process.stack().len());
    }

    #[test]
    fn catch_handler_wraps_error_class_as_exit_reason_stacktrace() {
        let code = label20_module();
        let mut process = Process::new(1, 64);
        catch_(&mut process, &code, &Operand::Y(0), &Operand::Label(20)).expect("catch");
        assert_eq!(
            raise_exception(
                &mut process,
                exception(Atom::ERROR, Term::atom(Atom::BADARG), Term::small_int(777))
            ),
            Ok(jump_to_label20())
        );
        let outer = Tuple::new(process.x_reg(0)).expect("outer EXIT tuple");
        assert_eq!(outer.get(0), Some(Term::atom(Atom::EXIT)));
        let inner =
            Tuple::new(outer.get(1).expect("inner tuple")).expect("reason stacktrace tuple");
        assert_eq!(inner.get(0), Some(Term::atom(Atom::BADARG)));
        assert_eq!(inner.get(1), Some(Term::small_int(777)));
    }

    #[test]
    fn catch_handler_passes_throw_reason_through() {
        let code = label20_module();
        let mut process = Process::new(1, 32);
        catch_(&mut process, &code, &Operand::Y(0), &Operand::Label(20)).expect("catch");
        raise_exception(
            &mut process,
            exception(Atom::THROW, Term::small_int(123), Term::NIL),
        )
        .expect("throw caught");
        assert_eq!(process.x_reg(0), Term::small_int(123));
    }

    #[test]
    fn catch_handler_wraps_exit_class_as_exit_reason() {
        let code = label20_module();
        let mut process = Process::new(1, 32);
        catch_(&mut process, &code, &Operand::Y(0), &Operand::Label(20)).expect("catch");
        raise_exception(
            &mut process,
            exception(Atom::EXIT_CLASS, Term::atom(Atom::NORMAL), Term::NIL),
        )
        .expect("exit caught");
        let tuple = Tuple::new(process.x_reg(0)).expect("EXIT tuple");
        assert_eq!(tuple.get(0), Some(Term::atom(Atom::EXIT)));
        assert_eq!(tuple.get(1), Some(Term::atom(Atom::NORMAL)));
    }

    #[test]
    fn exception_in_nested_call_unwinds_intermediate_frames() {
        let code = label20_module();
        let mut process = Process::new(1, 64);
        let module_version = push_frame(&mut process, &code);
        catch_(&mut process, &code, &Operand::Y(0), &Operand::Label(20)).expect("catch");
        process
            .stack_mut()
            .push_frame(Atom::OK, 1, module_version, 1)
            .expect("intermediate frame");
        raise_exception(
            &mut process,
            exception(Atom::THROW, Term::small_int(123), Term::NIL),
        )
        .expect("caught");
        assert_eq!(process.stack().len(), 1);
        assert_eq!(process.x_reg(0), Term::small_int(123));
    }

    #[test]
    fn catch_end_pops_handler_clears_source_and_preserves_x0() {
        let code = label20_module();
        let mut process = Process::new(1, 32);
        push_frame(&mut process, &code);
        catch_(&mut process, &code, &Operand::Y(0), &Operand::Label(20)).expect("catch");
        process.set_x_reg(0, Term::small_int(55));
        process
            .stack_mut()
            .set_y_reg(0, Term::small_int(66))
            .expect("Y0");
        assert_eq!(
            catch_end(&mut process, &Operand::Y(0)),
            Ok(InstructionOutcome::Continue)
        );
        assert_eq!(process.exception_handler_count(), 0);
        assert_eq!(process.current_exception(), None);
        assert_eq!(process.x_reg(0), Term::small_int(55));
        assert_eq!(process.stack().y_reg(0), Ok(Term::NIL));
    }

    #[test]
    fn catch_end_clears_exception_state_after_caught_exception() {
        let code = label20_module();
        let mut process = Process::new(1, 32);
        push_frame(&mut process, &code);
        try_(&mut process, &code, &Operand::Y(0), &Operand::Label(20)).expect("outer try");
        catch_(&mut process, &code, &Operand::Y(0), &Operand::Label(20)).expect("catch");
        raise_exception(
            &mut process,
            exception(Atom::THROW, Term::small_int(123), Term::NIL),
        )
        .expect("caught");
        assert!(process.current_exception().is_some());
        assert_eq!(
            catch_end(&mut process, &Operand::Y(0)),
            Ok(InstructionOutcome::Continue)
        );
        assert_eq!(process.current_exception(), None);
        assert_eq!(process.exception_handler_count(), 1);
        assert_eq!(process.stack().y_reg(0), Ok(Term::NIL));
    }
}
