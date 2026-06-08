//! Erlang Term Storage BIFs.
//!
//! These BIFs expose the core ETS table lifecycle and lookup operations through
//! the normal native-function registry under the `ets` module.

use std::sync::Arc;

use crate::atom::{Atom, AtomTable};
use crate::ets::{
    AccessOp, EtsError, EtsRegistry, EtsTable, EtsTableId, EtsTableMetadata, EtsTableType,
    Protection,
};
use crate::native::{
    BifRegistryImpl, Capability, NativeFn, NativeRegistrationError, ProcessContext,
};
use crate::term::Term;
use crate::term::boxed::{Cons, Tuple};

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
}

type EtsBif = (&'static str, u8, NativeFn);

const ETS_BIFS: &[EtsBif] = &[
    ("new", 2, bif_new),
    ("insert", 2, bif_insert),
    ("lookup", 2, bif_lookup),
    ("delete", 1, bif_delete_1),
    ("delete", 2, bif_delete_2),
    ("member", 2, bif_member),
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

#[derive(Copy, Clone)]
struct NewOptions {
    table_type: EtsTableType,
    protection: Protection,
    named_table: bool,
    keypos: usize,
    read_concurrency: bool,
    write_concurrency: bool,
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
        owner,
        keypos: options.keypos,
        read_concurrency: options.read_concurrency,
        write_concurrency: options.write_concurrency,
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

/// ets:delete/1 — delete an entire table. Only the owner may delete it.
pub fn bif_delete_1(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [tab] = args else {
        return Err(badarg());
    };
    let table = resolve_existing_table(*tab, context, MissingTable::Badarg)?;
    let caller = context.pid().ok_or_else(badarg)?;
    if table.metadata().owner != caller {
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
            if tuple.arity() != 2 {
                return Err(badarg());
            }
            let option_name = tuple.get(0).ok_or_else(badarg)?;
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
        "owner" => Term::try_pid(metadata.owner).ok_or_else(badarg),
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{
        bif_delete_1, bif_delete_2, bif_info_2, bif_insert, bif_lookup, bif_member, bif_new,
        register_ets_bifs,
    };
    use crate::atom::{Atom, AtomTable};
    use crate::ets::EtsRegistry;
    use crate::native::{BifRegistryImpl, ProcessContext};
    use crate::process::Process;
    use crate::term::Term;
    use crate::term::boxed::{Cons, Tuple};

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
    fn ets_new_rejects_write_concurrency_auto() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let registry = Arc::new(EtsRegistry::new());
        let table_name = atom_table.intern("auto_concurrency_tab");
        let write_concurrency = atom_table.intern("write_concurrency");
        let auto = atom_table.intern("auto");
        let mut process = Process::new(1, 128);
        let mut context = context(&mut process, Arc::clone(&atom_table), registry);
        let option = tuple_option(&mut context, write_concurrency, Term::atom(auto));
        let options = context.alloc_list(&[option]).expect("option list");

        assert_eq!(
            bif_new(&[Term::atom(table_name), options], &mut context),
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
            ("delete", 1),
            ("delete", 2),
            ("member", 2),
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
