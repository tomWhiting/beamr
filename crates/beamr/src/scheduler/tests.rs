use std::collections::HashMap as StdHashMap;
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};
use std::task::{Context, Poll, Wake, Waker};

#[cfg(feature = "telemetry")]
use opentelemetry::Key;
#[cfg(feature = "telemetry")]
use opentelemetry::trace::{TraceContextExt, Tracer};
#[cfg(feature = "telemetry")]
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData, ResourceMetrics};
#[cfg(feature = "telemetry")]
use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};
#[cfg(feature = "telemetry")]
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};

use dashmap::{DashMap, DashSet};

use super::*;
use crate::atom::{Atom, AtomTable};
use crate::distribution::{ResolveError, ResolveFuture};
use crate::ets::{EtsTableMetadata, EtsTableType, Protection};
use crate::hook::{Hook, HookDecision};
use crate::io::{NullSink, RingConfig};
use crate::loader::Instruction;
use crate::loader::decode::compact::Operand;
use crate::mailbox::Mailbox;
use crate::module::{Module, ModuleOrigin};
use crate::namespace::NamespaceId;
use crate::native::{Capability, CapabilitySet, SpawnFacility, SpawnOptions};
use crate::process::heap::{DEFAULT_HEAP_SIZE, Heap};
use crate::process::registry::ProcessTable;
use crate::process::{CodePosition, ExitReason, Priority};
use crate::replay::{RecordedSchedule, ReplayEvent, ReplayLog};
use crate::scheduler::execution::{
    SliceOutcome, cleanup_if_tombstoned_after_store, execute_slice, store_runnable_process,
    take_runnable_process,
};
use crate::supervision::link::LinkSet;
use crate::supervision::monitor::MonitorSet;
use crate::term::{Term, boxed};
use crate::timer::TimerWheel;

fn ets_metadata(name: Option<Atom>, owner: u64) -> EtsTableMetadata {
    EtsTableMetadata::new(name, 0, EtsTableType::Set, Protection::Protected, owner)
}

#[test]
fn replay_scheduler_forces_single_worker_even_with_thread_override() {
    let scheduler = Scheduler::new_replay(
        SchedulerConfig {
            thread_count: Some(3),
            ..SchedulerConfig::default()
        },
        ReplayLog::default(),
    )
    .unwrap_or_else(|error| panic!("replay scheduler starts: {error}"));

    assert_eq!(scheduler.thread_count(), 1);
    scheduler.shutdown();
}

#[test]
fn replay_driver_exposes_recorded_schedule_order_without_run_queue_pop() {
    let scheduler = Scheduler::new_replay(
        SchedulerConfig::default(),
        ReplayLog::new(vec![
            ReplayEvent::Schedule(RecordedSchedule {
                pid: 3,
                scheduler_index: 0,
                reduction_budget: 11,
                reductions_consumed: 5,
            }),
            ReplayEvent::Schedule(RecordedSchedule {
                pid: 1,
                scheduler_index: 0,
                reduction_budget: 7,
                reductions_consumed: 2,
            }),
        ]),
    )
    .unwrap_or_else(|error| panic!("replay scheduler starts: {error}"));

    let driver = scheduler
        .shared
        .replay_driver
        .as_ref()
        .expect("replay driver installed");
    let mut guard = driver.lock().expect("replay driver lock");
    assert_eq!(guard.peek_schedule().map(|event| event.pid), Some(3));
    assert_eq!(
        guard
            .next_schedule(0)
            .unwrap_or_else(|error| panic!("first schedule: {error}"))
            .pid,
        3
    );
    assert_eq!(guard.peek_schedule().map(|event| event.pid), Some(1));
    assert_eq!(
        guard
            .next_schedule(0)
            .unwrap_or_else(|error| panic!("second schedule: {error}"))
            .pid,
        1
    );
    assert!(guard.is_complete());
    drop(guard);
    scheduler.shutdown();
}

struct NoopWake;

impl Wake for NoopWake {
    fn wake(self: Arc<Self>) {}
}

fn block_on_ready(future: ResolveFuture<'_>) -> Result<std::net::SocketAddr, ResolveError> {
    let waker = Waker::from(Arc::new(NoopWake));
    let mut context = Context::from_waker(&waker);
    let mut future = future;
    match future.as_mut().poll(&mut context) {
        Poll::Ready(result) => result,
        Poll::Pending => panic!("resolver test future should be ready immediately"),
    }
}

#[test]
fn default_distribution_config_resolves_nothing() {
    assert!(SchedulerConfig::default().distribution.is_none());
    assert_eq!(SchedulerConfig::default().jit_threshold, None);

    let scheduler = Scheduler::new(SchedulerConfig::default(), Arc::new(ModuleRegistry::new()))
        .expect("scheduler should start");
    assert_eq!(
        scheduler.jit_profiler().current_threshold(),
        crate::jit::DEFAULT_JIT_THRESHOLD
    );

    assert_eq!(
        block_on_ready(
            scheduler
                .distribution_config()
                .resolver
                .resolve("missing@localhost")
        ),
        Err(ResolveError::NotFound)
    );

    scheduler.shutdown();
}

#[test]
fn scheduler_uses_explicit_jit_threshold() {
    let scheduler = Scheduler::new(
        SchedulerConfig {
            jit_threshold: Some(500),
            ..SchedulerConfig::default()
        },
        Arc::new(ModuleRegistry::new()),
    )
    .expect("scheduler should start");

    assert_eq!(scheduler.jit_profiler().current_threshold(), 500);
    scheduler.shutdown();
}

#[test]
fn ets_registry_create_lookup_name_and_delete() {
    let scheduler = Scheduler::new(SchedulerConfig::default(), Arc::new(ModuleRegistry::new()))
        .expect("scheduler should start");
    let name = scheduler.shared.atom_table.intern("named_ets_table");

    let first_id = scheduler.shared.create_table(ets_metadata(Some(name), 99));
    let second_id = scheduler.shared.create_table(ets_metadata(None, 99));

    assert_ne!(first_id, second_id);
    assert!(second_id > first_id);
    assert_eq!(scheduler.shared.lookup_table_by_name(name), Some(first_id));

    let table = scheduler
        .shared
        .lookup_table(first_id)
        .expect("table should be present by id");
    assert_eq!(table.metadata().id, first_id);
    assert_eq!(table.metadata().name, Some(name));

    assert!(scheduler.shared.delete_table(first_id));
    assert!(scheduler.shared.lookup_table(first_id).is_none());
    assert_eq!(scheduler.shared.lookup_table_by_name(name), None);
    assert!(scheduler.shared.lookup_table(second_id).is_some());
    assert!(!scheduler.shared.delete_table(first_id));

    scheduler.shutdown();
}

fn test_module(name: Atom, code: Vec<Instruction>) -> Module {
    let label_index = code
        .iter()
        .enumerate()
        .filter_map(|(ip, instruction)| match instruction {
            Instruction::Label { label } => Some((*label, ip)),
            _ => None,
        })
        .collect();
    Module {
        name,
        generation: 0,
        origin: ModuleOrigin::Preloaded,
        exports: StdHashMap::new(),
        label_index,
        code,
        literals: Vec::new(),
        constant_pool: Default::default(),
        resolved_imports: Vec::new(),
        lambdas: Vec::new(),
        string_table: Vec::new(),
        function_table: Vec::new(),
        line_table: Vec::new(),
        line_info: Vec::new(),
    }
}

