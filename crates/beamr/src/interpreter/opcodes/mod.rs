//! Opcode dispatch table.
//!
//! Maps decoded BEAM instructions to handler functions. Foundational opcodes
//! live in [`core`]; later opcode families can add sibling modules without
//! changing the execution loop.

pub mod binary;
pub mod closures;
pub mod core;
pub mod guards;
pub mod messaging;

use std::sync::{Arc, Mutex};

use crate::error::ExecError;
use crate::interpreter::{InstructionOutcome, NativeServices};
use crate::loader::Instruction;
use crate::module::{Module, ModuleRegistry};
use crate::process::Process;
use crate::timer::TimerWheel;

/// Optional runtime context passed alongside instruction dispatch.
struct DispatchCtx<'a> {
    receiver: Option<&'a mut Process>,
    timers: Option<&'a Arc<Mutex<TimerWheel>>>,
    registry: Option<&'a ModuleRegistry>,
    services: Option<&'a NativeServices>,
}

/// Dispatch one already-fetched instruction.
pub fn dispatch(
    process: &mut Process,
    module: &Module,
    instruction: &Instruction,
    next_ip: usize,
    registry: Option<&ModuleRegistry>,
) -> Result<InstructionOutcome, ExecError> {
    dispatch_with_receiver(process, module, instruction, next_ip, None, registry)
}

/// Dispatch one instruction with optional timer services for native BIFs.
pub fn dispatch_with_timer_services(
    process: &mut Process,
    module: &Module,
    instruction: &Instruction,
    next_ip: usize,
    timers: Option<&Arc<Mutex<TimerWheel>>>,
    registry: Option<&ModuleRegistry>,
) -> Result<InstructionOutcome, ExecError> {
    dispatch_common(process, module, instruction, next_ip, DispatchCtx {
        receiver: None, timers, registry, services: None,
    })
}

/// Dispatch one instruction with full native services for BIFs.
pub fn dispatch_with_services(
    process: &mut Process,
    module: &Module,
    instruction: &Instruction,
    next_ip: usize,
    services: &NativeServices,
    registry: Option<&ModuleRegistry>,
) -> Result<InstructionOutcome, ExecError> {
    dispatch_common(process, module, instruction, next_ip, DispatchCtx {
        receiver: None, timers: services.timers.as_ref(), registry, services: Some(services),
    })
}

/// Dispatch one already-fetched instruction with an optional send target process.
///
/// The single-process execution loop calls [`dispatch`] and therefore treats a
/// missing send target as BEAM's silent drop. Scheduler integration can pass the
/// resolved target process here until process ownership/run-queue infrastructure
/// provides a richer execution context.
pub fn dispatch_with_receiver(
    process: &mut Process,
    module: &Module,
    instruction: &Instruction,
    next_ip: usize,
    receiver: Option<&mut Process>,
    registry: Option<&ModuleRegistry>,
) -> Result<InstructionOutcome, ExecError> {
    dispatch_common(process, module, instruction, next_ip, DispatchCtx {
        receiver, timers: None, registry, services: None,
    })
}

