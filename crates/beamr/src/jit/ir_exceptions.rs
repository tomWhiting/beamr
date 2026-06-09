//! Exception lowering helpers for JIT-generated BEAM code.
//!
//! The JIT uses an explicit return-code convention instead of native stack
//! unwinding. Compiled functions return `(status, value)`: status `0` is a
//! normal return, status `1` is an exception, and the value carries the raw
//! exception reason while the full `{class, reason, stacktrace}` payload remains
//! in the process exception state.

use crate::atom::Atom;
use crate::process::{Exception, Process, RawStackEntry};
use crate::term::Term;
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{Block, FuncRef, InstBuilder, Value, types};
use cranelift_frontend::FunctionBuilder;

use super::compiler::JitError;
use super::ir_common::{Register, read_register_term, register_operand, write_register_term};

pub(crate) const JIT_STATUS_NORMAL: u8 = 0;
pub(crate) const JIT_STATUS_EXCEPTION: u8 = 1;
pub(crate) const JIT_STATUS_DEOPT: u8 = 2;
pub(crate) const JIT_STATUS_YIELD: u8 = 3;

/// Native ABI representation for the JIT `(status, value)` return convention.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct JitReturn {
    pub(crate) status: u8,
    pub(crate) _padding: [u8; 7],
    pub(crate) value: u64,
}

impl JitReturn {
    pub(crate) const fn normal(value: u64) -> Self {
        Self {
            status: JIT_STATUS_NORMAL,
            _padding: [0; 7],
            value,
        }
    }

    pub(crate) const fn exception(value: u64) -> Self {
        Self {
            status: JIT_STATUS_EXCEPTION,
            _padding: [0; 7],
            value,
        }
    }

    pub(crate) const fn deopt(value: u64) -> Self {
        Self {
            status: JIT_STATUS_DEOPT,
            _padding: [0; 7],
            value,
        }
    }

    pub(crate) const fn yield_(value: u64) -> Self {
        Self {
            status: JIT_STATUS_YIELD,
            _padding: [0; 7],
            value,
        }
    }
}

/// Per-try scope data retained while lowering a function.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TryCatchFrame {
    pub(crate) catch_block: Block,
    pub(crate) class_register: Register,
    pub(crate) reason_register: Register,
    pub(crate) trace_register: Register,
}

/// Compile-time MFA metadata used when a compiled frame propagates an exception.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CompiledFrameInfo {
    pub(crate) module: Atom,
    pub(crate) function: Atom,
    pub(crate) arity: u8,
}

/// SSA values read from the catch registers by `try_case`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CaughtExceptionValues {
    pub(crate) class: Value,
    pub(crate) reason: Value,
    pub(crate) trace: Value,
}

#[derive(Default)]
pub(crate) struct ExceptionLoweringState {
    try_stack: Vec<TryCatchFrame>,
}

impl ExceptionLoweringState {
    pub(crate) fn current_frame(&self) -> Option<TryCatchFrame> {
        self.try_stack.last().copied()
    }

    pub(crate) fn translate_try(
        &mut self,
        catch_block: Block,
        destination: &crate::loader::decode::Operand,
    ) -> Result<TryCatchFrame, JitError> {
        let class_register = register_operand(destination)?;
        let (base, is_y) = match class_register {
            Register::Y(index) => (index, true),
            Register::X(index) => (index, false),
        };
        if !is_y {
            return Err(JitError::UnsupportedOperand {
                operand: "try destination must be a Y register".to_owned(),
            });
        }
        let Some(reason_index) = base.checked_add(1) else {
            return Err(JitError::UnsupportedOperand {
                operand: format!("try Y register triplet out of range: {base}"),
            });
        };
        let Some(trace_index) = base.checked_add(2) else {
            return Err(JitError::UnsupportedOperand {
                operand: format!("try Y register triplet out of range: {base}"),
            });
        };
        let frame = TryCatchFrame {
            catch_block,
            class_register,
            reason_register: Register::Y(reason_index),
            trace_register: Register::Y(trace_index),
        };
        self.try_stack.push(frame);
        Ok(frame)
    }

    pub(crate) fn translate_try_end(&mut self) -> Result<(), JitError> {
        self.try_stack
            .pop()
            .map(|_| ())
            .ok_or_else(|| JitError::UnsupportedOpcode {
                opcode: "try_end without active try".to_owned(),
            })
    }

    pub(crate) fn translate_try_case(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        register_file: Value,
    ) -> Result<CaughtExceptionValues, JitError> {
        let frame = self
            .try_stack
            .pop()
            .ok_or_else(|| JitError::UnsupportedOpcode {
                opcode: "try_case without active try".to_owned(),
            })?;
        Ok(read_caught_exception(builder, register_file, frame))
    }
}

