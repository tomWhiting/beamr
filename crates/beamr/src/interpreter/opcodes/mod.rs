//! Opcode dispatch table.
//!
//! Maps decoded BEAM instructions to handler functions. Foundational opcodes
//! live in [`core`]; later opcode families can add sibling modules without
//! changing the execution loop.

#[path = "binary/mod.rs"]
pub mod binary;
pub mod closures;
pub mod core;
pub mod exceptions;
pub mod floats;
pub mod guards;
pub mod messaging;
pub mod recv;
pub mod trampoline;

use std::sync::{Arc, Mutex};

use crate::atom::AtomTable;
use crate::error::ExecError;
use crate::interpreter::{InstructionOutcome, NativeServices};
use crate::jit::JitCache;
use crate::loader::Instruction;
use crate::loader::decode::Operand;
use crate::module::{Module, ModuleRegistry};
use crate::process::Process;
use crate::term::Term;
use crate::timer::TimerWheel;

/// Optional runtime context passed alongside instruction dispatch.
struct DispatchCtx<'a> {
    receiver: Option<&'a mut Process>,
    timers: Option<&'a Arc<Mutex<TimerWheel>>>,
    registry: Option<&'a ModuleRegistry>,
    services: Option<&'a NativeServices>,
    atom_table: Option<&'a AtomTable>,
    jit_cache: Option<&'a JitCache>,
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
    dispatch_common(
        process,
        module,
        instruction,
        next_ip,
        DispatchCtx {
            receiver: None,
            timers,
            registry,
            services: None,
            atom_table: None,
            jit_cache: None,
        },
    )
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
    dispatch_common(
        process,
        module,
        instruction,
        next_ip,
        DispatchCtx {
            receiver: None,
            timers: services.timers.as_ref(),
            registry,
            services: Some(services),
            atom_table: services.atom_table.as_deref(),
            jit_cache: services.jit_cache.as_deref(),
        },
    )
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
    dispatch_common(
        process,
        module,
        instruction,
        next_ip,
        DispatchCtx {
            receiver,
            timers: None,
            registry,
            services: None,
            atom_table: None,
            jit_cache: None,
        },
    )
}

