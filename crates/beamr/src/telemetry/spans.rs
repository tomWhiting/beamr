//! OpenTelemetry span helpers for scheduler and message boundaries.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use opentelemetry::global;
use opentelemetry::trace::{Span, TraceContextExt, Tracer};
use opentelemetry::{Context, KeyValue};

use crate::atom::{Atom, AtomTable};
use crate::process::Process;
use crate::term::{
    Term,
    binary::Binary,
    boxed::{BigInt, Closure, Cons, Float, Map, Reference, Tuple},
};

const TRACER_NAME: &str = "beamr";
const WORD_BYTES: usize = std::mem::size_of::<u64>();
const MAX_TERM_DEPTH: usize = 64;

/// Serializable propagation carrier stored alongside a mailbox message.
pub type TraceCarrier = HashMap<String, String>;

#[derive(Clone, Debug)]
pub(crate) struct MessageTraceContext {
    carrier: TraceCarrier,
    span_context: opentelemetry::trace::SpanContext,
}

#[derive(Debug)]
pub(crate) struct ExecutionSliceSpan {
    span: opentelemetry::global::BoxedSpan,
}

impl ExecutionSliceSpan {
    /// Start a span for one scheduler execution slice.
    pub(crate) fn start(atom_table: &AtomTable, process: &Process) -> Self {
        let tracer = global::tracer(TRACER_NAME);
        let mut span = tracer.start("beamr.scheduler.execute_slice");
        span.set_attribute(KeyValue::new(
            "process.pid",
            i64::try_from(process.pid()).unwrap_or(i64::MAX),
        ));
        Self { span }
    }

    /// Complete the execution-slice span with final code metadata, reductions, and outcome.
    pub(crate) fn finish(
        mut self,
        atom_table: &AtomTable,
        process: &Process,
        reductions_consumed: u32,
        outcome: SliceSpanOutcome,
    ) {
        let (module, function, arity) = process.current_mfa().unwrap_or((Atom::NIL, Atom::NIL, 0));
        self.span.set_attributes([
            KeyValue::new("code.module", atom_name(atom_table, module)),
            KeyValue::new("code.function", atom_name(atom_table, function)),
            KeyValue::new("code.arity", i64::from(arity)),
            KeyValue::new("reductions.consumed", i64::from(reductions_consumed)),
            KeyValue::new("outcome", outcome.as_str()),
        ]);
        self.span.end();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SliceSpanOutcome {
    Yielded,
    Waiting,
    Exited,
}

impl SliceSpanOutcome {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Yielded => "yielded",
            Self::Waiting => "waiting",
            Self::Exited => "exited",
        }
    }
}

pub(crate) fn record_message_send_context(
    sender_pid: u64,
    receiver_pid: u64,
    message: Term,
) -> MessageTraceContext {
    let tracer = global::tracer(TRACER_NAME);
    let mut span = tracer.start("beamr.message.send");
    span.set_attributes(message_send_attributes(sender_pid, receiver_pid, message));

    let span_context = span.span_context().clone();
    let context = Context::current_with_span(span);
    let mut carrier = TraceCarrier::new();
    global::get_text_map_propagator(|propagator| propagator.inject_context(&context, &mut carrier));
    context.span().end();
    MessageTraceContext {
        carrier,
        span_context,
    }
}

/// Record a matched receive span and attach/extract propagated send context when present.
pub(crate) fn record_message_receive(
    receiver_pid: u64,
    wait_duration: Option<Duration>,
    matched: bool,
    trace_context: Option<&MessageTraceContext>,
) {
    let parent = trace_context.map_or_else(Context::current, |trace_context| {
        global::get_text_map_propagator(|propagator| propagator.extract(&trace_context.carrier))
    });
    let tracer = global::tracer(TRACER_NAME);
    let mut span = tracer.start_with_context("beamr.message.receive", &parent);
    if let Some(trace_context) = trace_context {
        span.add_link(trace_context.span_context.clone(), Vec::new());
    }
    span.set_attributes([
        KeyValue::new(
            "message.receiver.pid",
            i64::try_from(receiver_pid).unwrap_or(i64::MAX),
        ),
        KeyValue::new(
            "message.wait_duration_ms",
            i64::try_from(wait_duration.map_or(0_u128, |duration| duration.as_millis()))
                .unwrap_or(i64::MAX),
        ),
        KeyValue::new("message.matched", matched),
    ]);
    span.end();
}

fn message_send_attributes(sender_pid: u64, receiver_pid: u64, message: Term) -> [KeyValue; 3] {
    [
        KeyValue::new(
            "message.sender.pid",
            i64::try_from(sender_pid).unwrap_or(i64::MAX),
        ),
        KeyValue::new(
            "message.receiver.pid",
            i64::try_from(receiver_pid).unwrap_or(i64::MAX),
        ),
        KeyValue::new(
            "message.size",
            i64::try_from(estimate_message_size(message)).unwrap_or(i64::MAX),
        ),
    ]
}

/// Timestamp used to compute receive wait duration.
pub(crate) type ReceiveWaitStarted = Instant;

pub(crate) fn receive_wait_started_now() -> ReceiveWaitStarted {
    Instant::now()
}

fn atom_name(atom_table: &AtomTable, atom: Atom) -> String {
    atom_table
        .resolve(atom)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("atom:{}", atom.index()))
}

