//! Erlang Term Storage BIFs.
//!
//! These BIFs expose the core ETS table lifecycle and lookup operations through
//! the normal native-function registry under the `ets` module.

use std::{collections::BTreeMap, sync::Arc};

use crate::atom::{Atom, AtomTable};
use crate::ets::{
    AccessOp, CompiledMatchSpec, EtsError, EtsHeir, EtsRegistry, EtsTable, EtsTableId,
    EtsTableMetadata, EtsTableType, Protection, TermKey, copy_term_to_ets,
};
use crate::native::stdlib_stubs::maps_bifs::ContinuationStep;
use crate::native::{
    BifRegistryImpl, Capability, NativeContinuation, NativeFn, NativeRegistrationError,
    ProcessContext,
};
use crate::term::boxed::{Closure, Cons, Tuple};
use crate::term::{Term, compare};

/// Scheduler-facing ETS registry operations used by ETS BIFs.
pub trait EtsFacility: Send + Sync {
    /// Create a table and return its allocated id.
    fn create_table(&self, metadata: EtsTableMetadata) -> Result<EtsTableId, EtsError>;
    /// Look up a table by numeric id.
    fn lookup_table(&self, id: EtsTableId) -> Option<Arc<dyn EtsTable>>;
    /// Look up a named table by atom.
    fn lookup_named_table(&self, name: Atom) -> Option<Arc<dyn EtsTable>>;
    /// Look up the id currently bound to a table name.
    fn lookup_table_by_name(&self, name: Atom) -> Option<EtsTableId>;
    /// Delete a table by numeric id.
    fn delete_table(&self, id: EtsTableId) -> bool;
    /// Transfer table ownership and deliver the ETS-TRANSFER message.
    fn give_away_table(
        &self,
        table_id: EtsTableId,
        new_owner: u64,
        from_pid: u64,
        gift_data: Term,
        atom_table: &AtomTable,
    ) -> Result<(), EtsError>;
}

fn parse_heir_option(tuple: Tuple, none: Atom, options: &mut NewOptions) -> Result<(), Term> {
    match tuple.arity() {
        2 => {
            if tuple.get(1) != Some(Term::atom(none)) {
                return Err(badarg());
            }
            options.heir = None;
            Ok(())
        }
        3 => {
            let heir_pid = tuple.get(1).and_then(Term::as_pid).ok_or_else(badarg)?;
            let heir_data = tuple.get(2).ok_or_else(badarg)?;
            let data = copy_term_to_ets(heir_data).map_err(ets_error_to_badarg)?;
            options.heir = Some(EtsHeir {
                pid: heir_pid,
                data,
            });
            Ok(())
        }
        _ => Err(badarg()),
    }
}

impl EtsFacility for EtsRegistry {
    fn create_table(&self, metadata: EtsTableMetadata) -> Result<EtsTableId, EtsError> {
        self.try_create_table(metadata)
    }

    fn lookup_table(&self, id: EtsTableId) -> Option<Arc<dyn EtsTable>> {
        self.lookup_table(id)
    }

    fn lookup_named_table(&self, name: Atom) -> Option<Arc<dyn EtsTable>> {
        self.lookup_named_table(name)
    }

    fn lookup_table_by_name(&self, name: Atom) -> Option<EtsTableId> {
        self.lookup_table_by_name(name)
    }

    fn delete_table(&self, id: EtsTableId) -> bool {
        self.delete_table(id)
    }

    fn give_away_table(
        &self,
        table_id: EtsTableId,
        new_owner: u64,
        _from_pid: u64,
        _gift_data: Term,
        _atom_table: &AtomTable,
    ) -> Result<(), EtsError> {
        if self.transfer_table_owner(table_id, new_owner) {
            Ok(())
        } else {
            Err(EtsError::Badarg)
        }
    }
}

