use std::io::Write;
use std::time::Instant;

use crate::atom::Atom;
use crate::native::ExceptionClass;
use crate::replay::{
    NativeOutcome, RecordedDeliveryKind, RecordedMessageDelivery, RecordedNativeCall,
    RecordedSchedule, RecordedSelect, RecordedTimerExpiry, ReplayEvent, ReplayLog,
};
use crate::term::Term;
use crate::timer::{ExpiredTimer, TimerRef};

const MAGIC: &[u8; 8] = b"BMRRPLY\0";

#[test]
fn replay_log_save_load_round_trips_all_event_variants() {
    let path = std::env::temp_dir().join(format!(
        "beamr-replay-{}-{}.rlog",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let now = Instant::now();
    let log = ReplayLog::new(vec![
        ReplayEvent::Select(RecordedSelect {
            pid: 1,
            index: 0,
            message: Term::small_int(10),
        }),
        ReplayEvent::MessageDelivery(RecordedMessageDelivery {
            order: 2,
            kind: RecordedDeliveryKind::RuntimeMessage,
            sender_pid: None,
            receiver_pid: 3,
            sender_clock: 0,
            receiver_clock: 4,
            message: Term::atom(Atom::OK),
        }),
        ReplayEvent::Schedule(RecordedSchedule {
            pid: 3,
            scheduler_index: 0,
            reduction_budget: 100,
            reductions_consumed: 7,
        }),
        ReplayEvent::TimerExpiry(RecordedTimerExpiry {
            now,
            expired: vec![ExpiredTimer {
                reference: TimerRef::from_id(9),
                target_pid: 3,
                message: Term::small_int(20),
                expires_at: now,
            }],
        }),
        ReplayEvent::NativeCall(RecordedNativeCall {
            pid: 3,
            module: Atom::MODULE,
            function: Atom::OK,
            arity: 0,
            outcome: NativeOutcome::err(Term::atom(Atom::BADARG), ExceptionClass::Error, Term::NIL),
        }),
    ]);

    log.save(&path).expect("save replay log");
    let loaded = ReplayLog::load(&path).expect("load replay log");
    let _ = std::fs::remove_file(path);

    assert_eq!(loaded.len(), log.len());
    assert_eq!(loaded.events()[0], log.events()[0]);
    assert_eq!(loaded.events()[1], log.events()[1]);
    assert_eq!(loaded.events()[2], log.events()[2]);
    assert_timer_fields_round_trip(&loaded.events()[3], &log.events()[3]);
    assert_eq!(loaded.events()[4], log.events()[4]);
}

#[test]
fn replay_log_save_load_preserves_cli_transcript() {
    let path = std::env::temp_dir().join(format!(
        "beamr-replay-cli-{}-{}.rlog",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let log = ReplayLog::with_cli_result(Vec::new(), "ok\n".to_owned(), 7);

    log.save(&path).expect("save replay log with transcript");
    let loaded = ReplayLog::load(&path).expect("load replay log with transcript");
    let _ = std::fs::remove_file(path);
    let result = loaded.cli_result().expect("transcript is present");

    assert_eq!(result.output(), "ok\n");
    assert_eq!(result.exit_code(), 7);
}

#[test]
fn replay_log_load_rejects_unknown_header_flags() {
    let path = temp_replay_path("unknown-flags");
    let mut bytes = Vec::new();
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&ReplayLog::format_version().to_le_bytes());
    bytes.push(0x80);
    bytes.extend_from_slice(&0_u64.to_le_bytes());
    std::fs::File::create(&path)
        .and_then(|mut file| file.write_all(&bytes))
        .expect("write malformed replay log");

    let error = ReplayLog::load(&path).expect_err("unknown flags should be rejected");
    let _ = std::fs::remove_file(path);

    assert!(error.to_string().contains("unknown replay log flags"));
}

fn assert_timer_fields_round_trip(loaded: &ReplayEvent, original: &ReplayEvent) {
    match (loaded, original) {
        (ReplayEvent::TimerExpiry(loaded), ReplayEvent::TimerExpiry(original)) => {
            assert_eq!(loaded.expired.len(), original.expired.len());
            assert_eq!(loaded.expired[0].reference, original.expired[0].reference);
            assert_eq!(loaded.expired[0].target_pid, original.expired[0].target_pid);
            assert_eq!(loaded.expired[0].message, original.expired[0].message);
        }
        other => panic!("unexpected timer events: {other:?}"),
    }
}

fn temp_replay_path(label: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "beamr-replay-{label}-{}-{}.rlog",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ))
}
