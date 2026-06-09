//! Tests for non-higher-order collection BIFs: maps, lists, timer.

use crate::atom::{Atom, AtomTable};
use crate::native::{BifRegistryImpl, ProcessContext};
use crate::process::Process;
use crate::scheduler::dirty::DirtySchedulerKind;
use crate::term::Term;
use crate::term::boxed::{Cons, Map, write_closure, write_cons, write_map, write_tuple};

use super::collection_bifs::{
    bif_lists_reverse, bif_maps_from_list, bif_maps_map, bif_maps_merge, bif_maps_remove,
    bif_timer_sleep,
};
use super::register_stdlib_stubs;

fn context(process: &mut Process) -> ProcessContext<'_> {
    let mut context = ProcessContext::new();
    context.set_atom_table(Some(std::sync::Arc::new(AtomTable::with_common_atoms())));
    context.attach_process(process, 0);
    context
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}

// ---- maps:from_list/1 ----

#[test]
fn maps_from_list_builds_map_from_2tuple_list() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);

    // Build [{1, ok}, {2, error}]
    let mut t1_heap = [0u64; 3];
    let t1 = write_tuple(&mut t1_heap, &[Term::small_int(1), Term::atom(Atom::OK)]).unwrap();
    let mut t2_heap = [0u64; 3];
    let t2 = write_tuple(&mut t2_heap, &[Term::small_int(2), Term::atom(Atom::ERROR)]).unwrap();

    let mut c2 = [0u64; 2];
    let tail = write_cons(&mut c2, t2, Term::NIL).unwrap();
    let mut c1 = [0u64; 2];
    let list = write_cons(&mut c1, t1, tail).unwrap();

    let result = bif_maps_from_list(&[list], &mut ctx).unwrap();
    let map = Map::new(result).expect("should be a map");
    assert_eq!(map.len(), 2);
    assert_eq!(map.get(Term::small_int(1)), Some(Term::atom(Atom::OK)));
    assert_eq!(map.get(Term::small_int(2)), Some(Term::atom(Atom::ERROR)));
}

#[test]
fn maps_from_list_empty_list_returns_empty_map() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    let result = bif_maps_from_list(&[Term::NIL], &mut ctx).unwrap();
    let map = Map::new(result).expect("should be a map");
    assert_eq!(map.len(), 0);
}

#[test]
fn maps_from_list_duplicate_keys_last_wins() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);

    // Build [{1, ok}, {1, error}] — last value (error) should win.
    let mut t1_heap = [0u64; 3];
    let t1 = write_tuple(&mut t1_heap, &[Term::small_int(1), Term::atom(Atom::OK)]).unwrap();
    let mut t2_heap = [0u64; 3];
    let t2 = write_tuple(&mut t2_heap, &[Term::small_int(1), Term::atom(Atom::ERROR)]).unwrap();

    let mut c2 = [0u64; 2];
    let tail = write_cons(&mut c2, t2, Term::NIL).unwrap();
    let mut c1 = [0u64; 2];
    let list = write_cons(&mut c1, t1, tail).unwrap();

    let result = bif_maps_from_list(&[list], &mut ctx).unwrap();
    let map = Map::new(result).expect("should be a map");
    assert_eq!(map.len(), 1);
    assert_eq!(map.get(Term::small_int(1)), Some(Term::atom(Atom::ERROR)));
}

#[test]
fn maps_from_list_rejects_non_list() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    assert_eq!(
        bif_maps_from_list(&[Term::small_int(42)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn maps_from_list_rejects_list_of_non_tuples() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    let mut cell = [0u64; 2];
    let list = write_cons(&mut cell, Term::small_int(1), Term::NIL).unwrap();
    assert_eq!(bif_maps_from_list(&[list], &mut ctx), Err(badarg()));
}

#[test]
fn maps_from_list_rejects_wrong_arity() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    assert_eq!(bif_maps_from_list(&[], &mut ctx), Err(badarg()));
    assert_eq!(
        bif_maps_from_list(&[Term::NIL, Term::NIL], &mut ctx),
        Err(badarg())
    );
}

// ---- maps:merge/2 ----

#[test]
fn maps_merge_combines_two_maps() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);

    let mut heap1 = [0u64; 4];
    let m1 = write_map(&mut heap1, &[Term::small_int(1)], &[Term::atom(Atom::OK)]).unwrap();

    let mut heap2 = [0u64; 4];
    let m2 = write_map(
        &mut heap2,
        &[Term::small_int(2)],
        &[Term::atom(Atom::ERROR)],
    )
    .unwrap();

    let result = bif_maps_merge(&[m1, m2], &mut ctx).unwrap();
    let map = Map::new(result).expect("should be a map");
    assert_eq!(map.len(), 2);
    assert_eq!(map.get(Term::small_int(1)), Some(Term::atom(Atom::OK)));
    assert_eq!(map.get(Term::small_int(2)), Some(Term::atom(Atom::ERROR)));
}

