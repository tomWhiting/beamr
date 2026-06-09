//! Binary matching and construction opcode lowering for the JIT compiler.

use crate::loader::decode::BinaryOp;
use crate::loader::decode::compact::Operand;
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{Block, FuncRef, InstBuilder, Value, types};
use cranelift_frontend::FunctionBuilder;

use super::compiler::JitError;
use super::ir_common::{branch_to_fail_if, read_operand_term, write_operand_term};
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
        _ => Ok(Vec::new()),
    }
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

fn lower_start_match(
    builder: &mut FunctionBuilder<'_>,
    context: BinaryLoweringContext,
    helper: FuncRef,
    source: &Operand,
    destination: &Operand,
    fail: Block,
) -> Result<(), JitError> {
    let binary = read_operand_term(builder, context.register_file, source)?;
    let call = builder.ins().call(helper, &[context.process, binary]);
    let match_context = builder.inst_results(call)[0];
    branch_to_fail_if_null(builder, match_context, fail);
    write_operand_term(builder, context.register_file, destination, match_context)
}

struct IntegerGetLowering<'a> {
    helper: FuncRef,
    match_context: &'a Operand,
    bits: u64,
    flags: u64,
    destination: &'a Operand,
    fail: Block,
}

fn lower_get_integer(
    builder: &mut FunctionBuilder<'_>,
    context: BinaryLoweringContext,
    lowering: IntegerGetLowering<'_>,
) -> Result<(), JitError> {
    let match_context = read_operand_term(builder, context.register_file, lowering.match_context)?;
    let bits = iconst_u64(builder, lowering.bits);
    let flags = iconst_u64(builder, lowering.flags);
    let call = builder
        .ins()
        .call(lowering.helper, &[match_context, bits, flags]);
    let value = builder.inst_results(call)[0];
    branch_to_fail_if_null(builder, value, lowering.fail);
    write_operand_term(builder, context.register_file, lowering.destination, value)
}

fn lower_get_binary(
    builder: &mut FunctionBuilder<'_>,
    context: BinaryLoweringContext,
    helper: FuncRef,
    match_context: &Operand,
    bits: u64,
    destination: &Operand,
    fail: Block,
) -> Result<(), JitError> {
    let match_context = read_operand_term(builder, context.register_file, match_context)?;
    let bits = iconst_u64(builder, bits);
    let call = builder
        .ins()
        .call(helper, &[context.process, match_context, bits]);
    let value = builder.inst_results(call)[0];
    branch_to_fail_if_null(builder, value, fail);
    write_operand_term(builder, context.register_file, destination, value)
}

fn lower_get_utf(
    builder: &mut FunctionBuilder<'_>,
    context: BinaryLoweringContext,
    helper: FuncRef,
    match_context: &Operand,
    flags: u64,
    destination: &Operand,
    fail: Block,
) -> Result<(), JitError> {
    let match_context = read_operand_term(builder, context.register_file, match_context)?;
    let flags = iconst_u64(builder, flags);
    let call = builder.ins().call(helper, &[match_context, flags]);
    let value = builder.inst_results(call)[0];
    branch_to_fail_if_null(builder, value, fail);
    write_operand_term(builder, context.register_file, destination, value)
}

fn lower_test(
    builder: &mut FunctionBuilder<'_>,
    context: BinaryLoweringContext,
    helper: FuncRef,
    match_context: &Operand,
    expected: u64,
    fail: Block,
) -> Result<(), JitError> {
    let match_context = read_operand_term(builder, context.register_file, match_context)?;
    let expected = iconst_u64(builder, expected);
    let call = builder.ins().call(helper, &[match_context, expected]);
    let ok = builder.inst_results(call)[0];
    let failed = builder.ins().icmp_imm(IntCC::Equal, ok, 0);
    branch_to_fail_if(builder, failed, fail);
    Ok(())
}

