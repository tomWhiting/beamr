# Distribution Handshake — Connection-Establishment Redesign

**Status:** Design (no code changes in this branch beyond this doc)
**Scope:** `beamr` `net`-gated distribution (`crates/beamr/src/distribution/`)
**Motivation:** A real multi-process spike showed the distribution handshake
**deadlocks for a 3+-peer mesh across OS processes**. This blocks all `>=3`-node
clustering, which is the prerequisite for haematite failover (a 2-node cluster
cannot survive a single failure: `quorum_size(2) = 2`).

This document diagnoses the deadlock precisely, surveys the distributed-Erlang
prior art that solves exactly this problem, proposes a connection-establishment
protocol (state machine + simultaneous-connect tie-break + deadlines +
reconnection semantics), surfaces the key decisions for Tom to weigh, and lays
out a spike-first decomposition.

---

## 1. Precise diagnosis

### 1.1 The seam and the call chain

A haematite node forms its cluster by dialing every other peer by name:

- `DistributionEndpoint::connect` — `haematite/crates/haematite/src/sync/endpoint.rs:409`
  ```rust
  pub fn connect(&self, peer_name: &str) -> Result<(), SyncError> {
      ensure_outside_runtime()?;
      let manager = self.manager.clone();
      let peer_name = peer_name.to_owned();
      self.runtime()?
          .block_on(async move { manager.connect(&peer_name).await })   // :414
          .map(drop)
          .map_err(|_error| SyncError::TransportConnectFailed)
  }
  ```
  `connect` **blocks the calling (synchronous shard/db) thread** on
  `Runtime::block_on` for the *entire* handshake. There is no per-call timeout
  around `block_on`; the only deadline anywhere on this path is the TCP-connect
  timeout inside beamr (below). The retry that exists on the haematite/aion side
  sits *above* this call — it can only retry once `connect` **returns**. If
  `connect` never returns, the retry never runs.

- beamr `ConnectionManager::connect` — `beamr/crates/beamr/src/distribution/connection.rs:518`
  Resolves the name, opens a TCP stream with a 5 s timeout
  (`DEFAULT_CONNECT_TIMEOUT`, `connection.rs:24`; applied at `connection.rs:525`),
  then runs the **untimed** handshake:
  ```rust
  let result = initiate_handshake_async(&mut stream, &local, &self.inner.cookie,
                                        self.inner.gen_challenge()).await   // :541
      .map_err(|error| ConnectError::Io(error.to_string()))?;
  let node = self.inner.atom_table.intern(result.remote_name());
  Ok(self.register_connection(node, peer_addr, stream))                    // :553
  ```

- beamr `initiate_handshake_async` — `beamr/crates/beamr/src/distribution/handshake.rs:351`
  Multi-roundtrip exchange `N → s → N → r → a`, every read via
  `read_packet_async` (`handshake.rs:432`) which is a bare
  `reader.read_exact(...).await` **with no deadline** (`handshake.rs:436`,
  `:445`). The responder twin `respond_handshake_async` (`handshake.rs:388`) is
  spawned by the accept loop (`handle_accepted`, `connection.rs:658-682`) and is
  likewise untimed.

### 1.2 Why 2 nodes "sometimes win" and 3 reliably deadlock

The handshake itself is *role-asymmetric* (initiator vs responder send different
packets), so two **independent** TCP connections `A→B` and `B→A` can each
complete in isolation. The deadlock is a property of **how the dials are driven**
plus **the absence of any simultaneous-connect arbitration**:

1. **Per-thread blocking dials over a full mesh.** `DistributionEndpoint::connect`
   blocks a synchronous thread for the whole handshake (`endpoint.rs:414`). The
   cluster topology is `FullMesh`
   (`haematite/crates/haematite/src/sync/topology.rs:100`,
   `full_mesh_pairs:263`), which generates all `N*(N-1)/2` **undirected** pairs —
   each pair can be dialed from *either* endpoint. A node bringing up its mesh
   issues these dials from a bounded set of threads. While thread *T* is parked in
   `block_on` waiting on an untimed `read_exact`, it makes no progress on anything
   else.

2. **No simultaneous-connect arbitration.** When `A` dials `B` at the same instant
   `B` dials `A`, **two** TCP connections come up. Each side's accept loop spawns
   a responder (`connection.rs:660`) while its own dialing thread is mid-initiator.
   Nothing decides that one of these two connections should be abandoned. Both
   handshakes proceed.

