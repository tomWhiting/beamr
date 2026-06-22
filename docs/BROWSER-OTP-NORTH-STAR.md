# beamr — Browser-OTP North Star ("Phoenix, compiled to the browser")

> **Status: north-star vision, 2026-06-22.** Companion to `AOT-NORTH-STAR.md`. This is a direction,
> not a build plan — it records *what* we'd build and *why it's now possible*, and is honest about the
> hard parts. Nothing here is committed work.

## The pitch (one line)

A Phoenix-shaped web framework for Gleam where the **OTP runtime itself runs in the browser** (beamr
compiled to wasm): supervised processes, message passing, and channels execute *client-side*, so the
reactive loop has **no server round-trip** — and the *same* Gleam code can run server-side on native
beamr, with processes message-passing transparently across the wasm↔native boundary. "OTP in the
browser. LiveView without the latency. Offline-first for free."

## Why this is suddenly real (the unlock)

A Phoenix-like framework **is** OTP: supervised processes + message passing + PubSub/channels. Until
now beamr couldn't be that substrate — cross-process `send` silently dropped, so GenServer/Supervisor
patterns had to be faked with synchronous structs (see CORE-007, liminal's channel actor). As of
**beamr 0.7.0** local cross-process send delivers and references round-trip (gen_server call tags,
monitor DOWNs) — the `LocalSendFacility`. Real supervised processes are now viable. That is the
foundation the whole idea stands on; without it there is no "Phoenix."

The second half of the foundation already exists: **beamr-wasm** (`crates/beamr-wasm`) compiles the VM
to wasm and already has the host seam — `wasm-bindgen` + `js-sys`, a BEAM-term ↔ `JsValue` converter
(`convert.rs`), and `Promise`/`Closure`/`JsFuture` interop (`lib.rs`). The VM-in-browser-calling-into-JS
primitive is present today; it has not yet been pointed at the DOM.

## What it is — and what it deliberately is NOT

**NOT** "reimplement LiveView in Gleam." The Gleam ecosystem already has **Lustre** (Elm-architecture +
server components) which is the natural reference/borrow for the view + vdom-diff layer. Cloning
LiveView's server-push model adds nothing.

**IS** the thing Elixir structurally *cannot* do today: **isomorphic OTP**. One Gleam codebase; the
framework decides where each process lives:
- **Browser (wasm beamr):** the per-view GenServer, its supervisor, and the reactive render/diff loop
  run client-side. State transitions and DOM patches happen locally at zero latency — instant, and
  it keeps working offline.
- **Server (native beamr):** the same process types run for multi-user, durability, and shared state.
- **Across the boundary:** a process in the browser and a process on the server message-pass as if
  local, over a websocket-backed *distribution* transport. LiveView's mandatory round-trip becomes
  *optional* — you sync to the backend (over liminal) only when the domain needs it.

This is the inversion: LiveView holds the view process on the server and streams diffs down; here the
view process can live *in the browser*, and the server is just another node it talks to when it wants.

## Component map (compose the stack, don't rebuild it)

| Layer | Phoenix analogue | In our stack |
|---|---|---|
| Process/OTP (GenServer, Supervisor, Registry) | OTP | **beamr 0.7.0** — now real (LocalSendFacility) |
| Browser runtime | BEAM | **beamr-wasm** (+ a DOM/host BIF layer to add) |
| PubSub / Channels | `Phoenix.PubSub`, Channels | **liminal** ("conversation-based messaging bus") |
| View + vdom diff | LiveView / HEEx | **Lustre** (borrow/integrate, don't reinvent) |
| Router | Phoenix.Router | liminal routing + a Gleam router |
| Durability / replay | Ecto + (LiveView reconnect) | **aion** (event-sourced, deterministic replay) |
| Shared state / sync | Postgres / CRDTs | **haematite** (content-addressed, fork/merge/sync) |
| Cross-node transport | Distributed Erlang | beamr **distribution** over websocket (next milestone) |

The "framework" is mostly *glue + ergonomics* over pieces that already exist or are being built — which
is the point.

## The honest hard parts

1. **DOM diffing through a VM.** Lustre/Elm diff in native JS; running the diff loop inside a BEAM VM in
   wasm adds a layer. Fine for the vast majority of apps; an open question for 60fps-heavy UIs. Measure
   before believing.
2. **Bundle size.** A VM + bytecode in wasm is chunky. This is exactly where **`AOT-NORTH-STAR.md`**
   pays off: AOT-compile Gleam→wasm and drop the interpreter for the hot path → small bundles. The two
   north stars reinforce each other; browser-OTP is a prime *consumer* of the AOT work.
3. **Cross-boundary distribution.** "Processes message-pass across wasm↔native" needs the *distribution*
   send working over a websocket transport — the next messaging milestone after the local send just
   landed. Until then, browser and server talk via an explicit liminal channel rather than transparent
   `!`.
4. **DOM/host BIFs.** beamr-wasm has the JS bridge but not DOM bindings; we'd add a host-NIF surface
   (the `WasmAsyncNifFacility` seam in `NativeServices` is the hook) for DOM/fetch/storage/websocket.
5. **Scheduler in wasm.** wasm is single-threaded (without threads proposal); beamr's scheduler must run
   cooperatively on the browser event loop. Replay mode is already single-threaded, so the model exists,
   but the browser scheduler is its own piece of work.

## Why it's defensible (vs the field)

- **Elixir→wasm (Firefly/Lumen) stalled.** beamr is alive, actively built, and now has working actors.
- **Lustre** is excellent but is a view framework, not a full in-browser OTP runtime with isomorphic
  process placement + distribution. We'd build *with* it, above it.
- The unique compound: **durable (aion) + content-addressed-syncable (haematite) + supervised (OTP) +
  isomorphic (wasm/native) Gleam** — no one else has that stack to compose from.

## Smallest proof that de-risks everything (when we choose to)

Before any framework abstraction: a **counter / todo app where a GenServer runs in wasm and patches the
DOM** through the existing js-sys bridge — **no backend at all**. It exercises the real loop: a supervised
process holding state in the browser, receiving messages, rendering, diffing, applying to the DOM. If that
feels good, the framework is real and worth building; if the perf/DOM ergonomics disappoint, we learn it
cheaply before investing in the abstraction. Second milestone: the same app's process *also* talking to a
native-beamr process over a websocket distribution channel — proving the isomorphic story.

## Non-goals (for clarity)

- Not a LiveView clone; not server-push-only.
- Not "rewrite the frontend ecosystem" — interop with JS/DOM is a feature, not a defeat.
- Not committing to a timeline. This is the apex the other pieces (actors ✓, AOT, distribution) build
  toward; it gets built when the substrate is ready and a real use-case pulls it.

## Dependencies / sequence (what has to be true first)

1. ✅ Local cross-process send + ref round-trip (beamr 0.7.0).
2. Distribution send over a pluggable (websocket) transport.
3. beamr-wasm: cooperative browser scheduler + DOM/host BIF surface.
4. AOT path (per `AOT-NORTH-STAR.md`) for bundle size — parallel, not strictly blocking a first demo.
5. Lustre integration for the view/diff layer.
6. liminal as the channel/PubSub layer (server + in-browser).

See `AOT-NORTH-STAR.md` (the other half of the browser story) and `MESSAGING-FIX-SCOPE.md` /
`LOCAL-SEND-IMPL-BRIEF.md` (the actor foundation that made this reachable).
