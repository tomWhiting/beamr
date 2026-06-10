use super::*;
use crate::atom::{Atom, AtomTable};
use crate::capability::Sandbox;
use crate::distribution::remote_link::{DistributionControlFacility, RemoteLinkError};
use crate::native::links::{LinkError, LinkFacility, LinkRecord};
use crate::native::spawn::{
    SpawnError, SpawnFacility, SpawnMonitorResult, SpawnOptions, SpawnOptionsResult, SpawnRecord,
};
use crate::native::supervision::{
    MonitorResult, SupervisionError, SupervisionFacility, SupervisionRecord,
};
use crate::native::{
    BifRegistryImpl, CapabilitySet, ExceptionClass, ProcessContext, RemoteSpawnError,
    RemoteSpawnFacility, RemoteSpawnResult,
};
use crate::process::{ExitReason, Priority, Process, RemotePid};
use crate::term::Term;
use crate::term::boxed::{
    Reference, Tuple, write_closure, write_cons, write_external_pid, write_tuple,
};
use crate::term::pid_ref::PidRef;
use std::sync::{Arc, Mutex};

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}

#[test]
fn process_flag_priority_sets_high_and_returns_old_priority() {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let mut process = Process::new(1, 128);
    let mut ctx = ProcessContext::new();
    ctx.set_atom_table(Some(atom_table.clone()));
    ctx.attach_process(&mut process, 0);
    let priority = atom_table.intern("priority");
    let high = atom_table.intern("high");

    assert_eq!(
        bif_process_flag(&[Term::atom(priority), Term::atom(high)], &mut ctx),
        Ok(Term::atom(Atom::NORMAL)),
    );
    assert_eq!(ctx.priority(), Ok(Priority::High));
}

#[test]
fn process_flag_priority_rejects_invalid_atom() {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let mut process = Process::new(1, 128);
    let mut ctx = ProcessContext::new();
    ctx.set_atom_table(Some(atom_table.clone()));
    ctx.attach_process(&mut process, 0);
    let priority = atom_table.intern("priority");

    assert_eq!(
        bif_process_flag(&[Term::atom(priority), Term::atom(Atom::OK)], &mut ctx),
        Err(badarg()),
    );
}

fn attached_spawn_ctx(
    next_pid: u64,
    next_ref: u64,
    caller_pid: u64,
    process: &mut Process,
) -> (Arc<MockSpawnFacility>, ProcessContext<'_>) {
    let f = Arc::new(MockSpawnFacility::with_reference(next_pid, next_ref));
    let mut ctx = ProcessContext::new();
    ctx.attach_process(process, 0);
    ctx.set_pid(Some(caller_pid));
    ctx.set_atom_table(Some(Arc::new(AtomTable::with_common_atoms())));
    ctx.set_spawn_facility(Some(f.clone()));
    (f, ctx)
}

fn assert_spawn_monitor_tuple(term: Term, pid: u64, reference: u64) {
    let tuple = Tuple::new(term).expect("spawn_monitor returns tuple");
    assert_eq!(tuple.arity(), 2);
    assert_eq!(tuple.get(0), Some(Term::pid(pid)));
    let ref_term = tuple.get(1).expect("reference element");
    assert_eq!(
        Reference::new(ref_term).expect("boxed reference").id(),
        reference
    );
}

// ---- erlang:self/0 ----

#[test]
fn self_returns_pid() {
    let mut ctx = ProcessContext::new();
    ctx.set_pid(Some(42));
    assert_eq!(bif_self(&[], &mut ctx), Ok(Term::pid(42)));
}

#[test]
fn self_badarg_no_pid() {
    assert_eq!(bif_self(&[], &mut ProcessContext::new()), Err(badarg()));
}

#[test]
fn self_badarg_wrong_arity() {
    let mut ctx = ProcessContext::new();
    ctx.set_pid(Some(1));
    assert_eq!(bif_self(&[Term::small_int(1)], &mut ctx), Err(badarg()));
}

// ---- erlang:spawn/3 ----

#[test]
fn spawn_badarg_without_facility() {
    let mut ctx = ProcessContext::new();
    ctx.set_pid(Some(0));
    assert_eq!(
        bif_spawn(
            &[Term::atom(Atom::OK), Term::atom(Atom::ERROR), Term::NIL],
            &mut ctx
        ),
        Err(badarg()),
    );
}

#[test]
fn spawn_returns_new_pid() {
    let (f, mut ctx) = spawn_ctx(7, 0);
    assert_eq!(
        bif_spawn(
            &[Term::atom(Atom::OK), Term::atom(Atom::ERROR), Term::NIL],
            &mut ctx
        ),
        Ok(Term::pid(7)),
    );
    assert_eq!(f.records().len(), 1);
    assert_eq!(f.records()[0].caller_pid, 0);
    assert_eq!(f.records()[0].link_to, None);
}

#[test]
fn spawn_passes_list_args() {
    let (f, mut ctx) = spawn_ctx(10, 0);
    let mut c2 = [0u64; 2];
    let tail = write_cons(&mut c2, Term::small_int(2), Term::NIL).unwrap();
    let mut c1 = [0u64; 2];
    let list = write_cons(&mut c1, Term::small_int(1), tail).unwrap();
    assert_eq!(
        bif_spawn(
            &[Term::atom(Atom::OK), Term::atom(Atom::ERROR), list],
            &mut ctx
        ),
        Ok(Term::pid(10)),
    );
    assert_eq!(
        f.records()[0].args,
        vec![Term::small_int(1), Term::small_int(2)]
    );
}

#[test]
fn spawn_badarg_non_atom_module() {
    let (_, mut ctx) = spawn_ctx(1, 0);
    assert_eq!(
        bif_spawn(
            &[Term::small_int(42), Term::atom(Atom::OK), Term::NIL],
            &mut ctx
        ),
        Err(badarg()),
    );
}

#[test]
fn spawn_badarg_wrong_arity() {
    assert_eq!(
        bif_spawn(&[Term::atom(Atom::OK)], &mut ProcessContext::new()),
        Err(badarg())
    );
}

#[test]
fn spawn_badarg_facility_fails() {
    let f: Arc<dyn SpawnFacility> = Arc::new(FailingSpawnFacility);
    let mut ctx = ProcessContext::new();
    ctx.set_pid(Some(0));
    ctx.set_spawn_facility(Some(f));
    assert_eq!(
        bif_spawn(
            &[Term::atom(Atom::OK), Term::atom(Atom::ERROR), Term::NIL],
            &mut ctx
        ),
        Err(badarg()),
    );
}

// ---- erlang:spawn_link/3 ----

#[test]
fn spawn_link_sets_link_to_parent() {
    let (f, mut ctx) = spawn_ctx(8, 3);
    assert_eq!(
        bif_spawn_link(
            &[Term::atom(Atom::OK), Term::atom(Atom::ERROR), Term::NIL],
            &mut ctx
        ),
        Ok(Term::pid(8)),
    );
    assert_eq!(f.records()[0].caller_pid, 3);
    assert_eq!(f.records()[0].link_to, Some(3));
}

#[test]
fn spawn_link_badarg_without_pid() {
    let f: Arc<dyn SpawnFacility> = Arc::new(MockSpawnFacility::new(8));
    let mut ctx = ProcessContext::new();
    ctx.set_spawn_facility(Some(f));
    assert_eq!(
        bif_spawn_link(
            &[Term::atom(Atom::OK), Term::atom(Atom::ERROR), Term::NIL],
            &mut ctx
        ),
        Err(badarg()),
    );
}

// ---- erlang:spawn_monitor/3 ----

