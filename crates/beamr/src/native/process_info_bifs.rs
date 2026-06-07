//! Process introspection BIFs — process_info/1,2.
//!
//! This module intentionally implements only the process information items
//! currently needed by OTP/Gleam compatibility. The scheduler snapshots process
//! metadata through [`ProcessInfoFacility`]; this BIF module is responsible for
//! turning that allocation-free snapshot into caller-heap Erlang terms.

use crate::atom::{Atom, AtomTable};
use crate::native::{
    BifRegistryImpl, Capability, NativeFn, NativeRegistrationError, ProcessContext,
};
use crate::term::Term;

/// Supported `erlang:process_info/*` item names in deterministic result order.
const SUPPORTED_ITEMS: &[ProcessInfoItem] = &[
    ProcessInfoItem::CurrentFunction,
    ProcessInfoItem::HeapSize,
    ProcessInfoItem::MessageQueueLen,
    ProcessInfoItem::RegisteredName,
    ProcessInfoItem::Status,
    ProcessInfoItem::TrapExit,
    ProcessInfoItem::Links,
    ProcessInfoItem::Monitors,
];

type ProcessInfoBif = (&'static str, u8, Capability, NativeFn);

const PROCESS_INFO_BIFS: &[ProcessInfoBif] = &[
    ("process_info", 1, Capability::Pure, bif_process_info_1),
    ("process_info", 2, Capability::Pure, bif_process_info_2),
];

/// Process-info item understood by the scheduler-backed introspection facility.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ProcessInfoItem {
    /// `current_function` → `{Module, Function, Arity}`.
    CurrentFunction,
    /// `heap_size` → words currently allocated by the process heap.
    HeapSize,
    /// `message_queue_len` → number of queued mailbox messages.
    MessageQueueLen,
    /// `registered_name` → registered atom name, or `[]`.
    RegisteredName,
    /// `status` → `running | waiting | suspended`.
    Status,
    /// `trap_exit` → boolean atom.
    TrapExit,
    /// `links` → list of linked process identifiers.
    Links,
    /// `monitors` → list of monitored process descriptors.
    Monitors,
}

impl ProcessInfoItem {
    fn name(self) -> &'static str {
        match self {
            Self::CurrentFunction => "current_function",
            Self::HeapSize => "heap_size",
            Self::MessageQueueLen => "message_queue_len",
            Self::RegisteredName => "registered_name",
            Self::Status => "status",
            Self::TrapExit => "trap_exit",
            Self::Links => "links",
            Self::Monitors => "monitors",
        }
    }
}

/// Public, allocation-free process status snapshot for process_info.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ProcessInfoStatus {
    /// Process is running or runnable.
    Running,
    /// Process is waiting for a message or timeout.
    Waiting,
    /// Process is scheduler-suspended.
    Suspended,
}

/// Monitor metadata snapshot safe to expose through `process_info(Pid, monitors)`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ProcessMonitorInfo {
    /// PID that owns the monitor.
    pub watcher: u64,
    /// PID being monitored.
    pub target: u64,
}

/// Allocation-free snapshot returned by the scheduler and rendered by this BIF.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProcessInfoValue {
    /// Current module/function/arity metadata.
    CurrentFunction(Option<(Atom, Atom, u8)>),
    /// Heap words currently used.
    HeapSize(usize),
    /// Number of queued messages.
    MessageQueueLen(usize),
    /// Registered process name, if one is available.
    RegisteredName(Option<Atom>),
    /// Observable process status.
    Status(ProcessInfoStatus),
    /// Trap-exit flag.
    TrapExit(bool),
    /// Linked process identifiers.
    Links(Vec<u64>),
    /// Monitor records attached to the process.
    Monitors(Vec<ProcessMonitorInfo>),
}

/// Scheduler-provided process information reader.
pub trait ProcessInfoFacility: Send + Sync {
    /// Snapshot a single process-info item. Returns `None` when `pid` is not a
    /// live process or the process body is absent.
    fn process_info(&self, pid: u64, item: ProcessInfoItem) -> Option<ProcessInfoValue>;
}

/// Registers process introspection BIFs into the VM-owned BIF registry.
pub fn register_process_info_bifs(
    registry: &BifRegistryImpl,
    atom_table: &AtomTable,
) -> Result<(), NativeRegistrationError> {
    let erlang = atom_table.intern("erlang");

    for &(function_name, arity, capability, native_function) in PROCESS_INFO_BIFS {
        let function = atom_table.intern(function_name);
        registry.register(erlang, function, arity, native_function, capability)?;
    }

    Ok(())
}

