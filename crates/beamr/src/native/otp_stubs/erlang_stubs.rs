//! Erlang stdlib stub BIFs for OTP import resolution.
//!
//! These stubs satisfy imports from gleam_erlang_ffi.beam for Erlang
//! modules that are not part of the beamr VM core:
//! - `application` — application controller
//! - `os` — operating system interface
//! - `io` — I/O primitives
//! - `code` — code path management
//! - `net_kernel` — distribution
//! - `string` — string processing

use crate::atom::{Atom, AtomTable};
use crate::native::ProcessContext;
use crate::term::Term;

/// Atom for "unix".
static UNIX_ATOM: std::sync::OnceLock<Atom> = std::sync::OnceLock::new();
/// Atom for "darwin".
static DARWIN_ATOM: std::sync::OnceLock<Atom> = std::sync::OnceLock::new();

pub fn init_erlang_atoms(atom_table: &AtomTable) {
    let _ = UNIX_ATOM.set(atom_table.intern("unix"));
    let _ = DARWIN_ATOM.set(atom_table.intern("darwin"));
}

/// `application:ensure_all_started/1` -- stub returning `{ok, []}`.
pub fn bif_ensure_all_started(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [_app] = args else {
        return Err(badarg());
    };
    context.alloc_tuple(&[Term::atom(Atom::OK), Term::NIL])
}

/// `os:getenv/0` -- returns an empty list.
pub fn bif_os_getenv_0(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    if !args.is_empty() {
        return Err(badarg());
    }
    Ok(Term::NIL)
}

/// `os:getenv/1` -- returns `false` (env var not found).
pub fn bif_os_getenv_1(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [_name] = args else {
        return Err(badarg());
    };
    Ok(Term::atom(Atom::FALSE))
}

/// `os:putenv/2` -- no-op stub, returns `true`.
pub fn bif_os_putenv(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [_key, _value] = args else {
        return Err(badarg());
    };
    Ok(Term::atom(Atom::TRUE))
}

/// `os:unsetenv/1` -- no-op stub, returns `true`.
pub fn bif_os_unsetenv(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [_name] = args else {
        return Err(badarg());
    };
    Ok(Term::atom(Atom::TRUE))
}

/// `os:type/0` -- returns `{unix, darwin}`.
pub fn bif_os_type(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    if !args.is_empty() {
        return Err(badarg());
    }
    let unix = UNIX_ATOM.get().copied().unwrap_or_else(|| Atom::new(9998));
    let darwin = DARWIN_ATOM
        .get()
        .copied()
        .unwrap_or_else(|| Atom::new(9997));
    context.alloc_tuple(&[Term::atom(unix), Term::atom(darwin)])
}

/// `code:priv_dir/1` -- stub returning `{error, bad_name}`.
pub fn bif_code_priv_dir(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [_app] = args else {
        return Err(badarg());
    };
    context.alloc_tuple(&[Term::atom(Atom::ERROR), Term::atom(Atom::BADARG)])
}

/// `net_kernel:connect_node/1` -- manually connect to a named node.
pub fn bif_connect_node(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [node] = args else {
        return Err(badarg());
    };
    let node = node.as_atom().ok_or_else(badarg)?;
    let connected = context
        .net_kernel()
        .is_some_and(|net_kernel| net_kernel.connect_node(node));
    Ok(Term::atom(if connected { Atom::TRUE } else { Atom::FALSE }))
}

/// `string:split/2` -- stub returning `[Input]`.
pub fn bif_string_split(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [input, _pattern] = args else {
        return Err(badarg());
    };
    context.alloc_cons(*input, Term::NIL)
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}
