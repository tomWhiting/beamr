//! Opcode dispatch table.
//!
//! Maps decoded BEAM instructions to handler functions. Foundational opcodes
//! live in [`core`]; later opcode families can add sibling modules without
//! changing the execution loop.

pub mod core;

use crate::error::ExecError;
use crate::interpreter::InstructionOutcome;
use crate::loader::Instruction;
use crate::module::Module;
use crate::process::Process;

/// Dispatch one already-fetched instruction.
pub fn dispatch(
    process: &mut Process,
    module: &Module,
    instruction: &Instruction,
    next_ip: usize,
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
            core::call_ext(process, module, arity, import, next_ip, true)
        }
        Instruction::CallExtOnly { arity, import } => {
            core::call_ext(process, module, arity, import, next_ip, false)
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
        } => core::call_ext_last(process, module, arity, import, deallocate),
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