fn wait_until(deadline_ms: u64, mut predicate: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(deadline_ms);
    while !predicate() {
        assert!(std::time::Instant::now() <= deadline, "condition timed out");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[cfg(feature = "telemetry")]
fn install_telemetry_test_provider() -> (InMemorySpanExporter, SdkTracerProvider) {
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    opentelemetry::global::set_tracer_provider(provider.clone());
    (exporter, provider)
}

#[cfg(feature = "telemetry")]
fn span_attr_i64(span: &opentelemetry_sdk::trace::SpanData, key: &'static str) -> Option<i64> {
    span.attributes.iter().find_map(|attribute| {
        (attribute.key == Key::from_static_str(key)).then(|| match &attribute.value {
            opentelemetry::Value::I64(value) => Some(*value),
            _ => None,
        })?
    })
}

#[cfg(feature = "telemetry")]
fn span_attr_str(span: &opentelemetry_sdk::trace::SpanData, key: &'static str) -> Option<String> {
    span.attributes.iter().find_map(|attribute| {
        (attribute.key == Key::from_static_str(key)).then(|| match &attribute.value {
            opentelemetry::Value::String(value) => Some(value.to_string()),
            _ => None,
        })?
    })
}

#[cfg(feature = "telemetry")]
fn install_metric_test_provider() -> (InMemoryMetricExporter, SdkMeterProvider) {
    let exporter = InMemoryMetricExporter::default();
    let reader = PeriodicReader::builder(exporter.clone()).build();
    let provider = SdkMeterProvider::builder().with_reader(reader).build();
    opentelemetry::global::set_meter_provider(provider.clone());
    (exporter, provider)
}

#[cfg(feature = "telemetry")]
fn find_metric<'a>(
    metrics: &'a [ResourceMetrics],
    name: &str,
) -> Option<&'a opentelemetry_sdk::metrics::data::Metric> {
    metrics
        .iter()
        .flat_map(|resource| resource.scope_metrics())
        .flat_map(|scope| scope.metrics())
        .find(|metric| metric.name() == name)
}

#[cfg(feature = "telemetry")]
fn metric_has_u64_sum_at_least(metrics: &[ResourceMetrics], name: &str, minimum: u64) -> bool {
    let Some(metric) = find_metric(metrics, name) else {
        return false;
    };
    match metric.data() {
        AggregatedMetrics::U64(MetricData::Sum(sum)) => {
            sum.data_points().any(|point| point.value() >= minimum)
        }
        _ => false,
    }
}

#[cfg(feature = "telemetry")]
fn metric_has_u64_gauge_at_least(metrics: &[ResourceMetrics], name: &str, minimum: u64) -> bool {
    let Some(metric) = find_metric(metrics, name) else {
        return false;
    };
    match metric.data() {
        AggregatedMetrics::U64(MetricData::Gauge(gauge)) => {
            gauge.data_points().any(|point| point.value() >= minimum)
        }
        _ => false,
    }
}

#[cfg(feature = "telemetry")]
fn metric_has_f64_gauge_between(
    metrics: &[ResourceMetrics],
    name: &str,
    minimum: f64,
    maximum: f64,
) -> bool {
    let Some(metric) = find_metric(metrics, name) else {
        return false;
    };
    match metric.data() {
        AggregatedMetrics::F64(MetricData::Gauge(gauge)) => gauge
            .data_points()
            .any(|point| (minimum..=maximum).contains(&point.value())),
        _ => false,
    }
}

#[cfg(feature = "telemetry")]
fn metric_has_histogram_count_at_least(
    metrics: &[ResourceMetrics],
    name: &str,
    minimum: u64,
) -> bool {
    let Some(metric) = find_metric(metrics, name) else {
        return false;
    };
    match metric.data() {
        AggregatedMetrics::F64(MetricData::Histogram(histogram)) => histogram
            .data_points()
            .any(|point| point.count() >= minimum),
        _ => false,
    }
}

#[cfg(feature = "telemetry")]
fn metric_has_string_attribute(
    metrics: &[ResourceMetrics],
    name: &str,
    key: &str,
    value: &str,
) -> bool {
    fn has_attribute<'a>(
        mut attributes: impl Iterator<Item = &'a opentelemetry::KeyValue>,
        key: &str,
        value: &str,
    ) -> bool {
        attributes.any(|attribute| {
            attribute.key.as_str() == key
                && matches!(&attribute.value, opentelemetry::Value::String(actual) if actual.to_string() == value)
        })
    }

    let Some(metric) = find_metric(metrics, name) else {
        return false;
    };
    match metric.data() {
        AggregatedMetrics::U64(MetricData::Sum(sum)) => sum
            .data_points()
            .any(|point| has_attribute(point.attributes(), key, value)),
        AggregatedMetrics::U64(MetricData::Gauge(gauge)) => gauge
            .data_points()
            .any(|point| has_attribute(point.attributes(), key, value)),
        AggregatedMetrics::F64(MetricData::Histogram(histogram)) => histogram
            .data_points()
            .any(|point| has_attribute(point.attributes(), key, value)),
        _ => false,
    }
}

#[cfg(feature = "telemetry")]
fn metric_has_pid_gauge_at_least(
    metrics: &[ResourceMetrics],
    name: &str,
    pid: u64,
    minimum: u64,
) -> bool {
    let Some(metric) = find_metric(metrics, name) else {
        return false;
    };
    let pid_i64 = match i64::try_from(pid) {
        Ok(value) => value,
        Err(_) => i64::MAX,
    };
    match metric.data() {
        AggregatedMetrics::U64(MetricData::Gauge(gauge)) => gauge.data_points().any(|point| {
            point.value() >= minimum
                && point.attributes().any(|attribute| {
                    attribute.key == Key::from_static_str("pid")
                        && matches!(&attribute.value, opentelemetry::Value::I64(value) if *value == pid_i64)
                })
        }),
        _ => false,
    }
}

#[test]
fn scheduler_creates_requested_thread_count_and_names() {
    let registry = Arc::new(ModuleRegistry::new());
    let scheduler = Scheduler::new(
        SchedulerConfig {
            thread_count: Some(4),
            ..SchedulerConfig::default()
        },
        registry,
    )
    .unwrap_or_else(|error| panic!("scheduler starts: {error}"));

    assert_eq!(scheduler.thread_count(), 4);
    assert_eq!(scheduler.dirty_cpu_pool().thread_count(), num_cpus::get());
    assert_eq!(
        scheduler.dirty_io_pool().thread_count(),
        dirty::DEFAULT_DIRTY_IO_THREADS
    );
    assert_eq!(
        scheduler.worker_names(),
        &[
            "beamr-sched-0",
            "beamr-sched-1",
            "beamr-sched-2",
            "beamr-sched-3"
        ]
    );

    scheduler.shutdown();
}

