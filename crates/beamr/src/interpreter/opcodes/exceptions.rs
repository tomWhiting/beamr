//! Exception opcode handlers.

use std::sync::Arc;

use crate::atom::Atom;
use crate::error::ExecError;
use crate::interpreter::InstructionOutcome;
use crate::interpreter::opcodes::core;
use crate::loader::decode::compact::Operand;
use crate::module::Module;
use crate::process::{
    CodePosition, Exception, ExceptionHandler, ExitReason, HandlerKind, Process, RawStackEntry,
    Register,
};
use crate::term::Term;
use crate::term::boxed::{write_cons, write_tuple};

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
    process.clear_raw_stacktrace();
    Ok(InstructionOutcome::Continue)
}

pub fn catch_end(process: &mut Process, source: &Operand) -> Result<InstructionOutcome, ExecError> {
    let destination = y_register(source)?;
    if process.current_exception().is_none() {
        let _ = process.pop_exception_handler();
    }
    process.set_current_exception(None);
    process.clear_raw_stacktrace();
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
        // The exception is consumed into x0-x2 here; the handler owns it
        // from now on. Residual state would otherwise report a later normal
        // exit as a crash and leak this class into a later `raise`. The raw
        // stacktrace is intentionally kept: a `build_stacktrace` in the
        // handler still reads it (try_end/catch_end clear it).
        process.set_current_exception(None);
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

pub fn raw_raise(process: &mut Process) -> Result<InstructionOutcome, ExecError> {
    let class = process.x_reg(0);
    let reason = process.x_reg(1);
    let stacktrace = process.x_reg(2);

    // Validate class is one of the three BEAM exception classes.
    if class != Term::atom(Atom::ERROR)
        && class != Term::atom(Atom::THROW)
        && class != Term::atom(Atom::EXIT_CLASS)
    {
        return Err(ExecError::Badarg);
    }

    raise_exception(
        process,
        Exception {
            class,
            reason,
            stacktrace,
        },
    )
}

pub fn raise(
    process: &mut Process,
    module: &Module,
    stacktrace: &Operand,
    reason: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    let stacktrace = core::read_term(process, module, stacktrace)?;
    let reason = core::read_term(process, module, reason)?;

    // The compiler emits this instruction on the catch-clause fallthrough
    // path, where try_case has just written the caught class to x0 and the
    // failed class dispatch left it untouched — that register is the only
    // place the class still lives once try_case consumes the exception.
    // Anything else in x0 means the instruction ran outside that contract;
    // default to `error`, mirroring OTP's fallback when the stacktrace
    // operand carries no class.
    let x0 = process.x_reg(0);
    let class = if x0 == Term::atom(Atom::ERROR)
        || x0 == Term::atom(Atom::THROW)
        || x0 == Term::atom(Atom::EXIT_CLASS)
    {
        x0
    } else {
        Term::atom(Atom::ERROR)
    };
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
    capture_raw_stacktrace(process);

    if let Some(handler) = process.pop_exception_handler() {
        process.stack_mut().truncate(handler.stack_depth);
        // Continuations whose trampoline return frames were just discarded
        // belong to aborted closure calls and must never resume.
        process.prune_native_continuations(handler.stack_depth);
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

pub fn build_stacktrace(process: &mut Process) -> Result<InstructionOutcome, ExecError> {
    let raw_stacktrace = process.raw_stacktrace().to_vec();
    let mut list = Term::NIL;

    for entry in raw_stacktrace.iter().rev() {
        let (function, arity) = entry
            .mfa
            .map(|(_, function, arity)| (function, arity))
            .or_else(|| entry.module.function_at_ip(entry.ip))
            .unwrap_or((Atom::UNDEFINED, 0));
        let info = if !entry.compiled {
            stacktrace_info(process, entry.module.line_at_ip(entry.ip))?
        } else {
            entry.location_info
        };
        let frame = four_tuple(
            process,
            Term::atom(entry.module.name),
            Term::atom(function),
            Term::small_int(i64::from(arity)),
            info,
        )?;
        list = cons(process, frame, list)?;
    }

    process.set_x_reg(0, list);
    Ok(InstructionOutcome::Continue)
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

fn capture_raw_stacktrace(process: &mut Process) {
    let mut raw_stacktrace = Vec::new();
    if let (Some(module), Some(position)) =
        (process.current_module().cloned(), process.code_position())
    {
        raw_stacktrace.push(RawStackEntry {
            module,
            ip: position.instruction_pointer,
            mfa: process.current_mfa(),
            location_info: Term::NIL,
            compiled: false,
        });
    }
    raw_stacktrace.extend(
        process
            .stack()
            .frames_from_top()
            .map(|frame| RawStackEntry {
                module: Arc::clone(frame.pinned_module()),
                ip: frame.return_ip(),
                mfa: None,
                location_info: Term::NIL,
                compiled: false,
            }),
    );
    process.set_raw_stacktrace(raw_stacktrace);
}

fn stacktrace_info(process: &mut Process, line: Option<u32>) -> Result<Term, ExecError> {
    let Some(line) = line else {
        return Ok(Term::NIL);
    };
    let line_value = Term::small_int(i64::from(line));
    let tuple = two_tuple(process, Term::atom(Atom::LINE), line_value)?;
    cons(process, tuple, Term::NIL)
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

fn four_tuple(
    process: &mut Process,
    first: Term,
    second: Term,
    third: Term,
    fourth: Term,
) -> Result<Term, ExecError> {
    let ptr = process.heap_mut().alloc(5).map_err(ExecError::from)?;
    let words = core::heap_slice(ptr, 5);
    write_tuple(words, &[first, second, third, fourth]).ok_or(ExecError::Badarg)
}

fn cons(process: &mut Process, head: Term, tail: Term) -> Result<Term, ExecError> {
    let ptr = process.heap_mut().alloc(2).map_err(ExecError::from)?;
    let words = core::heap_slice(ptr, 2);
    write_cons(words, head, tail).ok_or(ExecError::Badarg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interpreter::opcodes::dispatch;
    use crate::loader::{Instruction, LineInfo};
    use crate::module::ModuleOrigin;
    use crate::term::boxed::{Cons, Tuple};
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
            origin: ModuleOrigin::Preloaded,
            exports: HashMap::new(),
            label_index,
            code,
            literals: Vec::new(),
            constant_pool: Default::default(),
            resolved_imports: Vec::new(),
            lambdas: Vec::new(),
            string_table: Vec::new(),
            function_table: Vec::new(),
            line_table: Vec::new(),
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

    fn set_current_location(process: &mut Process, module_version: Arc<Module>, ip: usize) {
        process.set_current_module(module_version);
        process.set_code_position(Some(CodePosition {
            module: Atom::OK,
            instruction_pointer: ip,
        }));
        process.set_current_mfa(Some((Atom::OK, Atom::BADARG, 1)));
    }

    #[test]
    fn raw_stacktrace_captures_current_and_return_frames_before_unwind() {
        let code = label20_module();
        let module_version = Arc::new(code.clone());
        let catch_module_version = Arc::new(code.clone());
        let mut process = Process::new(1, 64);

        set_current_location(&mut process, Arc::clone(&module_version), 99);
        push_three_frames(&mut process, &module_version);
        try_(&mut process, &code, &Operand::X(0), &Operand::Label(20)).expect("try");

        raise_exception(
            &mut process,
            exception(Atom::ERROR, Term::atom(Atom::BADARG), Term::NIL),
        )
        .expect("try caught");

        assert_raw_trace(&process, &module_version, 99);

        let mut process = Process::new(2, 64);
        set_current_location(&mut process, Arc::clone(&catch_module_version), 99);
        push_three_frames(&mut process, &catch_module_version);
        catch_(&mut process, &code, &Operand::Y(0), &Operand::Label(20)).expect("catch");

        raise_exception(
            &mut process,
            exception(Atom::THROW, Term::atom(Atom::BADARG), Term::NIL),
        )
        .expect("catch caught");

        assert_raw_trace(&process, &catch_module_version, 99);
    }

    fn push_three_frames(process: &mut Process, module_version: &Arc<Module>) {
        process
            .stack_mut()
            .push_frame(Atom::OK, 10, Arc::clone(module_version), 1)
            .expect("oldest frame");
        process
            .stack_mut()
            .push_frame(Atom::OK, 20, Arc::clone(module_version), 1)
            .expect("middle frame");
        process
            .stack_mut()
            .push_frame(Atom::OK, 30, Arc::clone(module_version), 1)
            .expect("newest frame");
    }

    fn assert_raw_trace(process: &Process, module_version: &Arc<Module>, current_ip: usize) {
        let raw = process.raw_stacktrace();
        assert_eq!(raw.len(), 4);
        assert!(Arc::ptr_eq(&raw[0].module, module_version));
        assert_eq!(raw[0].ip, current_ip);
        assert_eq!(raw[0].mfa, Some((Atom::OK, Atom::BADARG, 1)));
        assert_eq!(raw[1].ip, 30);
        assert_eq!(raw[2].ip, 20);
        assert_eq!(raw[3].ip, 10);
        assert!(
            raw.iter()
                .all(|entry| Arc::ptr_eq(&entry.module, module_version))
        );
    }

    #[test]
    fn try_end_and_catch_end_clear_raw_stacktrace() {
        let code = label20_module();
        let module_version = Arc::new(code.clone());
        let mut process = Process::new(1, 64);
        set_current_location(&mut process, module_version, 7);
        try_(&mut process, &code, &Operand::X(0), &Operand::Label(20)).expect("try");
        raise_exception(
            &mut process,
            exception(Atom::ERROR, Term::atom(Atom::BADARG), Term::NIL),
        )
        .expect("try caught");
        assert!(!process.raw_stacktrace().is_empty());
        try_end(&mut process, &Operand::X(0)).expect("try_end");
        assert!(process.raw_stacktrace().is_empty());

        let module_version = Arc::new(code.clone());
        set_current_location(&mut process, module_version, 8);
        process
            .stack_mut()
            .push_frame(Atom::OK, 0, Arc::new(code.clone()), 1)
            .expect("frame for catch_end source");
        catch_(&mut process, &code, &Operand::Y(0), &Operand::Label(20)).expect("catch");
        raise_exception(
            &mut process,
            exception(Atom::THROW, Term::atom(Atom::BADARG), Term::NIL),
        )
        .expect("catch caught");
        assert!(!process.raw_stacktrace().is_empty());
        catch_end(&mut process, &Operand::Y(0)).expect("catch_end");
        assert!(process.raw_stacktrace().is_empty());
    }

    #[test]
    fn build_stacktrace_empty_raw_trace_sets_nil() {
        let mut process = Process::new(1, 16);

        build_stacktrace(&mut process).expect("empty stacktrace builds");

        assert_eq!(process.x_reg(0), Term::NIL);
    }

    #[test]
    fn build_stacktrace_resolves_mfa_and_line_info() {
        let mut code = label20_module();
        code.function_table = vec![(0, Atom::BADARG, 1), (10, Atom::FLUSH, 2)];
        code.line_table = vec![(0, 0), (10, 1)];
        code.line_info = vec![
            LineInfo { file: 0, line: 123 },
            LineInfo { file: 0, line: 456 },
        ];
        let module_version = Arc::new(code.clone());
        let mut process = Process::new(1, 128);
        set_current_location(&mut process, Arc::clone(&module_version), 0);
        process
            .stack_mut()
            .push_frame(Atom::OK, 10, Arc::clone(&module_version), 1)
            .expect("return frame");
        raise_exception(
            &mut process,
            exception(Atom::ERROR, Term::atom(Atom::BADARG), Term::NIL),
        )
        .expect("uncaught exit captures raw trace");

        build_stacktrace(&mut process).expect("stacktrace builds");

        let cons = Cons::new(process.x_reg(0)).expect("stacktrace cons");
        assert_stacktrace_frame(cons.head(), Atom::OK, Atom::BADARG, 1, 123);
        let tail = Cons::new(cons.tail()).expect("stacktrace tail cons");
        assert_eq!(tail.tail(), Term::NIL);
        assert_stacktrace_frame(tail.head(), Atom::OK, Atom::FLUSH, 2, 456);
        assert!(!process.raw_stacktrace().is_empty());
    }

    #[test]
    fn build_stacktrace_dispatch_is_supported() {
        let code = label20_module();
        let mut process = Process::new(1, 16);

        assert_eq!(
            dispatch(&mut process, &code, &Instruction::BuildStacktrace, 1, None),
            Ok(InstructionOutcome::Continue)
        );
        assert_eq!(process.x_reg(0), Term::NIL);
    }

    fn assert_stacktrace_frame(term: Term, module: Atom, function: Atom, arity: u8, line: i64) {
        let frame = Tuple::new(term).expect("stacktrace frame tuple");
        assert_eq!(frame.arity(), 4);
        assert_eq!(frame.get(0), Some(Term::atom(module)));
        assert_eq!(frame.get(1), Some(Term::atom(function)));
        assert_eq!(frame.get(2), Some(Term::small_int(i64::from(arity))));
        let info = Cons::new(frame.get(3).expect("info list")).expect("line info cons");
        assert_eq!(info.tail(), Term::NIL);
        let line_tuple = Tuple::new(info.head()).expect("line tuple");
        assert_eq!(line_tuple.get(0), Some(Term::atom(Atom::LINE)));
        assert_eq!(line_tuple.get(1), Some(Term::small_int(line)));
    }

    #[test]
    fn build_stacktrace_heap_pressure_returns_heap_full() {
        let code = label20_module();
        let module_version = Arc::new(code.clone());
        let mut process = Process::new(1, 1);
        set_current_location(&mut process, module_version, 0);
        raise_exception(
            &mut process,
            exception(Atom::ERROR, Term::atom(Atom::BADARG), Term::NIL),
        )
        .expect("uncaught exit captures raw trace");

        assert!(matches!(
            build_stacktrace(&mut process),
            Err(ExecError::HeapFull { .. })
        ));
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
    fn try_case_clears_current_exception_once_consumed_into_registers() {
        let code = label20_module();
        let mut process = Process::new(1, 32);
        try_(&mut process, &code, &Operand::X(0), &Operand::Label(20)).expect("try");
        raise_exception(
            &mut process,
            exception(Atom::THROW, Term::small_int(9), Term::NIL),
        )
        .expect("caught");
        assert!(process.current_exception().is_some());
        try_case(&mut process, &Operand::X(0)).expect("expose exception");
        assert_eq!(process.current_exception(), None);
        assert_eq!(process.x_reg(0), Term::atom(Atom::THROW));
        assert_eq!(process.x_reg(1), Term::small_int(9));
        assert_eq!(process.x_reg(2), Term::NIL);
    }

    #[test]
    fn raise_after_try_case_preserves_the_caught_class() {
        // Compiled catch clauses that match no class re-raise with `raise`
        // right after try_case: class still in x0, reason x1, trace x2.
        let code = module(vec![
            Instruction::Label { label: 10 },
            Instruction::Label { label: 20 },
        ]);
        let mut process = Process::new(1, 64);
        try_(&mut process, &code, &Operand::X(10), &Operand::Label(10)).expect("outer try");
        try_(&mut process, &code, &Operand::X(20), &Operand::Label(20)).expect("inner try");
        raise_exception(
            &mut process,
            exception(Atom::EXIT_CLASS, Term::small_int(5), Term::NIL),
        )
        .expect("caught by inner");
        try_case(&mut process, &Operand::X(20)).expect("expose exception");
        assert_eq!(process.current_exception(), None);
        raise(&mut process, &code, &Operand::X(2), &Operand::X(1)).expect("re-raise");
        let rethrown = process.current_exception().expect("outer catch pending");
        assert_eq!(rethrown.class, Term::atom(Atom::EXIT_CLASS));
        assert_eq!(rethrown.reason, Term::small_int(5));
    }

    #[test]
    fn raise_after_handled_exception_does_not_inherit_the_stale_class() {
        let code = module(vec![
            Instruction::Label { label: 10 },
            Instruction::Label { label: 20 },
        ]);
        let mut process = Process::new(1, 64);
        try_(&mut process, &code, &Operand::X(10), &Operand::Label(10)).expect("outer try");
        try_(&mut process, &code, &Operand::X(20), &Operand::Label(20)).expect("inner try");
        raise_exception(
            &mut process,
            exception(Atom::THROW, Term::small_int(1), Term::NIL),
        )
        .expect("caught by inner");
        try_case(&mut process, &Operand::X(20)).expect("expose exception");
        // The handler ran on and x0 no longer holds a class: a fresh raise
        // must not resurrect the handled throw's class.
        process.set_x_reg(0, Term::small_int(99));
        raise(
            &mut process,
            &code,
            &Operand::Integer(0),
            &Operand::Atom(Some(Atom::BADARG)),
        )
        .expect("fresh raise");
        let fresh = process.current_exception().expect("outer catch pending");
        assert_eq!(fresh.class, Term::atom(Atom::ERROR));
        assert_eq!(fresh.reason, Term::atom(Atom::BADARG));
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