/// erlang:process_info/2 — query one process information item.
pub fn bif_process_info_2(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [pid_term, item_term] = args else {
        return Err(badarg());
    };
    let pid = pid_term.as_pid().ok_or_else(badarg)?;
    let item_atom = item_term.as_atom().ok_or_else(badarg)?;
    let item = parse_item(context, item_atom)?;
    let Some(value) = query_process_info(context, pid, item) else {
        return Ok(Term::atom(Atom::UNDEFINED));
    };

    let words = 3 + value_heap_words(pid, &value);
    context.ensure_heap_space(words)?;
    let value_term = alloc_value(context, pid, value)?;
    context.alloc_tuple(&[*item_term, value_term])
}

/// erlang:process_info/1 — query all supported process information items.
pub fn bif_process_info_1(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [pid_term] = args else {
        return Err(badarg());
    };
    let pid = pid_term.as_pid().ok_or_else(badarg)?;

    let mut values = Vec::with_capacity(SUPPORTED_ITEMS.len());
    for item in SUPPORTED_ITEMS {
        let Some(value) = query_process_info(context, pid, *item) else {
            return Ok(Term::atom(Atom::UNDEFINED));
        };
        values.push((*item, value));
    }

    let words = values
        .iter()
        .map(|(_, value)| 3 + value_heap_words(pid, value) + 2)
        .sum();
    context.ensure_heap_space(words)?;

    let mut tuples = Vec::with_capacity(values.len());
    for (item, value) in values {
        let item_atom = intern_item_atom(context, item)?;
        let value_term = alloc_value(context, pid, value)?;
        tuples.push(context.alloc_tuple(&[Term::atom(item_atom), value_term])?);
    }
    context.alloc_list(&tuples)
}

fn query_process_info(
    context: &ProcessContext,
    pid: u64,
    item: ProcessInfoItem,
) -> Option<ProcessInfoValue> {
    context.process_info_facility()?.process_info(pid, item)
}

fn parse_item(context: &ProcessContext, atom: Atom) -> Result<ProcessInfoItem, Term> {
    for item in SUPPORTED_ITEMS {
        if intern_item_atom(context, *item)? == atom {
            return Ok(*item);
        }
    }
    Err(badarg())
}

fn intern_item_atom(context: &ProcessContext, item: ProcessInfoItem) -> Result<Atom, Term> {
    let atom_table = context.atom_table().ok_or_else(badarg)?;
    Ok(atom_table.intern(item.name()))
}

fn alloc_value(
    context: &mut ProcessContext,
    queried_pid: u64,
    value: ProcessInfoValue,
) -> Result<Term, Term> {
    match value {
        ProcessInfoValue::CurrentFunction(current_mfa) => {
            let (module, function, arity) =
                current_mfa.unwrap_or((Atom::UNDEFINED, Atom::UNDEFINED, 0));
            context.alloc_tuple(&[
                Term::atom(module),
                Term::atom(function),
                Term::small_int(i64::from(arity)),
            ])
        }
        ProcessInfoValue::HeapSize(words) | ProcessInfoValue::MessageQueueLen(words) => {
            usize_to_small_int(words)
        }
        ProcessInfoValue::RegisteredName(Some(name)) => Ok(Term::atom(name)),
        ProcessInfoValue::RegisteredName(None) => Ok(Term::NIL),
        ProcessInfoValue::Status(status) => Ok(Term::atom(status_atom(context, status)?)),
        ProcessInfoValue::TrapExit(value) => Ok(bool_to_atom(value)),
        ProcessInfoValue::Links(links) => alloc_pid_list(context, &links),
        ProcessInfoValue::Monitors(monitors) => alloc_monitor_list(context, queried_pid, &monitors),
    }
}

fn value_heap_words(queried_pid: u64, value: &ProcessInfoValue) -> usize {
    match value {
        ProcessInfoValue::CurrentFunction(_) => 4,
        ProcessInfoValue::HeapSize(_)
        | ProcessInfoValue::MessageQueueLen(_)
        | ProcessInfoValue::RegisteredName(_)
        | ProcessInfoValue::Status(_)
        | ProcessInfoValue::TrapExit(_) => 0,
        ProcessInfoValue::Links(links) => links.len() * 2,
        ProcessInfoValue::Monitors(monitors) => {
            monitors
                .iter()
                .filter(|monitor| monitor.watcher == queried_pid)
                .count()
                * 5
        }
    }
}

