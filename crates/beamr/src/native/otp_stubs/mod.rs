//! OTP module stub BIFs for gleam_otp support.
//!
//! These stubs satisfy imports from compiled .beam files that reference
//! OTP modules with no .beam form (os, io, application, supervisor, code).
//! Modules that ship as compiled bytecode in every Gleam build — gleam@*
//! and the gleam_stdlib FFI — must NOT be stubbed here: native entries
//! shadow loaded bytecode, and a stub that drifts from the real
//! implementation breaks code that the real module would serve correctly.

mod erlang_stubs;

use crate::atom::{Atom, AtomTable};
use crate::native::{
    BifRegistryImpl, Capability, NativeFn, NativeRegistrationError, ProcessContext,
};
use crate::term::Term;

use erlang_stubs::{
    bif_code_priv_dir, bif_connect_node, bif_ensure_all_started, bif_os_getenv_0, bif_os_getenv_1,
    bif_os_putenv, bif_os_type, bif_os_unsetenv, bif_string_split,
};
type OtpBif = (&'static str, &'static str, u8, Capability, NativeFn);

const OTP_STUBS: &[OtpBif] = &[
    // gleam_otp_external
    (
        "gleam_otp_external",
        "application_stopped",
        0,
        Capability::Pure,
        bif_application_stopped,
    ),
    // supervisor
    (
        "supervisor",
        "start_link",
        2,
        Capability::Pure,
        bif_supervisor_start_link,
    ),
    // application
    (
        "application",
        "ensure_all_started",
        1,
        Capability::Pure,
        bif_ensure_all_started,
    ),
    // os
    ("os", "getenv", 0, Capability::ExternalIo, bif_os_getenv_0),
    ("os", "getenv", 1, Capability::ExternalIo, bif_os_getenv_1),
    ("os", "putenv", 2, Capability::ExternalIo, bif_os_putenv),
    ("os", "unsetenv", 1, Capability::ExternalIo, bif_os_unsetenv),
    ("os", "type", 0, Capability::ExternalIo, bif_os_type),
    // code
    (
        "code",
        "priv_dir",
        1,
        Capability::ExternalIo,
        bif_code_priv_dir,
    ),
    // net_kernel
    (
        "net_kernel",
        "connect_node",
        1,
        Capability::ExternalIo,
        bif_connect_node,
    ),
    // string
    ("string", "split", 2, Capability::Pure, bif_string_split),
];

/// Registers all OTP stub BIFs under their respective module names.
pub fn register_otp_stubs(
    registry: &BifRegistryImpl,
    atom_table: &AtomTable,
) -> Result<(), NativeRegistrationError> {
    for &(module_name, function_name, arity, capability, native_function) in OTP_STUBS {
        let module = atom_table.intern(module_name);
        let function = atom_table.intern(function_name);
        registry.register(module, function, arity, native_function, capability)?;
    }
    Ok(())
}

/// Initializes sentinel atoms used by OTP stubs.
///
/// Must be called before `register_otp_stubs` so that atoms like OS
/// identifiers and OTP error reasons resolve correctly at runtime.
pub fn init_otp_atoms(atom_table: &AtomTable) {
    erlang_stubs::init_erlang_atoms(atom_table);
}

// ── gleam_otp_external ────────────────────────────────────────────────────

/// `gleam_otp_external:application_stopped/0` -- returns the atom `ok`.
pub fn bif_application_stopped(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    if !args.is_empty() {
        return Err(badarg());
    }
    Ok(Term::atom(Atom::OK))
}

// ── supervisor ────────────────────────────────────────────────────────────

/// `supervisor:start_link/2` -- stub returning `{ok, self_pid}`.
///
/// Gleam_otp manages children directly; the Erlang supervisor module is
/// only called as a compatibility shim.
pub fn bif_supervisor_start_link(
    args: &[Term],
    context: &mut ProcessContext,
) -> Result<Term, Term> {
    let [_module, _init_args] = args else {
        return Err(badarg());
    };
    let pid = context.pid().ok_or_else(badarg)?;
    let pid_term = Term::try_pid(pid).ok_or_else(badarg)?;
    context.alloc_tuple(&[Term::atom(Atom::OK), pid_term])
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}

#[cfg(test)]
mod tests;