fn dispatch_common(
    process: &mut Process,
    module: &Module,
    instruction: &Instruction,
    next_ip: usize,
    ctx: DispatchCtx<'_>,
) -> Result<InstructionOutcome, ExecError> {
    if process.has_native_continuation() {
        return trampoline::handle_native_continuation(process, module, ctx.registry);
    }
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
        } => core::move_(process, module, source, destination),
        Instruction::Fmove { source, dest } => floats::fmove(process, module, source, dest),
        Instruction::Fconv { source, dest } => floats::fconv(process, module, source, dest),
        Instruction::Fadd {
            fail: _,
            left,
            right,
            dest,
        } => floats::fadd(process, left, right, dest),
        Instruction::Fsub {
            fail: _,
            left,
            right,
            dest,
        } => floats::fsub(process, left, right, dest),
        Instruction::Fmul {
            fail: _,
            left,
            right,
            dest,
        } => floats::fmul(process, left, right, dest),
        Instruction::Fdiv {
            fail: _,
            left,
            right,
            dest,
        } => floats::fdiv(process, left, right, dest),
        Instruction::Fnegate {
            fail: _,
            source,
            dest,
        } => floats::fnegate(process, source, dest),
        Instruction::Swap { left, right } => core::swap(process, module, left, right),
        Instruction::Call { arity, label } => core::call(
            process,
            module,
            arity,
            label,
            next_ip,
            true,
            ctx.jit_cache,
            ctx.registry,
        ),
        Instruction::CallOnly { arity, label } => core::call(
            process,
            module,
            arity,
            label,
            next_ip,
            false,
            ctx.jit_cache,
            ctx.registry,
        ),
        Instruction::CallExt { arity, import } => {
            let ext = core::ExtCallContext {
                timers: ctx.timers,
                services: ctx.services,
                registry: ctx.registry,
                atom_table: ctx.atom_table,
                jit_cache: ctx.jit_cache,
            };
            core::call_ext(process, module, arity, import, next_ip, true, &ext)
        }
        Instruction::CallExtOnly { arity, import } => {
            let ext = core::ExtCallContext {
                timers: ctx.timers,
                services: ctx.services,
                registry: ctx.registry,
                atom_table: ctx.atom_table,
                jit_cache: ctx.jit_cache,
            };
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
            let ext = core::ExtCallContext {
                timers: ctx.timers,
                services: ctx.services,
                registry: ctx.registry,
                atom_table: ctx.atom_table,
                jit_cache: ctx.jit_cache,
            };
            core::call_ext_last(process, module, arity, import, deallocate, &ext)
        }
        Instruction::Return => core::return_(process),
        Instruction::Allocate { stack_need, .. } => core::allocate(process, module, stack_need),
        Instruction::AllocateHeap {
            stack_need,
            heap_need,
            live,
        } => core::allocate_heap(process, module, stack_need, heap_need, live),
        Instruction::AllocateZero { stack_need, .. } => {
            core::allocate_zero(process, module, stack_need)
        }
        Instruction::Deallocate { words } => core::deallocate(process, words),
        Instruction::Trim { words, remaining } => core::trim(process, words, remaining),
        Instruction::TestHeap { heap_need, live } => core::test_heap(process, heap_need, live),
        Instruction::PutList {
            head,
            tail,
            destination,
        } => core::put_list(process, module, head, tail, destination),
        Instruction::PutTuple2 {
            destination,
            elements,
        } => core::put_tuple2(process, module, destination, elements),
        Instruction::GetTupleElement {
            source,
            index,
            destination,
        } => core::get_tuple_element(process, module, source, index, destination),
        Instruction::GetList { source, head, tail } => {
            guards::get_list(process, module, source, head, tail)
        }
        Instruction::UpdateRecord { operands } => core::update_record(process, module, operands),
        Instruction::GetHd {
            source,
            destination,
        } => guards::get_hd(process, module, source, destination),
        Instruction::GetTl {
            source,
            destination,
        } => guards::get_tl(process, module, source, destination),
        Instruction::TypeTest { op, fail, value } => {
            guards::type_test(process, module, *op, fail, value)
        }
        Instruction::Comparison {
            op,
            fail,
            left,
            right,
        } => guards::comparison(process, module, *op, fail, left, right, ctx.atom_table),
        Instruction::TestArity { fail, tuple, arity } => {
            guards::test_arity(process, module, fail, tuple, arity)
        }
        Instruction::IsTaggedTuple {
            fail,
            value,
            arity,
            tag,
        } => guards::is_tagged_tuple(process, module, fail, value, arity, tag),
        Instruction::SelectVal { value, fail, list } => {
            guards::select_val(process, module, value, fail, list)
        }
        Instruction::SelectTupleArity { value, fail, list } => {
            guards::select_tuple_arity(process, module, value, fail, list)
        }
        Instruction::Jump { target } => guards::jump(module, target),
        Instruction::Bif { op, operands } => guards::bif(process, module, *op, operands),
        Instruction::Send => messaging::send(
            process,
            ctx.receiver,
            ctx.services
                .and_then(|services| services.distribution_send.as_deref()),
        ),
        Instruction::LoopRec { fail, destination } => {
            messaging::loop_rec(process, module, fail, destination)
        }
        Instruction::LoopRecEnd { fail } => messaging::loop_rec_end(process, module, fail),
        Instruction::RemoveMessage => messaging::remove_message(process),
        Instruction::Wait { fail } => messaging::wait(process, module, fail),
        Instruction::WaitTimeout { fail, timeout } => {
            messaging::wait_timeout(process, module, fail, timeout)
        }
        Instruction::RecvMarkerReserve { dest } => recv::recv_marker_reserve(process, dest),
        Instruction::RecvMarkerBind { marker, label } => {
            recv::recv_marker_bind(process, module, marker, label)
        }
        Instruction::RecvMarkerClear { marker } => recv::recv_marker_clear(process, module, marker),
        Instruction::RecvMarkerUse { marker } => recv::recv_marker_use(process, module, marker),
        Instruction::Timeout => messaging::timeout(process),
        Instruction::Try { destination, label } => {
            exceptions::try_(process, module, destination, label)
        }
        Instruction::TryEnd { source } => exceptions::try_end(process, source),
        Instruction::TryCase { source } => exceptions::try_case(process, source),
        Instruction::TryCaseEnd { source } => exceptions::try_case_end(process, module, source),
        Instruction::Catch { destination, label } => {
            exceptions::catch_(process, module, destination, label)
        }
        Instruction::CatchEnd { source } => exceptions::catch_end(process, source),
        Instruction::Raise { stacktrace, reason } => {
            exceptions::raise(process, module, stacktrace, reason)
        }
        Instruction::RawRaise => exceptions::raw_raise(process),
        Instruction::Badmatch { value } => exceptions::badmatch(process, module, value),
        Instruction::CaseEnd { value } => exceptions::case_end(process, module, value),
        Instruction::IfEnd => exceptions::if_end(process),
        Instruction::BuildStacktrace => exceptions::build_stacktrace(process),
        Instruction::Line { .. }
        | Instruction::Generic {
            name: "executable_line",
            ..
        } => Ok(InstructionOutcome::Continue),
        Instruction::BinaryOp { op, operands } => binary::binary_op(process, module, *op, operands),
        Instruction::MapOp { op, operands } => {
            closures::map_op(process, module, *op, operands, ctx.atom_table)
        }
        Instruction::MakeFun { operands } => closures::make_fun(process, module, operands),
        Instruction::CallFun { arity } => {
            closures::call_fun(process, module, arity, next_ip, ctx.registry)
        }
        Instruction::CallFun2 {
            function: _tag,
            arity,
            destination: func,
        } => closures::call_fun2(process, module, func, arity, next_ip, ctx.registry),
        Instruction::Apply { arity } => {
            let registry = ctx
                .registry
                .ok_or(ExecError::InvalidOperand("apply: registry required"))?;
            closures::apply(process, registry, arity, next_ip, module.name)
        }
        Instruction::ApplyLast { arity, deallocate } => {
            let registry = ctx
                .registry
                .ok_or(ExecError::InvalidOperand("apply_last: registry required"))?;
            closures::apply_last(process, registry, arity, deallocate, next_ip)
        }
        Instruction::OnLoad => Ok(InstructionOutcome::OnLoadComplete),
        Instruction::InitYregs { registers } => {
            if let Operand::List(regs) = registers {
                for reg in regs {
                    core::write_term(process, reg, Term::NIL)?;
                }
                Ok(InstructionOutcome::Continue)
            } else {
                Err(ExecError::InvalidOperand(
                    "init_yregs: expected register list",
                ))
            }
        }
        Instruction::Generic { opcode, .. } => Err(ExecError::UnknownOpcode { opcode: *opcode }),
        other => Err(ExecError::UnsupportedOpcode {
            name: instruction_name(other),
        }),
    }
}