#[test]
fn maps_merge_second_overrides_first_on_collision() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);

    let mut heap1 = [0u64; 4];
    let m1 = write_map(&mut heap1, &[Term::small_int(1)], &[Term::atom(Atom::OK)]).unwrap();

    let mut heap2 = [0u64; 4];
    let m2 = write_map(
        &mut heap2,
        &[Term::small_int(1)],
        &[Term::atom(Atom::ERROR)],
    )
    .unwrap();

    let result = bif_maps_merge(&[m1, m2], &mut ctx).unwrap();
    let map = Map::new(result).expect("should be a map");
    assert_eq!(map.len(), 1);
    assert_eq!(map.get(Term::small_int(1)), Some(Term::atom(Atom::ERROR)));
}

#[test]
fn maps_merge_empty_maps() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);

    let mut heap1 = [0u64; 2];
    let m1 = write_map(&mut heap1, &[], &[]).unwrap();
    let mut heap2 = [0u64; 2];
    let m2 = write_map(&mut heap2, &[], &[]).unwrap();

    let result = bif_maps_merge(&[m1, m2], &mut ctx).unwrap();
    let map = Map::new(result).expect("should be a map");
    assert_eq!(map.len(), 0);
}

#[test]
fn maps_merge_rejects_non_maps() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    let mut heap = [0u64; 4];
    let m = write_map(&mut heap, &[Term::small_int(1)], &[Term::atom(Atom::OK)]).unwrap();

    assert_eq!(
        bif_maps_merge(&[Term::small_int(1), m], &mut ctx),
        Err(badarg())
    );
    assert_eq!(
        bif_maps_merge(&[m, Term::small_int(1)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn maps_merge_rejects_wrong_arity() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    assert_eq!(bif_maps_merge(&[], &mut ctx), Err(badarg()));
}

// ---- maps:remove/2 ----

#[test]
fn maps_remove_removes_existing_key() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);

    let mut heap = [0u64; 6];
    let m = write_map(
        &mut heap,
        &[Term::small_int(1), Term::small_int(2)],
        &[Term::atom(Atom::OK), Term::atom(Atom::ERROR)],
    )
    .unwrap();

    let result = bif_maps_remove(&[Term::small_int(1), m], &mut ctx).unwrap();
    let map = Map::new(result).expect("should be a map");
    assert_eq!(map.len(), 1);
    assert_eq!(map.get(Term::small_int(1)), None);
    assert_eq!(map.get(Term::small_int(2)), Some(Term::atom(Atom::ERROR)));
}

#[test]
fn maps_remove_returns_same_structure_if_key_not_present() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);

    let mut heap = [0u64; 4];
    let m = write_map(&mut heap, &[Term::small_int(1)], &[Term::atom(Atom::OK)]).unwrap();

    let result = bif_maps_remove(&[Term::small_int(999), m], &mut ctx).unwrap();
    let map = Map::new(result).expect("should be a map");
    assert_eq!(map.len(), 1);
    assert_eq!(map.get(Term::small_int(1)), Some(Term::atom(Atom::OK)));
}

#[test]
fn maps_remove_from_single_entry_map_returns_empty() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);

    let mut heap = [0u64; 4];
    let m = write_map(&mut heap, &[Term::small_int(1)], &[Term::atom(Atom::OK)]).unwrap();

    let result = bif_maps_remove(&[Term::small_int(1), m], &mut ctx).unwrap();
    let map = Map::new(result).expect("should be a map");
    assert_eq!(map.len(), 0);
}

