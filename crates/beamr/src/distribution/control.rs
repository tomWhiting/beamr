//! Distribution control message framing, SEND/REG_SEND handling, and remote
//! spawn helpers (SPAWN_REQUEST/SPAWN_REPLY).

use std::fmt;

use crate::atom::{Atom, AtomTable};
use crate::etf::decode::{DecodeError, decode_term};
use crate::etf::encode::{EncodeError, encode_term};
use crate::native::ProcessContext;
use crate::native::spawn::{SpawnError, SpawnFacility, SpawnOptions};
use crate::process::Process;
use crate::term::Term;
use crate::term::boxed::{Cons, Tuple};
use crate::term::pid_ref::PidRef;

/// Distribution control opcode for PID-to-PID send.
pub const SEND: i64 = 2;
/// Distribution control opcode for registered-name send.
pub const REG_SEND: i64 = 6;

/// Error raised when a remote send cannot be completed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DistributionSendError {
    /// The target node has no usable distribution connection.
    NoConnection,
    /// The target PID or message cannot be encoded for distribution.
    Encode,
}

impl fmt::Display for DistributionSendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoConnection => formatter.write_str("noconnection"),
            Self::Encode => formatter.write_str("distribution encode failed"),
        }
    }
}

/// Facility used by opcodes and BIFs to send a message to a remote PID.
pub trait DistributionSendFacility: Send + Sync {
    /// Encode and send `message` to `target` on its remote node.
    fn send_remote(&self, target: Term, message: Term) -> Result<(), DistributionSendError>;
}

/// Scheduler-safe delivery target for incoming decoded control messages.
pub trait ControlDelivery: Send + Sync {
    /// Decode `payload_etf` for `target_pid` and enqueue it in the target mailbox.
    fn deliver_payload(&self, target_pid: u64, payload_etf: &[u8]) -> bool;
}

/// Registry lookup used by incoming REG_SEND controls.
pub trait ControlRegistry: Send + Sync {
    /// Resolve a registered atom name to a local pid.
    fn whereis(&self, name: Atom) -> Option<u64>;
}

/// Decoded distribution control message.
///
/// Fields are extracted values rather than raw Terms because the decode
/// process heap is temporary — boxed Terms would become dangling after return.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlMessage {
    /// `{2, Cookie, ToPid}` — stores extracted pid number.
    Send { to_pid: u64 },
    /// `{6, FromPid, Cookie, ToName}` — stores extracted name atom.
    RegSend { to_name: Atom },
}

/// Errors while decoding or handling a distribution control frame.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ControlError {
    /// The frame prefix or lengths were invalid.
    InvalidFrame,
    /// ETF decoding failed.
    Decode(DecodeError),
    /// Control tuple shape was not SEND or REG_SEND.
    InvalidControl,
}

impl From<DecodeError> for ControlError {
    fn from(error: DecodeError) -> Self {
        Self::Decode(error)
    }
}

/// Encode a framed SEND control and payload.
pub fn encode_send_frame(
    cookie: Term,
    to_pid: Term,
    message: Term,
    atom_table: &AtomTable,
) -> Result<Vec<u8>, EncodeError> {
    let mut process = Process::new(0, 32);
    let mut context = ProcessContext::new();
    context.attach_process(&mut process, 0);
    let control = context
        .alloc_tuple(&[Term::small_int(SEND), cookie, to_pid])
        .map_err(|_| EncodeError::UnsupportedTerm)?;
    encode_frame(control, message, atom_table)
}

/// Encode a framed REG_SEND control and payload.
pub fn encode_reg_send_frame(
    from_pid: Term,
    cookie: Term,
    to_name: Atom,
    message: Term,
    atom_table: &AtomTable,
) -> Result<Vec<u8>, EncodeError> {
    let mut process = Process::new(0, 32);
    let mut context = ProcessContext::new();
    context.attach_process(&mut process, 0);
    let control = context
        .alloc_tuple(&[
            Term::small_int(REG_SEND),
            from_pid,
            cookie,
            Term::atom(to_name),
        ])
        .map_err(|_| EncodeError::UnsupportedTerm)?;
    encode_frame(control, message, atom_table)
}

