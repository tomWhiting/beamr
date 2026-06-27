//! Ergonomic `Actor` (gen_server-style) layer over [`NativeHandler`].
//!
//! NATIVE-001's raw [`NativeHandler`] runs a Rust struct as a first-class,
//! scheduler-supervised process, but the author must hand-drain the mailbox,
//! match request/reply refs by hand, and reach for scheduler types. This module
//! layers a typed facade over it so downstream crates write actors, not
//! scheduler internals: the [`Actor`] trait ([`Actor::handle_call`] for
//! request/reply, [`Actor::handle_cast`] for fire-and-forget); [`spawn_actor`],
//! which spawns an actor as a restart-capable native process (NATIVE-002
//! factory spawn) and returns an [`ActorRef`] carrying the `u64` pid plus a
//! typed, `Clone`-able [`SenderHandle`]; and [`SenderHandle::call`] /
//! [`SenderHandle::cast`], the request/reply (correlated by a unique ref) and
//! fire-and-forget helpers.
//!
//! # The call deadlock hazard (read before using `call`)
//!
//! [`SenderHandle::call`] BLOCKS the calling thread until the correlated reply
//! arrives. That is correct for an **external driver** — Rust code that owns the
//! [`Scheduler`] and drives actors from *outside* a slice. It would **deadlock**
//! if invoked from *inside* a handler: the handler holds a scheduler worker
//! thread, and the target it waits on cannot run until the handler returns and
//! frees that thread, so the reply never arrives. The API shape reflects this —
//! a handler is given an [`ActorContext`] that offers only non-blocking
//! [`ActorContext::cast`] (and child spawning); there is NO blocking `call`
//! reachable from inside a handler. **Intra-actor request/reply uses `cast` + an
//! explicit reply message** (the gen_server pattern): the requester casts a
//! message carrying its own pid as `reply_to`, returns from its slice, and later
//! receives the answer as a separate cast.
//!
//! # What crosses the actor boundary
//!
//! Messages are immediates/refs/scalars — small integers, atoms, and tuples of
//! them (see [`ActorMessage`]) — never a raw free-variable closure to a possibly
//! Executing actor (the pre-existing ETF closure-encoding limitation on
//! [`crate::native::native_process`]). All sends route through
//! [`NativeContext::send`] / the existing `LocalSendFacility`, so NATIVE-001's
//! sender-clock and replay discipline holds verbatim.
//!
//! **Carry a pid as an integer scalar, never a pid term.** Delivery to an
//! Executing receiver goes through ETF, where a local pid term decodes back as
//! an *external* pid — so a pid term survives the in-hand copy path but is
//! silently corrupted on the Executing path (an intermittent loss). Encode a pid
//! as `pid as i64` via [`Term::try_small_int`] and read it back with
//! [`Term::as_small_int`], as the facade's own envelopes do. (References
//! round-trip faithfully and may be sent as terms.)

use std::marker::PhantomData;
use std::sync::Arc;
#[cfg(feature = "threads")]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(feature = "threads")]
use std::time::Duration;

use crate::native::native_process::{NativeContext, NativeHandler, NativeOutcome};
// The external-driver request/reply path (`SenderHandle`/`spawn_actor`) owns the
// threaded `Scheduler` and blocks on a crossbeam channel; neither exists on
// wasm32. Under `cooperative` only the platform-neutral actor traits compile;
// the cooperative spawn/call surface arrives in WR-2/WR-6.
#[cfg(feature = "threads")]
use crate::scheduler::Scheduler;
use crate::term::Term;
use crate::term::boxed::Tuple;

/// Envelope discriminant for a fire-and-forget cast: `{0, request}`.
const TAG_CAST: i64 = 0;
/// Envelope discriminant for a request/reply call:
/// `{1, ref, reply_to, request}`.
const TAG_CALL: i64 = 1;
/// Envelope discriminant for a correlated reply: `{2, ref, reply}`.
const TAG_REPLY: i64 = 2;

/// Default time a [`SenderHandle::call`] waits for the correlated reply before
/// giving up with [`ActorError::Timeout`].
#[cfg(feature = "threads")]
const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(5);

/// Monotonic source of unique call refs. A ref correlates a reply with the
/// in-flight call that produced it, so concurrent calls never cross replies.
#[cfg(feature = "threads")]
static NEXT_REF: AtomicU64 = AtomicU64::new(1);

#[cfg(feature = "threads")]
fn next_ref() -> u64 {
    NEXT_REF.fetch_add(1, Ordering::Relaxed)
}

