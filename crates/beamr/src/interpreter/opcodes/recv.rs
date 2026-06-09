//! Receive marker opcode handlers.
//!
//! OTP 24+ emits these opcodes to avoid rescanning messages that arrived
//! before a newly-created reference. beamr currently models the optimization
//! with the existing single mailbox save pointer rather than independent
//! marker state per process.

use crate::error::ExecError;
use crate::interpreter::InstructionOutcome;
use crate::loader::decode::compact::Operand;
use crate::module::Module;
use crate::process::Process;
use crate::term::Term;

use super::core;

pub fn recv_marker_reserve(
    process: &mut Process,
    dest: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    let marker = process.mailbox_mut().reserve_save_marker();
    let marker = i64::try_from(marker).map_err(|_| ExecError::Badarg)?;
    let marker = Term::try_small_int(marker).ok_or(ExecError::Badarg)?;
    core::write_term(process, dest, marker)?;
    Ok(InstructionOutcome::Continue)
}

pub fn recv_marker_bind(
    process: &mut Process,
    module: &Module,
    marker: &Operand,
    label: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    let marker_position = marker_position(process, module, marker)?;
    if let Ok(marker_key) = core::read_term(process, module, label) {
        process
            .mailbox_mut()
            .bind_recv_marker(marker_key, marker_position);
    } else {
        let _ = core::operand_label(label)?;
    }
    Ok(InstructionOutcome::Continue)
}

pub fn recv_marker_clear(
    process: &mut Process,
    module: &Module,
    marker: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    let marker_key = core::read_term(process, module, marker)?;
    process.mailbox_mut().clear_recv_marker(marker_key);
    process.mailbox_mut().reset_save_pointer();
    Ok(InstructionOutcome::Continue)
}

pub fn recv_marker_use(
    process: &mut Process,
    module: &Module,
    marker: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    let value = core::read_term(process, module, marker)?;
    if !process.mailbox_mut().use_recv_marker(value) {
        let position = value
            .as_small_int()
            .and_then(|value| usize::try_from(value).ok())
            .ok_or(ExecError::Badarg)?;
        process.mailbox_mut().set_save_pointer(position);
    }
    Ok(InstructionOutcome::Continue)
}

fn marker_position(
    process: &Process,
    module: &Module,
    marker: &Operand,
) -> Result<usize, ExecError> {
    let value = core::read_term(process, module, marker)?;
    let value = value.as_small_int().ok_or(ExecError::Badarg)?;
    usize::try_from(value).map_err(|_| ExecError::Badarg)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::atom::Atom;
    use crate::error::ExecError;
    use crate::interpreter::opcodes::dispatch;
    use crate::loader::Instruction;
    use crate::loader::decode::Operand;
    use crate::module::{Module, ModuleOrigin};
    use crate::process::Process;
    use crate::term::Term;

    fn module() -> Module {
        Module {
            name: Atom::OK,
            generation: 0,
            origin: ModuleOrigin::Preloaded,
            exports: HashMap::new(),
            label_index: [(7, 0)].into_iter().collect(),
            code: vec![Instruction::Label { label: 7 }],
            literals: Vec::new(),
            constant_pool: Default::default(),
            resolved_imports: Vec::new(),
            lambdas: Vec::new(),
            string_table: Vec::new(),
            function_table: Vec::new(),
            line_table: Vec::new(),
            line_info: Vec::new(),
        }
    }

    #[test]
    fn recv_marker_handlers_dispatch_and_control_save_pointer() {
        let module = module();
        let mut process = Process::new(1, 128);
        process
            .mailbox_mut()
            .push_owned_for_test(Term::small_int(10));
        process
            .mailbox_mut()
            .push_owned_for_test(Term::small_int(20));

        assert_eq!(
            dispatch(
                &mut process,
                &module,
                &Instruction::RecvMarkerReserve {
                    dest: Operand::X(0)
                },
                1,
                None,
            ),
            Ok(crate::interpreter::InstructionOutcome::Continue)
        );
        assert_eq!(process.x_reg(0).as_small_int(), Some(2));

        process
            .mailbox_mut()
            .push_owned_for_test(Term::small_int(30));
        assert_eq!(
            dispatch(
                &mut process,
                &module,
                &Instruction::RecvMarkerBind {
                    marker: Operand::X(0),
                    label: Operand::Label(7),
                },
                1,
                None,
            ),
            Ok(crate::interpreter::InstructionOutcome::Continue)
        );
        assert_eq!(
            dispatch(
                &mut process,
                &module,
                &Instruction::RecvMarkerUse {
                    marker: Operand::X(0)
                },
                1,
                None,
            ),
            Ok(crate::interpreter::InstructionOutcome::Continue)
        );
        assert_eq!(process.mailbox().save_pointer_marker(), 2);
        assert_eq!(
            process.mailbox_mut().current_message(),
            Some(Term::small_int(30))
        );

        assert_eq!(
            dispatch(
                &mut process,
                &module,
                &Instruction::RecvMarkerClear {
                    marker: Operand::X(0)
                },
                1,
                None,
            ),
            Ok(crate::interpreter::InstructionOutcome::Continue)
        );
        assert_eq!(process.mailbox().save_pointer_marker(), 0);
    }

    #[test]
    fn recv_marker_use_rejects_non_integer_marker() {
        let module = module();
        let mut process = Process::new(1, 128);
        process.set_x_reg(0, Term::atom(Atom::OK));

        assert_eq!(
            dispatch(
                &mut process,
                &module,
                &Instruction::RecvMarkerUse {
                    marker: Operand::X(0)
                },
                1,
                None,
            ),
            Err(ExecError::Badarg)
        );
    }
}
