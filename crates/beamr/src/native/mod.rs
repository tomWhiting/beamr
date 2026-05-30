//! Native function interface — how Gleam reaches into Rust.
//!
//! A registry where Rust functions are registered under MFA names.
//! When the interpreter hits a call to a registered native, it invokes
//! the Rust function directly — same process, no IPC, no serialisation.
//! BIFs (built-in, ship with the VM) and NIFs (registered by the host)
//! use the same mechanism but have different ownership (per D6).
pub mod bifs;
pub mod gate3_bifs;
mod context;
pub mod gleam_ffi;
pub mod links;
pub mod process_bifs;
pub mod registry;
pub mod select;
pub mod selector_ffi;
pub mod spawn;
pub mod stdlib_stubs;
pub mod supervision;

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::error::Error;
use std::fmt;

use crate::atom::Atom;
use crate::term::Term;

pub use context::{ProcessContext, SuspendRequest, TrampolineRequest};
pub use links::LinkFacility;
pub use registry::RegistryFacility;
pub use select::SelectFacility;
pub use spawn::SpawnFacility;
pub use supervision::SupervisionFacility;

/// Registry key for a native module/function/arity tuple.
pub type NativeKey = (Atom, Atom, u8);

/// Function pointer type used by BIFs and NIFs.
pub type NativeFn = fn(&[Term], &mut ProcessContext) -> Result<Term, Term>;

/// A registered native function and dispatch metadata.
#[derive(Copy, Clone, Debug)]
pub struct NativeEntry {
    /// Function implementing the native call.
    pub function: NativeFn,
    /// Whether the function should eventually run on the dirty scheduler pool.
    pub is_dirty: bool,
}

/// Errors returned while registering native functions.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum NativeRegistrationError {
    /// A native function already exists for the given module/function/arity.
    DuplicateMfa {
        /// Module atom.
        module: Atom,
        /// Function atom.
        function: Atom,
        /// Function arity.
        arity: u8,
    },
}

impl fmt::Display for NativeRegistrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateMfa {
                module,
                function,
                arity,
            } => write!(
                formatter,
                "native function already registered for {module:?}:{function:?}/{arity}"
            ),
        }
    }
}

impl Error for NativeRegistrationError {}

/// Trait used by import resolution to query built-in functions.
pub trait BifRegistry {
    /// Look up a BIF by module/function/arity.
    fn lookup(&self, module: Atom, function: Atom, arity: u8) -> Option<NativeEntry>;
}

pub use crate::loader::{UnresolvedImport, UnresolvedImportReport};

#[derive(Clone, Debug, Default)]
struct NativeRegistry {
    entries: HashMap<NativeKey, NativeEntry>,
}

impl NativeRegistry {
    fn register(
        &mut self,
        module: Atom,
        function: Atom,
        arity: u8,
        native_function: NativeFn,
        is_dirty: bool,
    ) -> Result<(), NativeRegistrationError> {
        match self.entries.entry((module, function, arity)) {
            Entry::Vacant(entry) => {
                entry.insert(NativeEntry {
                    function: native_function,
                    is_dirty,
                });
                Ok(())
            }
            Entry::Occupied(_) => Err(NativeRegistrationError::DuplicateMfa {
                module,
                function,
                arity,
            }),
        }
    }

    fn lookup(&self, module: Atom, function: Atom, arity: u8) -> Option<NativeEntry> {
        self.entries.get(&(module, function, arity)).copied()
    }
}

/// Built-in function registry populated by the VM before module loading.
#[derive(Clone, Debug, Default)]
pub struct BifRegistryImpl {
    registry: NativeRegistry,
}

impl BifRegistryImpl {
    /// Creates an empty BIF registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a normal built-in function.
    pub fn register(
        &mut self,
        module: Atom,
        function: Atom,
        arity: u8,
        native_function: NativeFn,
    ) -> Result<(), NativeRegistrationError> {
        self.registry
            .register(module, function, arity, native_function, false)
    }

    /// Registers a built-in function that should use dirty scheduling later.
    pub fn register_dirty(
        &mut self,
        module: Atom,
        function: Atom,
        arity: u8,
        native_function: NativeFn,
    ) -> Result<(), NativeRegistrationError> {
        self.registry
            .register(module, function, arity, native_function, true)
    }

