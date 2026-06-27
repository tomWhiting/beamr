//! Module loading, hot-code replacement, and purge support.

use std::sync::Arc;

use crate::atom::Atom;
use crate::error::LoadError;
use crate::module::{Module, ModuleOrigin, PurgeError};
use crate::namespace::NamespaceId;
use crate::native::CodeManagementFacility;

use super::spawning::drain_pending_spawns;
use super::{Scheduler, SharedState, namespace_registry};

pub use super::{HotLoadResult, PurgeResult};

impl Scheduler {
    /// Hot-load a BEAM module, running on_load before committing when required.
    pub fn hot_load_module(&self, bytes: &[u8]) -> Result<HotLoadResult, LoadError> {
        self.hot_load_module_in(NamespaceId::DEFAULT, bytes)
    }

    /// Load a module from the embedded archive by module name when present.
    pub fn load_embedded_module(
        &self,
        module_name: &str,
    ) -> Result<Option<HotLoadResult>, LoadError> {
        self.load_embedded_module_in(NamespaceId::DEFAULT, module_name)
    }

    /// Load a BEAM module into a specific namespace.
    pub fn load_module_in(
        &self,
        namespace: NamespaceId,
        bytes: &[u8],
    ) -> Result<HotLoadResult, LoadError> {
        self.hot_load_module_in(namespace, bytes)
    }

    /// Hot-load a BEAM module into a specific namespace.
    pub fn hot_load_module_in(
        &self,
        namespace: NamespaceId,
        bytes: &[u8],
    ) -> Result<HotLoadResult, LoadError> {
        let registry = namespace_registry_for_load(&self.shared, namespace)?;
        hot_load_module_in_shared(&self.shared, namespace, &registry, bytes)
    }

    /// Load a module from the embedded archive into a specific namespace by module name when present.
    pub fn load_embedded_module_in(
        &self,
        namespace: NamespaceId,
        module_name: &str,
    ) -> Result<Option<HotLoadResult>, LoadError> {
        let Some(bytes) = crate::loader::embedded_module_bytes(module_name) else {
            return Ok(None);
        };
        let registry = namespace_registry_for_load(&self.shared, namespace)?;
        hot_load_module_in_shared_with_origin(
            &self.shared,
            namespace,
            &registry,
            &bytes,
            ModuleOrigin::Embedded,
        )
        .map(Some)
    }

    /// Safely purge retained old code when no process still references it.
    pub fn purge_module(&self, name: Atom) -> Result<PurgeResult, PurgeError> {
        self.purge_module_in(NamespaceId::DEFAULT, name)
    }

    /// Kill processes pinned to old code, then purge the retained old version.
    pub fn force_purge_module(&self, name: Atom) -> Result<PurgeResult, PurgeError> {
        self.force_purge_module_in(NamespaceId::DEFAULT, name)
    }

    /// Safely purge retained old code in a specific namespace.
    pub fn purge_module_in(
        &self,
        namespace: NamespaceId,
        name: Atom,
    ) -> Result<PurgeResult, PurgeError> {
        let Some(registry) = namespace_registry(&self.shared, namespace) else {
            return Err(PurgeError::NoOldVersion { module: name });
        };
        drain_pending_spawns(&self.shared, &self.inject_queues);
        purge_module_in_shared(&self.shared, namespace, &registry, name)
    }

    /// Kill processes in a namespace pinned to old code, then purge that namespace's old version.
    pub fn force_purge_module_in(
        &self,
        namespace: NamespaceId,
        name: Atom,
    ) -> Result<PurgeResult, PurgeError> {
        let Some(registry) = namespace_registry(&self.shared, namespace) else {
            return Err(PurgeError::NoOldVersion { module: name });
        };
        drain_pending_spawns(&self.shared, &self.inject_queues);
        force_purge_module_in_shared(&self.shared, namespace, &registry, name)
    }

    /// Look up the current module version in a namespace's registry.
    pub fn lookup_module_in(&self, namespace: NamespaceId, name: Atom) -> Option<Arc<Module>> {
        namespace_registry(&self.shared, namespace)?.lookup(name)
    }

    /// Remove every version of a module from the registry.
    pub fn delete_module(&self, name: Atom) -> bool {
        self.shared.module_registry.delete_module(name)
    }

    /// Return true when an old module version is retained.
    pub fn check_old_code(&self, name: Atom) -> bool {
        self.shared.module_registry.has_old_code(name)
    }

    /// Return true when a process is currently running or pinned to old code.
    pub fn check_process_code(&self, pid: u64, name: Atom) -> bool {
        let Some(old) = self.shared.module_registry.lookup_old(name) else {
            return false;
        };
        process_references_old_code(&self.shared, pid, &old)
    }
}

pub(in crate::scheduler) struct SchedulerCodeManagementFacility {
    pub(super) shared: Arc<SharedState>,
}

impl CodeManagementFacility for SchedulerCodeManagementFacility {
    fn load_module(&self, bytes: &[u8]) -> Result<HotLoadResult, LoadError> {
        hot_load_module_shared(&self.shared, bytes)
    }

    fn purge_module(&self, module: Atom) -> Result<PurgeResult, PurgeError> {
        purge_module_shared(&self.shared, module)
    }

    fn delete_module(&self, module: Atom) -> bool {
        self.shared.module_registry.delete_module(module)
    }

    fn check_old_code(&self, module: Atom) -> bool {
        self.shared.module_registry.has_old_code(module)
    }

    fn check_process_code(&self, pid: u64, module: Atom) -> bool {
        let Some(old) = self.shared.module_registry.lookup_old(module) else {
            return false;
        };
        process_references_old_code(&self.shared, pid, &old)
    }

    fn module_origin(&self, module: Atom) -> Option<ModuleOrigin> {
        self.shared.module_registry.origin(module)
    }

    fn all_loaded_modules(&self) -> Vec<(Atom, ModuleOrigin)> {
        self.shared.module_registry.all_loaded()
    }
}

mod helpers;
use helpers::{
    force_purge_module_in_shared, hot_load_module_in_shared, hot_load_module_in_shared_with_origin,
    hot_load_module_shared, namespace_registry_for_load, process_references_old_code,
    purge_module_in_shared, purge_module_shared,
};
