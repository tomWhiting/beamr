//! Cooperative (single-threaded / `wasm32`) actor surface bound to the
//! [`WasmScheduler`] (WR-6).
//!
//! The threaded [`super::SenderHandle`] owns an `Arc<Scheduler>` and its
//! request/reply [`super::SenderHandle::call`] BLOCKS the calling thread on a
//! `crossbeam_channel` — illegal on the wasm main thread, which *is* the browser
//! event loop. This module provides the same actor surface for the cooperative
//! runtime:
//!
//! - [`spawn_actor_cooperative`] spawns an [`Actor`] as a first-class native
//!   process on the `WasmScheduler` and returns a [`CoopActorRef`].
//! - [`CoopSenderHandle::cast`] is fire-and-forget, routed through a transient
//!   native sender (no side channel) exactly like the threaded `cast`.
//! - [`CoopSenderHandle::call_async`] is the heart of WR-6: it returns a
//!   host-pumpable [`CallFuture`] instead of blocking. A transient
//!   request/reply client process sends `{TAG_CALL, ref, reply_to, request}`
//!   through [`NativeContext::send`] (full sender-clock discipline — no side
//!   channel), parks, and on a later turn either receives the ref-matched reply
//!   cast and writes it into a shared slot, or its timeout self-tick fires and
//!   writes [`ActorError::Timeout`]. The [`CallFuture`] polls that slot; the
//!   host driving `run_until_idle` turns is what advances it.
//!
//! Reply correlation reuses the threaded path's machinery verbatim — the shared
//! monotonic [`super::next_ref`] ref space, the `{1, ref, reply_to, request}` /
//! `{2, ref, reply}` envelopes, and [`super::decode_reply`] — so concurrent
//! `call_async`s never cross replies, and a cooperative call is wire-compatible
//! with the same [`Actor`] a threaded `call` talks to.

use std::cell::RefCell;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use super::{
    Actor, ActorError, ActorMessage, DEFAULT_CALL_TIMEOUT, TAG_CALL, TAG_CAST, actor_factory,
    decode_reply, next_ref,
};
use crate::native::native_process::{NativeContext, NativeHandler, NativeOutcome};
use crate::process::ExitReason;
use crate::scheduler::WasmScheduler;
use crate::term::Term;

/// Marker payload a call client schedules to itself as a `Deliver` timer to
/// represent its reply timeout. It is delivered as a bare small-int into the
/// client's own mailbox (it never collides with a `{2, ref, reply}` reply tuple,
/// which is boxed, not an immediate), so the client distinguishes "reply" from
/// "timeout" by term shape.
const TIMEOUT_MARKER: i64 = -1;

/// Resolution of one in-flight cooperative call, written once by the transient
/// client process and read by the [`CallFuture`].
enum CallSlot<R> {
    /// No resolution yet; the optional waker is woken when one is written.
    Pending(Option<Waker>),
    /// The actor replied with `R` (ref-correlated).
    Ready(R),
    /// The reply did not arrive before the timeout deadline.
    TimedOut,
}

/// Shared single-cell slot connecting a transient call client to its
/// [`CallFuture`].
///
/// `Arc<Mutex<…>>` because the client is built by a `Send + Sync`
/// [`crate::native::native_process::NativeHandlerFactory`] closure (Decision
/// D3); the lock is uncontended on the single host thread.
type SharedCallSlot<R> = Arc<Mutex<CallSlot<R>>>;

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A spawned cooperative actor: its `u64` pid plus a typed [`CoopSenderHandle`].
pub struct CoopActorRef<A: Actor> {
    /// The actor's process id.
    pub pid: u64,
    /// A `Clone`-able handle for sending typed calls and casts to the actor.
    pub sender: CoopSenderHandle<A>,
}

impl<A: Actor> Clone for CoopActorRef<A> {
    fn clone(&self) -> Self {
        Self {
            pid: self.pid,
            sender: self.sender.clone(),
        }
    }
}

/// A typed, `Clone`-able handle bound to one actor pid on a cooperative
/// [`WasmScheduler`].
///
/// Unlike the threaded [`super::SenderHandle`] it holds an
/// `Rc<RefCell<WasmScheduler>>` (the cooperative scheduler is single-threaded
/// and `!Send`), and its request/reply [`CoopSenderHandle::call_async`] returns
/// a host-pumpable [`CallFuture`] rather than blocking.
pub struct CoopSenderHandle<A: Actor> {
    scheduler: Rc<RefCell<WasmScheduler>>,
    pid: u64,
    _marker: PhantomData<fn() -> A>,
}

impl<A: Actor> Clone for CoopSenderHandle<A> {
    fn clone(&self) -> Self {
        Self {
            scheduler: Rc::clone(&self.scheduler),
            pid: self.pid,
            _marker: PhantomData,
        }
    }
}

