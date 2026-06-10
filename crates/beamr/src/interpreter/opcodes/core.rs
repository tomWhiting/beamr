//! Foundational BEAM opcode handlers.

use std::sync::{Arc, Mutex};

use crate::atom::{Atom, AtomTable};
use crate::error::ExecError;
use crate::gc::{GcError, ensure_space};
use crate::interpreter::InstructionOutcome;
use crate::interpreter::NativeServices;
use crate::jit::JitCache;
use crate::jit::ir_common::JIT_DEOPT_SENTINEL;
use crate::jit::ir_exceptions::{
    JIT_STATUS_DEOPT, JIT_STATUS_EXCEPTION, JIT_STATUS_YIELD, JitReturn,
};
use crate::jit::runtime::JIT_YIELD_SENTINEL;
use crate::loader::decode::compact::Operand;
use crate::module::{Module, ModuleRegistry, ResolvedImportTarget};
use crate::native::ExceptionClass;
use crate::process::{CodePosition, ExitReason, JitRuntimeContext, JitStatus, Process};
use crate::term::Term;
use crate::term::boxed::{Tuple, write_cons, write_tuple};
use crate::timer::TimerWheel;


/// JIT dispatch services shared by local and external call handlers.
#[derive(Copy, Clone)]
pub struct JitDispatchContext<'a> {
    pub jit_cache: Option<&'a JitCache>,
    pub registry: Option<&'a ModuleRegistry>,
}

/// Combined context for external calls carrying timer, facility, registry, and JIT services.
pub struct ExtCallContext<'a> {
    pub timers: Option<&'a Arc<Mutex<TimerWheel>>>,
    pub services: Option<&'a NativeServices>,
    pub registry: Option<&'a ModuleRegistry>,
    pub atom_table: Option<&'a AtomTable>,
    pub jit_cache: Option<&'a JitCache>,
}

pub(super) fn exception_class_atom(class: ExceptionClass) -> Atom {
    match class {
        ExceptionClass::Error => Atom::ERROR,
        ExceptionClass::Throw => Atom::THROW,
        ExceptionClass::Exit => Atom::EXIT_CLASS,
    }
}

pub fn label(_label: u32) -> Result<InstructionOutcome, ExecError> {
    Ok(InstructionOutcome::Continue)
}

pub fn func_info(
    process: &mut Process,
    module: &Operand,
    function: &Operand,
    arity: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    let metadata = (
        operand_atom(module)?,
        operand_atom(function)?,
        operand_u8(arity, "func_info arity")?,
    );
    process.set_current_mfa(Some(metadata));
    Ok(InstructionOutcome::Continue)
}

pub fn move_(
    process: &mut Process,
    module: &Module,
    source: &Operand,
    destination: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    let value = read_term(process, module, source)?;
    write_term(process, destination, value)?;
    Ok(InstructionOutcome::Continue)
}

pub fn swap(
    process: &mut Process,
    module: &Module,
    left: &Operand,
    right: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    let left_value = read_term(process, module, left)?;
    let right_value = read_term(process, module, right)?;
    write_term(process, left, right_value)?;
    write_term(process, right, left_value)?;
    Ok(InstructionOutcome::Continue)
}

pub fn call(
    process: &mut Process,
    module: &Module,
    arity: &Operand,
    label: &Operand,
    return_ip: usize,
    save_return: bool,
    jit_ctx: JitDispatchContext<'_>,
) -> Result<InstructionOutcome, ExecError> {
    let arity = operand_u8(arity, "call arity")?;
    let target = label_ip(module, operand_label(label)?)?;
    if save_return {
        let caller_module = current_module_pin(process, module);
        process
            .stack_mut()
            .push_frame(module.name, return_ip, caller_module, 0)
            .map_err(ExecError::from)?;
    }
    if let Some(outcome) = dispatch_local_jit(
        process,
        module,
        target,
        arity,
        save_return,
        jit_ctx.jit_cache,
        jit_ctx.registry,
    )? {
        return Ok(outcome);
    }
    jump_with_reduction(process, module.name, target)
}

pub fn call_ext(
    process: &mut Process,
    module: &Module,
    arity: &Operand,
    import: &Operand,
    return_ip: usize,
    save_return: bool,
    ctx: &ExtCallContext<'_>,
) -> Result<InstructionOutcome, ExecError> {
    call_external_target(process, module, arity, import, save_return, return_ip, ctx)
}

