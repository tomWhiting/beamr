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
use crate::module::ModuleOrigin;
use crate::native::ProcessContext;
use crate::term::Term;
use crate::term::binary_ref::BinaryRef;
use crate::term::boxed::Cons;

/// Atom for "unix".
static UNIX_ATOM: std::sync::OnceLock<Atom> = std::sync::OnceLock::new();
/// Atom for "darwin".
static DARWIN_ATOM: std::sync::OnceLock<Atom> = std::sync::OnceLock::new();
/// Atom for "linux".
static LINUX_ATOM: std::sync::OnceLock<Atom> = std::sync::OnceLock::new();
/// Atom for "win32".
static WIN32_ATOM: std::sync::OnceLock<Atom> = std::sync::OnceLock::new();
/// Atom for "nt".
static NT_ATOM: std::sync::OnceLock<Atom> = std::sync::OnceLock::new();
/// Atom for "unknown".
static UNKNOWN_ATOM: std::sync::OnceLock<Atom> = std::sync::OnceLock::new();
/// Atom for "bad_name".
static BAD_NAME_ATOM: std::sync::OnceLock<Atom> = std::sync::OnceLock::new();
/// Atom for "not_loaded".
static NOT_LOADED_ATOM: std::sync::OnceLock<Atom> = std::sync::OnceLock::new();

pub fn init_erlang_atoms(atom_table: &AtomTable) {
    let _ = UNIX_ATOM.set(atom_table.intern("unix"));
    let _ = DARWIN_ATOM.set(atom_table.intern("darwin"));
    let _ = LINUX_ATOM.set(atom_table.intern("linux"));
    let _ = WIN32_ATOM.set(atom_table.intern("win32"));
    let _ = NT_ATOM.set(atom_table.intern("nt"));
    let _ = UNKNOWN_ATOM.set(atom_table.intern("unknown"));
    let _ = BAD_NAME_ATOM.set(atom_table.intern("bad_name"));
    let _ = NOT_LOADED_ATOM.set(atom_table.intern("not_loaded"));
}

/// `application:ensure_all_started/1` -- best-effort loaded-module validation.
pub fn bif_ensure_all_started(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [app] = args else {
        return Err(badarg());
    };
    let app = app.as_atom().ok_or_else(badarg)?;
    if module_loaded(context, app) {
        let apps = context.alloc_list(&[Term::atom(app)])?;
        context.alloc_tuple(&[Term::atom(Atom::OK), apps])
    } else {
        ensure_all_started_not_loaded(context, app)
    }
}

/// `os:getenv/0` -- returns all host environment variables as `KEY=VALUE` binaries.
pub fn bif_os_getenv_0(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    if !args.is_empty() {
        return Err(badarg());
    }
    let mut variables = Vec::new();
    for (key, value) in std::env::vars() {
        variables.push(context.alloc_binary(format!("{key}={value}").as_bytes())?);
    }
    context.alloc_list(&variables)
}

/// `os:getenv/1` -- reads a host environment variable.
pub fn bif_os_getenv_1(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [name] = args else {
        return Err(badarg());
    };
    let key = beam_string_to_string(*name)?;
    validate_env_key(&key)?;
    match std::env::var(&key) {
        Ok(value) => context.alloc_binary(value.as_bytes()),
        Err(std::env::VarError::NotPresent) => Ok(Term::atom(Atom::FALSE)),
        Err(std::env::VarError::NotUnicode(_)) => Err(badarg()),
    }
}

/// `os:putenv/2` -- sets a host environment variable.
pub fn bif_os_putenv(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [key, value] = args else {
        return Err(badarg());
    };
    let key = beam_string_to_string(*key)?;
    let value = beam_string_to_string(*value)?;
    validate_env_key(&key)?;
    validate_env_value(&value)?;
    // SAFETY: Environment mutation is process-global and therefore unsafe in
    // Rust 2024 because concurrent environment access in other threads can be
    // undefined on some platforms. BEAM's os:putenv/2 is specified as a global
    // host-environment mutation; keys/values are validated above to avoid the
    // std panics for empty keys, '=' in keys, and NUL bytes.
    unsafe {
        std::env::set_var(&key, &value);
    }
    Ok(Term::atom(Atom::TRUE))
}

