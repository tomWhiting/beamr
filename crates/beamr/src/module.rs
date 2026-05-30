//! Module registry — single-version.
//!
//! Stores loaded modules by atom name. Supports lookup by name,
//! function lookup by MFA (module:function/arity), and handles
//! duplicate module loads (the new version replaces the old).
//! Returns an explicit undef error for missing exports.

use std::collections::HashMap;
use std::sync::Arc;

use dashmap::DashMap;

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
    /// Exported functions keyed by function atom and arity, mapping to code labels.
    pub exports: HashMap<(Atom, u8), u32>,
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

/// Code pointer returned by function lookup.
#[derive(Clone, Debug)]
pub struct CodePointer {
    /// Loaded module containing the target code.
    pub module: Arc<Module>,
    /// Code label for the exported function.
    pub label: u32,
}

/// Thread-safe single-version module registry.
#[derive(Debug, Default)]
pub struct ModuleRegistry {
    modules: DashMap<Atom, Arc<Module>>,
}

impl ModuleRegistry {
    /// Creates an empty module registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts a module, replacing any current module with the same name.
    pub fn insert(&self, module: Module) -> Arc<Module> {
        self.insert_arc(Arc::new(module))
    }

    /// Inserts an already shared module, replacing any current module with the same name.
    pub fn insert_arc(&self, module: Arc<Module>) -> Arc<Module> {
        self.modules.insert(module.name, Arc::clone(&module));
        module
    }

    /// Looks up the current module version by name.
    #[must_use]
    pub fn lookup(&self, name: Atom) -> Option<Arc<Module>> {
        self.modules
            .get(&name)
            .map(|entry| Arc::clone(entry.value()))
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
            module: loaded,
            label,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Module, ModuleRegistry};
    use crate::atom::AtomTable;
    use crate::error::ExecError;

    fn empty_module(name: crate::atom::Atom) -> Module {
        Module {
            name,
            exports: std::collections::HashMap::new(),
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