3. **Last-writer-wins clobber with an orphaned reader.** Both handshakes
   eventually call `register_connection` (`connection.rs:556`), which does an
   **unconditional** `connections.insert(node, ...)` (`connection.rs:569`). The
   second insert silently replaces the first `Arc<DistConnection>` in the
   `DashMap<Atom, Arc<DistConnection>>` (`connection.rs:248`). The first
   connection's read task keeps running, orphaned, on a socket whose write half is
   no longer reachable. Each side may keep a *different* one of the two sockets,
   so subsequent writes and reads can land on mismatched half-links.

4. **The untimed read turns any stall into a permanent hang.** Because every
   handshake read is `read_exact` with no deadline (`handshake.rs:436`,
   `:445`), the moment the byte-stream ordering desynchronizes — e.g. a node is
   busy inside a *blocked* `block_on` for a different peer and its accept-side
   responder task is starved, or both sides are each waiting to read the other's
   next packet — the read **never returns**. With 2 peers the race window is small
   and one ordering usually wins; with 3 peers the all-pairs simultaneous-dial
   fan-out makes the bad interleaving the common case, and at least one
   `block_on`'d thread parks forever. That parked thread is a synchronous
   haematite shard/db thread, so the node wedges.

### 1.3 Root-cause summary (what is actually missing)

| Missing mechanism | Evidence |
|---|---|
| Read/write **deadlines** on handshake steps | `read_packet_async` bare `read_exact`, `handshake.rs:436`, `:445`; no `tokio::time::timeout` anywhere in `handshake.rs` |
| **Simultaneous-connect tie-break** | responder always writes `"ok"` (`handshake.rs:403`); `is_success_status` accepts `ok_simultaneous` (`handshake.rs:746`) but **nothing ever sends it** |
| **Connection dedup / "already up" check** | `register_connection` blind `insert` (`connection.rs:569`); `connect_node` checks existence only *once* before dialing (`connection.rs:455-463`) — racy |
| **Connection state machine** | no `Connecting/Pending/Up/Down` enum; only a post-hoc `AtomicBool` `down` flag (`connection.rs:155`, `ConnectionDownReason` at `connection.rs:53`) |
| **Bounded "connect returns or fails"** contract | `connect` can hang because the handshake can hang; haematite's retry above the seam cannot fire |

Everything above is **`net`-only** code. `pub mod distribution` is gated
`#[cfg(feature = "net")]` (`beamr/crates/beamr/src/lib.rs:14`), and `net` pulls
`dep:tokio` (`Cargo.toml:61`); the `cooperative`/wasm feature deliberately
excludes tokio (`Cargo.toml:60-61`). The fix stays entirely inside this gate and
does not touch the wasm cfg.

---

## 2. Prior art — the distributed-Erlang handshake resolution

OTP solves this exact problem in the distribution setup handshake
(`erl_dist_protocol`), in the **status** step that precedes the cryptographic
challenge. After the initiator `A` sends `send_name` (the `N` packet), responder
`B` replies with a status (the `s` packet) **before** any challenge is exchanged:

| Status | Meaning |
|---|---|
| `ok` | Continue. `B` had no competing connection to `A`. |
| `ok_simultaneous` | Continue **this** (inbound to `B`) connection; `B` also has an outgoing handshake to `A` in flight, and `B` will **abort its own outgoing** one. Sent when **A's name > B's name, compared literally**. |
| `nok` | Do **not** continue. `B` already has its *own* initiated handshake to `A` and will keep that one; `A` must abort this connection. Sent when **B's name > A's name**. |
| `alive` | `B` already has an *active* connection under this name (stale link not yet reaped, or `A` is confused). `A` must reply with a further status: `true` (kill the old, continue) or `false` (abort). |
| `not_allowed` | Refused for a security/policy reason. |
| `named:` | Dynamic-node-name assignment (not relevant here). |

**The tie-break rule (the load-bearing part).** When both nodes dial each other,
each one is simultaneously a responder for the peer's inbound and an initiator for
its own outbound. The responder side resolves the race by **literal node-name
comparison**:

- If the **incoming** initiator's name is **greater** than the local name →
  respond `ok_simultaneous`, and **drop the local outgoing** attempt. The inbound
  connection survives.
- If the local name is **greater** → respond `nok`. The inbound connection is
  rejected; the local **outgoing** survives and the peer aborts its inbound view.