/// `os:unsetenv/1` -- removes a host environment variable.
pub fn bif_os_unsetenv(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [name] = args else {
        return Err(badarg());
    };
    let key = beam_string_to_string(*name)?;
    validate_env_key(&key)?;
    // SAFETY: This performs the process-global mutation required by
    // os:unsetenv/1. The key is validated above so std will not panic for an
    // empty key, '=' in the key, or embedded NUL bytes.
    unsafe {
        std::env::remove_var(&key);
    }
    Ok(Term::atom(Atom::TRUE))
}

/// `os:type/0` -- returns the host OS family/name tuple.
pub fn bif_os_type(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    if !args.is_empty() {
        return Err(badarg());
    }
    let (family, name) = if cfg!(target_os = "macos") {
        (
            cached_atom(context, &UNIX_ATOM, "unix")?,
            cached_atom(context, &DARWIN_ATOM, "darwin")?,
        )
    } else if cfg!(target_os = "linux") {
        (
            cached_atom(context, &UNIX_ATOM, "unix")?,
            cached_atom(context, &LINUX_ATOM, "linux")?,
        )
    } else if cfg!(target_os = "windows") {
        (
            cached_atom(context, &WIN32_ATOM, "win32")?,
            cached_atom(context, &NT_ATOM, "nt")?,
        )
    } else {
        (
            cached_atom(context, &UNIX_ATOM, "unix")?,
            cached_atom(context, &UNKNOWN_ATOM, "unknown")?,
        )
    };
    context.alloc_tuple(&[Term::atom(family), Term::atom(name)])
}

