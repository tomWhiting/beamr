//! I/O native stubs for `io` and `io_lib` modules.

use crate::atom::Atom;
use crate::native::ProcessContext;
use crate::term::Term;
use crate::term::binary_ref::BinaryRef;
use crate::term::boxed::{Cons, Tuple};
use crate::term::compare;

pub fn bif_io_put_chars_1(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [chars] = args else {
        return Err(badarg());
    };
    write_iodata(*chars, context)?;
    Ok(Term::atom(Atom::OK))
}

pub fn bif_io_put_chars_2(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [_device, chars] = args else {
        return Err(badarg());
    };
    write_iodata(*chars, context)?;
    Ok(Term::atom(Atom::OK))
}

pub fn bif_io_format_3(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [_device, format, arguments] = args else {
        return Err(badarg());
    };
    let bytes = format_bytes(*format, *arguments, context)?;
    context.io_sink().write(&bytes);
    Ok(Term::atom(Atom::OK))
}

pub fn bif_io_format_2(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [format, arguments] = args else {
        return Err(badarg());
    };
    let target = context.group_leader()?;
    let bytes = format_bytes(*format, *arguments, context)?;
    let chars = context.alloc_binary(&bytes)?;
    send_put_chars(target, chars, context)
}

pub fn bif_io_get_line_1(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [prompt] = args else {
        return Err(badarg());
    };
    let target = context.group_leader()?;
    let prompt_bytes = iodata_bytes(*prompt)?;
    let prompt_bin = context.alloc_binary(&prompt_bytes)?;
    let request = io_request_tuple(context, "get_line", prompt_bin)?;
    send_io_request_and_wait(target, request, context)
}

pub fn bif_io_setopts_2(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [_device, _options] = args else {
        return Err(badarg());
    };
    Ok(Term::atom(Atom::OK))
}

pub fn bif_io_lib_format_2(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [format, arguments] = args else {
        return Err(badarg());
    };
    let bytes = format_bytes(*format, *arguments, context)?;
    context.alloc_binary(&bytes)
}

fn format_bytes(format: Term, arguments: Term, context: &ProcessContext) -> Result<Vec<u8>, Term> {
    let format = iodata_bytes(format)?;
    let arguments = list_terms(arguments)?;
    let mut out = Vec::with_capacity(format.len());
    let mut arg_index = 0usize;
    let mut index = 0usize;
    while index < format.len() {
        if format[index] != b'~' {
            out.push(format[index]);
            index += 1;
            continue;
        }
        index += 1;
        let Some(&directive) = format.get(index) else {
            return Err(badarg());
        };
        match directive {
            b's' => {
                let arg = next_arg(&arguments, &mut arg_index)?;
                out.extend_from_slice(&binary_bytes(arg)?);
            }
            b'p' | b'w' => {
                let arg = next_arg(&arguments, &mut arg_index)?;
                out.extend_from_slice(render_term(arg, context).as_bytes());
            }
            b'n' => out.push(b'\n'),
            b'~' => out.push(b'~'),
            _ => return Err(badarg()),
        }
        index += 1;
    }
    if arg_index != arguments.len() {
        return Err(badarg());
    }
    Ok(out)
}

fn next_arg(arguments: &[Term], index: &mut usize) -> Result<Term, Term> {
    let term = arguments.get(*index).copied().ok_or_else(badarg)?;
    *index += 1;
    Ok(term)
}

fn write_iodata(term: Term, context: &ProcessContext) -> Result<(), Term> {
    let bytes = iodata_bytes(term)?;
    context.io_sink().write(&bytes);
    Ok(())
}

fn iodata_bytes(term: Term) -> Result<Vec<u8>, Term> {
    let mut bytes = Vec::new();
    collect_iodata(term, &mut bytes)?;
    Ok(bytes)
}

fn collect_iodata(term: Term, out: &mut Vec<u8>) -> Result<(), Term> {
    if term.is_nil() {
        return Ok(());
    }
    if let Some(binary) = BinaryRef::new(term) {
        out.extend_from_slice(binary.as_bytes());
        return Ok(());
    }
    if let Some(byte) = term
        .as_small_int()
        .and_then(|value| u8::try_from(value).ok())
    {
        out.push(byte);
        return Ok(());
    }
    let cons = Cons::new(term).ok_or_else(badarg)?;
    collect_iodata(cons.head(), out)?;
    collect_iodata(cons.tail(), out)
}

fn list_terms(term: Term) -> Result<Vec<Term>, Term> {
    let mut terms = Vec::new();
    let mut current = term;
    loop {
        if current.is_nil() {
            return Ok(terms);
        }
        let cons = Cons::new(current).ok_or_else(badarg)?;
        terms.push(cons.head());
        current = cons.tail();
    }
}

