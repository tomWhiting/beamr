//! The interpreter — the execution loop and heartbeat of fairness.
//!
//! Fetch, decode, execute, decrement reduction counter. When the
//! counter hits zero, save state and yield. Implements the subset
//! of BEAM opcodes that Gleam actually emits (per D5).
pub mod opcodes;
pub mod pattern;

use std::sync::{Arc, Mutex};

use crate::atom::AtomTable;
use crate::distribution::{NetKernel, Node};
use crate::error::ExecError;
use crate::io::{IoFacility, IoSink};
use crate::module::{Module, ModuleRegistry};
use crate::native::code_management_bifs::CodeManagementFacility;
use crate::native::ets_bifs::EtsFacility;
use crate::native::group_leader::GroupLeaderFacility;
use crate::native::io_message::IoMessageFacility;
use crate::native::links::LinkFacility;
use crate::native::process_info_bifs::ProcessInfoFacility;
use crate::native::spawn::SpawnFacility;
use crate::native::supervision::SupervisionFacility;
use crate::native::system_info_bifs::SystemInfoFacility;
use crate::native::{FileIoFacility, NativeEntry, TcpIoFacility};
use crate::process::{CodePosition, ExitReason, Process};
use crate::scheduler::dirty::DirtySchedulerKind;
use crate::term::Term;
use crate::timer::TimerWheel;

/// Bundle of native services injected by the scheduler into BIF execution.
pub struct NativeServices {
    /// Atom table used for BEAM term ordering and atom conversion BIFs.
    pub atom_table: Option<Arc<AtomTable>>,
    /// Local node identity for node-aware BIFs.
    pub local_node: Option<Node>,
    /// Net-kernel facade for distribution connection BIFs.
    pub net_kernel: Option<Arc<NetKernel>>,
    /// Timer wheel for asynchronous timer BIFs.
    pub timers: Option<Arc<Mutex<TimerWheel>>>,
    /// Spawn facility for process creation BIFs.
    pub spawn_facility: Option<Arc<dyn SpawnFacility>>,
    /// Link facility for link management BIFs.
    pub link_facility: Option<Arc<dyn LinkFacility>>,
    /// Group-leader facility for process metadata BIFs.
    pub group_leader_facility: Option<Arc<dyn GroupLeaderFacility>>,
    /// Supervision facility for monitor/demonitor/exit BIFs.
    pub supervision_facility: Option<Arc<dyn SupervisionFacility>>,
    /// Process information facility for process_info/1,2 BIFs.
    pub process_info_facility: Option<Arc<dyn ProcessInfoFacility>>,
    /// Output sink for `io` module BIFs.
    pub io_sink: Option<Arc<dyn IoSink>>,
    /// Code management facility for hot-loading BIFs.
    pub code_management_facility: Option<Arc<dyn CodeManagementFacility>>,
    /// System-info facility for VM introspection BIFs.
    pub system_info_facility: Option<Arc<dyn SystemInfoFacility>>,
    /// ETS facility for shared table storage BIFs.
    pub ets_facility: Option<Arc<dyn EtsFacility>>,
    /// Async I/O facility for process-side ring submissions.
    pub io_facility: Option<Arc<dyn IoFacility>>,
    /// IO message facility for group-leader protocol BIFs.
    pub io_message_facility: Option<Arc<dyn IoMessageFacility>>,
    /// Completion-ring backed facility for file BIFs.
    pub file_io_facility: Option<Arc<dyn FileIoFacility>>,
    /// Active-mode TCP read-loop facility for socket option BIFs.
    pub tcp_io_facility: Option<Arc<dyn TcpIoFacility>>,
}

/// Result of running a process until it yields, waits, exits, or faults.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecutionResult {
    /// Reduction budget exhausted; scheduler should reset and requeue.
    Yielded,
    /// Process blocked waiting for a receive-family opcode.
    Waiting,
    /// Process terminated with an exit reason.
    Exited(ExitReason),
    /// Process submitted a dirty native call and must wait for its result.
    DirtyCall {
        /// Registered native entry to execute on the dirty pool.
        entry: NativeEntry,
        /// Arguments copied from x registers at the call boundary.
        args: Vec<Term>,
        /// Dirty scheduler pool that must execute the call.
        kind: DirtySchedulerKind,
    },
}

/// Control-flow outcome from one atomically completed instruction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstructionOutcome {
    /// Continue at the sequential next instruction.
    Continue,
    /// Jump to a non-sequential code position.
    Jump(CodePosition),
    /// Run a native continuation using the current value in x(0).
    NativeContinuation,
    /// Yield after preserving the next code position in the process.
    Yield,
    /// Block waiting for a message.
    Waiting,
    /// Exit the process.
    Exit(ExitReason),
    /// The loader on_load instruction completed successfully.
    OnLoadComplete,
    /// A dirty native call was reached; yield to the scheduler for migration.
    DirtyCall {
        /// Registered native entry to execute on the dirty pool.
        entry: NativeEntry,
        /// Arguments copied from x registers at the call boundary.
        args: Vec<Term>,
        /// Dirty scheduler pool that must execute the call.
        kind: DirtySchedulerKind,
    },
}

