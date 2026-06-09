//! OTP module stub BIFs for gleam_otp support.
//!
//! These stubs satisfy imports from gleam_otp's compiled .beam files that
//! reference modules without corresponding .beam fixtures in the test suite.
//!
//! Gleam-level stubs (gleam@dynamic, gleam@string, etc.) are in the
//! `gleam_stubs` submodule. Erlang stdlib stubs (os, io, application, etc.)
//! are in the `erlang_stubs` submodule.

mod erlang_stubs;
pub(crate) mod gleam_stubs;

use crate::atom::{Atom, AtomTable};
use crate::native::{
    BifRegistryImpl, Capability, NativeFn, NativeRegistrationError, ProcessContext,
};
use crate::term::Term;

use erlang_stubs::{
    bif_code_priv_dir, bif_connect_node, bif_ensure_all_started, bif_os_getenv_0, bif_os_getenv_1,
    bif_os_putenv, bif_os_type, bif_os_unsetenv, bif_string_split,
};
use gleam_stubs::{
    bif_dynamic_classify, bif_dynamic_int, bif_dynamic_string, bif_intensity_tracker_add_event,
    bif_intensity_tracker_new, bif_option_map, bif_option_unwrap, bif_result_map_error,
    bif_result_then, bif_string_append, bif_string_inspect,
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
    // gleam@dynamic
    (
        "gleam@dynamic",
        "classify",
        1,
        Capability::Pure,
        bif_dynamic_classify,
    ),
    ("gleam@dynamic", "int", 1, Capability::Pure, bif_dynamic_int),
    (
        "gleam@dynamic",
        "string",
        1,
        Capability::Pure,
        bif_dynamic_string,
    ),
    // gleam@string
    (
        "gleam@string",
        "inspect",
        1,
        Capability::Pure,
        bif_string_inspect,
    ),
    (
        "gleam@string",
        "append",
        2,
        Capability::Pure,
        bif_string_append,
    ),
    // gleam@option
    ("gleam@option", "map", 2, Capability::Pure, bif_option_map),
    (
        "gleam@option",
        "unwrap",
        2,
        Capability::Pure,
        bif_option_unwrap,
    ),
    // gleam@result
    (
        "gleam@result",
        "map_error",
        2,
        Capability::Pure,
        bif_result_map_error,
    ),
    ("gleam@result", "then", 2, Capability::Pure, bif_result_then),
    // gleam@otp@intensity_tracker
    (
        "gleam@otp@intensity_tracker",
        "new",
        2,
        Capability::Pure,
        bif_intensity_tracker_new,
    ),
    (
        "gleam@otp@intensity_tracker",
        "add_event",
        1,
        Capability::Pure,
        bif_intensity_tracker_add_event,
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
/// Must be called before `register_otp_stubs` so that atoms like "None",
/// "unix", and "darwin" resolve correctly at runtime.
pub fn init_otp_atoms(atom_table: &AtomTable) {
    gleam_stubs::init_gleam_atoms(atom_table);
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