fn encode_frame(
    control: Term,
    message: Term,
    atom_table: &AtomTable,
) -> Result<Vec<u8>, EncodeError> {
    let control_etf = encode_term(control, atom_table)?;
    let payload_etf = encode_term(message, atom_table)?;
    let control_len = u32::try_from(control_etf.len()).map_err(|_| EncodeError::UnsupportedTerm)?;
    let payload_len = u32::try_from(payload_etf.len()).map_err(|_| EncodeError::UnsupportedTerm)?;
    let mut frame = Vec::with_capacity(8 + control_etf.len() + payload_etf.len());
    frame.extend_from_slice(&control_len.to_be_bytes());
    frame.extend_from_slice(&payload_len.to_be_bytes());
    frame.extend_from_slice(&control_etf);
    frame.extend_from_slice(&payload_etf);
    Ok(frame)
}

/// Split a frame produced by [`encode_send_frame`] or [`encode_reg_send_frame`].
pub fn split_frame(frame: &[u8]) -> Result<(&[u8], &[u8]), ControlError> {
    let header = frame.get(..8).ok_or(ControlError::InvalidFrame)?;
    let control_len = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
    let payload_len = u32::from_be_bytes([header[4], header[5], header[6], header[7]]) as usize;
    let control_start = 8_usize;
    let payload_start = control_start
        .checked_add(control_len)
        .ok_or(ControlError::InvalidFrame)?;
    let end = payload_start
        .checked_add(payload_len)
        .ok_or(ControlError::InvalidFrame)?;
    if end != frame.len() {
        return Err(ControlError::InvalidFrame);
    }
    let control = frame
        .get(control_start..payload_start)
        .ok_or(ControlError::InvalidFrame)?;
    let payload = frame
        .get(payload_start..end)
        .ok_or(ControlError::InvalidFrame)?;
    Ok((control, payload))
}

/// Decode a control ETF term.
pub fn decode_control(
    control_etf: &[u8],
    atom_table: &AtomTable,
) -> Result<ControlMessage, ControlError> {
    let mut process = Process::new(0, 64);
    let mut context = ProcessContext::new();
    context.attach_process(&mut process, 0);
    let term = decode_term(control_etf, &mut context, atom_table)?;
    let tuple = Tuple::new(term).ok_or(ControlError::InvalidControl)?;
    match tuple.get(0).and_then(Term::as_small_int) {
        Some(SEND) if tuple.arity() == 3 => {
            let to = tuple.get(2).ok_or(ControlError::InvalidControl)?;
            let to_pid = PidRef::new(to)
                .ok_or(ControlError::InvalidControl)?
                .pid_number();
            Ok(ControlMessage::Send { to_pid })
        }
        Some(REG_SEND) if tuple.arity() == 4 => {
            let to_name = tuple
                .get(3)
                .and_then(Term::as_atom)
                .ok_or(ControlError::InvalidControl)?;
            Ok(ControlMessage::RegSend { to_name })
        }
        _ => Err(ControlError::InvalidControl),
    }
}

/// Handle an incoming frame by decoding the control term and delivering the payload.
pub fn handle_frame(
    control_etf: &[u8],
    payload_etf: &[u8],
    atom_table: &AtomTable,
    delivery: &dyn ControlDelivery,
    registry: Option<&dyn ControlRegistry>,
) -> Result<bool, ControlError> {
    match decode_control(control_etf, atom_table)? {
        ControlMessage::Send { to_pid } => Ok(delivery.deliver_payload(to_pid, payload_etf)),
        ControlMessage::RegSend { to_name } => {
            let Some(registry) = registry else {
                return Ok(false);
            };
            let Some(pid) = registry.whereis(to_name) else {
                return Ok(false);
            };
            Ok(delivery.deliver_payload(pid, payload_etf))
        }
    }
}

// ── SPAWN_REQUEST / SPAWN_REPLY ─────────────────────────────────────────────