impl<A: Actor> CoopSenderHandle<A> {
    /// Build a handle for an existing actor `pid` on a cooperative scheduler.
    ///
    /// Like a BEAM pid the handle is untyped on the wire: the caller asserts the
    /// process is an [`Actor`] of type `A`. An envelope the target cannot decode
    /// is ignored, so a type mismatch fails closed.
    #[must_use]
    pub fn attach(scheduler: &Rc<RefCell<WasmScheduler>>, pid: u64) -> Self {
        Self {
            scheduler: Rc::clone(scheduler),
            pid,
            _marker: PhantomData,
        }
    }

    /// The target actor's pid.
    #[must_use]
    pub const fn pid(&self) -> u64 {
        self.pid
    }

    /// Send a fire-and-forget cast to the actor and return immediately.
    ///
    /// The send is performed by a transient native process so it routes through
    /// [`NativeContext::send`] / the cooperative `LocalSendFacility` with full
    /// sender-clock discipline — there is no side channel. A cast to a dead pid
    /// is silently dropped. Non-blocking on the host thread.
    ///
    /// # Errors
    ///
    /// Never returns an error in the cooperative runtime (the transient sender
    /// always spawns); the `Result` mirrors the threaded [`super::SenderHandle::cast`]
    /// signature so call sites are source-compatible across both runtimes.
    pub fn cast(&self, message: A::Cast) -> Result<(), ActorError> {
        let target = self.pid;
        self.scheduler
            .borrow_mut()
            .spawn_native_root(Box::new(move || {
                Box::new(CoopCastClient::<A> {
                    target,
                    message: message.clone(),
                    sent: false,
                    _marker: PhantomData,
                })
            }));
        Ok(())
    }

    /// Non-blocking request/reply: send `request` to the actor and return a
    /// [`CallFuture`] that resolves with the actor's reply, correlated by a
    /// unique ref so concurrent calls never cross replies. Times out after the
    /// default 5s; use [`CoopSenderHandle::call_async_timeout`] to override.
    ///
    /// The future is advanced by the host pumping
    /// [`WasmScheduler::run_until_idle`] turns; it never blocks the event loop.
    pub fn call_async(&self, request: A::Call) -> CallFuture<A::Reply> {
        self.call_async_timeout(request, DEFAULT_CALL_TIMEOUT)
    }

    /// [`CoopSenderHandle::call_async`] with an explicit reply timeout.
    pub fn call_async_timeout(&self, request: A::Call, timeout: Duration) -> CallFuture<A::Reply> {
        let slot: SharedCallSlot<A::Reply> = Arc::new(Mutex::new(CallSlot::Pending(None)));
        let target = self.pid;
        let reference = next_ref();
        let client_slot = Arc::clone(&slot);
        self.scheduler
            .borrow_mut()
            .spawn_native_root(Box::new(move || {
                Box::new(CoopCallClient::<A> {
                    target,
                    request: request.clone(),
                    reference,
                    timeout,
                    slot: Arc::clone(&client_slot),
                    sent: false,
                })
            }));
        CallFuture { slot }
    }
}

/// Host-pumpable future resolving a cooperative `call_async`.
///
/// Polling reads the shared slot the transient client fills: `Pending` (storing
/// the waker) until the ref-matched reply arrives (`Ready(reply)`) or the
/// timeout self-tick fires (`Err(Timeout)`). The host driving `run_until_idle`
/// is what fills the slot, so a JS `Promise` over this future resolves as the
/// host pump advances — no event-loop blocking.
pub struct CallFuture<R> {
    slot: SharedCallSlot<R>,
}

impl<R> Future for CallFuture<R> {
    type Output = Result<R, ActorError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut guard = lock(&self.slot);
        match std::mem::replace(&mut *guard, CallSlot::Pending(None)) {
            CallSlot::Ready(reply) => Poll::Ready(Ok(reply)),
            CallSlot::TimedOut => Poll::Ready(Err(ActorError::Timeout)),
            CallSlot::Pending(_) => {
                *guard = CallSlot::Pending(Some(cx.waker().clone()));
                Poll::Pending
            }
        }
    }
}

/// Transient native process that performs one cooperative fire-and-forget cast,
/// then stops. Mirror of the threaded `actor_clients::CastClient`.
struct CoopCastClient<A: Actor> {
    target: u64,
    message: A::Cast,
    sent: bool,
    _marker: PhantomData<fn() -> A>,
}

impl<A: Actor> NativeHandler for CoopCastClient<A> {
    fn handle(&mut self, ctx: &mut NativeContext<'_>) -> NativeOutcome {
        if !self.sent {
            self.sent = true;
            if let Some(payload) = self.message.encode(ctx)
                && let Some(envelope) = ctx.alloc_tuple(&[Term::small_int(TAG_CAST), payload])
            {
                ctx.send(self.target, envelope);
            }
        }
        NativeOutcome::Stop(ExitReason::Normal)
    }
}

