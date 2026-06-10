//! Closure, dynamic dispatch, and flatmap opcode handlers.

use std::sync::Arc;

use crate::atom::{Atom, AtomTable};
use crate::error::ExecError;
use crate::interpreter::InstructionOutcome;
use crate::loader::decode::MapOp;
use crate::loader::decode::compact::Operand;
use crate::module::{Module, ModuleRegistry};
use crate::process::{CodePosition, Process};
use crate::term::Term;
use crate::term::boxed::{Closure, Map, write_closure, write_map};
use crate::term::compare;

use super::core;

pub fn make_fun(
    process: &mut Process,
    module: &Module,
    operands: &[Operand],
) -> Result<InstructionOutcome, ExecError> {
    let lambda_index = make_fun_lambda_index(operands)?;
    let lambda = module
        .lambdas
        .get(lambda_index)
        .ok_or(ExecError::InvalidOperand("make_fun lambda index"))?;
    let num_free = usize::try_from(lambda.num_free)
        .map_err(|_| ExecError::InvalidOperand("make_fun num_free"))?;
    if num_free > 256 {
        return Err(ExecError::InvalidOperand("make_fun num_free"));
    }

    let free_vars = make_fun_free_vars(process, module, operands, num_free)?;

    let closure_arity = if has_explicit_free_vars(operands) {
        lambda
            .arity
            .checked_sub(
                u8::try_from(num_free)
                    .map_err(|_| ExecError::InvalidOperand("make_fun num_free"))?,
            )
            .ok_or(ExecError::InvalidOperand("make_fun arity"))?
    } else {
        lambda.arity
    };
    let words = 7usize
        .checked_add(free_vars.len())
        .ok_or(ExecError::InvalidOperand("closure size"))?;
    let ptr = process.heap_mut().alloc(words).map_err(ExecError::from)?;
    let heap = core::heap_slice(ptr, words);
    let function_index = u64::try_from(lambda_index)
        .map_err(|_| ExecError::InvalidOperand("make_fun lambda index"))?;
    let term = write_closure(
        heap,
        module.name,
        function_index,
        closure_arity,
        module.generation(),
        lambda.unique_id,
        &free_vars,
    )
    .ok_or(ExecError::Badarg)?;
    write_make_fun_result(process, operands, term)?;
    Ok(InstructionOutcome::Continue)
}

pub fn call_fun(
    process: &mut Process,
    module: &Module,
    arity: &Operand,
    return_ip: usize,
    registry: Option<&ModuleRegistry>,
) -> Result<InstructionOutcome, ExecError> {
    let arity = operand_u8(arity, "call_fun arity")?;
    let fun_term = process.x_reg(arity.into());
    let closure = Closure::new(fun_term).ok_or(ExecError::Badfun { term: fun_term })?;
    if closure.arity() != arity {
        let args = collect_args(process, arity);
        return Err(ExecError::Badarity {
            fun: fun_term,
            args,
        });
    }

    let free_count = closure.num_free();
    if usize::from(arity)
        .checked_add(free_count)
        .filter(|count| *count <= 256)
        .is_none()
    {
        return Err(ExecError::InvalidOperand("closure free variables"));
    }
    for index in 0..free_count {
        let value = closure
            .free_var(index)
            .ok_or(ExecError::InvalidOperand("closure free variable"))?;
        let register = u16::try_from(usize::from(arity) + index)
            .map_err(|_| ExecError::InvalidOperand("X register"))?;
        process.set_x_reg(register, value);
    }

    let resolved = resolve_closure_target(closure, module, registry, fun_term)?;
    let caller_module = core::current_module_pin(process, module);
    process
        .stack_mut()
        .push_frame(module.name, return_ip, caller_module, 0)
        .map_err(ExecError::from)?;
    let target = CodePosition {
        module: resolved.module_name,
        instruction_pointer: core::label_ip(resolved.module.as_ref(), resolved.label)?,
    };
    process.set_current_module(resolved.module);
    core::jump_position_with_reduction(process, target)
}

pub(crate) struct ResolvedClosureTarget {
    pub module: Arc<Module>,
    pub module_name: Atom,
    pub label: u32,
}

