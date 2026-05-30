//! Foundational BEAM opcode handlers.

use crate::error::ExecError;
use crate::interpreter::InstructionOutcome;
use crate::loader::Literal;
use crate::loader::decode::compact::Operand;
use crate::module::{Module, ResolvedImportTarget};
use crate::native::ProcessContext;
use crate::process::{CodePosition, ExitReason, Process};
use crate::term::Term;
use crate::term::boxed::{Tuple, write_cons, write_tuple};

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
) -> Result<InstructionOutcome, ExecError> {
    let value = read_term(process, source)?;
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
        process
            .stack_mut()
            .push_frame(module.name, return_ip, 0)
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
) -> Result<InstructionOutcome, ExecError> {
    if save_return {
        process
            .stack_mut()
            .push_frame(module.name, return_ip, 0)
            .map_err(ExecError::from)?;
    }
    call_external_target(process, module, arity, import)
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
) -> Result<InstructionOutcome, ExecError> {
    deallocate_frame(process, deallocate)?;
    call_external_target(process, module, arity, import)
}

pub fn return_(process: &mut Process) -> Result<InstructionOutcome, ExecError> {
    if process.stack().is_empty() {
        return Ok(InstructionOutcome::Exit(ExitReason::Normal));
    }
    let return_point = process.stack_mut().pop_frame().map_err(ExecError::from)?;
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
) -> Result<InstructionOutcome, ExecError> {
    test_heap(process, heap_need)?;
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
) -> Result<InstructionOutcome, ExecError> {
    let needed = operand_usize(heap_need, "heap words")?;
    let available = process.heap().available();
    if available < needed {
        return Err(ExecError::GcNeeded {
            requested: needed,
            available,
        });
    }
    Ok(InstructionOutcome::Continue)
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
        ResolvedImportTarget::Code { module, label } => {
            let target = CodePosition {
                module,
                instruction_pointer: usize::try_from(label)
                    .map_err(|_| ExecError::InvalidLabel { label })?,
            };
            jump_position_with_reduction(process, target)
        }
        ResolvedImportTarget::Native(entry) => {
            let mut args = Vec::with_capacity(usize::from(arity));
            for register in 0..arity {
                args.push(process.x_reg(register));
            }
            let mut context = ProcessContext::new();
            let result = (entry.function)(&args, &mut context).map_err(|_| ExecError::Badarg)?;
            process.set_x_reg(0, result);
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
    process
        .stack_mut()
        .push_frame(module.name, return_ip, slots)
        .map_err(ExecError::from)?;
    Ok(InstructionOutcome::Continue)
}

fn deallocate_frame(process: &mut Process, words: &Operand) -> Result<(), ExecError> {
    let _words = operand_u16(words, "deallocate words")?;
    let _ = process.stack_mut().pop_frame().map_err(ExecError::from)?;
    Ok(())
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

fn jump_position_with_reduction(
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

fn charge_reduction(process: &mut Process) -> Result<(), ExecError> {
    process.decrement_reductions(1);
    Ok(())
}

fn label_ip(module: &Module, label: u32) -> Result<usize, ExecError> {
    module
        .code
        .iter()
        .position(|instruction| matches!(instruction, crate::loader::Instruction::Label { label: seen } if *seen == label))
        .ok_or(ExecError::InvalidLabel { label })
}

fn read_term(process: &Process, operand: &Operand) -> Result<Term, ExecError> {
    match operand {
        Operand::Integer(value) => Term::try_small_int(*value).ok_or(ExecError::Badarg),
        Operand::Unsigned(value) => {
            let value = i64::try_from(*value).map_err(|_| ExecError::Badarg)?;
            Term::try_small_int(value).ok_or(ExecError::Badarg)
        }
        Operand::Atom(Some(atom)) => Ok(Term::atom(*atom)),
        Operand::Atom(None) => Ok(Term::NIL),
        Operand::X(index) => Ok(process.x_reg(u8_from_u32(*index, "X register")?)),
        Operand::Y(index) => process
            .stack()
            .y_reg(u16_from_u32(*index, "Y register")?)
            .map_err(ExecError::from),
        Operand::Literal(literal) => literal_term(literal),
        Operand::TypedRegister { register, .. } => read_term(process, register),
        _ => Err(ExecError::InvalidOperand("term source")),
    }
}

fn write_term(process: &mut Process, destination: &Operand, value: Term) -> Result<(), ExecError> {
    match destination {
        Operand::X(index) => {
            process.set_x_reg(u8_from_u32(*index, "X register")?, value);
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

fn literal_term(literal: &Literal) -> Result<Term, ExecError> {
    match literal {
        Literal::Integer(value) => Term::try_small_int(*value).ok_or(ExecError::Badarg),
        Literal::Atom(atom) => Ok(Term::atom(*atom)),
        Literal::Nil => Ok(Term::NIL),
        _ => Err(ExecError::UnsupportedLiteral),
    }
}

fn operand_atom(operand: &Operand) -> Result<crate::atom::Atom, ExecError> {
    match operand {
        Operand::Atom(Some(atom)) => Ok(*atom),
        Operand::Literal(Literal::Atom(atom)) => Ok(*atom),
        _ => Err(ExecError::InvalidOperand("atom")),
    }
}

fn operand_label(operand: &Operand) -> Result<u32, ExecError> {
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

fn operand_usize(operand: &Operand, context: &'static str) -> Result<usize, ExecError> {
    match operand {
        Operand::Unsigned(value) => {
            usize::try_from(*value).map_err(|_| ExecError::InvalidOperand(context))
        }
        Operand::Integer(value) => {
            usize::try_from(*value).map_err(|_| ExecError::InvalidOperand(context))
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

fn u8_from_u32(value: u32, context: &'static str) -> Result<u8, ExecError> {
    u8::try_from(value).map_err(|_| ExecError::InvalidOperand(context))
}

fn u16_from_u32(value: u32, context: &'static str) -> Result<u16, ExecError> {
    u16::try_from(value).map_err(|_| ExecError::InvalidOperand(context))
}

fn heap_slice<'a>(ptr: *mut u64, words: usize) -> &'a mut [u64] {
    // SAFETY: `Heap::alloc(words)` returned a non-overlapping allocation with
    // exactly `words` contiguous machine words that remain owned by the process
    // heap. The slice is used immediately to initialise the new object.
    unsafe { std::slice::from_raw_parts_mut(ptr, words) }
}