#[test]
fn spawn_monitor_3_returns_pid_and_boxed_reference() {
    let mut process = Process::new(3, 128);
    let (f, mut ctx) = attached_spawn_ctx(8, 42, 3, &mut process);
    let result = bif_spawn_monitor_3(
        &[Term::atom(Atom::OK), Term::atom(Atom::ERROR), Term::NIL],
        &mut ctx,
    )
    .expect("spawn_monitor/3 succeeds");
    assert_spawn_monitor_tuple(result, 8, 42);
    let records = f.spawn_monitor_records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].caller_pid, 3);
    assert_eq!(records[0].link_to, None);
}

#[test]
fn spawn_monitor_3_passes_list_args() {
    let mut process = Process::new(1, 128);
    let (f, mut ctx) = attached_spawn_ctx(10, 0, 1, &mut process);
    let mut c2 = [0u64; 2];
    let tail = write_cons(&mut c2, Term::small_int(2), Term::NIL).expect("tail cons");
    let mut c1 = [0u64; 2];
    let list = write_cons(&mut c1, Term::small_int(1), tail).expect("head cons");
    let result = bif_spawn_monitor_3(
        &[Term::atom(Atom::OK), Term::atom(Atom::ERROR), list],
        &mut ctx,
    )
    .expect("spawn_monitor/3 succeeds");
    assert_spawn_monitor_tuple(result, 10, 0);
    assert_eq!(
        f.spawn_monitor_records()[0].args,
        vec![Term::small_int(1), Term::small_int(2)]
    );
}

#[test]
fn spawn_monitor_3_badarg_without_facility() {
    let mut process = Process::new(1, 128);
    let mut ctx = ProcessContext::new();
    ctx.attach_process(&mut process, 0);
    ctx.set_pid(Some(1));
    assert_eq!(
        bif_spawn_monitor_3(
            &[Term::atom(Atom::OK), Term::atom(Atom::ERROR), Term::NIL],
            &mut ctx
        ),
        Err(badarg()),
    );
}

#[test]
fn spawn_monitor_3_badarg_facility_fails() {
    let f: Arc<dyn SpawnFacility> = Arc::new(FailingSpawnFacility);
    let mut process = Process::new(1, 128);
    let mut ctx = ProcessContext::new();
    ctx.attach_process(&mut process, 0);
    ctx.set_pid(Some(1));
    ctx.set_spawn_facility(Some(f));
    assert_eq!(
        bif_spawn_monitor_3(
            &[Term::atom(Atom::OK), Term::atom(Atom::ERROR), Term::NIL],
            &mut ctx
        ),
        Err(badarg()),
    );
}

// ---- erlang:spawn_monitor/1 ----

#[test]
fn spawn_monitor_1_with_zero_arity_closure() {
    let mut process = Process::new(1, 128);
    let (f, mut ctx) = attached_spawn_ctx(42, 7, 1, &mut process);
    let mut heap = [0u64; 7];
    let fun = write_closure(&mut heap, Atom::OK, 0, 0, 1, 0, &[]).expect("closure");
    let result = bif_spawn_monitor_1(&[fun], &mut ctx).expect("spawn_monitor/1 succeeds");
    assert_spawn_monitor_tuple(result, 42, 7);
    let records = f.lambda_monitor_records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].caller_pid, 1);
}

#[test]
fn spawn_monitor_1_badarg_non_zero_arity() {
    let mut process = Process::new(1, 128);
    let (_, mut ctx) = attached_spawn_ctx(42, 7, 1, &mut process);
    let mut heap = [0u64; 7];
    let fun = write_closure(&mut heap, Atom::OK, 0, 2, 1, 0, &[]).expect("closure");
    assert_eq!(bif_spawn_monitor_1(&[fun], &mut ctx), Err(badarg()));
}

#[test]
fn spawn_monitor_1_badarg_with_captures() {
    let mut process = Process::new(1, 128);
    let (_, mut ctx) = attached_spawn_ctx(42, 7, 1, &mut process);
    let free_vars = [Term::small_int(1)];
    let mut heap = [0u64; 8];
    let fun = write_closure(&mut heap, Atom::OK, 0, 0, 1, 0, &free_vars).expect("closure");
    assert_eq!(bif_spawn_monitor_1(&[fun], &mut ctx), Err(badarg()));
}

#[test]
fn spawn_monitor_1_badarg_non_closure() {
    let mut process = Process::new(1, 128);
    let (_, mut ctx) = attached_spawn_ctx(42, 7, 1, &mut process);
    assert_eq!(
        bif_spawn_monitor_1(&[Term::small_int(42)], &mut ctx),
        Err(badarg())
    );
}

// ---- spawn options ----

#[test]
fn parse_spawn_options_link_monitor_priority() {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let mut ctx = ProcessContext::new();
    ctx.set_atom_table(Some(atom_table.clone()));
    let link = atom_table.intern("link");
    let monitor = atom_table.intern("monitor");
    let priority = atom_table.intern("priority");
    let high = atom_table.intern("high");
    let mut tuple_heap = [0u64; 3];
    let priority_tuple = write_tuple(&mut tuple_heap, &[Term::atom(priority), Term::atom(high)])
        .expect("priority tuple");
    let mut c3 = [0u64; 2];
    let tail = write_cons(&mut c3, priority_tuple, Term::NIL).expect("tail cons");
    let mut c2 = [0u64; 2];
    let tail = write_cons(&mut c2, Term::atom(monitor), tail).expect("monitor cons");
    let mut c1 = [0u64; 2];
    let list = write_cons(&mut c1, Term::atom(link), tail).expect("link cons");

    let options = parse_spawn_options(list, &ctx).expect("options parse");
    assert!(options.link);
    assert!(options.monitor);
    assert_eq!(options.priority, Some(Priority::High));
    assert_eq!(options.min_heap_size, None);
}

#[test]
fn parse_spawn_options_min_heap_size() {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let mut ctx = ProcessContext::new();
    ctx.set_atom_table(Some(atom_table.clone()));
    let min_heap_size = atom_table.intern("min_heap_size");
    let mut tuple_heap = [0u64; 3];
    let heap_tuple = write_tuple(
        &mut tuple_heap,
        &[Term::atom(min_heap_size), Term::small_int(512)],
    )
    .expect("min_heap_size tuple");
    let mut cons_heap = [0u64; 2];
    let list = write_cons(&mut cons_heap, heap_tuple, Term::NIL).expect("options list");

    let options = parse_spawn_options(list, &ctx).expect("options parse");
    assert_eq!(options.min_heap_size, Some(512));
}

#[test]
fn parse_spawn_options_capabilities() {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let mut ctx = ProcessContext::new();
    ctx.set_atom_table(Some(atom_table.clone()));
    let capabilities = atom_table.intern("capabilities");
    let file = atom_table.intern("file");
    let spawn = atom_table.intern("spawn");

    let mut capability_tail_heap = [0u64; 2];
    let capability_tail = write_cons(&mut capability_tail_heap, Term::atom(spawn), Term::NIL)
        .expect("spawn capability cons");
    let mut capability_head_heap = [0u64; 2];
    let capability_list = write_cons(&mut capability_head_heap, Term::atom(file), capability_tail)
        .expect("file capability cons");
    let mut tuple_heap = [0u64; 3];
    let capability_tuple = write_tuple(
        &mut tuple_heap,
        &[Term::atom(capabilities), capability_list],
    )
    .expect("capabilities tuple");
    let mut cons_heap = [0u64; 2];
    let list = write_cons(&mut cons_heap, capability_tuple, Term::NIL).expect("options list");

    let options = parse_spawn_options(list, &ctx).expect("options parse");
    let requested = options.capabilities.expect("capabilities option");
    assert!(requested.contains(Capability::ExternalIo));
    assert!(requested.contains(Capability::Spawn));
    assert!(!requested.contains(Capability::Clock));
}

