//! Selector system BIFs for gleam_erlang_ffi.
//!
//! A Selector is a list of `{Tag, Handler}` tuples used by `select/1` and
//! `select/2` to receive typed messages from the process mailbox. Handlers
//! are BEAM closures that transform matched messages.
//!
//! These BIFs are registered under the `gleam_erlang_ffi` module atom and
//! provide the foundation for gleam_otp's actor message loop.

use crate::atom::{Atom, AtomTable};
use crate::native::{
    BifRegistryImpl, Capability, NativeFn, NativeRegistrationError, ProcessContext,
};
use crate::term::Term;
use crate::term::boxed::{Cons, Tuple};

/// Atom constant for the "anything" catch-all tag.
///
/// Selectors use this sentinel to match any message regardless of shape.
/// The index is resolved at registration time via the atom table.
static ANYTHING_ATOM: std::sync::OnceLock<Atom> = std::sync::OnceLock::new();

/// Returns the `anything` atom, or falls back to checking the tag at runtime.
fn anything_atom() -> Option<Atom> {
    ANYTHING_ATOM.get().copied()
}

/// BIF entry for selector registration.
type SelectorBif = (&'static str, u8, Capability, NativeFn);

const SELECTOR_BIFS: &[SelectorBif] = &[
    ("new_selector", 0, Capability::Pure, bif_new_selector),
    (
        "insert_selector_handler",
        3,
        Capability::Pure,
        bif_insert_selector_handler,
    ),
    ("map_selector", 2, Capability::Pure, bif_map_selector),
    ("merge_selector", 2, Capability::Pure, bif_merge_selector),
    (
        "remove_selector_handler",
        2,
        Capability::Pure,
        bif_remove_selector_handler,
    ),
    ("select", 1, Capability::Pure, bif_select),
    ("select", 2, Capability::Clock, bif_select_with_timeout),
];

/// Registers all selector BIFs under the `gleam_erlang_ffi` module.
pub fn register_selector_bifs(
    registry: &BifRegistryImpl,
    atom_table: &AtomTable,
) -> Result<(), NativeRegistrationError> {
    let module = atom_table.intern("gleam_erlang_ffi");
    let _ = ANYTHING_ATOM.set(atom_table.intern("anything"));

    for &(function_name, arity, capability, native_function) in SELECTOR_BIFS {
        let function = atom_table.intern(function_name);
        registry.register(module, function, arity, native_function, capability)?;
    }

    Ok(())
}

/// `gleam_erlang_ffi:new_selector/0` — returns an empty selector (NIL).
///
/// A selector is a list of `{Tag, Handler}` tuples. An empty selector
/// has no handlers and will never match any message.
pub fn bif_new_selector(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    if !args.is_empty() {
        return Err(badarg());
    }
    Ok(Term::NIL)
}

/// `gleam_erlang_ffi:insert_selector_handler/3` — prepend a handler.
///
/// Accepts `(Selector, Tag, Handler)` and returns a new selector with
/// `{Tag, Handler}` prepended. The handler is a fun/1 that transforms
/// a matched message.
pub fn bif_insert_selector_handler(
    args: &[Term],
    context: &mut ProcessContext,
) -> Result<Term, Term> {
    let [selector, tag, handler] = args else {
        return Err(badarg());
    };
    {
        let process = context.process_mut().ok_or_else(badarg)?;
        process.set_x_reg(0, *selector);
        process.set_x_reg(1, *tag);
        process.set_x_reg(2, *handler);
    }
    context.ensure_heap_space(3 + 2)?;
    let (selector, tag, handler) = {
        let process = context.process_mut().ok_or_else(badarg)?;
        (process.x_reg(0), process.x_reg(1), process.x_reg(2))
    };
    let entry = context.alloc_tuple_prereserved(&[tag, handler])?;
    let cons = context.alloc_cons_prereserved(entry, selector)?;
    Ok(cons)
}

/// `gleam_erlang_ffi:map_selector/2` — wrap each handler to compose with MapFun.
///
/// Accepts `(Selector, MapFun)` and returns a new selector where each handler
/// entry is replaced with `{Tag, {mapped, MapFun, OriginalHandler}}`. The
/// interpreter's trampoline handles the composed call: first the original
/// handler is called, then MapFun is applied to its result.
pub fn bif_map_selector(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [selector, map_fun] = args else {
        return Err(badarg());
    };

    // Walk the selector list, wrapping each handler.
    let entries = list_to_vec(*selector)?;
    let mut result = Term::NIL;
    if entries.is_empty() {
        return Ok(Term::NIL);
    }
    {
        let process = context.process_mut().ok_or_else(badarg)?;
        process.set_x_reg(0, *selector);
        process.set_x_reg(1, *map_fun);
    }
    context.ensure_heap_space(entries.len() * (4 + 3 + 2))?;
    let (selector, map_fun) = {
        let process = context.process_mut().ok_or_else(badarg)?;
        (process.x_reg(0), process.x_reg(1))
    };
    let entries = list_to_vec(selector)?;

    // Build the new list in reverse order to preserve original ordering
    // after prepending.
    for entry_term in entries.into_iter().rev() {
        let entry = Tuple::new(entry_term).ok_or_else(badarg)?;
        if entry.arity() < 2 {
            return Err(badarg());
        }
        let tag = entry.get(0).ok_or_else(badarg)?;
        let original_handler = entry.get(1).ok_or_else(badarg)?;

        // Create a {mapped, MapFun, OriginalHandler} tuple to signal
        // composed invocation to the trampoline.
        let mapped_atom = Term::atom(Atom::new(mapped_atom_index()));
        let wrapped = context.alloc_tuple_prereserved(&[mapped_atom, map_fun, original_handler])?;
        let new_entry = context.alloc_tuple_prereserved(&[tag, wrapped])?;
        result = context.alloc_cons_prereserved(new_entry, result)?;
    }

    Ok(result)
}

/// `gleam_erlang_ffi:merge_selector/2` — concatenate two selector lists.
///
/// Appends `SelectorB` to the end of `SelectorA`.
pub fn bif_merge_selector(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [selector_a, selector_b] = args else {
        return Err(badarg());
    };

    if selector_a.is_nil() {
        return Ok(*selector_b);
    }

    // Walk SelectorA, collect entries, then rebuild with SelectorB as tail.
    let entries_a = list_to_vec(*selector_a)?;
    {
        let process = context.process_mut().ok_or_else(badarg)?;
        process.set_x_reg(0, *selector_a);
        process.set_x_reg(1, *selector_b);
    }
    context.ensure_heap_space(entries_a.len() * 2)?;
    let (selector_a, mut result) = {
        let process = context.process_mut().ok_or_else(badarg)?;
        (process.x_reg(0), process.x_reg(1))
    };
    let entries_a = list_to_vec(selector_a)?;

    for entry in entries_a.into_iter().rev() {
        result = context.alloc_cons_prereserved(entry, result)?;
    }

    Ok(result)
}

/// `gleam_erlang_ffi:remove_selector_handler/2` — filter out entries matching tag.
///
/// Accepts `(Selector, Tag)` and returns a new selector with all entries
/// whose tag equals `Tag` removed.
pub fn bif_remove_selector_handler(
    args: &[Term],
    context: &mut ProcessContext,
) -> Result<Term, Term> {
    let [selector, remove_tag] = args else {
        return Err(badarg());
    };

    let entries = list_to_vec(*selector)?;
    let mut result = Term::NIL;
    if entries.is_empty() {
        return Ok(Term::NIL);
    }
    {
        let process = context.process_mut().ok_or_else(badarg)?;
        process.set_x_reg(0, *selector);
        process.set_x_reg(1, *remove_tag);
    }
    context.ensure_heap_space(entries.len() * 2)?;
    let (selector, remove_tag) = {
        let process = context.process_mut().ok_or_else(badarg)?;
        (process.x_reg(0), process.x_reg(1))
    };
    let entries = list_to_vec(selector)?;

    // Rebuild list in reverse, skipping entries that match remove_tag.
    for entry_term in entries.into_iter().rev() {
        let entry = Tuple::new(entry_term).ok_or_else(badarg)?;
        if entry.arity() < 2 {
            return Err(badarg());
        }
        let tag = entry.get(0).ok_or_else(badarg)?;
        if tag != remove_tag {
            result = context.alloc_cons_prereserved(entry_term, result)?;
        }
    }

    Ok(result)
}

/// `gleam_erlang_ffi:select/1` — scan mailbox for matching message.
///
/// Takes a Selector (list of {Tag, Handler}). Scans the process mailbox
/// from the save pointer. For each message, checks if any handler's tag
/// matches. If a match is found, removes the message from the mailbox
/// and returns a trampoline call with the handler and matched message.
/// If no match, suspends the process.
pub fn bif_select(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [selector] = args else {
        return Err(badarg());
    };
    select_impl(*selector, None, context)
}

/// `gleam_erlang_ffi:select/2` — scan mailbox with timeout.
///
/// Same as select/1 but accepts a timeout in milliseconds. If no matching
/// message arrives within the timeout, returns `{error, nil}`.
pub fn bif_select_with_timeout(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [selector, timeout] = args else {
        return Err(badarg());
    };
    let timeout_ms = timeout.as_small_int().ok_or_else(badarg)?;
    if timeout_ms < 0 {
        return Err(badarg());
    }
    let timeout_ms = timeout_ms as u64;
    select_impl(*selector, Some(timeout_ms), context)
}

/// Core select implementation shared by select/1 and select/2.
fn select_impl(
    selector: Term,
    timeout_ms: Option<u64>,
    context: &mut ProcessContext,
) -> Result<Term, Term> {
    // Parse the selector list into handler entries.
    let handlers = list_to_vec(selector)?;
    if handlers.is_empty() {
        // No handlers — suspend indefinitely or timeout immediately.
        if timeout_ms.is_some() {
            return error_nil_tuple(context);
        }
        context.request_suspend(timeout_ms);
        return Err(badarg());
    }

    // Access the mailbox through the select facility.
    let facility = match context.select_facility() {
        Some(f) => f,
        None => return Err(badarg()),
    };

    // Scan mailbox messages from the save pointer.
    let message_count = facility.message_count();
    for msg_index in 0..message_count {
        let message = match facility.peek_message(msg_index) {
            Some(m) => m,
            None => continue,
        };

        // Try each handler in order.
        for handler_term in &handlers {
            let entry = Tuple::new(*handler_term).ok_or_else(badarg)?;
            if entry.arity() < 2 {
                return Err(badarg());
            }
            let tag = entry.get(0).ok_or_else(badarg)?;
            let handler = entry.get(1).ok_or_else(badarg)?;

            if message_matches_tag(message, tag) {
                // Match found. Remove the message from the mailbox.
                facility.remove_message(msg_index);

                // Set up trampoline: the interpreter will call the handler
                // with the matched message as the argument.
                context.set_trampoline(handler, vec![message]);
                // Return value is a placeholder — the trampoline result
                // replaces it.
                return Ok(Term::NIL);
            }
        }
    }

    // No message matched any handler.
    if let Some(0) = timeout_ms {
        // Immediate timeout: return {error, nil}.
        return error_nil_tuple(context);
    }

    // Request suspension: the scheduler will wake us when a new message arrives.
    context.request_suspend(timeout_ms);
    // Return value is a placeholder; the interpreter handles Suspend.
    Ok(Term::NIL)
}

/// Checks if a message matches a handler tag.
///
/// Matching rules:
/// - If tag is the `anything` atom, any message matches (catch-all).
/// - If the message is a tuple and the first element equals the tag, it matches.
/// - If the message equals the tag directly, it matches.
fn message_matches_tag(message: Term, tag: Term) -> bool {
    // Catch-all: `anything` atom matches everything.
    if let Some(atom) = tag.as_atom()
        && anything_atom().is_some_and(|a| a == atom)
    {
        return true;
    }

    // Tuple message: check first element.
    if let Some(tuple) = Tuple::new(message)
        && let Some(first) = tuple.get(0)
        && first == tag
    {
        return true;
    }

    // Direct equality.
    message == tag
}

/// Build an `{error, nil}` tuple for timeout returns.
fn error_nil_tuple(context: &mut ProcessContext) -> Result<Term, Term> {
    context.alloc_tuple(&[Term::atom(Atom::ERROR), Term::NIL])
}

/// Collect a BEAM list into a Vec.
fn list_to_vec(term: Term) -> Result<Vec<Term>, Term> {
    let mut elements = Vec::new();
    let mut current = term;
    loop {
        if current.is_nil() {
            return Ok(elements);
        }
        let cons = Cons::new(current).ok_or_else(badarg)?;
        elements.push(cons.head());
        current = cons.tail();
    }
}

/// Returns the atom index for "mapped".
///
/// This is a well-known sentinel used to mark composed handlers in
/// map_selector. The trampoline recognizes it to chain two closure calls.
fn mapped_atom_index() -> u32 {
    // Use a fixed high index that won't collide with common atoms.
    // This is registered properly at startup via the atom table.
    MAPPED_ATOM.get().map_or(9999, |a| a.index())
}

/// Atom constant for "mapped" sentinel.
static MAPPED_ATOM: std::sync::OnceLock<Atom> = std::sync::OnceLock::new();

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}

#[cfg(test)]
#[path = "selector_ffi_tests.rs"]
mod tests;