pub(crate) fn resolve_closure_target(
    closure: Closure,
    fallback_module: &Module,
    registry: Option<&ModuleRegistry>,
    fun_term: Term,
) -> Result<ResolvedClosureTarget, ExecError> {
    let target_module_atom = closure.module().unwrap_or(fallback_module.name);
    let current = registry
        .and_then(|registry| registry.lookup(target_module_atom))
        .unwrap_or_else(|| Arc::new(fallback_module.clone()));

    let label = if closure.generation() == current.generation() {
        let function_index = usize::try_from(closure.function_index())
            .map_err(|_| ExecError::InvalidOperand("closure function index"))?;
        let lambda = current
            .lambdas
            .get(function_index)
            .ok_or(ExecError::Badfun { term: fun_term })?;
        if lambda.unique_id != closure.unique_id() {
            return Err(ExecError::Badfun { term: fun_term });
        }
        lambda.label
    } else if let Some(lambda) = current.find_lambda_by_id(closure.unique_id()) {
        lambda.label
    } else {
        let old = registry.and_then(|registry| registry.lookup_old(target_module_atom));
        let old = old
            .filter(|module| module.find_lambda_by_id(closure.unique_id()).is_some())
            .ok_or(ExecError::Badfun { term: fun_term })?;
        let label = old
            .find_lambda_by_id(closure.unique_id())
            .ok_or(ExecError::Badfun { term: fun_term })?
            .label;
        return Ok(ResolvedClosureTarget {
            module_name: old.name,
            module: old,
            label,
        });
    };

    Ok(ResolvedClosureTarget {
        module_name: current.name,
        module: current,
        label,
    })
}

pub fn call_fun2(
    process: &mut Process,
    module: &Module,
    func: &Operand,
    arity: &Operand,
    return_ip: usize,
    registry: Option<&ModuleRegistry>,
) -> Result<InstructionOutcome, ExecError> {
    let arity = operand_u8(arity, "call_fun2 arity")?;
    let fun_term = core::read_term(process, module, func)?;
    let closure = Closure::new(fun_term).ok_or(ExecError::Badfun { term: fun_term })?;
    if closure.arity() != arity {
        let args = collect_args(process, arity);
        return Err(ExecError::Badarity {
            fun: fun_term,
            args,
        });
    }

    let free_count = closure.num_free();
    for index in 0..free_count {
        let value = closure
            .free_var(index)
            .ok_or(ExecError::InvalidOperand("closure free variable"))?;
        let register = u16::try_from(usize::from(arity) + index)
            .map_err(|_| ExecError::InvalidOperand("X register"))?;
        process.set_x_reg(register, value);
    }

    let resolved = resolve_closure_target(closure, module, registry, fun_term)?;
    let caller_module = core::current_module_pin(process, module);
    process
        .stack_mut()
        .push_frame(module.name, return_ip, caller_module, 0)
        .map_err(ExecError::from)?;
    let target = CodePosition {
        module: resolved.module_name,
        instruction_pointer: core::label_ip(resolved.module.as_ref(), resolved.label)?,
    };
    process.set_current_module(resolved.module);
    core::jump_position_with_reduction(process, target)
}

pub fn apply(
    process: &mut Process,
    registry: &ModuleRegistry,
    arity: &Operand,
    return_ip: usize,
    save_return_module: Atom,
) -> Result<InstructionOutcome, ExecError> {
    apply_common(
        process,
        registry,
        arity,
        None,
        return_ip,
        Some(save_return_module),
    )
}

pub fn apply_last(
    process: &mut Process,
    registry: &ModuleRegistry,
    arity: &Operand,
    deallocate: &Operand,
    return_ip: usize,
) -> Result<InstructionOutcome, ExecError> {
    apply_common(process, registry, arity, Some(deallocate), return_ip, None)
}

pub fn map_op(
    process: &mut Process,
    module: &Module,
    op: MapOp,
    operands: &[Operand],
    atom_table: Option<&AtomTable>,
) -> Result<InstructionOutcome, ExecError> {
    match op {
        MapOp::HasMapFields => has_map_fields(process, module, operands),
        MapOp::GetMapElements => get_map_elements(process, module, operands),
        MapOp::PutMapAssoc => put_map(process, module, operands, PutMapMode::Assoc, atom_table),
        MapOp::PutMapExact => put_map(process, module, operands, PutMapMode::Exact, atom_table),
    }
}

