//! Compact on-disk replay-log format.
use std::fmt;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::atom::AtomTable;
use crate::native::ExceptionClass;
use crate::process::heap::Heap;
use crate::replay::driver::*;
use crate::term::Term;
use crate::timer::{ExpiredTimer, TimerRef};

const MAGIC: &[u8; 8] = b"BMRRPLY\0";
const FORMAT_VERSION: u16 = 1;
const FLAG_ZSTD: u8 = 0x01;
const FLAG_CLI_RESULT: u8 = 0x02;
const ZSTD_LEVEL: i32 = 3;
const MAX_EVENTS: usize = 1_000_000;
const MAX_PAYLOAD_BYTES: usize = 256 * 1024 * 1024;
const MAX_TERM_BYTES: usize = 16 * 1024 * 1024;

/// Error raised while saving or loading a replay log file.
#[derive(Debug)]
pub enum ReplayLogFileError {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    InvalidFormat(&'static str),
    UnsupportedVersion(u16),
    EncodeTerm(crate::distribution::etf::EncodeError),
    DecodeTerm(crate::distribution::etf::DecodeError),
    Compression(std::io::Error),
}

impl fmt::Display for ReplayLogFileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::InvalidFormat(message) => formatter.write_str(message),
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported replay log format version {version}")
            }
            Self::EncodeTerm(error) => write!(formatter, "encode term: {error:?}"),
            Self::DecodeTerm(error) => write!(formatter, "decode term: {error:?}"),
            Self::Compression(error) => write!(formatter, "compression: {error}"),
        }
    }
}

impl std::error::Error for ReplayLogFileError {}

impl ReplayLog {
    /// Current replay log file format version.
    #[must_use]
    pub const fn format_version() -> u16 {
        FORMAT_VERSION
    }

    /// Save this replay log to `path` using the compact versioned binary format.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), ReplayLogFileError> {
        let path = path.as_ref();
        let mut payload = Vec::new();
        let atoms = AtomTable::with_common_atoms();
        write_u64(&mut payload, usize_to_u64(self.events().len())?);
        for event in self.events() {
            encode_event(event, &atoms, &mut payload)?;
        }
        if let Some(result) = self.cli_result() {
            write_string(&mut payload, result.output())?;
            payload.push(result.exit_code());
        }

