//! Binary matching and construction opcode lowering for the JIT compiler.

use crate::loader::decode::BinaryOp;
use crate::loader::decode::compact::Operand;
use cranelift_codegen::ir::{Block, FuncRef, InstBuilder, Value, types};
use cranelift_frontend::FunctionBuilder;

use super::compiler::JitError;
use super::ir_binary_lowering::{
    IntegerGetLowering, flags_to_raw, immediate_u64, invalid_operands, lower_bs_create_bin,
    lower_bs_get_tail, lower_bs_init_writable, lower_bs_match, lower_get_binary, lower_get_integer,
    lower_get_utf, lower_start_match, lower_test, parse_get_operands, parse_start_match_operands,
    parse_utf_get_operands, required_fail, segment_bits, segment_roots,
};
use super::ir_exceptions::{JIT_STATUS_EXCEPTION, return_status};

#[derive(Clone, Copy)]
pub(crate) struct BinaryHelpers {
    pub(crate) start_match: FuncRef,
    pub(crate) get_integer: FuncRef,
    pub(crate) get_binary: FuncRef,
    pub(crate) test_tail: FuncRef,
    pub(crate) test_unit: FuncRef,
    pub(crate) get_utf8: FuncRef,
    pub(crate) get_utf16: FuncRef,
    pub(crate) get_utf32: FuncRef,
    pub(crate) init: FuncRef,
    pub(crate) put_integer: FuncRef,
    pub(crate) put_binary: FuncRef,
    pub(crate) put_utf8: FuncRef,
    pub(crate) put_utf16: FuncRef,
    pub(crate) put_utf32: FuncRef,
    pub(crate) finish: FuncRef,
}

#[derive(Clone, Copy)]
pub(crate) struct BinaryLoweringContext {
    pub(crate) register_file: Value,
    pub(crate) process: Value,
    pub(crate) deopt: Block,
    pub(crate) exception: Block,
}

pub(crate) fn lower_binary_op(
    builder: &mut FunctionBuilder<'_>,
    context: BinaryLoweringContext,
    helpers: BinaryHelpers,
    op: BinaryOp,
    operands: &[Operand],
    fail: Option<Block>,
) -> Result<(), JitError> {
    match op {
        BinaryOp::BsStartMatch3 | BinaryOp::BsStartMatch4 => {
            let (fail_operand, source, destination) = parse_start_match_operands(operands)?;
            let fail = required_fail(fail, fail_operand)?;
            lower_start_match(
                builder,
                context,
                helpers.start_match,
                source,
                destination,
                fail,
            )
        }
        BinaryOp::BsGetInteger2 => {
            let (fail_operand, match_context, size, unit, flags, destination) =
                parse_get_operands(operands, "bs_get_integer2")?;
            let fail = required_fail(fail, fail_operand)?;
            lower_get_integer(
                builder,
                context,
                IntegerGetLowering {
                    helper: helpers.get_integer,
                    match_context,
                    bits: segment_bits(size, unit, "bs_get_integer2 size")?,
                    flags: flags_to_raw(flags),
                    destination,
                    fail,
                },
            )
        }
        BinaryOp::BsGetBinary2 => {
            let (fail_operand, match_context, size, unit, _flags, destination) =
                parse_get_operands(operands, "bs_get_binary2")?;
            let fail = required_fail(fail, fail_operand)?;
            lower_get_binary(
                builder,
                context,
                helpers.get_binary,
                match_context,
                segment_bits(size, unit, "bs_get_binary2 size")?,
                destination,
                fail,
            )
        }
        BinaryOp::BsTestTail2 => {
            let [fail_operand, match_context, expected] = operands else {
                return Err(invalid_operands("bs_test_tail2"));
            };
            let fail = required_fail(fail, fail_operand)?;
            lower_test(
                builder,
                context,
                helpers.test_tail,
                match_context,
                immediate_u64(expected, "bs_test_tail2 expected bits")?,
                fail,
            )
        }
        BinaryOp::BsTestUnit => {
            let [fail_operand, match_context, unit] = operands else {
                return Err(invalid_operands("bs_test_unit"));
            };
            let fail = required_fail(fail, fail_operand)?;
            lower_test(
                builder,
                context,
                helpers.test_unit,
                match_context,
                immediate_u64(unit, "bs_test_unit unit")?,
                fail,
            )
        }
        BinaryOp::BsGetUtf8 | BinaryOp::BsGetUtf16 | BinaryOp::BsGetUtf32 => {
            let (fail_operand, match_context, flags, destination) =
                parse_utf_get_operands(operands, "bs_get_utf operands")?;
            let fail = required_fail(fail, fail_operand)?;
            let helper = match op {
                BinaryOp::BsGetUtf8 => helpers.get_utf8,
                BinaryOp::BsGetUtf16 => helpers.get_utf16,
                BinaryOp::BsGetUtf32 => helpers.get_utf32,
                _ => helpers.get_utf8,
            };
            lower_get_utf(
                builder,
                context,
                helper,
                match_context,
                flags_to_raw(flags),
                destination,
                fail,
            )
        }
        BinaryOp::BsInitWritable => {
            lower_bs_init_writable(builder, context, helpers.init, operands)
        }
        BinaryOp::BsCreateBin => lower_bs_create_bin(builder, context, helpers, operands),
        BinaryOp::BsMatch => lower_bs_match(builder, context, helpers, operands, fail),
        BinaryOp::BsGetTail => lower_bs_get_tail(builder, context, helpers.get_binary, operands),
        _ => Err(JitError::UnsupportedOpcode {
            opcode: format!("binary op {op:?}"),
        }),
    }
}