/// Execute `process` against `module` until a scheduler boundary or exit.
pub fn run(process: &mut Process, module: &Module) -> Result<ExecutionResult, ExecError> {
    let empty = NativeServices {
        atom_table: None,
        local_node: None,
        net_kernel: None,
        timers: None,
        spawn_facility: None,
        link_facility: None,
        group_leader_facility: None,
        supervision_facility: None,
        process_info_facility: None,
        io_sink: None,
        code_management_facility: None,
        system_info_facility: None,
        ets_facility: None,
        io_facility: None,
        io_message_facility: None,
        file_io_facility: None,
        tcp_io_facility: None,
    };
    run_loop(process, module, None, &empty)
}

/// Execute `process` with a registry so dynamic calls can cross module boundaries.
pub fn run_with_registry(
    process: &mut Process,
    initial_module: &Module,
    registry: &ModuleRegistry,
) -> Result<ExecutionResult, ExecError> {
    let empty = NativeServices {
        atom_table: None,
        local_node: None,
        net_kernel: None,
        timers: None,
        spawn_facility: None,
        link_facility: None,
        group_leader_facility: None,
        supervision_facility: None,
        process_info_facility: None,
        io_sink: None,
        code_management_facility: None,
        system_info_facility: None,
        ets_facility: None,
        io_facility: None,
        io_message_facility: None,
        file_io_facility: None,
        tcp_io_facility: None,
    };
    run_loop(process, initial_module, Some(registry), &empty)
}

/// Execute `process` with timer services available to asynchronous timer BIFs.
pub fn run_with_timer_services(
    process: &mut Process,
    initial_module: &Module,
    timers: Arc<Mutex<TimerWheel>>,
) -> Result<ExecutionResult, ExecError> {
    let services = NativeServices {
        atom_table: None,
        local_node: None,
        net_kernel: None,
        timers: Some(timers),
        spawn_facility: None,
        link_facility: None,
        group_leader_facility: None,
        supervision_facility: None,
        process_info_facility: None,
        io_sink: None,
        code_management_facility: None,
        system_info_facility: None,
        ets_facility: None,
        io_facility: None,
        io_message_facility: None,
        file_io_facility: None,
        tcp_io_facility: None,
    };
    run_loop(process, initial_module, None, &services)
}

/// Execute `process` with all native services and a module registry.
/// Used by the scheduler for full BIF support.
pub fn run_with_native_services(
    process: &mut Process,
    initial_module: &Module,
    registry: &ModuleRegistry,
    services: &NativeServices,
) -> Result<ExecutionResult, ExecError> {
    run_loop(process, initial_module, Some(registry), services)
}

fn current_module_for_position(
    process: &mut Process,
    position: CodePosition,
    initial_module: &Module,
    registry: Option<&ModuleRegistry>,
) -> Result<Arc<Module>, ExecError> {
    if let Some(current) = process.current_module()
        && current.name == position.module
    {
        return Ok(Arc::clone(current));
    }

    let module = registry
        .and_then(|registry| registry.lookup(position.module))
        .or_else(|| {
            (initial_module.name == position.module).then(|| Arc::new(initial_module.clone()))
        })
        .ok_or(ExecError::InvalidOperand("code position module"))?;
    process.set_current_module(Arc::clone(&module));
    Ok(module)
}

fn run_loop(
    process: &mut Process,
    initial_module: &Module,
    registry: Option<&ModuleRegistry>,
    services: &NativeServices,
) -> Result<ExecutionResult, ExecError> {
    if process.code_position().is_none() {
        process.set_code_position(Some(CodePosition {
            module: initial_module.name,
            instruction_pointer: 0,
        }));
    }

    loop {
        let position = process
            .code_position()
            .ok_or(ExecError::InvalidOperand("code position"))?;
        let module_arc = current_module_for_position(process, position, initial_module, registry)?;
        let module = module_arc.as_ref();
        let instruction = module
            .code
            .get(position.instruction_pointer)
            .ok_or(ExecError::InvalidOperand("instruction pointer"))?;
        let next_ip = position
            .instruction_pointer
            .checked_add(1)
            .ok_or(ExecError::InvalidOperand("instruction pointer"))?;

        match opcodes::dispatch_with_services(
            process,
            module,
            instruction,
            next_ip,
            services,
            registry,
        )? {
            InstructionOutcome::Continue => process.set_code_position(Some(CodePosition {
                module: module.name,
                instruction_pointer: next_ip,
            })),
            InstructionOutcome::NativeContinuation => {}
            InstructionOutcome::Jump(target) => process.set_code_position(Some(target)),
            InstructionOutcome::Yield => return Ok(ExecutionResult::Yielded),
            InstructionOutcome::Waiting => return Ok(ExecutionResult::Waiting),
            InstructionOutcome::Exit(reason) => {
                process.set_code_position(None);
                process.clear_current_module();
                return Ok(ExecutionResult::Exited(reason));
            }
            InstructionOutcome::OnLoadComplete => {
                process.set_code_position(None);
                process.clear_current_module();
                return Ok(ExecutionResult::Exited(ExitReason::Normal));
            }
            InstructionOutcome::DirtyCall { entry, args, kind } => {
                return Ok(ExecutionResult::DirtyCall { entry, args, kind });
            }
        }
    }
}

#[cfg(test)]
mod tests;