#[test]
fn parse_spawn_options_sandbox_profiles() {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let mut ctx = ProcessContext::new();
    ctx.set_atom_table(Some(atom_table.clone()));

    let pure_options = sandbox_option_list(&atom_table, "pure");
    let pure = parse_spawn_options(pure_options, &ctx)
        .expect("pure sandbox options")
        .capabilities
        .expect("pure sandbox capabilities");
    assert_eq!(pure, Sandbox::Pure.capabilities());

    let worker_options = sandbox_option_list(&atom_table, "worker");
    let worker = parse_spawn_options(worker_options, &ctx)
        .expect("worker sandbox options")
        .capabilities
        .expect("worker sandbox capabilities");
    assert_eq!(worker, Sandbox::Worker.capabilities());

    let supervisor_options = sandbox_option_list(&atom_table, "supervisor");
    let supervisor = parse_spawn_options(supervisor_options, &ctx)
        .expect("supervisor sandbox options")
        .capabilities
        .expect("supervisor sandbox capabilities");
    assert_eq!(supervisor, Sandbox::Supervisor.capabilities());

    let unrestricted_options = sandbox_option_list(&atom_table, "unrestricted");
    let unrestricted = parse_spawn_options(unrestricted_options, &ctx)
        .expect("unrestricted sandbox options")
        .capabilities
        .expect("unrestricted sandbox capabilities");
    assert_eq!(unrestricted, Sandbox::Unrestricted.capabilities());
}

#[test]
fn parse_spawn_options_rejects_malformed_sandbox_options() {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let mut ctx = ProcessContext::new();
    ctx.set_atom_table(Some(atom_table.clone()));
    let sandbox = atom_table.intern("sandbox");
    let unknown = atom_table.intern("isolated");

    let mut malformed_tuple_heap = [0u64; 2];
    let malformed_tuple = write_tuple(&mut malformed_tuple_heap, &[Term::atom(sandbox)])
        .expect("malformed sandbox tuple");
    let mut malformed_list_heap = [0u64; 2];
    let malformed_list = write_cons(&mut malformed_list_heap, malformed_tuple, Term::NIL)
        .expect("malformed sandbox option list");
    assert_eq!(parse_spawn_options(malformed_list, &ctx), Err(badarg()));

    let mut unknown_tuple_heap = [0u64; 3];
    let unknown_tuple = write_tuple(
        &mut unknown_tuple_heap,
        &[Term::atom(sandbox), Term::atom(unknown)],
    )
    .expect("unknown sandbox tuple");
    let mut unknown_list_heap = [0u64; 2];
    let unknown_list =
        write_cons(&mut unknown_list_heap, unknown_tuple, Term::NIL).expect("unknown option list");
    assert_eq!(parse_spawn_options(unknown_list, &ctx), Err(badarg()));
}

#[test]
fn parse_spawn_options_ignores_unknown_atoms_and_tuples() {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let mut ctx = ProcessContext::new();
    ctx.set_atom_table(Some(atom_table.clone()));
    let unknown = atom_table.intern("fullsweep_after");
    let unknown_atom = atom_table.intern("message_queue_data");
    let mut tuple_heap = [0u64; 3];
    let unknown_tuple = write_tuple(&mut tuple_heap, &[Term::atom(unknown), Term::small_int(10)])
        .expect("unknown tuple");
    let mut c2 = [0u64; 2];
    let tail = write_cons(&mut c2, unknown_tuple, Term::NIL).expect("tail cons");
    let mut c1 = [0u64; 2];
    let list = write_cons(&mut c1, Term::atom(unknown_atom), tail).expect("options list");

    assert_eq!(parse_spawn_options(list, &ctx), Ok(SpawnOptions::default()));
}

#[test]
fn parse_spawn_options_rejects_malformed_supported_options() {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let mut ctx = ProcessContext::new();
    ctx.set_atom_table(Some(atom_table.clone()));
    let priority = atom_table.intern("priority");
    let min_heap_size = atom_table.intern("min_heap_size");
    let sandbox = atom_table.intern("sandbox");

    let mut priority_heap = [0u64; 2];
    let priority_tuple =
        write_tuple(&mut priority_heap, &[Term::atom(priority)]).expect("malformed priority tuple");
    let mut priority_list_heap = [0u64; 2];
    let priority_list = write_cons(&mut priority_list_heap, priority_tuple, Term::NIL)
        .expect("priority option list");
    assert_eq!(parse_spawn_options(priority_list, &ctx), Err(badarg()));

    let mut heap_tuple_heap = [0u64; 3];
    let heap_tuple = write_tuple(
        &mut heap_tuple_heap,
        &[Term::atom(min_heap_size), Term::small_int(-1)],
    )
    .expect("negative min_heap_size tuple");
    let mut heap_list_heap = [0u64; 2];
    let heap_list =
        write_cons(&mut heap_list_heap, heap_tuple, Term::NIL).expect("heap option list");
    assert_eq!(parse_spawn_options(heap_list, &ctx), Err(badarg()));

    let mut sandbox_heap = [0u64; 2];
    let sandbox_tuple =
        write_tuple(&mut sandbox_heap, &[Term::atom(sandbox)]).expect("malformed sandbox tuple");
    let mut sandbox_list_heap = [0u64; 2];
    let sandbox_list =
        write_cons(&mut sandbox_list_heap, sandbox_tuple, Term::NIL).expect("sandbox option list");
    assert_eq!(parse_spawn_options(sandbox_list, &ctx), Err(badarg()));
}

#[test]
fn spawn_in_sandbox_uses_sandbox_capabilities() {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let facility = Arc::new(MockSpawnFacility::new(35));
    let mut ctx = ProcessContext::new();
    ctx.set_pid(Some(3));
    ctx.set_atom_table(Some(atom_table));
    ctx.set_spawn_facility(Some(facility.clone()));

    let result = spawn_in_sandbox(Sandbox::Pure, Atom::OK, Atom::ERROR, Vec::new(), &mut ctx);

    assert_eq!(result, Ok(Term::pid(35)));
    let records = facility.options_records();
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].options.capabilities.as_ref(),
        Some(&Sandbox::Pure.capabilities())
    );
}

#[test]
fn spawn_opt_sandbox_attenuates_but_never_escalates() {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let facility = Arc::new(MockSpawnFacility::new(23));
    let mut process = Process::with_capabilities(3, 128, Sandbox::Worker.capabilities());

    let worker_options = sandbox_option_list(&atom_table, "worker");
    {
        let mut ctx = ProcessContext::new();
        ctx.set_pid(Some(3));
        ctx.set_atom_table(Some(atom_table.clone()));
        ctx.set_spawn_facility(Some(facility.clone()));
        ctx.attach_process(&mut process, 0);
        let allowed = bif_spawn_opt_4(
            &[
                Term::atom(Atom::OK),
                Term::atom(Atom::ERROR),
                Term::NIL,
                worker_options,
            ],
            &mut ctx,
        );
        ctx.detach_process();
        assert_eq!(allowed, Ok(Term::pid(23)));
    }
    let records = facility.options_records();
    assert_eq!(
        records[0].options.capabilities.as_ref(),
        Some(&Sandbox::Worker.capabilities())
    );

    let supervisor_options = sandbox_option_list(&atom_table, "supervisor");
    {
        let mut ctx = ProcessContext::new();
        ctx.set_pid(Some(3));
        ctx.set_atom_table(Some(atom_table.clone()));
        ctx.set_spawn_facility(Some(facility.clone()));
        ctx.attach_process(&mut process, 0);
        let denied = bif_spawn_opt_4(
            &[
                Term::atom(Atom::OK),
                Term::atom(Atom::ERROR),
                Term::NIL,
                supervisor_options,
            ],
            &mut ctx,
        );
        ctx.detach_process();
        assert_eq!(denied, Err(badarg()));
    }
}