pub(crate) fn binary_allocation_roots(
    op: BinaryOp,
    operands: &[Operand],
) -> Result<Vec<Operand>, JitError> {
    match op {
        BinaryOp::BsStartMatch3 | BinaryOp::BsStartMatch4 => {
            let (_fail, source, destination) = parse_start_match_operands(operands)?;
            Ok(vec![source.clone(), destination.clone()])
        }
        BinaryOp::BsGetBinary2 => {
            let (_fail, match_context, _size, _unit, _flags, destination) =
                parse_get_operands(operands, "bs_get_binary2")?;
            Ok(vec![match_context.clone(), destination.clone()])
        }
        BinaryOp::BsInitWritable => match operands {
            [_, destination] => Ok(vec![destination.clone()]),
            _ => Ok(Vec::new()),
        },
        BinaryOp::BsCreateBin => match operands {
            [destination, _, segments @ ..] => {
                let mut roots = vec![destination.clone()];
                roots.extend(segment_roots(segments));
                Ok(roots)
            }
            _ => Ok(Vec::new()),
        },
        BinaryOp::BsGetTail => match operands {
            [_fail, context, _live, destination] => Ok(vec![context.clone(), destination.clone()]),
            [_fail, context, destination] => Ok(vec![context.clone(), destination.clone()]),
            _ => Ok(Vec::new()),
        },
        BinaryOp::BsMatch => bs_match_allocation_roots(operands),
        _ => Ok(Vec::new()),
    }
}

fn bs_match_allocation_roots(operands: &[Operand]) -> Result<Vec<Operand>, JitError> {
    let (_fail, match_context, commands) = match operands {
        [fail, context, Operand::List(commands)] => (fail, context, commands.as_slice()),
        [fail, context, rest @ ..] => (fail, context, rest),
        _ => return Err(invalid_operands("bs_match")),
    };
    if !commands
        .iter()
        .all(|command| matches!(command, Operand::List(_)))
    {
        return Err(JitError::UnsupportedOperand {
            operand: "flat bs_match command stream".to_owned(),
        });
    }

    let mut destinations = Vec::new();
    for command in commands {
        let Operand::List(items) = command else {
            return Err(invalid_operands("bs_match command"));
        };
        match items.as_slice() {
            [Operand::Unsigned(4) | Operand::Integer(4), .., dst] if items.len() == 6 => {
                destinations.push(dst.clone());
            }
            [Operand::Unsigned(6) | Operand::Integer(6), .., dst]
                if items.len() == 3 || items.len() == 4 =>
            {
                destinations.push(dst.clone());
            }
            [Operand::Unsigned(0) | Operand::Integer(0), ..] if items.len() == 3 => {}
            [Operand::Unsigned(1) | Operand::Integer(1), ..] if items.len() == 3 => {}
            [Operand::Unsigned(2) | Operand::Integer(2), ..] if items.len() == 6 => {}
            [Operand::Unsigned(3) | Operand::Integer(3), ..] if items.len() == 6 => {}
            _ => {
                return Err(JitError::UnsupportedOperand {
                    operand: format!("bs_match command {items:?}"),
                });
            }
        }
    }
    if destinations.is_empty() {
        return Ok(Vec::new());
    }
    let mut roots = Vec::with_capacity(destinations.len() + 1);
    roots.push(match_context.clone());
    roots.extend(destinations);
    Ok(roots)
}

