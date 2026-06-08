//! VM introspection BIFs — `erlang:system_info/1` and related stubs.

use crate::atom::{Atom, AtomTable};
use crate::native::{
    BifRegistryImpl, Capability, NativeFn, NativeRegistrationError, ProcessContext,
};
use crate::term::Term;

/// Default process limit reported to OTP compatibility code.
pub const DEFAULT_PROCESS_LIMIT: usize = 262_144;
/// OTP release compatibility claim used by stdlib probes.
pub const OTP_RELEASE: &[u8] = b"27";
/// BEAM word size reported for the supported 64-bit runtime target.
pub const WORDSIZE_BYTES: usize = 8;

const MEMORY_ITEMS: &[&str] = &["total", "processes", "system", "atom", "binary"];

type SystemInfoBif = (&'static str, u8, Capability, NativeFn);

const SYSTEM_INFO_BIFS: &[SystemInfoBif] = &[
    ("system_info", 1, Capability::Pure, bif_system_info),
    ("statistics", 1, Capability::Pure, bif_statistics_1),
    ("memory", 0, Capability::Pure, bif_memory_0),
    ("memory", 1, Capability::Pure, bif_memory_1),
    ("ports", 0, Capability::Pure, bif_ports_0),
    ("port_info", 1, Capability::Pure, bif_port_info_1),
    ("open_port", 2, Capability::Pure, bif_open_port_2),
];

/// Basic statistics summary returned by `erlang:statistics/1`.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct StatisticsSummary {
    pub wall_clock_millis: usize,
    pub wall_clock_since_last_millis: usize,
    pub runtime_millis: usize,
    pub runtime_since_last_millis: usize,
    pub reductions: usize,
    pub reductions_since_last: usize,
}

/// Approximate memory summary returned by `erlang:memory/0,1`.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct MemorySummary {
    pub total: usize,
    pub processes: usize,
    pub system: usize,
    pub atom: usize,
    pub binary: usize,
}

impl MemorySummary {
    pub fn from_components(processes: usize, atom: usize, binary: usize) -> Self {
        let system = atom.saturating_add(binary);
        let total = processes.saturating_add(system);
        Self {
            total,
            processes,
            system,
            atom,
            binary,
        }
    }
}

/// Narrow interface used by `system_info/1` to query runtime metrics.
///
/// Implementations are provided by the scheduler and injected into
/// [`ProcessContext`] before BIF execution so native code does not receive direct
/// access to scheduler internals.
pub trait SystemInfoFacility: Send + Sync {
    /// Number of normal scheduler threads.
    fn scheduler_count(&self) -> usize;

    /// Number of currently alive processes.
    fn process_count(&self) -> usize;

    /// Maximum number of alive processes supported by this runtime.
    fn process_limit(&self) -> usize {
        DEFAULT_PROCESS_LIMIT
    }

    /// Current atom table size.
    fn atom_count(&self) -> usize;

    /// Maximum atom count supported by this runtime.
    fn atom_limit(&self) -> usize;

    /// Runtime statistics. Defaults to zeroed stubs until the scheduler tracks totals.
    fn statistics_summary(&self) -> StatisticsSummary {
        StatisticsSummary::default()
    }

    /// Approximate memory usage. Defaults to an atom-table-only system estimate.
    fn memory_summary(&self) -> MemorySummary {
        let atom = self.atom_count().saturating_mul(WORDSIZE_BYTES);
        MemorySummary::from_components(0, atom, 0)
    }
}

/// Registers system introspection BIFs and OTP compatibility stubs.
pub fn register_system_info_bifs(
    registry: &BifRegistryImpl,
    atom_table: &AtomTable,
) -> Result<(), NativeRegistrationError> {
    let erlang = atom_table.intern("erlang");

    for &(function_name, arity, capability, native_function) in SYSTEM_INFO_BIFS {
        let function = atom_table.intern(function_name);
        registry.register(erlang, function, arity, native_function, capability)?;
    }

    Ok(())
}