fn apply_common(
    process: &mut Process,
    registry: &ModuleRegistry,
    arity: &Operand,
    deallocate: Option<&Operand>,
    return_ip: usize,
    save_return_module: Option<Atom>,
) -> Result<InstructionOutcome, ExecError> {
    let arity = operand_u8(arity, "apply arity")?;
    let module_term = process.x_reg(arity.into());
    let function_register = arity
        .checked_add(1)
        .ok_or(ExecError::InvalidOperand("apply function register"))?;
    let function_term = process.x_reg(function_register.into());
    let module_atom = module_term.as_atom().ok_or(ExecError::Badarg)?;
    let function_atom = function_term.as_atom().ok_or(ExecError::Badarg)?;

    let pointer = registry.lookup_mfa(module_atom, function_atom, arity)?;
    let target_ip = pointer.module.export_ip(function_atom, arity)?;
    if let Some(words) = deallocate {
        core::deallocate_frame(process, words)?;
    }
    if let Some(return_module) = save_return_module {
        let return_module_version = process
            .current_module()
            .filter(|current| current.name == return_module)
            .cloned()
            .or_else(|| registry.lookup(return_module))
            .ok_or(ExecError::Undef {
                module: return_module,
                function: Atom::UNDEFINED,
                arity,
            })?;
        process
            .stack_mut()
            .push_frame(return_module, return_ip, return_module_version, 0)
            .map_err(ExecError::from)?;
    }
    process.set_current_module(Arc::clone(&pointer.module));
    core::jump_position_with_reduction(
        process,
        CodePosition {
            module: pointer.module.name,
            instruction_pointer: target_ip,
        },
    )
}

fn has_map_fields(
    process: &mut Process,
    module: &Module,
    operands: &[Operand],
) -> Result<InstructionOutcome, ExecError> {
    let [fail, source, Operand::List(keys)] = operands else {
        return Err(ExecError::InvalidOperand("has_map_fields operands"));
    };
    let map_term = core::read_term(process, module, source)?;
    let Some(map) = Map::new(map_term) else {
        return jump_label(module, fail);
    };
    for key in keys {
        let key = core::read_term(process, module, key)?;
        if map.get(key).is_none() {
            return jump_label(module, fail);
        }
    }
    Ok(InstructionOutcome::Continue)
}

fn get_map_elements(
    process: &mut Process,
    module: &Module,
    operands: &[Operand],
) -> Result<InstructionOutcome, ExecError> {
    let [fail, source, Operand::List(items)] = operands else {
        return Err(ExecError::InvalidOperand("get_map_elements operands"));
    };
    if items.len() % 2 != 0 {
        return Err(ExecError::InvalidOperand("get_map_elements pairs"));
    }
    let map_term = core::read_term(process, module, source)?;
    let Some(map) = Map::new(map_term) else {
        return jump_label(module, fail);
    };

    let mut extracted = Vec::with_capacity(items.len() / 2);
    for pair in items.chunks_exact(2) {
        let key = core::read_term(process, module, &pair[0])?;
        let Some(value) = map.get(key) else {
            return jump_label(module, fail);
        };
        extracted.push((pair[1].clone(), value));
    }
    for (destination, value) in extracted {
        core::write_term(process, &destination, value)?;
    }
    Ok(InstructionOutcome::Continue)
}

#[derive(Copy, Clone)]
enum PutMapMode {
    Assoc,
    Exact,
}