#[test]
fn scheduler_defaults_to_nonode_nohost_local_node() {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let scheduler = Scheduler::with_code_server(
        SchedulerConfig {
            thread_count: Some(1),
            ..SchedulerConfig::default()
        },
        Arc::new(ModuleRegistry::new()),
        Arc::clone(&atom_table),
        Arc::new(BifRegistryImpl::new()),
    )
    .unwrap_or_else(|error| panic!("scheduler starts: {error}"));

    let local_node = scheduler.local_node();
    assert_eq!(atom_table.resolve(local_node.name), Some("nonode@nohost"));
    assert_eq!(local_node.creation, 0);
    assert!(local_node.is_local(&scheduler.shared.local_node));

    scheduler.shutdown();
}

#[test]
fn scheduler_uses_configured_local_node_identity() {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let scheduler = Scheduler::with_code_server(
        SchedulerConfig {
            thread_count: Some(1),
            node_name: Some("worker@example.test".to_string()),
            creation: Some(7),
            ..SchedulerConfig::default()
        },
        Arc::new(ModuleRegistry::new()),
        Arc::clone(&atom_table),
        Arc::new(BifRegistryImpl::new()),
    )
    .unwrap_or_else(|error| panic!("scheduler starts: {error}"));

    let local_node = scheduler.local_node();
    assert_eq!(
        atom_table.resolve(local_node.name),
        Some("worker@example.test")
    );
    assert_eq!(local_node.creation, 7);

    scheduler.shutdown();
}

#[test]
fn shared_state_metric_accessors_report_scheduler_process_and_atom_counts() {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let extra_atom = atom_table.intern("scheduler_metrics_extra");
    assert!(atom_table.resolve(extra_atom).is_some());
    let registry = Arc::new(ModuleRegistry::new());
    let scheduler = Scheduler::with_code_server(
        SchedulerConfig {
            thread_count: Some(2),
            ..SchedulerConfig::default()
        },
        registry,
        Arc::clone(&atom_table),
        Arc::new(BifRegistryImpl::new()),
    )
    .unwrap_or_else(|error| panic!("scheduler starts: {error}"));

    assert_eq!(scheduler.scheduler_count(), 2);
    assert_eq!(scheduler.thread_count(), scheduler.scheduler_count());
    // Standard IO server is pre-registered as process 0.
    assert_eq!(scheduler.process_count(), 1);
    assert_eq!(scheduler.atom_count(), atom_table.len());
    assert_eq!(scheduler.atom_limit(), atom_table.limit());

    let pid = scheduler.shared.next_pid.fetch_add(1, Ordering::Relaxed);
    scheduler.process_table().spawn_with_pid(pid);
    assert_eq!(scheduler.process_count(), 2);
    let removed = scheduler.process_table().remove(pid);
    assert!(removed.is_some());
    assert_eq!(scheduler.process_count(), 1);

    scheduler.shutdown();
}