#[test]
fn spawn_opt_capabilities_attenuate_but_never_escalate() {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let facility = Arc::new(MockSpawnFacility::new(22));
    let mut process =
        Process::with_capabilities(3, 128, CapabilitySet::from_slice(&[Capability::ExternalIo]));
    let mut ctx = ProcessContext::new();
    ctx.set_pid(Some(3));
    ctx.set_atom_table(Some(atom_table.clone()));
    ctx.set_spawn_facility(Some(facility.clone()));

    let file_options = capabilities_option_list(&atom_table, &["file"]);
    {
        let mut ctx = ProcessContext::new();
        ctx.set_pid(Some(3));
        ctx.set_atom_table(Some(atom_table.clone()));
        ctx.set_spawn_facility(Some(facility.clone()));
        ctx.attach_process(&mut process, 0);
        let allowed = bif_spawn_opt_4(
            &[
                Term::atom(Atom::OK),
                Term::atom(Atom::ERROR),
                Term::NIL,
                file_options,
            ],
            &mut ctx,
        );
        ctx.detach_process();
        assert_eq!(allowed, Ok(Term::pid(22)));
    }
    let requested = facility.options_records()[0]
        .options
        .capabilities
        .clone()
        .expect("requested capabilities");
    assert_eq!(
        requested,
        CapabilitySet::from_slice(&[Capability::ExternalIo])
    );

    let escalating_options = capabilities_option_list(&atom_table, &["file", "spawn"]);
    {
        let mut ctx = ProcessContext::new();
        ctx.set_pid(Some(3));
        ctx.set_atom_table(Some(atom_table.clone()));
        ctx.set_spawn_facility(Some(facility.clone()));
        ctx.attach_process(&mut process, 0);
        let denied = bif_spawn_opt_4(
            &[
                Term::atom(Atom::OK),
                Term::atom(Atom::ERROR),
                Term::NIL,
                escalating_options,
            ],
            &mut ctx,
        );
        ctx.detach_process();
        assert_eq!(denied, Err(badarg()));
    }
}

fn sandbox_option_list(atom_table: &AtomTable, profile: &str) -> Term {
    let mut tuple_heap = Box::new([0u64; 3]);
    let sandbox_tuple = write_tuple(
        tuple_heap.as_mut(),
        &[
            Term::atom(atom_table.intern("sandbox")),
            Term::atom(atom_table.intern(profile)),
        ],
    )
    .expect("sandbox tuple");
    Box::leak(tuple_heap);
    let mut option_heap = Box::new([0u64; 2]);
    let options = write_cons(option_heap.as_mut(), sandbox_tuple, Term::NIL).expect("options cons");
    Box::leak(option_heap);
    options
}

fn capabilities_option_list(atom_table: &AtomTable, names: &[&str]) -> Term {
    let mut capability_list = Term::NIL;
    for name in names.iter().rev() {
        let mut cons_heap = Box::new([0u64; 2]);
        capability_list = write_cons(
            cons_heap.as_mut(),
            Term::atom(atom_table.intern(name)),
            capability_list,
        )
        .expect("capability cons");
        Box::leak(cons_heap);
    }
    let mut tuple_heap = Box::new([0u64; 3]);
    let capability_tuple = write_tuple(
        tuple_heap.as_mut(),
        &[
            Term::atom(atom_table.intern("capabilities")),
            capability_list,
        ],
    )
    .expect("capabilities tuple");
    Box::leak(tuple_heap);
    let mut option_heap = Box::new([0u64; 2]);
    let options =
        write_cons(option_heap.as_mut(), capability_tuple, Term::NIL).expect("options cons");
    Box::leak(option_heap);
    options
}

// ---- erlang:spawn_opt/4 ----

#[test]
fn spawn_opt_4_without_monitor_returns_pid() {
    let (f, mut ctx) = spawn_ctx(11, 3);
    let result = bif_spawn_opt_4(
        &[
            Term::atom(Atom::OK),
            Term::atom(Atom::ERROR),
            Term::NIL,
            Term::NIL,
        ],
        &mut ctx,
    );
    assert_eq!(result, Ok(Term::pid(11)));
    let records = f.options_records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].caller_pid, 3);
    assert_eq!(records[0].module, Atom::OK);
    assert_eq!(records[0].function, Atom::ERROR);
    assert!(records[0].args.is_empty());
    assert!(!records[0].options.monitor);
}

#[test]
fn spawn_opt_4_with_monitor_returns_pid_and_reference() {
    let mut process = Process::new(3, 128);
    let (f, mut ctx) = attached_spawn_ctx(12, 44, 3, &mut process);
    let monitor = ctx.atom_table().expect("atom table").intern("monitor");
    let mut c1 = [0u64; 2];
    let options = write_cons(&mut c1, Term::atom(monitor), Term::NIL).expect("options");
    let result = bif_spawn_opt_4(
        &[
            Term::atom(Atom::OK),
            Term::atom(Atom::ERROR),
            Term::NIL,
            options,
        ],
        &mut ctx,
    )
    .expect("spawn_opt/4 succeeds");
    assert_spawn_monitor_tuple(result, 12, 44);
    assert!(f.options_records()[0].options.monitor);
}

#[test]
fn spawn_opt_4_records_link_and_priority() {
    let (f, mut ctx) = spawn_ctx(13, 5);
    let atom_table = ctx.atom_table().expect("atom table");
    let link = atom_table.intern("link");
    let priority = atom_table.intern("priority");
    let high = atom_table.intern("high");
    let mut tuple_heap = [0u64; 3];
    let priority_tuple = write_tuple(&mut tuple_heap, &[Term::atom(priority), Term::atom(high)])
        .expect("priority tuple");
    let mut c2 = [0u64; 2];
    let tail = write_cons(&mut c2, priority_tuple, Term::NIL).expect("tail cons");
    let mut c1 = [0u64; 2];
    let options = write_cons(&mut c1, Term::atom(link), tail).expect("options");

    assert_eq!(
        bif_spawn_opt_4(
            &[
                Term::atom(Atom::OK),
                Term::atom(Atom::ERROR),
                Term::NIL,
                options,
            ],
            &mut ctx,
        ),
        Ok(Term::pid(13)),
    );
    let records = f.options_records();
    assert!(records[0].options.link);
    assert_eq!(records[0].options.priority, Some(Priority::High));
}

// ---- erlang:spawn_opt/2 ----

#[test]
fn spawn_opt_2_with_zero_arity_closure_and_monitor_returns_tuple() {
    let mut process = Process::new(1, 128);
    let (f, mut ctx) = attached_spawn_ctx(42, 77, 1, &mut process);
    let monitor = ctx.atom_table().expect("atom table").intern("monitor");
    let mut options_heap = [0u64; 2];
    let options =
        write_cons(&mut options_heap, Term::atom(monitor), Term::NIL).expect("monitor option list");
    let mut heap = [0u64; 7];
    let fun = write_closure(&mut heap, Atom::OK, 0, 0, 1, 0, &[]).expect("closure");

    let result = bif_spawn_opt_2(&[fun, options], &mut ctx).expect("spawn_opt/2 succeeds");
    assert_spawn_monitor_tuple(result, 42, 77);
    let records = f.lambda_options_records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].caller_pid, 1);
    assert!(records[0].options.monitor);
}

// ---- erlang:link/1 ----

#[test]
fn link_establishes_bidirectional_link() {
    let (f, mut ctx) = link_ctx(1);
    assert_eq!(
        bif_link(&[Term::pid(2)], &mut ctx),
        Ok(Term::atom(Atom::TRUE))
    );
    assert_eq!(
        f.records(),
        vec![LinkRecord::Link {
            caller_pid: 1,
            target_pid: 2
        }]
    );
}