type EtsBif = (&'static str, u8, NativeFn);

const ETS_BIFS: &[EtsBif] = &[
    ("new", 2, bif_new),
    ("insert", 2, bif_insert),
    ("lookup", 2, bif_lookup),
    ("tab2list", 1, bif_tab2list),
    ("foldl", 3, bif_foldl),
    ("match", 1, bif_match_1),
    ("match", 2, bif_match_2),
    ("match", 3, bif_match_3),
    ("match_object", 2, bif_match_object),
    ("match_delete", 2, bif_match_delete),
    ("select", 1, bif_select_1),
    ("select", 2, bif_select_2),
    ("select", 3, bif_select_3),
    ("delete", 1, bif_delete_1),
    ("delete", 2, bif_delete_2),
    ("give_away", 3, bif_give_away),
    ("member", 2, bif_member),
    ("first", 1, bif_first),
    ("next", 2, bif_next),
    ("last", 1, bif_last),
    ("prev", 2, bif_prev),
    ("info", 1, bif_info_1),
    ("info", 2, bif_info_2),
];

const INFO_ITEMS: &[&str] = &[
    "name",
    "type",
    "size",
    "keypos",
    "protection",
    "owner",
    "memory",
];

struct NewOptions {
    table_type: EtsTableType,
    protection: Protection,
    named_table: bool,
    keypos: usize,
    read_concurrency: bool,
    write_concurrency: bool,
    heir: Option<EtsHeir>,
}

/// Native continuation state for ets:foldl/3.
#[derive(Clone, Debug)]
pub struct EtsFoldlState {
    fun: Term,
    entries: Vec<Term>,
    index: usize,
}

/// Registers all ETS BIFs under the `ets` module.
pub fn register_ets_bifs(
    registry: &BifRegistryImpl,
    atom_table: &AtomTable,
) -> Result<(), NativeRegistrationError> {
    let ets = atom_table.intern("ets");

    for &(function_name, arity, native_function) in ETS_BIFS {
        let function = atom_table.intern(function_name);
        registry.register(
            ets,
            function,
            arity,
            native_function,
            Capability::ProcessLocal,
        )?;
    }

    Ok(())
}

/// ets:new/2 — create an ETS table.
pub fn bif_new(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [name_term, options_term] = args else {
        return Err(badarg());
    };
    let name = name_term.as_atom().ok_or_else(badarg)?;
    let options = parse_new_options(*options_term, context.atom_table().ok_or_else(badarg)?)?;
    let owner = context.pid().ok_or_else(badarg)?;
    let facility = context.ets_facility().ok_or_else(badarg)?;

    let metadata = EtsTableMetadata {
        name: options.named_table.then_some(name),
        id: 0,
        table_type: options.table_type,
        protection: options.protection,
        owner: crate::ets::EtsOwner::new(owner),
        keypos: options.keypos,
        read_concurrency: options.read_concurrency,
        write_concurrency: options.write_concurrency,
        heir: options.heir,
    };
    let table_id = facility
        .create_table(metadata)
        .map_err(ets_error_to_badarg)?;

    if options.named_table {
        Ok(Term::atom(name))
    } else {
        small_int_from_u64(table_id)
    }
}

/// ets:insert/2 — insert one tuple or a proper list of tuples.
pub fn bif_insert(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [tab, object_or_objects] = args else {
        return Err(badarg());
    };
    let table = resolve_existing_table(*tab, context, MissingTable::Badarg)?;
    let caller = context.pid().ok_or_else(badarg)?;
    table
        .check_access(caller, AccessOp::Write)
        .map_err(|_| badarg())?;

    let tuples = parse_insert_objects(*object_or_objects)?;
    for tuple in &tuples {
        crate::ets::tuple_key(*tuple, table.metadata().keypos).map_err(ets_error_to_badarg)?;
    }
    for tuple in tuples {
        table.insert(tuple).map_err(ets_error_to_badarg)?;
    }

    Ok(Term::atom(Atom::TRUE))
}

/// ets:give_away/3 — transfer a table to another process.
pub fn bif_give_away(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [tab, pid_term, gift_data] = args else {
        return Err(badarg());
    };
    let table = resolve_existing_table(*tab, context, MissingTable::Badarg)?;
    let caller = context.pid().ok_or_else(badarg)?;
    if table.metadata().owner.get() != caller {
        return Err(badarg());
    }
    let recipient = pid_term.as_pid().ok_or_else(badarg)?;
    let atom_table = context.atom_table().ok_or_else(badarg)?;
    let facility = context.ets_facility().ok_or_else(badarg)?;
    facility
        .give_away_table(
            table.metadata().id,
            recipient,
            caller,
            *gift_data,
            atom_table,
        )
        .map_err(ets_error_to_badarg)?;
    Ok(Term::atom(Atom::TRUE))
}

/// ets:lookup/2 — return all tuples matching a key.
pub fn bif_lookup(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [tab, key] = args else {
        return Err(badarg());
    };
    let table = resolve_existing_table(*tab, context, MissingTable::Badarg)?;
    let caller = context.pid().ok_or_else(badarg)?;
    table
        .check_access(caller, AccessOp::Read)
        .map_err(|_| badarg())?;

    let tuples = table.lookup(*key);
    context.ensure_heap_space(list_heap_words(tuples.len()))?;
    context.alloc_list(&tuples)
}

/// ets:tab2list/1 — return a snapshot list of all tuples in the table.
pub fn bif_tab2list(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [tab] = args else {
        return Err(badarg());
    };
    let table = resolve_readable_table(*tab, context)?;

    let tuples = table.tab2list();
    context.ensure_heap_space(list_heap_words(tuples.len()))?;
    context.alloc_list(&tuples)
}

/// ets:foldl/3 — fold a function over a table snapshot from left to right.
pub fn bif_foldl(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [fun, acc, tab] = args else {
        return Err(badarg());
    };
    ensure_fun_arity(*fun, 2)?;
    let table = resolve_readable_table(*tab, context)?;
    let entries = table.tab2list();
    if entries.is_empty() {
        return Ok(*acc);
    }

    context.set_continuation_trampoline(
        *fun,
        vec![entries[0], *acc],
        NativeContinuation::EtsFoldl(EtsFoldlState {
            fun: *fun,
            entries,
            index: 1,
        }),
    );
    Ok(Term::NIL)
}

/// Resume ets:foldl/3 after one closure invocation returns the next accumulator.
pub fn resume_ets_foldl(
    state: EtsFoldlState,
    closure_result: Term,
) -> Result<ContinuationStep, Term> {
    if let Some(element) = state.entries.get(state.index).copied() {
        Ok(ContinuationStep::Call {
            fun: state.fun,
            args: vec![element, closure_result],
            continuation: NativeContinuation::EtsFoldl(EtsFoldlState {
                fun: state.fun,
                entries: state.entries,
                index: state.index + 1,
            }),
        })
    } else {
        Ok(ContinuationStep::Done(closure_result))
    }
}

/// ets:match/1 — resume a paginated match continuation.
pub fn bif_match_1(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [continuation] = args else {
        return Err(badarg());
    };
    let continuation =
        MatchContinuation::parse(*continuation, MatchContinuationKind::Match, context)?;
    let table = resolve_readable_table(continuation.tab, context)?;
    collect_match_page(
        &table,
        continuation.pattern,
        continuation.position,
        continuation.limit,
        context,
    )
}

/// ets:match/2 — return bound variables for all objects matching a pattern.
pub fn bif_match_2(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [tab, pattern] = args else {
        return Err(badarg());
    };
    let table = resolve_readable_table(*tab, context)?;
    let results = collect_match_results(&table, *pattern, MatchResultMode::Bindings, context)?;
    context.alloc_list(&results)
}

/// ets:match/3 — return a limited page of bound-variable results plus a continuation.
pub fn bif_match_3(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [tab, pattern, limit] = args else {
        return Err(badarg());
    };
    let limit = parse_positive_limit(*limit)?;
    let table = resolve_readable_table(*tab, context)?;
    collect_match_page(&table, *pattern, 0, limit, context)
}

/// ets:match_object/2 — return all objects matching a pattern.
pub fn bif_match_object(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [tab, pattern] = args else {
        return Err(badarg());
    };
    let table = resolve_readable_table(*tab, context)?;
    let results = collect_match_results(&table, *pattern, MatchResultMode::Object, context)?;
    context.alloc_list(&results)
}

/// ets:select/1 — resume a paginated select continuation.
pub fn bif_select_1(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [continuation] = args else {
        return Err(badarg());
    };
    let continuation =
        MatchContinuation::parse(*continuation, MatchContinuationKind::Select, context)?;
    let table = resolve_readable_table(continuation.tab, context)?;
    collect_select_page(
        &table,
        continuation.pattern,
        continuation.position,
        continuation.limit,
        context,
    )
}

/// ets:select/2 — evaluate a match specification against all table objects.
pub fn bif_select_2(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [tab, spec] = args else {
        return Err(badarg());
    };
    let table = resolve_readable_table(*tab, context)?;
    let results = collect_select_results(&table, *spec, 0, None, context)?.results;
    context.alloc_list(&results)
}

/// ets:select/3 — evaluate a limited page of match-spec results plus a continuation.
pub fn bif_select_3(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [tab, spec, limit] = args else {
        return Err(badarg());
    };
    let limit = parse_positive_limit(*limit)?;
    let table = resolve_readable_table(*tab, context)?;
    collect_select_page(&table, *spec, 0, limit, context)
}

/// ets:match_delete/2 — delete every object matching a pattern.
pub fn bif_match_delete(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [tab, pattern] = args else {
        return Err(badarg());
    };
    let table = resolve_existing_table(*tab, context, MissingTable::Badarg)?;
    let caller = context.pid().ok_or_else(badarg)?;
    table
        .check_access(caller, AccessOp::Write)
        .map_err(|_| badarg())?;

    for object in table.tab2list() {
        if ets_pattern_match(*pattern, object, context)?.is_some() {
            let _deleted = table.delete_object(object);
        }
    }
    Ok(Term::atom(Atom::TRUE))
}

/// ets:first/1 — return the first key in a table snapshot or '$end_of_table'.
pub fn bif_first(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [tab] = args else {
        return Err(badarg());
    };
    let table = resolve_readable_table(*tab, context)?;
    let keys = table_snapshot_keys(&table, context)?;
    Ok(keys.first().copied().unwrap_or(end_of_table_atom(context)?))
}

/// ets:next/2 — return the next key in a table snapshot or '$end_of_table'.
pub fn bif_next(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [tab, key] = args else {
        return Err(badarg());
    };
    let table = resolve_readable_table(*tab, context)?;
    let keys = table_snapshot_keys(&table, context)?;
    cursor_neighbor(&table, keys, *key, CursorDirection::Next, context)
}

/// ets:last/1 — return the last key in a table snapshot or '$end_of_table'.
pub fn bif_last(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [tab] = args else {
        return Err(badarg());
    };
    let table = resolve_readable_table(*tab, context)?;
    let keys = table_snapshot_keys(&table, context)?;
    Ok(keys.last().copied().unwrap_or(end_of_table_atom(context)?))
}

/// ets:prev/2 — return the previous key in a table snapshot or '$end_of_table'.
pub fn bif_prev(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [tab, key] = args else {
        return Err(badarg());
    };
    let table = resolve_readable_table(*tab, context)?;
    let keys = table_snapshot_keys(&table, context)?;
    cursor_neighbor(&table, keys, *key, CursorDirection::Prev, context)
}

/// ets:delete/1 — delete an entire table. Only the owner may delete it.
pub fn bif_delete_1(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [tab] = args else {
        return Err(badarg());
    };
    let table = resolve_existing_table(*tab, context, MissingTable::Badarg)?;
    let caller = context.pid().ok_or_else(badarg)?;
    if table.metadata().owner.get() != caller {
        return Err(badarg());
    }

    let facility = context.ets_facility().ok_or_else(badarg)?;
    if !facility.delete_table(table.metadata().id) {
        return Err(badarg());
    }
    Ok(Term::atom(Atom::TRUE))
}

/// ets:delete/2 — delete all entries matching a key.
pub fn bif_delete_2(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [tab, key] = args else {
        return Err(badarg());
    };
    let table = resolve_existing_table(*tab, context, MissingTable::Badarg)?;
    let caller = context.pid().ok_or_else(badarg)?;
    table
        .check_access(caller, AccessOp::Write)
        .map_err(|_| badarg())?;
    let _deleted = table.delete_key(*key);
    Ok(Term::atom(Atom::TRUE))
}

/// ets:member/2 — true when a key has at least one object.
pub fn bif_member(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [tab, key] = args else {
        return Err(badarg());
    };
    let table = resolve_existing_table(*tab, context, MissingTable::Badarg)?;
    let caller = context.pid().ok_or_else(badarg)?;
    table
        .check_access(caller, AccessOp::Read)
        .map_err(|_| badarg())?;
    Ok(bool_term(!table.lookup(*key).is_empty()))
}

/// ets:info/1 — return a deterministic metadata proplist, or `undefined`.
pub fn bif_info_1(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [tab] = args else {
        return Err(badarg());
    };
    let Some(table) = resolve_table(*tab, context)? else {
        return Ok(Term::atom(Atom::UNDEFINED));
    };

    let atom_table = context.atom_table().ok_or_else(badarg)?;
    let item_atoms = INFO_ITEMS
        .iter()
        .map(|item| atom_table.intern(item))
        .collect::<Vec<_>>();
    let values = INFO_ITEMS
        .iter()
        .map(|item| info_value(&table, item, context))
        .collect::<Result<Vec<_>, _>>()?;

    context.ensure_heap_space(info_proplist_heap_words(INFO_ITEMS.len()))?;
    let mut entries = Vec::with_capacity(INFO_ITEMS.len());
    for (item_atom, value) in item_atoms.into_iter().zip(values) {
        entries.push(context.alloc_tuple(&[Term::atom(item_atom), value])?);
    }
    context.alloc_list(&entries)
}

/// ets:info/2 — return a single metadata item, or `undefined` for missing table.
pub fn bif_info_2(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [tab, item_term] = args else {
        return Err(badarg());
    };
    let item = item_term.as_atom().ok_or_else(badarg)?;
    let atom_table = context.atom_table().ok_or_else(badarg)?;
    let item_name = atom_table.resolve(item).ok_or_else(badarg)?;
    if !INFO_ITEMS.contains(&item_name) {
        return Err(badarg());
    }

    let Some(table) = resolve_table(*tab, context)? else {
        return Ok(Term::atom(Atom::UNDEFINED));
    };
    info_value(&table, item_name, context)
}

fn parse_new_options(options_term: Term, atom_table: &AtomTable) -> Result<NewOptions, Term> {
    let mut options = NewOptions {
        table_type: EtsTableType::Set,
        protection: Protection::Protected,
        named_table: false,
        keypos: 1,
        read_concurrency: false,
        write_concurrency: false,
        heir: None,
    };
    let set = atom_table.intern("set");
    let ordered_set = atom_table.intern("ordered_set");
    let bag = atom_table.intern("bag");
    let duplicate_bag = atom_table.intern("duplicate_bag");
    let public = atom_table.intern("public");
    let protected = atom_table.intern("protected");
    let private = atom_table.intern("private");
    let named_table = atom_table.intern("named_table");
    let keypos = atom_table.intern("keypos");
    let read_concurrency = atom_table.intern("read_concurrency");
    let write_concurrency = atom_table.intern("write_concurrency");
    let heir = atom_table.intern("heir");
    let none = atom_table.intern("none");

    let mut tail = options_term;
    while !tail.is_nil() {
        let cons = Cons::new(tail).ok_or_else(badarg)?;
        let option = cons.head();
        if let Some(atom) = option.as_atom() {
            if atom == set {
                options.table_type = EtsTableType::Set;
            } else if atom == ordered_set {
                options.table_type = EtsTableType::OrderedSet;
            } else if atom == bag {
                options.table_type = EtsTableType::Bag;
            } else if atom == duplicate_bag {
                options.table_type = EtsTableType::DuplicateBag;
            } else if atom == public {
                options.protection = Protection::Public;
            } else if atom == protected {
                options.protection = Protection::Protected;
            } else if atom == private {
                options.protection = Protection::Private;
            } else if atom == named_table {
                options.named_table = true;
            } else {
                return Err(badarg());
            }
        } else if let Some(tuple) = Tuple::new(option) {
            let option_name = tuple.get(0).ok_or_else(badarg)?;
            if option_name == Term::atom(heir) {
                parse_heir_option(tuple, none, &mut options)?;
                tail = cons.tail();
                continue;
            }
            if tuple.arity() != 2 {
                return Err(badarg());
            }
            let option_value = tuple.get(1).ok_or_else(badarg)?;
            if option_name == Term::atom(keypos) {
                let keypos_value = option_value
                    .as_small_int()
                    .and_then(|value| usize::try_from(value).ok())
                    .filter(|value| *value > 0)
                    .ok_or_else(badarg)?;
                options.keypos = keypos_value;
            } else if option_name == Term::atom(read_concurrency) {
                options.read_concurrency = parse_bool_option(option_value)?;
            } else if option_name == Term::atom(write_concurrency) {
                options.write_concurrency = parse_bool_option(option_value)?;
            } else {
                return Err(badarg());
            }
        } else {
            return Err(badarg());
        }
        tail = cons.tail();
    }

    Ok(options)
}

fn parse_bool_option(value: Term) -> Result<bool, Term> {
    if value == Term::atom(Atom::TRUE) {
        Ok(true)
    } else if value == Term::atom(Atom::FALSE) {
        Ok(false)
    } else {
        Err(badarg())
    }
}

fn parse_insert_objects(object_or_objects: Term) -> Result<Vec<Term>, Term> {
    if Tuple::new(object_or_objects).is_some() {
        return Ok(vec![object_or_objects]);
    }

    let mut tuples = Vec::new();
    let mut tail = object_or_objects;
    while !tail.is_nil() {
        let cons = Cons::new(tail).ok_or_else(badarg)?;
        let tuple = cons.head();
        if Tuple::new(tuple).is_none() {
            return Err(badarg());
        }
        tuples.push(tuple);
        tail = cons.tail();
    }
    Ok(tuples)
}

#[derive(Copy, Clone)]
enum MatchResultMode {
    Bindings,
    Object,
}

#[derive(Copy, Clone, Eq, PartialEq)]
enum MatchContinuationKind {
    Match,
    Select,
}

struct MatchContinuation {
    tab: Term,
    position: usize,
    limit: usize,
    pattern: Term,
}

struct PagedResults {
    results: Vec<Term>,
    next_position: Option<usize>,
}

impl MatchContinuation {
    fn parse(
        term: Term,
        expected_kind: MatchContinuationKind,
        context: &ProcessContext,
    ) -> Result<Self, Term> {
        let tuple = Tuple::new(term).ok_or_else(badarg)?;
        if tuple.arity() != 5 {
            return Err(badarg());
        }
        let tag = tuple.get(0).ok_or_else(badarg)?;
        let table_id = tuple.get(1).ok_or_else(badarg)?;
        let position = tuple.get(2).ok_or_else(badarg)?;
        let limit = tuple.get(3).ok_or_else(badarg)?;
        let pattern = tuple.get(4).ok_or_else(badarg)?;
        if tag != continuation_tag(expected_kind, context)? {
            return Err(badarg());
        }
        Ok(Self {
            tab: table_id,
            position: position
                .as_small_int()
                .and_then(|value| usize::try_from(value).ok())
                .ok_or_else(badarg)?,
            limit: parse_positive_limit(limit)?,
            pattern,
        })
    }
}

fn collect_match_results(
    table: &Arc<dyn EtsTable>,
    pattern: Term,
    mode: MatchResultMode,
    context: &mut ProcessContext,
) -> Result<Vec<Term>, Term> {
    let mut results = Vec::new();
    for object in table.tab2list() {
        let Some(bindings) = ets_pattern_match(pattern, object, context)? else {
            continue;
        };
        match mode {
            MatchResultMode::Bindings => results.push(alloc_bindings_list(&bindings, context)?),
            MatchResultMode::Object => results.push(object),
        }
    }
    Ok(results)
}

fn collect_match_page(
    table: &Arc<dyn EtsTable>,
    pattern: Term,
    start_position: usize,
    limit: usize,
    context: &mut ProcessContext,
) -> Result<Term, Term> {
    let page = collect_match_paged_results(table, pattern, start_position, limit, context)?;
    finish_paged_results(
        page,
        MatchContinuationKind::Match,
        table.metadata().id,
        pattern,
        limit,
        context,
    )
}

fn collect_match_paged_results(
    table: &Arc<dyn EtsTable>,
    pattern: Term,
    start_position: usize,
    limit: usize,
    context: &mut ProcessContext,
) -> Result<PagedResults, Term> {
    let snapshot = table.tab2list();
    let mut results = Vec::new();
    for (position, object) in snapshot.into_iter().enumerate().skip(start_position) {
        let Some(bindings) = ets_pattern_match(pattern, object, context)? else {
            continue;
        };
        if results.len() == limit {
            return Ok(PagedResults {
                results,
                next_position: Some(position),
            });
        }
        results.push(alloc_bindings_list(&bindings, context)?);
    }
    Ok(PagedResults {
        results,
        next_position: None,
    })
}

fn collect_select_results(
    table: &Arc<dyn EtsTable>,
    spec: Term,
    start_position: usize,
    limit: Option<usize>,
    context: &mut ProcessContext,
) -> Result<PagedResults, Term> {
    let compiled = {
        let atom_table = context.atom_table().ok_or_else(badarg)?;
        CompiledMatchSpec::compile(spec, atom_table).map_err(|_| badarg())?
    };
    let snapshot = table.tab2list();
    let mut results = Vec::new();
    for (position, object) in snapshot.into_iter().enumerate().skip(start_position) {
        if limit.is_some_and(|limit| results.len() == limit) {
            return Ok(PagedResults {
                results,
                next_position: Some(position),
            });
        }
        if let Some(result) = compiled
            .eval_with_context(object, context)
            .map_err(|_| badarg())?
        {
            results.push(result);
        }
    }
    Ok(PagedResults {
        results,
        next_position: None,
    })
}

fn collect_select_page(
    table: &Arc<dyn EtsTable>,
    spec: Term,
    start_position: usize,
    limit: usize,
    context: &mut ProcessContext,
) -> Result<Term, Term> {
    let page = collect_select_results(table, spec, start_position, Some(limit), context)?;
    finish_paged_results(
        page,
        MatchContinuationKind::Select,
        table.metadata().id,
        spec,
        limit,
        context,
    )
}

fn finish_paged_results(
    page: PagedResults,
    kind: MatchContinuationKind,
    table_id: EtsTableId,
    pattern_or_spec: Term,
    limit: usize,
    context: &mut ProcessContext,
) -> Result<Term, Term> {
    if page.results.is_empty() && page.next_position.is_none() {
        return end_of_table_atom(context);
    }
    let results = context.alloc_list(&page.results)?;
    let continuation = match page.next_position {
        Some(next_position) => alloc_match_continuation(
            kind,
            table_id,
            next_position,
            limit,
            pattern_or_spec,
            context,
        )?,
        None => end_of_table_atom(context)?,
    };
    context.alloc_tuple(&[results, continuation])
}

fn alloc_match_continuation(
    kind: MatchContinuationKind,
    table_id: EtsTableId,
    position: usize,
    limit: usize,
    pattern_or_spec: Term,
    context: &mut ProcessContext,
) -> Result<Term, Term> {
    let tag = continuation_tag(kind, context)?;
    context.alloc_tuple(&[
        tag,
        small_int_from_u64(table_id)?,
        small_int_from_usize(position)?,
        small_int_from_usize(limit)?,
        pattern_or_spec,
    ])
}

fn continuation_tag(kind: MatchContinuationKind, context: &ProcessContext) -> Result<Term, Term> {
    let atom_table = context.atom_table().ok_or_else(badarg)?;
    let name = match kind {
        MatchContinuationKind::Match => "$ets_match_continuation",
        MatchContinuationKind::Select => "$ets_select_continuation",
    };
    Ok(Term::atom(atom_table.intern(name)))
}

fn alloc_bindings_list(
    bindings: &BTreeMap<usize, Term>,
    context: &mut ProcessContext,
) -> Result<Term, Term> {
    let values = bindings.values().copied().collect::<Vec<_>>();
    context.alloc_list(&values)
}

fn parse_positive_limit(limit: Term) -> Result<usize, Term> {
    limit
        .as_small_int()
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(badarg)
}

#[derive(Copy, Clone)]
enum MissingTable {
    Badarg,
}

fn resolve_existing_table(
    tab: Term,
    context: &ProcessContext,
    missing: MissingTable,
) -> Result<Arc<dyn EtsTable>, Term> {
    match resolve_table(tab, context)? {
        Some(table) => Ok(table),
        None => match missing {
            MissingTable::Badarg => Err(badarg()),
        },
    }
}

fn resolve_table(tab: Term, context: &ProcessContext) -> Result<Option<Arc<dyn EtsTable>>, Term> {
    let facility = context.ets_facility().ok_or_else(badarg)?;
    if let Some(table_id) = tab.as_small_int().and_then(ets_table_id_from_i64) {
        return Ok(facility.lookup_table(table_id));
    }
    if let Some(name) = tab.as_atom() {
        return Ok(facility.lookup_named_table(name));
    }
    Err(badarg())
}

fn resolve_readable_table(tab: Term, context: &ProcessContext) -> Result<Arc<dyn EtsTable>, Term> {
    let table = resolve_existing_table(tab, context, MissingTable::Badarg)?;
    let caller = context.pid().ok_or_else(badarg)?;
    table
        .check_access(caller, AccessOp::Read)
        .map_err(|_| badarg())?;
    Ok(table)
}

fn ets_pattern_match(
    pattern: Term,
    object: Term,
    context: &ProcessContext,
) -> Result<Option<BTreeMap<usize, Term>>, Term> {
    let atom_table = context.atom_table().ok_or_else(badarg)?;
    let mut bindings = BTreeMap::new();
    if match_pattern_term(pattern, object, atom_table, &mut bindings)? {
        Ok(Some(bindings))
    } else {
        Ok(None)
    }
}

fn match_pattern_term(
    pattern: Term,
    value: Term,
    atom_table: &AtomTable,
    bindings: &mut BTreeMap<usize, Term>,
) -> Result<bool, Term> {
    if ets_is_dont_care(pattern, atom_table) {
        return Ok(true);
    }
    if let Some(index) = ets_match_variable_index(pattern, atom_table) {
        return Ok(match bindings.get(&index).copied() {
            Some(bound) => compare::exact_eq(bound, value),
            None => {
                bindings.insert(index, value);
                true
            }
        });
    }
    if let Some(pattern_tuple) = Tuple::new(pattern) {
        let Some(value_tuple) = Tuple::new(value) else {
            return Ok(false);
        };
        if pattern_tuple.arity() != value_tuple.arity() {
            return Ok(false);
        }
        for index in 0..pattern_tuple.arity() {
            let pattern_element = pattern_tuple.get(index).ok_or_else(badarg)?;
            let value_element = value_tuple.get(index).ok_or_else(badarg)?;
            if !match_pattern_term(pattern_element, value_element, atom_table, bindings)? {
                return Ok(false);
            }
        }
        return Ok(true);
    }
    if pattern.is_list() || pattern.is_nil() {
        return match_list_pattern(pattern, value, atom_table, bindings);
    }
    Ok(compare::exact_eq(pattern, value))
}

fn match_list_pattern(
    pattern: Term,
    value: Term,
    atom_table: &AtomTable,
    bindings: &mut BTreeMap<usize, Term>,
) -> Result<bool, Term> {
    let mut pattern_tail = pattern;
    let mut value_tail = value;
    while !pattern_tail.is_nil() {
        let pattern_cons = Cons::new(pattern_tail).ok_or_else(badarg)?;
        let Some(value_cons) = Cons::new(value_tail) else {
            return Ok(false);
        };
        if !match_pattern_term(pattern_cons.head(), value_cons.head(), atom_table, bindings)? {
            return Ok(false);
        }
        pattern_tail = pattern_cons.tail();
        value_tail = value_cons.tail();
    }
    Ok(value_tail.is_nil())
}

fn ets_is_dont_care(term: Term, atom_table: &AtomTable) -> bool {
    term.as_atom()
        .and_then(|atom| atom_table.resolve(atom))
        .is_some_and(|name| name == "_")
}

fn ets_match_variable_index(term: Term, atom_table: &AtomTable) -> Option<usize> {
    let name = term.as_atom().and_then(|atom| atom_table.resolve(atom))?;
    let digits = name.strip_prefix('$')?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let index = digits.parse::<usize>().ok()?;
    (index > 0).then_some(index)
}

#[derive(Copy, Clone)]
enum CursorDirection {
    Next,
    Prev,
}

fn table_snapshot_keys(
    table: &Arc<dyn EtsTable>,
    context: &ProcessContext,
) -> Result<Vec<Term>, Term> {
    let mut keys = table
        .tab2list()
        .into_iter()
        .map(|tuple| {
            crate::ets::tuple_key(tuple, table.metadata().keypos).map_err(ets_error_to_badarg)
        })
        .collect::<Result<Vec<_>, _>>()?;

    if table.metadata().table_type == EtsTableType::OrderedSet {
        return Ok(keys);
    }

    let atom_table = context.atom_table_arc().ok_or_else(badarg)?;
    keys.sort_by(|left, right| {
        TermKey::with_atom_table(*left, Arc::clone(&atom_table))
            .cmp(&TermKey::with_atom_table(*right, Arc::clone(&atom_table)))
    });
    keys.dedup_by(|left, right| {
        TermKey::with_atom_table(*left, Arc::clone(&atom_table))
            == TermKey::with_atom_table(*right, Arc::clone(&atom_table))
    });
    Ok(keys)
}

fn cursor_neighbor(
    table: &Arc<dyn EtsTable>,
    keys: Vec<Term>,
    key: Term,
    direction: CursorDirection,
    context: &ProcessContext,
) -> Result<Term, Term> {
    if keys.is_empty() {
        return end_of_table_atom(context);
    }

    let atom_table = context.atom_table_arc().ok_or_else(badarg)?;
    if table.metadata().table_type == EtsTableType::OrderedSet {
        let cursor = TermKey::with_atom_table(key, Arc::clone(&atom_table));
        let position = keys.binary_search_by(|probe| {
            TermKey::with_atom_table(*probe, Arc::clone(&atom_table)).cmp(&cursor)
        });
        let neighbor = match (direction, position) {
            (CursorDirection::Next, Ok(index)) => keys.get(index + 1).copied(),
            (CursorDirection::Next, Err(index)) => keys.get(index).copied(),
            (CursorDirection::Prev, Ok(0) | Err(0)) => None,
            (CursorDirection::Prev, Ok(index) | Err(index)) => keys.get(index - 1).copied(),
        };
        return Ok(neighbor.unwrap_or(end_of_table_atom(context)?));
    }

    let position = keys.iter().position(|candidate| *candidate == key);
    let neighbor = match (direction, position) {
        (CursorDirection::Next, Some(index)) => keys.get(index + 1).copied(),
        (CursorDirection::Prev, Some(index)) if index > 0 => keys.get(index - 1).copied(),
        _ => None,
    };
    Ok(neighbor.unwrap_or(end_of_table_atom(context)?))
}

fn end_of_table_atom(context: &ProcessContext) -> Result<Term, Term> {
    let atom_table = context.atom_table().ok_or_else(badarg)?;
    Ok(Term::atom(atom_table.intern("$end_of_table")))
}

fn ets_table_id_from_i64(value: i64) -> Option<EtsTableId> {
    u64::try_from(value).ok().filter(|id| *id > 0)
}

fn info_value(
    table: &Arc<dyn EtsTable>,
    item: &str,
    context: &ProcessContext,
) -> Result<Term, Term> {
    let atom_table = context.atom_table().ok_or_else(badarg)?;
    let metadata = table.metadata();
    match item {
        "name" => Ok(metadata
            .name
            .map(Term::atom)
            .unwrap_or_else(|| Term::atom(Atom::UNDEFINED))),
        "type" => Ok(Term::atom(table_type_atom(metadata.table_type, atom_table))),
        "size" => small_int_from_usize(table.tab2list().len()),
        "keypos" => small_int_from_usize(metadata.keypos),
        "protection" => Ok(Term::atom(protection_atom(metadata.protection, atom_table))),
        "owner" => Term::try_pid(metadata.owner.get()).ok_or_else(badarg),
        "memory" => small_int_from_usize(approximate_memory_words(table)),
        _ => Err(badarg()),
    }
}

fn table_type_atom(table_type: EtsTableType, atom_table: &AtomTable) -> Atom {
    match table_type {
        EtsTableType::Set => atom_table.intern("set"),
        EtsTableType::OrderedSet => atom_table.intern("ordered_set"),
        EtsTableType::Bag => atom_table.intern("bag"),
        EtsTableType::DuplicateBag => atom_table.intern("duplicate_bag"),
    }
}

fn protection_atom(protection: Protection, atom_table: &AtomTable) -> Atom {
    match protection {
        Protection::Public => atom_table.intern("public"),
        Protection::Protected => atom_table.intern("protected"),
        Protection::Private => atom_table.intern("private"),
    }
}

fn approximate_memory_words(table: &Arc<dyn EtsTable>) -> usize {
    const METADATA_WORDS: usize = 8;
    let entries = table.tab2list();
    METADATA_WORDS + entries.len() * 3
}

fn small_int_from_u64(value: u64) -> Result<Term, Term> {
    let value = i64::try_from(value).map_err(|_| badarg())?;
    Term::try_small_int(value).ok_or_else(badarg)
}

fn small_int_from_usize(value: usize) -> Result<Term, Term> {
    let value = i64::try_from(value).map_err(|_| badarg())?;
    Term::try_small_int(value).ok_or_else(badarg)
}

fn bool_term(value: bool) -> Term {
    Term::atom(if value { Atom::TRUE } else { Atom::FALSE })
}

const fn list_heap_words(element_count: usize) -> usize {
    element_count * 2
}

const fn info_proplist_heap_words(item_count: usize) -> usize {
    item_count * 5
}

fn ets_error_to_badarg(_error: EtsError) -> Term {
    badarg()
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}

fn ensure_fun_arity(fun: Term, arity: u8) -> Result<(), Term> {
    let closure = Closure::new(fun).ok_or_else(badarg)?;
    if closure.arity() == arity {
        Ok(())
    } else {
        Err(Term::atom(Atom::BADARITY))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{
        bif_delete_1, bif_delete_2, bif_first, bif_foldl, bif_give_away, bif_info_2, bif_insert,
        bif_last, bif_lookup, bif_match_1, bif_match_2, bif_match_3, bif_match_delete,
        bif_match_object, bif_member, bif_new, bif_next, bif_prev, bif_select_1, bif_select_2,
        bif_select_3, bif_tab2list, register_ets_bifs, resume_ets_foldl,
    };
    use crate::atom::{Atom, AtomTable};
    use crate::ets::EtsRegistry;
    use crate::native::stdlib_stubs::maps_bifs::ContinuationStep;
    use crate::native::{BifRegistryImpl, NativeContinuation, ProcessContext};
    use crate::process::Process;
    use crate::term::Term;
    use crate::term::boxed::{Cons, Tuple, write_closure};

    fn context<'a>(
        process: &'a mut Process,
        atom_table: Arc<AtomTable>,
        registry: Arc<EtsRegistry>,
    ) -> ProcessContext<'a> {
        let ets_facility: Arc<dyn crate::native::EtsFacility> = registry;
        let mut context = ProcessContext::new();
        context.set_atom_table(Some(atom_table));
        context.set_ets_facility(Some(ets_facility));
        context.attach_process(process, 0);
        context
    }

    fn atom_list(context: &mut ProcessContext, atoms: &[Atom]) -> Term {
        let terms = atoms.iter().copied().map(Term::atom).collect::<Vec<_>>();
        context.alloc_list(&terms).expect("list allocation")
    }

    fn tuple(context: &mut ProcessContext, elements: &[Term]) -> Term {
        context.alloc_tuple(elements).expect("tuple allocation")
    }

    fn tuple_option(context: &mut ProcessContext, name: Atom, value: Term) -> Term {
        tuple(context, &[Term::atom(name), value])
    }

    fn tuple3(context: &mut ProcessContext, elements: &[Term; 3]) -> Term {
        tuple(context, elements)
    }

    fn table_id(tab: Term) -> u64 {
        tab.as_small_int()
            .and_then(|value| u64::try_from(value).ok())
            .expect("numeric table id")
    }

    fn list_terms(list: Term) -> Vec<Term> {
        let mut values = Vec::new();
        let mut tail = list;
        while !tail.is_nil() {
            let cons = Cons::new(tail).expect("proper list cons");
            values.push(cons.head());
            tail = cons.tail();
        }
        values
    }

    fn tuple_values(term: Term) -> Vec<Term> {
        let tuple = Tuple::new(term).expect("tuple");
        (0..tuple.arity())
            .map(|index| tuple.get(index).expect("tuple element"))
            .collect()
    }

    fn page_tuple(term: Term) -> (Term, Term) {
        let values = tuple_values(term);
        assert_eq!(values.len(), 2);
        (values[0], values[1])
    }

    fn new_table(
        context: &mut ProcessContext,
        _atom_table: &AtomTable,
        name: Atom,
        options: &[Atom],
    ) -> Term {
        let options = atom_list(context, options);
        bif_new(&[Term::atom(name), options], context).expect("ets:new succeeds")
    }

    fn created_table(registry: &EtsRegistry, tab: Term) -> Arc<dyn crate::ets::EtsTable> {
        let table_id = tab
            .as_small_int()
            .and_then(|value| u64::try_from(value).ok())
            .expect("unnamed table returns positive numeric id");
        registry.lookup_table(table_id).expect("table exists")
    }

    #[test]
    fn ets_new_named_public_set_returns_name_and_rejects_duplicate() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let registry = Arc::new(EtsRegistry::new());
        let my_table = atom_table.intern("my_table");
        let set = atom_table.intern("set");
        let public = atom_table.intern("public");
        let named_table = atom_table.intern("named_table");
        let mut process = Process::new(1, 128);
        let mut context = context(&mut process, Arc::clone(&atom_table), registry);

        let result = new_table(
            &mut context,
            &atom_table,
            my_table,
            &[set, public, named_table],
        );

        assert_eq!(result, Term::atom(my_table));
        let duplicate_options = atom_list(&mut context, &[set, public, named_table]);
        assert_eq!(
            bif_new(&[Term::atom(my_table), duplicate_options], &mut context),
            Err(Term::atom(Atom::BADARG))
        );
    }

    #[test]
    fn give_away_transfers_owner_in_registry_facility_and_enforces_owner_only() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let registry = Arc::new(EtsRegistry::new());
        let private = atom_table.intern("private");
        let set = atom_table.intern("set");
        let mut owner_process = Process::new(1, 512);
        let mut owner_context = context(
            &mut owner_process,
            Arc::clone(&atom_table),
            Arc::clone(&registry),
        );
        let tab = new_table(
            &mut owner_context,
            &atom_table,
            atom_table.intern("give_away_tab"),
            &[set, private],
        );

        assert_eq!(
            bif_give_away(
                &[tab, Term::pid(2), Term::atom(Atom::OK)],
                &mut owner_context
            ),
            Ok(Term::atom(Atom::TRUE))
        );
        assert_eq!(created_table(&registry, tab).metadata().owner.get(), 2);
        let row = tuple(
            &mut owner_context,
            &[Term::atom(Atom::OK), Term::small_int(1)],
        );
        assert_eq!(
            bif_insert(&[tab, row], &mut owner_context),
            Err(Term::atom(Atom::BADARG))
        );

        let mut non_owner_process = Process::new(3, 256);
        let mut non_owner_context =
            context(&mut non_owner_process, Arc::clone(&atom_table), registry);
        assert_eq!(
            bif_give_away(&[tab, Term::pid(4), Term::NIL], &mut non_owner_context),
            Err(Term::atom(Atom::BADARG))
        );
    }

    #[test]
    fn ets_new_unnamed_table_returns_numeric_id() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let registry = Arc::new(EtsRegistry::new());
        let table_name = atom_table.intern("unnamed_source_name");
        let mut process = Process::new(1, 128);
        let mut context = context(&mut process, Arc::clone(&atom_table), registry);
        let options = context.alloc_list(&[]).expect("empty option list");

        let result = bif_new(&[Term::atom(table_name), options], &mut context).expect("new table");

        assert_eq!(result.as_small_int(), Some(1));
    }

    #[test]
    fn ets_new_defaults_concurrency_options_to_false() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let registry = Arc::new(EtsRegistry::new());
        let table_name = atom_table.intern("default_concurrency_tab");
        let mut process = Process::new(1, 128);
        let mut context = context(&mut process, Arc::clone(&atom_table), Arc::clone(&registry));
        let options = context.alloc_list(&[]).expect("empty option list");

        let tab = bif_new(&[Term::atom(table_name), options], &mut context).expect("new table");
        let table = created_table(&registry, tab);

        assert!(!table.metadata().read_concurrency);
        assert!(!table.metadata().write_concurrency);
    }

    #[test]
    fn ets_new_parses_heir_pid_data_and_explicit_none() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let registry = Arc::new(EtsRegistry::new());
        let table_name = atom_table.intern("heir_tab");
        let none_table_name = atom_table.intern("heir_none_tab");
        let heir = atom_table.intern("heir");
        let none = atom_table.intern("none");
        let payload = atom_table.intern("payload");
        let mut process = Process::new(1, 256);
        let mut context = context(&mut process, Arc::clone(&atom_table), Arc::clone(&registry));
        let data = tuple(&mut context, &[Term::atom(payload), Term::small_int(42)]);
        let heir_option = tuple3(&mut context, &[Term::atom(heir), Term::pid(99), data]);
        let options = context.alloc_list(&[heir_option]).expect("option list");

        let tab = bif_new(&[Term::atom(table_name), options], &mut context).expect("new table");
        let table = created_table(&registry, tab);
        let metadata_heir = table.metadata().heir.as_ref().expect("heir stored");
        assert_eq!(metadata_heir.pid, 99);
        assert_ne!(metadata_heir.data.root(), data);

        let none_option = tuple_option(&mut context, heir, Term::atom(none));
        let none_options = context.alloc_list(&[none_option]).expect("option list");
        let none_tab =
            bif_new(&[Term::atom(none_table_name), none_options], &mut context).expect("new table");
        assert!(created_table(&registry, none_tab).metadata().heir.is_none());
    }

    #[test]
    fn ets_new_parses_read_concurrency_option() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let registry = Arc::new(EtsRegistry::new());
        let table_name = atom_table.intern("read_concurrency_tab");
        let read_concurrency = atom_table.intern("read_concurrency");
        let mut process = Process::new(1, 128);
        let mut context = context(&mut process, Arc::clone(&atom_table), Arc::clone(&registry));
        let option = tuple_option(&mut context, read_concurrency, Term::atom(Atom::TRUE));
        let options = context.alloc_list(&[option]).expect("option list");

        let tab = bif_new(&[Term::atom(table_name), options], &mut context).expect("new table");
        let table = created_table(&registry, tab);

        assert!(table.metadata().read_concurrency);
        assert!(!table.metadata().write_concurrency);
    }

    #[test]
    fn ets_new_parses_write_concurrency_option() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let registry = Arc::new(EtsRegistry::new());
        let table_name = atom_table.intern("write_concurrency_tab");
        let write_concurrency = atom_table.intern("write_concurrency");
        let mut process = Process::new(1, 128);
        let mut context = context(&mut process, Arc::clone(&atom_table), Arc::clone(&registry));
        let option = tuple_option(&mut context, write_concurrency, Term::atom(Atom::TRUE));
        let options = context.alloc_list(&[option]).expect("option list");

        let tab = bif_new(&[Term::atom(table_name), options], &mut context).expect("new table");
        let table = created_table(&registry, tab);

        assert!(!table.metadata().read_concurrency);
        assert!(table.metadata().write_concurrency);
    }

    #[test]
    fn ets_new_parses_read_and_write_concurrency_together() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let registry = Arc::new(EtsRegistry::new());
        let table_name = atom_table.intern("both_concurrency_tab");
        let ordered_set = atom_table.intern("ordered_set");
        let read_concurrency = atom_table.intern("read_concurrency");
        let write_concurrency = atom_table.intern("write_concurrency");
        let mut process = Process::new(1, 256);
        let mut context = context(&mut process, Arc::clone(&atom_table), Arc::clone(&registry));
        let read_option = tuple_option(&mut context, read_concurrency, Term::atom(Atom::TRUE));
        let write_option = tuple_option(&mut context, write_concurrency, Term::atom(Atom::TRUE));
        let options = context
            .alloc_list(&[Term::atom(ordered_set), read_option, write_option])
            .expect("option list");

        let tab = bif_new(&[Term::atom(table_name), options], &mut context).expect("new table");
        let table = created_table(&registry, tab);

        assert_eq!(
            table.metadata().table_type,
            crate::ets::EtsTableType::OrderedSet
        );
        assert!(table.metadata().read_concurrency);
        assert!(table.metadata().write_concurrency);
    }

    #[test]
    fn ets_new_accepts_explicit_false_concurrency_options() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let registry = Arc::new(EtsRegistry::new());
        let table_name = atom_table.intern("false_concurrency_tab");
        let read_concurrency = atom_table.intern("read_concurrency");
        let write_concurrency = atom_table.intern("write_concurrency");
        let mut process = Process::new(1, 256);
        let mut context = context(&mut process, Arc::clone(&atom_table), Arc::clone(&registry));
        let read_option = tuple_option(&mut context, read_concurrency, Term::atom(Atom::FALSE));
        let write_option = tuple_option(&mut context, write_concurrency, Term::atom(Atom::FALSE));
        let options = context
            .alloc_list(&[read_option, write_option])
            .expect("option list");

        let tab = bif_new(&[Term::atom(table_name), options], &mut context).expect("new table");
        let table = created_table(&registry, tab);

        assert!(!table.metadata().read_concurrency);
        assert!(!table.metadata().write_concurrency);
    }

    #[test]
    fn ets_new_rejects_non_boolean_concurrency_options() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let registry = Arc::new(EtsRegistry::new());
        let write_table_name = atom_table.intern("auto_write_concurrency_tab");
        let read_table_name = atom_table.intern("auto_read_concurrency_tab");
        let write_concurrency = atom_table.intern("write_concurrency");
        let read_concurrency = atom_table.intern("read_concurrency");
        let auto = atom_table.intern("auto");
        let mut process = Process::new(1, 256);
        let mut context = context(&mut process, Arc::clone(&atom_table), registry);
        let write_option = tuple_option(&mut context, write_concurrency, Term::atom(auto));
        let write_options = context.alloc_list(&[write_option]).expect("option list");

        assert_eq!(
            bif_new(&[Term::atom(write_table_name), write_options], &mut context),
            Err(Term::atom(Atom::BADARG))
        );

        let read_option = tuple_option(&mut context, read_concurrency, Term::small_int(1));
        let read_options = context.alloc_list(&[read_option]).expect("option list");
        assert_eq!(
            bif_new(&[Term::atom(read_table_name), read_options], &mut context),
            Err(Term::atom(Atom::BADARG))
        );
    }

    #[test]
    fn insert_and_lookup_round_trip_for_set_bag_and_ordered_set() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let registry = Arc::new(EtsRegistry::new());
        let public = atom_table.intern("public");
        let set = atom_table.intern("set");
        let bag = atom_table.intern("bag");
        let ordered_set = atom_table.intern("ordered_set");
        let mut process = Process::new(1, 512);
        let mut context = context(&mut process, Arc::clone(&atom_table), registry);

        for (index, table_type) in [set, bag, ordered_set].into_iter().enumerate() {
            let table_name = atom_table.intern(&format!("table_{index}"));
            let tab = new_table(&mut context, &atom_table, table_name, &[table_type, public]);
            let tuple = tuple(
                &mut context,
                &[Term::atom(Atom::OK), Term::small_int(index as i64)],
            );

            assert_eq!(
                bif_insert(&[tab, tuple], &mut context),
                Ok(Term::atom(Atom::TRUE))
            );
            let result = bif_lookup(&[tab, Term::atom(Atom::OK)], &mut context).expect("lookup");
            assert_eq!(list_terms(result), vec![tuple]);
        }
    }

    #[test]
    fn insert_accepts_proper_list_of_tuples() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let registry = Arc::new(EtsRegistry::new());
        let public = atom_table.intern("public");
        let bag = atom_table.intern("bag");
        let mut process = Process::new(1, 512);
        let mut context = context(&mut process, Arc::clone(&atom_table), registry);
        let tab = new_table(
            &mut context,
            &atom_table,
            atom_table.intern("bag_tab"),
            &[bag, public],
        );
        let first = tuple(&mut context, &[Term::atom(Atom::OK), Term::small_int(1)]);
        let second = tuple(&mut context, &[Term::atom(Atom::OK), Term::small_int(2)]);
        let objects = context.alloc_list(&[first, second]).expect("tuple list");

        assert_eq!(
            bif_insert(&[tab, objects], &mut context),
            Ok(Term::atom(Atom::TRUE))
        );
        let result = bif_lookup(&[tab, Term::atom(Atom::OK)], &mut context).expect("lookup");
        let mut values = list_terms(result);
        values.sort();
        let mut expected = vec![first, second];
        expected.sort();
        assert_eq!(values, expected);
    }

    #[test]
    fn tab2list_returns_all_inserted_tuples() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let registry = Arc::new(EtsRegistry::new());
        let public = atom_table.intern("public");
        let set = atom_table.intern("set");
        let mut process = Process::new(1, 512);
        let mut context = context(&mut process, Arc::clone(&atom_table), registry);
        let tab = new_table(
            &mut context,
            &atom_table,
            atom_table.intern("tab2list_tab"),
            &[set, public],
        );
        let first = tuple(&mut context, &[Term::small_int(1), Term::small_int(10)]);
        let second = tuple(&mut context, &[Term::small_int(2), Term::small_int(20)]);
        let third = tuple(&mut context, &[Term::small_int(3), Term::small_int(30)]);
        let objects = context
            .alloc_list(&[first, second, third])
            .expect("tuple list");

        assert_eq!(
            bif_insert(&[tab, objects], &mut context),
            Ok(Term::atom(Atom::TRUE))
        );
        let result = bif_tab2list(&[tab], &mut context).expect("tab2list");
        let mut values = list_terms(result);
        values.sort();
        let mut expected = vec![first, second, third];
        expected.sort();
        assert_eq!(values, expected);
    }

    #[test]
    fn tab2list_empty_table_returns_nil_and_ordered_set_is_key_sorted() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let registry = Arc::new(EtsRegistry::new());
        let public = atom_table.intern("public");
        let ordered_set = atom_table.intern("ordered_set");
        let mut process = Process::new(1, 512);
        let mut context = context(&mut process, Arc::clone(&atom_table), registry);
        let tab = new_table(
            &mut context,
            &atom_table,
            atom_table.intern("tab2list_ordered_tab"),
            &[ordered_set, public],
        );

        assert_eq!(bif_tab2list(&[tab], &mut context), Ok(Term::NIL));

        for (key, value) in [(3, 30), (1, 10), (2, 20)] {
            let row = tuple(
                &mut context,
                &[Term::small_int(key), Term::small_int(value)],
            );
            assert_eq!(
                bif_insert(&[tab, row], &mut context),
                Ok(Term::atom(Atom::TRUE))
            );
        }

        let result = bif_tab2list(&[tab], &mut context).expect("tab2list");
        let keys = list_terms(result)
            .into_iter()
            .map(|row| {
                Tuple::new(row)
                    .and_then(|tuple| tuple.get(0))
                    .expect("tuple key")
            })
            .collect::<Vec<_>>();
        assert_eq!(
            keys,
            vec![Term::small_int(1), Term::small_int(2), Term::small_int(3)]
        );
    }

    #[test]
    fn foldl_queues_continuation_over_table_entries() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let registry = Arc::new(EtsRegistry::new());
        let public = atom_table.intern("public");
        let set = atom_table.intern("set");
        let mut process = Process::new(1, 512);
        let mut context = context(&mut process, Arc::clone(&atom_table), registry);
        let tab = new_table(
            &mut context,
            &atom_table,
            atom_table.intern("foldl_tab"),
            &[set, public],
        );
        let first = tuple(&mut context, &[Term::small_int(1), Term::small_int(10)]);
        let second = tuple(&mut context, &[Term::small_int(2), Term::small_int(20)]);
        let objects = context.alloc_list(&[first, second]).expect("tuple list");
        let mut closure_heap = [0_u64; 7];
        let fun = write_closure(&mut closure_heap, Atom::OK, 0, 2, 1, 0, &[]).expect("closure");

        assert_eq!(
            bif_insert(&[tab, objects], &mut context),
            Ok(Term::atom(Atom::TRUE))
        );
        assert_eq!(
            bif_foldl(&[fun, Term::small_int(0), tab], &mut context),
            Ok(Term::NIL)
        );
        let trampoline = context.take_trampoline().expect("foldl trampoline");
        assert_eq!(trampoline.fun, fun);
        assert_eq!(trampoline.args.len(), 2);
        assert_eq!(trampoline.args[1], Term::small_int(0));
        assert!(matches!(
            trampoline.continuation,
            Some(NativeContinuation::EtsFoldl(_))
        ));
    }

    #[test]
    fn foldl_empty_table_returns_accumulator_and_rejects_wrong_fun_arity() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let registry = Arc::new(EtsRegistry::new());
        let public = atom_table.intern("public");
        let set = atom_table.intern("set");
        let mut process = Process::new(1, 256);
        let mut context = context(&mut process, Arc::clone(&atom_table), registry);
        let tab = new_table(
            &mut context,
            &atom_table,
            atom_table.intern("foldl_empty_tab"),
            &[set, public],
        );
        let mut unary_closure_heap = [0_u64; 7];
        let unary_fun =
            write_closure(&mut unary_closure_heap, Atom::OK, 0, 1, 1, 0, &[]).expect("closure");
        let mut binary_closure_heap = [0_u64; 7];
        let binary_fun =
            write_closure(&mut binary_closure_heap, Atom::OK, 0, 2, 1, 0, &[]).expect("closure");

        assert_eq!(
            bif_foldl(&[binary_fun, Term::small_int(42), tab], &mut context),
            Ok(Term::small_int(42))
        );
        assert!(context.take_trampoline().is_none());
        assert_eq!(
            bif_foldl(&[unary_fun, Term::small_int(0), tab], &mut context),
            Err(Term::atom(Atom::BADARITY))
        );
    }

    #[test]
    fn foldl_resume_passes_element_and_accumulator_until_done() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let registry = Arc::new(EtsRegistry::new());
        let public = atom_table.intern("public");
        let ordered_set = atom_table.intern("ordered_set");
        let mut process = Process::new(1, 512);
        let mut context = context(&mut process, Arc::clone(&atom_table), registry);
        let tab = new_table(
            &mut context,
            &atom_table,
            atom_table.intern("foldl_resume_tab"),
            &[ordered_set, public],
        );
        let first = tuple(&mut context, &[Term::small_int(1), Term::small_int(10)]);
        let second = tuple(&mut context, &[Term::small_int(2), Term::small_int(20)]);
        let objects = context.alloc_list(&[second, first]).expect("tuple list");
        let mut closure_heap = [0_u64; 7];
        let fun = write_closure(&mut closure_heap, Atom::OK, 0, 2, 1, 0, &[]).expect("closure");

        assert_eq!(
            bif_insert(&[tab, objects], &mut context),
            Ok(Term::atom(Atom::TRUE))
        );
        assert_eq!(
            bif_foldl(&[fun, Term::small_int(0), tab], &mut context),
            Ok(Term::NIL)
        );
        let trampoline = context.take_trampoline().expect("foldl trampoline");
        assert_eq!(trampoline.args, vec![first, Term::small_int(0)]);
        let Some(NativeContinuation::EtsFoldl(state)) = trampoline.continuation else {
            panic!("expected foldl continuation");
        };

        let step = resume_ets_foldl(state, Term::small_int(10)).expect("resume step");
        let ContinuationStep::Call {
            fun: step_fun,
            args,
            continuation,
        } = step
        else {
            panic!("expected second foldl call");
        };
        assert_eq!(step_fun, fun);
        assert_eq!(args, vec![second, Term::small_int(10)]);

        let NativeContinuation::EtsFoldl(state) = continuation else {
            panic!("expected foldl continuation");
        };
        let final_step = resume_ets_foldl(state, Term::small_int(30)).expect("final resume");
        let ContinuationStep::Done(result) = final_step else {
            panic!("expected foldl completion");
        };
        assert_eq!(result, Term::small_int(30));
    }

    #[test]
    fn ordered_set_cursor_traversal_visits_keys_in_order() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let registry = Arc::new(EtsRegistry::new());
        let public = atom_table.intern("public");
        let ordered_set = atom_table.intern("ordered_set");
        let end_of_table = atom_table.intern("$end_of_table");
        let mut process = Process::new(1, 512);
        let mut context = context(&mut process, Arc::clone(&atom_table), registry);
        let tab = new_table(
            &mut context,
            &atom_table,
            atom_table.intern("ordered_cursor_tab"),
            &[ordered_set, public],
        );
        for key in [Term::small_int(2), Term::small_int(1), Term::small_int(3)] {
            let row = tuple(&mut context, &[key, key]);
            assert_eq!(
                bif_insert(&[tab, row], &mut context),
                Ok(Term::atom(Atom::TRUE))
            );
        }

        assert_eq!(bif_first(&[tab], &mut context), Ok(Term::small_int(1)));
        assert_eq!(
            bif_next(&[tab, Term::small_int(1)], &mut context),
            Ok(Term::small_int(2))
        );
        assert_eq!(
            bif_next(&[tab, Term::small_int(2)], &mut context),
            Ok(Term::small_int(3))
        );
        assert_eq!(
            bif_next(&[tab, Term::small_int(3)], &mut context),
            Ok(Term::atom(end_of_table))
        );
        assert_eq!(bif_last(&[tab], &mut context), Ok(Term::small_int(3)));
        assert_eq!(
            bif_prev(&[tab, Term::small_int(3)], &mut context),
            Ok(Term::small_int(2))
        );
        assert_eq!(
            bif_next(&[tab, Term::small_int(0)], &mut context),
            Ok(Term::small_int(1))
        );
        assert_eq!(
            bif_next(&[tab, Term::small_int(4)], &mut context),
            Ok(Term::atom(end_of_table))
        );
        assert_eq!(
            bif_prev(&[tab, Term::small_int(4)], &mut context),
            Ok(Term::small_int(3))
        );
        assert_eq!(
            bif_prev(&[tab, Term::small_int(1)], &mut context),
            Ok(Term::atom(end_of_table))
        );
    }

    #[test]
    fn set_cursor_traversal_visits_all_keys_once_and_empty_tables_end() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let registry = Arc::new(EtsRegistry::new());
        let public = atom_table.intern("public");
        let set = atom_table.intern("set");
        let end_of_table = atom_table.intern("$end_of_table");
        let mut process = Process::new(1, 512);
        let mut context = context(&mut process, Arc::clone(&atom_table), registry);
        let tab = new_table(
            &mut context,
            &atom_table,
            atom_table.intern("set_cursor_tab"),
            &[set, public],
        );

        assert_eq!(
            bif_first(&[tab], &mut context),
            Ok(Term::atom(end_of_table))
        );
        assert_eq!(bif_last(&[tab], &mut context), Ok(Term::atom(end_of_table)));

        for key in [Term::small_int(2), Term::small_int(1), Term::small_int(3)] {
            let row = tuple(&mut context, &[key, key]);
            assert_eq!(
                bif_insert(&[tab, row], &mut context),
                Ok(Term::atom(Atom::TRUE))
            );
        }

        let mut visited = Vec::new();
        let mut cursor = bif_first(&[tab], &mut context).expect("first key");
        while cursor != Term::atom(end_of_table) {
            visited.push(cursor);
            cursor = bif_next(&[tab, cursor], &mut context).expect("next key");
        }
        visited.sort();
        assert_eq!(
            visited,
            vec![Term::small_int(1), Term::small_int(2), Term::small_int(3)]
        );
    }

    #[test]
    fn match_returns_bound_variable_lists_and_match_object_returns_tuples() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let registry = Arc::new(EtsRegistry::new());
        let public = atom_table.intern("public");
        let ordered_set = atom_table.intern("ordered_set");
        let wildcard = atom_table.intern("_");
        let variable = atom_table.intern("$1");
        let a = atom_table.intern("a");
        let b = atom_table.intern("b");
        let c = atom_table.intern("c");
        let mut process = Process::new(1, 1024);
        let mut context = context(&mut process, Arc::clone(&atom_table), registry);
        let tab = new_table(
            &mut context,
            &atom_table,
            atom_table.intern("match_tab"),
            &[ordered_set, public],
        );
        let first = tuple(&mut context, &[Term::atom(a), Term::small_int(1)]);
        let second = tuple(&mut context, &[Term::atom(b), Term::small_int(2)]);
        let third = tuple(&mut context, &[Term::atom(c), Term::small_int(3)]);
        let objects = context
            .alloc_list(&[first, second, third])
            .expect("objects");
        let pattern = tuple(&mut context, &[Term::atom(wildcard), Term::atom(variable)]);

        assert_eq!(
            bif_insert(&[tab, objects], &mut context),
            Ok(Term::atom(Atom::TRUE))
        );
        let result = bif_match_2(&[tab, pattern], &mut context).expect("match/2");
        let bound_lists = list_terms(result)
            .into_iter()
            .map(list_terms)
            .collect::<Vec<_>>();
        assert_eq!(
            bound_lists,
            vec![
                vec![Term::small_int(1)],
                vec![Term::small_int(2)],
                vec![Term::small_int(3)],
            ]
        );

        let matched_objects =
            bif_match_object(&[tab, pattern], &mut context).expect("match_object/2");
        assert_eq!(list_terms(matched_objects), vec![first, second, third]);
    }

    #[test]
    fn select_runs_match_spec_with_guard() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let registry = Arc::new(EtsRegistry::new());
        let public = atom_table.intern("public");
        let ordered_set = atom_table.intern("ordered_set");
        let gt = atom_table.intern(">");
        let first_var = atom_table.intern("$1");
        let second_var = atom_table.intern("$2");
        let a = atom_table.intern("a");
        let b = atom_table.intern("b");
        let c = atom_table.intern("c");
        let mut process = Process::new(1, 2048);
        let mut context = context(&mut process, Arc::clone(&atom_table), registry);
        let tab = new_table(
            &mut context,
            &atom_table,
            atom_table.intern("select_tab"),
            &[ordered_set, public],
        );
        for (key, value) in [(1, a), (2, b), (3, c)] {
            let row = tuple(&mut context, &[Term::small_int(key), Term::atom(value)]);
            assert_eq!(
                bif_insert(&[tab, row], &mut context),
                Ok(Term::atom(Atom::TRUE))
            );
        }
        let head = tuple(
            &mut context,
            &[Term::atom(first_var), Term::atom(second_var)],
        );
        let guard = tuple(
            &mut context,
            &[Term::atom(gt), Term::atom(first_var), Term::small_int(1)],
        );
        let guards = context.alloc_list(&[guard]).expect("guards");
        let body = context.alloc_list(&[Term::atom(second_var)]).expect("body");
        let clause = tuple(&mut context, &[head, guards, body]);
        let spec = context.alloc_list(&[clause]).expect("spec");

        let result = bif_select_2(&[tab, spec], &mut context).expect("select/2");
        assert_eq!(list_terms(result), vec![Term::atom(b), Term::atom(c)]);
    }

    #[test]
    fn match_and_select_pagination_consumes_all_rows() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let registry = Arc::new(EtsRegistry::new());
        let public = atom_table.intern("public");
        let ordered_set = atom_table.intern("ordered_set");
        let wildcard = atom_table.intern("_");
        let first_var = atom_table.intern("$1");
        let second_var = atom_table.intern("$2");
        let end_of_table = atom_table.intern("$end_of_table");
        let mut process = Process::new(1, 20000);
        let mut context = context(&mut process, Arc::clone(&atom_table), registry);
        let tab = new_table(
            &mut context,
            &atom_table,
            atom_table.intern("page_tab"),
            &[ordered_set, public],
        );
        for key in 1..=100 {
            let row = tuple(
                &mut context,
                &[Term::small_int(key), Term::small_int(key * 10)],
            );
            assert_eq!(
                bif_insert(&[tab, row], &mut context),
                Ok(Term::atom(Atom::TRUE))
            );
        }

        let match_pattern = tuple(
            &mut context,
            &[Term::atom(wildcard), Term::atom(second_var)],
        );
        let mut match_count = 0;
        let mut match_page =
            bif_match_3(&[tab, match_pattern, Term::small_int(10)], &mut context).expect("match/3");
        loop {
            let (results, continuation) = page_tuple(match_page);
            match_count += list_terms(results).len();
            if continuation == Term::atom(end_of_table) {
                break;
            }
            match_page = bif_match_1(&[continuation], &mut context).expect("match/1");
        }
        assert_eq!(match_count, 100);

        let head = tuple(
            &mut context,
            &[Term::atom(first_var), Term::atom(second_var)],
        );
        let guards = context.alloc_list(&[]).expect("empty guards");
        let body = context.alloc_list(&[Term::atom(second_var)]).expect("body");
        let clause = tuple(&mut context, &[head, guards, body]);
        let spec = context.alloc_list(&[clause]).expect("spec");
        let mut select_count = 0;
        let mut select_page =
            bif_select_3(&[tab, spec, Term::small_int(10)], &mut context).expect("select/3");
        loop {
            let (results, continuation) = page_tuple(select_page);
            select_count += list_terms(results).len();
            if continuation == Term::atom(end_of_table) {
                break;
            }
            select_page = bif_select_1(&[continuation], &mut context).expect("select/1");
        }
        assert_eq!(select_count, 100);
    }

    #[test]
    fn match_delete_removes_only_matching_bag_objects() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let registry = Arc::new(EtsRegistry::new());
        let public = atom_table.intern("public");
        let duplicate_bag = atom_table.intern("duplicate_bag");
        let wildcard = atom_table.intern("_");
        let ok = atom_table.intern("ok");
        let mut process = Process::new(1, 1024);
        let mut context = context(&mut process, Arc::clone(&atom_table), registry);
        let tab = new_table(
            &mut context,
            &atom_table,
            atom_table.intern("match_delete_tab"),
            &[duplicate_bag, public],
        );
        let first = tuple(&mut context, &[Term::atom(ok), Term::small_int(1)]);
        let second = tuple(&mut context, &[Term::atom(ok), Term::small_int(2)]);
        let duplicate = tuple(&mut context, &[Term::atom(ok), Term::small_int(1)]);
        let objects = context
            .alloc_list(&[first, second, duplicate])
            .expect("objects");
        let pattern = tuple(&mut context, &[Term::atom(wildcard), Term::small_int(1)]);

        assert_eq!(
            bif_insert(&[tab, objects], &mut context),
            Ok(Term::atom(Atom::TRUE))
        );
        assert_eq!(
            bif_match_delete(&[tab, pattern], &mut context),
            Ok(Term::atom(Atom::TRUE))
        );
        let remaining = bif_lookup(&[tab, Term::atom(ok)], &mut context).expect("lookup");
        assert_eq!(list_terms(remaining), vec![second]);
    }

    #[test]
    fn delete_key_and_member_round_trip() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let registry = Arc::new(EtsRegistry::new());
        let public = atom_table.intern("public");
        let set = atom_table.intern("set");
        let mut process = Process::new(1, 512);
        let mut context = context(&mut process, Arc::clone(&atom_table), registry);
        let tab = new_table(
            &mut context,
            &atom_table,
            atom_table.intern("member_tab"),
            &[set, public],
        );
        let tuple = tuple(&mut context, &[Term::atom(Atom::OK), Term::small_int(1)]);

        assert_eq!(
            bif_insert(&[tab, tuple], &mut context),
            Ok(Term::atom(Atom::TRUE))
        );
        assert_eq!(
            bif_member(&[tab, Term::atom(Atom::OK)], &mut context),
            Ok(Term::atom(Atom::TRUE))
        );
        assert_eq!(
            bif_delete_2(&[tab, Term::atom(Atom::OK)], &mut context),
            Ok(Term::atom(Atom::TRUE))
        );
        assert_eq!(
            bif_member(&[tab, Term::atom(Atom::OK)], &mut context),
            Ok(Term::atom(Atom::FALSE))
        );
    }

    #[test]
    fn delete_one_removes_named_table() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let registry = Arc::new(EtsRegistry::new());
        let public = atom_table.intern("public");
        let named_table = atom_table.intern("named_table");
        let table_name = atom_table.intern("delete_named_tab");
        let mut process = Process::new(1, 256);
        let mut context = context(&mut process, Arc::clone(&atom_table), registry);
        let tab = new_table(
            &mut context,
            &atom_table,
            table_name,
            &[public, named_table],
        );

        assert_eq!(
            bif_delete_1(&[tab], &mut context),
            Ok(Term::atom(Atom::TRUE))
        );
        assert_eq!(
            bif_info_2(&[tab, Term::atom(atom_table.intern("size"))], &mut context),
            Ok(Term::atom(Atom::UNDEFINED))
        );
    }

    #[test]
    fn info_size_matches_inserted_entries() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let registry = Arc::new(EtsRegistry::new());
        let public = atom_table.intern("public");
        let bag = atom_table.intern("bag");
        let size = atom_table.intern("size");
        let mut process = Process::new(1, 512);
        let mut context = context(&mut process, Arc::clone(&atom_table), registry);
        let tab = new_table(
            &mut context,
            &atom_table,
            atom_table.intern("info_tab"),
            &[bag, public],
        );
        let first = tuple(&mut context, &[Term::atom(Atom::OK), Term::small_int(1)]);
        let second = tuple(&mut context, &[Term::atom(Atom::ERROR), Term::small_int(2)]);
        let objects = context.alloc_list(&[first, second]).expect("tuple list");

        assert_eq!(
            bif_insert(&[tab, objects], &mut context),
            Ok(Term::atom(Atom::TRUE))
        );
        assert_eq!(
            bif_info_2(&[tab, Term::atom(size)], &mut context),
            Ok(Term::small_int(2))
        );
    }

    #[test]
    fn info_one_returns_metadata_proplist() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let registry = Arc::new(EtsRegistry::new());
        let public = atom_table.intern("public");
        let named_table = atom_table.intern("named_table");
        let table_name = atom_table.intern("info_named_tab");
        let mut process = Process::new(1, 512);
        let mut context = context(&mut process, Arc::clone(&atom_table), registry);
        let tab = new_table(
            &mut context,
            &atom_table,
            table_name,
            &[public, named_table],
        );

        let proplist = super::bif_info_1(&[tab], &mut context).expect("info/1");
        let entries = list_terms(proplist);
        assert_eq!(entries.len(), 7);
        let first_entry = Tuple::new(entries[0]).expect("info tuple");
        assert_eq!(first_entry.arity(), 2);
    }

    #[test]
    fn ets_bifs_are_registered_under_ets_module() {
        let atom_table = AtomTable::with_common_atoms();
        let registry = BifRegistryImpl::new();
        register_ets_bifs(&registry, &atom_table).expect("register ets bifs");
        let ets = atom_table.intern("ets");

        for (name, arity) in [
            ("new", 2),
            ("insert", 2),
            ("lookup", 2),
            ("tab2list", 1),
            ("foldl", 3),
            ("match", 1),
            ("match", 2),
            ("match", 3),
            ("match_object", 2),
            ("match_delete", 2),
            ("select", 1),
            ("select", 2),
            ("select", 3),
            ("delete", 1),
            ("delete", 2),
            ("give_away", 3),
            ("member", 2),
            ("first", 1),
            ("next", 2),
            ("last", 1),
            ("prev", 2),
            ("info", 1),
            ("info", 2),
        ] {
            assert!(
                registry
                    .lookup(ets, atom_table.intern(name), arity)
                    .is_some(),
                "missing ets:{name}/{arity}"
            );
        }
    }
}