fn render_term(term: Term, context: &ProcessContext) -> String {
    if let Some(integer) = term.as_small_int() {
        return integer.to_string();
    }
    if let Some(atom) = term.as_atom() {
        return context
            .atom_table()
            .and_then(|table| table.resolve(atom))
            .map(str::to_owned)
            .unwrap_or_else(|| format!("Atom({atom:?})"));
    }
    if term.is_nil() {
        return "[]".to_owned();
    }
    if let Some(pid) = term.as_pid() {
        return format!("<0.{pid}.0>");
    }
    if let Some(binary) = BinaryRef::new(term) {
        return match std::str::from_utf8(binary.as_bytes()) {
            Ok(text) => format!("<<\"{text}\">>"),
            Err(_) => format!("<<{} bytes>>", binary.len()),
        };
    }
    if let Some(tuple) = Tuple::new(term) {
        let mut elements = Vec::with_capacity(tuple.arity());
        for index in 0..tuple.arity() {
            if let Some(element) = tuple.get(index) {
                elements.push(render_term(element, context));
            }
        }
        return format!("{{{}}}", elements.join(", "));
    }
    format!("{term:?}")
}

fn binary_bytes(term: Term) -> Result<Vec<u8>, Term> {
    BinaryRef::new(term)
        .map(|binary| binary.as_bytes().to_vec())
        .ok_or_else(badarg)
}

// ── Group-leader protocol helpers ──────────────────────────────────────────

fn send_put_chars(target: Term, chars: Term, context: &mut ProcessContext) -> Result<Term, Term> {
    let request = io_request_tuple(context, "put_chars", chars)?;
    send_io_request_and_wait(target, request, context)
}

fn io_request_tuple(
    context: &mut ProcessContext,
    request_atom: &str,
    data: Term,
) -> Result<Term, Term> {
    let (request_tag, unicode) = {
        let table = context.atom_table().ok_or_else(badarg)?;
        (table.intern(request_atom), table.intern("unicode"))
    };
    context.alloc_tuple(&[Term::atom(request_tag), Term::atom(unicode), data])
}

fn send_io_request_and_wait(
    target: Term,
    request: Term,
    context: &mut ProcessContext,
) -> Result<Term, Term> {
    let target_pid = target.as_pid().ok_or_else(badarg)?;
    let caller_pid = context.pid().ok_or_else(badarg)?;
    let io_request_atom = context
        .atom_table()
        .ok_or_else(badarg)?
        .intern("io_request");
    context.ensure_heap_space(2 + 5)?;
    let reply_ref = context.alloc_reference_prereserved(reply_ref_id(caller_pid))?;
    if let Some(result) = take_io_reply(reply_ref, context)? {
        return Ok(result);
    }
    let message = context.alloc_tuple_prereserved(&[
        Term::atom(io_request_atom),
        Term::pid(caller_pid),
        reply_ref,
        request,
    ])?;
    let sent = {
        let facility = context.io_message_facility().ok_or_else(badarg)?;
        facility.send_message(caller_pid, target_pid, message)
    };
    if !sent {
        return error_tuple(context, Atom::NOPROC);
    }
    take_io_reply(reply_ref, context)?.map_or_else(
        || {
            context.request_suspend(None);
            Ok(Term::atom(Atom::OK))
        },
        Ok,
    )
}

fn take_io_reply(reply_ref: Term, context: &mut ProcessContext) -> Result<Option<Term>, Term> {
    let io_reply = context.atom_table().ok_or_else(badarg)?.intern("io_reply");
    let Some(select) = context.select_facility() else {
        return Err(badarg());
    };
    for index in 0..select.message_count() {
        let Some(message) = select.peek_message(index) else {
            continue;
        };
        let Some(tuple) = Tuple::new(message) else {
            continue;
        };
        if tuple.arity() != 3 || tuple.get(0) != Some(Term::atom(io_reply)) {
            continue;
        }
        let Some(reply_as) = tuple.get(1) else {
            continue;
        };
        if !compare::exact_eq(reply_as, reply_ref) {
            continue;
        }
        let result = tuple.get(2).ok_or_else(badarg)?;
        select.remove_message(index);
        return Ok(Some(result));
    }
    Ok(None)
}

fn reply_ref_id(pid: u64) -> u64 {
    pid
}

fn error_tuple(context: &mut ProcessContext, reason: Atom) -> Result<Term, Term> {
    context.alloc_tuple(&[Term::atom(Atom::ERROR), Term::atom(reason)])
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}