/// A value that crosses the actor boundary as a beamr [`Term`].
///
/// Implementations encode/decode immediates/refs/scalars and tuples of them
/// (built with [`NativeContext::alloc_tuple`]), never a raw free-variable
/// closure (module docs). The `Clone + Send + Sync + 'static` bound lets a
/// request be captured by the restart-capable spawn factory and cross threads.
pub trait ActorMessage: Clone + Send + Sync + 'static {
    /// Encode `self` as a term on the running process's heap, or `None` when
    /// the heap is full.
    fn encode(&self, ctx: &mut NativeContext<'_>) -> Option<Term>;

    /// Decode a term received from the mailbox back into `Self`, or `None` when
    /// the term does not match this message shape.
    fn decode(term: Term) -> Option<Self>;
}

/// A scalar message that is itself a small integer.
impl ActorMessage for i64 {
    fn encode(&self, _ctx: &mut NativeContext<'_>) -> Option<Term> {
        Term::try_small_int(*self)
    }

    fn decode(term: Term) -> Option<Self> {
        term.as_small_int()
    }
}

/// A gen_server-style actor running over a [`NativeHandler`].
///
/// An actor owns private state and reacts to a [`Actor::Call`] (request/reply,
/// answered with a [`Actor::Reply`]) or a [`Actor::Cast`] (fire-and-forget). The
/// adapter drains the mailbox, decodes each call/cast envelope, dispatches to
/// the methods below, routes replies back by ref, and parks when the mailbox
/// drains — the author writes none of that.
pub trait Actor: Send + 'static {
    /// Request type delivered to [`Actor::handle_call`].
    type Call: ActorMessage;
    /// Reply type returned from [`Actor::handle_call`].
    type Reply: ActorMessage;
    /// Message type delivered to [`Actor::handle_cast`].
    type Cast: ActorMessage;

    /// Handle a request and return the reply (the adapter routes it back to the
    /// caller by ref).
    ///
    /// Do NOT attempt a blocking [`SenderHandle::call`] from here — it deadlocks
    /// the worker thread (module docs). To ask another actor, [`ActorContext::cast`]
    /// it a message carrying `ctx.self_pid()` and handle the answer as a later cast.
    fn handle_call(&mut self, request: Self::Call, ctx: &mut ActorContext<'_, '_>) -> Self::Reply;

    /// Handle a fire-and-forget message. No reply is sent.
    fn handle_cast(&mut self, request: Self::Cast, ctx: &mut ActorContext<'_, '_>);
}

/// The capability surface an [`Actor`] handler is given for one message.
///
/// It exposes only non-blocking operations — [`ActorContext::cast`] and
/// [`ActorContext::spawn_child`] — so the call-deadlock hazard (module docs) is
/// unreachable from inside a handler.
pub struct ActorContext<'ctx, 'slice> {
    inner: &'ctx mut NativeContext<'slice>,
}

impl<'ctx, 'slice> ActorContext<'ctx, 'slice> {
    fn new(inner: &'ctx mut NativeContext<'slice>) -> Self {
        Self { inner }
    }

    /// PID of the running actor.
    #[must_use]
    pub fn self_pid(&self) -> u64 {
        self.inner.self_pid()
    }

    /// Send a fire-and-forget [`ActorMessage`] to `target_pid`, routed through
    /// [`NativeContext::send`] (the existing `LocalSendFacility`).
    ///
    /// A cast to a dead/absent pid is silently dropped (BEAM semantics) and the
    /// caller does not block; the message reaches the target's
    /// [`Actor::handle_cast`] on its next slice. This is the intra-actor
    /// request/reply primitive — put `self_pid()` in the message to get an
    /// answer back as a later cast.
    pub fn cast<M: ActorMessage>(&mut self, target_pid: u64, message: &M) {
        if let Some(payload) = message.encode(self.inner)
            && let Some(envelope) = self
                .inner
                .alloc_tuple(&[Term::small_int(TAG_CAST), payload])
        {
            self.inner.send(target_pid, envelope);
        }
    }

    /// Spawn a child [`Actor`] linked to this actor and return its pid, or
    /// `None` if the spawn was refused.
    ///
    /// The child is built through the NATIVE-002 factory path, so a supervisor
    /// can restart it by re-invoking `factory`; because it is linked, the
    /// child's exit propagates to this actor (or is trapped at the
    /// [`NativeHandler`] layer).
    pub fn spawn_child<C, F>(&mut self, factory: F) -> Option<u64>
    where
        C: Actor,
        F: Fn() -> C + Send + Sync + 'static,
    {
        let self_pid = self.inner.self_pid();
        self.inner
            .spawn_native(actor_factory(factory), Some(self_pid))
            .ok()
    }
}