Net effect: **exactly one connection survives**, and which one is chosen is a pure
function of the two globally-unique node names — no clocks, no coordination, no
extra round-trips. The lower-named node's *outgoing* connection is the survivor.
Challenge/auth only runs *after* status is `ok`/`ok_simultaneous`, so disputes are
settled before any cryptographic work.

beamr is already wire-compatible-shaped for this: it sends `N` then reads `s`
(`handshake.rs:357-362`), the responder writes a status packet
(`encode_status`, `handshake.rs:403`/`:523`), and `is_success_status` already
tolerates `ok_simultaneous` on the initiator side (`handshake.rs:746`). What is
missing is (a) the responder ever *choosing* a non-`ok` status, and (b) the
connection manager tracking an in-flight outbound so the responder can detect the
simultaneous case at all.

---

## 3. Proposed protocol

### 3.1 Connection state machine

Introduce an explicit per-peer state, owned by the connection manager and keyed by
**peer node name** (the globally-unique `Atom`/string already used as the table
key, `connection.rs:248`). This replaces today's "either present in the `DashMap`
or not" with a real lifecycle:

```
                 dial requested
   (none) ───────────────────────────▶ Connecting
      ▲                                     │  TCP up, send N, recv status
      │ Down (reap)                         ▼
      │                                  Pending ──── status=nok / lose tie-break ──▶ (abort, none)
      │                                     │  status ok / ok_simultaneous, auth ok
      │                                     ▼
      └──────── read/write error ──────  Up  ◀──── inbound handshake completes (dedup winner)
```

- **Connecting** — an outbound dial is in flight (TCP connect + initiator
  handshake). Recorded *before* the await so a concurrent inbound can detect it.
- **Pending** — handshake bytes are mid-exchange; auth not yet verified.
- **Up** — authenticated, link installed, read/data loop running.
- **Down** — terminal; reaped from the table. (Today's `ConnectionDownReason`,
  `connection.rs:53`, maps onto the Down transition.)

The manager holds a small `DashMap<Atom, PeerState>` (or folds the state into the
existing connections map via an enum value). The single invariant: **at most one
`Up` connection per peer name, and the manager arbitrates which in-flight attempt
becomes `Up`.**

### 3.2 Simultaneous-connect tie-break (the core fix)

Two changes, both responder-side, plus a manager-level dedup:

1. **Responder consults local outbound state.** In `respond_handshake_async`
   (`handshake.rs:388`), after reading the peer's `N` packet (which carries the
   peer's name), the responder asks the manager: *do I have a `Connecting`/`Pending`
   outbound to this same peer name?*

