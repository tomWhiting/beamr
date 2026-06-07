//! Foundational BEAM opcode handlers.

use std::sync::{Arc, Mutex};

use crate::atom::AtomTable;
use crate::error::ExecError;
use crate::gc::{GcError, ensure_space};
use crate::interpreter::InstructionOutcome;
use crate::interpreter::NativeServices;
use crate::loader::Literal;
use crate::loader::decode::compact::Operand;
use crate::module::{Module, ModuleRegistry, ResolvedImportTarget};
use crate::native::ProcessContext;
use crate::process::{CodePosition, ExitReason, Process};
use crate::term::boxed::{Tuple, write_cons, write_tuple};
use crate::term::{Term, compare};
use crate::timer::TimerWheel;

use super::trampoline;

/// Combined context for external calls carrying timer, facility, and registry services.
pub struct ExtCallContext<'a> {
    pub timers: Option<&'a Arc<Mutex<TimerWheel>>>,
    pub services: Option<&'a NativeServices>,
    pub registry: Option<&'a ModuleRegistry>,
    pub atom_table: Option<&'a AtomTable>,
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
    source: &Operand,
    destination: &Operand,
    atom_table: Option<&AtomTable>,
) -> Result<InstructionOutcome, ExecError> {
    let value = read_term_with_atom_table(process, source, atom_table)?;
    write_term(process, destination, value)?;
    Ok(InstructionOutcome::Continue)
}

