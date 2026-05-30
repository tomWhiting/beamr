use super::*;
use crate::native::ProcessContext;
use crate::term::boxed::write_tuple;

fn context() -> ProcessContext {
    ProcessContext::new()
}

#[test]
fn new_selector_returns_nil() {
    let mut ctx = context();
    assert_eq!(bif_new_selector(&[], &mut ctx), Ok(Term::NIL));
}

#[test]
fn new_selector_rejects_arguments() {
    let mut ctx = context();
    assert_eq!(
        bif_new_selector(&[Term::small_int(1)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn insert_selector_handler_prepends_entry() {
    let mut ctx = context();
    let tag = Term::atom(Atom::OK);
    let handler = Term::small_int(42); // placeholder

    let selector = bif_insert_selector_handler(&[Term::NIL, tag, handler], &mut ctx)
        .expect("insert should succeed");

    assert!(selector.is_list());
    let cons = Cons::new(selector).expect("should be a cons cell");
    let entry = Tuple::new(cons.head()).expect("head should be a tuple");
    assert_eq!(entry.arity(), 2);
    assert_eq!(entry.get(0), Some(tag));
    assert_eq!(entry.get(1), Some(handler));
    assert!(cons.tail().is_nil());
}

#[test]
fn merge_selector_concatenates_lists() {
    let mut ctx = context();

    // Build two single-element selectors.
    let sel_a = bif_insert_selector_handler(
        &[Term::NIL, Term::atom(Atom::OK), Term::small_int(1)],
        &mut ctx,
    )
    .expect("insert a");

    let sel_b = bif_insert_selector_handler(
        &[Term::NIL, Term::atom(Atom::ERROR), Term::small_int(2)],
        &mut ctx,
    )
    .expect("insert b");

    let merged = bif_merge_selector(&[sel_a, sel_b], &mut ctx).expect("merge should succeed");

    // Walk the merged list -- should have 2 entries.
    let entries = list_to_vec(merged).expect("merged should be a list");
    assert_eq!(entries.len(), 2);

    let first = Tuple::new(entries[0]).expect("first entry");
    assert_eq!(first.get(0), Some(Term::atom(Atom::OK)));

    let second = Tuple::new(entries[1]).expect("second entry");
    assert_eq!(second.get(0), Some(Term::atom(Atom::ERROR)));
}

#[test]
fn remove_selector_handler_filters_by_tag() {
    let mut ctx = context();

    let sel = bif_insert_selector_handler(
        &[Term::NIL, Term::atom(Atom::OK), Term::small_int(1)],
        &mut ctx,
    )
    .expect("insert ok");

    let sel = bif_insert_selector_handler(
        &[sel, Term::atom(Atom::ERROR), Term::small_int(2)],
        &mut ctx,
    )
    .expect("insert error");

    let filtered = bif_remove_selector_handler(&[sel, Term::atom(Atom::OK)], &mut ctx)
        .expect("remove should succeed");

    let entries = list_to_vec(filtered).expect("should be a list");
    assert_eq!(entries.len(), 1);
    let remaining = Tuple::new(entries[0]).expect("remaining entry");
    assert_eq!(remaining.get(0), Some(Term::atom(Atom::ERROR)));
}

#[test]
fn merge_with_empty_selectors() {
    let mut ctx = context();

    // NIL + NIL = NIL
    let result =
        bif_merge_selector(&[Term::NIL, Term::NIL], &mut ctx).expect("merge empty + empty");
    assert!(result.is_nil());

    // NIL + non-empty = non-empty
    let sel = bif_insert_selector_handler(
        &[Term::NIL, Term::atom(Atom::OK), Term::small_int(1)],
        &mut ctx,
    )
    .expect("insert");
    let result =
        bif_merge_selector(&[Term::NIL, sel], &mut ctx).expect("merge empty + non-empty");
    let entries = list_to_vec(result).expect("list");
    assert_eq!(entries.len(), 1);
}

#[test]
fn message_matches_tag_tuple_first_element() {
    let mut heap = [0u64; 3];
    let tuple =
        write_tuple(&mut heap, &[Term::atom(Atom::OK), Term::small_int(42)]).expect("tuple");
    assert!(message_matches_tag(tuple, Term::atom(Atom::OK)));
    assert!(!message_matches_tag(tuple, Term::atom(Atom::ERROR)));
}

#[test]
fn message_matches_tag_direct_equality() {
    assert!(message_matches_tag(
        Term::atom(Atom::OK),
        Term::atom(Atom::OK)
    ));
    assert!(!message_matches_tag(
        Term::atom(Atom::OK),
        Term::atom(Atom::ERROR)
    ));
}

#[test]
fn message_matches_tag_integer() {
    assert!(message_matches_tag(
        Term::small_int(42),
        Term::small_int(42)
    ));
    assert!(!message_matches_tag(
        Term::small_int(42),
        Term::small_int(43)
    ));
}

#[test]
fn select_rejects_wrong_arity() {
    let mut ctx = context();
    assert_eq!(bif_select(&[], &mut ctx), Err(badarg()));
    assert_eq!(
        bif_select(&[Term::NIL, Term::NIL], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn select_with_timeout_rejects_negative() {
    let mut ctx = context();
    assert_eq!(
        bif_select_with_timeout(&[Term::NIL, Term::small_int(-1)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn register_selector_bifs_registers_all_expected_mfas() {
    use crate::atom::AtomTable;
    use crate::native::BifRegistryImpl;

    let atom_table = AtomTable::with_common_atoms();
    let mut registry = BifRegistryImpl::new();
    register_selector_bifs(&mut registry, &atom_table)
        .expect("selector registration should succeed");

    let module = atom_table.intern("gleam_erlang_ffi");
    for (name, arity) in [
        ("new_selector", 0),
        ("insert_selector_handler", 3),
        ("map_selector", 2),
        ("merge_selector", 2),
        ("remove_selector_handler", 2),
        ("select", 1),
        ("select", 2),
    ] {
        let function = atom_table.intern(name);
        assert!(
            registry.lookup(module, function, arity).is_some(),
            "missing gleam_erlang_ffi:{name}/{arity}"
        );
    }
}

#[test]
fn register_selector_bifs_fails_on_duplicate() {
    use crate::atom::AtomTable;
    use crate::native::BifRegistryImpl;

    let atom_table = AtomTable::with_common_atoms();
    let mut registry = BifRegistryImpl::new();
    register_selector_bifs(&mut registry, &atom_table).expect("first");
    assert!(register_selector_bifs(&mut registry, &atom_table).is_err());
}

#[test]
fn select_with_facility_finds_matching_message() {
    use crate::native::select::MailboxSnapshot;
    use std::sync::Arc;

    let mut ctx = context();

    // Build a selector with one handler for tag `ok`.
    let handler = Term::small_int(99); // placeholder for closure
    let selector =
        bif_insert_selector_handler(&[Term::NIL, Term::atom(Atom::OK), handler], &mut ctx)
            .expect("insert");

    // Set up a mailbox snapshot with a matching tuple message.
    let mut msg_heap = [0u64; 3];
    let message =
        write_tuple(&mut msg_heap, &[Term::atom(Atom::OK), Term::small_int(42)])
            .expect("message tuple");
    let snapshot = Arc::new(MailboxSnapshot::new(vec![message]));
    ctx.set_select_facility(Some(
        snapshot.clone() as Arc<dyn crate::native::SelectFacility>,
    ));

    // Call select -- should find the matching message.
    let _result = bif_select(&[selector], &mut ctx).expect("select should succeed");

    // The snapshot should record the removal.
    assert_eq!(snapshot.removed_index(), Some(0));

    // A trampoline should have been set.
    assert!(ctx.has_trampoline());
    let trampoline = ctx.take_trampoline().expect("trampoline should be set");
    assert_eq!(trampoline.fun, handler);
    assert_eq!(trampoline.args.len(), 1);
    assert_eq!(trampoline.args[0], message);
}

#[test]
fn select_with_no_match_requests_suspend() {
    use crate::native::select::MailboxSnapshot;
    use std::sync::Arc;

    let mut ctx = context();

    // Build a selector that looks for `error` tag.
    let handler = Term::small_int(99);
    let selector = bif_insert_selector_handler(
        &[Term::NIL, Term::atom(Atom::ERROR), handler],
        &mut ctx,
    )
    .expect("insert");

    // Mailbox has only `ok`-tagged messages.
    let mut msg_heap = [0u64; 3];
    let message =
        write_tuple(&mut msg_heap, &[Term::atom(Atom::OK), Term::small_int(42)])
            .expect("message tuple");
    let snapshot = Arc::new(MailboxSnapshot::new(vec![message]));
    ctx.set_select_facility(Some(
        snapshot.clone() as Arc<dyn crate::native::SelectFacility>,
    ));

    let _result = bif_select(&[selector], &mut ctx).expect("select returns ok");

    // No match -- should request suspend.
    assert!(!ctx.has_trampoline());
    let suspend = ctx.take_suspend().expect("suspend should be requested");
    assert_eq!(suspend.timeout_ms, None);
    // No message should have been removed.
    assert_eq!(snapshot.removed_index(), None);
}

#[test]
fn select_with_timeout_zero_returns_error_nil_on_no_match() {
    use crate::native::select::MailboxSnapshot;
    use std::sync::Arc;

    let mut ctx = context();

    let handler = Term::small_int(99);
    let selector = bif_insert_selector_handler(
        &[Term::NIL, Term::atom(Atom::ERROR), handler],
        &mut ctx,
    )
    .expect("insert");

    // Mailbox has no matching messages.
    let mut msg_heap = [0u64; 3];
    let message =
        write_tuple(&mut msg_heap, &[Term::atom(Atom::OK), Term::small_int(1)]).expect("tuple");
    let snapshot = Arc::new(MailboxSnapshot::new(vec![message]));
    ctx.set_select_facility(Some(
        snapshot as Arc<dyn crate::native::SelectFacility>,
    ));

    let result = bif_select_with_timeout(&[selector, Term::small_int(0)], &mut ctx)
        .expect("select with timeout 0");

    // Should return {error, nil}.
    let tuple = Tuple::new(result).expect("result should be a tuple");
    assert_eq!(tuple.get(0), Some(Term::atom(Atom::ERROR)));
    assert_eq!(tuple.get(1), Some(Term::NIL));

    // No trampoline or suspend.
    assert!(!ctx.has_trampoline());
    assert!(ctx.take_suspend().is_none());
}

#[test]
fn select_first_matching_handler_wins() {
    use crate::native::select::MailboxSnapshot;
    use std::sync::Arc;

    let mut ctx = context();

    // Build a selector with two handlers for the same tag.
    let handler1 = Term::small_int(1);
    let handler2 = Term::small_int(2);
    let selector = bif_insert_selector_handler(
        &[Term::NIL, Term::atom(Atom::OK), handler1],
        &mut ctx,
    )
    .expect("insert 1");
    let selector = bif_insert_selector_handler(
        &[selector, Term::atom(Atom::OK), handler2],
        &mut ctx,
    )
    .expect("insert 2");

    let mut msg_heap = [0u64; 3];
    let message =
        write_tuple(&mut msg_heap, &[Term::atom(Atom::OK), Term::small_int(42)]).expect("tuple");
    let snapshot = Arc::new(MailboxSnapshot::new(vec![message]));
    ctx.set_select_facility(Some(
        snapshot as Arc<dyn crate::native::SelectFacility>,
    ));

    let _result = bif_select(&[selector], &mut ctx).expect("select");
    let trampoline = ctx.take_trampoline().expect("trampoline");
    // The most recently inserted handler (handler2) is first in the list
    // because insert prepends. So handler2 should be the winner.
    assert_eq!(trampoline.fun, handler2);
}

#[test]
fn map_selector_wraps_handlers() {
    let mut ctx = context();

    let handler = Term::small_int(42);
    let map_fun = Term::small_int(99);

    let selector = bif_insert_selector_handler(
        &[Term::NIL, Term::atom(Atom::OK), handler],
        &mut ctx,
    )
    .expect("insert");

    let mapped =
        bif_map_selector(&[selector, map_fun], &mut ctx).expect("map_selector should succeed");

    let entries = list_to_vec(mapped).expect("should be a list");
    assert_eq!(entries.len(), 1);
    let entry = Tuple::new(entries[0]).expect("entry tuple");
    assert_eq!(entry.get(0), Some(Term::atom(Atom::OK)));

    // The handler should now be a {mapped, MapFun, OriginalHandler} tuple.
    let wrapped = Tuple::new(entry.get(1).expect("wrapped handler"))
        .expect("wrapped should be a tuple");
    assert_eq!(wrapped.arity(), 3);
    assert_eq!(wrapped.get(1), Some(map_fun));
    assert_eq!(wrapped.get(2), Some(handler));
}