pub(crate) fn fail_operand(op: BinaryOp, operands: &[Operand]) -> Option<&Operand> {
    match op {
        BinaryOp::BsStartMatch3
        | BinaryOp::BsStartMatch4
        | BinaryOp::BsGetInteger2
        | BinaryOp::BsGetBinary2
        | BinaryOp::BsTestTail2
        | BinaryOp::BsTestUnit
        | BinaryOp::BsGetUtf8
        | BinaryOp::BsGetUtf16
        | BinaryOp::BsGetUtf32
        | BinaryOp::BsMatch
        | BinaryOp::BsGetTail => operands.first(),
        _ => None,
    }
}

pub(crate) fn lower_exception_block(builder: &mut FunctionBuilder<'_>) {
    let reason = builder.ins().iconst(
        types::I64,
        crate::term::Term::atom(crate::atom::Atom::BADARG).raw() as i64,
    );
    return_status(builder, JIT_STATUS_EXCEPTION, reason);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bs_match_roots(commands: Vec<Operand>) -> Vec<Operand> {
        binary_allocation_roots(
            BinaryOp::BsMatch,
            &[Operand::Label(1), Operand::X(0), Operand::List(commands)],
        )
        .expect("bs_match roots")
    }

    fn get_binary_command(destination: Operand) -> Operand {
        Operand::List(vec![
            Operand::Unsigned(4),
            Operand::Unsigned(0),
            Operand::Unsigned(0),
            Operand::Unsigned(8),
            Operand::Unsigned(1),
            destination,
        ])
    }

    fn get_tail_command(destination: Operand) -> Operand {
        Operand::List(vec![
            Operand::Unsigned(6),
            Operand::Unsigned(0),
            destination,
        ])
    }

    fn get_integer_command(destination: Operand) -> Operand {
        Operand::List(vec![
            Operand::Unsigned(2),
            Operand::Unsigned(0),
            Operand::Unsigned(0),
            Operand::Unsigned(8),
            Operand::Unsigned(1),
            destination,
        ])
    }

    fn get_float_command(destination: Operand) -> Operand {
        Operand::List(vec![
            Operand::Unsigned(3),
            Operand::Unsigned(0),
            Operand::Unsigned(0),
            Operand::Unsigned(64),
            Operand::Unsigned(1),
            destination,
        ])
    }

    #[test]
    fn bs_match_get_binary_roots_match_context_and_destination() {
        let roots = bs_match_roots(vec![get_binary_command(Operand::X(1))]);

        assert_eq!(roots, vec![Operand::X(0), Operand::X(1)]);
    }

    #[test]
    fn bs_match_get_tail_roots_match_context_and_destination() {
        let roots = bs_match_roots(vec![get_tail_command(Operand::Y(2))]);

        assert_eq!(roots, vec![Operand::X(0), Operand::Y(2)]);
    }

    #[test]
    fn bs_match_multiple_allocating_commands_root_all_destinations() {
        let roots = bs_match_roots(vec![
            get_binary_command(Operand::X(1)),
            get_tail_command(Operand::X(2)),
        ]);

        assert_eq!(roots, vec![Operand::X(0), Operand::X(1), Operand::X(2)]);
    }

    #[test]
    fn bs_match_non_allocating_commands_add_no_roots() {
        let roots = bs_match_roots(vec![
            get_integer_command(Operand::X(1)),
            get_float_command(Operand::X(2)),
        ]);

        assert!(roots.is_empty());
    }
}
