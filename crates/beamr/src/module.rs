//! Module registry — dual-version.
//!
//! Stores loaded modules by atom name. Supports lookup by name,
//! function lookup by MFA (module:function/arity), and handles
//! duplicate module loads (the new version becomes current while the
//! previous current remains available as the old version until purged).
//! Returns an explicit undef error for missing exports.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use dashmap::DashMap;
use dashmap::mapref::entry::Entry;

use crate::atom::Atom;
use crate::error::ExecError;
use crate::loader::{Instruction, LambdaEntry, LineInfo, Literal};
use crate::native::NativeEntry;

/// Callable target produced by import resolution.
#[derive(Copy, Clone, Debug)]
pub enum ResolvedImportTarget {
    /// A function exported by another loaded BEAM module.
    Code {
        /// Target module atom.
        module: Atom,
        /// Label exported by the target module.
        label: u32,
    },
    /// A Rust native function registered as a BIF.
    Native(NativeEntry),
    /// A BEAM function whose module was not loaded when this module was loaded.
    Deferred {
        /// Target module atom.
        module: Atom,
        /// Target function atom.
        function: Atom,
        /// Target arity.
        arity: u8,
    },
    /// An import whose module was loaded but did not export the requested MFA.
    ///
    /// Keeping a placeholder preserves BEAM import-table indexing so later
    /// imports remain reachable even when an earlier import is unresolved.
    Unresolved {
        /// Target module atom.
        module: Atom,
        /// Target function atom.
        function: Atom,
        /// Target arity.
        arity: u8,
    },
}

/// One import table entry and the callable target it resolved to.
#[derive(Copy, Clone, Debug)]
pub struct ResolvedImport {
    /// Imported module atom.
    pub module: Atom,
    /// Imported function atom.
    pub function: Atom,
    /// Imported arity.
    pub arity: u8,
    /// Resolved callable target.
    pub target: ResolvedImportTarget,
}

/// Immutable loaded module data shared by the registry and processes.
#[derive(Clone, Debug)]
pub struct Module {
    /// Module atom name.
    pub name: Atom,
    /// Monotonically increasing generation assigned by the registry.
    pub generation: u64,
    /// Exported functions keyed by function atom and arity, mapping to code labels.
    pub exports: HashMap<(Atom, u8), u32>,
    /// O(1) index from code label numbers to instruction indices.
    pub label_index: HashMap<u32, usize>,
    /// Decoded BEAM instructions.
    pub code: Vec<Instruction>,
    /// Decoded literal table.
    pub literals: Vec<Literal>,
    /// Import table entries that resolved to callable targets.
    pub resolved_imports: Vec<ResolvedImport>,
    /// Decoded lambda table entries.
    pub lambdas: Vec<LambdaEntry>,
    /// Decoded string table bytes.
    pub string_table: Vec<u8>,
    /// Decoded line information.
    pub line_info: Vec<LineInfo>,
}

impl Module {
    /// Returns the registry-assigned module generation.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Finds a lambda by its stable hot-code identifier.
    #[must_use]
    pub fn find_lambda_by_id(&self, unique_id: u64) -> Option<&LambdaEntry> {
        self.lambdas
            .iter()
            .find(|lambda| lambda.unique_id == unique_id)
    }

    /// Resolves a code label to its instruction index.
    pub fn label_ip(&self, label: u32) -> Result<usize, ExecError> {
        self.label_index
            .get(&label)
            .copied()
            .ok_or(ExecError::InvalidLabel { label })
    }

    /// Resolves an exported function to its instruction index.
    pub fn export_ip(&self, function: Atom, arity: u8) -> Result<usize, ExecError> {
        let label = self
            .exports
            .get(&(function, arity))
            .copied()
            .ok_or(ExecError::Undef {
                module: self.name,
                function,
                arity,
            })?;

        self.label_ip(label)
    }
}

/// Code pointer returned by function lookup.
#[derive(Clone, Debug)]
pub struct CodePointer {
    /// Loaded module containing the target code.
    pub module: Arc<Module>,
    /// Code label for the exported function.
    pub label: u32,
    /// Generation of the loaded module containing the target code.
    pub generation: u64,
}

impl PartialEq for CodePointer {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.module, &other.module)
            && self.label == other.label
            && self.generation == other.generation
    }
}

impl Eq for CodePointer {}

/// Current and retained old versions for one loaded module name.
#[derive(Clone, Debug)]
pub struct ModuleVersions {
    /// Current module version used by compatibility lookups.
    pub current: Arc<Module>,
    /// Previous current module version, retained until safe purge.
    pub old: Option<Arc<Module>>,
}

/// Error returned when purging retained old module versions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PurgeError {
    /// The old version is still referenced outside the registry.
    StillReferenced { module: Atom, ref_count: usize },
    /// The module has no retained old version.
    NoOldVersion { module: Atom },
}