/// Distribution control opcode for SPAWN_REQUEST.
pub const SPAWN_REQUEST: i64 = 29;
/// Distribution control opcode for SPAWN_REPLY.
pub const SPAWN_REPLY: i64 = 31;

/// Module/function/arguments entry point carried by SPAWN_REQUEST.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteMfa {
    /// Target module atom.
    pub module: Atom,
    /// Target function atom.
    pub function: Atom,
    /// Arguments list.
    pub args: Vec<Term>,
}

/// Parsed SPAWN_REQUEST control message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpawnRequest {
    /// Unique request identifier correlating request/reply.
    pub request_id: u64,
    /// The sender PID term.
    pub from: Term,
    /// The group leader PID term.
    pub group_leader: Term,
    /// Module/function/arguments entry point.
    pub mfa: RemoteMfa,
    /// Spawn options (link, monitor).
    pub options: SpawnOptions,
}

/// Parsed SPAWN_REPLY control message.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SpawnReply {
    /// Request id from the original SPAWN_REQUEST.
    pub request_id: u64,
    /// The newly spawned PID term.
    pub pid: Term,
}

/// Error returned while parsing a distribution control term for spawn operations.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ControlDecodeError {
    /// The control term was not a tuple.
    NotTuple,
    /// The control tuple had the wrong number of elements.
    BadArity,
    /// The opcode was unknown or did not match the expected operation.
    UnknownOp,
    /// The request id was not a non-negative integer.
    BadRequestId,
    /// The MFA entry point was malformed.
    BadMfa,
    /// The spawn option list was malformed.
    BadOptions,
    /// The PID element was not a valid PID term.
    BadPid,
}

/// Error returned by a local SPAWN_REQUEST handler.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SpawnRequestError {
    /// The control tuple could not be decoded.
    Decode(ControlDecodeError),
    /// No caller PID was set on the process context.
    MissingCallerPid,
    /// The local spawn facility returned an error.
    Spawn(SpawnError),
    /// The spawned PID is out of u32 wire range or allocation failed.
    PidOutOfRange,
}

/// Decode a `{29, ReqId, From, GroupLeader, {M,F,A}, OptList}` control term.
pub fn decode_spawn_request(
    term: Term,
    context: &ProcessContext<'_>,
) -> Result<SpawnRequest, ControlDecodeError> {
    let tuple = Tuple::new(term).ok_or(ControlDecodeError::NotTuple)?;
    if tuple.arity() != 6 {
        return Err(ControlDecodeError::BadArity);
    }
    let op = tuple
        .get(0)
        .and_then(|t| t.as_small_int())
        .ok_or(ControlDecodeError::UnknownOp)?;
    if op != SPAWN_REQUEST {
        return Err(ControlDecodeError::UnknownOp);
    }
    let request_id = parse_non_negative_u64(tuple.get(1).ok_or(ControlDecodeError::BadRequestId)?)
        .ok_or(ControlDecodeError::BadRequestId)?;
    let from = tuple.get(2).ok_or(ControlDecodeError::BadArity)?;
    let group_leader = tuple.get(3).ok_or(ControlDecodeError::BadArity)?;
    let mfa = parse_mfa(tuple.get(4).ok_or(ControlDecodeError::BadMfa)?)?;
    let options =
        parse_remote_spawn_options(tuple.get(5).ok_or(ControlDecodeError::BadOptions)?, context)?;

    Ok(SpawnRequest {
        request_id,
        from,
        group_leader,
        mfa,
        options,
    })
}

/// Decode a `{31, ReqId, Pid}` control term.
pub fn decode_spawn_reply(term: Term) -> Result<SpawnReply, ControlDecodeError> {
    let tuple = Tuple::new(term).ok_or(ControlDecodeError::NotTuple)?;
    if tuple.arity() != 3 {
        return Err(ControlDecodeError::BadArity);
    }
    let op = tuple
        .get(0)
        .and_then(|t| t.as_small_int())
        .ok_or(ControlDecodeError::UnknownOp)?;
    if op != SPAWN_REPLY {
        return Err(ControlDecodeError::UnknownOp);
    }
    let request_id = parse_non_negative_u64(tuple.get(1).ok_or(ControlDecodeError::BadRequestId)?)
        .ok_or(ControlDecodeError::BadRequestId)?;
    let pid = tuple.get(2).ok_or(ControlDecodeError::BadPid)?;
    if PidRef::new(pid).is_none() {
        return Err(ControlDecodeError::BadPid);
    }
    Ok(SpawnReply { request_id, pid })
}

