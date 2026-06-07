//! Guard, test, and branching BEAM opcode handlers.

use std::cmp::Ordering;

use crate::atom::{Atom, AtomTable};
use crate::error::ExecError;
use crate::interpreter::InstructionOutcome;
use crate::loader::decode::compact::Operand;
use crate::loader::decode::{BifOp, ComparisonOp, TypeTestOp};
use crate::module::{Module, ResolvedImportTarget};
use crate::native::ProcessContext;
use crate::process::{CodePosition, Process};
use crate::term::boxed::{Closure, Cons, Float, Map, Reference, Tuple};
use crate::term::{Term, binary::Binary, compare};

use super::core;

pub fn get_hd(
    process: &mut Process,
    module: &Module,
    source: &Operand,
    destination: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    let cons = Cons::new(core::read_term(process, module, source)?).ok_or(ExecError::Badarg)?;
    core::write_term(process, destination, cons.head())?;
    Ok(InstructionOutcome::Continue)
}

pub fn get_tl(
    process: &mut Process,
    module: &Module,
    source: &Operand,
    destination: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    let cons = Cons::new(core::read_term(process, module, source)?).ok_or(ExecError::Badarg)?;
    core::write_term(process, destination, cons.tail())?;
    Ok(InstructionOutcome::Continue)
}

pub fn type_test(
    process: &Process,
    module: &Module,
    op: TypeTestOp,
    fail: &Operand,
    value: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    let (value, arity) = function_test_value_and_arity(process, module, op, value)?;
    branch_if_false(module, fail, matches_type(op, value, arity)?)
}

pub fn comparison(
    process: &Process,
    module: &Module,
    op: ComparisonOp,
    fail: &Operand,
    left: &Operand,
    right: &Operand,
    atom_table: Option<&AtomTable>,
) -> Result<InstructionOutcome, ExecError> {
    let left = core::read_term(process, module, left)?;
    let right = core::read_term(process, module, right)?;
    let order = || {
        atom_table.map_or_else(
            || compare::raw_cmp(left, right),
            |table| compare::cmp(left, right, table),
        )
    };
    let passed = match op {
        ComparisonOp::Lt => order() == Ordering::Less,
        ComparisonOp::Ge => order() != Ordering::Less,
        ComparisonOp::Eq => compare::numeric_eq(left, right),
        ComparisonOp::Ne => !compare::numeric_eq(left, right),
        ComparisonOp::EqExact => compare::exact_eq(left, right),
        ComparisonOp::NeExact => !compare::exact_eq(left, right),
    };
    branch_if_false(module, fail, passed)
}

pub fn test_arity(
    process: &Process,
    module: &Module,
    fail: &Operand,
    tuple: &Operand,
    arity: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    let tuple = core::read_term(process, module, tuple)?;
    let expected = core::operand_usize(arity, "tuple arity")?;
    let passed = Tuple::new(tuple).is_some_and(|tuple| tuple.arity() == expected);
    branch_if_false(module, fail, passed)
}

pub fn select_val(
    process: &Process,
    module: &Module,
    value: &Operand,
    fail: &Operand,
    list: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    let value = core::read_term(process, module, value)?;
    for pair in select_pairs(list, "select_val list")? {
        let (candidate, label) = pair?;
        if compare::exact_eq(value, core::read_term(process, module, candidate)?) {
            return jump_label(module, label);
        }
    }
    jump_label(module, fail)
}

pub fn select_tuple_arity(
    process: &Process,
    module: &Module,
    value: &Operand,
    fail: &Operand,
    list: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    let arity = Tuple::new(core::read_term(process, module, value)?).map(Tuple::arity);
    if let Some(arity) = arity {
        for pair in select_pairs(list, "select_tuple_arity list")? {
            let (candidate, label) = pair?;
            if core::operand_usize(candidate, "tuple arity")? == arity {
                return jump_label(module, label);
            }
        }
    }
    jump_label(module, fail)
}

pub fn jump(module: &Module, target: &Operand) -> Result<InstructionOutcome, ExecError> {
    jump_label(module, target)
}

pub fn bif(
    process: &mut Process,
    module: &Module,
    op: BifOp,
    operands: &[Operand],
) -> Result<InstructionOutcome, ExecError> {
    let parsed = parse_bif_operands(op, operands)?;
    if let Some(heap_need) = parsed.heap_need {
        let live = Operand::Unsigned(parsed.expected_arity.into());
        core::test_heap(process, heap_need, &live)?;
    }

    let import_index = core::operand_usize(parsed.import, "bif import index")?;
    let resolved = module
        .resolved_imports
        .get(import_index)
        .ok_or(ExecError::InvalidImport {
            index: import_index,
        })?;
    if resolved.arity != parsed.expected_arity {
        return Err(ExecError::InvalidOperand("bif arity mismatch"));
    }

    let ResolvedImportTarget::Native(entry) = resolved.target else {
        return Err(ExecError::InvalidOperand("guard bif native import"));
    };

    let mut args = Vec::with_capacity(parsed.args.len());
    for arg in parsed.args {
        args.push(core::read_term(process, module, arg)?);
    }

    let mut context = ProcessContext::new();
    context.set_pid(Some(process.pid()));
    context.attach_process(process, usize::from(parsed.expected_arity));
    match (entry.function)(&args, &mut context) {
        Ok(result) => {
            context.detach_process();
            core::write_term(process, parsed.destination, result)?;
            Ok(InstructionOutcome::Continue)
        }
        Err(_) => {
            context.detach_process();
            let label = core::operand_label(parsed.fail)?;
            if label == 0 {
                return Err(ExecError::Badarg);
            }
            jump_label(module, parsed.fail)
        }
    }
}