fn status_atom(context: &ProcessContext, status: ProcessInfoStatus) -> Result<Atom, Term> {
    let atom_table = context.atom_table().ok_or_else(badarg)?;
    Ok(match status {
        ProcessInfoStatus::Running => atom_table.intern("running"),
        ProcessInfoStatus::Waiting => atom_table.intern("waiting"),
        ProcessInfoStatus::Suspended => atom_table.intern("suspended"),
    })
}

fn alloc_pid_list(context: &mut ProcessContext, pids: &[u64]) -> Result<Term, Term> {
    let mut terms = Vec::with_capacity(pids.len());
    for pid in pids {
        terms.push(Term::try_pid(*pid).ok_or_else(badarg)?);
    }
    context.alloc_list(&terms)
}

fn alloc_monitor_list(
    context: &mut ProcessContext,
    queried_pid: u64,
    monitors: &[ProcessMonitorInfo],
) -> Result<Term, Term> {
    let process_atom = Term::atom(Atom::PROCESS);
    let mut terms = Vec::new();
    for monitor in monitors {
        if monitor.watcher == queried_pid {
            let target = Term::try_pid(monitor.target).ok_or_else(badarg)?;
            terms.push(context.alloc_tuple(&[process_atom, target])?);
        }
    }
    context.alloc_list(&terms)
}

fn usize_to_small_int(value: usize) -> Result<Term, Term> {
    let value = i64::try_from(value).map_err(|_| badarg())?;
    Term::try_small_int(value).ok_or_else(badarg)
}