fn lower_bs_init_writable(
    builder: &mut FunctionBuilder<'_>,
    context: BinaryLoweringContext,
    helper: FuncRef,
    operands: &[Operand],
) -> Result<(), JitError> {
    let [size_hint, destination] = operands else {
        return Err(invalid_operands("bs_init_writable"));
    };
    let size_hint = iconst_u64(builder, immediate_u64(size_hint, "bs_init_writable size")?);
    let call = builder.ins().call(helper, &[context.process, size_hint]);
    let builder_term = builder.inst_results(call)[0];
    branch_to_deopt_if_null(builder, builder_term, context.deopt);
    write_operand_term(builder, context.register_file, destination, builder_term)
}

fn lower_bs_create_bin(
    builder: &mut FunctionBuilder<'_>,
    context: BinaryLoweringContext,
    helpers: BinaryHelpers,
    operands: &[Operand],
) -> Result<(), JitError> {
    let _utf_put_helpers = (helpers.put_utf16, helpers.put_utf32);
    let [destination, size_hint, segments @ ..] = operands else {
        return Err(invalid_operands("bs_create_bin"));
    };
    let size_hint = iconst_u64(builder, immediate_u64(size_hint, "bs_create_bin size")?);
    let call = builder
        .ins()
        .call(helpers.init, &[context.process, size_hint]);
    let builder_term = builder.inst_results(call)[0];
    branch_to_deopt_if_null(builder, builder_term, context.deopt);
    for segment in segments {
        lower_create_bin_segment(builder, context, helpers, builder_term, segment)?;
    }
    let call = builder
        .ins()
        .call(helpers.finish, &[context.process, builder_term]);
    let result = builder.inst_results(call)[0];
    branch_to_deopt_if_null(builder, result, context.deopt);
    write_operand_term(builder, context.register_file, destination, result)
}

fn lower_create_bin_segment(
    builder: &mut FunctionBuilder<'_>,
    context: BinaryLoweringContext,
    helpers: BinaryHelpers,
    builder_term: Value,
    segment: &Operand,
) -> Result<(), JitError> {
    let Operand::List(fields) = segment else {
        return Err(invalid_operands("bs_create_bin segment"));
    };
    match fields.as_slice() {
        [Operand::Atom(None), value, size, unit, flags] => {
            let value = read_operand_term(builder, context.register_file, value)?;
            let bits = iconst_u64(
                builder,
                segment_bits(size, unit, "bs_create_bin integer size")?,
            );
            let flags = iconst_u64(builder, flags_to_raw(flags));
            let call = builder.ins().call(
                helpers.put_integer,
                &[context.process, builder_term, value, bits, flags],
            );
            dispatch_helper_status(builder, context, call);
            Ok(())
        }
        [Operand::Atom(None), source] => {
            let source = read_operand_term(builder, context.register_file, source)?;
            let call = builder
                .ins()
                .call(helpers.put_binary, &[context.process, builder_term, source]);
            dispatch_helper_status(builder, context, call);
            Ok(())
        }
        [Operand::Atom(Some(atom)), value] if *atom == crate::atom::Atom::UTF8 => {
            let value = read_operand_term(builder, context.register_file, value)?;
            let call = builder
                .ins()
                .call(helpers.put_utf8, &[context.process, builder_term, value]);
            dispatch_helper_status(builder, context, call);
            Ok(())
        }
        _ => Err(invalid_operands("bs_create_bin segment")),
    }
}

fn lower_bs_get_tail(
    builder: &mut FunctionBuilder<'_>,
    context: BinaryLoweringContext,
    helper: FuncRef,
    operands: &[Operand],
) -> Result<(), JitError> {
    let (fail_operand, match_context, destination) = match operands {
        [fail, context, _live, destination] => (fail, context, destination),
        [fail, context, destination] => (fail, context, destination),
        _ => return Err(invalid_operands("bs_get_tail")),
    };
    let fail = context.deopt;
    let _ = fail_operand;
    lower_get_binary(
        builder,
        context,
        helper,
        match_context,
        u64::MAX,
        destination,
        fail,
    )
}