/// The [`NativeHandler`] adapter that drives any [`Actor`].
///
/// One message per slice: decode the next envelope, dispatch to
/// [`Actor::handle_call`] / [`Actor::handle_cast`], then return
/// [`NativeOutcome::Continue`] while the mailbox holds more or
/// [`NativeOutcome::Wait`] once it drains (parking through the existing 3-phase
/// park-gap). It reimplements no scheduling, parking, or send routing — it
/// delegates entirely to [`NativeContext`].
struct ActorHandler<A: Actor> {
    actor: A,
}

impl<A: Actor> NativeHandler for ActorHandler<A> {
    fn handle(&mut self, ctx: &mut NativeContext<'_>) -> NativeOutcome {
        let Some(message) = ctx.recv() else {
            return NativeOutcome::Wait;
        };
        match Incoming::decode(message) {
            Some(Incoming::Call {
                reference,
                reply_to,
                request,
            }) => {
                if let Some(request) = A::Call::decode(request) {
                    let reply = {
                        let mut actor_ctx = ActorContext::new(ctx);
                        self.actor.handle_call(request, &mut actor_ctx)
                    };
                    if let Some(reply_term) = reply.encode(ctx)
                        && let Some(envelope) = ctx.alloc_tuple(&[
                            Term::small_int(TAG_REPLY),
                            Term::small_int(reference),
                            reply_term,
                        ])
                    {
                        ctx.send(reply_to, envelope);
                    }
                }
            }
            Some(Incoming::Cast { request }) => {
                if let Some(request) = A::Cast::decode(request) {
                    let mut actor_ctx = ActorContext::new(ctx);
                    self.actor.handle_cast(request, &mut actor_ctx);
                }
            }
            None => {
                // Not an actor envelope (or a malformed one): ignore it, exactly
                // as a gen_server drops an unrecognised message.
            }
        }
        if ctx.has_messages() {
            NativeOutcome::Continue
        } else {
            NativeOutcome::Wait
        }
    }
}

/// A decoded inbound envelope on the server (actor) side.
enum Incoming {
    Call {
        reference: i64,
        reply_to: u64,
        request: Term,
    },
    Cast {
        request: Term,
    },
}

impl Incoming {
    fn decode(term: Term) -> Option<Self> {
        let tuple = Tuple::new(term)?;
        match tuple.get(0)?.as_small_int()? {
            TAG_CAST => Some(Self::Cast {
                request: tuple.get(1)?,
            }),
            TAG_CALL => Some(Self::Call {
                reference: tuple.get(1)?.as_small_int()?,
                // `reply_to` is an integer scalar, not a pid term, so it survives
                // the Executing/ETF delivery path (module docs).
                reply_to: u64::try_from(tuple.get(2)?.as_small_int()?).ok()?,
                request: tuple.get(3)?,
            }),
            _ => None,
        }
    }
}

/// Decode a `{2, ref, reply}` reply envelope into its ref and reply payload.
fn decode_reply(term: Term) -> Option<(i64, Term)> {
    let tuple = Tuple::new(term)?;
    if tuple.get(0)?.as_small_int()? != TAG_REPLY {
        return None;
    }
    Some((tuple.get(1)?.as_small_int()?, tuple.get(2)?))
}

/// Wrap an actor `factory` into a [`NativeHandler`] factory the scheduler can
/// use for both the initial spawn and any NATIVE-002 restart.
fn actor_factory<A, F>(factory: F) -> crate::native::native_process::NativeHandlerFactory
where
    A: Actor,
    F: Fn() -> A + Send + Sync + 'static,
{
    Box::new(move || Box::new(ActorHandler { actor: factory() }))
}

/// Errors returned by the actor facade.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ActorError {
    /// The scheduler refused to spawn the process.
    Spawn,
    /// A [`SenderHandle::call`] did not receive its correlated reply in time.
    Timeout,
}

impl std::fmt::Display for ActorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn => f.write_str("actor spawn refused by scheduler"),
            Self::Timeout => f.write_str("actor call timed out awaiting reply"),
        }
    }
}

impl std::error::Error for ActorError {}

/// A spawned actor: its `u64` pid plus a typed [`SenderHandle`].
#[cfg(feature = "threads")]
pub struct ActorRef<A: Actor> {
    /// The actor's process id.
    pub pid: u64,
    /// A `Clone`-able handle for sending typed calls and casts to the actor.
    pub sender: SenderHandle<A>,
}

#[cfg(feature = "threads")]
impl<A: Actor> Clone for ActorRef<A> {
    fn clone(&self) -> Self {
        Self {
            pid: self.pid,
            sender: self.sender.clone(),
        }
    }
}