pub fn call_last(
    process: &mut Process,
    module: &Module,
    arity: &Operand,
    label: &Operand,
    deallocate: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    let _arity = operand_u8(arity, "call_last arity")?;
    deallocate_frame(process, deallocate)?;
    let target = label_ip(module, operand_label(label)?)?;
    jump_with_reduction(process, module.name, target)
}

pub fn call_ext_last(
    process: &mut Process,
    module: &Module,
    arity: &Operand,
    import: &Operand,
    deallocate: &Operand,
    ctx: &ExtCallContext<'_>,
) -> Result<InstructionOutcome, ExecError> {
    deallocate_frame(process, deallocate)?;
    call_external_target(process, module, arity, import, false, 0, ctx)
}

pub fn return_(process: &mut Process) -> Result<InstructionOutcome, ExecError> {
    if process.stack().is_empty() {
        return Ok(InstructionOutcome::Exit(ExitReason::Normal));
    }
    let return_point = process.stack_mut().pop_frame().map_err(ExecError::from)?;
    process.set_current_module(Arc::clone(&return_point.module_version));
    Ok(InstructionOutcome::Jump(CodePosition {
        module: return_point.module,
        instruction_pointer: return_point.ip,
    }))
}

pub fn allocate(
    process: &mut Process,
    module: &Module,
    stack_need: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    push_y_frame(process, module, stack_need)
}

pub fn allocate_heap(
    process: &mut Process,
    module: &Module,
    stack_need: &Operand,
    heap_need: &Operand,
    live: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    test_heap(process, heap_need, live)?;
    push_y_frame(process, module, stack_need)
}

pub fn allocate_zero(
    process: &mut Process,
    module: &Module,
    stack_need: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    push_y_frame(process, module, stack_need)
}

pub fn deallocate(process: &mut Process, words: &Operand) -> Result<InstructionOutcome, ExecError> {
    deallocate_frame(process, words)?;
    Ok(InstructionOutcome::Continue)
}

pub fn trim(
    process: &mut Process,
    words: &Operand,
    remaining: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    let words = operand_u16(words, "trim words")?;
    let remaining = operand_u16(remaining, "trim remaining")?;
    let expected_slots = words.checked_add(remaining).ok_or(ExecError::Badarg)?;
    let current_slots = process
        .stack()
        .current_frame()
        .map_err(ExecError::from)?
        .y_slots();

    if current_slots != expected_slots {
        return Err(ExecError::Badarg);
    }

    process
        .stack_mut()
        .trim_y_regs(remaining)
        .map_err(ExecError::from)?;
    Ok(InstructionOutcome::Continue)
}

pub fn test_heap(
    process: &mut Process,
    heap_need: &Operand,
    live: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    let needed = operand_usize(heap_need, "heap words")?;
    let live = operand_usize(live, "live x registers")?;
    ensure_space(process, needed, live).map_err(gc_error_to_exec)?;
    Ok(InstructionOutcome::Continue)
}

pub(super) fn gc_error_to_exec(error: GcError) -> ExecError {
    match error {
        GcError::HeapFull(error) => ExecError::from(error),
        GcError::InvalidObjectHeader(_) => ExecError::Badarg,
    }
}

pub fn put_list(
    process: &mut Process,
    module: &Module,
    head: &Operand,
    tail: &Operand,
    destination: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    let head = read_term(process, module, head)?;
    let tail = read_term(process, module, tail)?;
    let ptr = process.heap_mut().alloc(2).map_err(ExecError::from)?;
    let heap = heap_slice(ptr, 2);
    let term = write_cons(heap, head, tail).ok_or(ExecError::Badarg)?;
    write_term(process, destination, term)?;
    Ok(InstructionOutcome::Continue)
}

pub fn put_tuple2(
    process: &mut Process,
    module: &Module,
    destination: &Operand,
    elements: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    let Operand::List(element_operands) = elements else {
        return Err(ExecError::InvalidOperand("put_tuple2 elements"));
    };
    let mut values = Vec::with_capacity(element_operands.len());
    for operand in element_operands {
        values.push(read_term(process, module, operand)?);
    }
    let words = values
        .len()
        .checked_add(1)
        .ok_or(ExecError::InvalidOperand("tuple size"))?;
    let ptr = process.heap_mut().alloc(words).map_err(ExecError::from)?;
    let heap = heap_slice(ptr, words);
    let term = write_tuple(heap, &values).ok_or(ExecError::Badarg)?;
    write_term(process, destination, term)?;
    Ok(InstructionOutcome::Continue)
}