pub fn call(
    process: &mut Process,
    module: &Module,
    arity: &Operand,
    label: &Operand,
    return_ip: usize,
    save_return: bool,
) -> Result<InstructionOutcome, ExecError> {
    let _arity = operand_u8(arity, "call arity")?;
    if save_return {
        let caller_module = current_module_pin(process, module);
        process
            .stack_mut()
            .push_frame(module.name, return_ip, caller_module, 0)
            .map_err(ExecError::from)?;
    }
    let target = label_ip(module, operand_label(label)?)?;
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

fn gc_error_to_exec(error: GcError) -> ExecError {
    match error {
        GcError::HeapFull(error) => ExecError::from(error),
        GcError::InvalidObjectHeader(_) => ExecError::Badarg,
    }
}

pub fn put_list(
    process: &mut Process,
    head: &Operand,
    tail: &Operand,
    destination: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    let head = read_term(process, head)?;
    let tail = read_term(process, tail)?;
    let ptr = process.heap_mut().alloc(2).map_err(ExecError::from)?;
    let heap = heap_slice(ptr, 2);
    let term = write_cons(heap, head, tail).ok_or(ExecError::Badarg)?;
    write_term(process, destination, term)?;
    Ok(InstructionOutcome::Continue)
}

pub fn put_tuple2(
    process: &mut Process,
    destination: &Operand,
    elements: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    let Operand::List(element_operands) = elements else {
        return Err(ExecError::InvalidOperand("put_tuple2 elements"));
    };
    let mut values = Vec::with_capacity(element_operands.len());
    for operand in element_operands {
        values.push(read_term(process, operand)?);
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
    source: &Operand,
    index: &Operand,
    destination: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    let tuple_term = read_term(process, source)?;
    let tuple = Tuple::new(tuple_term).ok_or(ExecError::Badarg)?;
    let index = operand_usize(index, "tuple index")?;
    let value = tuple.get(index).ok_or(ExecError::Badarg)?;
    write_term(process, destination, value)?;
    Ok(InstructionOutcome::Continue)
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
        ResolvedImportTarget::Native(entry) => {
            if entry.function as usize == crate::native::denial_stub as usize {
                return Err(ExecError::Undef {
                    module: resolved.module,
                    function: resolved.function,
                    arity: resolved.arity,
                });
            }

            let mut args = Vec::with_capacity(usize::from(arity));
            for register in 0..arity {
                args.push(process.x_reg(register.into()));
            }
            let mut context = match ctx.timers {
                Some(timers) => {
                    ProcessContext::with_timer_services(process.pid(), Arc::clone(timers))
                }
                None => {
                    let mut pctx = ProcessContext::new();
                    pctx.set_pid(Some(process.pid()));
                    pctx
                }
            };
            if let Some(svc) = ctx.services {
                context.set_atom_table(svc.atom_table.clone());
                context.set_spawn_facility(svc.spawn_facility.clone());
                context.set_link_facility(svc.link_facility.clone());
                context.set_supervision_facility(svc.supervision_facility.clone());
                context.set_code_management_facility(svc.code_management_facility.clone());
                if let Some(sink) = &svc.io_sink {
                    context.set_io_sink(Arc::clone(sink));
                }
            }

            // Provide mailbox access for select BIFs before borrowing the process for heap allocation.
            let snapshot = trampoline::build_mailbox_snapshot(process);
            context.set_select_facility(
                snapshot
                    .clone()
                    .map(|s| s as Arc<dyn crate::native::SelectFacility>),
            );
            context.attach_process(process, usize::from(arity));

            let call_result = (entry.function)(&args, &mut context);
            let shutdown_requested = context.take_shutdown_request();
            let suspend = context.take_suspend();
            let trampoline_req = context.take_trampoline();
            context.detach_process();
            let result = match call_result {
                Ok(value) => value,
                Err(reason) => {
                    let exception = crate::process::Exception {
                        class: Term::atom(crate::atom::Atom::ERROR),
                        reason,
                        stacktrace: Term::NIL,
                    };
                    return super::messaging::raise_exception(process, exception);
                }
            };

            // Handle mailbox removal if the select facility recorded one.
            if let Some(snapshot) = snapshot {
                trampoline::apply_mailbox_removal(process, &snapshot);
            }

            // Check for suspend request before trampoline (suspend takes priority
            // when no message matched).
            if let Some(suspend) = suspend {
                return trampoline::handle_suspend(process, module, suspend);
            }

            // Check for trampoline request from the BIF.
            if let Some(trampoline_req) = trampoline_req {
                return trampoline::handle_trampoline(
                    process,
                    module,
                    ctx.registry,
                    trampoline_req,
                );
            }

            process.set_x_reg(0, result);
            if shutdown_requested {
                return Ok(InstructionOutcome::Exit(crate::process::ExitReason::Normal));
            }
            charge_reduction(process)?;
            Ok(InstructionOutcome::Continue)
        }
    }
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

pub(crate) fn read_term(process: &Process, operand: &Operand) -> Result<Term, ExecError> {
    read_term_with_atom_table(process, operand, None)
}

pub(crate) fn read_term_with_atom_table(
    process: &Process,
    operand: &Operand,
    atom_table: Option<&AtomTable>,
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
        Operand::Literal(literal) => literal_term(literal, atom_table),
        Operand::TypedRegister { register, .. } => {
            read_term_with_atom_table(process, register, atom_table)
        }
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

fn literal_term(literal: &Literal, atom_table: Option<&AtomTable>) -> Result<Term, ExecError> {
    match literal {
        Literal::Integer(value) => Term::try_small_int(*value).ok_or(ExecError::Badarg),
        Literal::Float(value) => {
            let heap = Box::leak(Box::new([0u64; 2]));
            crate::term::boxed::write_float(heap, *value).ok_or(ExecError::Badarg)
        }
        Literal::BigInteger(limbs) => {
            let limbs = limbs_to_u64(limbs)?;
            let heap = Box::leak(vec![0u64; 3 + limbs.len()].into_boxed_slice());
            crate::term::boxed::write_bigint(heap, false, &limbs).ok_or(ExecError::Badarg)
        }
        Literal::Atom(atom) => Ok(Term::atom(*atom)),
        Literal::Binary(bytes) | Literal::String(bytes) => {
            let data_words = crate::term::binary::packed_word_count(bytes.len());
            let heap = Box::leak(vec![0u64; 2 + data_words].into_boxed_slice());
            crate::term::binary::write_binary(heap, bytes).ok_or(ExecError::Badarg)
        }
        Literal::Nil => Ok(Term::NIL),
        Literal::Tuple(elements) => {
            let mut terms = Vec::with_capacity(elements.len());
            for element in elements {
                terms.push(literal_term(element, atom_table)?);
            }
            let heap = Box::leak(vec![0u64; 1 + terms.len()].into_boxed_slice());
            crate::term::boxed::write_tuple(heap, &terms).ok_or(ExecError::Badarg)
        }
        Literal::List(elements, tail) => {
            let mut result = literal_term(tail, atom_table)?;
            for element in elements.iter().rev() {
                let head = literal_term(element, atom_table)?;
                let heap = Box::leak(Box::new([0u64; 2]));
                result =
                    crate::term::boxed::write_cons(heap, head, result).ok_or(ExecError::Badarg)?;
            }
            Ok(result)
        }
        Literal::Map(entries) => {
            let mut pairs = Vec::with_capacity(entries.len());
            for (key, value) in entries {
                pairs.push((
                    literal_term(key, atom_table)?,
                    literal_term(value, atom_table)?,
                ));
            }
            pairs.sort_by(|(left, _), (right, _)| {
                atom_table.map_or_else(
                    || compare::raw_cmp(*left, *right),
                    |table| compare::cmp(*left, *right, table),
                )
            });
            let keys: Vec<_> = pairs.iter().map(|(key, _)| *key).collect();
            let values: Vec<_> = pairs.iter().map(|(_, value)| *value).collect();
            let heap = Box::leak(vec![0u64; 2 + keys.len() + values.len()].into_boxed_slice());
            crate::term::boxed::write_map(heap, &keys, &values).ok_or(ExecError::Badarg)
        }
    }
}

fn limbs_to_u64(bytes: &[u8]) -> Result<Vec<u64>, ExecError> {
    if !bytes.len().is_multiple_of(8) {
        return Err(ExecError::UnsupportedLiteral);
    }
    let mut limbs = Vec::with_capacity(bytes.len() / 8);
    for chunk in bytes.chunks_exact(8) {
        let mut limb = [0u8; 8];
        limb.copy_from_slice(chunk);
        limbs.push(u64::from_le_bytes(limb));
    }
    Ok(limbs)
}

fn operand_atom(operand: &Operand) -> Result<crate::atom::Atom, ExecError> {
    match operand {
        Operand::Atom(Some(atom)) => Ok(*atom),
        Operand::Literal(Literal::Atom(atom)) => Ok(*atom),
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