fn put_map(
    process: &mut Process,
    module: &Module,
    operands: &[Operand],
    mode: PutMapMode,
    atom_table: Option<&AtomTable>,
) -> Result<InstructionOutcome, ExecError> {
    let [fail, source, destination, _live, Operand::List(items)] = operands else {
        return Err(ExecError::InvalidOperand("put_map operands"));
    };
    if items.len() % 2 != 0 {
        return Err(ExecError::InvalidOperand("put_map pairs"));
    }

    let source_term = core::read_term(process, module, source)?;
    let Some(source_map) = Map::new(source_term) else {
        return jump_label(module, fail);
    };

    let mut updates = Vec::with_capacity(items.len() / 2);
    for pair in items.chunks_exact(2) {
        updates.push((
            core::read_term(process, module, &pair[0])?,
            core::read_term(process, module, &pair[1])?,
        ));
    }

    if matches!(mode, PutMapMode::Exact)
        && updates
            .iter()
            .any(|(key, _value)| source_map.get(*key).is_none())
    {
        return jump_label(module, fail);
    }

    let mut entries = map_entries(source_map)?;
    for (key, value) in updates {
        if let Some((_existing_key, existing_value)) = entries
            .iter_mut()
            .find(|(existing_key, _)| *existing_key == key)
        {
            *existing_value = value;
        } else {
            entries.push((key, value));
        }
    }
    entries.sort_by(|(left, _), (right, _)| {
        atom_table.map_or_else(
            || compare::raw_cmp(*left, *right),
            |table| compare::cmp(*left, *right, table),
        )
    });

    let keys: Vec<Term> = entries.iter().map(|(key, _)| *key).collect();
    let values: Vec<Term> = entries.iter().map(|(_, value)| *value).collect();
    let words = 2usize
        .checked_add(
            keys.len()
                .checked_mul(2)
                .ok_or(ExecError::InvalidOperand("map size"))?,
        )
        .ok_or(ExecError::InvalidOperand("map size"))?;
    let ptr = process.heap_mut().alloc(words).map_err(ExecError::from)?;
    let heap = core::heap_slice(ptr, words);
    let result = write_map(heap, &keys, &values).ok_or(ExecError::Badarg)?;
    core::write_term(process, destination, result)?;
    Ok(InstructionOutcome::Continue)
}

fn map_entries(map: Map) -> Result<Vec<(Term, Term)>, ExecError> {
    let mut entries = Vec::with_capacity(map.len());
    for index in 0..map.len() {
        let key = map.key(index).ok_or(ExecError::InvalidOperand("map key"))?;
        let value = map
            .value(index)
            .ok_or(ExecError::InvalidOperand("map value"))?;
        entries.push((key, value));
    }
    Ok(entries)
}

fn jump_label(module: &Module, label: &Operand) -> Result<InstructionOutcome, ExecError> {
    let label = core::operand_label(label)?;
    Ok(InstructionOutcome::Jump(CodePosition {
        module: module.name,
        instruction_pointer: core::label_ip(module, label)?,
    }))
}

fn make_fun_lambda_index(operands: &[Operand]) -> Result<usize, ExecError> {
    match operands {
        [index] => core::operand_usize(index, "make_fun lambda index"),
        [index, _destination, Operand::List(_)] => {
            core::operand_usize(index, "make_fun lambda index")
        }
        [index, _uniq, _old_index] => core::operand_usize(index, "make_fun lambda index"),
        [index, _uniq, _old_index, _destination, _free_vars] => {
            core::operand_usize(index, "make_fun lambda index")
        }
        _ => Err(ExecError::InvalidOperand("make_fun operands")),
    }
}

fn has_explicit_free_vars(operands: &[Operand]) -> bool {
    matches!(
        operands,
        [_, _, Operand::List(_)] | [_, _, _, _, Operand::List(_)]
    )
}

fn write_make_fun_result(
    process: &mut Process,
    operands: &[Operand],
    term: Term,
) -> Result<(), ExecError> {
    match operands {
        [_, destination, Operand::List(_)] | [_, _, _, destination, _] => {
            core::write_term(process, destination, term)
        }
        _ => {
            process.set_x_reg(0, term);
            Ok(())
        }
    }
}

fn make_fun_free_vars(
    process: &Process,
    module: &Module,
    operands: &[Operand],
    num_free: usize,
) -> Result<Vec<Term>, ExecError> {
    let explicit_free_vars = match operands {
        [_, _, Operand::List(values)] | [_, _, _, _, Operand::List(values)] => {
            Some(values.as_slice())
        }
        _ => None,
    };

    if let Some(values) = explicit_free_vars {
        if values.len() != num_free {
            return Err(ExecError::InvalidOperand("make_fun free variable count"));
        }
        values
            .iter()
            .map(|operand| core::read_term(process, module, operand))
            .collect()
    } else {
        (0..num_free)
            .map(|register| {
                let register =
                    u16::try_from(register).map_err(|_| ExecError::InvalidOperand("X register"))?;
                Ok(process.x_reg(register))
            })
            .collect()
    }
}

fn operand_u8(operand: &Operand, context: &'static str) -> Result<u8, ExecError> {
    u8::try_from(core::operand_usize(operand, context)?)
        .map_err(|_| ExecError::InvalidOperand(context))
}

fn collect_args(process: &Process, arity: u8) -> Vec<Term> {
    (0..arity)
        .map(|register| process.x_reg(register.into()))
        .collect()
}

#[cfg(test)]
mod tests;
