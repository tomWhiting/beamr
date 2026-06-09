//! IR lowering for BEAM message send and selective receive opcodes.

use crate::loader::decode::Operand;
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{Block, FuncRef, InstBuilder, Value};
use cranelift_frontend::FunctionBuilder;

use super::compiler::JitError;
use super::ir_common::{read_operand_term, write_operand_term};

const RECEIVE_STATUS_MESSAGE: i64 = 0;
const RECEIVE_STATUS_EMPTY: i64 = 1;
const WAIT_STATUS_NEW_MESSAGE: i64 = 0;
const WAIT_STATUS_TIMEOUT: i64 = 1;
const WAIT_STATUS_WAITING: i64 = 2;

/// Runtime helpers used by message-send and receive lowering.
#[derive(Clone, Copy)]
pub(crate) struct MessageHelpers {
    pub(crate) send: FuncRef,
    pub(crate) receive_peek: FuncRef,
    pub(crate) receive_next: FuncRef,
    pub(crate) receive_accept: FuncRef,
    pub(crate) receive_wait: FuncRef,
    pub(crate) receive_wait_timeout: FuncRef,
    pub(crate) receive_timeout: FuncRef,
}

/// Common SSA values needed by message opcode lowering.
#[derive(Clone, Copy)]
pub(crate) struct MessageLoweringContext {
    pub(crate) register_file: Value,
    pub(crate) process: Value,
    pub(crate) deopt: Block,
    pub(crate) yield_block: Block,
}

pub(crate) fn translate_send(
    builder: &mut FunctionBuilder<'_>,
    context: MessageLoweringContext,
    helpers: MessageHelpers,
    dest: &Operand,
    message: &Operand,
    result_dest: &Operand,
) -> Result<(), JitError> {
    let dest_pid = read_operand_term(builder, context.register_file, dest)?;
    let message = read_operand_term(builder, context.register_file, message)?;
    let call = builder
        .ins()
        .call(helpers.send, &[context.process, dest_pid, message]);
    let sent = builder.inst_results(call)[0];
    write_operand_term(builder, context.register_file, result_dest, sent)
}

pub(crate) fn translate_loop_rec(
    builder: &mut FunctionBuilder<'_>,
    context: MessageLoweringContext,
    helpers: MessageHelpers,
    fail_label: Block,
    destination: &Operand,
) -> Result<(), JitError> {
    let call = builder.ins().call(helpers.receive_peek, &[context.process]);
    let results = builder.inst_results(call).to_vec();
    branch_to_deopt_on_status(builder, results[0], context.deopt);
    let exhausted = builder
        .ins()
        .icmp_imm(IntCC::Equal, results[0], RECEIVE_STATUS_EMPTY);
    let continuation = builder.create_block();
    builder
        .ins()
        .brif(exhausted, fail_label, &[], continuation, &[]);
    builder.switch_to_block(continuation);
    write_operand_term(builder, context.register_file, destination, results[1])
}

pub(crate) fn translate_loop_rec_end(
    builder: &mut FunctionBuilder<'_>,
    context: MessageLoweringContext,
    helpers: MessageHelpers,
    loop_label: Block,
) {
    builder.ins().call(helpers.receive_next, &[context.process]);
    builder.ins().jump(loop_label, &[]);
}

pub(crate) fn translate_remove_message(
    builder: &mut FunctionBuilder<'_>,
    context: MessageLoweringContext,
    helpers: MessageHelpers,
) {
    builder
        .ins()
        .call(helpers.receive_accept, &[context.process]);
}

pub(crate) fn translate_wait(
    builder: &mut FunctionBuilder<'_>,
    context: MessageLoweringContext,
    helpers: MessageHelpers,
    charge_helper: FuncRef,
    loop_label: Block,
) {
    let call = builder.ins().call(helpers.receive_wait, &[context.process]);
    let status = builder.inst_results(call)[0];
    charge_reduction_or_yield(builder, charge_helper, context.process, context.yield_block);
    branch_wait_status(
        builder,
        status,
        loop_label,
        context.yield_block,
        context.deopt,
        None,
    );
}

pub(crate) fn translate_wait_timeout(
    builder: &mut FunctionBuilder<'_>,
    context: MessageLoweringContext,
    helpers: MessageHelpers,
    charge_helper: FuncRef,
    timeout_value: &Operand,
    timeout_label: Block,
    loop_label: Block,
) -> Result<(), JitError> {
    let timeout = read_operand_term(builder, context.register_file, timeout_value)?;
    let call = builder
        .ins()
        .call(helpers.receive_wait_timeout, &[context.process, timeout]);
    let status = builder.inst_results(call)[0];
    charge_reduction_or_yield(builder, charge_helper, context.process, context.yield_block);
    branch_wait_status(
        builder,
        status,
        loop_label,
        context.yield_block,
        context.deopt,
        Some(timeout_label),
    );
    Ok(())
}

pub(crate) fn translate_timeout(
    builder: &mut FunctionBuilder<'_>,
    context: MessageLoweringContext,
    helpers: MessageHelpers,
) {
    builder
        .ins()
        .call(helpers.receive_timeout, &[context.process]);
}

fn branch_to_deopt_on_status(builder: &mut FunctionBuilder<'_>, status: Value, deopt: Block) {
    let is_message = builder
        .ins()
        .icmp_imm(IntCC::Equal, status, RECEIVE_STATUS_MESSAGE);
    let is_empty = builder
        .ins()
        .icmp_imm(IntCC::Equal, status, RECEIVE_STATUS_EMPTY);
    let known = builder.ins().bor(is_message, is_empty);
    let unknown = builder.ins().icmp_imm(IntCC::Equal, known, 0);
    let continuation = builder.create_block();
    builder.ins().brif(unknown, deopt, &[], continuation, &[]);
    builder.switch_to_block(continuation);
}

fn branch_wait_status(
    builder: &mut FunctionBuilder<'_>,
    status: Value,
    loop_label: Block,
    yield_block: Block,
    deopt: Block,
    timeout_label: Option<Block>,
) {
    let is_new_message = builder
        .ins()
        .icmp_imm(IntCC::Equal, status, WAIT_STATUS_NEW_MESSAGE);
    let check_timeout = builder.create_block();
    builder
        .ins()
        .brif(is_new_message, loop_label, &[], check_timeout, &[]);
    builder.switch_to_block(check_timeout);

    if let Some(timeout_label) = timeout_label {
        let is_timeout = builder
            .ins()
            .icmp_imm(IntCC::Equal, status, WAIT_STATUS_TIMEOUT);
        let check_waiting = builder.create_block();
        builder
            .ins()
            .brif(is_timeout, timeout_label, &[], check_waiting, &[]);
        builder.switch_to_block(check_waiting);
    }

    let is_waiting = builder
        .ins()
        .icmp_imm(IntCC::Equal, status, WAIT_STATUS_WAITING);
    builder.ins().brif(is_waiting, yield_block, &[], deopt, &[]);
}

fn charge_reduction_or_yield(
    builder: &mut FunctionBuilder<'_>,
    charge_helper: FuncRef,
    process: Value,
    yield_block: Block,
) {
    let exhausted = builder.ins().call(charge_helper, &[process]);
    let exhausted = builder.inst_results(exhausted)[0];
    let should_yield = builder.ins().icmp_imm(IntCC::NotEqual, exhausted, 0);
    let continuation = builder.create_block();
    builder
        .ins()
        .brif(should_yield, yield_block, &[], continuation, &[]);
    builder.switch_to_block(continuation);
}