impl fmt::Display for PurgeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StillReferenced { module, ref_count } => write!(
                formatter,
                "old module version {:?} is still referenced ({ref_count} references)",
                module
            ),
            Self::NoOldVersion { module } => {
                write!(formatter, "module {:?} has no old version to purge", module)
            }
        }
    }
}

impl std::error::Error for PurgeError {}

/// Thread-safe dual-version module registry.
#[derive(Debug, Default)]
pub struct ModuleRegistry {
    modules: DashMap<Atom, ModuleVersions>,
}

impl ModuleRegistry {
    /// Creates an empty module registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts a module, promoting any current version to old.
    pub fn insert(&self, module: Module) -> Arc<Module> {
        self.insert_version(module)
    }

    /// Inserts an already shared module, promoting any current version to old.
    ///
    /// The registry assigns generations at insertion time, so this method clones
    /// the module data into a newly shared current version instead of storing the
    /// caller-provided `Arc` by pointer identity.
    pub fn insert_arc(&self, module: Arc<Module>) -> Arc<Module> {
        self.insert_version((*module).clone())
    }

    fn insert_version(&self, mut module: Module) -> Arc<Module> {
        let name = module.name;

        match self.modules.entry(name) {
            Entry::Occupied(mut entry) => {
                let previous_current = Arc::clone(&entry.get().current);
                module.generation = previous_current.generation().saturating_add(1);
                let module = Arc::new(module);
                *entry.get_mut() = ModuleVersions {
                    current: Arc::clone(&module),
                    old: Some(previous_current),
                };
                module
            }
            Entry::Vacant(entry) => {
                module.generation = 1;
                let module = Arc::new(module);
                entry.insert(ModuleVersions {
                    current: Arc::clone(&module),
                    old: None,
                });
                module
            }
        }
    }

    /// Looks up the current module version by name.
    #[must_use]
    pub fn lookup(&self, name: Atom) -> Option<Arc<Module>> {
        self.modules
            .get(&name)
            .map(|entry| Arc::clone(&entry.value().current))
    }

    /// Looks up the retained old module version by name.
    #[must_use]
    pub fn lookup_old(&self, name: Atom) -> Option<Arc<Module>> {
        self.modules
            .get(&name)
            .and_then(|entry| entry.value().old.as_ref().map(Arc::clone))
    }

    /// Returns the number of retained versions for a module name.
    #[must_use]
    pub fn module_version_count(&self, name: Atom) -> usize {
        self.modules
            .get(&name)
            .map_or(0, |entry| 1 + usize::from(entry.value().old.is_some()))
    }

    /// Purges an old module version when only the registry still references it.
    ///
    /// Callers must serialize purge requests through the single code-server
    /// thread. This method keeps the strong-count check and removal under one
    /// DashMap entry lock.
    pub fn purge_old(&self, name: Atom) -> Result<(), PurgeError> {
        let mut entry = self
            .modules
            .get_mut(&name)
            .ok_or(PurgeError::NoOldVersion { module: name })?;
        let old = entry
            .old
            .as_ref()
            .ok_or(PurgeError::NoOldVersion { module: name })?;
        let ref_count = Arc::strong_count(old);
        if ref_count != 1 {
            return Err(PurgeError::StillReferenced {
                module: name,
                ref_count,
            });
        }

        entry.old = None;
        Ok(())
    }