/// erlang:system_info/1 — return a small OTP-compatible subset of VM metadata.
pub fn bif_system_info(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [item_term] = args else {
        return Err(badarg());
    };
    let item_name = atom_name(*item_term, context)?.to_owned();

    match item_name.as_str() {
        "schedulers" => facility_small_int(context, SystemInfoMetric::Schedulers),
        "process_count" => facility_small_int(context, SystemInfoMetric::ProcessCount),
        "process_limit" => facility_small_int(context, SystemInfoMetric::ProcessLimit),
        "wordsize" => small_int(WORDSIZE_BYTES),
        "otp_release" => context.alloc_binary(OTP_RELEASE),
        "version" => context.alloc_binary(env!("CARGO_PKG_VERSION").as_bytes()),
        "system_architecture" => context.alloc_binary(system_architecture().as_bytes()),
        "atom_count" => facility_small_int(context, SystemInfoMetric::AtomCount),
        "atom_limit" => facility_small_int(context, SystemInfoMetric::AtomLimit),
        _ => Err(badarg()),
    }
}

/// erlang:statistics/1 — return basic VM statistics accepted by OTP probes.
pub fn bif_statistics_1(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [item_term] = args else {
        return Err(badarg());
    };
    let item_name = atom_name(*item_term, context)?.to_owned();
    let summary = context
        .system_info_facility()
        .map(SystemInfoFacility::statistics_summary)
        .unwrap_or_default();

    let (total, since_last) = match item_name.as_str() {
        "wall_clock" => (
            summary.wall_clock_millis,
            summary.wall_clock_since_last_millis,
        ),
        "runtime" => (summary.runtime_millis, summary.runtime_since_last_millis),
        "reductions" => (summary.reductions, summary.reductions_since_last),
        _ => return Err(badarg()),
    };

    context.alloc_tuple(&[small_int(total)?, small_int(since_last)?])
}

/// erlang:memory/0 — return a deterministic memory proplist.
pub fn bif_memory_0(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [] = args else {
        return Err(badarg());
    };

    let summary = memory_summary(context);
    let item_atoms = {
        let atom_table = context.atom_table().ok_or_else(badarg)?;
        MEMORY_ITEMS
            .iter()
            .map(|item| atom_table.intern(item))
            .collect::<Vec<_>>()
    };
    let values = memory_values(summary)?;

    context.ensure_heap_space(info_proplist_heap_words(MEMORY_ITEMS.len()))?;
    let mut entries = Vec::with_capacity(MEMORY_ITEMS.len());
    for (item_atom, value) in item_atoms.into_iter().zip(values) {
        entries.push(context.alloc_tuple(&[Term::atom(item_atom), value])?);
    }
    context.alloc_list(&entries)
}

/// erlang:memory/1 — return one approximate memory item.
pub fn bif_memory_1(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [item_term] = args else {
        return Err(badarg());
    };
    let item_name = atom_name(*item_term, context)?.to_owned();
    let summary = memory_summary(context);

    match item_name.as_str() {
        "total" => small_int(summary.total),
        "processes" => small_int(summary.processes),
        "system" => small_int(summary.system),
        "atom" => small_int(summary.atom),
        "binary" => small_int(summary.binary),
        _ => Err(badarg()),
    }
}

/// erlang:ports/0 — beamr has no port subsystem.
pub fn bif_ports_0(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [] = args else {
        return Err(badarg());
    };
    Ok(Term::NIL)
}

/// erlang:port_info/1 — no port metadata exists in beamr.
pub fn bif_port_info_1(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [_port] = args else {
        return Err(badarg());
    };
    Ok(Term::atom(Atom::UNDEFINED))
}

/// erlang:open_port/2 — ports are deliberately unsupported.
pub fn bif_open_port_2(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [_port_name, _settings] = args else {
        return Err(badarg());
    };
    Err(badarg())
}

enum SystemInfoMetric {
    Schedulers,
    ProcessCount,
    ProcessLimit,
    AtomCount,
    AtomLimit,
}

