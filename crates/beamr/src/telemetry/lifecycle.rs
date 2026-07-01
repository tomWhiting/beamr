//! OpenTelemetry log helpers for process lifecycle events.

use std::sync::{Arc, RwLock};
use std::time::SystemTime;

use opentelemetry::logs::{AnyValue, LogRecord, Logger, LoggerProvider, Severity};
use opentelemetry::trace::TraceContextExt;
use opentelemetry::{Context, InstrumentationScope, Key};

use crate::atom::{Atom, AtomTable};
use crate::process::{Exception, ExitReason};
use crate::term::format::format_term;

const LOGGER_NAME: &str = "beamr";
const EVENT_PROCESS_SPAWNED: &str = "process.spawned";
const EVENT_PROCESS_EXITED: &str = "process.exited";
const EVENT_PROCESS_LINKED: &str = "process.linked";
const EVENT_PROCESS_MONITORED: &str = "process.monitored";
const EVENT_PROCESS_CRASHED: &str = "process.crashed";

static LIFECYCLE_EMITTER: RwLock<Option<Arc<dyn LifecycleLogEmitter>>> = RwLock::new(None);

trait LifecycleLogEmitter: Send + Sync {
    fn emit(&self, event_name: &'static str, severity: Severity, attributes: Vec<(Key, AnyValue)>);
}

struct ProviderLifecycleLogEmitter<P> {
    provider: P,
}

impl<P> LifecycleLogEmitter for ProviderLifecycleLogEmitter<P>
where
    P: LoggerProvider + Send + Sync,
    P::Logger: Send + Sync,
{
    fn emit(&self, event_name: &'static str, severity: Severity, attributes: Vec<(Key, AnyValue)>) {
        let logger = self.provider.logger_with_scope(lifecycle_scope());
        if !logger.event_enabled(severity, LOGGER_NAME, Some(event_name)) {
            return;
        }
        let mut record = logger.create_log_record();
        let now = SystemTime::now();
        record.set_event_name(event_name);
        record.set_target(LOGGER_NAME);
        record.set_timestamp(now);
        record.set_observed_timestamp(now);
        record.set_severity_number(severity);
        record.set_severity_text(severity.name());
        attach_current_trace_context(&mut record);
        record.add_attributes(attributes);
        logger.emit(record);
    }
}

/// Install the OpenTelemetry logger provider used for Beamr lifecycle events.
///
/// Without an installed provider lifecycle helpers are no-ops, preserving the
/// optional telemetry feature's zero-configuration behavior.
pub fn set_lifecycle_logger_provider<P>(provider: P)
where
    P: LoggerProvider + Send + Sync + 'static,
    P::Logger: Send + Sync,
{
    let mut guard = write_emitter_slot();
    *guard = Some(Arc::new(ProviderLifecycleLogEmitter { provider }));
}

pub(crate) fn record_process_spawned(
    atom_table: &AtomTable,
    pid: u64,
    parent_pid: u64,
    module: Atom,
    function: Atom,
    arity: u8,
) {
    emit_lifecycle_event(
        EVENT_PROCESS_SPAWNED,
        Severity::Info,
        vec![
            int_attr("process.pid", pid_to_i64(pid)),
            int_attr("pid", pid_to_i64(pid)),
            int_attr("process.parent_pid", pid_to_i64(parent_pid)),
            int_attr("parent_pid", pid_to_i64(parent_pid)),
            string_attr("code.module", atom_name(atom_table, module)),
            string_attr("module", atom_name(atom_table, module)),
            string_attr("code.function", atom_name(atom_table, function)),
            string_attr("function", atom_name(atom_table, function)),
            int_attr("code.arity", i64::from(arity)),
            int_attr("arity", i64::from(arity)),
        ],
    );
}

pub(crate) fn record_process_exited(atom_table: &AtomTable, pid: u64, reason: ExitReason) {
    emit_lifecycle_event(
        EVENT_PROCESS_EXITED,
        Severity::Info,
        vec![
            int_attr("process.pid", pid_to_i64(pid)),
            int_attr("pid", pid_to_i64(pid)),
            string_attr(
                "process.exit.reason",
                atom_name(atom_table, reason.as_atom()),
            ),
            string_attr("reason", atom_name(atom_table, reason.as_atom())),
            string_attr("process.exit_class", exit_class(reason)),
            string_attr("exit_class", exit_class(reason)),
        ],
    );
}

pub(crate) fn record_process_linked(pid_a: u64, pid_b: u64) {
    emit_lifecycle_event(
        EVENT_PROCESS_LINKED,
        Severity::Info,
        vec![
            int_attr("process.pid_a", pid_to_i64(pid_a)),
            int_attr("pid_a", pid_to_i64(pid_a)),
            int_attr("process.pid_b", pid_to_i64(pid_b)),
            int_attr("pid_b", pid_to_i64(pid_b)),
        ],
    );
}

pub(crate) fn record_process_monitored(watcher_pid: u64, target_pid: u64, reference: u64) {
    emit_lifecycle_event(
        EVENT_PROCESS_MONITORED,
        Severity::Info,
        vec![
            int_attr("process.watcher_pid", pid_to_i64(watcher_pid)),
            int_attr("watcher_pid", pid_to_i64(watcher_pid)),
            int_attr("process.target_pid", pid_to_i64(target_pid)),
            int_attr("target_pid", pid_to_i64(target_pid)),
            int_attr("process.monitor.ref", pid_to_i64(reference)),
            int_attr("ref", pid_to_i64(reference)),
        ],
    );
}

pub(crate) fn record_process_crashed(atom_table: &AtomTable, pid: u64, exception: Exception) {
    emit_lifecycle_event(
        EVENT_PROCESS_CRASHED,
        Severity::Error,
        vec![
            int_attr("process.pid", pid_to_i64(pid)),
            int_attr("pid", pid_to_i64(pid)),
            string_attr("exception.class", format_term(exception.class, atom_table)),
            string_attr("exception_class", format_term(exception.class, atom_table)),
            string_attr(
                "exception.reason",
                format_term(exception.reason, atom_table),
            ),
            string_attr(
                "exception.stacktrace",
                format_term(exception.stacktrace, atom_table),
            ),
            string_attr("reason", exception.format_with_atoms(atom_table)),
            string_attr("stacktrace", format_term(exception.stacktrace, atom_table)),
        ],
    );
}

pub(crate) fn record_process_crashed_reason(atom_table: &AtomTable, pid: u64, reason: ExitReason) {
    let reason_name = atom_name(atom_table, reason.as_atom());
    emit_lifecycle_event(
        EVENT_PROCESS_CRASHED,
        Severity::Error,
        vec![
            int_attr("process.pid", pid_to_i64(pid)),
            int_attr("pid", pid_to_i64(pid)),
            string_attr("exception.class", "error"),
            string_attr("exception_class", "error"),
            string_attr("exception.reason", reason_name.clone()),
            string_attr("exception.stacktrace", "[]"),
            string_attr("reason", reason_name),
            string_attr("stacktrace", "[]"),
        ],
    );
}

fn emit_lifecycle_event(
    event_name: &'static str,
    severity: Severity,
    attributes: Vec<(Key, AnyValue)>,
) {
    let emitter = {
        let guard = read_emitter_slot();
        guard.as_ref().map(Arc::clone)
    };
    if let Some(emitter) = emitter {
        emitter.emit(event_name, severity, attributes);
    }
}

fn lifecycle_scope() -> InstrumentationScope {
    InstrumentationScope::builder(LOGGER_NAME)
        .with_version(env!("CARGO_PKG_VERSION"))
        .build()
}

fn attach_current_trace_context<R>(record: &mut R)
where
    R: LogRecord,
{
    let context = Context::current();
    let span_context = context.span().span_context().clone();
    if span_context.is_valid() {
        record.set_trace_context(
            span_context.trace_id(),
            span_context.span_id(),
            Some(span_context.trace_flags()),
        );
    }
}

fn read_emitter_slot() -> std::sync::RwLockReadGuard<'static, Option<Arc<dyn LifecycleLogEmitter>>>
{
    match LIFECYCLE_EMITTER.read() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn write_emitter_slot() -> std::sync::RwLockWriteGuard<'static, Option<Arc<dyn LifecycleLogEmitter>>>
{
    match LIFECYCLE_EMITTER.write() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn int_attr(key: &'static str, value: i64) -> (Key, AnyValue) {
    (Key::from_static_str(key), AnyValue::Int(value))
}

fn string_attr(key: &'static str, value: impl Into<opentelemetry::StringValue>) -> (Key, AnyValue) {
    (Key::from_static_str(key), AnyValue::String(value.into()))
}

fn pid_to_i64(pid: u64) -> i64 {
    i64::try_from(pid).unwrap_or(i64::MAX)
}

fn atom_name(atom_table: &AtomTable, atom: Atom) -> String {
    atom_table
        .resolve(atom)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("atom:{}", atom.index()))
}

const fn exit_class(reason: ExitReason) -> &'static str {
    match reason {
        ExitReason::Normal => "normal",
        ExitReason::Kill | ExitReason::Killed | ExitReason::Error | ExitReason::NoConnection => {
            "error"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry::logs::Severity;
    use opentelemetry::{Key, logs::AnyValue};
    use opentelemetry_sdk::logs::{InMemoryLogExporter, SdkLoggerProvider};

    fn install_test_provider() -> (InMemoryLogExporter, SdkLoggerProvider) {
        let exporter = InMemoryLogExporter::default();
        let provider = SdkLoggerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build();
        set_lifecycle_logger_provider(provider.clone());
        (exporter, provider)
    }

    fn attr_i64(log: &opentelemetry_sdk::logs::SdkLogRecord, key: &str) -> Option<i64> {
        log.attributes_iter().find_map(|(attribute_key, value)| {
            (attribute_key == &Key::new(key.to_owned())).then_some(match value {
                AnyValue::Int(value) => Some(*value),
                _ => None,
            })?
        })
    }

    fn attr_string(log: &opentelemetry_sdk::logs::SdkLogRecord, key: &str) -> Option<String> {
        log.attributes_iter().find_map(|(attribute_key, value)| {
            (attribute_key == &Key::new(key.to_owned())).then(|| match value {
                AnyValue::String(value) => Some(value.to_string()),
                _ => None,
            })?
        })
    }

    #[test]
    fn lifecycle_helpers_emit_process_events_with_attributes() {
        let _guard = crate::telemetry::test_lock::guard();
        let (exporter, provider) = install_test_provider();
        let atom_table = AtomTable::with_common_atoms();
        let module = atom_table.intern("demo_module");
        let function = atom_table.intern("start");

        record_process_spawned(&atom_table, 2, 1, module, function, 3);
        record_process_linked(1, 2);
        record_process_monitored(1, 2, 99);
        record_process_exited(&atom_table, 2, ExitReason::Normal);
        provider.force_flush().expect("logs flush");

        let logs = exporter.get_emitted_logs().expect("emitted logs");
        let spawned = logs
            .iter()
            .find(|log| log.record.event_name() == Some(EVENT_PROCESS_SPAWNED))
            .expect("spawned event emitted");
        assert_eq!(
            spawned.record.target().map(|target| target.as_ref()),
            Some(LOGGER_NAME)
        );
        assert!(spawned.record.timestamp().is_some());
        assert!(spawned.record.observed_timestamp().is_some());
        assert_eq!(spawned.record.severity_number(), Some(Severity::Info));
        assert_eq!(attr_i64(&spawned.record, "process.pid"), Some(2));
        assert_eq!(attr_i64(&spawned.record, "parent_pid"), Some(1));
        assert_eq!(
            attr_string(&spawned.record, "module"),
            Some("demo_module".to_owned())
        );
        assert_eq!(
            attr_string(&spawned.record, "function"),
            Some("start".to_owned())
        );
        assert_eq!(attr_i64(&spawned.record, "arity"), Some(3));

        let linked = logs
            .iter()
            .find(|log| log.record.event_name() == Some(EVENT_PROCESS_LINKED))
            .expect("linked event emitted");
        assert_eq!(attr_i64(&linked.record, "pid_a"), Some(1));
        assert_eq!(attr_i64(&linked.record, "pid_b"), Some(2));

        let monitored = logs
            .iter()
            .find(|log| log.record.event_name() == Some(EVENT_PROCESS_MONITORED))
            .expect("monitored event emitted");
        assert_eq!(attr_i64(&monitored.record, "watcher_pid"), Some(1));
        assert_eq!(attr_i64(&monitored.record, "target_pid"), Some(2));
        assert_eq!(attr_i64(&monitored.record, "ref"), Some(99));

        let exited = logs
            .iter()
            .find(|log| log.record.event_name() == Some(EVENT_PROCESS_EXITED))
            .expect("exited event emitted");
        assert_eq!(
            attr_string(&exited.record, "reason"),
            Some("normal".to_owned())
        );
        assert_eq!(
            attr_string(&exited.record, "exit_class"),
            Some("normal".to_owned())
        );
        provider.shutdown().expect("provider shutdown");
    }

    #[test]
    fn crash_event_records_error_severity_and_exception_details() {
        let _guard = crate::telemetry::test_lock::guard();
        let (exporter, provider) = install_test_provider();
        let atom_table = AtomTable::with_common_atoms();
        let badarg = atom_table.intern("badarg");
        let exception = Exception {
            class: crate::term::Term::atom(Atom::ERROR),
            reason: crate::term::Term::atom(badarg),
            stacktrace: crate::term::Term::NIL,
        };

        record_process_crashed(&atom_table, 42, exception);
        provider.force_flush().expect("logs flush");
        let logs = exporter.get_emitted_logs().expect("emitted logs");
        let crashed = logs
            .iter()
            .find(|log| log.record.event_name() == Some(EVENT_PROCESS_CRASHED))
            .expect("crashed event emitted");
        assert_eq!(crashed.record.severity_number(), Some(Severity::Error));
        assert_eq!(attr_i64(&crashed.record, "process.pid"), Some(42));
        assert_eq!(
            attr_string(&crashed.record, "exception.class"),
            Some("error".to_owned())
        );
        assert_eq!(
            attr_string(&crashed.record, "exception_class"),
            Some("error".to_owned())
        );
        assert_eq!(
            attr_string(&crashed.record, "exception.reason"),
            Some("badarg".to_owned())
        );
        provider.shutdown().expect("provider shutdown");
    }

    #[test]
    fn supervision_tree_can_be_reconstructed_from_event_stream() {
        let _guard = crate::telemetry::test_lock::guard();
        let (exporter, provider) = install_test_provider();
        let atom_table = AtomTable::new();
        let supervisor = atom_table.intern("supervisor");
        let worker = atom_table.intern("worker");
        let start = atom_table.intern("start_link");

        record_process_spawned(&atom_table, 10, 1, supervisor, start, 0);
        record_process_spawned(&atom_table, 11, 10, worker, start, 0);
        record_process_spawned(&atom_table, 12, 10, worker, start, 0);
        record_process_linked(10, 11);
        record_process_linked(10, 12);
        record_process_monitored(10, 12, 7);
        provider.force_flush().expect("logs flush");

        let logs = exporter.get_emitted_logs().expect("emitted logs");
        let mut children_by_parent = std::collections::BTreeMap::<i64, Vec<i64>>::new();
        let mut links = Vec::new();
        let mut monitors = Vec::new();
        for log in &logs {
            match log.record.event_name() {
                Some(EVENT_PROCESS_SPAWNED) => {
                    if let (Some(parent), Some(child)) = (
                        attr_i64(&log.record, "parent_pid"),
                        attr_i64(&log.record, "process.pid"),
                    ) {
                        children_by_parent.entry(parent).or_default().push(child);
                    }
                }
                Some(EVENT_PROCESS_LINKED) => {
                    if let (Some(pid_a), Some(pid_b)) = (
                        attr_i64(&log.record, "pid_a"),
                        attr_i64(&log.record, "pid_b"),
                    ) {
                        links.push((pid_a, pid_b));
                    }
                }
                Some(EVENT_PROCESS_MONITORED) => {
                    if let (Some(watcher), Some(target), Some(reference)) = (
                        attr_i64(&log.record, "watcher_pid"),
                        attr_i64(&log.record, "target_pid"),
                        attr_i64(&log.record, "ref"),
                    ) {
                        monitors.push((watcher, target, reference));
                    }
                }
                _ => {}
            }
        }

        assert_eq!(children_by_parent.get(&1), Some(&vec![10]));
        assert_eq!(children_by_parent.get(&10), Some(&vec![11, 12]));
        assert_eq!(links, vec![(10, 11), (10, 12)]);
        assert_eq!(monitors, vec![(10, 12, 7)]);
        provider.shutdown().expect("provider shutdown");
    }
}