    /// Looks up an exported function by module/function/arity.
    pub fn lookup_mfa(
        &self,
        module: Atom,
        function: Atom,
        arity: u8,
    ) -> Result<CodePointer, ExecError> {
        let loaded = self.lookup(module).ok_or(ExecError::Undef {
            module,
            function,
            arity,
        })?;
        let label = loaded
            .exports
            .get(&(function, arity))
            .copied()
            .ok_or(ExecError::Undef {
                module,
                function,
                arity,
            })?;

        Ok(CodePointer {
            generation: loaded.generation(),
            module: loaded,
            label,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{Module, ModuleRegistry, PurgeError};
    use crate::atom::AtomTable;
    use crate::error::ExecError;
    use crate::loader::LambdaEntry;

    fn label_index(code: &[crate::loader::Instruction]) -> HashMap<u32, usize> {
        code.iter()
            .enumerate()
            .filter_map(|(ip, instruction)| match instruction {
                crate::loader::Instruction::Label { label } => Some((*label, ip)),
                _ => None,
            })
            .collect()
    }

    fn empty_module(name: crate::atom::Atom) -> Module {
        Module {
            name,
            generation: 0,
            exports: HashMap::new(),
            label_index: HashMap::new(),
            code: Vec::new(),
            literals: Vec::new(),
            resolved_imports: Vec::new(),
            lambdas: Vec::new(),
            string_table: Vec::new(),
            line_info: Vec::new(),
        }
    }

    #[test]
    fn registry_stores_and_replaces_modules_by_name() {
        let atoms = AtomTable::new();
        let module_name = atoms.intern("sample");
        let registry = ModuleRegistry::new();

        let first = registry.insert(empty_module(module_name));
        let mut replacement = empty_module(module_name);
        replacement.code.push(crate::loader::Instruction::Return);
        let second = registry.insert(replacement);

        assert!(std::sync::Arc::ptr_eq(
            &registry.lookup(module_name).expect("module loaded"),
            &second
        ));
        assert!(!std::sync::Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn registry_retains_only_current_and_previous_old_versions() {
        let atoms = AtomTable::new();
        let module_name = atoms.intern("sample");
        let registry = ModuleRegistry::new();

        let v1 = registry.insert(empty_module(module_name));
        assert_eq!(registry.module_version_count(module_name), 1);
        assert!(registry.lookup_old(module_name).is_none());
        assert!(std::sync::Arc::ptr_eq(
            &registry.lookup(module_name).expect("v1 current"),
            &v1
        ));

        let mut second = empty_module(module_name);
        second.code.push(crate::loader::Instruction::Return);
        let v2 = registry.insert(second);
        assert_eq!(registry.module_version_count(module_name), 2);
        assert!(std::sync::Arc::ptr_eq(
            &registry.lookup(module_name).expect("v2 current"),
            &v2
        ));
        assert!(std::sync::Arc::ptr_eq(
            &registry.lookup_old(module_name).expect("v1 old"),
            &v1
        ));

        let mut third = empty_module(module_name);
        third.code.push(crate::loader::Instruction::Return);
        third.code.push(crate::loader::Instruction::Return);
        let v3 = registry.insert(third);
        assert_eq!(registry.module_version_count(module_name), 2);
        assert!(std::sync::Arc::ptr_eq(
            &registry.lookup(module_name).expect("v3 current"),
            &v3
        ));
        assert!(std::sync::Arc::ptr_eq(
            &registry.lookup_old(module_name).expect("v2 old"),
            &v2
        ));
        assert_eq!(v1.generation(), 1);
        assert_eq!(v2.generation(), 2);
        assert_eq!(v3.generation(), 3);
    }

    #[test]
    fn generations_are_tracked_per_module_name() {
        let atoms = AtomTable::new();
        let first_name = atoms.intern("first");
        let second_name = atoms.intern("second");
        let registry = ModuleRegistry::new();

        let first_v1 = registry.insert(empty_module(first_name));
        let second_v1 = registry.insert(empty_module(second_name));
        let first_v2 = registry.insert(empty_module(first_name));

        assert_eq!(first_v1.generation(), 1);
        assert_eq!(second_v1.generation(), 1);
        assert_eq!(first_v2.generation(), 2);
    }

    #[test]
    fn purge_old_requires_no_external_references() {
        let atoms = AtomTable::new();
        let module_name = atoms.intern("sample");
        let registry = ModuleRegistry::new();
        registry.insert(empty_module(module_name));
        registry.insert(empty_module(module_name));

        let old_ref = registry.lookup_old(module_name).expect("old version");
        assert!(matches!(
            registry.purge_old(module_name),
            Err(PurgeError::StillReferenced { module, ref_count })
                if module == module_name && ref_count >= 2
        ));
        drop(old_ref);

        assert_eq!(registry.purge_old(module_name), Ok(()));
        assert!(registry.lookup_old(module_name).is_none());
        assert_eq!(registry.module_version_count(module_name), 1);
        assert_eq!(
            registry.purge_old(module_name),
            Err(PurgeError::NoOldVersion {
                module: module_name
            })
        );
    }

    #[test]
    fn registry_lookup_unloaded_module_returns_none() {
        let atoms = AtomTable::new();
        let registry = ModuleRegistry::new();

        assert!(registry.lookup(atoms.intern("missing")).is_none());
    }

    #[test]
    fn lookup_mfa_returns_code_pointer_for_export() {
        let atoms = AtomTable::new();
        let module_name = atoms.intern("sample");
        let function = atoms.intern("main");
        let registry = ModuleRegistry::new();
        let mut module = empty_module(module_name);
        module.exports.insert((function, 0), 7);
        registry.insert(module);

        let pointer = registry
            .lookup_mfa(module_name, function, 0)
            .expect("exported function");

        assert_eq!(pointer.label, 7);
        assert_eq!(pointer.module.name, module_name);
        assert_eq!(pointer.generation, 1);
    }

    #[test]
    fn module_resolves_labels_from_index() {
        let atoms = AtomTable::new();
        let mut module = empty_module(atoms.intern("sample"));
        module.code = vec![
            crate::loader::Instruction::Return,
            crate::loader::Instruction::Label { label: 10 },
            crate::loader::Instruction::Return,
            crate::loader::Instruction::Label { label: 20 },
        ];
        module.label_index = label_index(&module.code);

        assert_eq!(module.label_ip(10), Ok(1));
        assert_eq!(module.label_ip(20), Ok(3));
        assert_eq!(
            module.label_ip(30),
            Err(ExecError::InvalidLabel { label: 30 })
        );
    }

    #[test]
    fn module_resolves_exports_to_instruction_indices() {
        let atoms = AtomTable::new();
        let function = atoms.intern("main");
        let mut module = empty_module(atoms.intern("sample"));
        module.code = vec![
            crate::loader::Instruction::Return,
            crate::loader::Instruction::Label { label: 10 },
            crate::loader::Instruction::Return,
            crate::loader::Instruction::Label { label: 20 },
        ];
        module.label_index = label_index(&module.code);
        module.exports.insert((function, 0), 20);

        assert_eq!(module.export_ip(function, 0), Ok(3));
    }

    #[test]
    fn module_reports_undef_for_missing_export() {
        let atoms = AtomTable::new();
        let module_name = atoms.intern("sample");
        let function = atoms.intern("missing");
        let module = empty_module(module_name);

        assert!(matches!(
            module.export_ip(function, 0),
            Err(ExecError::Undef {
                module,
                function: undef_function,
                arity: 0,
            }) if module == module_name && undef_function == function
        ));
    }

    #[test]
    fn module_reports_invalid_label_for_export_missing_from_index() {
        let atoms = AtomTable::new();
        let function = atoms.intern("main");
        let mut module = empty_module(atoms.intern("sample"));
        module.exports.insert((function, 0), 99);

        assert_eq!(
            module.export_ip(function, 0),
            Err(ExecError::InvalidLabel { label: 99 })
        );
    }

    #[test]
    fn find_lambda_by_id_resolves_reordered_lambda_tables() {
        let atoms = AtomTable::new();
        let module_name = atoms.intern("sample");
        let first_fun = atoms.intern("first@anon");
        let second_fun = atoms.intern("second@anon");
        let first_id = crate::loader::lambda_unique_id(&atoms, module_name, first_fun, 1, 2)
            .expect("first id");
        let second_id = crate::loader::lambda_unique_id(&atoms, module_name, second_fun, 0, 0)
            .expect("second id");

        let mut v1 = empty_module(module_name);
        v1.lambdas = vec![
            LambdaEntry {
                function: first_fun,
                arity: 1,
                label: 10,
                num_free: 2,
                unique_id: first_id,
            },
            LambdaEntry {
                function: second_fun,
                arity: 0,
                label: 20,
                num_free: 0,
                unique_id: second_id,
            },
        ];
        let mut v2 = empty_module(module_name);
        v2.lambdas = vec![
            LambdaEntry {
                function: second_fun,
                arity: 0,
                label: 200,
                num_free: 0,
                unique_id: second_id,
            },
            LambdaEntry {
                function: first_fun,
                arity: 1,
                label: 100,
                num_free: 2,
                unique_id: first_id,
            },
        ];

        assert_eq!(
            v1.find_lambda_by_id(first_id).map(|lambda| lambda.label),
            Some(10)
        );
        assert_eq!(
            v2.find_lambda_by_id(first_id).map(|lambda| lambda.label),
            Some(100)
        );
        assert_eq!(
            v1.find_lambda_by_id(second_id).map(|lambda| lambda.label),
            Some(20)
        );
        assert_eq!(
            v2.find_lambda_by_id(second_id).map(|lambda| lambda.label),
            Some(200)
        );
    }

    #[test]
    fn lookup_mfa_reports_undef_for_missing_targets() {
        let atoms = AtomTable::new();
        let module_name = atoms.intern("sample");
        let function = atoms.intern("main");
        let other = atoms.intern("other");
        let registry = ModuleRegistry::new();
        registry.insert(empty_module(module_name));

        assert!(matches!(
            registry.lookup_mfa(other, function, 0),
            Err(ExecError::Undef {
                module,
                function: undef_function,
                arity: 0,
            }) if module == other && undef_function == function
        ));
        assert!(matches!(
            registry.lookup_mfa(module_name, function, 0),
            Err(ExecError::Undef {
                module,
                function: undef_function,
                arity: 0,
            }) if module == module_name && undef_function == function
        ));
        assert!(matches!(
            registry.lookup_mfa(module_name, function, 1),
            Err(ExecError::Undef {
                module,
                function: undef_function,
                arity: 1,
            }) if module == module_name && undef_function == function
        ));
    }
}