fn facility_small_int(context: &ProcessContext, metric: SystemInfoMetric) -> Result<Term, Term> {
    let facility = context.system_info_facility().ok_or_else(badarg)?;
    let value = match metric {
        SystemInfoMetric::Schedulers => facility.scheduler_count(),
        SystemInfoMetric::ProcessCount => facility.process_count(),
        SystemInfoMetric::ProcessLimit => facility.process_limit(),
        SystemInfoMetric::AtomCount => facility.atom_count(),
        SystemInfoMetric::AtomLimit => facility.atom_limit(),
    };
    small_int(value)
}

fn atom_name<'context>(
    term: Term,
    context: &'context ProcessContext<'_>,
) -> Result<&'context str, Term> {
    let item = term.as_atom().ok_or_else(badarg)?;
    let table = context.atom_table().ok_or_else(badarg)?;
    table.resolve(item).ok_or_else(badarg)
}

fn memory_summary(context: &ProcessContext) -> MemorySummary {
    context
        .system_info_facility()
        .map(SystemInfoFacility::memory_summary)
        .unwrap_or_default()
}

fn memory_values(summary: MemorySummary) -> Result<[Term; 5], Term> {
    Ok([
        small_int(summary.total)?,
        small_int(summary.processes)?,
        small_int(summary.system)?,
        small_int(summary.atom)?,
        small_int(summary.binary)?,
    ])
}

const fn info_proplist_heap_words(item_count: usize) -> usize {
    item_count * 5
}

fn small_int(value: usize) -> Result<Term, Term> {
    let value = i64::try_from(value).map_err(|_| badarg())?;
    Term::try_small_int(value).ok_or_else(badarg)
}