/// A typed, `Clone`-able handle bound to one actor pid.
///
/// Use it from an **external driver** (Rust that owns the [`Scheduler`] and is
/// not inside a slice) to [`SenderHandle::call`] the actor (blocking
/// request/reply) or [`SenderHandle::cast`] it (fire-and-forget). Never `call`
/// from inside a handler — it deadlocks the worker thread (module docs).
#[cfg(feature = "threads")]
pub struct SenderHandle<A: Actor> {
    scheduler: Arc<Scheduler>,
    pid: u64,
    _marker: PhantomData<fn() -> A>,
}

#[cfg(feature = "threads")]
impl<A: Actor> Clone for SenderHandle<A> {
    fn clone(&self) -> Self {
        Self {
            scheduler: Arc::clone(&self.scheduler),
            pid: self.pid,
            _marker: PhantomData,
        }
    }
}

#[cfg(feature = "threads")]
impl<A: Actor> SenderHandle<A> {
    /// Build a handle for an existing actor `pid` (e.g. a child from
    /// [`ActorContext::spawn_child`]).
    ///
    /// Like a BEAM pid the handle is untyped on the wire: the caller asserts the
    /// process is an [`Actor`] of type `A`. An envelope the target cannot decode
    /// is ignored, so a type mismatch fails closed, never unsoundly.
    #[must_use]
    pub fn attach(scheduler: &Arc<Scheduler>, pid: u64) -> Self {
        Self {
            scheduler: Arc::clone(scheduler),
            pid,
            _marker: PhantomData,
        }
    }

    /// The target actor's pid.
    #[must_use]
    pub fn pid(&self) -> u64 {
        self.pid
    }

    /// Send a fire-and-forget cast to the actor and return immediately.
    ///
    /// The send is performed by a transient native process so it routes through
    /// [`NativeContext::send`] / the `LocalSendFacility` with full sender-clock
    /// discipline — there is no side channel. A cast to a dead pid is silently
    /// dropped. Returns [`ActorError::Spawn`] only if the scheduler refused to
    /// create that transient sender.
    pub fn cast(&self, message: A::Cast) -> Result<(), ActorError> {
        let target = self.pid;
        self.scheduler
            .spawn_native(Box::new(move || {
                clients::cast_handler::<A>(target, message.clone())
            }))
            .map(|_pid| ())
            .map_err(|_| ActorError::Spawn)
    }

    /// Blocking request/reply: send `request` to the actor and return its reply,
    /// correlated by a unique ref so concurrent calls never cross replies.
    ///
    /// MUST be called from an external driver, never from inside an actor
    /// handler — see the module-level deadlock note. Waits a default 5s for the
    /// reply; use [`SenderHandle::call_timeout`] to override.
    pub fn call(&self, request: A::Call) -> Result<A::Reply, ActorError> {
        self.call_timeout(request, DEFAULT_CALL_TIMEOUT)
    }

    /// [`SenderHandle::call`] with an explicit reply timeout.
    pub fn call_timeout(
        &self,
        request: A::Call,
        timeout: Duration,
    ) -> Result<A::Reply, ActorError> {
        let (reply_tx, reply_rx) = crossbeam_channel::bounded::<A::Reply>(1);
        let target = self.pid;
        let reference = next_ref();
        self.scheduler
            .spawn_native(Box::new(move || {
                clients::call_handler::<A>(target, request.clone(), reference, reply_tx.clone())
            }))
            .map_err(|_| ActorError::Spawn)?;
        reply_rx
            .recv_timeout(timeout)
            .map_err(|_| ActorError::Timeout)
    }
}

/// Spawn `factory`'s actor as a restart-capable native process and return its
/// [`ActorRef`].
///
/// The actor runs as a first-class, scheduler-supervised beamr process (real
/// pid, mailbox, links/monitors, supervision) via NATIVE-002's factory-based
/// native spawn, so a supervisor can restart it by re-invoking `factory`. The
/// returned [`ActorRef::sender`] is the typed handle an external driver uses to
/// [`SenderHandle::call`] / [`SenderHandle::cast`] the actor.
#[cfg(feature = "threads")]
pub fn spawn_actor<A, F>(scheduler: &Arc<Scheduler>, factory: F) -> Result<ActorRef<A>, ActorError>
where
    A: Actor,
    F: Fn() -> A + Send + Sync + 'static,
{
    let pid = scheduler
        .spawn_native(actor_factory(factory))
        .map_err(|_| ActorError::Spawn)?;
    Ok(ActorRef {
        pid,
        sender: SenderHandle {
            scheduler: Arc::clone(scheduler),
            pid,
            _marker: PhantomData,
        },
    })
}

#[cfg(feature = "threads")]
#[path = "actor_clients.rs"]
mod clients;

#[cfg(test)]
#[path = "actor_tests.rs"]
mod tests;
