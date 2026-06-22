//! OpenTelemetry metric helpers for VM health and per-process scheduler state.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use opentelemetry::KeyValue;
use opentelemetry::global;
use opentelemetry::metrics::{Counter, Gauge, Histogram};

const METER_NAME: &str = "beamr";

struct Instruments {
    processes_alive: Gauge<u64>,
    scheduler_utilization: Gauge<f64>,
    gc_collections: Counter<u64>,
    gc_duration: Histogram<f64>,
    messages_sent: Counter<u64>,
    messages_dropped: Counter<u64>,
    memory_heap_words: Gauge<u64>,
    process_message_queue_len: Gauge<u64>,
    process_reductions: Counter<u64>,
    workflow_steps_completed: Counter<u64>,
    workflow_step_duration: Histogram<f64>,
    workflow_active: Gauge<u64>,
}

impl Instruments {
    fn new() -> Self {
        let meter = global::meter(METER_NAME);
        Self {
            processes_alive: meter
                .u64_gauge("beamr.processes.alive")
                .with_description("Current number of live Beamr processes")
                .with_unit("{process}")
                .build(),
            scheduler_utilization: meter
                .f64_gauge("beamr.scheduler.utilization")
                .with_description(
                    "Fraction of scheduler time spent executing processes rather than idle",
                )
                .with_unit("1")
                .build(),
            gc_collections: meter
                .u64_counter("beamr.gc.collections")
                .with_description("Total number of Beamr garbage collections")
                .with_unit("{collection}")
                .build(),
            gc_duration: meter
                .f64_histogram("beamr.gc.duration")
                .with_description("Beamr garbage collection duration")
                .with_unit("s")
                .build(),
            messages_sent: meter
                .u64_counter("beamr.messages.sent")
                .with_description("Total number of Beamr messages sent")
                .with_unit("{message}")
                .build(),
            messages_dropped: meter
                .u64_counter("beamr.messages.dropped")
                .with_description(
                    "Total Beamr local messages dropped on delivery (e.g. ETF codec gap or \
                     mailbox failure on the deferred cross-heap path)",
                )
                .with_unit("{message}")
                .build(),
            memory_heap_words: meter
                .u64_gauge("beamr.memory.heap_words")
                .with_description("Total process heap words allocated")
                .with_unit("{word}")
                .build(),
            process_message_queue_len: meter
                .u64_gauge("beamr.process.message_queue_len")
                .with_description(
                    "Current process mailbox depth sampled at scheduler slice boundaries",
                )
                .with_unit("{message}")
                .build(),
            process_reductions: meter
                .u64_counter("beamr.process.reductions")
                .with_description("Total scheduler reductions consumed by process")
                .with_unit("{reduction}")
                .build(),
            workflow_steps_completed: meter
                .u64_counter("beamr.workflow.steps_completed")
                .with_description("Total completed Beamr workflow steps")
                .with_unit("{step}")
                .build(),
            workflow_step_duration: meter
                .f64_histogram("beamr.workflow.step_duration")
                .with_description("Beamr workflow step duration")
                .with_unit("s")
                .build(),
            workflow_active: meter
                .u64_gauge("beamr.workflow.active")
                .with_description("Current number of active Beamr workflows")
                .with_unit("{workflow}")
                .build(),
        }
    }
}

fn instruments() -> &'static Instruments {
    static INSTRUMENTS: OnceLock<Instruments> = OnceLock::new();
    INSTRUMENTS.get_or_init(Instruments::new)
}

/// Record a VM health snapshot.
pub(crate) fn record_vm_health(
    processes_alive: usize,
    heap_words: usize,
    scheduler_utilization: f64,
) {
    let instruments = instruments();
    instruments
        .processes_alive
        .record(usize_to_u64(processes_alive), &[]);
    instruments
        .memory_heap_words
        .record(usize_to_u64(heap_words), &[]);
    instruments
        .scheduler_utilization
        .record(scheduler_utilization.clamp(0.0, 1.0), &[]);
}

/// Record one successfully completed GC collection.
pub(crate) fn record_gc_collection(kind: &'static str, duration: Duration) {
    let attributes = [KeyValue::new("gc.kind", kind)];
    let instruments = instruments();
    instruments.gc_collections.add(1, &attributes);
    instruments
        .gc_duration
        .record(duration.as_secs_f64(), &attributes);
}

/// Record one successfully sent message.
pub(crate) fn record_message_sent() {
    instruments().messages_sent.add(1, &[]);
}

/// Record one local message dropped on delivery. The `reason` attribute names the
/// drop site (e.g. `"etf_encode"`, `"mailbox_present"`) so a regression in the
/// deferred cross-heap delivery path is observable rather than silent.
pub(crate) fn record_message_dropped(reason: &'static str) {
    instruments()
        .messages_dropped
        .add(1, &[KeyValue::new("reason", reason)]);
}

/// Record sampled process state at a scheduler slice boundary.
pub(crate) fn record_process_slice(pid: u64, reductions: u32, message_queue_len: usize) {
    let pid_value = match i64::try_from(pid) {
        Ok(value) => value,
        Err(_) => i64::MAX,
    };
    let attributes = [KeyValue::new("pid", pid_value)];
    let instruments = instruments();
    instruments
        .process_reductions
        .add(u64::from(reductions), &attributes);
    instruments
        .process_message_queue_len
        .record(usize_to_u64(message_queue_len), &attributes);
}

/// Record that a workflow instance has started and update the active workflow gauge.
pub fn record_workflow_started(workflow_id: impl Into<String>) {
    let active = active_workflows()
        .fetch_add(1, Ordering::Relaxed)
        .saturating_add(1);
    let attributes = [KeyValue::new("workflow_id", workflow_id.into())];
    instruments().workflow_active.record(active, &attributes);
}

/// Record that a workflow instance has finished and update the active workflow gauge.
pub fn record_workflow_finished(workflow_id: impl Into<String>) {
    let previous = active_workflows().fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
        Some(value.saturating_sub(1))
    });
    let active = previous.map_or(0, |value| value.saturating_sub(1));
    let attributes = [KeyValue::new("workflow_id", workflow_id.into())];
    instruments().workflow_active.record(active, &attributes);
}

/// Record one completed workflow step for caller-supplied workflow metadata.
pub fn record_workflow_step_completed(
    workflow_id: impl Into<String>,
    step_type: impl Into<String>,
    duration: Duration,
) {
    let attributes = [
        KeyValue::new("workflow_id", workflow_id.into()),
        KeyValue::new("step_type", step_type.into()),
    ];
    let instruments = instruments();
    instruments.workflow_steps_completed.add(1, &attributes);
    instruments
        .workflow_step_duration
        .record(duration.as_secs_f64(), &attributes);
}

fn active_workflows() -> &'static AtomicU64 {
    static ACTIVE_WORKFLOWS: AtomicU64 = AtomicU64::new(0);
    &ACTIVE_WORKFLOWS
}

fn usize_to_u64(value: usize) -> u64 {
    match u64::try_from(value) {
        Ok(value) => value,
        Err(_) => u64::MAX,
    }
}