pub fn get_tuple_element(
    process: &mut Process,
    module: &Module,
    source: &Operand,
    index: &Operand,
    destination: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    let tuple_term = read_term(process, module, source)?;
    let tuple = Tuple::new(tuple_term).ok_or(ExecError::Badarg)?;
    let index = operand_usize(index, "tuple index")?;
    let value = tuple.get(index).ok_or(ExecError::Badarg)?;
    write_term(process, destination, value)?;
    Ok(InstructionOutcome::Continue)
}

pub fn update_record(
    process: &mut Process,
    module: &Module,
    operands: &[Operand],
) -> Result<InstructionOutcome, ExecError> {
    let parsed = parse_update_record_operands(operands)?;
    let words = parsed
        .arity
        .checked_add(1)
        .ok_or(ExecError::InvalidOperand("update_record tuple size"))?;

    ensure_space(process, words, 256).map_err(gc_error_to_exec)?;

    let source_term = read_term(process, module, parsed.source)?;
    let tuple = Tuple::new(source_term).ok_or(ExecError::Badarg)?;
    if tuple.arity() != parsed.arity {
        return Err(ExecError::Badarg);
    }

    let mut values = Vec::with_capacity(parsed.arity);
    for index in 0..parsed.arity {
        values.push(tuple.get(index).ok_or(ExecError::Badarg)?);
    }

    for update in parsed.updates {
        let slot = update.index.checked_sub(1).ok_or(ExecError::Badarg)?;
        let value = values.get_mut(slot).ok_or(ExecError::Badarg)?;
        *value = read_term(process, module, update.value)?;
    }

    let ptr = process.heap_mut().alloc(words).map_err(ExecError::from)?;
    let heap = heap_slice(ptr, words);
    let term = write_tuple(heap, &values).ok_or(ExecError::Badarg)?;
    write_term(process, parsed.destination, term)?;
    Ok(InstructionOutcome::Continue)
}

struct UpdateRecordOperands<'a> {
    arity: usize,
    source: &'a Operand,
    destination: &'a Operand,
    updates: Vec<UpdateRecordPair<'a>>,
}

struct UpdateRecordPair<'a> {
    index: usize,
    value: &'a Operand,
}

fn parse_update_record_operands(
    operands: &[Operand],
) -> Result<UpdateRecordOperands<'_>, ExecError> {
    if operands.len() < 4 {
        return Err(ExecError::InvalidOperand("update_record operands"));
    }

    let _hint = operand_atom(&operands[0])?;
    let arity = operand_usize(&operands[1], "update_record arity")?;
    let pair_operands = update_record_pair_operands(operands)?;
    let update_count = pair_operands.len() / 2;
    let mut updates = Vec::with_capacity(update_count);
    for pair in pair_operands.chunks_exact(2) {
        updates.push(UpdateRecordPair {
            index: operand_usize(&pair[0], "update_record index")?,
            value: &pair[1],
        });
    }

    Ok(UpdateRecordOperands {
        arity,
        source: &operands[2],
        destination: &operands[3],
        updates,
    })
}

fn update_record_pair_operands(operands: &[Operand]) -> Result<&[Operand], ExecError> {
    if operands.len() == 5
        && let Operand::List(pairs) = &operands[4]
    {
        if pairs.len() % 2 == 0 {
            return Ok(pairs);
        }
        return Err(ExecError::InvalidOperand("update_record pairs"));
    }

    if !(operands.len() - 4).is_multiple_of(2) {
        return Err(ExecError::InvalidOperand("update_record pairs"));
    }
    Ok(&operands[4..])
}