    /// Looks up a built-in function by module/function/arity.
    #[must_use]
    pub fn lookup(&self, module: Atom, function: Atom, arity: u8) -> Option<NativeEntry> {
        self.registry.lookup(module, function, arity)
    }

    /// Returns imports that remain unresolved after checking registered BIFs.
    #[must_use]
    pub fn coverage(&self, report: &UnresolvedImportReport) -> Vec<UnresolvedImport> {
        report
            .imports()
            .into_iter()
            .filter(|import| {
                self.lookup(import.module, import.function, import.arity)
                    .is_none()
            })
            .collect()
    }
}

impl BifRegistry for BifRegistryImpl {
    fn lookup(&self, module: Atom, function: Atom, arity: u8) -> Option<NativeEntry> {
        self.lookup(module, function, arity)
    }
}

/// Host-provided native implemented function registry.
#[derive(Clone, Debug, Default)]
pub struct NifRegistry {
    registry: NativeRegistry,
}

impl NifRegistry {
    /// Creates an empty NIF registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a normal host native function.
    pub fn register(
        &mut self,
        module: Atom,
        function: Atom,
        arity: u8,
        native_function: NativeFn,
    ) -> Result<(), NativeRegistrationError> {
        self.registry
            .register(module, function, arity, native_function, false)
    }

    /// Registers a host native function that should use dirty scheduling later.
    pub fn register_dirty(
        &mut self,
        module: Atom,
        function: Atom,
        arity: u8,
        native_function: NativeFn,
    ) -> Result<(), NativeRegistrationError> {
        self.registry
            .register(module, function, arity, native_function, true)
    }

    /// Looks up a host native function by module/function/arity.
    #[must_use]
    pub fn lookup(&self, module: Atom, function: Atom, arity: u8) -> Option<NativeEntry> {
        self.registry.lookup(module, function, arity)
    }
}

/// Looks up a native function using import-resolution precedence: BIFs first,
/// then host-registered NIFs.
///
/// Keeping this precedence in one helper prevents a host NIF from shadowing a
/// built-in when the loader/interpreter wires native resolution through both
/// registries.
#[must_use]
pub fn lookup_native(
    bif_registry: &impl BifRegistry,
    nif_registry: &NifRegistry,
    module: Atom,
    function: Atom,
    arity: u8,
) -> Option<NativeEntry> {
    bif_registry
        .lookup(module, function, arity)
        .or_else(|| nif_registry.lookup(module, function, arity))
}

#[cfg(test)]
mod tests {
    use super::{
        BifRegistryImpl, NativeRegistrationError, NifRegistry, ProcessContext, UnresolvedImport,
        UnresolvedImportReport, lookup_native,
    };
    use crate::atom::AtomTable;
    use crate::term::Term;

    fn forty_two(_args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
        Ok(Term::small_int(42))
    }

    fn thirteen(_args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
        Ok(Term::small_int(13))
    }

    fn exit_badarith(_args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
        Err(Term::atom(crate::atom::Atom::BADARITH))
    }

    #[test]
    fn bif_registry_registers_and_looks_up_entries() {
        let atom_table = AtomTable::new();
        let erlang = atom_table.intern("erlang");
        let plus = atom_table.intern("+");
        let unknown = atom_table.intern("unknown");
        let mut registry = BifRegistryImpl::new();

        assert!(registry.register(erlang, plus, 2, forty_two).is_ok());

        let entry = registry.lookup(erlang, plus, 2).expect("registered BIF");
        assert_eq!(entry.function as usize, forty_two as usize);
        assert!(!entry.is_dirty);
        assert!(registry.lookup(erlang, unknown, 0).is_none());
    }

    #[test]
    fn bif_registry_rejects_duplicate_mfas() {
        let atom_table = AtomTable::new();
        let erlang = atom_table.intern("erlang");
        let plus = atom_table.intern("+");
        let mut registry = BifRegistryImpl::new();

        assert!(registry.register(erlang, plus, 2, forty_two).is_ok());

        assert_eq!(
            registry.register(erlang, plus, 2, thirteen),
            Err(NativeRegistrationError::DuplicateMfa {
                module: erlang,
                function: plus,
                arity: 2,
            })
        );
    }

