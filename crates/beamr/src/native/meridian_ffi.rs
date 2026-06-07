//! Meridian workflow NIFs — Rust functions callable from Gleam workflows.
//!
//! Registered under the `meridian_ffi` module atom. These are proof-of-concept
//! implementations for testing the NIF wiring end-to-end.

use crate::atom::{Atom, AtomTable};
use crate::native::{BifRegistryImpl, Capability, NativeRegistrationError, ProcessContext};
use crate::scheduler::dirty::DirtySchedulerKind;
use crate::term::Term;
use crate::term::binary::Binary;

pub fn register_meridian_ffi(
    registry: &BifRegistryImpl,
    atom_table: &AtomTable,
) -> Result<(), NativeRegistrationError> {
    let module = atom_table.intern("meridian_ffi");
    registry.register_dirty(
        module,
        atom_table.intern("read_file"),
        1,
        nif_read_file,
        DirtySchedulerKind::Io,
        Capability::ExternalIo,
    )?;
    registry.register_dirty(
        module,
        atom_table.intern("run_cmd"),
        1,
        nif_run_cmd,
        DirtySchedulerKind::Io,
        Capability::ExternalIo,
    )?;
    registry.register_dirty(
        module,
        atom_table.intern("write_file"),
        2,
        nif_write_file,
        DirtySchedulerKind::Io,
        Capability::ExternalIo,
    )?;
    registry.register(
        module,
        atom_table.intern("read_json"),
        1,
        nif_read_json,
        Capability::ExternalIo,
    )?;
    registry.register(
        module,
        atom_table.intern("commit"),
        1,
        nif_commit,
        Capability::ExternalIo,
    )?;
    registry.register(
        module,
        atom_table.intern("run_step_norn"),
        4,
        nif_run_step_norn,
        Capability::ExternalIo,
    )?;
    Ok(())
}

fn ok_binary(ctx: &mut ProcessContext, content: &[u8]) -> Result<Term, Term> {
    let binary = ctx.alloc_binary(content)?;
    ctx.alloc_tuple(&[Term::atom(Atom::OK), binary])
}

fn err_binary(ctx: &mut ProcessContext, reason: &[u8]) -> Result<Term, Term> {
    let binary = ctx.alloc_binary(reason)?;
    ctx.alloc_tuple(&[Term::atom(Atom::ERROR), binary])
}

fn ok_nil(ctx: &mut ProcessContext) -> Result<Term, Term> {
    ctx.alloc_tuple(&[Term::atom(Atom::OK), Term::NIL])
}

fn extract_string(term: Term) -> Result<String, Term> {
    let binary = Binary::new(term).ok_or(Term::atom(Atom::BADARG))?;
    String::from_utf8(binary.as_bytes().to_vec()).map_err(|_| Term::atom(Atom::BADARG))
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}

fn nif_read_file(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    let [path_term] = args else {
        return Err(badarg());
    };
    let path = extract_string(*path_term)?;
    match std::fs::read(&path) {
        Ok(content) => ok_binary(ctx, &content),
        Err(e) => Err(err_binary(ctx, e.to_string().as_bytes())?),
    }
}

fn nif_run_cmd(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    let [command_term] = args else {
        return Err(badarg());
    };
    let command = extract_string(*command_term)?;
    match std::process::Command::new("sh")
        .arg("-c")
        .arg(&command)
        .output()
    {
        Ok(output) => ok_binary(ctx, &output.stdout),
        Err(e) => Err(err_binary(ctx, e.to_string().as_bytes())?),
    }
}

fn nif_write_file(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    let [path_term, content_term] = args else {
        return Err(badarg());
    };
    let path = extract_string(*path_term)?;
    let content = extract_string(*content_term)?;
    if let Some(parent) = std::path::Path::new(&path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::write(&path, &content) {
        Ok(()) => ok_nil(ctx),
        Err(e) => Err(err_binary(ctx, e.to_string().as_bytes())?),
    }
}

fn nif_read_json(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    let [path_term] = args else {
        return Err(badarg());
    };
    let path = extract_string(*path_term)?;
    match std::fs::read_to_string(&path) {
        Ok(content) => ok_binary(ctx, content.as_bytes()),
        Err(e) => Err(err_binary(ctx, e.to_string().as_bytes())?),
    }
}

fn nif_commit(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    let [message_term] = args else {
        return Err(badarg());
    };
    let _message = extract_string(*message_term)?;
    ok_binary(ctx, b"commit stub")
}

fn nif_run_step_norn(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    let [name_term, profile_term, instruction_term, schema_term] = args else {
        return Err(badarg());
    };
    let _name = extract_string(*name_term)?;
    let _profile = extract_string(*profile_term)?;
    let _instruction = extract_string(*instruction_term)?;
    let _schema = extract_string(*schema_term)?;
    ok_binary(ctx, b"step stub")
}

#[cfg(test)]
mod tests {
    use super::register_meridian_ffi;
    use crate::atom::AtomTable;
    use crate::native::BifRegistryImpl;
    use crate::scheduler::dirty::DirtySchedulerKind;

    #[test]
    fn register_meridian_ffi_marks_blocking_io_nifs_dirty() {
        let atom_table = AtomTable::new();
        let registry = BifRegistryImpl::new();
        register_meridian_ffi(&registry, &atom_table).expect("meridian ffi registration");

        let module = atom_table.intern("meridian_ffi");
        for (function_name, arity) in [("read_file", 1), ("run_cmd", 1), ("write_file", 2)] {
            let function = atom_table.intern(function_name);
            let entry = registry
                .lookup(module, function, arity)
                .expect("blocking meridian ffi NIF");
            assert_eq!(entry.dirty_kind, Some(DirtySchedulerKind::Io));
        }
    }
}