2. **Name-comparison decision** (adopt OTP's literal compare; see decision D1 for
   the direction):
   - No competing outbound → send `ok` (today's behavior).
   - Competing outbound **and** `peer_name > local_name` → send
     `ok_simultaneous`, mark the local outbound to **abort**, continue this
     inbound to `Up`.
   - Competing outbound **and** `local_name > peer_name` → send `nok`, drop this
     inbound; the local outbound proceeds to `Up`.

3. **Initiator honors `nok`.** `initiate_handshake_async` already treats non-success
   status as `BadStatus` and returns (`handshake.rs:359-362`). We make `nok`
   specifically a **clean, non-error abort** (the peer is keeping the reciprocal
   link) so the caller does not log it as a failure or retry-storm.

4. **Manager dedup on install.** `register_connection` (`connection.rs:556`) stops
   doing a blind `insert`. Instead it transitions the peer to `Up` only if no `Up`
   connection already exists; if one does (the tie-break elsewhere already
   installed the survivor), the loser's socket is closed and its read task is not
   spawned. This closes the residual race even if both sides momentarily think they
   won (e.g. equal-name pathological case — see D1).

**Wire/sequence delta.** No new packet types and no new fields: the change is
*which status byte the responder emits* and *when the initiator treats it as a
benign abort*. `ok_simultaneous` and `nok` are already valid OTP `s`-packet
payloads and already partially recognized (`handshake.rs:746`). This keeps the
beamr handshake OTP-wire-compatible.

`alive` (stale-link) handling is **optional for v1** (decision D4). Without it,
the reconnection path (3.4) simply reaps the stale `Up` link on its next failed
read/write and a fresh dial re-establishes it.

### 3.3 Deadlines on every step (no unbounded `read_exact`)

Wrap each handshake read and write in a per-step deadline so a stalled or
malicious peer can never park a thread forever:

- Add a `handshake_timeout: Duration` to `ConnectionManagerInner` (sibling of
  `connect_timeout`, `connection.rs:24`/`:251`), default ~**5 s** (D3), and pass it
  into the async handshake functions.
- In `read_packet_async`/`write_packet_async` (`handshake.rs:419-447`), bound each
  `read_exact`/`write_all` with `tokio::time::timeout(deadline, ...)`; on elapse
  return a new `HandshakeError::Timeout`. (Prefer a single deadline for the whole
  handshake via `tokio::time::timeout` around the top-level
  `initiate_handshake_async`/`respond_handshake_async` future — simpler and
  covers the whole exchange; per-step is an option if finer attribution is wanted.)
- The accept-side spawn (`handle_accepted`, `connection.rs:658`) and the outbound
  `connect` (`connection.rs:541`) both gain the timeout, so **`connect` is now
  guaranteed to return** (Ok / handshake error / timeout) and the haematite-side
  retry above `endpoint.rs:414` can finally make progress.

### 3.4 Reconnection semantics

beamr itself stays **mechanism, not policy** (consistent with ADR-007
"supervision is library"): it must (a) make `connect` *terminate*, and (b) make
re-handshake *idempotent*. Concretely:

- **Idempotent re-dial.** A dial to a peer already `Up` returns success without a
  new handshake (today's early-return at `connect_node`, `connection.rs:456`,
  generalized to the state machine and made race-safe against an in-flight
  `Connecting`).
- **Down reaping unchanged.** Read/write failure → `mark_down` → table removal +
  `connection_down_hook` (`connection.rs:204-211`, `:306-320`). The hook already
  drives haematite's `purge_remote_node` per the finish-spec.
- **Backoff lives above the seam.** The retry/backoff loop stays on the
  haematite/aion caller (where it already is, above `endpoint.rs:409`), because
  membership/seed policy is haematite's concern. beamr only guarantees the
  bounded-return contract that makes that loop correct. (If we later want
  beamr-internal reconnection, it slots in behind the state machine as a
  follow-up; explicitly out of scope here — see §6.)

---

## 4. Key decisions for Tom

**D1 — Tie-break direction: keep the *lower*-named node's outbound, or the
higher's?**
Recommendation: **adopt OTP's rule verbatim — lower-named node's *outbound*
survives** (responder sends `ok_simultaneous` when `peer_name > local_name`,
`nok` when `local_name > peer_name`). Rationale: it is battle-tested, it is a
pure function of the two globally-unique names (no clocks), and matching OTP keeps
us wire-compatible and reviewable against a known spec. The node name is already
a fully-orderable identity on the haematite side — `SyncNodeId` derives `Ord`
over its inner `String` (lexicographic), `topology.rs:20-25` — and beamr keys its
table by the authenticated handshake name (`connection.rs:248`), so the same
comparison is available on both sides with no new state. Equal-name case cannot
occur for distinct cluster members (names are unique); the manager-level dedup
(§3.2.4) is the backstop if it ever does. Decision needed only if Tom wants to
diverge from OTP — I recommend not.

**D2 — Keep `block_on` at the haematite seam, or make `connect` fully async?**
Recommendation: **keep `block_on` for now.** Once §3.3 lands, `connect` is
*bounded* — it always returns within `handshake_timeout` — which removes the
actual harm of the blocking call. haematite's synchronous shard/db threading model
(`ensure_outside_runtime`, `endpoint.rs:410`/`:930`) is built around blocking
seams (`send` does the same, `endpoint.rs:448-459`). Going fully async is a larger
cross-repo change for marginal benefit *after* the deadlock is gone. Revisit only
if dial fan-out latency becomes a bottleneck (a node dialing N peers serially on
one thread pays up to `N × handshake_timeout` worst case — mitigated by dialing
peers concurrently from the caller, or by a beamr-internal concurrent-dial helper,
both follow-ups).

**D3 — Timeout values.**
Recommendation: `handshake_timeout = 5 s` (mirrors the existing TCP
`DEFAULT_CONNECT_TIMEOUT`, `connection.rs:24`), configurable via
`with_connect_timeout`-style constructor plumbing. A whole-handshake deadline is
simpler and sufficient; per-step deadlines are an optional refinement. The number
matters less than its *existence* — anything finite fixes the deadlock; 5 s is a
safe default that tolerates a loaded peer without wedging a cluster.

**D4 — Implement OTP `alive` (stale-link) status in v1, or defer?**
Recommendation: **defer `alive` to a follow-up.** v1 needs `ok` / `ok_simultaneous`
/ `nok` (the simultaneous-connect resolution) plus deadlines — that is what unblocks
`>=3` clustering. Stale-link replacement is adequately handled in v1 by reap-on-
failure (§3.4) plus re-dial; `alive` is a latency optimization for the
"reconnect before the old link's death is noticed" window. Note it in the wire
notes so the status parser is forward-compatible.

The two biggest are **D1** (tie-break direction — recommend: OTP verbatim, lower
node's outbound wins) and **D3** (deadline existence/value — recommend: 5 s
whole-handshake), with **D2** (keep bounded `block_on`) close behind.

---

## 5. Spike-first decomposition (HS-0 … HS-5)

Each step is independently landable and verifiable; the deadlock is gone after
HS-3.

- **HS-0 — Reproduce the deadlock as a test (spike).** A 3-process (or 3-runtime,
  loopback) all-pairs simultaneous-dial test that hangs today. This is the
  regression oracle for everything below. *Risk:* timing-dependent; mitigate by
  forcing the simultaneous window (barrier before each `connect`).

- **HS-1 — Handshake deadlines.** Add `HandshakeError::Timeout` and wrap the
  async handshake in `tokio::time::timeout` (whole-handshake, D3). Plumb
  `handshake_timeout` through `ConnectionManagerInner` and both call sites
  (`connection.rs:541`, `:665`). *Outcome:* `connect` is now guaranteed to return;
  HS-0 stops hanging (it may now *fail* fast — that's progress, not regression).
  *Risk:* low; pure additive guard.

- **HS-2 — Connection state + race-safe install.** Introduce the
  `Connecting/Pending/Up/Down` state keyed by peer name; make `register_connection`
  dedup against an existing `Up` (close the loser, don't spawn its reader).
  *Risk:* concurrency correctness around the `DashMap`; cover with a "two
  simultaneous installs, exactly one survives, no orphaned reader" test.

- **HS-3 — Simultaneous-connect tie-break.** Responder consults outbound state and
  emits `ok` / `ok_simultaneous` / `nok` by name comparison (D1); initiator treats
  `nok` as a benign abort. *Outcome:* HS-0 now *passes* — exactly one link per
  pair, no deadlock, at `>=3` nodes. *Risk:* the load-bearing step; verify the
  name-comparison direction matches OTP and that the surviving link is usable
  bidirectionally.

- **HS-4 — Reconnection hardening.** Idempotent re-dial against `Connecting`/`Up`;
  confirm reap-on-failure + `connection_down_hook` still fire exactly once
  (existing tests at `connection.rs:996-1109` must stay green). *Risk:* low;
  mostly making the existing idempotent check race-safe.

- **HS-5 — Real 3-node cross-process integration test.** Drive it through the
  haematite `DistributionEndpoint` seam (`endpoint.rs:409`) to prove the fix at
  the actual seam that deadlocked, and that a 3-node mesh forms so failover quorum
  (`quorum_size(3) = 2`, `consistency.rs:204`) is reachable. *Risk:* cross-repo;
  may live as a haematite-side test consuming the bumped beamr. Optionally pair
  with the live cross-node failover demo Tom wants.

(Optional follow-ups, not part of this line: OTP `alive` status (D4); beamr-
internal concurrent dial / reconnection policy; fully-async `connect` (D2).)

---

## 6. What this deliberately does NOT change

- **No new wire packets or fields.** Only *which* status byte the responder picks
  and how the initiator reacts; stays OTP-`s`-packet compatible.
- **No change to the cryptographic challenge/auth** (`challenge_digest`,
  `constant_time_eq`, the `N→s→N→r→a` ordering) — only the status that gates it.
- **No change to the wasm/cooperative split.** All edits are inside
  `#[cfg(feature = "net")]` (`lib.rs:14`); the cooperative feature (no tokio,
  `Cargo.toml:60`) is untouched.
- **No reconnection *policy* in beamr.** Backoff/seed/membership stays on the
  haematite/aion caller (above `endpoint.rs:409`); beamr only guarantees the
  bounded-return + idempotent-re-handshake mechanism (ADR-007: supervision is
  library).
- **No removal of `block_on` at the haematite seam** (D2) — bounded, not removed.
- **No OTP `alive` / dynamic-node-name (`named:`) support** in v1 (D4) — parser
  left forward-compatible only.
- **No change to the data-frame phase** (8-byte `[control_len|payload_len]`
  framing, `spawn_read_lifecycle`, `connection.rs:587`) — only the pre-data
  handshake phase.
