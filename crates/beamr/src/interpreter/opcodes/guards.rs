//! Guard, test, and branch opcode handlers.

use crate::atom::Atom;
use crate::error::ExecError;
use crate::interpreter::InstructionOutcome;
use crate::loader::decode::compact::Operand;
use crate::loader::decode::{BifOp, ComparisonOp, TypeTestOp};
use crate::module::{Module, ResolvedImportTarget};
use crate::native::ProcessContext;
use crate::process::{CodePosition, Process};
use crate::term::Term;
use crate::term::binary::Binary;
use crate::term::boxed::{Closure, Cons, Float, Map, Reference, Tuple};
use crate::term::compare;

use super::core;

pub fn get_hd(
    process: &mut Process,
    source: &Operand,
    destination: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    let source = core::read_term(process, source)?;
    let cons = Cons::new(source).ok_or(ExecError::Badarg)?;
    core::write_term(process, destination, cons.head())?;
    Ok(InstructionOutcome::Continue)
}

pub fn get_tl(
    process: &mut Process,
    source: &Operand,
    destination: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    let source = core::read_term(process, source)?;
    let cons = Cons::new(source).ok_or(ExecError::Badarg)?;
    core::write_term(process, destination, cons.tail())?;
    Ok(InstructionOutcome::Continue)
}

pub fn type_test(
    process: &mut Process,
    module: &Module,
    op: TypeTestOp,
    fail: &Operand,
    value: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    let value = core::read_term(process, value)?;
    branch_if_false(module, fail, type_test_passes(op, value))
}

pub fn comparison(
    process: &Process,
    module: &Module,
    op: ComparisonOp,
    fail: &Operand,
    left: &Operand,
    right: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    let left = core::read_term(process, left)?;
    let right = core::read_term(process, right)?;
    let passed = match op {
        ComparisonOp::Lt => compare::cmp(left, right).is_lt(),
        ComparisonOp::Ge => compare::cmp(left, right).is_ge(),
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
    let tuple = core::read_term(process, tuple)?;
    let arity = core::operand_usize(arity, "tuple arity")?;
    let passed = Tuple::new(tuple).is_some_and(|tuple| tuple.arity() == arity);
    branch_if_false(module, fail, passed)
}

pub fn jump(module: &Module, target: &Operand) -> Result<InstructionOutcome, ExecError> {
    let label = core::operand_label(target)?;
    let instruction_pointer = core::label_ip(module, label)?;
    Ok(InstructionOutcome::Jump(CodePosition {
        module: module.name,
        instruction_pointer,
    }))
}

pub fn bif(
    process: &mut Process,
    module: &Module,
    op: BifOp,
    operands: &[Operand],
) -> Result<InstructionOutcome, ExecError> {
    let spec = BifSpec::parse(op, operands)?;
    let import_index = core::operand_usize(spec.import, "guard bif import index")?;
    let resolved = module
        .resolved_imports
        .get(import_index)
        .ok_or(ExecError::InvalidImport {
            index: import_index,
        })?;
    if usize::from(resolved.arity) != spec.args.len() {
        return Err(ExecError::InvalidOperand("guard bif arity mismatch"));
    }
    let ResolvedImportTarget::Native(entry) = resolved.target else {
        return Err(ExecError::InvalidOperand("guard bif native import"));
    };

    if let Some(heap_need) = spec.heap_need {
        core::test_heap(process, heap_need)?;
    }

    let mut args = Vec::with_capacity(spec.args.len());
    for arg in spec.args {
        args.push(core::read_term(process, arg)?);
    }

    let mut context = ProcessContext::new();
    match (entry.function)(&args, &mut context) {
        Ok(result) => {
            core::write_term(process, spec.destination, result)?;
            Ok(InstructionOutcome::Continue)
        }
        Err(_) => jump(module, spec.fail),
    }
}

pub(crate) fn branch_if_false(
    module: &Module,
    fail: &Operand,
    passed: bool,
) -> Result<InstructionOutcome, ExecError> {
    if passed {
        Ok(InstructionOutcome::Continue)
    } else {
        jump(module, fail)
    }
}

fn type_test_passes(op: TypeTestOp, value: Term) -> bool {
    match op {
        TypeTestOp::IsInteger => value.is_small_int(),
        TypeTestOp::IsFloat => Float::new(value).is_some(),
        TypeTestOp::IsNumber => value.is_small_int() || Float::new(value).is_some(),
        TypeTestOp::IsAtom => value.is_atom(),
        TypeTestOp::IsPid => value.is_pid(),
        TypeTestOp::IsReference => Reference::new(value).is_some(),
        TypeTestOp::IsPort => false,
        TypeTestOp::IsNil => value.is_nil(),
        TypeTestOp::IsBinary | TypeTestOp::IsBitstr => Binary::new(value).is_some(),
        TypeTestOp::IsList => value.is_nil() || value.is_list(),
        TypeTestOp::IsNonemptyList => value.is_list(),
        TypeTestOp::IsTuple => Tuple::new(value).is_some(),
        TypeTestOp::IsFunction => Closure::new(value).is_some(),
        TypeTestOp::IsBoolean => matches!(value.as_atom(), Some(Atom::TRUE | Atom::FALSE)),
        TypeTestOp::IsFunction2 => Closure::new(value).is_some(),
        TypeTestOp::IsMap => Map::new(value).is_some(),
        TypeTestOp::IsTaggedTuple => false,
    }
}

struct BifSpec<'a> {
    fail: &'a Operand,
    import: &'a Operand,
    args: &'a [Operand],
    destination: &'a Operand,
    heap_need: Option<&'a Operand>,
}

