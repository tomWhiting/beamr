# beamr distribution finishing — spec for SRV-005 foundation

Two pieces. Build PIECE 2 (distributed pg) first (smaller, mostly assembly), then PIECE 1 (handshake).
Goal: real cross-node process-group propagation + authenticated named node connections, so liminal SRV-005
can build cluster pub/sub on real primitives (no liminal-side hack, no address-trust). Every symbol below is
verified against beamr source; read the cited file:line before editing.

## PIECE 2 — distributed pg (replace NullPgPropagation)
Already exist (pg.rs): PgUpdate::{Join,Leave}{scope,group,pid}; PgPropagation trait + broadcast; apply_remote_join
(pg.rs:178); apply_remote_leave (pg.rs:201); purge_remote_node(node) (pg.rs:240 — gives SRV-005 R6 for free);
remove_pid_from_all_scopes broadcasts leaves on local exit (pg.rs:219). MISSING: real propagation impl, inbound
decode, wiring, node-down hook.

Build (Approach B — dedicated control op, mailbox-free fast path):
1. control.rs: add `pub const PG_UPDATE: i64 = 101` (confirm unused vs control_lifecycle.rs:46-60 ControlOp::from_opcode;
   OTP's highest here is 31; pick high private + document). Add ControlMessage::PgJoin/PgLeave{scope,group,node,pid_number,serial}.
   `encode_pg_update_frame(update, atom_table)` reusing encode_frame (control.rs:124): control tuple {101, Join|Leave, Scope,
   Group, MemberExternalPid}, empty payload. **MUST encode member as an EXTERNAL pid carrying local node name** (technique:
   spawn_reply_pid control.rs:410-419) else receiver records node=None and corrupts RemoteMember. Extend decode_control
   (control.rs:166-192) to recognize PG_UPDATE → read discriminant/scope/group + member via PidRef (node()/pid_number()/serial(),
   as DistributedPid::from_term control_lifecycle.rs:83).
2. control.rs: add `pub trait PgDelivery { fn apply_pg_join(..); fn apply_pg_leave(..); }`. handle_frame (control.rs:195-214)
   gains param `pg: Option<&dyn PgDelivery>` + match arms routing PgJoin/PgLeave → pg.apply_*. (Tests control.rs:550-605 pass None.)
3. supervision_integration.rs: `struct SchedulerPgPropagation { shared: Weak<SharedState> }` impl PgPropagation::broadcast:
   upgrade weak; encode frame (member = external pid on local node); for node in shared.distribution_connections.connected_nodes()
   (connection.rs:378) → block_on_distribution_send (supervision_integration.rs:669, reuse). impl PgDelivery on
   SchedulerDistributionSendFacility (holds Arc<SharedState>) → shared.pg_registry.apply_remote_join/leave. Update
   register_distribution_control_handler (supervision_integration.rs:77-93) to pass Some(&facility) as pg arg to handle_frame.
4. pg.rs: add `set_propagation(&self, Arc<dyn PgPropagation>)` (make propagation an Arc<RwLock<Arc<dyn PgPropagation>>> swap)
   to break the Arc cycle (pg_registry is a field of SharedState but propagation needs SharedState).
5. scheduler/mod.rs: keep PgRegistry::new at :638 (Null initially); AFTER `shared` built (~:714, where the control handler is
   already registered) call shared.pg_registry.set_propagation(Arc::new(SchedulerPgPropagation{ shared: Arc::downgrade(&shared) }));
   AND register connection-down hook: shared.distribution_connections.register_connection_down(move |event| { weak.upgrade →
   shared.pg_registry.purge_remote_node(event.node) }) (connection.rs:305; ConnectionDownEvent.node connection.rs:65). = R6 free.
6. Public API: `Scheduler::pg_registry(&self) -> Arc<PgRegistry>` (mirror distribution_connections() scheduler/mod.rs:925).
HAZARDS: Weak everywhere (propagation + down-closure) or SharedState never drops. broadcast OUTSIDE PgState lock (pg.rs:122-137
already drops guard before broadcast — keep it). block_on_distribution_send may block calling thread (low join/leave freq — OK;
queue later if hot). handle_frame sig change → update its 1 prod caller (supervision_integration.rs:85) + tests (None).
TESTS: encode→split_frame→decode round-trip Join+Leave (node/pid/serial preserved); broadcast 1 frame/connected node (loopback);
inbound PG_UPDATE → apply_remote_join → remote_members reflects; 2-node pg:join on A → B's remote_members includes A's external
pid; node-down → B's remote_members drops A (purge via hook); local exit still broadcasts leave (now transmits).

## PIECE 1 — wire OTP handshake into ConnectionManager (resolve "B-115" connection.rs:314)
handshake.rs codec complete + tested but sync (Read+Write) and NEVER called outside its tests. connect() (connection.rs:437)
+ accept path (handle_accepted connection.rs:554) register connections with NO handshake, identity via address→atom seam.
FRAMING: handshake = 2-byte BE length prefix (write_packet/read_packet handshake.rs:367-390); data = 8-byte [control_len|
payload_len] (spawn_read_lifecycle connection.rs:497-509). Handshake MUST complete BEFORE spawn_read_lifecycle; phase boundary
positional (no in-band tag). Sequence: TCP connect → [handshake: 2-byte packets] → Ok → into_split + register_connection +
spawn_read_lifecycle [data frames].
Build (Option B — async twins): handshake.rs add `initiate_handshake_async<S: AsyncRead+AsyncWrite+Unpin>(stream, local:
&HandshakeNode, cookie, challenge) -> Result<HandshakeResult,HandshakeError>` + `respond_handshake_async(...)` mirroring sync
bodies handshake.rs:278-341 (same packet order: initiator N→s→N→r→a; responder N→s→N→r→a), reusing pure helpers (encode_name
:392, read_status_packet :438, challenge_digest :344 MD5(cookie||challenge), encode_challenge_reply :423, read_challenge_ack
:477, constant_time_eq). Add async write_packet_async/read_packet_async (2-byte prefix, mirror :367-390). gen_challenge: random
u32 (counter/rng, not security-critical given cookie).
connection.rs: ConnectionManagerInner gains cookie/local_node_name/local_creation (plumb new/with_connect_timeout :264-290).
connect(): after TcpStream connect, BEFORE register_connection: local=HandshakeNode::with_default_flags(name,creation);
result=initiate_handshake_async(&mut stream,&local,&cookie,gen_challenge()).await.map_err(→ConnectError::Io)?; node=atom_table.
intern(result.remote_name()); register_connection(node, peer_addr, stream). handle_accepted(): tokio::spawn async →
respond_handshake_async → on Ok register_connection(intern(result.remote_name())), on Err drop stream. DELETE pending_inbound,
inbound_identifier, register/unregister_inbound_identifier, identify_pending_inbound, pending_inbound_count (connection.rs:237,
312-353,361-365,571-584).
Cookie via DistributionConfig (mod.rs:28-33; cookie distribution-scoped; node_name/creation already on SchedulerConfig :95-96).
Thread cookie+node_name+creation into ConnectionManager construction at scheduler/mod.rs:614-616. Failure: connect→ConnectError::
Io(String) → connect_node false; inbound→drop stream. Public API: `Scheduler::start_distribution_listener(&self, addr) ->
io::Result<AcceptHandle>` calling distribution_connections.listen (connection.rs:420), return AcceptHandle (drop aborts accept,
connection.rs:222 — caller keeps it alive).
HAZARDS: inbound handshake now async+spawned → connection_count not synchronously updated (tests poll). Wire handshake into
distribution_connections (scheduler/mod.rs:615), NOT the separate NetKernel manager (:635). with_default_flags requires non-empty
name. TESTS: async handshake round-trip over loopback TcpStream; connect registers under remote's handshake name (not resolver
key); bad cookie → Err + no entry + peer DigestMismatch; inbound peer in connected_nodes() keyed by advertised name w/ no
inbound_identifier; 2-node send-after-handshake end-to-end.

## After both land: bump beamr (real new caps), then liminal SRV-005 on pg groups (channel=group, subscribe=pg.join,
## publish fan-out to remote_members, node-down auto-drops = R6) + discovery/membership (listener + seeds + down-hook).