/// Allocate a SPAWN_REQUEST control tuple on `context`'s process heap.
pub fn alloc_spawn_request(
    context: &mut ProcessContext<'_>,
    request: &SpawnRequest,
) -> Result<Term, Term> {
    let args = context.alloc_list(&request.mfa.args)?;
    let mfa = context.alloc_tuple(&[
        Term::atom(request.mfa.module),
        Term::atom(request.mfa.function),
        args,
    ])?;
    let opt_list = spawn_options_to_list(context, request.options.clone())?;
    let op = Term::try_small_int(SPAWN_REQUEST).ok_or_else(badarg)?;
    let req_id = Term::try_small_int(i64::try_from(request.request_id).map_err(|_| badarg())?)
        .ok_or_else(badarg)?;
    context.alloc_tuple(&[
        op,
        req_id,
        request.from,
        request.group_leader,
        mfa,
        opt_list,
    ])
}

/// Allocate a SPAWN_REPLY control tuple on `context`'s process heap.
pub fn alloc_spawn_reply(
    context: &mut ProcessContext<'_>,
    request_id: u64,
    pid: Term,
) -> Result<Term, Term> {
    let op = Term::try_small_int(SPAWN_REPLY).ok_or_else(badarg)?;
    let req_id =
        Term::try_small_int(i64::try_from(request_id).map_err(|_| badarg())?).ok_or_else(badarg)?;
    context.alloc_tuple(&[op, req_id, pid])
}

/// Handle a decoded SPAWN_REQUEST by spawning locally with link/monitor options
/// applied atomically.
///
/// The current scheduler spawn API is local-caller based; until remote
/// link/monitor metadata is represented in the scheduler, this uses the supplied
/// local service caller PID as the atomic-options owner rather than pretending
/// the external `From` PID is local.
pub fn handle_spawn_request(
    term: Term,
    context: &mut ProcessContext<'_>,
    spawn_facility: &dyn SpawnFacility,
) -> Result<Term, SpawnRequestError> {
    let request = decode_spawn_request(term, context).map_err(SpawnRequestError::Decode)?;
    let caller_pid = context.pid().ok_or(SpawnRequestError::MissingCallerPid)?;
    let result = spawn_facility
        .spawn_with_options(
            caller_pid,
            request.mfa.module,
            request.mfa.function,
            request.mfa.args,
            request.options,
        )
        .map_err(SpawnRequestError::Spawn)?;
    let pid_term = spawn_reply_pid(context, result.pid)?;
    alloc_spawn_reply(context, request.request_id, pid_term)
        .map_err(|_| SpawnRequestError::PidOutOfRange)
}

fn spawn_reply_pid(context: &mut ProcessContext<'_>, pid: u64) -> Result<Term, SpawnRequestError> {
    if let Some(local_node) = context.local_node() {
        let pid_number = u32::try_from(pid).map_err(|_| SpawnRequestError::PidOutOfRange)?;
        return context
            .alloc_external_pid(local_node.name, u64::from(pid_number), 0)
            .map_err(|_| SpawnRequestError::PidOutOfRange);
    }

    Term::try_pid(pid).ok_or(SpawnRequestError::PidOutOfRange)
}

fn parse_mfa(term: Term) -> Result<RemoteMfa, ControlDecodeError> {
    let tuple = Tuple::new(term).ok_or(ControlDecodeError::BadMfa)?;
    if tuple.arity() != 3 {
        return Err(ControlDecodeError::BadMfa);
    }
    let module = tuple
        .get(0)
        .and_then(|t| t.as_atom())
        .ok_or(ControlDecodeError::BadMfa)?;
    let function = tuple
        .get(1)
        .and_then(|t| t.as_atom())
        .ok_or(ControlDecodeError::BadMfa)?;
    let args = spawn_list_to_vec(tuple.get(2).ok_or(ControlDecodeError::BadMfa)?)
        .ok_or(ControlDecodeError::BadMfa)?;
    Ok(RemoteMfa {
        module,
        function,
        args,
    })
}