fn estimate_message_size(term: Term) -> usize {
    let mut seen = HashSet::new();
    estimate_term_size(term, 0, &mut seen)
}

fn estimate_term_size(term: Term, depth: usize, seen: &mut HashSet<usize>) -> usize {
    if depth >= MAX_TERM_DEPTH {
        return WORD_BYTES;
    }
    if term.is_list() {
        let Some(cons) = Cons::new(term) else {
            return WORD_BYTES;
        };
        return mark_seen(term, seen).map_or(WORD_BYTES, |_| {
            WORD_BYTES * 2
                + estimate_term_size(cons.head(), depth + 1, seen)
                + estimate_term_size(cons.tail(), depth + 1, seen)
        });
    }
    if !term.is_boxed() {
        return WORD_BYTES;
    }
    if let Some(binary) = Binary::new(term) {
        return WORD_BYTES * 2 + binary.len();
    }
    if let Some(tuple) = Tuple::new(term) {
        return mark_seen(term, seen).map_or(WORD_BYTES, |_| {
            WORD_BYTES * (1 + tuple.arity())
                + (0..tuple.arity())
                    .filter_map(|index| tuple.get(index))
                    .map(|element| estimate_term_size(element, depth + 1, seen))
                    .sum::<usize>()
        });
    }
    if let Some(map) = Map::new(term) {
        return mark_seen(term, seen).map_or(WORD_BYTES, |_| {
            WORD_BYTES * (2 + map.len() * 2)
                + (0..map.len())
                    .flat_map(|index| [map.key(index), map.value(index)])
                    .flatten()
                    .map(|element| estimate_term_size(element, depth + 1, seen))
                    .sum::<usize>()
        });
    }
    if let Some(bigint) = BigInt::new(term) {
        return WORD_BYTES * (3 + bigint.limb_count());
    }
    if Float::new(term).is_some() || Reference::new(term).is_some() {
        return WORD_BYTES * 2;
    }
    if let Some(closure) = Closure::new(term) {
        return mark_seen(term, seen).map_or(WORD_BYTES, |_| {
            WORD_BYTES * (7 + closure.num_free())
                + (0..closure.num_free())
                    .filter_map(|index| closure.free_var(index))
                    .map(|free_var| estimate_term_size(free_var, depth + 1, seen))
                    .sum::<usize>()
        });
    }
    WORD_BYTES
}

fn mark_seen(term: Term, seen: &mut HashSet<usize>) -> Option<()> {
    let ptr = term.heap_ptr()? as usize;
    seen.insert(ptr).then_some(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry::Key;
    use opentelemetry::global;
    use opentelemetry::trace::TraceContextExt;
    use opentelemetry_sdk::propagation::TraceContextPropagator;
    use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};

    fn install_test_provider() -> (InMemorySpanExporter, SdkTracerProvider) {
        global::set_text_map_propagator(TraceContextPropagator::new());
        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build();
        global::set_tracer_provider(provider.clone());
        (exporter, provider)
    }

    fn attr_i64(span: &opentelemetry_sdk::trace::SpanData, key: &str) -> Option<i64> {
        span.attributes.iter().find_map(|attribute| {
            (attribute.key == Key::from_static_str(key)).then(|| match &attribute.value {
                opentelemetry::Value::I64(value) => Some(*value),
                _ => None,
            })?
        })
    }

    fn attr_bool(span: &opentelemetry_sdk::trace::SpanData, key: &str) -> Option<bool> {
        span.attributes.iter().find_map(|attribute| {
            (attribute.key == Key::from_static_str(key)).then(|| match &attribute.value {
                opentelemetry::Value::Bool(value) => Some(*value),
                _ => None,
            })?
        })
    }

    #[test]
    fn message_send_and_receive_spans_record_attributes_and_link_context() {
        let (exporter, provider) = install_test_provider();
        let trace_context = record_message_send_context(10, 20, Term::small_int(123));
        record_message_receive(
            20,
            Some(Duration::from_millis(7)),
            true,
            Some(&trace_context),
        );
        provider.force_flush().expect("spans flush");

        let spans = exporter.get_finished_spans().expect("finished spans");
        let send_span = spans
            .iter()
            .find(|span| span.name.as_ref() == "beamr.message.send")
            .expect("send span emitted");
        let receive_span = spans
            .iter()
            .find(|span| span.name.as_ref() == "beamr.message.receive")
            .expect("receive span emitted");

        assert_eq!(attr_i64(send_span, "message.sender.pid"), Some(10));
        assert_eq!(attr_i64(send_span, "message.receiver.pid"), Some(20));
        assert_eq!(attr_i64(send_span, "message.size"), Some(WORD_BYTES as i64));
        assert_eq!(attr_i64(receive_span, "message.receiver.pid"), Some(20));
        assert_eq!(attr_i64(receive_span, "message.wait_duration_ms"), Some(7));
        assert_eq!(attr_bool(receive_span, "message.matched"), Some(true));
        assert_eq!(
            receive_span.parent_span_id,
            send_span.span_context.span_id(),
            "receive span should extract propagated send context as parent"
        );
        assert!(
            receive_span
                .links
                .links
                .iter()
                .any(|link| link.span_context.span_id() == send_span.span_context.span_id()),
            "receive span should also link directly to the send span context"
        );

        provider.shutdown().expect("provider shutdown");
    }
}