fn instruction_name(instruction: &Instruction) -> &'static str {
    match instruction {
        Instruction::GetList { .. } => "get_list",
        Instruction::GetHd { .. } => "get_hd",
        Instruction::GetTl { .. } => "get_tl",
        Instruction::Fmove { .. } => "fmove",
        Instruction::Fconv { .. } => "fconv",
        Instruction::Fadd { .. } => "fadd",
        Instruction::Fsub { .. } => "fsub",
        Instruction::Fmul { .. } => "fmul",
        Instruction::Fdiv { .. } => "fdiv",
        Instruction::Fnegate { .. } => "fnegate",
        Instruction::TypeTest { .. } => "type_test",
        Instruction::Comparison { .. } => "comparison",
        Instruction::TestArity { .. } => "test_arity",
        Instruction::IsTaggedTuple { .. } => "is_tagged_tuple",
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
        Instruction::RecvMarkerReserve { .. } => "recv_marker_reserve",
        Instruction::RecvMarkerBind { .. } => "recv_marker_bind",
        Instruction::RecvMarkerClear { .. } => "recv_marker_clear",
        Instruction::RecvMarkerUse { .. } => "recv_marker_use",
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::atom::Atom;
    use crate::interpreter::InstructionOutcome;
    use crate::loader::decode::Operand;
    use crate::module::{Module, ModuleOrigin};
    use crate::process::Process;
    use crate::term::Term;
    use crate::term::boxed::{Float, write_tuple};

    use super::*;

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

    #[test]
    fn dispatch_handles_is_tagged_tuple_without_unknown_opcode() {
        let mut process = Process::new(1, 16);
        let mut tuple_words = [0_u64; 3];
        process.set_x_reg(
            0,
            write_tuple(
                &mut tuple_words,
                &[Term::atom(Atom::OK), Term::small_int(1)],
            )
            .expect("tuple"),
        );
        let module = module(vec![Instruction::Label { label: 9 }]);
        let instruction = Instruction::IsTaggedTuple {
            fail: Operand::Label(9),
            value: Operand::X(0),
            arity: Operand::Unsigned(2),
            tag: Operand::Atom(Some(Atom::OK)),
        };

        assert_eq!(
            dispatch(&mut process, &module, &instruction, 1, None),
            Ok(InstructionOutcome::Continue)
        );
    }

    #[test]
    fn dispatch_handles_float_instructions_without_unknown_opcode() {
        let mut process = Process::new(1, 16);
        process.set_x_reg(0, Term::small_int(41));
        process.set_float_reg(1, 1.0).expect("valid float register");
        let module = module(Vec::new());

        assert_eq!(
            dispatch(
                &mut process,
                &module,
                &Instruction::Fconv {
                    source: Operand::X(0),
                    dest: Operand::FloatRegister(0),
                },
                1,
                None,
            ),
            Ok(InstructionOutcome::Continue)
        );
        assert_eq!(
            dispatch(
                &mut process,
                &module,
                &Instruction::Fadd {
                    fail: Operand::Label(0),
                    left: Operand::FloatRegister(0),
                    right: Operand::FloatRegister(1),
                    dest: Operand::FloatRegister(2),
                },
                2,
                None,
            ),
            Ok(InstructionOutcome::Continue)
        );
        assert_eq!(
            dispatch(
                &mut process,
                &module,
                &Instruction::Fmove {
                    source: Operand::FloatRegister(2),
                    dest: Operand::X(0),
                },
                3,
                None,
            ),
            Ok(InstructionOutcome::Continue)
        );

        let float = Float::new(process.x_reg(0)).expect("boxed float result");
        assert_eq!(float.value(), 42.0);
    }
}