impl<'a> BifSpec<'a> {
    fn parse(op: BifOp, operands: &'a [Operand]) -> Result<Self, ExecError> {
        let arity = match op {
            BifOp::Bif0 => 0,
            BifOp::Bif1 | BifOp::GcBif1 => 1,
            BifOp::Bif2 | BifOp::GcBif2 => 2,
            BifOp::GcBif3 => 3,
        };
        if operands.len() == arity + 3 {
            return Ok(Self {
                fail: &operands[0],
                import: &operands[1],
                args: &operands[2..2 + arity],
                destination: &operands[2 + arity],
                heap_need: None,
            });
        }
        if matches!(op, BifOp::GcBif1 | BifOp::GcBif2 | BifOp::GcBif3)
            && operands.len() == arity + 4
        {
            return Ok(Self {
                fail: &operands[0],
                import: &operands[1],
                heap_need: Some(&operands[2]),
                args: &operands[3..3 + arity],
                destination: &operands[3 + arity],
            });
        }
        Err(ExecError::InvalidOperand("guard bif operands"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::Instruction;
    use crate::module::{ResolvedImport, ResolvedImportTarget};
    use crate::native::{NativeEntry, bifs};
    use crate::term::boxed::{write_closure, write_cons, write_float, write_tuple};
    use std::collections::HashMap;

    fn module(code: Vec<Instruction>) -> Module {
        Module {
            name: Atom::OK,
            exports: HashMap::new(),
            code,
            literals: Vec::new(),
            resolved_imports: Vec::new(),
            lambdas: Vec::new(),
            string_table: Vec::new(),
            line_info: Vec::new(),
        }
    }

    fn jump_ip(outcome: InstructionOutcome) -> usize {
        let InstructionOutcome::Jump(position) = outcome else {
            panic!("expected jump outcome, got {outcome:?}");
        };
        position.instruction_pointer
    }

    #[test]
    fn get_hd_and_get_tl_extract_cons_parts_without_copying() {
        let mut process = Process::new(1, 8);
        let ptr = process.heap_mut().alloc(2).expect("cons allocation");
        let cons = write_cons(
            unsafe { std::slice::from_raw_parts_mut(ptr, 2) },
            Term::small_int(1),
            Term::small_int(2),
        )
        .expect("cons term");
        process.set_x_reg(0, cons);

        assert_eq!(
            get_hd(&mut process, &Operand::X(0), &Operand::X(1)),
            Ok(InstructionOutcome::Continue)
        );
        assert_eq!(process.x_reg(1), Term::small_int(1));
        assert_eq!(
            get_tl(&mut process, &Operand::X(0), &Operand::X(2)),
            Ok(InstructionOutcome::Continue)
        );
        assert_eq!(process.x_reg(2), Term::small_int(2));

        assert_eq!(
            get_hd(&mut process, &Operand::Integer(0), &Operand::X(3)),
            Err(ExecError::Badarg)
        );
    }

    #[test]
    fn type_tests_fall_through_or_jump_to_fail_label() {
        let module = module(vec![
            Instruction::Label { label: 10 },
            Instruction::Return,
            Instruction::Label { label: 20 },
        ]);
        let mut process = Process::new(1, 16);
        let tuple_ptr = process.heap_mut().alloc(3).expect("tuple allocation");
        let tuple = write_tuple(
            unsafe { std::slice::from_raw_parts_mut(tuple_ptr, 3) },
            &[Term::small_int(1), Term::small_int(2)],
        )
        .expect("tuple term");
        process.set_x_reg(0, tuple);
        let closure_ptr = process.heap_mut().alloc(5).expect("closure allocation");
        let closure = write_closure(
            unsafe { std::slice::from_raw_parts_mut(closure_ptr, 5) },
            Atom::OK,
            0,
            2,
            &[],
        )
        .expect("closure term");
        process.set_x_reg(1, closure);

        assert_eq!(
            type_test(
                &mut process,
                &module,
                TypeTestOp::IsInteger,
                &Operand::Label(20),
                &Operand::Integer(1),
            ),
            Ok(InstructionOutcome::Continue)
        );
        assert_eq!(
            jump_ip(
                type_test(
                    &mut process,
                    &module,
                    TypeTestOp::IsInteger,
                    &Operand::Label(20),
                    &Operand::Atom(Some(Atom::OK)),
                )
                .expect("type test jump")
            ),
            2
        );
        assert_eq!(
            type_test(
                &mut process,
                &module,
                TypeTestOp::IsAtom,
                &Operand::Label(20),
                &Operand::Atom(Some(Atom::OK)),
            ),
            Ok(InstructionOutcome::Continue)
        );
        assert_eq!(
            type_test(
                &mut process,
                &module,
                TypeTestOp::IsTuple,
                &Operand::Label(20),
                &Operand::X(0),
            ),
            Ok(InstructionOutcome::Continue)
        );
        assert_eq!(
            type_test(
                &mut process,
                &module,
                TypeTestOp::IsNil,
                &Operand::Label(20),
                &Operand::Atom(None),
            ),
            Ok(InstructionOutcome::Continue)
        );
        assert_eq!(
            type_test(
                &mut process,
                &module,
                TypeTestOp::IsBoolean,
                &Operand::Label(20),
                &Operand::Atom(Some(Atom::TRUE)),
            ),
            Ok(InstructionOutcome::Continue)
        );
        assert_eq!(
            jump_ip(
                type_test(
                    &mut process,
                    &module,
                    TypeTestOp::IsBoolean,
                    &Operand::Label(20),
                    &Operand::Atom(Some(Atom::OK)),
                )
                .expect("type test jump")
            ),
            2
        );
        assert_eq!(
            type_test(
                &mut process,
                &module,
                TypeTestOp::IsFunction2,
                &Operand::Label(20),
                &Operand::X(1),
            ),
            Ok(InstructionOutcome::Continue)
        );
    }

    #[test]
    fn exact_and_ordering_comparisons_branch_with_beam_semantics() {
        let module = module(vec![Instruction::Label { label: 1 }]);
        let mut process = Process::new(1, 8);
        let float_ptr = process.heap_mut().alloc(2).expect("float allocation");
        let float_one = write_float(unsafe { std::slice::from_raw_parts_mut(float_ptr, 2) }, 1.0)
            .expect("float term");
        process.set_x_reg(0, float_one);

        assert_eq!(
            comparison(
                &process,
                &module,
                ComparisonOp::EqExact,
                &Operand::Label(1),
                &Operand::Integer(1),
                &Operand::Integer(1),
            ),
            Ok(InstructionOutcome::Continue)
        );
        assert!(matches!(
            comparison(
                &process,
                &module,
                ComparisonOp::EqExact,
                &Operand::Label(1),
                &Operand::Integer(1),
                &Operand::Integer(2),
            ),
            Ok(InstructionOutcome::Jump(_))
        ));
        assert!(matches!(
            comparison(
                &process,
                &module,
                ComparisonOp::EqExact,
                &Operand::Label(1),
                &Operand::Integer(1),
                &Operand::X(0),
            ),
            Ok(InstructionOutcome::Jump(_))
        ));
        assert_eq!(
            comparison(
                &process,
                &module,
                ComparisonOp::NeExact,
                &Operand::Label(1),
                &Operand::Integer(1),
                &Operand::Integer(2),
            ),
            Ok(InstructionOutcome::Continue)
        );
        assert!(matches!(
            comparison(
                &process,
                &module,
                ComparisonOp::NeExact,
                &Operand::Label(1),
                &Operand::Integer(1),
                &Operand::Integer(1),
            ),
            Ok(InstructionOutcome::Jump(_))
        ));
        assert_eq!(
            comparison(
                &process,
                &module,
                ComparisonOp::Lt,
                &Operand::Label(1),
                &Operand::Integer(1),
                &Operand::Integer(2),
            ),
            Ok(InstructionOutcome::Continue)
        );
        assert_eq!(
            comparison(
                &process,
                &module,
                ComparisonOp::Ge,
                &Operand::Label(1),
                &Operand::Integer(2),
                &Operand::Integer(1),
            ),
            Ok(InstructionOutcome::Continue)
        );
        assert_eq!(
            comparison(
                &process,
                &module,
                ComparisonOp::Lt,
                &Operand::Label(1),
                &Operand::Integer(1),
                &Operand::Atom(Some(Atom::OK)),
            ),
            Ok(InstructionOutcome::Continue)
        );
    }

    #[test]
    fn guard_bif_success_stores_result_and_failure_branches() {
        let import = ResolvedImport {
            module: Atom::OK,
            function: Atom::OK,
            arity: 2,
            target: ResolvedImportTarget::Native(NativeEntry {
                function: bifs::add,
                is_dirty: false,
            }),
        };
        let mut module = module(vec![Instruction::Label { label: 7 }]);
        module.resolved_imports.push(import);
        let mut process = Process::new(1, 16);

        assert_eq!(
            bif(
                &mut process,
                &module,
                BifOp::GcBif2,
                &[
                    Operand::Label(7),
                    Operand::Unsigned(0),
                    Operand::Integer(3),
                    Operand::Integer(4),
                    Operand::X(0),
                ],
            ),
            Ok(InstructionOutcome::Continue)
        );
        assert_eq!(process.x_reg(0), Term::small_int(7));
        assert_eq!(
            bif(
                &mut process,
                &module,
                BifOp::Bif2,
                &[
                    Operand::Label(7),
                    Operand::Unsigned(0),
                    Operand::Atom(Some(Atom::OK)),
                    Operand::Integer(1),
                    Operand::X(1),
                ],
            ),
            Ok(InstructionOutcome::Jump(CodePosition {
                module: Atom::OK,
                instruction_pointer: 0,
            }))
        );
    }

    #[test]
    fn jump_sets_target_without_modifying_registers() {
        let module = module(vec![
            Instruction::Label { label: 1 },
            Instruction::Label { label: 2 },
        ]);
        let mut process = Process::new(1, 8);
        process.set_x_reg(0, Term::small_int(42));

        assert_eq!(
            jump(&module, &Operand::Label(2)),
            Ok(InstructionOutcome::Jump(CodePosition {
                module: Atom::OK,
                instruction_pointer: 1,
            }))
        );
        assert_eq!(process.x_reg(0), Term::small_int(42));
    }
}