#[test]
fn link_self_is_noop() {
    let (f, mut ctx) = link_ctx(1);
    assert_eq!(
        bif_link(&[Term::pid(1)], &mut ctx),
        Ok(Term::atom(Atom::TRUE))
    );
    assert!(f.records().is_empty());
}

#[test]
fn link_noproc_for_dead_target() {
    let f: Arc<dyn LinkFacility> = Arc::new(NoprocLinkFacility);
    let mut ctx = ProcessContext::new();
    ctx.set_pid(Some(1));
    ctx.set_link_facility(Some(f));
    assert_eq!(
        bif_link(&[Term::pid(2)], &mut ctx),
        Err(Term::atom(Atom::NOPROC))
    );
}

#[test]
fn link_badarg_no_pid() {
    let mut ctx = ProcessContext::new();
    assert_eq!(bif_link(&[Term::pid(2)], &mut ctx), Err(badarg()));
}

#[test]
fn link_badarg_non_pid_arg() {
    let (_, mut ctx) = link_ctx(1);
    assert_eq!(bif_link(&[Term::small_int(2)], &mut ctx), Err(badarg()));
}

// ---- erlang:unlink/1 ----

#[test]
fn unlink_removes_link() {
    let (f, mut ctx) = link_ctx(1);
    assert_eq!(
        bif_unlink(&[Term::pid(2)], &mut ctx),
        Ok(Term::atom(Atom::TRUE))
    );
    assert_eq!(
        f.records(),
        vec![LinkRecord::Unlink {
            caller_pid: 1,
            target_pid: 2
        }]
    );
}

#[test]
fn unlink_self_is_noop() {
    let (f, mut ctx) = link_ctx(1);
    assert_eq!(
        bif_unlink(&[Term::pid(1)], &mut ctx),
        Ok(Term::atom(Atom::TRUE))
    );
    assert!(f.records().is_empty());
}

// ---- erlang:process_flag/2 ----

#[test]
fn process_flag_trap_exit_returns_old_value() {
    let (_, mut ctx) = link_ctx(1);
    assert_eq!(
        bif_process_flag(
            &[Term::atom(Atom::TRAP_EXIT), Term::atom(Atom::TRUE)],
            &mut ctx
        ),
        Ok(Term::atom(Atom::FALSE)),
    );
}

#[test]
fn process_flag_badarg_unknown_flag() {
    let (_, mut ctx) = link_ctx(1);
    assert_eq!(
        bif_process_flag(&[Term::atom(Atom::OK), Term::atom(Atom::TRUE)], &mut ctx),
        Err(badarg()),
    );
}

#[test]
fn process_flag_badarg_non_bool_value() {
    let (_, mut ctx) = link_ctx(1);
    assert_eq!(
        bif_process_flag(
            &[Term::atom(Atom::TRAP_EXIT), Term::atom(Atom::OK)],
            &mut ctx
        ),
        Err(badarg()),
    );
}

// ---- list_to_vec ----

#[test]
fn list_to_vec_empty() {
    assert!(list_to_vec(Term::NIL).unwrap().is_empty());
}

#[test]
fn list_to_vec_proper() {
    let mut c2 = [0u64; 2];
    let tail = write_cons(&mut c2, Term::small_int(2), Term::NIL).unwrap();
    let mut c1 = [0u64; 2];
    let list = write_cons(&mut c1, Term::small_int(1), tail).unwrap();
    assert_eq!(
        list_to_vec(list).unwrap(),
        vec![Term::small_int(1), Term::small_int(2)]
    );
}

#[test]
fn list_to_vec_rejects_non_list() {
    assert_eq!(list_to_vec(Term::small_int(42)), Err(badarg()));
}

// ---- Registration ----

#[test]
fn register_gate2_bifs_registers_all() {
    let at = AtomTable::new();
    let reg = BifRegistryImpl::new();
    register_gate2_bifs(&reg, &at).expect("gate 2 registration");
    let erlang = at.intern("erlang");
    for (name, arity) in [
        ("self", 0),
        ("spawn", 3),
        ("spawn", 4),
        ("spawn_link", 3),
        ("spawn_link", 4),
        ("spawn_monitor", 1),
        ("spawn_monitor", 3),
        ("spawn_monitor", 4),
        ("spawn_opt", 2),
        ("spawn_opt", 4),
        ("link", 1),
        ("unlink", 1),
        ("process_flag", 2),
        ("monitor", 2),
        ("demonitor", 1),
        ("exit", 1),
        ("exit", 2),
    ] {
        assert!(
            reg.lookup(erlang, at.intern(name), arity).is_some(),
            "missing erlang:{name}/{arity}"
        );
    }
}

#[test]
fn register_gate2_bifs_fails_twice() {
    let at = AtomTable::new();
    let reg = BifRegistryImpl::new();
    register_gate2_bifs(&reg, &at).expect("first");
    assert!(register_gate2_bifs(&reg, &at).is_err());
}

#[test]
fn gate1_and_gate2_coexist() {
    let at = AtomTable::new();
    let reg = BifRegistryImpl::new();
    crate::native::bifs::register_gate1_bifs(&reg, &at).expect("gate 1");
    register_gate2_bifs(&reg, &at).expect("gate 2");
    let erlang = at.intern("erlang");
    assert!(reg.lookup(erlang, at.intern("+"), 2).is_some());
    assert!(reg.lookup(erlang, at.intern("self"), 0).is_some());
    assert!(reg.lookup(erlang, at.intern("monitor"), 2).is_some());
}

// ---- erlang:monitor/2 ----

#[test]
fn monitor_returns_reference() {
    let (f, mut ctx) = sup_ctx(42, 1);
    let result = bif_monitor(&[Term::atom(Atom::PROCESS), Term::pid(2)], &mut ctx);
    assert_eq!(result, Ok(Term::small_int(42)));
    assert_eq!(
        f.records(),
        vec![SupervisionRecord::Monitor {
            caller_pid: 1,
            target_pid: 2
        }]
    );
}