fn call_external_target(
    process: &mut Process,
    module: &Module,
    arity: &Operand,
    import: &Operand,
    save_return: bool,
    return_ip: usize,
    ctx: &ExtCallContext<'_>,
) -> Result<InstructionOutcome, ExecError> {
    let arity = operand_u8(arity, "external call arity")?;
    let import_index = operand_usize(import, "import index")?;
    let resolved = module
        .resolved_imports
        .get(import_index)
        .ok_or(ExecError::InvalidImport {
            index: import_index,
        })?;
    if resolved.arity != arity {
        return Err(ExecError::InvalidOperand("external call arity mismatch"));
    }
    match resolved.target {
        ResolvedImportTarget::Code {
            module: _,
            label: _,
        } => {
            let target_module = resolved.module;
            let target_mod =
                ctx.registry
                    .and_then(|r| r.lookup(target_module))
                    .ok_or(ExecError::Undef {
                        module: target_module,
                        function: resolved.function,
                        arity,
                    })?;
            let instruction_pointer = target_mod.export_ip(resolved.function, resolved.arity)?;
            if save_return {
                let caller_module = current_module_pin(process, module);
                process
                    .stack_mut()
                    .push_frame(module.name, return_ip, caller_module, 0)
                    .map_err(ExecError::from)?;
            }
            if let Some(outcome) = dispatch_external_jit(
                process,
                target_mod.as_ref(),
                instruction_pointer,
                resolved.function,
                arity,
                save_return,
                ctx,
            )? {
                return Ok(outcome);
            }
            let target = CodePosition {
                module: target_module,
                instruction_pointer,
            };
            process.set_current_module(Arc::clone(&target_mod));
            jump_position_with_reduction(process, target)
        }
        ResolvedImportTarget::Deferred {
            module: target_module,
            function,
            arity: target_arity,
        } => {
            let target_mod =
                ctx.registry
                    .and_then(|r| r.lookup(target_module))
                    .ok_or(ExecError::Undef {
                        module: target_module,
                        function,
                        arity: target_arity,
                    })?;
            let instruction_pointer = target_mod.export_ip(function, target_arity)?;
            if save_return {
                let caller_module = current_module_pin(process, module);
                process
                    .stack_mut()
                    .push_frame(module.name, return_ip, caller_module, 0)
                    .map_err(ExecError::from)?;
            }
            if let Some(outcome) = dispatch_external_jit(
                process,
                target_mod.as_ref(),
                instruction_pointer,
                function,
                target_arity,
                save_return,
                ctx,
            )? {
                return Ok(outcome);
            }
            let target = CodePosition {
                module: target_module,
                instruction_pointer,
            };
            process.set_current_module(Arc::clone(&target_mod));
            jump_position_with_reduction(process, target)
        }
        ResolvedImportTarget::Unresolved {
            module,
            function,
            arity,
        } => Err(ExecError::Undef {
            module,
            function,
            arity,
        }),
        ResolvedImportTarget::Denied { .. } => Err(ExecError::Undef {
            module: resolved.module,
            function: resolved.function,
            arity: resolved.arity,
        }),
        ResolvedImportTarget::Native(entry) => super::native_call::call_native_entry(
            process,
            module,
            entry,
            (resolved.module, resolved.function, resolved.arity),
            save_return,
            ctx,
        ),
    }
}

type RawJitFn = extern "C" fn(*mut u64, *mut Process) -> JitReturn;

fn dispatch_local_jit(
    process: &mut Process,
    module: &Module,
    target_ip: usize,
    arity: u8,
    _save_return: bool,
    jit_cache: Option<&JitCache>,
    registry: Option<&ModuleRegistry>,
) -> Result<Option<InstructionOutcome>, ExecError> {
    let Some(cache) = jit_cache else {
        return Ok(None);
    };
    let Some((function, function_arity)) = module.function_at_ip(target_ip) else {
        return Ok(None);
    };
    if function_arity != arity {
        return Ok(None);
    }
    let Some(native) = cache.lookup(module.name, function, arity, module.generation()) else {
        return Ok(None);
    };
    process.set_code_position(Some(CodePosition {
        module: module.name,
        instruction_pointer: target_ip,
    }));
    invoke_jit(process, module, native, registry, jit_cache)
}

fn dispatch_external_jit(
    process: &mut Process,
    target_module: &Module,
    target_ip: usize,
    function: Atom,
    arity: u8,
    _save_return: bool,
    ctx: &ExtCallContext<'_>,
) -> Result<Option<InstructionOutcome>, ExecError> {
    let Some(cache) = ctx.jit_cache else {
        return Ok(None);
    };
    let Some(native) = cache.lookup(
        target_module.name,
        function,
        arity,
        target_module.generation(),
    ) else {
        return Ok(None);
    };
    process.set_code_position(Some(CodePosition {
        module: target_module.name,
        instruction_pointer: target_ip,
    }));
    invoke_jit(process, target_module, native, ctx.registry, ctx.jit_cache)
}

