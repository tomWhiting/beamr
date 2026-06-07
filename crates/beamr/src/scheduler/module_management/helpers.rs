//! Private module-management helpers.

use std::sync::Arc;

use crate::atom::Atom;
use crate::error::LoadError;
use crate::interpreter::{self, ExecutionResult};
use crate::loader::{self, Instruction};
use crate::module::{Module, ModuleRegistry, PurgeError};
use crate::namespace::NamespaceId;
use crate::process::heap::DEFAULT_HEAP_SIZE;
use crate::process::{CodePosition, ExitReason, Process};

use super::{HotLoadResult, PurgeResult};
use crate::scheduler::execution::cleanup_exited_process;
use crate::scheduler::{
    DEFAULT_REDUCTION_BUDGET, ProcessSlot, SharedState, lock_or_recover, namespace_registry,
    supervision_integration,
};

pub(super) fn namespace_registry_for_load(
    shared: &SharedState,
    namespace: NamespaceId,
) -> Result<Arc<ModuleRegistry>, LoadError> {
    namespace_registry(shared, namespace).ok_or(LoadError::UnknownNamespace { namespace })
}

pub(super) fn hot_load_module_shared(
    shared: &Arc<SharedState>,
    bytes: &[u8],
) -> Result<HotLoadResult, LoadError> {
    hot_load_module_in_shared(shared, NamespaceId::DEFAULT, &shared.module_registry, bytes)
}

pub(super) fn hot_load_module_in_shared(
    shared: &Arc<SharedState>,
    namespace: NamespaceId,
    registry: &Arc<ModuleRegistry>,
    bytes: &[u8],
) -> Result<HotLoadResult, LoadError> {
    let (staged, _report) = loader::prepare_module_with_policy(
        bytes,
        &shared.atom_table,
        registry,
        shared.bif_registry.as_ref(),
        shared.capability_policy.as_ref(),
    )?;
    let module_name = staged.name;
    if registry.lookup_old(module_name).is_some() {
        return Err(LoadError::OldCodeStillRunning);
    }
    let had_old_version = registry.lookup(module_name).is_some();
    let on_load_ip = find_on_load_ip(&staged);
    if let Some(ip) = on_load_ip {
        let outcome = run_on_load(shared, namespace, registry, &staged, ip);
        if outcome != ExitReason::Normal {
            return Ok(HotLoadResult {
                module_name,
                generation: staged.generation,
                had_old_version,
                on_load_required: true,
                on_load_succeeded: false,
            });
        }
    }
    let committed = registry.insert(staged);
    Ok(HotLoadResult {
        module_name,
        generation: committed.generation(),
        had_old_version,
        on_load_required: on_load_ip.is_some(),
        on_load_succeeded: on_load_ip.is_some(),
    })
}

fn find_on_load_ip(module: &Module) -> Option<usize> {
    module
        .code
        .iter()
        .position(|instruction| matches!(instruction, Instruction::OnLoad))
}

fn run_on_load(
    shared: &Arc<SharedState>,
    namespace: NamespaceId,
    registry: &Arc<ModuleRegistry>,
    module: &Module,
    ip: usize,
) -> ExitReason {
    let Some(entry_ip) = ip
        .checked_add(1)
        .filter(|entry_ip| *entry_ip < module.code.len())
    else {
        return ExitReason::Error;
    };
    let mut process = Process::new(u64::MAX, DEFAULT_HEAP_SIZE);
    process.set_namespace_id(namespace);
    process.set_code_position(Some(CodePosition {
        module: module.name,
        instruction_pointer: entry_ip,
    }));
    process.set_current_module(Arc::new(module.clone()));
    loop {
        process.reset_reductions(DEFAULT_REDUCTION_BUDGET);
        let services = supervision_integration::build_native_services(shared, namespace);
        match interpreter::run_with_native_services(&mut process, module, registry, &services) {
            Ok(ExecutionResult::Exited(reason)) => return reason,
            Ok(ExecutionResult::Yielded) => continue,
            Ok(ExecutionResult::Waiting) | Err(_) => return ExitReason::Error,
        }
    }
}

pub(super) fn purge_module_shared(
    shared: &Arc<SharedState>,
    name: Atom,
) -> Result<PurgeResult, PurgeError> {
    purge_module_in_shared(shared, NamespaceId::DEFAULT, &shared.module_registry, name)
}

pub(super) fn purge_module_in_shared(
    shared: &Arc<SharedState>,
    namespace: NamespaceId,
    registry: &Arc<ModuleRegistry>,
    name: Atom,
) -> Result<PurgeResult, PurgeError> {
    if let Some(old) = registry.lookup_old(name) {
        let references = process_references_to_module_in(shared, namespace, &old);
        if references != 0 {
            return Err(PurgeError::StillReferenced {
                module: name,
                ref_count: references,
            });
        }
    }
    registry.purge_old(name)?;
    Ok(PurgeResult {
        module_name: name,
        processes_killed: 0,
    })
}

pub(super) fn force_purge_module_in_shared(
    shared: &Arc<SharedState>,
    namespace: NamespaceId,
    registry: &Arc<ModuleRegistry>,
    name: Atom,
) -> Result<PurgeResult, PurgeError> {
    let old = registry
        .lookup_old(name)
        .ok_or(PurgeError::NoOldVersion { module: name })?;
    let victims = old_code_pids_in(shared, namespace, &old);
    let processes_killed = victims.len();
    for pid in victims {
        cleanup_exited_process(shared, pid, ExitReason::Killed);
    }
    registry.force_remove_old(name)?;
    Ok(PurgeResult {
        module_name: name,
        processes_killed,
    })
}

fn process_references_to_module_in(
    shared: &SharedState,
    namespace: NamespaceId,
    module: &Arc<Module>,
) -> usize {
    old_code_pids_in(shared, namespace, module).len()
}

fn old_code_pids_in(
    shared: &SharedState,
    namespace: NamespaceId,
    module: &Arc<Module>,
) -> Vec<u64> {
    shared
        .process_bodies
        .iter()
        .filter_map(|entry| {
            let pid = *entry.key();
            process_references_old_code_in(shared, pid, namespace, module).then_some(pid)
        })
        .collect()
}

pub(super) fn process_references_old_code(
    shared: &SharedState,
    pid: u64,
    module: &Arc<Module>,
) -> bool {
    let Some(entry) = shared.process_bodies.get(&pid) else {
        return false;
    };
    let slot = lock_or_recover(&entry);
    match &*slot {
        ProcessSlot::Present(scheduled) => scheduled.0.references_module(module),
        ProcessSlot::Executing(_) | ProcessSlot::Absent => false,
    }
}

fn process_references_old_code_in(
    shared: &SharedState,
    pid: u64,
    namespace: NamespaceId,
    module: &Arc<Module>,
) -> bool {
    let Some(entry) = shared.process_bodies.get(&pid) else {
        return false;
    };
    let slot = lock_or_recover(&entry);
    match &*slot {
        ProcessSlot::Present(scheduled) => {
            scheduled.0.namespace_id() == namespace && scheduled.0.references_module(module)
        }
        ProcessSlot::Executing(_) | ProcessSlot::Absent => false,
    }
}
