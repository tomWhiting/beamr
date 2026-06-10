//! Private lowering functions for binary matching and construction opcodes.
//!
//! Split from `ir_binary` to keep each file under 500 lines.

use crate::loader::decode::compact::Operand;
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{Block, FuncRef, InstBuilder, Value, types};
use cranelift_frontend::FunctionBuilder;

use super::compiler::JitError;
use super::ir_binary::{BinaryHelpers, BinaryLoweringContext};
use super::ir_common::{branch_to_fail_if, read_operand_term, write_operand_term};

pub(super) struct IntegerGetLowering<'a> {
    pub(super) helper: FuncRef,
    pub(super) match_context: &'a Operand,
    pub(super) bits: u64,
    pub(super) flags: u64,
    pub(super) destination: &'a Operand,
    pub(super) fail: Block,
}

pub(super) fn lower_start_match(
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
    branch_to_fail_if_invalid_binary_result(builder, match_context, fail);
    write_operand_term(builder, context.register_file, destination, match_context)
}

pub(super) fn lower_get_integer(
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
    branch_to_fail_if_invalid_binary_result(builder, value, lowering.fail);
    write_operand_term(builder, context.register_file, lowering.destination, value)
}

pub(super) fn lower_get_binary(
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
    branch_to_fail_if_invalid_binary_result(builder, value, fail);
    write_operand_term(builder, context.register_file, destination, value)
}

pub(super) fn lower_get_utf(
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
    branch_to_fail_if_invalid_binary_result(builder, value, fail);
    write_operand_term(builder, context.register_file, destination, value)
}

pub(super) fn lower_test(
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

pub(super) fn lower_bs_init_writable(
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

pub(super) fn lower_bs_create_bin(
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

pub(super) fn lower_bs_get_tail(
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

pub(super) fn lower_bs_match(
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

fn branch_to_fail_if_invalid_binary_result(
    builder: &mut FunctionBuilder<'_>,
    value: Value,
    fail: Block,
) {
    let is_null = builder.ins().icmp_imm(IntCC::Equal, value, 0);
    branch_to_fail_if(builder, is_null, fail);
    let is_failure = builder.ins().icmp_imm(IntCC::Equal, value, -1);
    branch_to_fail_if(builder, is_failure, fail);
}

fn branch_to_deopt_if_null(builder: &mut FunctionBuilder<'_>, value: Value, deopt: Block) {
    let is_null = builder.ins().icmp_imm(IntCC::Equal, value, 0);
    branch_to_fail_if(builder, is_null, deopt);
}

pub(super) fn parse_start_match_operands(
    operands: &[Operand],
) -> Result<(&Operand, &Operand, &Operand), JitError> {
    match operands {
        [fail, source, destination] => Ok((fail, source, destination)),
        [fail, source, _live, destination] => Ok((fail, source, destination)),
        _ => Err(invalid_operands("bs_start_match")),
    }
}

pub(super) fn parse_get_operands<'a>(
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

pub(super) fn parse_utf_get_operands<'a>(
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

pub(super) fn required_fail(resolved: Option<Block>, operand: &Operand) -> Result<Block, JitError> {
    resolved.ok_or_else(|| JitError::UnsupportedOperand {
        operand: format!("missing fail block for {operand:?}"),
    })
}

pub(super) fn segment_bits(
    size: &Operand,
    unit: &Operand,
    context: &'static str,
) -> Result<u64, JitError> {
    let size = immediate_u64(size, context)?;
    let unit = immediate_u64(unit, context)?;
    size.checked_mul(unit)
        .ok_or_else(|| JitError::UnsupportedOperand {
            operand: format!("{context} overflows"),
        })
}

pub(super) fn immediate_u64(operand: &Operand, context: &'static str) -> Result<u64, JitError> {
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

pub(super) fn flags_to_raw(flags: &Operand) -> u64 {
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

pub(super) fn segment_roots(segments: &[Operand]) -> Vec<Operand> {
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

pub(super) fn invalid_operands(context: &'static str) -> JitError {
    JitError::UnsupportedOperand {
        operand: format!("invalid {context} operands"),
    }
}