#[test]
fn hook_records_reduction_yield_metadata_and_can_suspend_then_resume() {
    let atoms = AtomTable::new();
    let module_name = atoms.intern("hook_loop");
    let function = atoms.intern("main");
    let registry = Arc::new(ModuleRegistry::new());
    let module = test_module(
        module_name,
        vec![
            Instruction::FuncInfo {
                module: Operand::Atom(Some(module_name)),
                function: Operand::Atom(Some(function)),
                arity: Operand::Unsigned(0),
            },
            Instruction::Label { label: 1 },
            Instruction::CallOnly {
                arity: Operand::Unsigned(0),
                label: Operand::Label(1),
            },
        ],
    );
    let module = registry.insert(module);
    let scheduler = Scheduler::new(
        SchedulerConfig {
            thread_count: Some(1),
            ..SchedulerConfig::default()
        },
        Arc::clone(&registry),
    )
    .unwrap_or_else(|error| panic!("scheduler starts: {error}"));
    let events = Arc::new(Mutex::new(Vec::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let events_by_hook = Arc::clone(&events);
    let calls_by_hook = Arc::clone(&calls);
    scheduler.hook().register(move |event| {
        events_by_hook
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(event);
        if calls_by_hook.fetch_add(1, Ordering::AcqRel) == 0 {
            HookDecision::Suspend
        } else {
            HookDecision::Continue
        }
    });

    let pid = scheduler.spawn_process(&module);
    wait_until(2_000, || calls.load(Ordering::Acquire) == 1);
    std::thread::sleep(std::time::Duration::from_millis(25));
    assert_eq!(
        calls.load(Ordering::Acquire),
        1,
        "suspended process is held"
    );
    assert!(scheduler.resume_process(pid));
    wait_until(2_000, || calls.load(Ordering::Acquire) > 1);

    let events = events.lock().unwrap_or_else(|error| error.into_inner());
    let first = events.first().copied().expect("hook event recorded");
    assert_eq!(first.pid, pid);
    assert_eq!(first.module, module_name);
    assert_eq!(first.function, function);
    assert_eq!(first.arity, 0);
    assert_eq!(first.reductions_consumed, DEFAULT_REDUCTION_BUDGET);
    drop(events);
    scheduler.shutdown();
}

#[cfg(feature = "telemetry")]
#[test]
fn execute_slice_emits_telemetry_span_with_mfa_reductions_and_outcome() {
    let (exporter, provider) = install_telemetry_test_provider();
    let atoms = Arc::new(AtomTable::new());
    let module_name = atoms.intern("telemetry_slice");
    let function = atoms.intern("main");
    let registry = Arc::new(ModuleRegistry::new());
    let module = registry.insert(test_module(
        module_name,
        vec![
            Instruction::FuncInfo {
                module: Operand::Atom(Some(module_name)),
                function: Operand::Atom(Some(function)),
                arity: Operand::Unsigned(0),
            },
            Instruction::Label { label: 1 },
            Instruction::CallOnly {
                arity: Operand::Unsigned(0),
                label: Operand::Label(1),
            },
        ],
    ));
    let scheduler = Scheduler::with_code_server(
        SchedulerConfig {
            thread_count: Some(1),
            ..SchedulerConfig::default()
        },
        Arc::clone(&registry),
        Arc::clone(&atoms),
        Arc::new(BifRegistryImpl::new()),
    )
    .unwrap_or_else(|error| panic!("scheduler starts: {error}"));
    scheduler.shutdown();

    let mut process = Process::new(44, DEFAULT_HEAP_SIZE);
    process.set_code_position(Some(CodePosition {
        module: module_name,
        instruction_pointer: 0,
    }));
    process.set_current_module(Arc::clone(&module));

    let SliceOutcome::Requeue(_) = execute_slice(&scheduler.shared, &mut process) else {
        panic!("looping process should yield after consuming its slice");
    };
    provider.force_flush().expect("spans flush");
    let spans = exporter.get_finished_spans().expect("finished spans");
    let span = spans
        .iter()
        .find(|span| span.name.as_ref() == "beamr.scheduler.execute_slice")
        .expect("execution-slice span emitted");

    assert_eq!(span_attr_i64(span, "process.pid"), Some(44));
    assert_eq!(
        span_attr_str(span, "code.module").as_deref(),
        Some("telemetry_slice")
    );
    assert_eq!(
        span_attr_str(span, "code.function").as_deref(),
        Some("main")
    );
    assert_eq!(span_attr_i64(span, "code.arity"), Some(0));
    assert_eq!(
        span_attr_i64(span, "reductions.consumed"),
        Some(i64::from(DEFAULT_REDUCTION_BUDGET))
    );
    assert_eq!(span_attr_str(span, "outcome").as_deref(), Some("yielded"));

    provider.shutdown().expect("provider shutdown");
}

#[cfg(feature = "telemetry")]
#[test]
fn spawned_process_trace_context_nests_process_and_slice_under_workflow_span() {
    let (exporter, provider) = install_telemetry_test_provider();
    let atoms = Arc::new(AtomTable::new());
    let module_name = atoms.intern("telemetry_workflow_slice");
    let function = atoms.intern("main");
    let registry = Arc::new(ModuleRegistry::new());
    let module = registry.insert(test_module(
        module_name,
        vec![
            Instruction::FuncInfo {
                module: Operand::Atom(Some(module_name)),
                function: Operand::Atom(Some(function)),
                arity: Operand::Unsigned(0),
            },
            Instruction::Label { label: 1 },
            Instruction::Return,
        ],
    ));
    let scheduler = Scheduler::with_code_server(
        SchedulerConfig {
            thread_count: Some(1),
            ..SchedulerConfig::default()
        },
        Arc::clone(&registry),
        Arc::clone(&atoms),
        Arc::new(BifRegistryImpl::new()),
    )
    .unwrap_or_else(|error| panic!("scheduler starts: {error}"));
    scheduler.shutdown();

    let tracer = opentelemetry::global::tracer("beamr-test");
    let workflow_span = tracer.start("meridian.workflow");
    let workflow_span_id = workflow_span.span_context().span_id();
    let workflow_context = opentelemetry::Context::current_with_span(workflow_span);
    let pid = 66;
    let mut process = super::spawning::build_process(super::spawning::SpawnRequest {
        pid,
        module: module_name,
        module_version: module,
        instruction_pointer: 0,
        args: Vec::new(),
        parent_pid: 0,
        function,
        arity: 0,
        capabilities: CapabilitySet::all(),
        namespace_id: NamespaceId::DEFAULT,
        group_leader: Term::pid(pid),
        priority: Priority::Normal,
        heap_size: DEFAULT_HEAP_SIZE,
        trace_context: Some(crate::telemetry::spans::inject_context(&workflow_context)),
    });

    let SliceOutcome::Exited(ExitReason::Normal, _) =
        execute_slice(&scheduler.shared, &mut process)
    else {
        panic!("workflow step process should exit normally");
    };
    workflow_context.span().end();
    provider.force_flush().expect("spans flush");
    let spans = exporter.get_finished_spans().expect("finished spans");
    let workflow = spans
        .iter()
        .find(|span| span.name.as_ref() == "meridian.workflow")
        .expect("workflow span emitted");
    let process = spans
        .iter()
        .find(|span| span.name.as_ref() == "beamr.process")
        .expect("process span emitted");
    let slice = spans
        .iter()
        .find(|span| span.name.as_ref() == "beamr.scheduler.execute_slice")
        .expect("execution-slice span emitted");

    assert_eq!(workflow.span_context.span_id(), workflow_span_id);
    assert_eq!(process.parent_span_id, workflow.span_context.span_id());
    assert_eq!(slice.parent_span_id, process.span_context.span_id());
    assert_eq!(
        span_attr_i64(process, "process.pid"),
        Some(i64::try_from(pid).unwrap_or(i64::MAX))
    );
    assert_eq!(
        span_attr_i64(slice, "process.pid"),
        Some(i64::try_from(pid).unwrap_or(i64::MAX))
    );
    assert_eq!(
        span_attr_str(process, "process.exit_reason").as_deref(),
        Some("normal")
    );

    provider.shutdown().expect("provider shutdown");
}

#[cfg(feature = "telemetry")]
#[test]
fn execute_slice_emits_vm_health_and_process_metrics() {
    let (exporter, provider) = install_metric_test_provider();
    let atoms = Arc::new(AtomTable::new());
    let module_name = atoms.intern("telemetry_metrics_slice");
    let function = atoms.intern("main");
    let registry = Arc::new(ModuleRegistry::new());
    let module = registry.insert(test_module(
        module_name,
        vec![
            Instruction::FuncInfo {
                module: Operand::Atom(Some(module_name)),
                function: Operand::Atom(Some(function)),
                arity: Operand::Unsigned(0),
            },
            Instruction::Label { label: 1 },
            Instruction::CallOnly {
                arity: Operand::Unsigned(0),
                label: Operand::Label(1),
            },
        ],
    ));
    let scheduler = Scheduler::with_code_server(
        SchedulerConfig {
            thread_count: Some(1),
            telemetry_sample_interval: Some(std::time::Duration::ZERO),
            ..SchedulerConfig::default()
        },
        Arc::clone(&registry),
        Arc::clone(&atoms),
        Arc::new(BifRegistryImpl::new()),
    )
    .unwrap_or_else(|error| panic!("scheduler starts: {error}"));
    scheduler.shutdown();

    let mut process = Process::new(55, DEFAULT_HEAP_SIZE);
    process.set_code_position(Some(CodePosition {
        module: module_name,
        instruction_pointer: 0,
    }));
    process.set_current_module(Arc::clone(&module));
    for index in 0..5 {
        process
            .mailbox_mut()
            .push_owned_for_test(Term::small_int(index));
    }

    let SliceOutcome::Requeue(_) = execute_slice(&scheduler.shared, &mut process) else {
        panic!("looping process should yield after consuming its slice");
    };

    crate::telemetry::metrics::record_gc_collection("minor", std::time::Duration::from_micros(50));
    crate::telemetry::metrics::record_message_sent();
    crate::telemetry::metrics::record_workflow_started("workflow-123");
    crate::telemetry::metrics::record_workflow_step_completed(
        "workflow-123",
        "function",
        std::time::Duration::from_millis(25),
    );
    provider.force_flush().expect("metrics flush");
    let metrics = exporter.get_finished_metrics().expect("finished metrics");

    assert!(metric_has_u64_gauge_at_least(
        &metrics,
        "beamr.processes.alive",
        1
    ));
    assert!(metric_has_f64_gauge_between(
        &metrics,
        "beamr.scheduler.utilization",
        0.0,
        1.0
    ));
    assert!(metric_has_u64_sum_at_least(
        &metrics,
        "beamr.gc.collections",
        1
    ));
    assert!(metric_has_histogram_count_at_least(
        &metrics,
        "beamr.gc.duration",
        1
    ));
    assert!(metric_has_u64_sum_at_least(
        &metrics,
        "beamr.messages.sent",
        1
    ));
    assert!(find_metric(&metrics, "beamr.memory.heap_words").is_some());
    assert!(metric_has_u64_sum_at_least(
        &metrics,
        "beamr.process.reductions",
        u64::from(DEFAULT_REDUCTION_BUDGET)
    ));
    assert!(metric_has_pid_gauge_at_least(
        &metrics,
        "beamr.process.message_queue_len",
        55,
        5
    ));

    assert!(metric_has_u64_sum_at_least(
        &metrics,
        "beamr.workflow.steps_completed",
        1
    ));
    assert!(metric_has_histogram_count_at_least(
        &metrics,
        "beamr.workflow.step_duration",
        1
    ));
    assert!(find_metric(&metrics, "beamr.workflow.active").is_some());
    assert!(metric_has_string_attribute(
        &metrics,
        "beamr.workflow.steps_completed",
        "workflow_id",
        "workflow-123"
    ));
    assert!(metric_has_string_attribute(
        &metrics,
        "beamr.workflow.step_duration",
        "step_type",
        "function"
    ));
    assert!(metric_has_string_attribute(
        &metrics,
        "beamr.workflow.active",
        "workflow_id",
        "workflow-123"
    ));

    crate::telemetry::metrics::record_workflow_finished("workflow-123");
    provider
        .force_flush()
        .expect("metrics flush after workflow finish");

    provider.shutdown().expect("provider shutdown");
}

#[test]
fn hook_fires_when_process_blocks_on_receive() {
    let atoms = AtomTable::new();
    let module_name = atoms.intern("hook_wait");
    let function = atoms.intern("main");
    let registry = Arc::new(ModuleRegistry::new());
    let module = test_module(
        module_name,
        vec![
            Instruction::FuncInfo {
                module: Operand::Atom(Some(module_name)),
                function: Operand::Atom(Some(function)),
                arity: Operand::Unsigned(0),
            },
            Instruction::Label { label: 10 },
            Instruction::Wait {
                fail: Operand::Label(10),
            },
        ],
    );
    let module = registry.insert(module);
    let scheduler = Scheduler::new(
        SchedulerConfig {
            thread_count: Some(1),
            ..SchedulerConfig::default()
        },
        Arc::clone(&registry),
    )
    .unwrap_or_else(|error| panic!("scheduler starts: {error}"));
    let events = Arc::new(Mutex::new(Vec::new()));
    let events_by_hook = Arc::clone(&events);
    scheduler.hook().register(move |event| {
        events_by_hook
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(event);
        HookDecision::Continue
    });

    let pid = scheduler.spawn_process(&module);
    wait_until(2_000, || {
        !events
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_empty()
    });
    let events = events.lock().unwrap_or_else(|error| error.into_inner());
    assert_eq!(events[0].pid, pid);
    assert_eq!(events[0].module, module_name);
    assert_eq!(events[0].function, function);
    assert_eq!(events[0].arity, 0);
    drop(events);
    scheduler.shutdown();
}

#[test]
fn scheduler_default_thread_count_matches_available_parallelism() {
    let registry = Arc::new(ModuleRegistry::new());
    let scheduler = Scheduler::new(SchedulerConfig::default(), registry)
        .unwrap_or_else(|error| panic!("scheduler starts: {error}"));

    let expected = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
    assert_eq!(scheduler.thread_count(), expected);

    scheduler.shutdown();
}

#[test]
fn shutdown_is_idempotent() {
    let registry = Arc::new(ModuleRegistry::new());
    let scheduler = Scheduler::new(
        SchedulerConfig {
            thread_count: Some(2),
            ..SchedulerConfig::default()
        },
        registry,
    )
    .unwrap_or_else(|error| panic!("scheduler starts: {error}"));

    scheduler.shutdown();
    scheduler.shutdown();
}

#[test]
fn single_process_runs_to_completion_and_is_removed() {
    let atoms = AtomTable::new();
    let module_name = atoms.intern("simple");
    let registry = Arc::new(ModuleRegistry::new());
    let module = test_module(module_name, vec![Instruction::Return]);
    let module = registry.insert(module);
    let scheduler = Scheduler::new(
        SchedulerConfig {
            thread_count: Some(1),
            ..SchedulerConfig::default()
        },
        Arc::clone(&registry),
    )
    .unwrap_or_else(|error| panic!("scheduler starts: {error}"));

    let pid = scheduler.spawn_process(&module);

    wait_until(2_000, || scheduler.process_table().get(pid).is_none());
    scheduler.shutdown();
}

#[test]
fn exported_spawn_starts_at_entry_function_with_args() {
    let atoms = AtomTable::new();
    let module_name = atoms.intern("entry_mod");
    let function = atoms.intern("main");
    let mut module = test_module(
        module_name,
        vec![
            Instruction::Label { label: 7 },
            Instruction::Move {
                source: Operand::X(0),
                destination: Operand::X(1),
            },
            Instruction::Return,
        ],
    );
    module.exports.insert((function, 1), 7);
    let registry = Arc::new(ModuleRegistry::new());
    registry.insert(module);
    let scheduler = Scheduler::new(
        SchedulerConfig {
            thread_count: Some(1),
            ..SchedulerConfig::default()
        },
        Arc::clone(&registry),
    )
    .unwrap_or_else(|error| panic!("scheduler starts: {error}"));

    let pid = scheduler
        .spawn(
            module_name,
            function,
            vec![Term::try_small_int(42).unwrap_or(Term::NIL)],
        )
        .unwrap_or_else(|error| panic!("spawn succeeds: {error}"));

    wait_until(2_000, || scheduler.process_table().get(pid).is_none());
    scheduler.shutdown();
}

#[test]
fn execute_slice_resumes_yielded_process_with_pinned_module_version() {
    let atoms = AtomTable::new();
    let module_name = atoms.intern("slice_pin");
    let registry = Arc::new(ModuleRegistry::new());
    let atom_table = Arc::new(crate::atom::AtomTable::new());
    let distribution = DistributionConfig::default();
    let distribution_connections = crate::distribution::connection::ConnectionManager::new(
        Arc::clone(&atom_table),
        Arc::clone(&distribution.resolver),
    );
    let net_kernel = Arc::new(crate::distribution::NetKernel::new(
        crate::distribution::connection::ConnectionManager::new(
            Arc::clone(&atom_table),
            distribution.resolver.clone(),
        ),
    ));
    let module_v1 = registry.insert(test_module(
        module_name,
        vec![
            Instruction::Label { label: 1 },
            Instruction::CallOnly {
                arity: Operand::Unsigned(0),
                label: Operand::Label(1),
            },
        ],
    ));
    let shared = Arc::new(SharedState {
        shutdown: AtomicBool::new(false),
        process_table: ProcessTable::new(),
        module_registry: Arc::clone(&registry),
        namespace_store: {
            let store = DashMap::new();
            store.insert(NamespaceId::DEFAULT, Arc::clone(&registry));
            store
        },
        next_namespace_id: AtomicU64::new(1),
        spawn_counter: AtomicUsize::new(0),
        thread_count: 1,
        dirty_cpu: dirty::DirtyPool::with_queue_depth("dirty-test-cpu", 1, 1),
        dirty_io: dirty::DirtyPool::with_queue_depth("dirty-test-io", 1, 1),
        next_pid: AtomicU64::new(0),
        wait_set: Mutex::new(WaitSet::default()),
        wake_condvar: Condvar::new(),
        process_bodies: DashMap::new(),
        exit_tombstones: DashMap::new(),
        exit_results: DashMap::new(),
        exit_errors: DashMap::new(),
        exit_exceptions: DashMap::new(),
        async_results: DashMap::new(),
        link_set: Mutex::new(LinkSet::new()),
        monitor_set: Mutex::new(MonitorSet::new()),
        hook: Hook::new(),
        distribution,
        distribution_connections,
        control_router: crate::distribution::remote_link::ControlRouter::new(),
        process_registry: DashMap::new(),
        timers: Arc::new(Mutex::new(TimerWheel::new())),
        output_sink: Mutex::new(Arc::new(NullSink)),
        io_ring: None,
        io_registry: None,
        io_bridge: Mutex::new(None),
        io_facility: None,
        atom_table,
        ets_registry: Arc::new(crate::ets::EtsRegistry::new()),
        pg_registry: Arc::new(crate::distribution::pg::PgRegistry::new(
            &crate::atom::AtomTable::with_common_atoms(),
        )),
        bif_registry: Arc::new(crate::native::BifRegistryImpl::new()),
        capability_policy: Arc::new(crate::native::AllCapabilitiesPolicy),
        idle_parks: AtomicUsize::new(0),
        dirty_results: DashMap::new(),
        file_io_ring: Arc::from(crate::io::create_ring(RingConfig::default())),
        file_io_pending: DashMap::new(),
        file_io_orphans: DashMap::new(),
        file_io_results: DashMap::new(),
        file_io_canceled: DashSet::new(),
        standard_io_pid: u64::MAX,
        _standard_io_server: crate::io::StandardIoServer::new(
            u64::MAX,
            Arc::from(crate::io::create_ring(RingConfig::default())),
            &crate::atom::AtomTable::new(),
        ),
        local_node: crate::distribution::Node::new(crate::atom::Atom::new(0), 0),
        net_kernel,
        jit_profiler: Arc::new(crate::jit::JitProfiler::new(1000)),
        jit_cache: Arc::new(crate::jit::JitCache::new()),
        replay_driver: None,
        replay_mode: false,
    });
    let mut process = Process::new(1, DEFAULT_HEAP_SIZE);
    process.set_code_position(Some(CodePosition {
        module: module_name,
        instruction_pointer: 0,
    }));
    process.set_current_module(Arc::clone(&module_v1));

    let _module_v2 = registry.insert(test_module(module_name, vec![Instruction::Return]));

    let SliceOutcome::Requeue(resumed) = execute_slice(&shared, &mut process) else {
        panic!("pinned loop should yield again instead of using reloaded return-only module");
    };
    assert!(
        resumed
            .current_module()
            .is_some_and(|current| Arc::ptr_eq(current, &module_v1))
    );
}

#[test]
fn linked_test_spawn_inherits_parent_group_leader_not_child_pid() {
    let registry = Arc::new(ModuleRegistry::new());
    let scheduler = Scheduler::new(
        SchedulerConfig {
            thread_count: Some(1),
            ..SchedulerConfig::default()
        },
        registry,
    )
    .unwrap_or_else(|error| panic!("scheduler starts: {error}"));
    scheduler.shutdown();
    let parent = scheduler.spawn_test_process(false);
    let parent_group_leader = Term::pid(77);
    assert!(scheduler.set_test_group_leader(parent, parent_group_leader));

    let child = scheduler
        .spawn_linked_test_process(parent)
        .unwrap_or_else(|error| panic!("linked child starts: {error}"));

    assert_eq!(
        scheduler.test_group_leader(child),
        Some(parent_group_leader)
    );
    assert_ne!(scheduler.test_group_leader(child), Some(Term::pid(child)));
}

#[test]
fn spawn_link_uses_executing_parent_namespace_and_merges_parent_link() {
    let atoms = AtomTable::new();
    let module_name = atoms.intern("spawn_link_child");
    let function = atoms.intern("main");
    let registry = Arc::new(ModuleRegistry::new());
    let scheduler = Scheduler::new(
        SchedulerConfig {
            thread_count: Some(1),
            ..SchedulerConfig::default()
        },
        Arc::clone(&registry),
    )
    .unwrap_or_else(|error| panic!("scheduler starts: {error}"));
    let namespace = scheduler.create_namespace();
    let namespace_registry = scheduler
        .shared
        .namespace_store
        .get(&namespace)
        .map(|entry| Arc::clone(&entry))
        .unwrap_or_else(|| panic!("namespace registry exists"));
    let mut module = test_module(module_name, vec![Instruction::Label { label: 7 }]);
    module.exports.insert((function, 0), 7);
    let module = namespace_registry.insert(module);
    scheduler.shutdown();
    let parent = scheduler.spawn_test_process_in(namespace, Arc::clone(&module));

    let process = take_runnable_process(&scheduler.shared, parent)
        .unwrap_or_else(|| panic!("parent body taken"));

    let child = scheduler
        .spawn_link(parent, module_name, function, Vec::new())
        .unwrap_or_else(|error| panic!("spawn_link succeeds with executing parent: {error:?}"));

    assert_eq!(scheduler.process_namespace(parent), Some(namespace));
    assert_eq!(scheduler.process_namespace(child), Some(namespace));
    assert!(process_links_contain(&scheduler.shared, parent, child));
    store_runnable_process(&scheduler.shared, process);
    assert!(scheduler.is_linked(parent, child));
}

#[test]
fn spawn_facility_options_apply_link_monitor_priority_and_heap_before_wake() {
    let atoms = AtomTable::new();
    let module_name = atoms.intern("spawn_opt_scheduler");
    let function = atoms.intern("main");
    let mut module = test_module(module_name, vec![Instruction::Label { label: 7 }]);
    module.exports.insert((function, 0), 7);
    let registry = Arc::new(ModuleRegistry::new());
    let module = registry.insert(module);
    let scheduler = Scheduler::new(
        SchedulerConfig {
            thread_count: Some(1),
            ..SchedulerConfig::default()
        },
        Arc::clone(&registry),
    )
    .unwrap_or_else(|error| panic!("scheduler starts: {error}"));
    scheduler.shutdown();
    let parent = scheduler.spawn_test_process_in(NamespaceId::DEFAULT, Arc::clone(&module));
    let facility = supervision_integration::SchedulerSpawnFacility {
        shared: Arc::clone(&scheduler.shared),
        namespace_id: NamespaceId::DEFAULT,
    };

    let result = facility
        .spawn_with_options(
            parent,
            module_name,
            function,
            Vec::new(),
            SpawnOptions {
                link: true,
                monitor: true,
                priority: Some(Priority::High),
                min_heap_size: Some(512),
                capabilities: None,
            },
        )
        .unwrap_or_else(|error| panic!("spawn_with_options succeeds: {error:?}"));

    assert!(scheduler.is_linked(parent, result.pid));
    let parent_entry = scheduler
        .shared
        .process_bodies
        .get(&parent)
        .unwrap_or_else(|| panic!("parent body exists"));
    let parent_slot = lock_or_recover(&parent_entry);
    let ProcessSlot::Present(ScheduledProcess(parent_process)) = &*parent_slot else {
        panic!("parent process should be present");
    };
    let reference = result.reference.expect("monitor reference");
    assert!(parent_process.links().contains(&result.pid));
    assert!(
        parent_process
            .monitors()
            .iter()
            .any(|monitor| monitor.reference() == reference
                && monitor.watcher() == parent
                && monitor.target() == result.pid)
    );
    drop(parent_slot);
    drop(parent_entry);

    let child_entry = scheduler
        .shared
        .process_bodies
        .get(&result.pid)
        .unwrap_or_else(|| panic!("child body exists"));
    let child_slot = lock_or_recover(&child_entry);
    let ProcessSlot::Present(ScheduledProcess(child_process)) = &*child_slot else {
        panic!("child process should be present");
    };
    assert_eq!(child_process.priority(), Priority::High);
    assert_eq!(child_process.heap().capacity(), 512);
    assert!(child_process.links().contains(&parent));
    assert!(
        child_process
            .monitors()
            .iter()
            .any(|monitor| monitor.reference() == reference
                && monitor.watcher() == parent
                && monitor.target() == result.pid)
    );
}

#[test]
fn spawn_facility_restricts_child_to_explicit_capabilities() {
    let atoms = AtomTable::new();
    let module_name = atoms.intern("spawn_opt_capability_scheduler");
    let function = atoms.intern("main");
    let mut module = test_module(module_name, vec![Instruction::Label { label: 7 }]);
    module.exports.insert((function, 0), 7);
    let registry = Arc::new(ModuleRegistry::new());
    let module = registry.insert(module);
    let scheduler = Scheduler::new(
        SchedulerConfig {
            thread_count: Some(1),
            ..SchedulerConfig::default()
        },
        Arc::clone(&registry),
    )
    .unwrap_or_else(|error| panic!("scheduler starts: {error}"));
    scheduler.shutdown();
    let parent = scheduler.spawn_test_process_in(NamespaceId::DEFAULT, Arc::clone(&module));
    let facility = supervision_integration::SchedulerSpawnFacility {
        shared: Arc::clone(&scheduler.shared),
        namespace_id: NamespaceId::DEFAULT,
    };
    let restricted = CapabilitySet::from_slice(&[Capability::Pure, Capability::ProcessLocal]);

    let result = facility
        .spawn_with_options(
            parent,
            module_name,
            function,
            Vec::new(),
            SpawnOptions {
                capabilities: Some(restricted.clone()),
                ..SpawnOptions::default()
            },
        )
        .unwrap_or_else(|error| panic!("spawn_with_options succeeds: {error:?}"));

    let child_entry = scheduler
        .shared
        .process_bodies
        .get(&result.pid)
        .unwrap_or_else(|| panic!("child body exists"));
    let child_slot = lock_or_recover(&child_entry);
    let ProcessSlot::Present(ScheduledProcess(child_process)) = &*child_slot else {
        panic!("child process should be present");
    };
    assert_eq!(child_process.capabilities(), &restricted);
    assert!(
        !child_process
            .capabilities()
            .contains(Capability::ExternalIo)
    );
}

#[test]
fn process_info_reads_executing_process_metadata() {
    let atoms = AtomTable::new();
    let module_name = atoms.intern("executing_info");
    let function = atoms.intern("main");
    let module = test_module(module_name, vec![Instruction::Label { label: 1 }]);
    let registry = Arc::new(ModuleRegistry::new());
    let module = registry.insert(module);
    let scheduler = Scheduler::new(
        SchedulerConfig {
            thread_count: Some(1),
            ..SchedulerConfig::default()
        },
        Arc::clone(&registry),
    )
    .unwrap_or_else(|error| panic!("scheduler starts: {error}"));
    scheduler.shutdown();
    let pid = scheduler.spawn_test_process_in(NamespaceId::DEFAULT, Arc::clone(&module));
    {
        let entry = scheduler
            .shared
            .process_bodies
            .get(&pid)
            .unwrap_or_else(|| panic!("process body exists"));
        let mut slot = lock_or_recover(&entry);
        let ProcessSlot::Present(ScheduledProcess(process)) = &mut *slot else {
            panic!("test process should be present");
        };
        process.set_current_mfa(Some((module_name, function, 0)));
    }

    let process = take_runnable_process(&scheduler.shared, pid)
        .unwrap_or_else(|| panic!("process should transition to executing"));

    assert_eq!(
        scheduler
            .shared
            .process_info(pid, ProcessInfoItem::CurrentFunction),
        Some(ProcessInfoValue::CurrentFunction(Some((
            module_name,
            function,
            0
        ))))
    );
    assert_eq!(
        scheduler.shared.process_info(pid, ProcessInfoItem::Status),
        Some(ProcessInfoValue::Status(ProcessInfoStatus::Running))
    );

    store_runnable_process(&scheduler.shared, process);
}

#[test]
fn tombstone_after_wait_store_prevents_wait_parking() {
    let atom_table = Arc::new(crate::atom::AtomTable::new());
    let distribution = DistributionConfig::default();
    let distribution_connections = crate::distribution::connection::ConnectionManager::new(
        Arc::clone(&atom_table),
        Arc::clone(&distribution.resolver),
    );
    let shared = Arc::new(SharedState {
        shutdown: AtomicBool::new(false),
        process_table: ProcessTable::new(),
        module_registry: Arc::new(ModuleRegistry::new()),
        namespace_store: {
            let registry = Arc::new(ModuleRegistry::new());
            let store = DashMap::new();
            store.insert(NamespaceId::DEFAULT, registry);
            store
        },
        next_namespace_id: AtomicU64::new(1),
        spawn_counter: AtomicUsize::new(0),
        thread_count: 1,
        next_pid: AtomicU64::new(0),
        wait_set: Mutex::new(WaitSet::default()),
        wake_condvar: Condvar::new(),
        process_bodies: DashMap::new(),
        exit_tombstones: DashMap::new(),
        exit_results: DashMap::new(),
        exit_errors: DashMap::new(),
        exit_exceptions: DashMap::new(),
        async_results: DashMap::new(),
        link_set: Mutex::new(LinkSet::new()),
        monitor_set: Mutex::new(MonitorSet::new()),
        hook: Hook::new(),
        distribution,
        distribution_connections,
        control_router: crate::distribution::remote_link::ControlRouter::new(),
        process_registry: DashMap::new(),
        timers: Arc::new(Mutex::new(TimerWheel::new())),
        output_sink: Mutex::new(Arc::new(NullSink)),
        io_ring: None,
        io_registry: None,
        io_bridge: Mutex::new(None),
        io_facility: None,
        atom_table,
        ets_registry: Arc::new(crate::ets::EtsRegistry::new()),
        pg_registry: Arc::new(crate::distribution::pg::PgRegistry::new(
            &crate::atom::AtomTable::with_common_atoms(),
        )),
        bif_registry: Arc::new(crate::native::BifRegistryImpl::new()),
        capability_policy: Arc::new(crate::native::AllCapabilitiesPolicy),
        idle_parks: AtomicUsize::new(0),
        dirty_cpu: crate::scheduler::dirty::DirtyPool::new("test-cpu", 1),
        dirty_io: crate::scheduler::dirty::DirtyPool::new("test-io", 1),
        dirty_results: DashMap::new(),
        file_io_ring: Arc::from(crate::io::create_ring(RingConfig::default())),
        file_io_pending: DashMap::new(),
        file_io_orphans: DashMap::new(),
        file_io_results: DashMap::new(),
        file_io_canceled: DashSet::new(),
        standard_io_pid: u64::MAX,
        _standard_io_server: crate::io::StandardIoServer::new(
            u64::MAX,
            Arc::from(crate::io::create_ring(RingConfig::default())),
            &crate::atom::AtomTable::new(),
        ),
        local_node: crate::distribution::Node::new(crate::atom::Atom::new(0), 0),
        net_kernel: {
            let dist = DistributionConfig::default();
            let at = Arc::new(crate::atom::AtomTable::new());
            let cm =
                crate::distribution::connection::ConnectionManager::new(at, dist.resolver.clone());
            Arc::new(crate::distribution::NetKernel::new(cm))
        },
        jit_profiler: Arc::new(crate::jit::JitProfiler::new(1000)),
        jit_cache: Arc::new(crate::jit::JitCache::new()),
        replay_driver: None,
        replay_mode: false,
    });
    let pid = 1;
    shared.process_table.spawn_with_pid(pid);
    let process = Process::new(pid, DEFAULT_HEAP_SIZE);
    shared.process_bodies.insert(
        pid,
        Mutex::new(ProcessSlot::Executing(ProcessMetadata {
            namespace_id: NamespaceId::DEFAULT,
            capabilities: process.capabilities().clone(),
            links: Vec::new(),
            remote_links: Vec::new(),
            monitors: Vec::new(),
            trap_exit: false,
            priority: process.priority(),
            current_mfa: None,
            heap_size: 0,
            binary_heap_size: 0,
            message_queue_len: 0,
            group_leader: process.group_leader(),
            logical_clock: process.logical_clock(),
            pending_exit_messages: Vec::new(),
            pending_down_messages: Vec::new(),
            pending_io_messages: Vec::new(),
            pending_distribution_payloads: Vec::new(),
            pending_ets_transfer_messages: Vec::new(),
            pending_udp_messages: Vec::new(),
            pending_tcp_messages: Vec::new(),
        })),
    );
    shared.exit_tombstones.insert(pid, ExitReason::Error);

    store_runnable_process(&shared, process);
    assert!(cleanup_if_tombstoned_after_store(&shared, pid));

    let ws = lock_or_recover(&shared.wait_set);
    assert!(
        !ws.waiting.contains_key(&pid),
        "tombstoned process must not be parked after store-back"
    );
}

#[test]
fn yielded_process_is_rescheduled() {
    let atoms = AtomTable::new();
    let module_name = atoms.intern("loopy");
    let registry = Arc::new(ModuleRegistry::new());
    let module = test_module(
        module_name,
        vec![
            Instruction::Label { label: 1 },
            Instruction::CallOnly {
                arity: Operand::Unsigned(0),
                label: Operand::Label(1),
            },
        ],
    );
    let module = registry.insert(module);
    let scheduler = Scheduler::new(
        SchedulerConfig {
            thread_count: Some(1),
            ..SchedulerConfig::default()
        },
        Arc::clone(&registry),
    )
    .unwrap_or_else(|error| panic!("scheduler starts: {error}"));

    let pid = scheduler.spawn_process(&module);
    std::thread::sleep(std::time::Duration::from_millis(75));

    assert!(scheduler.process_table().get(pid).is_some());
    scheduler.shutdown();
}

#[test]
fn multiple_processes_fairly_complete() {
    let atoms = AtomTable::new();
    let module_name = atoms.intern("multi");
    let registry = Arc::new(ModuleRegistry::new());
    let module = registry.insert(test_module(module_name, vec![Instruction::Return]));
    let scheduler = Scheduler::new(
        SchedulerConfig {
            thread_count: Some(2),
            ..SchedulerConfig::default()
        },
        Arc::clone(&registry),
    )
    .unwrap_or_else(|error| panic!("scheduler starts: {error}"));

    let pids: Vec<_> = (0..20).map(|_| scheduler.spawn_process(&module)).collect();

    wait_until(3_000, || {
        pids.iter()
            .all(|pid| scheduler.process_table().get(*pid).is_none())
    });
    scheduler.shutdown();
}

#[test]
fn mailbox_send_wakes_waiting_process_event_driven() {
    let registry = Arc::new(ModuleRegistry::new());
    let scheduler = Scheduler::new(
        SchedulerConfig {
            thread_count: Some(1),
            ..SchedulerConfig::default()
        },
        registry,
    )
    .unwrap_or_else(|error| panic!("scheduler starts: {error}"));
    let pid = 42;
    scheduler.shared.process_table.spawn_with_pid(pid);
    {
        let mut wait_set = lock_or_recover(&scheduler.shared.wait_set);
        wait_set.waiting.insert(pid, 0);
    }
    let mailbox = Mailbox::new();
    let sender = mailbox
        .sender()
        .with_wake_notifier(scheduler.wake_notifier(pid));
    let mut receiver_heap = Heap::new(16);

    sender
        .send(Term::small_int(7), &mut receiver_heap)
        .unwrap_or_else(|error| panic!("send succeeds: {error}"));

    let wait_set = lock_or_recover(&scheduler.shared.wait_set);
    assert!(!wait_set.waiting.contains_key(&pid));
    assert!(
        wait_set
            .woken
            .iter()
            .any(|(woken_pid, _)| *woken_pid == pid)
    );
    drop(wait_set);
    scheduler.shutdown();
}

#[test]
fn mailbox_send_does_not_wake_when_copy_fails() {
    let called = Arc::new(AtomicBool::new(false));
    let called_by_wake = Arc::clone(&called);
    let mailbox = Mailbox::new();
    let sender = mailbox.sender().with_wake_notifier(move || {
        called_by_wake.store(true, Ordering::Release);
    });
    let mut receiver_heap = Heap::new(0);
    let mut sender_words = [0_u64; 2];
    let too_large = boxed::write_cons(&mut sender_words, Term::small_int(1), Term::NIL)
        .unwrap_or_else(|| panic!("source cons fits"));

    assert!(sender.send(too_large, &mut receiver_heap).is_err());
    assert!(!called.load(Ordering::Acquire));
}

#[test]
fn idle_threads_park_instead_of_spinning() {
    let registry = Arc::new(ModuleRegistry::new());
    let scheduler = Scheduler::new(
        SchedulerConfig {
            thread_count: Some(2),
            ..SchedulerConfig::default()
        },
        registry,
    )
    .unwrap_or_else(|error| panic!("scheduler starts: {error}"));

    wait_until(500, || scheduler.idle_park_count() > 0);
    scheduler.shutdown();
}