fn dispatch_common(
    process: &mut Process,
    module: &Module,
    instruction: &Instruction,
    next_ip: usize,
    ctx: DispatchCtx<'_>,
) -> Result<InstructionOutcome, ExecError> {
    match instruction {
        Instruction::Label { label } => core::label(*label),
        Instruction::FuncInfo {
            module,
            function,
            arity,
        } => core::func_info(process, module, function, arity),
        Instruction::Move {
            source,
            destination,
        } => core::move_(process, source, destination),
        Instruction::Call { arity, label } => {
            core::call(process, module, arity, label, next_ip, true)
        }
        Instruction::CallOnly { arity, label } => {
            core::call(process, module, arity, label, next_ip, false)
        }
        Instruction::CallExt { arity, import } => {
            let ext = core::ExtCallContext { timers: ctx.timers, services: ctx.services };
            core::call_ext(process, module, arity, import, next_ip, true, &ext)
        }
        Instruction::CallExtOnly { arity, import } => {
            let ext = core::ExtCallContext { timers: ctx.timers, services: ctx.services };
            core::call_ext(process, module, arity, import, next_ip, false, &ext)
        }
        Instruction::CallLast {
            arity,
            label,
            deallocate,
        } => core::call_last(process, module, arity, label, deallocate),
        Instruction::CallExtLast {
            arity,
            import,
            deallocate,
        } => {
            let ext = core::ExtCallContext { timers: ctx.timers, services: ctx.services };
            core::call_ext_last(process, module, arity, import, deallocate, &ext)
        }
        Instruction::Return => core::return_(process),
        Instruction::Allocate { stack_need, .. } => core::allocate(process, module, stack_need),
        Instruction::AllocateHeap {
            stack_need,
            heap_need,
            ..
        } => core::allocate_heap(process, module, stack_need, heap_need),
        Instruction::AllocateZero { stack_need, .. } => {
            core::allocate_zero(process, module, stack_need)
        }
        Instruction::Deallocate { words } => core::deallocate(process, words),
        Instruction::TestHeap { heap_need, .. } => core::test_heap(process, heap_need),
        Instruction::PutList {
            head,
            tail,
            destination,
        } => core::put_list(process, head, tail, destination),
        Instruction::PutTuple2 {
            destination,
            elements,
        } => core::put_tuple2(process, destination, elements),
        Instruction::GetTupleElement {
            source,
            index,
            destination,
        } => core::get_tuple_element(process, source, index, destination),
        Instruction::GetHd {
            source,
            destination,
        } => guards::get_hd(process, source, destination),
        Instruction::GetTl {
            source,
            destination,
        } => guards::get_tl(process, source, destination),
        Instruction::TypeTest { op, fail, value } => {
            guards::type_test(process, module, *op, fail, value)
        }
        Instruction::Comparison {
            op,
            fail,
            left,
            right,
        } => guards::comparison(process, module, *op, fail, left, right),
        Instruction::TestArity { fail, tuple, arity } => {
            guards::test_arity(process, module, fail, tuple, arity)
        }
        Instruction::SelectVal { value, fail, list } => {
            guards::select_val(process, module, value, fail, list)
        }
        Instruction::SelectTupleArity { value, fail, list } => {
            guards::select_tuple_arity(process, module, value, fail, list)
        }
        Instruction::Jump { target } => guards::jump(module, target),
        Instruction::Bif { op, operands } => guards::bif(process, module, *op, operands),
        Instruction::Send => messaging::send(process, ctx.receiver),
        Instruction::LoopRec { fail, destination } => {
            messaging::loop_rec(process, module, fail, destination)
        }
        Instruction::LoopRecEnd { fail } => messaging::loop_rec_end(process, module, fail),
        Instruction::RemoveMessage => messaging::remove_message(process),
        Instruction::Wait { fail } => messaging::wait(process, module, fail),
        Instruction::WaitTimeout { fail, timeout } => {
            messaging::wait_timeout(process, module, fail, timeout)
        }
        Instruction::Timeout => messaging::timeout(process),
        Instruction::Try { destination, label } => {
            messaging::try_(process, module, destination, label)
        }
        Instruction::TryEnd { source } => messaging::try_end(process, source),
        Instruction::TryCase { source } => messaging::try_case(process, source),
        Instruction::TryCaseEnd { source } => messaging::try_case_end(process, source),
        Instruction::Raise { stacktrace, reason } => messaging::raise(process, stacktrace, reason),
        Instruction::Badmatch { value } => messaging::badmatch(process, value),
        Instruction::CaseEnd { value } => messaging::case_end(process, value),
        Instruction::IfEnd => messaging::if_end(process),
        Instruction::Line { .. } => Ok(InstructionOutcome::Continue),
        Instruction::BinaryOp { op, operands } => binary::binary_op(process, module, *op, operands),
        Instruction::MapOp { op, operands } => closures::map_op(process, module, *op, operands),
        Instruction::MakeFun { operands } => closures::make_fun(process, module, operands),
        Instruction::CallFun { arity } => closures::call_fun(process, module, arity, next_ip),
        Instruction::Apply { arity } => {
            let registry = ctx.registry.ok_or(ExecError::InvalidOperand("apply: registry required"))?;
            closures::apply(process, registry, arity, next_ip, module.name)
        }
        Instruction::ApplyLast { arity, deallocate } => {
            let registry =
                ctx.registry.ok_or(ExecError::InvalidOperand("apply_last: registry required"))?;
            closures::apply_last(process, registry, arity, deallocate, next_ip)
        }
        Instruction::Generic { opcode, .. } => Err(ExecError::UnknownOpcode { opcode: *opcode }),
        other => Err(ExecError::UnsupportedOpcode {
            name: instruction_name(other),
        }),
    }
}

fn instruction_name(instruction: &Instruction) -> &'static str {
    match instruction {
        Instruction::GetHd { .. } => "get_hd",
        Instruction::GetTl { .. } => "get_tl",
        Instruction::TypeTest { .. } => "type_test",
        Instruction::Comparison { .. } => "comparison",
        Instruction::TestArity { .. } => "test_arity",
        Instruction::SelectVal { .. } => "select_val",
        Instruction::SelectTupleArity { .. } => "select_tuple_arity",
        Instruction::Jump { .. } => "jump",
        Instruction::Bif { .. } => "bif",
        Instruction::Send => "send",
        Instruction::RemoveMessage => "remove_message",
        Instruction::Timeout => "timeout",
        Instruction::LoopRec { .. } => "loop_rec",
        Instruction::LoopRecEnd { .. } => "loop_rec_end",
        Instruction::Wait { .. } => "wait",
        Instruction::WaitTimeout { .. } => "wait_timeout",
        Instruction::Catch { .. } => "catch",
        Instruction::CatchEnd { .. } => "catch_end",
        Instruction::Try { .. } => "try",
        Instruction::TryEnd { .. } => "try_end",
        Instruction::TryCase { .. } => "try_case",
        Instruction::TryCaseEnd { .. } => "try_case_end",
        Instruction::BinaryOp { .. } => "binary_op",
        Instruction::MapOp { .. } => "map_op",
        Instruction::MakeFun { .. } => "make_fun",
        Instruction::CallFun { .. } => "call_fun",
        Instruction::CallFun2 { .. } => "call_fun2",
        Instruction::Apply { .. } => "apply",
        Instruction::ApplyLast { .. } => "apply_last",
        Instruction::Badmatch { .. } => "badmatch",
        Instruction::Badrecord { .. } => "badrecord",
        Instruction::CaseEnd { .. } => "case_end",
        Instruction::IfEnd => "if_end",
        Instruction::Raise { .. } => "raise",
        Instruction::RawRaise => "raw_raise",
        Instruction::Line { .. } => "line",
        Instruction::Trim { .. } => "trim",
        Instruction::OnLoad => "on_load",
        Instruction::BuildStacktrace => "build_stacktrace",
        Instruction::Swap { .. } => "swap",
        Instruction::InitYregs { .. } => "init_yregs",
        Instruction::NifStart => "nif_start",
        Instruction::UpdateRecord { .. } => "update_record",
        _ => "implemented_core_opcode",
    }
}
