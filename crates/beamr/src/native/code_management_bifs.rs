//! Code management native facilities and BIFs.
//!
//! These BIFs expose the scheduler code-server API to BEAM code without
//! duplicating hot-load or purge logic in native functions.

use crate::atom::{Atom, AtomTable};
use crate::error::LoadError;
use crate::module::{ModuleOrigin, PurgeError};
use crate::native::{
    BifRegistryImpl, Capability, NativeFn, NativeRegistrationError, ProcessContext,
};
use crate::scheduler::{HotLoadResult, PurgeResult};
use crate::term::Term;
use crate::term::binary::Binary;

/// Scheduler-backed code management operations used by hot-code BIFs.
pub trait CodeManagementFacility: Send + Sync {
    /// Load raw BEAM bytes as a new module version.
    fn load_module(&self, bytes: &[u8]) -> Result<HotLoadResult, LoadError>;

    /// Attempt to safely purge retained old code.
    fn purge_module(&self, module: Atom) -> Result<PurgeResult, PurgeError>;

    /// Remove all versions of a module from the registry.
    fn delete_module(&self, module: Atom) -> bool;

    /// Return true when retained old code exists for `module`.
    fn check_old_code(&self, module: Atom) -> bool;

    /// Return true when `pid` is running or pinned to old code for `module`.
    fn check_process_code(&self, pid: u64, module: Atom) -> bool;

    /// Return origin metadata for a current loaded module.
    fn module_origin(&self, module: Atom) -> Option<ModuleOrigin>;

    /// Return all currently loaded module names and origins.
    fn all_loaded_modules(&self) -> Vec<(Atom, ModuleOrigin)>;
}

type CodeBif = (&'static str, u8, Capability, NativeFn);

const CODE_BIFS: &[CodeBif] = &[
    ("load_module", 2, Capability::ExternalIo, load_module),
    ("purge_module", 1, Capability::ExternalIo, purge_module),
    ("delete_module", 1, Capability::ExternalIo, delete_module),
    ("check_old_code", 1, Capability::ExternalIo, check_old_code),
    (
        "check_process_code",
        2,
        Capability::ExternalIo,
        check_process_code,
    ),
];

/// Registers code-management BIFs under the `erlang` module.
pub fn register_code_management_bifs(
    registry: &BifRegistryImpl,
    atom_table: &AtomTable,
) -> Result<(), NativeRegistrationError> {
    let erlang = atom_table.intern("erlang");
    for &(function_name, arity, capability, native_function) in CODE_BIFS {
        let function = atom_table.intern(function_name);
        registry.register(erlang, function, arity, native_function, capability)?;
    }
    let code = atom_table.intern("code");
    let all_loaded_name = atom_table.intern("all_loaded");
    registry.register(code, all_loaded_name, 0, all_loaded, Capability::Pure)?;
    Ok(())
}

/// erlang:load_module/2. The first argument names the module; the second is
/// BEAM bytes.
pub fn load_module(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [name_term, bytes_term] = args else {
        return Err(badarg());
    };
    let module_name = name_term.as_atom().ok_or_else(badarg)?;
    let bytes = Binary::new(*bytes_term).ok_or_else(badarg)?.as_bytes();
    let facility = context.code_management_facility().ok_or_else(badarg)?;
    let result = facility.load_module(bytes).map_err(|_| badarg())?;
    if result.module_name != module_name {
        return Err(badarg());
    }
    context.alloc_tuple(&[Term::atom(Atom::MODULE), Term::atom(result.module_name)])
}

/// erlang:purge_module/1.
pub fn purge_module(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [module_term] = args else {
        return Err(badarg());
    };
    let module = module_term.as_atom().ok_or_else(badarg)?;
    let facility = context.code_management_facility().ok_or_else(badarg)?;
    facility.purge_module(module).map_err(|_| badarg())?;
    Ok(bool_term(true))
}

/// erlang:delete_module/1.
pub fn delete_module(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [module_term] = args else {
        return Err(badarg());
    };
    let module = module_term.as_atom().ok_or_else(badarg)?;
    let facility = context.code_management_facility().ok_or_else(badarg)?;
    Ok(bool_term(facility.delete_module(module)))
}

/// erlang:check_old_code/1.
pub fn check_old_code(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [module_term] = args else {
        return Err(badarg());
    };
    let module = module_term.as_atom().ok_or_else(badarg)?;
    let facility = context.code_management_facility().ok_or_else(badarg)?;
    Ok(bool_term(facility.check_old_code(module)))
}

/// erlang:check_process_code/2.
pub fn check_process_code(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [pid_term, module_term] = args else {
        return Err(badarg());
    };
    let pid = pid_term.as_pid().ok_or_else(badarg)?;
    let module = module_term.as_atom().ok_or_else(badarg)?;
    let facility = context.code_management_facility().ok_or_else(badarg)?;
    Ok(bool_term(facility.check_process_code(pid, module)))
}

/// code:all_loaded/0 returns currently loaded modules with their source metadata.
pub fn all_loaded(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    if !args.is_empty() {
        return Err(badarg());
    }
    let facility = context.code_management_facility().ok_or_else(badarg)?;
    let loaded = facility.all_loaded_modules();
    let atom_table = context.atom_table().ok_or_else(badarg)?;
    let loaded_terms: Vec<(Term, Term)> = loaded
        .into_iter()
        .map(|(module, origin)| {
            let source = atom_table.intern(origin.source_atom_name());
            (Term::atom(module), Term::atom(source))
        })
        .collect();

    let mut list = Term::NIL;
    for (module, source) in loaded_terms.into_iter().rev() {
        let tuple = context.alloc_tuple(&[module, source])?;
        list = context.alloc_cons(tuple, list)?;
    }
    Ok(list)
}

fn bool_term(value: bool) -> Term {
    Term::atom(if value { Atom::TRUE } else { Atom::FALSE })
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}