fn function_test_value_and_arity(
    process: &Process,
    module: &Module,
    op: TypeTestOp,
    value: &Operand,
) -> Result<(Term, Option<usize>), ExecError> {
    if op == TypeTestOp::IsFunction2 {
        let Operand::List(operands) = value else {
            return Err(ExecError::InvalidOperand("is_function2 operands"));
        };
        let [function, arity] = operands.as_slice() else {
            return Err(ExecError::InvalidOperand("is_function2 operands"));
        };
        Ok((
            core::read_term(process, module, function)?,
            Some(core::operand_usize(arity, "is_function2 arity")?),
        ))
    } else {
        Ok((core::read_term(process, module, value)?, None))
    }
}

fn branch_if_false(
    module: &Module,
    fail: &Operand,
    passed: bool,
) -> Result<InstructionOutcome, ExecError> {
    if passed {
        Ok(InstructionOutcome::Continue)
    } else {
        jump_label(module, fail)
    }
}

fn jump_label(module: &Module, label: &Operand) -> Result<InstructionOutcome, ExecError> {
    let label = core::operand_label(label)?;
    Ok(InstructionOutcome::Jump(CodePosition {
        module: module.name,
        instruction_pointer: core::label_ip(module, label)?,
    }))
}

fn matches_type(op: TypeTestOp, value: Term, arity: Option<usize>) -> Result<bool, ExecError> {
    let matched = match op {
        TypeTestOp::IsInteger => value.is_small_int(),
        TypeTestOp::IsFloat => Float::new(value).is_some(),
        TypeTestOp::IsNumber => value.is_small_int() || Float::new(value).is_some(),
        TypeTestOp::IsAtom => value.is_atom(),
        TypeTestOp::IsPid => value.is_pid(),
        TypeTestOp::IsReference => Reference::new(value).is_some(),
        TypeTestOp::IsPort => false,
        TypeTestOp::IsNil => value.is_nil(),
        TypeTestOp::IsBinary | TypeTestOp::IsBitstr => Binary::new(value).is_some(),
        TypeTestOp::IsList => value.is_list() || value.is_nil(),
        TypeTestOp::IsNonemptyList => value.is_list(),
        TypeTestOp::IsTuple => Tuple::new(value).is_some(),
        TypeTestOp::IsFunction => Closure::new(value).is_some(),
        TypeTestOp::IsBoolean => matches!(value.as_atom(), Some(Atom::TRUE | Atom::FALSE)),
        TypeTestOp::IsFunction2 => {
            let Some(expected_arity) = arity else {
                return Err(ExecError::InvalidOperand("is_function2 arity"));
            };
            Closure::new(value)
                .is_some_and(|closure| usize::from(closure.arity()) == expected_arity)
        }
        TypeTestOp::IsMap => Map::new(value).is_some(),
        TypeTestOp::IsTaggedTuple => false,
    };
    Ok(matched)
}

fn select_pairs<'a>(
    list: &'a Operand,
    context: &'static str,
) -> Result<impl Iterator<Item = Result<(&'a Operand, &'a Operand), ExecError>>, ExecError> {
    let Operand::List(items) = list else {
        return Err(ExecError::InvalidOperand(context));
    };
    if items.len() % 2 != 0 {
        return Err(ExecError::InvalidOperand(context));
    }
    Ok(items
        .chunks_exact(2)
        .map(|chunk| Ok((&chunk[0], &chunk[1]))))
}

static BIF0_NO_FAIL: Operand = Operand::Label(0);

struct ParsedBif<'a> {
    fail: &'a Operand,
    import: &'a Operand,
    args: &'a [Operand],
    destination: &'a Operand,
    heap_need: Option<&'a Operand>,
    expected_arity: u8,
}

fn parse_bif_operands(op: BifOp, operands: &[Operand]) -> Result<ParsedBif<'_>, ExecError> {
    let arity = match op {
        BifOp::Bif0 => 0,
        BifOp::Bif1 | BifOp::GcBif1 => 1,
        BifOp::Bif2 | BifOp::GcBif2 => 2,
        BifOp::GcBif3 => 3,
    };
    let non_gc_len = 3 + arity;
    let gc_len = 4 + arity;

    match op {
        BifOp::Bif0 => {
            // bif0 has no fail label: [import, destination]
            if operands.len() != 2 {
                return Err(ExecError::InvalidOperand("bif0 operands"));
            }
            Ok(ParsedBif {
                fail: &BIF0_NO_FAIL,
                import: &operands[0],
                args: &[],
                destination: &operands[1],
                heap_need: None,
                expected_arity: 0,
            })
        }
        BifOp::Bif1 | BifOp::Bif2 => {
            if operands.len() != non_gc_len {
                return Err(ExecError::InvalidOperand("bif operands"));
            }
            Ok(ParsedBif {
                fail: &operands[0],
                import: &operands[1],
                args: &operands[2..2 + arity],
                destination: &operands[2 + arity],
                heap_need: None,
                expected_arity: arity as u8,
            })
        }
        BifOp::GcBif1 | BifOp::GcBif2 | BifOp::GcBif3 => {
            if operands.len() != gc_len && operands.len() != non_gc_len {
                return Err(ExecError::InvalidOperand("gc_bif operands"));
            }
            let has_heap_need = operands.len() == gc_len;
            let offset = usize::from(has_heap_need);
            Ok(ParsedBif {
                fail: &operands[0],
                import: &operands[1 + offset],
                args: &operands[2 + offset..2 + offset + arity],
                destination: &operands[2 + offset + arity],
                heap_need: has_heap_need.then_some(&operands[1]),
                expected_arity: arity as u8,
            })
        }
    }
}

#[cfg(test)]
mod tests;