#[test]
fn monitor_badarg_non_process_type() {
    let (_, mut ctx) = sup_ctx(1, 1);
    assert_eq!(
        bif_monitor(&[Term::atom(Atom::OK), Term::pid(2)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn monitor_badarg_non_pid_target() {
    let (_, mut ctx) = sup_ctx(1, 1);
    assert_eq!(
        bif_monitor(&[Term::atom(Atom::PROCESS), Term::small_int(2)], &mut ctx),
        Err(badarg()),
    );
}

#[test]
fn monitor_badarg_no_facility() {
    let mut ctx = ProcessContext::new();
    ctx.set_pid(Some(1));
    assert_eq!(
        bif_monitor(&[Term::atom(Atom::PROCESS), Term::pid(2)], &mut ctx),
        Err(badarg()),
    );
}

// ---- erlang:demonitor/1 ----

#[test]
fn demonitor_returns_true() {
    let (f, mut ctx) = sup_ctx(0, 1);
    assert_eq!(
        bif_demonitor(&[Term::small_int(42)], &mut ctx),
        Ok(Term::atom(Atom::TRUE))
    );
    assert_eq!(
        f.records(),
        vec![SupervisionRecord::Demonitor {
            caller_pid: 1,
            reference: 42
        }]
    );
}

#[test]
fn demonitor_badarg_negative_ref() {
    let (_, mut ctx) = sup_ctx(0, 1);
    assert_eq!(
        bif_demonitor(&[Term::small_int(-1)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn demonitor_badarg_non_integer() {
    let (_, mut ctx) = sup_ctx(0, 1);
    assert_eq!(
        bif_demonitor(&[Term::atom(Atom::OK)], &mut ctx),
        Err(badarg())
    );
}

// ---- erlang:exit/1 and erlang:exit/2 ----

#[test]
fn exit_1_returns_reason_and_sets_exit_class() {
    let mut ctx = ProcessContext::new();
    assert_eq!(
        bif_exit_1(&[Term::atom(Atom::OK)], &mut ctx),
        Err(Term::atom(Atom::OK))
    );
    assert_eq!(ctx.take_exception_class(), ExceptionClass::Exit);
}

#[test]
fn exit_1_badarg_wrong_arity_does_not_set_exit_class() {
    let mut ctx = ProcessContext::new();
    assert_eq!(bif_exit_1(&[], &mut ctx), Err(badarg()));
    assert_eq!(ctx.take_exception_class(), ExceptionClass::Error);
}

#[test]
fn exit_sends_signal_and_returns_true() {
    let (f, mut ctx) = sup_ctx(0, 1);
    assert_eq!(
        bif_exit(&[Term::pid(2), Term::atom(Atom::KILL)], &mut ctx),
        Ok(Term::atom(Atom::TRUE))
    );
    assert_eq!(
        f.records(),
        vec![SupervisionRecord::ExitSignal {
            caller_pid: 1,
            target_pid: 2,
            reason: ExitReason::Kill
        }]
    );
}

#[test]
fn exit_normal_reason() {
    let (f, mut ctx) = sup_ctx(0, 1);
    assert_eq!(
        bif_exit(&[Term::pid(2), Term::atom(Atom::NORMAL)], &mut ctx),
        Ok(Term::atom(Atom::TRUE))
    );
    assert_eq!(
        f.records()[0],
        SupervisionRecord::ExitSignal {
            caller_pid: 1,
            target_pid: 2,
            reason: ExitReason::Normal
        }
    );
}

#[test]
fn exit_badarg_non_pid() {
    let (_, mut ctx) = sup_ctx(0, 1);
    assert_eq!(
        bif_exit(&[Term::small_int(2), Term::atom(Atom::KILL)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn exit_badarg_unknown_reason_atom() {
    let (_, mut ctx) = sup_ctx(0, 1);
    assert_eq!(
        bif_exit(&[Term::pid(2), Term::atom(Atom::OK)], &mut ctx),
        Err(badarg())
    );
}

// ---- Remote spawn BIFs ----

#[test]
fn remote_spawn_returns_external_pid_for_requested_node() {
    let (facility, mut ctx, node, module, function) = remote_spawn_ctx(77, None);

    let result = bif_spawn_4(
        &[
            Term::atom(node),
            Term::atom(module),
            Term::atom(function),
            Term::NIL,
        ],
        &mut ctx,
    )
    .expect("remote spawn succeeds");

    let pid = PidRef::new(result).expect("external pid");
    assert!(!pid.is_local());
    assert_eq!(pid.node(), Some(node));
    assert_eq!(pid.pid_number(), 77);
    let records = facility.records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].0, 10);
    assert_eq!(records[0].1, node);
    assert_eq!(records[0].2, module);
    assert_eq!(records[0].3, function);
    assert!(!records[0].5.link);
    assert!(!records[0].5.monitor);
}

#[test]
fn remote_spawn_link_sends_link_option() {
    let (facility, mut ctx, node, module, function) = remote_spawn_ctx(78, None);

    let result = bif_spawn_link_4(
        &[
            Term::atom(node),
            Term::atom(module),
            Term::atom(function),
            Term::NIL,
        ],
        &mut ctx,
    )
    .expect("remote spawn_link succeeds");

    assert_eq!(PidRef::new(result).and_then(|pid| pid.node()), Some(node));
    let records = facility.records();
    assert!(records[0].5.link);
    assert!(!records[0].5.monitor);
}

#[test]
fn remote_spawn_monitor_returns_external_pid_and_reference() {
    let (facility, mut ctx, node, module, function) = remote_spawn_ctx(79, Some(900));

    let result = bif_spawn_monitor_4(
        &[
            Term::atom(node),
            Term::atom(module),
            Term::atom(function),
            Term::NIL,
        ],
        &mut ctx,
    )
    .expect("remote spawn_monitor succeeds");

    let tuple = Tuple::new(result).expect("spawn_monitor tuple");
    assert_eq!(tuple.arity(), 2);
    let pid = PidRef::new(tuple.get(0).expect("pid element")).expect("external pid");
    assert_eq!(pid.node(), Some(node));
    assert_eq!(pid.pid_number(), 79);
    assert!(
        crate::term::reference_ref::ReferenceRef::new(tuple.get(1).expect("ref element"))
            .and_then(|reference| reference.node())
            .is_some()
    );
    let records = facility.records();
    assert!(records[0].5.monitor);
    assert!(!records[0].5.link);
}

#[test]
fn remote_spawn_arities_are_registered() {
    let registry = BifRegistryImpl::new();
    let atoms = AtomTable::with_common_atoms();
    register_gate2_bifs(&registry, &atoms).expect("register gate2 bifs");
    let erlang = atoms.intern("erlang");
    for (name, arity, expected_capability) in [
        ("spawn", 3, Capability::Spawn),
        ("spawn", 4, Capability::Spawn),
        ("spawn_link", 3, Capability::Spawn),
        ("spawn_link", 4, Capability::Spawn),
        ("spawn_monitor", 1, Capability::Spawn),
        ("spawn_monitor", 3, Capability::Spawn),
        ("spawn_monitor", 4, Capability::Spawn),
        ("spawn_opt", 2, Capability::Spawn),
        ("spawn_opt", 4, Capability::Spawn),
        ("link", 1, Capability::ProcessLocal),
        ("unlink", 1, Capability::ProcessLocal),
        ("monitor", 2, Capability::ProcessLocal),
        ("demonitor", 1, Capability::ProcessLocal),
    ] {
        let entry = registry
            .lookup(erlang, atoms.intern(name), arity)
            .unwrap_or_else(|| panic!("missing erlang:{name}/{arity}"));
        assert_eq!(entry.capability, expected_capability);
    }
}

#[test]
fn remote_spawn_badarg_without_facility() {
    let atoms = Arc::new(AtomTable::with_common_atoms());
    let node = atoms.intern("remote@host");
    let module = atoms.intern("sample");
    let function = atoms.intern("run");
    let mut process = Process::new(10, 128);
    let mut ctx = ProcessContext::new();
    ctx.attach_process(&mut process, 0);
    ctx.set_pid(Some(10));
    ctx.set_atom_table(Some(atoms));

    assert_eq!(
        bif_spawn_4(
            &[
                Term::atom(node),
                Term::atom(module),
                Term::atom(function),
                Term::NIL,
            ],
            &mut ctx,
        ),
        Err(badarg()),
    );
}

#[test]
fn remote_spawn_rejects_reply_for_unrequested_node() {
    let (facility, mut ctx, node, module, function) = remote_spawn_ctx(80, None);
    facility.set_reply_node(Atom::OK);

    assert_eq!(
        bif_spawn_4(
            &[
                Term::atom(node),
                Term::atom(module),
                Term::atom(function),
                Term::NIL,
            ],
            &mut ctx,
        ),
        Err(badarg()),
    );
}

// ---- Helpers ----

fn spawn_ctx(next_pid: u64, caller_pid: u64) -> (Arc<MockSpawnFacility>, ProcessContext<'static>) {
    let f = Arc::new(MockSpawnFacility::new(next_pid));
    let mut ctx = ProcessContext::new();
    ctx.set_pid(Some(caller_pid));
    ctx.set_atom_table(Some(Arc::new(AtomTable::with_common_atoms())));
    ctx.set_spawn_facility(Some(f.clone()));
    (f, ctx)
}

fn link_ctx(caller_pid: u64) -> (Arc<MockLinkFacility>, ProcessContext<'static>) {
    let f = Arc::new(MockLinkFacility::new());
    let mut ctx = ProcessContext::new();
    ctx.set_pid(Some(caller_pid));
    ctx.set_link_facility(Some(f.clone()));
    (f, ctx)
}

fn sup_ctx(
    next_ref: u64,
    caller_pid: u64,
) -> (Arc<MockSupervisionFacility>, ProcessContext<'static>) {
    let f = Arc::new(MockSupervisionFacility::new(next_ref));
    let mut ctx = ProcessContext::new();
    ctx.set_pid(Some(caller_pid));
    ctx.set_supervision_facility(Some(f.clone()));
    (f, ctx)
}

// ---- Mock spawn facility ----

#[derive(Clone)]
struct LambdaSpawnRecord {
    caller_pid: u64,
}

#[derive(Clone)]
struct SpawnOptionsRecord {
    caller_pid: u64,
    module: Atom,
    function: Atom,
    args: Vec<Term>,
    options: SpawnOptions,
}

#[derive(Clone)]
struct LambdaOptionsRecord {
    caller_pid: u64,
    options: SpawnOptions,
}

struct MockSpawnFacility {
    next_pid: u64,
    next_reference: u64,
    records: Mutex<Vec<SpawnRecord>>,
    spawn_monitor_records: Mutex<Vec<SpawnRecord>>,
    lambda_monitor_records: Mutex<Vec<LambdaSpawnRecord>>,
    options_records: Mutex<Vec<SpawnOptionsRecord>>,
    lambda_options_records: Mutex<Vec<LambdaOptionsRecord>>,
}

impl MockSpawnFacility {
    fn new(next_pid: u64) -> Self {
        Self::with_reference(next_pid, 0)
    }

    fn with_reference(next_pid: u64, next_reference: u64) -> Self {
        Self {
            next_pid,
            next_reference,
            records: Mutex::new(Vec::new()),
            spawn_monitor_records: Mutex::new(Vec::new()),
            lambda_monitor_records: Mutex::new(Vec::new()),
            options_records: Mutex::new(Vec::new()),
            lambda_options_records: Mutex::new(Vec::new()),
        }
    }

    fn records(&self) -> Vec<SpawnRecord> {
        self.records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn spawn_monitor_records(&self) -> Vec<SpawnRecord> {
        self.spawn_monitor_records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn lambda_monitor_records(&self) -> Vec<LambdaSpawnRecord> {
        self.lambda_monitor_records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
            .collect()
    }

    fn options_records(&self) -> Vec<SpawnOptionsRecord> {
        self.options_records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn lambda_options_records(&self) -> Vec<LambdaOptionsRecord> {
        self.lambda_options_records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

impl SpawnFacility for MockSpawnFacility {
    fn spawn(
        &self,
        caller_pid: u64,
        module: Atom,
        function: Atom,
        args: Vec<Term>,
        link_to: Option<u64>,
    ) -> Result<u64, SpawnError> {
        self.records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(SpawnRecord {
                caller_pid,
                module,
                function,
                args,
                link_to,
            });
        Ok(self.next_pid)
    }

    fn spawn_monitor(
        &self,
        caller_pid: u64,
        module: Atom,
        function: Atom,
        args: Vec<Term>,
    ) -> Result<SpawnMonitorResult, SpawnError> {
        self.spawn_monitor_records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(SpawnRecord {
                caller_pid,
                module,
                function,
                args,
                link_to: None,
            });
        Ok(SpawnMonitorResult {
            pid: self.next_pid,
            reference: self.next_reference,
        })
    }

    fn spawn_lambda(&self, _: u64, _: Atom, _: u32, _: Option<u64>) -> Result<u64, SpawnError> {
        Ok(self.next_pid)
    }

    fn spawn_lambda_monitor(
        &self,
        caller_pid: u64,
        _module: Atom,
        _lambda_index: u32,
    ) -> Result<SpawnMonitorResult, SpawnError> {
        self.lambda_monitor_records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(LambdaSpawnRecord { caller_pid });
        Ok(SpawnMonitorResult {
            pid: self.next_pid,
            reference: self.next_reference,
        })
    }

    fn spawn_with_options(
        &self,
        caller_pid: u64,
        module: Atom,
        function: Atom,
        args: Vec<Term>,
        options: SpawnOptions,
    ) -> Result<SpawnOptionsResult, SpawnError> {
        let monitor = options.monitor;
        self.options_records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(SpawnOptionsRecord {
                caller_pid,
                module,
                function,
                args,
                options,
            });
        Ok(SpawnOptionsResult {
            pid: self.next_pid,
            reference: monitor.then_some(self.next_reference),
        })
    }

    fn spawn_lambda_with_options(
        &self,
        caller_pid: u64,
        _module: Atom,
        _lambda_index: u32,
        options: SpawnOptions,
    ) -> Result<SpawnOptionsResult, SpawnError> {
        let monitor = options.monitor;
        self.lambda_options_records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(LambdaOptionsRecord {
                caller_pid,
                options,
            });
        Ok(SpawnOptionsResult {
            pid: self.next_pid,
            reference: monitor.then_some(self.next_reference),
        })
    }
}

struct FailingSpawnFacility;

impl SpawnFacility for FailingSpawnFacility {
    fn spawn(
        &self,
        _: u64,
        _: Atom,
        _: Atom,
        _: Vec<Term>,
        _: Option<u64>,
    ) -> Result<u64, SpawnError> {
        Err(SpawnError::UnresolvedMfa)
    }

    fn spawn_monitor(
        &self,
        _: u64,
        _: Atom,
        _: Atom,
        _: Vec<Term>,
    ) -> Result<SpawnMonitorResult, SpawnError> {
        Err(SpawnError::UnresolvedMfa)
    }

    fn spawn_lambda(&self, _: u64, _: Atom, _: u32, _: Option<u64>) -> Result<u64, SpawnError> {
        Err(SpawnError::UnresolvedMfa)
    }

    fn spawn_lambda_monitor(
        &self,
        _: u64,
        _: Atom,
        _: u32,
    ) -> Result<SpawnMonitorResult, SpawnError> {
        Err(SpawnError::UnresolvedMfa)
    }

    fn spawn_with_options(
        &self,
        _: u64,
        _: Atom,
        _: Atom,
        _: Vec<Term>,
        _: SpawnOptions,
    ) -> Result<SpawnOptionsResult, SpawnError> {
        Err(SpawnError::UnresolvedMfa)
    }

    fn spawn_lambda_with_options(
        &self,
        _: u64,
        _: Atom,
        _: u32,
        _: SpawnOptions,
    ) -> Result<SpawnOptionsResult, SpawnError> {
        Err(SpawnError::UnresolvedMfa)
    }
}

// ---- Mock link facility ----

struct MockLinkFacility {
    records: Mutex<Vec<LinkRecord>>,
}

impl MockLinkFacility {
    fn new() -> Self {
        Self {
            records: Mutex::new(Vec::new()),
        }
    }
    fn records(&self) -> Vec<LinkRecord> {
        self.records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

impl LinkFacility for MockLinkFacility {
    fn link(&self, caller_pid: u64, target_pid: u64) -> Result<(), LinkError> {
        self.records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(LinkRecord::Link {
                caller_pid,
                target_pid,
            });
        Ok(())
    }

    fn unlink(&self, caller_pid: u64, target_pid: u64) -> Result<(), LinkError> {
        self.records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(LinkRecord::Unlink {
                caller_pid,
                target_pid,
            });
        Ok(())
    }

    fn set_trap_exit(&self, _caller_pid: u64, _value: bool) -> Result<bool, LinkError> {
        Ok(false)
    }
}

struct NoprocLinkFacility;

impl LinkFacility for NoprocLinkFacility {
    fn link(&self, _: u64, _: u64) -> Result<(), LinkError> {
        Err(LinkError::NoProc)
    }
    fn unlink(&self, _: u64, _: u64) -> Result<(), LinkError> {
        Err(LinkError::NoProc)
    }
    fn set_trap_exit(&self, _: u64, _: bool) -> Result<bool, LinkError> {
        Err(LinkError::NoProc)
    }
}

// ---- Mock supervision facility ----

struct MockSupervisionFacility {
    next_reference: u64,
    records: Mutex<Vec<SupervisionRecord>>,
}

impl MockSupervisionFacility {
    fn new(next_reference: u64) -> Self {
        Self {
            next_reference,
            records: Mutex::new(Vec::new()),
        }
    }
    fn records(&self) -> Vec<SupervisionRecord> {
        self.records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

impl SupervisionFacility for MockSupervisionFacility {
    fn monitor(&self, caller_pid: u64, target_pid: u64) -> Result<MonitorResult, SupervisionError> {
        self.records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(SupervisionRecord::Monitor {
                caller_pid,
                target_pid,
            });
        Ok(MonitorResult {
            reference: self.next_reference,
            immediate_down: false,
        })
    }

    fn demonitor(&self, caller_pid: u64, reference: u64) -> Result<(), SupervisionError> {
        self.records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(SupervisionRecord::Demonitor {
                caller_pid,
                reference,
            });
        Ok(())
    }

    fn exit_signal(
        &self,
        caller_pid: u64,
        target_pid: u64,
        reason: ExitReason,
    ) -> Result<(), SupervisionError> {
        self.records.lock().unwrap_or_else(|e| e.into_inner()).push(
            SupervisionRecord::ExitSignal {
                caller_pid,
                target_pid,
                reason,
            },
        );
        Ok(())
    }
}

// ---- Mock remote spawn facility ----

type RemoteSpawnRecord = (
    u64,
    crate::atom::Atom,
    crate::atom::Atom,
    crate::atom::Atom,
    Vec<Term>,
    SpawnOptions,
);

struct MockRemoteSpawnFacility {
    pid_number: u64,
    serial: u64,
    monitor_reference: Option<u64>,
    reply_node: Mutex<Option<crate::atom::Atom>>,
    records: Mutex<Vec<RemoteSpawnRecord>>,
}

impl MockRemoteSpawnFacility {
    fn new(pid_number: u64, monitor_reference: Option<u64>) -> Self {
        Self {
            pid_number,
            serial: 0,
            monitor_reference,
            reply_node: Mutex::new(None),
            records: Mutex::new(Vec::new()),
        }
    }

    fn records(&self) -> Vec<RemoteSpawnRecord> {
        self.records
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    fn set_reply_node(&self, node: crate::atom::Atom) {
        *self
            .reply_node
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(node);
    }
}

impl RemoteSpawnFacility for MockRemoteSpawnFacility {
    fn remote_spawn(
        &self,
        caller_pid: u64,
        node: crate::atom::Atom,
        module: crate::atom::Atom,
        function: crate::atom::Atom,
        args: Vec<Term>,
        options: SpawnOptions,
    ) -> Result<RemoteSpawnResult, RemoteSpawnError> {
        self.records
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push((caller_pid, node, module, function, args, options));
        let reply_node = self
            .reply_node
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .unwrap_or(node);
        Ok(RemoteSpawnResult {
            node: reply_node,
            pid_number: self.pid_number,
            serial: self.serial,
            monitor_reference: self.monitor_reference,
        })
    }
}

fn remote_spawn_ctx(
    pid_number: u64,
    monitor_reference: Option<u64>,
) -> (
    Arc<MockRemoteSpawnFacility>,
    ProcessContext<'static>,
    crate::atom::Atom,
    crate::atom::Atom,
    crate::atom::Atom,
) {
    let atoms = Arc::new(AtomTable::with_common_atoms());
    let node = atoms.intern("remote@host");
    let module = atoms.intern("sample");
    let function = atoms.intern("run");
    let facility = Arc::new(MockRemoteSpawnFacility::new(pid_number, monitor_reference));
    let process = Box::leak(Box::new(Process::new(10, 128)));
    let mut ctx = ProcessContext::new();
    ctx.attach_process(process, 0);
    ctx.set_pid(Some(10));
    ctx.set_atom_table(Some(atoms));
    ctx.set_remote_spawn_facility(Some(facility.clone()));
    (facility, ctx, node, module, function)
}

// ---- Remote link mock and tests ----

#[derive(Clone, Debug, Eq, PartialEq)]
enum DistributionRecord {
    Link {
        caller_pid: u64,
        target: RemotePid,
    },
    Unlink {
        caller_pid: u64,
        target: RemotePid,
    },
    Exit {
        caller_pid: u64,
        target: RemotePid,
        reason: ExitReason,
    },
}

struct MockDistributionControlFacility {
    records: Mutex<Vec<DistributionRecord>>,
}

impl MockDistributionControlFacility {
    fn new() -> Self {
        Self {
            records: Mutex::new(Vec::new()),
        }
    }

    fn records(&self) -> Vec<DistributionRecord> {
        self.records
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

impl DistributionControlFacility for MockDistributionControlFacility {
    fn link_remote(&self, caller_pid: u64, target: RemotePid) -> Result<(), RemoteLinkError> {
        self.records
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(DistributionRecord::Link { caller_pid, target });
        Ok(())
    }

    fn unlink_remote(&self, caller_pid: u64, target: RemotePid) -> Result<(), RemoteLinkError> {
        self.records
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(DistributionRecord::Unlink { caller_pid, target });
        Ok(())
    }

    fn exit_remote(
        &self,
        caller_pid: u64,
        target: RemotePid,
        reason: ExitReason,
    ) -> Result<(), RemoteLinkError> {
        self.records
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(DistributionRecord::Exit {
                caller_pid,
                target,
                reason,
            });
        Ok(())
    }
}

fn remote_link_ctx(
    caller_pid: u64,
) -> (
    Arc<MockDistributionControlFacility>,
    ProcessContext<'static>,
) {
    let f = Arc::new(MockDistributionControlFacility::new());
    let mut ctx = ProcessContext::new();
    ctx.set_pid(Some(caller_pid));
    ctx.set_distribution_control_facility(Some(f.clone()));
    (f, ctx)
}

fn remote_pid_term(node: Atom, pid_number: u64, serial: u64) -> Term {
    let words = Box::leak(Box::new([0_u64; 4]));
    write_external_pid(words, node, pid_number, serial).expect("external pid fits")
}

#[test]
fn link_remote_pid_routes_through_distribution_control() {
    let (f, mut ctx) = remote_link_ctx(1);
    let remote = remote_pid_term(Atom::OK, 42, 7);

    assert_eq!(bif_link(&[remote], &mut ctx), Ok(Term::atom(Atom::TRUE)));

    assert_eq!(
        f.records(),
        vec![DistributionRecord::Link {
            caller_pid: 1,
            target: RemotePid {
                node: Atom::OK,
                pid_number: 42,
                serial: 7,
            },
        }]
    );
}

#[test]
fn unlink_remote_pid_routes_through_distribution_control() {
    let (f, mut ctx) = remote_link_ctx(1);
    let remote = remote_pid_term(Atom::OK, 42, 7);

    assert_eq!(bif_unlink(&[remote], &mut ctx), Ok(Term::atom(Atom::TRUE)));

    assert_eq!(
        f.records(),
        vec![DistributionRecord::Unlink {
            caller_pid: 1,
            target: RemotePid {
                node: Atom::OK,
                pid_number: 42,
                serial: 7,
            },
        }]
    );
}