fn lower_bs_match(
    builder: &mut FunctionBuilder<'_>,
    context: BinaryLoweringContext,
    helpers: BinaryHelpers,
    operands: &[Operand],
    fail: Option<Block>,
) -> Result<(), JitError> {
    let (fail_operand, match_context_operand, commands) = match operands {
        [fail, context, Operand::List(commands)] => (fail, context, commands.as_slice()),
        [fail, context, rest @ ..] => (fail, context, rest),
        _ => return Err(invalid_operands("bs_match")),
    };
    let fail = required_fail(fail, fail_operand)?;
    if commands
        .iter()
        .all(|command| matches!(command, Operand::List(_)))
    {
        for command in commands {
            lower_nested_match_command(
                builder,
                context,
                helpers,
                match_context_operand,
                command,
                fail,
            )?;
        }
        return Ok(());
    }
    Err(JitError::UnsupportedOperand {
        operand: "flat bs_match command stream".to_owned(),
    })
}

fn lower_nested_match_command(
    builder: &mut FunctionBuilder<'_>,
    context: BinaryLoweringContext,
    helpers: BinaryHelpers,
    match_context: &Operand,
    command: &Operand,
    fail: Block,
) -> Result<(), JitError> {
    let Operand::List(items) = command else {
        return Err(invalid_operands("bs_match command"));
    };
    match items.as_slice() {
        [Operand::Unsigned(0) | Operand::Integer(0), _live, bits] => lower_test(
            builder,
            context,
            helpers.test_unit,
            match_context,
            immediate_u64(bits, "bs_match ensure bits")?,
            fail,
        )?,
        [Operand::Unsigned(1) | Operand::Integer(1), bits, _unit] => lower_test(
            builder,
            context,
            helpers.test_tail,
            match_context,
            immediate_u64(bits, "bs_match ensure exactly")?,
            fail,
        )?,
        [
            Operand::Unsigned(2) | Operand::Integer(2),
            _live,
            flags,
            size,
            unit,
            dst,
        ] => {
            lower_get_integer(
                builder,
                context,
                IntegerGetLowering {
                    helper: helpers.get_integer,
                    match_context,
                    bits: segment_bits(size, unit, "bs_match integer size")?,
                    flags: flags_to_raw(flags),
                    destination: dst,
                    fail,
                },
            )?;
        }
        [
            Operand::Unsigned(4) | Operand::Integer(4),
            _live,
            _flags,
            size,
            unit,
            dst,
        ] => {
            lower_get_binary(
                builder,
                context,
                helpers.get_binary,
                match_context,
                segment_bits(size, unit, "bs_match binary size")?,
                dst,
                fail,
            )?;
        }
        [Operand::Unsigned(6) | Operand::Integer(6), _live, dst]
        | [Operand::Unsigned(6) | Operand::Integer(6), _live, _, dst] => {
            lower_get_binary(
                builder,
                context,
                helpers.get_binary,
                match_context,
                u64::MAX,
                dst,
                fail,
            )?;
        }
        _ => {
            return Err(JitError::UnsupportedOperand {
                operand: format!("bs_match command {items:?}"),
            });
        }
    }
    Ok(())
}

fn dispatch_helper_status(
    builder: &mut FunctionBuilder<'_>,
    context: BinaryLoweringContext,
    call: cranelift_codegen::ir::Inst,
) {
    let status = builder.inst_results(call)[0];
    let ok = builder.ins().icmp_imm(IntCC::Equal, status, 0);
    let continuation = builder.create_block();
    builder
        .ins()
        .brif(ok, continuation, &[], context.exception, &[]);
    builder.switch_to_block(continuation);
}

fn branch_to_fail_if_null(builder: &mut FunctionBuilder<'_>, value: Value, fail: Block) {
    let is_null = builder.ins().icmp_imm(IntCC::Equal, value, 0);
    branch_to_fail_if(builder, is_null, fail);
}

fn branch_to_deopt_if_null(builder: &mut FunctionBuilder<'_>, value: Value, deopt: Block) {
    let is_null = builder.ins().icmp_imm(IntCC::Equal, value, 0);
    branch_to_fail_if(builder, is_null, deopt);
}