        let mut flags = if self.cli_result().is_some() {
            FLAG_CLI_RESULT
        } else {
            0
        };
        let body = encode_body(payload, &mut flags)?;
        let mut file = Vec::with_capacity(MAGIC.len() + 11 + body.len());
        file.extend_from_slice(MAGIC);
        write_u16(&mut file, FORMAT_VERSION);
        file.push(flags);
        write_u64(&mut file, usize_to_u64(body.len())?);
        file.extend_from_slice(&body);
        std::fs::write(path, file).map_err(|source| ReplayLogFileError::Io {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Load a replay log from `path`.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ReplayLogFileError> {
        let path = path.as_ref();
        let bytes = std::fs::read(path).map_err(|source| ReplayLogFileError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let mut cursor = Cursor::new(bytes.as_slice());
        let mut magic = [0_u8; 8];
        cursor
            .read_exact(&mut magic)
            .map_err(|_| ReplayLogFileError::InvalidFormat("missing replay log header"))?;
        if &magic != MAGIC {
            return Err(ReplayLogFileError::InvalidFormat(
                "invalid replay log magic",
            ));
        }
        let version = read_u16(&mut cursor)?;
        if version != FORMAT_VERSION {
            return Err(ReplayLogFileError::UnsupportedVersion(version));
        }
        let flags = read_u8(&mut cursor)?;
        let body_len = u64_to_usize(read_u64(&mut cursor)?)?;
        if body_len > MAX_PAYLOAD_BYTES || body_len != remaining(&cursor) {
            return Err(ReplayLogFileError::InvalidFormat(
                "invalid replay payload length",
            ));
        }
        let mut body = vec![0_u8; body_len];
        cursor
            .read_exact(&mut body)
            .map_err(|_| ReplayLogFileError::InvalidFormat("truncated replay payload"))?;
        let payload = decode_body(body, flags)?;
        decode_payload(&payload, flags)
    }
}

fn encode_body(payload: Vec<u8>, flags: &mut u8) -> Result<Vec<u8>, ReplayLogFileError> {
    #[cfg(feature = "embedded")]
    {
        *flags |= FLAG_ZSTD;
        zstd::stream::encode_all(Cursor::new(payload), ZSTD_LEVEL)
            .map_err(ReplayLogFileError::Compression)
    }
    #[cfg(not(feature = "embedded"))]
    {
        let _ = flags;
        Ok(payload)
    }
}

fn decode_body(body: Vec<u8>, flags: u8) -> Result<Vec<u8>, ReplayLogFileError> {
    if flags & FLAG_ZSTD == 0 {
        return Ok(body);
    }
    #[cfg(feature = "embedded")]
    {
        zstd::stream::decode_all(Cursor::new(body)).map_err(ReplayLogFileError::Compression)
    }
    #[cfg(not(feature = "embedded"))]
    {
        let _ = body;
        Err(ReplayLogFileError::InvalidFormat(
            "compressed replay logs require the embedded feature",
        ))
    }
}

fn decode_payload(payload: &[u8], flags: u8) -> Result<ReplayLog, ReplayLogFileError> {
    let mut cursor = Cursor::new(payload);
    let count = u64_to_usize(read_u64(&mut cursor)?)?;
    if count > MAX_EVENTS {
        return Err(ReplayLogFileError::InvalidFormat("too many replay events"));
    }
    let atoms = AtomTable::with_common_atoms();
    let mut heaps = Vec::new();
    let mut events = Vec::with_capacity(count);
    for _ in 0..count {
        events.push(decode_event(&mut cursor, &atoms, &mut heaps)?);
    }
    let cli_result = if flags & FLAG_CLI_RESULT == 0 {
        None
    } else {
        let output = read_string(&mut cursor)?;
        let exit_code = read_u8(&mut cursor)?;
        Some(crate::replay::driver::CliReplayResult::new(
            output, exit_code,
        ))
    };
    if remaining(&cursor) != 0 {
        return Err(ReplayLogFileError::InvalidFormat(
            "trailing replay payload bytes",
        ));
    }
    Ok(ReplayLog::from_parts(events, Arc::from(heaps), cli_result))
}

fn encode_event(
    event: &ReplayEvent,
    atoms: &AtomTable,
    out: &mut Vec<u8>,
) -> Result<(), ReplayLogFileError> {
    match event {
        ReplayEvent::Select(recorded) => {
            out.push(0);
            write_u64(out, recorded.pid);
            write_u64(out, usize_to_u64(recorded.index)?);
            write_term(out, atoms, recorded.message)?;
        }
        ReplayEvent::MessageDelivery(recorded) => encode_delivery(recorded, atoms, out)?,
        ReplayEvent::Schedule(recorded) => {
            out.push(2);
            write_u64(out, recorded.pid);
            write_u64(out, usize_to_u64(recorded.scheduler_index)?);
            out.extend_from_slice(&recorded.reduction_budget.to_le_bytes());
            out.extend_from_slice(&recorded.reductions_consumed.to_le_bytes());
        }
        ReplayEvent::TimerExpiry(recorded) => encode_timer(recorded, atoms, out)?,
        ReplayEvent::NativeCall(recorded) => encode_native(recorded, atoms, out)?,
    }
    Ok(())
}

fn encode_delivery(
    recorded: &RecordedMessageDelivery,
    atoms: &AtomTable,
    out: &mut Vec<u8>,
) -> Result<(), ReplayLogFileError> {
    out.push(1);
    write_u64(out, recorded.order);
    out.push(delivery_kind_to_u8(recorded.kind));
    write_option_u64(out, recorded.sender_pid);
    write_u64(out, recorded.receiver_pid);
    write_u64(out, recorded.sender_clock);
    write_u64(out, recorded.receiver_clock);
    write_term(out, atoms, recorded.message)
}

fn encode_timer(
    recorded: &RecordedTimerExpiry,
    atoms: &AtomTable,
    out: &mut Vec<u8>,
) -> Result<(), ReplayLogFileError> {
    out.push(3);
    write_u64(out, 0);
    write_u64(out, usize_to_u64(recorded.expired.len())?);
    for expired in &recorded.expired {
        write_u64(out, expired.reference.id());
        write_u64(out, expired.target_pid);
        write_term(out, atoms, expired.message)?;
        write_u64(out, 0);
    }
    Ok(())
}

fn encode_native(
    recorded: &RecordedNativeCall,
    atoms: &AtomTable,
    out: &mut Vec<u8>,
) -> Result<(), ReplayLogFileError> {
    out.push(4);
    write_u64(out, recorded.pid);
    write_term(out, atoms, Term::atom(recorded.module))?;
    write_term(out, atoms, Term::atom(recorded.function))?;
    out.push(recorded.arity);
    match recorded.outcome.result {
        Ok(term) => {
            out.push(0);
            write_term(out, atoms, term)?;
        }
        Err(term) => {
            out.push(1);
            write_term(out, atoms, term)?;
        }
    }
    out.push(exception_class_to_u8(recorded.outcome.exception_class));
    write_term(out, atoms, recorded.outcome.exception_stacktrace)
}

fn decode_event(
    cursor: &mut Cursor<&[u8]>,
    atoms: &AtomTable,
    heaps: &mut Vec<Heap>,
) -> Result<ReplayEvent, ReplayLogFileError> {
    match read_u8(cursor)? {
        0 => Ok(ReplayEvent::Select(RecordedSelect {
            pid: read_u64(cursor)?,
            index: u64_to_usize(read_u64(cursor)?)?,
            message: read_term(cursor, atoms, heaps)?,
        })),
        1 => decode_delivery(cursor, atoms, heaps),
        2 => Ok(ReplayEvent::Schedule(RecordedSchedule {
            pid: read_u64(cursor)?,
            scheduler_index: u64_to_usize(read_u64(cursor)?)?,
            reduction_budget: read_fixed_u32(cursor)?,
            reductions_consumed: read_fixed_u32(cursor)?,
        })),
        3 => decode_timer(cursor, atoms, heaps),
        4 => decode_native(cursor, atoms, heaps),
        _ => Err(ReplayLogFileError::InvalidFormat(
            "unknown replay event tag",
        )),
    }
}

fn decode_delivery(
    cursor: &mut Cursor<&[u8]>,
    atoms: &AtomTable,
    heaps: &mut Vec<Heap>,
) -> Result<ReplayEvent, ReplayLogFileError> {
    Ok(ReplayEvent::MessageDelivery(RecordedMessageDelivery {
        order: read_u64(cursor)?,
        kind: delivery_kind_from_u8(read_u8(cursor)?)?,
        sender_pid: read_option_u64(cursor)?,
        receiver_pid: read_u64(cursor)?,
        sender_clock: read_u64(cursor)?,
        receiver_clock: read_u64(cursor)?,
        message: read_term(cursor, atoms, heaps)?,
    }))
}

fn decode_timer(
    cursor: &mut Cursor<&[u8]>,
    atoms: &AtomTable,
    heaps: &mut Vec<Heap>,
) -> Result<ReplayEvent, ReplayLogFileError> {
    let _now_offset = read_u64(cursor)?;
    let count = u64_to_usize(read_u64(cursor)?)?;
    let now = std::time::Instant::now();
    let mut expired = Vec::with_capacity(count);
    for _ in 0..count {
        expired.push(ExpiredTimer {
            reference: TimerRef::from_id(read_u64(cursor)?),
            target_pid: read_u64(cursor)?,
            message: read_term(cursor, atoms, heaps)?,
            expires_at: now,
        });
        let _expires_offset = read_u64(cursor)?;
    }
    Ok(ReplayEvent::TimerExpiry(RecordedTimerExpiry {
        now,
        expired,
    }))
}

fn decode_native(
    cursor: &mut Cursor<&[u8]>,
    atoms: &AtomTable,
    heaps: &mut Vec<Heap>,
) -> Result<ReplayEvent, ReplayLogFileError> {
    let pid = read_u64(cursor)?;
    let module =
        read_term(cursor, atoms, heaps)?
            .as_atom()
            .ok_or(ReplayLogFileError::InvalidFormat(
                "native module is not an atom",
            ))?;
    let function =
        read_term(cursor, atoms, heaps)?
            .as_atom()
            .ok_or(ReplayLogFileError::InvalidFormat(
                "native function is not an atom",
            ))?;
    let arity = read_u8(cursor)?;
    let result_term = match read_u8(cursor)? {
        0 => Ok(read_term(cursor, atoms, heaps)?),
        1 => Err(read_term(cursor, atoms, heaps)?),
        _ => {
            return Err(ReplayLogFileError::InvalidFormat(
                "invalid native result tag",
            ));
        }
    };
    Ok(ReplayEvent::NativeCall(RecordedNativeCall {
        pid,
        module,
        function,
        arity,
        outcome: NativeOutcome {
            result: result_term,
            exception_class: exception_class_from_u8(read_u8(cursor)?)?,
            exception_stacktrace: read_term(cursor, atoms, heaps)?,
        },
    }))
}

fn write_term(out: &mut Vec<u8>, atoms: &AtomTable, term: Term) -> Result<(), ReplayLogFileError> {
    let encoded = crate::distribution::etf::encode_term_result(term, atoms)
        .map_err(ReplayLogFileError::EncodeTerm)?;
    write_bytes(out, &encoded)
}

fn read_term(
    cursor: &mut Cursor<&[u8]>,
    atoms: &AtomTable,
    heaps: &mut Vec<Heap>,
) -> Result<Term, ReplayLogFileError> {
    let bytes = read_bytes(cursor)?;
    let mut heap = Heap::new(bytes.len().max(16));
    let term = crate::distribution::etf::decode_term(bytes, &mut heap, atoms)
        .map_err(ReplayLogFileError::DecodeTerm)?;
    heaps.push(heap);
    Ok(term)
}

fn delivery_kind_to_u8(kind: RecordedDeliveryKind) -> u8 {
    match kind {
        RecordedDeliveryKind::Message => 0,
        RecordedDeliveryKind::ExitSignal => 1,
        RecordedDeliveryKind::DownMessage => 2,
        RecordedDeliveryKind::RuntimeMessage => 3,
    }
}

fn delivery_kind_from_u8(value: u8) -> Result<RecordedDeliveryKind, ReplayLogFileError> {
    match value {
        0 => Ok(RecordedDeliveryKind::Message),
        1 => Ok(RecordedDeliveryKind::ExitSignal),
        2 => Ok(RecordedDeliveryKind::DownMessage),
        3 => Ok(RecordedDeliveryKind::RuntimeMessage),
        _ => Err(ReplayLogFileError::InvalidFormat("invalid delivery kind")),
    }
}

fn exception_class_to_u8(class: ExceptionClass) -> u8 {
    match class {
        ExceptionClass::Error => 0,
        ExceptionClass::Throw => 1,
        ExceptionClass::Exit => 2,
    }
}

fn exception_class_from_u8(value: u8) -> Result<ExceptionClass, ReplayLogFileError> {
    match value {
        0 => Ok(ExceptionClass::Error),
        1 => Ok(ExceptionClass::Throw),
        2 => Ok(ExceptionClass::Exit),
        _ => Err(ReplayLogFileError::InvalidFormat("invalid exception class")),
    }
}

fn write_string(out: &mut Vec<u8>, value: &str) -> Result<(), ReplayLogFileError> {
    write_bytes(out, value.as_bytes())
}

fn read_string(cursor: &mut Cursor<&[u8]>) -> Result<String, ReplayLogFileError> {
    let bytes = read_bytes(cursor)?;
    String::from_utf8(bytes.to_vec())
        .map_err(|_| ReplayLogFileError::InvalidFormat("invalid utf-8 string"))
}

fn write_bytes(out: &mut Vec<u8>, bytes: &[u8]) -> Result<(), ReplayLogFileError> {
    if bytes.len() > MAX_TERM_BYTES {
        return Err(ReplayLogFileError::InvalidFormat(
            "replay byte field too large",
        ));
    }
    write_u64(out, usize_to_u64(bytes.len())?);
    out.extend_from_slice(bytes);
    Ok(())
}

fn read_bytes<'a>(cursor: &mut Cursor<&'a [u8]>) -> Result<&'a [u8], ReplayLogFileError> {
    let len = u64_to_usize(read_u64(cursor)?)?;
    if len > MAX_TERM_BYTES || len > remaining(cursor) {
        return Err(ReplayLogFileError::InvalidFormat(
            "invalid byte field length",
        ));
    }
    let start = cursor.position() as usize;
    cursor.set_position(cursor.position().saturating_add(len as u64));
    Ok(&cursor.get_ref()[start..start + len])
}

fn write_option_u64(out: &mut Vec<u8>, value: Option<u64>) {
    match value {
        Some(value) => {
            out.push(1);
            write_u64(out, value);
        }
        None => out.push(0),
    }
}

fn read_option_u64(cursor: &mut Cursor<&[u8]>) -> Result<Option<u64>, ReplayLogFileError> {
    match read_u8(cursor)? {
        0 => Ok(None),
        1 => Ok(Some(read_u64(cursor)?)),
        _ => Err(ReplayLogFileError::InvalidFormat("invalid option tag")),
    }
}

fn write_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn write_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn read_u8(cursor: &mut Cursor<&[u8]>) -> Result<u8, ReplayLogFileError> {
    let mut value = [0_u8; 1];
    cursor
        .read_exact(&mut value)
        .map_err(|_| ReplayLogFileError::InvalidFormat("truncated replay log"))?;
    Ok(value[0])
}

fn read_u16(cursor: &mut Cursor<&[u8]>) -> Result<u16, ReplayLogFileError> {
    let mut value = [0_u8; 2];
    cursor
        .read_exact(&mut value)
        .map_err(|_| ReplayLogFileError::InvalidFormat("truncated replay log"))?;
    Ok(u16::from_le_bytes(value))
}

fn read_fixed_u32(cursor: &mut Cursor<&[u8]>) -> Result<u32, ReplayLogFileError> {
    let mut value = [0_u8; 4];
    cursor
        .read_exact(&mut value)
        .map_err(|_| ReplayLogFileError::InvalidFormat("truncated replay log"))?;
    Ok(u32::from_le_bytes(value))
}

fn read_u64(cursor: &mut Cursor<&[u8]>) -> Result<u64, ReplayLogFileError> {
    let mut value = [0_u8; 8];
    cursor
        .read_exact(&mut value)
        .map_err(|_| ReplayLogFileError::InvalidFormat("truncated replay log"))?;
    Ok(u64::from_le_bytes(value))
}

fn usize_to_u64(value: usize) -> Result<u64, ReplayLogFileError> {
    u64::try_from(value).map_err(|_| ReplayLogFileError::InvalidFormat("value too large"))
}

fn u64_to_usize(value: u64) -> Result<usize, ReplayLogFileError> {
    usize::try_from(value).map_err(|_| ReplayLogFileError::InvalidFormat("value too large"))
}

fn remaining(cursor: &Cursor<&[u8]>) -> usize {
    cursor
        .get_ref()
        .len()
        .saturating_sub(cursor.position() as usize)
}