fn parse_remote_spawn_options(
    term: Term,
    context: &ProcessContext<'_>,
) -> Result<SpawnOptions, ControlDecodeError> {
    let atom_table = context.atom_table().ok_or(ControlDecodeError::BadOptions)?;
    let link_atom = atom_table.intern("link");
    let monitor_atom = atom_table.intern("monitor");
    let mut options = SpawnOptions::default();
    for option in spawn_list_to_vec(term).ok_or(ControlDecodeError::BadOptions)? {
        if option.as_atom() == Some(link_atom) {
            options.link = true;
        } else if option.as_atom() == Some(monitor_atom) {
            options.monitor = true;
        } else {
            return Err(ControlDecodeError::BadOptions);
        }
    }
    Ok(options)
}

fn spawn_options_to_list(
    context: &mut ProcessContext<'_>,
    options: SpawnOptions,
) -> Result<Term, Term> {
    let atom_table = context.atom_table().ok_or_else(badarg)?;
    let mut elements = Vec::new();
    if options.link {
        elements.push(Term::atom(atom_table.intern("link")));
    }
    if options.monitor {
        elements.push(Term::atom(atom_table.intern("monitor")));
    }
    context.alloc_list(&elements)
}

fn spawn_list_to_vec(term: Term) -> Option<Vec<Term>> {
    let mut elements = Vec::new();
    let mut current = term;
    loop {
        if current.is_nil() {
            return Some(elements);
        }
        let cons = Cons::new(current)?;
        elements.push(cons.head());
        current = cons.tail();
    }
}