fn invoke_jit(
    process: &mut Process,
    module: &Module,
    native: crate::jit::NativeCode,
    registry: Option<&ModuleRegistry>,
    jit_cache: Option<&JitCache>,
) -> Result<Option<InstructionOutcome>, ExecError> {
    let Some(registry) = registry else {
        return Ok(None);
    };
    let previous_context = process.jit_runtime_context();
    process.set_jit_runtime_context(Some(JitRuntimeContext::new(
        module as *const Module,
        registry as *const ModuleRegistry,
        jit_cache.map_or(std::ptr::null(), |cache| cache as *const JitCache),
    )));
    process.set_jit_status(None);
    let outcome = call_native(process, native);
    process.set_jit_runtime_context(previous_context);
    outcome
}

fn call_native(
    process: &mut Process,
    native: crate::jit::NativeCode,
) -> Result<Option<InstructionOutcome>, ExecError> {
    let register_file = process.x_regs_mut().as_mut_ptr().cast::<u64>();
    // SAFETY: `NativeCode` is produced by `JitCompiler` with the raw ABI
    // `extern "C" fn(*mut u64, *mut Process) -> JitReturn`. `NativeCode` clones
    // keep the owning JIT module alive, and the register/process pointers are
    // valid for the synchronous duration of this call.
    let raw_fn: RawJitFn = unsafe { std::mem::transmute(native.call_ptr()) };
    let returned = raw_fn(register_file, process);
    match process.take_jit_status() {
        Some(JitStatus::Yield) => return Ok(Some(InstructionOutcome::Yield)),
        None => {}
    }
    match returned.status {
        JIT_STATUS_EXCEPTION => {
            return Ok(Some(InstructionOutcome::Exit(ExitReason::Error)));
        }
        JIT_STATUS_DEOPT => return Ok(None),
        JIT_STATUS_YIELD => return Ok(Some(InstructionOutcome::Yield)),
        _ => {}
    }
    let raw = returned.value;
    if raw == JIT_YIELD_SENTINEL as u64 {
        return Ok(Some(InstructionOutcome::Yield));
    }
    if raw == JIT_DEOPT_SENTINEL as u64 {
        return Ok(None);
    }
    process.set_x_reg(0, Term::from_raw(raw));
    return_(process).map(Some)
}

fn push_y_frame(
    process: &mut Process,
    module: &Module,
    stack_need: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    let slots = operand_u16(stack_need, "stack slots")?;
    let return_ip = process
        .code_position()
        .map_or(0, |position| position.instruction_pointer);
    let caller_module = current_module_pin(process, module);
    process
        .stack_mut()
        .push_frame(module.name, return_ip, caller_module, slots)
        .map_err(ExecError::from)?;
    Ok(InstructionOutcome::Continue)
}

pub(crate) fn deallocate_frame(process: &mut Process, words: &Operand) -> Result<(), ExecError> {
    let _words = operand_u16(words, "deallocate words")?;
    let _ = process.stack_mut().pop_frame().map_err(ExecError::from)?;
    Ok(())
}

pub(crate) fn current_module_pin(process: &Process, module: &Module) -> Arc<Module> {
    process
        .current_module()
        .filter(|current| current.name == module.name)
        .map_or_else(|| Arc::new(module.clone()), Arc::clone)
}

fn jump_with_reduction(
    process: &mut Process,
    module: crate::atom::Atom,
    instruction_pointer: usize,
) -> Result<InstructionOutcome, ExecError> {
    jump_position_with_reduction(
        process,
        CodePosition {
            module,
            instruction_pointer,
        },
    )
}

pub(crate) fn jump_position_with_reduction(
    process: &mut Process,
    target: CodePosition,
) -> Result<InstructionOutcome, ExecError> {
    charge_reduction(process)?;
    Ok(if process.reductions_exhausted() {
        process.set_code_position(Some(target));
        InstructionOutcome::Yield
    } else {
        InstructionOutcome::Jump(target)
    })
}

pub(crate) fn charge_reduction(process: &mut Process) -> Result<(), ExecError> {
    process.decrement_reductions(1);
    Ok(())
}

pub(crate) fn label_ip(module: &Module, label: u32) -> Result<usize, ExecError> {
    module.label_ip(label)
}