pub(crate) fn read_caught_exception(
    builder: &mut FunctionBuilder<'_>,
    register_file: Value,
    frame: TryCatchFrame,
) -> CaughtExceptionValues {
    CaughtExceptionValues {
        class: read_register_term(builder, register_file, frame.class_register),
        reason: read_register_term(builder, register_file, frame.reason_register),
        trace: read_register_term(builder, register_file, frame.trace_register),
    }
}

#[derive(Clone, Copy)]
pub(crate) struct ExceptionHelpers {
    pub(crate) class: FuncRef,
    pub(crate) reason: FuncRef,
    pub(crate) trace: FuncRef,
    pub(crate) clear: FuncRef,
    pub(crate) add_frame: FuncRef,
}

pub(crate) fn return_status(builder: &mut FunctionBuilder<'_>, status: u8, value: Value) {
    let status = builder.ins().iconst(types::I8, i64::from(status));
    builder.ins().return_(&[status, value]);
}

pub(crate) fn return_status_raw(builder: &mut FunctionBuilder<'_>, status: u8, raw: i64) {
    let value = builder.ins().iconst(types::I64, raw);
    return_status(builder, status, value);
}

pub(crate) fn dispatch_exception_status(
    builder: &mut FunctionBuilder<'_>,
    helpers: ExceptionHelpers,
    frame: Option<TryCatchFrame>,
    compiled_frame: CompiledFrameInfo,
    process: Value,
    register_file: Value,
    status: Value,
    value: Value,
    continuation: Block,
) {
    let is_exception =
        builder
            .ins()
            .icmp_imm(IntCC::Equal, status, i64::from(JIT_STATUS_EXCEPTION));
    let exception_block = builder.create_block();
    builder
        .ins()
        .brif(is_exception, exception_block, &[], continuation, &[]);
    builder.switch_to_block(exception_block);

    if let Some(frame) = frame {
        let class = call_unary(builder, helpers.class, process);
        let reason = call_unary(builder, helpers.reason, process);
        let trace = call_unary(builder, helpers.trace, process);
        write_register_term(builder, register_file, frame.class_register, class);
        write_register_term(builder, register_file, frame.reason_register, reason);
        write_register_term(builder, register_file, frame.trace_register, trace);
        builder.ins().call(helpers.clear, &[process]);
        builder.ins().jump(frame.catch_block, &[]);
        let unreachable = builder.create_block();
        builder.switch_to_block(unreachable);
    } else {
        let module = builder
            .ins()
            .iconst(types::I64, i64::from(compiled_frame.module.index()));
        let function = builder
            .ins()
            .iconst(types::I64, i64::from(compiled_frame.function.index()));
        let arity = builder
            .ins()
            .iconst(types::I64, i64::from(compiled_frame.arity));
        builder
            .ins()
            .call(helpers.add_frame, &[process, module, function, arity]);
        return_status(builder, JIT_STATUS_EXCEPTION, value);
    }

    builder.switch_to_block(continuation);
}

fn call_unary(builder: &mut FunctionBuilder<'_>, helper: FuncRef, process: Value) -> Value {
    let inst = builder.ins().call(helper, &[process]);
    builder.inst_results(inst)[0]
}

pub(crate) extern "C" fn jit_exception_class(process: *mut Process) -> u64 {
    process_exception(process).map_or(Term::NIL.raw(), |exception| exception.class.raw())
}

pub(crate) extern "C" fn jit_exception_reason(process: *mut Process) -> u64 {
    process_exception(process).map_or(Term::NIL.raw(), |exception| exception.reason.raw())
}

pub(crate) extern "C" fn jit_exception_trace(process: *mut Process) -> u64 {
    process_exception(process).map_or(Term::NIL.raw(), |exception| exception.stacktrace.raw())
}

pub(crate) extern "C" fn jit_clear_exception(process: *mut Process) {
    let Some(process) = process_from_abi(process) else {
        return;
    };
    process.set_current_exception(None);
    process.clear_raw_stacktrace();
}

pub(crate) extern "C" fn jit_add_compiled_frame(
    process: *mut Process,
    module: u64,
    function: u64,
    arity: u64,
) {
    let Some(process) = process_from_abi(process) else {
        return;
    };
    let Ok(module) = u32::try_from(module) else {
        return;
    };
    let Ok(function) = u32::try_from(function) else {
        return;
    };
    let Ok(arity) = u8::try_from(arity) else {
        return;
    };
    let Some(current_module) = process.current_module().cloned() else {
        return;
    };
    let mut stacktrace = process.raw_stacktrace().to_vec();
    stacktrace.push(RawStackEntry {
        module: current_module,
        ip: 0,
        mfa: Some((Atom::new(module), Atom::new(function), arity)),
        location_info: Term::NIL,
        compiled: true,
    });
    process.set_raw_stacktrace(stacktrace);
}

fn process_exception(process: *mut Process) -> Option<Exception> {
    process_from_abi(process).and_then(|process| process.current_exception())
}

fn process_from_abi(process: *mut Process) -> Option<&'static mut Process> {
    super::runtime::process_from_abi(process)
}