#[test]
fn maps_remove_rejects_non_map() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    assert_eq!(
        bif_maps_remove(&[Term::small_int(1), Term::small_int(2)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn maps_remove_rejects_wrong_arity() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    assert_eq!(bif_maps_remove(&[], &mut ctx), Err(badarg()));
}

fn test_closure(process: &mut Process, arity: u8) -> Term {
    let heap = process.heap_mut().alloc_slice(7).expect("closure heap");
    write_closure(heap, Atom::OK, 0, arity, 1, 0x100, &[]).expect("test closure")
}

// ---- maps:map/2 ----

#[test]
fn maps_map_empty_map_returns_fresh_empty_map() {
    let mut process = Process::new(1, 256);
    let fun = test_closure(&mut process, 2);
    let mut ctx = context(&mut process);

    let mut heap = [0u64; 2];
    let m = write_map(&mut heap, &[], &[]).unwrap();

    let result = bif_maps_map(&[fun, m], &mut ctx).expect("empty maps:map");
    let mapped = Map::new(result).expect("mapped map");
    assert_eq!(mapped.len(), 0);
    assert!(!ctx.has_trampoline());
}

#[test]
fn maps_map_non_empty_sets_continuation_trampoline() {
    let mut process = Process::new(1, 256);
    let fun = test_closure(&mut process, 2);
    let mut ctx = context(&mut process);

    let mut heap = [0u64; 4];
    let m = write_map(&mut heap, &[Term::small_int(1)], &[Term::atom(Atom::OK)]).unwrap();

    let result = bif_maps_map(&[fun, m], &mut ctx);
    assert_eq!(result, Ok(Term::NIL));
    assert!(ctx.has_trampoline());
}

#[test]
fn maps_map_rejects_wrong_arity() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    assert_eq!(bif_maps_map(&[], &mut ctx), Err(badarg()));
    assert_eq!(bif_maps_map(&[Term::NIL], &mut ctx), Err(badarg()));
}

// ---- lists:reverse/1 ----

#[test]
fn lists_reverse_reverses_proper_list() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);

    // Build [1, 2, 3]
    let mut c3 = [0u64; 2];
    let tail = write_cons(&mut c3, Term::small_int(3), Term::NIL).unwrap();
    let mut c2 = [0u64; 2];
    let mid = write_cons(&mut c2, Term::small_int(2), tail).unwrap();
    let mut c1 = [0u64; 2];
    let list = write_cons(&mut c1, Term::small_int(1), mid).unwrap();

    let result = bif_lists_reverse(&[list], &mut ctx).unwrap();

    // Should be [3, 2, 1]
    let cons1 = Cons::new(result).expect("first cons");
    assert_eq!(cons1.head(), Term::small_int(3));
    let cons2 = Cons::new(cons1.tail()).expect("second cons");
    assert_eq!(cons2.head(), Term::small_int(2));
    let cons3 = Cons::new(cons2.tail()).expect("third cons");
    assert_eq!(cons3.head(), Term::small_int(1));
    assert_eq!(cons3.tail(), Term::NIL);
}

#[test]
fn lists_reverse_empty_list_returns_empty() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    assert_eq!(bif_lists_reverse(&[Term::NIL], &mut ctx), Ok(Term::NIL));
}

#[test]
fn lists_reverse_single_element() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);

    let mut cell = [0u64; 2];
    let list = write_cons(&mut cell, Term::small_int(42), Term::NIL).unwrap();

    let result = bif_lists_reverse(&[list], &mut ctx).unwrap();
    let cons = Cons::new(result).expect("cons");
    assert_eq!(cons.head(), Term::small_int(42));
    assert_eq!(cons.tail(), Term::NIL);
}

#[test]
fn lists_reverse_rejects_non_list() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    assert_eq!(
        bif_lists_reverse(&[Term::small_int(42)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn lists_reverse_rejects_wrong_arity() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    assert_eq!(bif_lists_reverse(&[], &mut ctx), Err(badarg()));
    assert_eq!(
        bif_lists_reverse(&[Term::NIL, Term::NIL], &mut ctx),
        Err(badarg())
    );
}

// ---- timer:sleep/1 ----

#[test]
fn timer_sleep_returns_ok_for_zero() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    assert_eq!(
        bif_timer_sleep(&[Term::small_int(0)], &mut ctx),
        Ok(Term::atom(Atom::OK))
    );
}

#[test]
fn timer_sleep_returns_ok_for_small_duration() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    // Sleep 1ms — fast enough for tests.
    assert_eq!(
        bif_timer_sleep(&[Term::small_int(1)], &mut ctx),
        Ok(Term::atom(Atom::OK))
    );
}

#[test]
fn timer_sleep_rejects_negative() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    assert_eq!(
        bif_timer_sleep(&[Term::small_int(-1)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn timer_sleep_rejects_non_integer() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    assert_eq!(
        bif_timer_sleep(&[Term::atom(Atom::OK)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn timer_sleep_rejects_wrong_arity() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    assert_eq!(bif_timer_sleep(&[], &mut ctx), Err(badarg()));
}

// ---- Registration (collection BIFs) ----

#[test]
fn register_stdlib_stubs_includes_collection_bifs() {
    let atom_table = AtomTable::new();
    let registry = BifRegistryImpl::new();

    register_stdlib_stubs(&registry, &atom_table).expect("registration should succeed");

    let collection_bifs = [
        ("maps", "from_list", 1),
        ("maps", "merge", 2),
        ("maps", "remove", 2),
        ("maps", "map", 2),
        ("lists", "reverse", 1),
        ("timer", "sleep", 1),
    ];

    for (module_name, function_name, arity) in collection_bifs {
        let module = atom_table.intern(module_name);
        let function = atom_table.intern(function_name);
        assert!(
            registry.lookup(module, function, arity).is_some(),
            "missing {module_name}:{function_name}/{arity}"
        );
    }

    let timer = atom_table.intern("timer");
    let sleep = atom_table.intern("sleep");
    let sleep_entry = registry.lookup(timer, sleep, 1).expect("timer:sleep/1");
    assert_eq!(sleep_entry.dirty_kind, Some(DirtySchedulerKind::Io));
}