fn parse_non_negative_u64(term: Term) -> Option<u64> {
    let value = term.as_small_int()?;
    if value < 0 {
        return None;
    }
    u64::try_from(value).ok()
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;

    struct TestDelivery {
        messages: Mutex<HashMap<u64, Vec<Term>>>,
        atom_table: AtomTable,
    }

    impl TestDelivery {
        fn new() -> Self {
            Self {
                messages: Mutex::new(HashMap::new()),
                atom_table: AtomTable::with_common_atoms(),
            }
        }
    }

    impl ControlDelivery for TestDelivery {
        fn deliver_payload(&self, target_pid: u64, payload_etf: &[u8]) -> bool {
            let mut process = Process::new(target_pid, 64);
            let mut context = ProcessContext::new();
            context.attach_process(&mut process, 0);
            let Ok(message) = decode_term(payload_etf, &mut context, &self.atom_table) else {
                return false;
            };
            self.messages
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .entry(target_pid)
                .or_default()
                .push(message);
            true
        }
    }

    struct TestRegistry(Atom, u64);

    impl ControlRegistry for TestRegistry {
        fn whereis(&self, name: Atom) -> Option<u64> {
            (name == self.0).then_some(self.1)
        }
    }

    #[test]
    fn send_control_delivers_payload_to_pid() {
        let atom_table = AtomTable::with_common_atoms();
        let frame = encode_send_frame(
            Term::atom(Atom::OK),
            Term::pid(7),
            Term::atom(Atom::OK),
            &atom_table,
        )
        .expect("frame encodes");
        let (control, payload) = split_frame(&frame).expect("frame splits");
        let delivery = TestDelivery::new();

        assert_eq!(
            handle_frame(control, payload, &atom_table, &delivery, None),
            Ok(true)
        );
        let messages = delivery
            .messages
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        assert_eq!(
            messages.get(&7).and_then(|values| values.first()).copied(),
            Some(Term::atom(Atom::OK))
        );
    }

    #[test]
    fn reg_send_control_resolves_name_before_delivery() {
        let atom_table = AtomTable::with_common_atoms();
        let name = atom_table.intern("receiver");
        let frame = encode_reg_send_frame(
            Term::pid(1),
            Term::atom(Atom::OK),
            name,
            Term::atom(Atom::OK),
            &atom_table,
        )
        .expect("frame encodes");
        let (control, payload) = split_frame(&frame).expect("frame splits");
        let delivery = TestDelivery::new();
        let registry = TestRegistry(name, 9);

        assert_eq!(
            handle_frame(control, payload, &atom_table, &delivery, Some(&registry)),
            Ok(true)
        );
        let messages = delivery
            .messages
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        assert_eq!(
            messages.get(&9).and_then(|values| values.first()).copied(),
            Some(Term::atom(Atom::OK))
        );
    }

    // ── SPAWN_REQUEST / SPAWN_REPLY tests ───────────────────────────────

    use crate::distribution::Node;
    use crate::native::spawn::{SpawnMonitorResult, SpawnOptionsResult};
    use crate::term::boxed::{write_cons, write_external_pid, write_tuple};

    struct MockSpawnFacility {
        next_pid: u64,
        records: Mutex<Vec<(u64, Atom, Atom, Vec<Term>, SpawnOptions)>>,
    }

    impl MockSpawnFacility {
        fn new(next_pid: u64) -> Self {
            Self {
                next_pid,
                records: Mutex::new(Vec::new()),
            }
        }
    }

    impl SpawnFacility for MockSpawnFacility {
        fn spawn(
            &self,
            _caller_pid: u64,
            _module: Atom,
            _function: Atom,
            _args: Vec<Term>,
            _link_to: Option<u64>,
        ) -> Result<u64, SpawnError> {
            Ok(self.next_pid)
        }

        fn spawn_monitor(
            &self,
            _caller_pid: u64,
            _module: Atom,
            _function: Atom,
            _args: Vec<Term>,
        ) -> Result<SpawnMonitorResult, SpawnError> {
            Ok(SpawnMonitorResult {
                pid: self.next_pid,
                reference: 0,
            })
        }

        fn spawn_lambda(
            &self,
            _caller_pid: u64,
            _module: Atom,
            _lambda_index: u32,
            _link_to: Option<u64>,
        ) -> Result<u64, SpawnError> {
            Ok(self.next_pid)
        }

        fn spawn_lambda_monitor(
            &self,
            _caller_pid: u64,
            _module: Atom,
            _lambda_index: u32,
        ) -> Result<SpawnMonitorResult, SpawnError> {
            Ok(SpawnMonitorResult {
                pid: self.next_pid,
                reference: 0,
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
            self.records
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push((caller_pid, module, function, args, options));
            Ok(SpawnOptionsResult {
                pid: self.next_pid,
                reference: options.monitor.then_some(1),
            })
        }

        fn spawn_lambda_with_options(
            &self,
            _caller_pid: u64,
            _module: Atom,
            _lambda_index: u32,
            _options: SpawnOptions,
        ) -> Result<SpawnOptionsResult, SpawnError> {
            Ok(SpawnOptionsResult {
                pid: self.next_pid,
                reference: None,
            })
        }
    }

    #[test]
    fn decodes_spawn_request_with_link_monitor_options() {
        let atoms = std::sync::Arc::new(AtomTable::with_common_atoms());
        let module = atoms.intern("sample");
        let function = atoms.intern("run");
        let link = atoms.intern("link");
        let monitor = atoms.intern("monitor");
        let mut process = Process::new(1, 128);
        let mut context = ProcessContext::new();
        context.set_atom_table(Some(atoms));
        context.attach_process(&mut process, 0);

        let mut arg_list_heap = [0_u64; 2];
        let arg_list =
            write_cons(&mut arg_list_heap, Term::small_int(7), Term::NIL).expect("arg list fits");
        let mut mfa_heap = [0_u64; 4];
        let mfa = write_tuple(
            &mut mfa_heap,
            &[Term::atom(module), Term::atom(function), arg_list],
        )
        .expect("mfa tuple fits");
        let mut opt2_heap = [0_u64; 2];
        let opt_tail = write_cons(&mut opt2_heap, Term::atom(monitor), Term::NIL)
            .expect("monitor option fits");
        let mut opt1_heap = [0_u64; 2];
        let opt_list =
            write_cons(&mut opt1_heap, Term::atom(link), opt_tail).expect("link option fits");
        let mut from_heap = [0_u64; 4];
        let from = write_external_pid(&mut from_heap, module, 99, 0).expect("from pid fits");
        let mut gl_heap = [0_u64; 4];
        let group_leader =
            write_external_pid(&mut gl_heap, module, 1, 0).expect("group leader fits");
        let mut request_heap = [0_u64; 7];
        let request_term = write_tuple(
            &mut request_heap,
            &[
                Term::small_int(29),
                Term::small_int(42),
                from,
                group_leader,
                mfa,
                opt_list,
            ],
        )
        .expect("request tuple fits");

        let request = decode_spawn_request(request_term, &context).expect("spawn request decodes");

        assert_eq!(request.request_id, 42);
        assert_eq!(request.from, from);
        assert_eq!(request.group_leader, group_leader);
        assert_eq!(request.mfa.module, module);
        assert_eq!(request.mfa.function, function);
        assert_eq!(request.mfa.args, vec![Term::small_int(7)]);
        assert!(request.options.link);
        assert!(request.options.monitor);
        assert_eq!(request.options.priority, None);
        assert_eq!(request.options.min_heap_size, None);
    }

    #[test]
    fn handle_spawn_request_creates_local_process_and_reply() {
        let atoms = std::sync::Arc::new(AtomTable::with_common_atoms());
        let module = atoms.intern("sample");
        let function = atoms.intern("run");
        let link = atoms.intern("link");
        let local_node_name = atoms.intern("local@host");
        let mut process = Process::new(100, 128);
        let mut context = ProcessContext::new();
        context.set_pid(Some(100));
        context.set_atom_table(Some(atoms));
        context.set_local_node(Some(Node::new(local_node_name, 0)));
        context.attach_process(&mut process, 0);

        let mut mfa_heap = [0_u64; 4];
        let mfa = write_tuple(
            &mut mfa_heap,
            &[Term::atom(module), Term::atom(function), Term::NIL],
        )
        .expect("mfa tuple fits");
        let mut opt_heap = [0_u64; 2];
        let opt_list =
            write_cons(&mut opt_heap, Term::atom(link), Term::NIL).expect("option list fits");
        let mut request_heap = [0_u64; 7];
        let request = write_tuple(
            &mut request_heap,
            &[
                Term::small_int(29),
                Term::small_int(5),
                Term::pid(100),
                Term::pid(100),
                mfa,
                opt_list,
            ],
        )
        .expect("request tuple fits");
        let facility = MockSpawnFacility::new(321);

        let reply = handle_spawn_request(request, &mut context, &facility).expect("spawn handled");
        let decoded = decode_spawn_reply(reply).expect("reply decodes");

        assert_eq!(decoded.request_id, 5);
        let pid = PidRef::new(decoded.pid).expect("reply pid");
        assert!(!pid.is_local());
        assert_eq!(pid.node(), Some(local_node_name));
        assert_eq!(pid.pid_number(), 321);
        let records = facility
            .records
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].0, 100);
        assert_eq!(records[0].1, module);
        assert_eq!(records[0].2, function);
        assert!(records[0].4.link);
        assert!(!records[0].4.monitor);
    }

    #[test]
    fn alloc_spawn_reply_encodes_op_31() {
        let atoms = std::sync::Arc::new(AtomTable::with_common_atoms());
        let mut process = Process::new(1, 128);
        let mut context = ProcessContext::new();
        context.set_atom_table(Some(atoms));
        context.attach_process(&mut process, 0);

        let reply = alloc_spawn_reply(&mut context, 77, Term::pid(9)).expect("reply allocated");
        let tuple = Tuple::new(reply).expect("reply tuple");

        assert_eq!(tuple.arity(), 3);
        assert_eq!(tuple.get(0), Some(Term::small_int(31)));
        assert_eq!(tuple.get(1), Some(Term::small_int(77)));
        assert_eq!(tuple.get(2), Some(Term::pid(9)));
    }
}