fn bool_to_atom(value: bool) -> Term {
    Term::atom(if value { Atom::TRUE } else { Atom::FALSE })
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::process::Process;
    use crate::term::boxed::{Cons, Tuple};

    #[derive(Default)]
    struct MockProcessInfoFacility {
        values: Mutex<HashMap<(u64, ProcessInfoItem), ProcessInfoValue>>,
    }

    impl MockProcessInfoFacility {
        fn insert(&self, pid: u64, item: ProcessInfoItem, value: ProcessInfoValue) {
            self.values
                .lock()
                .expect("values mutex")
                .insert((pid, item), value);
        }
    }

    impl ProcessInfoFacility for MockProcessInfoFacility {
        fn process_info(&self, pid: u64, item: ProcessInfoItem) -> Option<ProcessInfoValue> {
            self.values
                .lock()
                .expect("values mutex")
                .get(&(pid, item))
                .cloned()
        }
    }

    fn context_with_facility(
        atom_table: Arc<AtomTable>,
        facility: Arc<MockProcessInfoFacility>,
        process: &mut Process,
    ) -> ProcessContext<'_> {
        let mut context = ProcessContext::new();
        context.set_atom_table(Some(atom_table));
        context.set_process_info_facility(Some(facility));
        context.attach_process(process, 0);
        context
    }

    fn tuple_elements(term: Term) -> Vec<Term> {
        let tuple = Tuple::new(term).expect("tuple term");
        (0..tuple.arity()).map(|index| tuple.get(index)).collect()
    }

    fn list_elements(mut term: Term) -> Vec<Term> {
        let mut elements = Vec::new();
        while term != Term::NIL {
            let cons = Cons::new(term).expect("proper list cons");
            elements.push(cons.head());
            term = cons.tail();
        }
        elements
    }

    #[test]
    fn register_process_info_bifs_registers_process_info_1_and_2() {
        let atom_table = AtomTable::with_common_atoms();
        let registry = BifRegistryImpl::new();
        register_process_info_bifs(&registry, &atom_table).expect("registration");
        let erlang = atom_table.intern("erlang");
        let process_info = atom_table.intern("process_info");
        assert!(registry.lookup(erlang, process_info, 1).is_some());
        assert!(registry.lookup(erlang, process_info, 2).is_some());
    }

    #[test]
    fn process_info_2_returns_tuple_for_each_supported_item() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let facility = Arc::new(MockProcessInfoFacility::default());
        let pid = 7;
        let module = atom_table.intern("mod");
        let function = atom_table.intern("fun");
        facility.insert(
            pid,
            ProcessInfoItem::CurrentFunction,
            ProcessInfoValue::CurrentFunction(Some((module, function, 2))),
        );
        facility.insert(
            pid,
            ProcessInfoItem::HeapSize,
            ProcessInfoValue::HeapSize(10),
        );
        facility.insert(
            pid,
            ProcessInfoItem::MessageQueueLen,
            ProcessInfoValue::MessageQueueLen(3),
        );
        facility.insert(
            pid,
            ProcessInfoItem::RegisteredName,
            ProcessInfoValue::RegisteredName(Some(atom_table.intern("name"))),
        );
        facility.insert(
            pid,
            ProcessInfoItem::Status,
            ProcessInfoValue::Status(ProcessInfoStatus::Running),
        );
        facility.insert(
            pid,
            ProcessInfoItem::TrapExit,
            ProcessInfoValue::TrapExit(true),
        );
        facility.insert(
            pid,
            ProcessInfoItem::Links,
            ProcessInfoValue::Links(vec![1, 2]),
        );
        facility.insert(
            pid,
            ProcessInfoItem::Monitors,
            ProcessInfoValue::Monitors(vec![ProcessMonitorInfo {
                watcher: pid,
                target: 9,
            }]),
        );

        let item_names = [
            "current_function",
            "heap_size",
            "message_queue_len",
            "registered_name",
            "status",
            "trap_exit",
            "links",
            "monitors",
        ];
        for item_name in item_names {
            let mut process = Process::new(0, 128);
            let mut context =
                context_with_facility(Arc::clone(&atom_table), Arc::clone(&facility), &mut process);
            let item = atom_table.intern(item_name);
            let result = bif_process_info_2(&[Term::pid(pid), Term::atom(item)], &mut context)
                .expect("process_info/2 succeeds");
            let elements = tuple_elements(result);
            assert_eq!(elements.len(), 2);
            assert_eq!(elements[0], Term::atom(item));
        }
    }

    #[test]
    fn process_info_2_returns_undefined_for_missing_process_and_badarg_for_unknown_item() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let facility = Arc::new(MockProcessInfoFacility::default());
        let mut process = Process::new(0, 128);
        let mut context = context_with_facility(atom_table.clone(), facility, &mut process);
        let heap_size = atom_table.intern("heap_size");
        assert_eq!(
            bif_process_info_2(&[Term::pid(99), Term::atom(heap_size)], &mut context),
            Ok(Term::atom(Atom::UNDEFINED))
        );
        let unknown = atom_table.intern("unknown_process_info_item");
        assert_eq!(
            bif_process_info_2(&[Term::pid(99), Term::atom(unknown)], &mut context),
            Err(Term::atom(Atom::BADARG))
        );
    }

    #[test]
    fn process_info_1_returns_deterministic_list_of_all_supported_items() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let facility = Arc::new(MockProcessInfoFacility::default());
        let pid = 11;
        for item in SUPPORTED_ITEMS {
            let value = match item {
                ProcessInfoItem::CurrentFunction => ProcessInfoValue::CurrentFunction(None),
                ProcessInfoItem::HeapSize => ProcessInfoValue::HeapSize(0),
                ProcessInfoItem::MessageQueueLen => ProcessInfoValue::MessageQueueLen(0),
                ProcessInfoItem::RegisteredName => ProcessInfoValue::RegisteredName(None),
                ProcessInfoItem::Status => ProcessInfoValue::Status(ProcessInfoStatus::Waiting),
                ProcessInfoItem::TrapExit => ProcessInfoValue::TrapExit(false),
                ProcessInfoItem::Links => ProcessInfoValue::Links(Vec::new()),
                ProcessInfoItem::Monitors => ProcessInfoValue::Monitors(Vec::new()),
            };
            facility.insert(pid, *item, value);
        }

        let mut process = Process::new(0, 256);
        let mut context = context_with_facility(atom_table.clone(), facility, &mut process);
        let result = bif_process_info_1(&[Term::pid(pid)], &mut context).expect("process_info/1");
        let entries = list_elements(result);
        assert_eq!(entries.len(), SUPPORTED_ITEMS.len());
        for (entry, item) in entries.into_iter().zip(SUPPORTED_ITEMS) {
            let tuple = tuple_elements(entry);
            assert_eq!(tuple[0], Term::atom(atom_table.intern(item.name())));
        }
    }
}