/// Transient native process for one cooperative request/reply: slice 1 sends the
/// `{TAG_CALL, ref, reply_to, request}` envelope and arms a timeout self-tick; a
/// later slice resolves the shared slot from either the ref-matched reply or the
/// timeout marker, then stops. The cooperative analogue of the threaded
/// `actor_clients::CallClient`, but writing a slot instead of a channel.
struct CoopCallClient<A: Actor> {
    target: u64,
    request: A::Call,
    reference: u64,
    timeout: Duration,
    slot: SharedCallSlot<A::Reply>,
    sent: bool,
}

impl<A: Actor> CoopCallClient<A> {
    /// Resolve the slot once (first writer wins) and wake any registered future
    /// waker. The client stops after its first resolve, so a second call cannot
    /// occur in practice; the guard keeps the path total regardless.
    fn resolve(&self, value: CallSlot<A::Reply>) {
        // Take the waker (if any) out under the lock, write the resolution, then
        // release the lock BEFORE waking so the wake never runs while held.
        let waker = {
            let mut guard = lock(&self.slot);
            // Only a still-pending slot is resolvable; an already-written
            // resolution is left untouched (first writer wins).
            let CallSlot::Pending(_) = &*guard else {
                return;
            };
            match std::mem::replace(&mut *guard, value) {
                CallSlot::Pending(waker) => waker,
                // Unreachable given the check above; keeps the path total.
                _ => None,
            }
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

impl<A: Actor> NativeHandler for CoopCallClient<A> {
    fn handle(&mut self, ctx: &mut NativeContext<'_>) -> NativeOutcome {
        if !self.sent {
            self.sent = true;
            // Build {1, ref, reply_to, request}. `reply_to` is our pid as an
            // integer scalar, not a pid term, so it survives the Executing/ETF
            // delivery path (see the actor module docs).
            let reference = Term::try_small_int(self.reference.cast_signed());
            let reply_to = i64::try_from(ctx.self_pid())
                .ok()
                .and_then(Term::try_small_int);
            if let (Some(reference), Some(reply_to), Some(request)) =
                (reference, reply_to, self.request.encode(ctx))
                && let Some(envelope) =
                    ctx.alloc_tuple(&[Term::small_int(TAG_CALL), reference, reply_to, request])
            {
                ctx.send(self.target, envelope);
            }
            // Arm the reply timeout on the WR-4 native timer wheel: a `Deliver`
            // self-tick of the timeout marker. If the wheel is absent the call
            // simply has no timeout (it resolves only on a real reply).
            let _timer = ctx.schedule(self.timeout, Term::small_int(TIMEOUT_MARKER));
            return NativeOutcome::Wait;
        }
        while let Some(message) = ctx.recv() {
            if let Some((reference, reply_term)) = decode_reply(message) {
                if reference == self.reference.cast_signed()
                    && let Some(reply) = A::Reply::decode(reply_term)
                {
                    self.resolve(CallSlot::Ready(reply));
                    return NativeOutcome::Stop(ExitReason::Normal);
                }
                // A reply for a different ref cannot reach this client (it is the
                // only correlator for its own ref); ignore defensively.
            } else if message.as_small_int() == Some(TIMEOUT_MARKER) {
                self.resolve(CallSlot::TimedOut);
                return NativeOutcome::Stop(ExitReason::Normal);
            }
            // Any other message is not for us; drop it (BEAM unknown-message).
        }
        NativeOutcome::Wait
    }
}

/// Spawn `factory`'s actor as a first-class native process on the cooperative
/// `scheduler` and return its [`CoopActorRef`].
///
/// The actor runs as a real beamr process (pid, mailbox, links/monitors,
/// supervision) via the cooperative native spawn path, restart-capable through
/// the retained `factory`. The returned [`CoopActorRef::sender`] is the typed
/// handle a host driver uses to [`CoopSenderHandle::call_async`] /
/// [`CoopSenderHandle::cast`] the actor.
pub fn spawn_actor_cooperative<A, F>(
    scheduler: &Rc<RefCell<WasmScheduler>>,
    factory: F,
) -> CoopActorRef<A>
where
    A: Actor,
    F: Fn() -> A + Send + Sync + 'static,
{
    let pid = scheduler
        .borrow_mut()
        .spawn_native_root(actor_factory(factory));
    CoopActorRef {
        pid,
        sender: CoopSenderHandle {
            scheduler: Rc::clone(scheduler),
            pid,
            _marker: PhantomData,
        },
    }
}

#[cfg(test)]
#[path = "actor_cooperative_tests.rs"]
mod tests;