pub(crate) fn lower_exception_block(builder: &mut FunctionBuilder<'_>) {
    let reason = builder.ins().iconst(
        types::I64,
        crate::term::Term::atom(crate::atom::Atom::BADARG).raw() as i64,
    );
    return_status(builder, JIT_STATUS_EXCEPTION, reason);
}

fn parse_start_match_operands(
    operands: &[Operand],
) -> Result<(&Operand, &Operand, &Operand), JitError> {
    match operands {
        [fail, source, destination] => Ok((fail, source, destination)),
        [fail, source, _live, destination] => Ok((fail, source, destination)),
        _ => Err(invalid_operands("bs_start_match")),
    }
}

fn parse_get_operands<'a>(
    operands: &'a [Operand],
    context: &'static str,
) -> Result<
    (
        &'a Operand,
        &'a Operand,
        &'a Operand,
        &'a Operand,
        &'a Operand,
        &'a Operand,
    ),
    JitError,
> {
    match operands {
        [fail, match_context, _live, size, unit, flags, destination] => {
            Ok((fail, match_context, size, unit, flags, destination))
        }
        [fail, match_context, size, unit, flags, destination] => {
            Ok((fail, match_context, size, unit, flags, destination))
        }
        _ => Err(invalid_operands(context)),
    }
}

fn parse_utf_get_operands<'a>(
    operands: &'a [Operand],
    context: &'static str,
) -> Result<(&'a Operand, &'a Operand, &'a Operand, &'a Operand), JitError> {
    match operands {
        [fail, match_context, _live, flags, destination] => {
            Ok((fail, match_context, flags, destination))
        }
        [fail, match_context, flags, destination] => Ok((fail, match_context, flags, destination)),
        _ => Err(invalid_operands(context)),
    }
}

fn required_fail(resolved: Option<Block>, operand: &Operand) -> Result<Block, JitError> {
    resolved.ok_or_else(|| JitError::UnsupportedOperand {
        operand: format!("missing fail block for {operand:?}"),
    })
}

fn segment_bits(size: &Operand, unit: &Operand, context: &'static str) -> Result<u64, JitError> {
    let size = immediate_u64(size, context)?;
    let unit = immediate_u64(unit, context)?;
    size.checked_mul(unit)
        .ok_or_else(|| JitError::UnsupportedOperand {
            operand: format!("{context} overflows"),
        })
}

fn immediate_u64(operand: &Operand, context: &'static str) -> Result<u64, JitError> {
    match operand {
        Operand::Unsigned(value) => Ok(*value),
        Operand::Integer(value) => {
            u64::try_from(*value).map_err(|_| JitError::UnsupportedOperand {
                operand: format!("{context}: {operand:?}"),
            })
        }
        _ => Err(JitError::UnsupportedOperand {
            operand: format!("{context}: {operand:?}"),
        }),
    }
}

fn flags_to_raw(flags: &Operand) -> u64 {
    match flags {
        Operand::Unsigned(value) => *value,
        Operand::Integer(value) => u64::try_from(*value).map_or(0, |raw| raw),
        Operand::List(items) => items
            .iter()
            .fold(0_u64, |bits, item| bits | flags_to_raw(item)),
        _ => 0,
    }
}

fn iconst_u64(builder: &mut FunctionBuilder<'_>, value: u64) -> Value {
    builder.ins().iconst(types::I64, value as i64)
}

fn segment_roots(segments: &[Operand]) -> Vec<Operand> {
    let mut roots = Vec::new();
    for segment in segments {
        if let Operand::List(fields) = segment {
            match fields.as_slice() {
                [Operand::Atom(None), value, _size, _unit, _flags] => roots.push(value.clone()),
                [Operand::Atom(None), source] => roots.push(source.clone()),
                _ => {}
            }
        }
    }
    roots
}

fn invalid_operands(context: &'static str) -> JitError {
    JitError::UnsupportedOperand {
        operand: format!("invalid {context} operands"),
    }
}