pub(crate) fn read_term(
    process: &Process,
    module: &Module,
    operand: &Operand,
) -> Result<Term, ExecError> {
    match operand {
        Operand::Integer(value) => Term::try_small_int(*value).ok_or(ExecError::Badarg),
        Operand::Unsigned(value) => {
            let value = i64::try_from(*value).map_err(|_| ExecError::Badarg)?;
            Term::try_small_int(value).ok_or(ExecError::Badarg)
        }
        Operand::Atom(Some(atom)) => Ok(Term::atom(*atom)),
        Operand::Atom(None) => Ok(Term::NIL),
        Operand::X(index) => Ok(process.x_reg(u16_from_u32(*index, "X register")?)),
        Operand::Y(index) => process
            .stack()
            .y_reg(u16_from_u32(*index, "Y register")?)
            .map_err(ExecError::from),
        Operand::Literal(index) => literal_term(module, *index),
        Operand::TypedRegister { register, .. } => read_term(process, module, register),
        _ => Err(ExecError::InvalidOperand("term source")),
    }
}

pub(crate) fn write_term(
    process: &mut Process,
    destination: &Operand,
    value: Term,
) -> Result<(), ExecError> {
    match destination {
        Operand::X(index) => {
            process.set_x_reg(u16_from_u32(*index, "X register")?, value);
            Ok(())
        }
        Operand::Y(index) => process
            .stack_mut()
            .set_y_reg(u16_from_u32(*index, "Y register")?, value)
            .map_err(ExecError::from),
        Operand::TypedRegister { register, .. } => write_term(process, register, value),
        _ => Err(ExecError::InvalidOperand("term destination")),
    }
}

pub(crate) fn literal_term(module: &Module, literal_index: usize) -> Result<Term, ExecError> {
    module
        .constant_pool
        .get(literal_index)
        .ok_or(ExecError::InvalidOperand("literal index"))
}

pub(crate) fn operand_atom(operand: &Operand) -> Result<crate::atom::Atom, ExecError> {
    match operand {
        Operand::Atom(Some(atom)) => Ok(*atom),
        _ => Err(ExecError::InvalidOperand("atom")),
    }
}

pub(crate) fn operand_label(operand: &Operand) -> Result<u32, ExecError> {
    match operand {
        Operand::Label(label) => Ok(*label),
        Operand::Unsigned(value) => {
            u32::try_from(*value).map_err(|_| ExecError::InvalidOperand("label"))
        }
        Operand::Integer(value) => {
            u32::try_from(*value).map_err(|_| ExecError::InvalidOperand("label"))
        }
        _ => Err(ExecError::InvalidOperand("label")),
    }
}

pub(crate) fn operand_usize(operand: &Operand, context: &'static str) -> Result<usize, ExecError> {
    match operand {
        Operand::Unsigned(value) => {
            usize::try_from(*value).map_err(|_| ExecError::InvalidOperand(context))
        }
        Operand::Integer(value) => {
            usize::try_from(*value).map_err(|_| ExecError::InvalidOperand(context))
        }
        Operand::Allocation(entries) => {
            use crate::loader::decode::compact::Allocation;
            let mut total: usize = 0;
            for entry in entries {
                let words = match entry {
                    Allocation::Words(n) => *n as usize,
                    Allocation::Floats(n) => (*n as usize) * 2,
                    Allocation::Funs(n) => (*n as usize) * 6,
                    Allocation::Unknown { .. } => 0,
                };
                total = total.saturating_add(words);
            }
            Ok(total)
        }
        _ => Err(ExecError::InvalidOperand(context)),
    }
}

fn operand_u8(operand: &Operand, context: &'static str) -> Result<u8, ExecError> {
    u8::try_from(operand_usize(operand, context)?).map_err(|_| ExecError::InvalidOperand(context))
}

fn operand_u16(operand: &Operand, context: &'static str) -> Result<u16, ExecError> {
    u16::try_from(operand_usize(operand, context)?).map_err(|_| ExecError::InvalidOperand(context))
}

fn u16_from_u32(value: u32, context: &'static str) -> Result<u16, ExecError> {
    u16::try_from(value).map_err(|_| ExecError::InvalidOperand(context))
}

pub(crate) fn heap_slice<'a>(ptr: *mut u64, words: usize) -> &'a mut [u64] {
    // SAFETY: `Heap::alloc(words)` returned a non-overlapping allocation with
    // exactly `words` contiguous machine words that remain owned by the process
    // heap. The slice is used immediately to initialise the new object.
    unsafe { std::slice::from_raw_parts_mut(ptr, words) }
}