/// `code:priv_dir/1` -- resolves a loaded application's sibling `priv/` directory.
pub fn bif_code_priv_dir(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [app] = args else {
        return Err(badarg());
    };
    let app = app.as_atom().ok_or_else(badarg)?;
    let Some(ModuleOrigin::Filesystem(path)) = module_origin(context, app) else {
        return error_bad_name(context);
    };
    let Some(priv_dir) = priv_dir_from_origin(&path) else {
        return error_bad_name(context);
    };
    if priv_dir.is_dir() {
        let path = priv_dir.to_string_lossy();
        context.alloc_binary(path.as_bytes())
    } else {
        error_bad_name(context)
    }
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

/// `string:split/2` -- splits at the first occurrence of a binary/list pattern.
pub fn bif_string_split(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [input, pattern] = args else {
        return Err(badarg());
    };
    let input_string = decode_beam_string(*input)?;
    let pattern = decode_beam_string(*pattern)?.into_bytes();
    if pattern.is_empty() {
        return Err(badarg());
    }
    let input_bytes = input_string.bytes();
    if let Some(index) = find_subsequence(input_bytes, &pattern) {
        let left = input_string.allocate_like_input(context, &input_bytes[..index])?;
        {
            let process = context.process_mut().ok_or_else(badarg)?;
            process.set_x_reg(0, left);
        }
        let right =
            input_string.allocate_like_input(context, &input_bytes[index + pattern.len()..])?;
        {
            let process = context.process_mut().ok_or_else(badarg)?;
            process.set_x_reg(1, right);
        }
        context.ensure_heap_space(4)?;
        let (left, right) = {
            let process = context.process_mut().ok_or_else(badarg)?;
            (process.x_reg(0), process.x_reg(1))
        };
        let tail = context.alloc_cons_prereserved(right, Term::NIL)?;
        context.alloc_cons_prereserved(left, tail)
    } else {
        {
            let process = context.process_mut().ok_or_else(badarg)?;
            process.set_x_reg(0, *input);
        }
        context.ensure_heap_space(2)?;
        let input = context.process_mut().ok_or_else(badarg)?.x_reg(0);
        context.alloc_cons_prereserved(input, Term::NIL)
    }
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}

enum BeamString {
    Binary(Vec<u8>),
    List(Vec<u8>),
}

impl BeamString {
    fn bytes(&self) -> &[u8] {
        match self {
            Self::Binary(bytes) | Self::List(bytes) => bytes,
        }
    }

    fn into_bytes(self) -> Vec<u8> {
        match self {
            Self::Binary(bytes) | Self::List(bytes) => bytes,
        }
    }

    fn allocate_like_input(
        &self,
        context: &mut ProcessContext,
        bytes: &[u8],
    ) -> Result<Term, Term> {
        match self {
            Self::Binary(_) => context.alloc_binary(bytes),
            Self::List(_) => make_byte_list(context, bytes),
        }
    }
}

fn decode_beam_string(term: Term) -> Result<BeamString, Term> {
    if let Some(binary) = BinaryRef::new(term) {
        return Ok(BeamString::Binary(binary.as_bytes().to_vec()));
    }
    proper_byte_list(term).map(BeamString::List)
}

fn beam_string_to_string(term: Term) -> Result<String, Term> {
    String::from_utf8(decode_beam_string(term)?.into_bytes()).map_err(|_| badarg())
}

fn proper_byte_list(term: Term) -> Result<Vec<u8>, Term> {
    let mut bytes = Vec::new();
    let mut current = term;
    loop {
        if current.is_nil() {
            return Ok(bytes);
        }
        let cons = Cons::new(current).ok_or_else(badarg)?;
        let byte = cons
            .head()
            .as_small_int()
            .and_then(|value| u8::try_from(value).ok())
            .ok_or_else(badarg)?;
        bytes.push(byte);
        current = cons.tail();
    }
}

fn make_byte_list(context: &mut ProcessContext, bytes: &[u8]) -> Result<Term, Term> {
    let elements: Vec<_> = bytes
        .iter()
        .copied()
        .map(|byte| Term::small_int(i64::from(byte)))
        .collect();
    context.alloc_list(&elements)
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn validate_env_key(key: &str) -> Result<(), Term> {
    if key.is_empty() || key.as_bytes().contains(&b'=') || key.as_bytes().contains(&0) {
        Err(badarg())
    } else {
        Ok(())
    }
}

fn validate_env_value(value: &str) -> Result<(), Term> {
    if value.as_bytes().contains(&0) {
        Err(badarg())
    } else {
        Ok(())
    }
}

fn cached_atom(
    context: &ProcessContext,
    cache: &std::sync::OnceLock<Atom>,
    name: &str,
) -> Result<Atom, Term> {
    if let Some(atom_table) = context.atom_table() {
        let atom = atom_table.intern(name);
        let _ = cache.set(atom);
        Ok(atom)
    } else {
        cache.get().copied().ok_or_else(badarg)
    }
}

fn module_origin(context: &ProcessContext, app: Atom) -> Option<ModuleOrigin> {
    context
        .code_management_facility()
        .and_then(|facility| facility.module_origin(app))
}

fn module_loaded(context: &ProcessContext, app: Atom) -> bool {
    module_origin(context, app).is_some()
}

fn error_bad_name(context: &mut ProcessContext) -> Result<Term, Term> {
    let bad_name = cached_atom(context, &BAD_NAME_ATOM, "bad_name")?;
    context.alloc_tuple(&[Term::atom(Atom::ERROR), Term::atom(bad_name)])
}

fn ensure_all_started_not_loaded(context: &mut ProcessContext, app: Atom) -> Result<Term, Term> {
    let not_loaded = cached_atom(context, &NOT_LOADED_ATOM, "not_loaded")?;
    let inner = context.alloc_tuple(&[Term::atom(not_loaded), Term::atom(app)])?;
    let reason = context.alloc_tuple(&[Term::atom(app), inner])?;
    context.alloc_tuple(&[Term::atom(Atom::ERROR), reason])
}

fn priv_dir_from_origin(path: &std::path::Path) -> Option<std::path::PathBuf> {
    let code_dir = if path.file_name().is_some_and(|name| name == "ebin") {
        path
    } else {
        path.parent()?
    };
    code_dir.parent().map(|app_dir| app_dir.join("priv"))
}
