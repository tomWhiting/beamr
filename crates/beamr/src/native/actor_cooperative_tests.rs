//! WR-6: native cooperative `call_async` end-to-end on the [`WasmScheduler`].
//!
//! These drive the SAME host pump a wasm host uses ([`WasmScheduler::run_until_idle`])
//! and poll the returned [`CallFuture`] with a no-op waker, proving the future
//! resolves with the actor's ref-correlated reply, that concurrent calls do not
//! cross replies, and that a missing reply rejects on the timeout — the latter
//! driven deterministically through the WR-4 `tick_native_timers_at` seam with a
//! large delay and multi-second margins (no tight wall-clock millisecond races).

use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
use std::time::{Duration, Instant};

use super::super::{Actor, ActorContext, ActorError};
use super::{CallFuture, CoopSenderHandle, spawn_actor_cooperative};
use crate::atom::AtomTable;
use crate::module::ModuleRegistry;
use crate::native::BifRegistryImpl;
use crate::scheduler::WasmScheduler;

/// An actor that, on each `Call(n)`, replies with `n + 1`. Carries no state
/// beyond the increment so concurrent calls are trivially distinguishable.
struct Adder;

impl Actor for Adder {
    type Call = i64;
    type Reply = i64;
    type Cast = i64;

    fn handle_call(&mut self, request: i64, _ctx: &mut ActorContext<'_, '_>) -> i64 {
        request + 1
    }

    fn handle_cast(&mut self, _request: i64, _ctx: &mut ActorContext<'_, '_>) {}
}

/// Build a cooperative scheduler wrapped the way a wasm host holds it.
fn scheduler() -> Rc<RefCell<WasmScheduler>> {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let modules = Arc::new(ModuleRegistry::new());
    let bifs = Arc::new(BifRegistryImpl::new());
    Rc::new(RefCell::new(WasmScheduler::new(atom_table, modules, bifs)))
}

/// A no-op waker: the host pump (not a waker thread) is what advances the
/// future, so polling needs only a valid `Context`.
fn noop_waker() -> Waker {
    fn clone(_: *const ()) -> RawWaker {
        RawWaker::new(std::ptr::null(), &VTABLE)
    }
    fn noop(_: *const ()) {}
    static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);
    // SAFETY: the vtable's functions are all no-ops over a null data pointer and
    // never dereference it, so the constructed waker is sound to use and drop.
    unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) }
}

/// Poll a `CallFuture` once with a no-op waker.
fn poll_once<R>(future: &mut Pin<Box<CallFuture<R>>>) -> Poll<Result<R, ActorError>> {
    let waker = noop_waker();
    let mut cx = Context::from_waker(&waker);
    future.as_mut().poll(&mut cx)
}

#[test]
fn call_async_resolves_with_the_actors_reply() {
    let scheduler = scheduler();
    let actor = spawn_actor_cooperative::<Adder, _>(&scheduler, || Adder);

    // Issue the call: this spawns the transient client and returns a pending
    // future. Nothing has run yet.
    let mut future = Box::pin(actor.sender.call_async(41));
    assert!(
        matches!(poll_once(&mut future), Poll::Pending),
        "future is pending before any turn runs"
    );

    // Pump the host turns: the client sends the request, the actor replies, the
    // client receives the ref-matched reply and resolves the slot.
    let mut resolved = None;
    for _ in 0..8 {
        scheduler.borrow_mut().run_until_idle();
        if let Poll::Ready(result) = poll_once(&mut future) {
            resolved = Some(result);
            break;
        }
    }

    assert_eq!(
        resolved,
        Some(Ok(42)),
        "the call_async future resolved with the actor's reply (41 + 1)"
    );
}

#[test]
fn concurrent_call_asyncs_never_cross_replies() {
    let scheduler = scheduler();
    let actor = spawn_actor_cooperative::<Adder, _>(&scheduler, || Adder);

    // Two in-flight calls with different requests; their replies must not cross.
    let mut first = Box::pin(actor.sender.call_async(10));
    let mut second = Box::pin(actor.sender.call_async(20));

    let mut first_result = None;
    let mut second_result = None;
    for _ in 0..16 {
        scheduler.borrow_mut().run_until_idle();
        if first_result.is_none()
            && let Poll::Ready(result) = poll_once(&mut first)
        {
            first_result = Some(result);
        }
        if second_result.is_none()
            && let Poll::Ready(result) = poll_once(&mut second)
        {
            second_result = Some(result);
        }
        if first_result.is_some() && second_result.is_some() {
            break;
        }
    }

    assert_eq!(first_result, Some(Ok(11)), "first call got its own reply");
    assert_eq!(second_result, Some(Ok(21)), "second call got its own reply");
}

#[test]
fn call_async_rejects_on_timeout_when_no_reply_arrives() {
    // A target pid that is not an actor (nothing decodes the call envelope, so no
    // reply is ever produced) forces the call to resolve only via its timeout.
    // The timeout is armed as a `Deliver` self-tick on the WR-4 native wheel and
    // fired deterministically through `tick_native_timers_at` with a large delay
    // and multi-second margins — never anchored to tight wall-clock timing.
    let scheduler = scheduler();

    // Spawn a do-nothing native root to occupy a real, live pid that will never
    // reply to a call (it parks forever), so the call can only time out.
    let target = scheduler
        .borrow_mut()
        .spawn_native_root(Box::new(|| Box::new(ParkForever)));

    let handle = CoopSenderHandle::<Adder>::attach(&scheduler, target);
    let delay = Duration::from_secs(10);
    let mut future = Box::pin(handle.call_async_timeout(7, delay));

    // Pump so the client sends the (unanswered) request and arms its timeout.
    scheduler.borrow_mut().run_until_idle();
    assert!(
        matches!(poll_once(&mut future), Poll::Pending),
        "future is pending while the request is outstanding and the timeout is armed"
    );

    let start = Instant::now();

    // Advance well short of the delay: the timeout must NOT fire.
    let woken_early = scheduler
        .borrow_mut()
        .tick_native_timers_at(start + Duration::from_secs(5));
    let _ = woken_early;
    scheduler.borrow_mut().run_until_idle();
    assert!(
        matches!(poll_once(&mut future), Poll::Pending),
        "future stays pending before the timeout deadline"
    );

    // Advance comfortably past the delay: the timeout self-tick fires, the client
    // resolves the slot with Timeout, and the next pump runs the client.
    let _woken = scheduler
        .borrow_mut()
        .tick_native_timers_at(start + delay + Duration::from_secs(5));

    let mut rejected = None;
    for _ in 0..8 {
        scheduler.borrow_mut().run_until_idle();
        if let Poll::Ready(result) = poll_once(&mut future) {
            rejected = Some(result);
            break;
        }
    }

    assert_eq!(
        rejected,
        Some(Err(ActorError::Timeout)),
        "the call_async future rejected with Timeout when no reply arrived"
    );
}

/// A native handler that parks forever — a live pid that never produces a reply.
struct ParkForever;

impl crate::native::native_process::NativeHandler for ParkForever {
    fn handle(
        &mut self,
        _ctx: &mut crate::native::native_process::NativeContext<'_>,
    ) -> crate::native::native_process::NativeOutcome {
        crate::native::native_process::NativeOutcome::Wait
    }
}