    #[test]
    fn nif_registry_is_separate_from_bif_registry() {
        let atom_table = AtomTable::new();
        let erlang = atom_table.intern("erlang");
        let plus = atom_table.intern("+");
        let mut bif_registry = BifRegistryImpl::new();
        let mut nif_registry = NifRegistry::new();

        assert!(bif_registry.register(erlang, plus, 2, forty_two).is_ok());
        assert!(nif_registry.register(erlang, plus, 2, thirteen).is_ok());

        let bif_entry = bif_registry
            .lookup(erlang, plus, 2)
            .expect("registered BIF");
        let nif_entry = nif_registry
            .lookup(erlang, plus, 2)
            .expect("registered NIF");
        assert_eq!(bif_entry.function as usize, forty_two as usize);
        assert_eq!(nif_entry.function as usize, thirteen as usize);
    }

    #[test]
    fn native_lookup_checks_bifs_before_nifs() {
        let atom_table = AtomTable::new();
        let erlang = atom_table.intern("erlang");
        let plus = atom_table.intern("+");
        let host_only = atom_table.intern("host_only");
        let mut bif_registry = BifRegistryImpl::new();
        let mut nif_registry = NifRegistry::new();

        bif_registry
            .register(erlang, plus, 2, forty_two)
            .expect("register plus BIF");
        nif_registry
            .register(erlang, plus, 2, thirteen)
            .expect("register plus NIF");
        nif_registry
            .register(erlang, host_only, 0, thirteen)
            .expect("register host-only NIF");

        let shadowed = lookup_native(&bif_registry, &nif_registry, erlang, plus, 2)
            .expect("BIF should win over colliding NIF");
        assert_eq!(shadowed.function as usize, forty_two as usize);

        let host_entry = lookup_native(&bif_registry, &nif_registry, erlang, host_only, 0)
            .expect("host-only NIF should resolve after BIF miss");
        assert_eq!(host_entry.function as usize, thirteen as usize);
    }

    #[test]
    fn dirty_registration_sets_entry_flag() {
        let atom_table = AtomTable::new();
        let erlang = atom_table.intern("erlang");
        let plus = atom_table.intern("+");
        let display = atom_table.intern("display");
        let mut registry = BifRegistryImpl::new();

        assert!(registry.register(erlang, plus, 2, forty_two).is_ok());
        assert!(
            registry
                .register_dirty(erlang, display, 1, thirteen)
                .is_ok()
        );

        assert!(!registry.lookup(erlang, plus, 2).expect("plus").is_dirty);
        assert!(
            registry
                .lookup(erlang, display, 1)
                .expect("display")
                .is_dirty
        );
    }

    #[test]
    fn process_context_allocates_immediate_terms_without_exposing_process() {
        let mut context = ProcessContext::new();
        assert_eq!(
            context.allocate_term(Term::small_int(42)),
            Term::small_int(42)
        );
        assert_eq!(forty_two(&[], &mut context), Ok(Term::small_int(42)));
        assert_eq!(
            exit_badarith(&[], &mut context),
            Err(Term::atom(crate::atom::Atom::BADARITH))
        );
    }

    #[test]
    fn coverage_returns_only_imports_not_registered_as_bifs() {
        let atom_table = AtomTable::new();
        let erlang = atom_table.intern("erlang");
        let plus = atom_table.intern("+");
        let unknown = atom_table.intern("unknown");
        let mut registry = BifRegistryImpl::new();
        registry
            .register(erlang, plus, 2, forty_two)
            .expect("register plus");
        let report = UnresolvedImportReport::new(vec![
            UnresolvedImport::new(erlang, plus, 2),
            UnresolvedImport::new(erlang, unknown, 0),
        ]);

        assert_eq!(
            registry.coverage(&report),
            vec![UnresolvedImport::new(erlang, unknown, 0)]
        );
    }

    #[test]
    fn coverage_is_empty_when_all_imports_are_registered() {
        let atom_table = AtomTable::new();
        let erlang = atom_table.intern("erlang");
        let plus = atom_table.intern("+");
        let mut registry = BifRegistryImpl::new();
        registry
            .register(erlang, plus, 2, forty_two)
            .expect("register plus");
        let report = UnresolvedImportReport::new(vec![UnresolvedImport::new(erlang, plus, 2)]);

        assert!(registry.coverage(&report).is_empty());
    }
}