fn system_architecture() -> String {
    format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::process::Process;
    use crate::term::binary::Binary;
    use crate::term::boxed::{Cons, Tuple};

    struct TestSystemInfoFacility;

    impl SystemInfoFacility for TestSystemInfoFacility {
        fn scheduler_count(&self) -> usize {
            4
        }

        fn process_count(&self) -> usize {
            12
        }

        fn process_limit(&self) -> usize {
            262_144
        }

        fn atom_count(&self) -> usize {
            44
        }

        fn atom_limit(&self) -> usize {
            u32::MAX as usize
        }

        fn statistics_summary(&self) -> StatisticsSummary {
            StatisticsSummary {
                wall_clock_millis: 100,
                wall_clock_since_last_millis: 0,
                runtime_millis: 20,
                runtime_since_last_millis: 0,
                reductions: 1_000,
                reductions_since_last: 0,
            }
        }

        fn memory_summary(&self) -> MemorySummary {
            MemorySummary {
                total: 1_024,
                processes: 384,
                system: 640,
                atom: 128,
                binary: 256,
            }
        }
    }

    fn context<'process>(
        process: &'process mut Process,
        atom_table: Arc<AtomTable>,
    ) -> ProcessContext<'process> {
        let mut context = ProcessContext::new();
        context.set_atom_table(Some(atom_table));
        context.set_system_info_facility(Some(Arc::new(TestSystemInfoFacility)));
        context.attach_process(process, 0);
        context
    }

    fn call_system_info(item: &str) -> Term {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let item_atom = atom_table.intern(item);
        let mut process = Process::new(1, 128);
        let mut context = context(&mut process, atom_table);
        bif_system_info(&[Term::atom(item_atom)], &mut context).expect("system_info succeeds")
    }

    fn call_system_info_binary_bytes(item: &str) -> Vec<u8> {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let item_atom = atom_table.intern(item);
        let mut process = Process::new(1, 128);
        let mut context = context(&mut process, atom_table);
        let term =
            bif_system_info(&[Term::atom(item_atom)], &mut context).expect("system_info succeeds");
        Binary::new(term)
            .expect("system_info item returns binary")
            .as_bytes()
            .to_vec()
    }

    fn call_statistics(item: &str) -> Term {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let item_atom = atom_table.intern(item);
        let mut process = Process::new(1, 128);
        let mut context = context(&mut process, atom_table);
        bif_statistics_1(&[Term::atom(item_atom)], &mut context).expect("statistics succeeds")
    }

    fn call_memory_one(item: &str) -> Term {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let item_atom = atom_table.intern(item);
        let mut process = Process::new(1, 128);
        let mut context = context(&mut process, atom_table);
        bif_memory_1(&[Term::atom(item_atom)], &mut context).expect("memory/1 succeeds")
    }

    fn proplist_to_pairs(list: Term) -> Vec<(Atom, Term)> {
        let mut pairs = Vec::new();
        let mut tail = list;
        while !tail.is_nil() {
            let cons = Cons::new(tail).expect("proper list cons");
            let tuple = Tuple::new(cons.head()).expect("proplist tuple");
            assert_eq!(tuple.arity(), 2);
            let key = tuple.get(0).expect("key").as_atom().expect("atom key");
            let value = tuple.get(1).expect("value");
            pairs.push((key, value));
            tail = cons.tail();
        }
        pairs
    }

    #[test]
    fn numeric_items_return_small_integers() {
        assert_eq!(call_system_info("schedulers").as_small_int(), Some(4));
        assert_eq!(call_system_info("process_count").as_small_int(), Some(12));
        assert_eq!(
            call_system_info("process_limit").as_small_int(),
            Some(262_144)
        );
        assert_eq!(
            call_system_info("wordsize").as_small_int(),
            Some(i64::try_from(WORDSIZE_BYTES).unwrap_or(i64::MAX))
        );
        assert_eq!(call_system_info("atom_count").as_small_int(), Some(44));
        assert_eq!(
            call_system_info("atom_limit").as_small_int(),
            Some(i64::from(u32::MAX))
        );
    }

    #[test]
    fn binary_items_return_expected_bytes() {
        assert_eq!(
            call_system_info_binary_bytes("otp_release").as_slice(),
            b"27"
        );
        assert_eq!(
            call_system_info_binary_bytes("version").as_slice(),
            env!("CARGO_PKG_VERSION").as_bytes()
        );
        assert_eq!(
            call_system_info_binary_bytes("system_architecture").as_slice(),
            system_architecture().as_bytes()
        );
    }

    #[test]
    fn statistics_returns_total_since_last_tuples() {
        let wall_clock = Tuple::new(call_statistics("wall_clock")).expect("wall_clock tuple");
        assert_eq!(wall_clock.arity(), 2);
        assert_eq!(wall_clock.get(0).and_then(Term::as_small_int), Some(100));
        assert_eq!(wall_clock.get(1).and_then(Term::as_small_int), Some(0));

        let runtime = Tuple::new(call_statistics("runtime")).expect("runtime tuple");
        assert_eq!(runtime.arity(), 2);
        assert_eq!(runtime.get(0).and_then(Term::as_small_int), Some(20));
        assert_eq!(runtime.get(1).and_then(Term::as_small_int), Some(0));

        let reductions = Tuple::new(call_statistics("reductions")).expect("reductions tuple");
        assert_eq!(reductions.arity(), 2);
        assert_eq!(reductions.get(0).and_then(Term::as_small_int), Some(1_000));
        assert_eq!(reductions.get(1).and_then(Term::as_small_int), Some(0));
    }

    #[test]
    fn statistics_badarg_for_unknown_item_non_atom_and_wrong_arity() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let unknown = atom_table.intern("does_not_exist");
        let mut process = Process::new(1, 128);
        let mut context = context(&mut process, atom_table);

        assert_eq!(
            bif_statistics_1(&[Term::atom(unknown)], &mut context),
            Err(badarg())
        );
        assert_eq!(
            bif_statistics_1(&[Term::small_int(1)], &mut context),
            Err(badarg())
        );
        assert_eq!(bif_statistics_1(&[], &mut context), Err(badarg()));
    }

    #[test]
    fn memory_zero_returns_expected_proplist_and_memory_one_matches() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let mut process = Process::new(1, 256);
        let mut context = context(&mut process, Arc::clone(&atom_table));
        let memory = bif_memory_0(&[], &mut context).expect("memory/0 succeeds");
        let pairs = proplist_to_pairs(memory);

        assert_eq!(pairs.len(), MEMORY_ITEMS.len());
        for (index, item) in MEMORY_ITEMS.iter().enumerate() {
            let expected_atom = atom_table.intern(item);
            assert_eq!(pairs[index].0, expected_atom);
        }
        assert_eq!(pairs[0].1.as_small_int(), Some(1_024));
        assert_eq!(pairs[1].1.as_small_int(), Some(384));
        assert_eq!(pairs[2].1.as_small_int(), Some(640));
        assert_eq!(pairs[3].1.as_small_int(), Some(128));
        assert_eq!(pairs[4].1.as_small_int(), Some(256));

        assert_eq!(call_memory_one("total").as_small_int(), Some(1_024));
        assert_eq!(call_memory_one("processes").as_small_int(), Some(384));
        assert_eq!(call_memory_one("system").as_small_int(), Some(640));
        assert_eq!(call_memory_one("atom").as_small_int(), Some(128));
        assert_eq!(call_memory_one("binary").as_small_int(), Some(256));
    }

    #[test]
    fn memory_badarg_for_unknown_item_non_atom_and_wrong_arity() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let unknown = atom_table.intern("does_not_exist");
        let mut process = Process::new(1, 128);
        let mut context = context(&mut process, atom_table);

        assert_eq!(
            bif_memory_0(&[Term::small_int(1)], &mut context),
            Err(badarg())
        );
        assert_eq!(
            bif_memory_1(&[Term::atom(unknown)], &mut context),
            Err(badarg())
        );
        assert_eq!(
            bif_memory_1(&[Term::small_int(1)], &mut context),
            Err(badarg())
        );
        assert_eq!(bif_memory_1(&[], &mut context), Err(badarg()));
    }

    #[test]
    fn port_stubs_return_otp_compatible_defaults() {
        let mut context = ProcessContext::new();

        assert_eq!(bif_ports_0(&[], &mut context), Ok(Term::NIL));
        assert_eq!(
            bif_port_info_1(&[Term::small_int(1)], &mut context),
            Ok(Term::atom(Atom::UNDEFINED))
        );
        assert_eq!(
            bif_open_port_2(&[Term::small_int(1), Term::NIL], &mut context),
            Err(badarg())
        );
    }

    #[test]
    fn port_stubs_validate_arity() {
        let mut context = ProcessContext::new();

        assert_eq!(bif_ports_0(&[Term::NIL], &mut context), Err(badarg()));
        assert_eq!(bif_port_info_1(&[], &mut context), Err(badarg()));
        assert_eq!(bif_open_port_2(&[Term::NIL], &mut context), Err(badarg()));
    }

    #[test]
    fn badarg_for_unknown_item_non_atom_wrong_arity_and_missing_facility() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let unknown = atom_table.intern("does_not_exist");
        let mut process = Process::new(1, 128);
        let mut context = context(&mut process, Arc::clone(&atom_table));
        assert_eq!(
            bif_system_info(&[Term::atom(unknown)], &mut context),
            Err(badarg())
        );
        assert_eq!(
            bif_system_info(&[Term::small_int(1)], &mut context),
            Err(badarg())
        );
        assert_eq!(bif_system_info(&[], &mut context), Err(badarg()));

        let scheduler_item = atom_table.intern("schedulers");
        let mut no_facility_process = Process::new(2, 128);
        let mut no_facility = ProcessContext::new();
        no_facility.set_atom_table(Some(atom_table));
        no_facility.attach_process(&mut no_facility_process, 0);
        assert_eq!(
            bif_system_info(&[Term::atom(scheduler_item)], &mut no_facility),
            Err(badarg())
        );
    }

    #[test]
    fn registers_erlang_system_info_bifs() {
        let atom_table = AtomTable::new();
        let registry = BifRegistryImpl::new();
        register_system_info_bifs(&registry, &atom_table).expect("registration succeeds");

        let erlang = atom_table.intern("erlang");
        for &(function_name, arity, capability, _native_function) in SYSTEM_INFO_BIFS {
            let function = atom_table.intern(function_name);
            let entry = registry
                .lookup(erlang, function, arity)
                .expect("system info BIF registered");
            assert_eq!(entry.capability, capability);
            assert!(entry.dirty_kind.is_none());
        }
    }
}
