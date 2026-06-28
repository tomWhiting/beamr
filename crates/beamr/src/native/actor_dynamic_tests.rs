//! WR-8: native end-to-end of the dynamic term-carrying actor over the
//! cooperative `call_async`/`CallFuture` surface.
//!
//! These prove the seam LOGIC the `beamr-wasm` JS bindings expose, on the SAME
//! cooperative scheduler and host pump (`WasmScheduler::run_until_idle`) a wasm
//! host drives — but in a native `#[test]` (the wasm-bindgen JS layer cannot run
//! headless). A [`DynActor`] is spawned with a pure Rust reply transform standing
//! in for the host (JS) callback; a call is issued through
//! [`CoopSenderHandle::call_async`], the host turns are pumped, and the
//! `CallFuture` is polled with a no-op waker. We assert: (1) an opaque compound
//! term graph round-trips through the boundary and the transform's reply comes
//! back ref-correlated; (2) a call to a non-actor pid rejects with `Timeout`
//! (the Promise-rejection path), driven deterministically via the WR-4
//! `tick_native_timers_at` seam.

use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
use std::time::{Duration, Instant};

use super::super::{ActorError, CoopSenderHandle, spawn_actor_cooperative};
use super::{DynActor, ReplyFn, WireTerm};
use crate::atom::{Atom, AtomTable};
use crate::ets::OwnedTerm;
use crate::module::ModuleRegistry;
use crate::native::BifRegistryImpl;
use crate::native::ProcessContext;
use crate::scheduler::WasmScheduler;
use crate::term::Term;
use crate::term::boxed::Tuple;

/// Build a cooperative scheduler wrapped the way a wasm host holds it.
fn scheduler() -> Rc<RefCell<WasmScheduler>> {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let modules = Arc::new(ModuleRegistry::new());
    let bifs = Arc::new(BifRegistryImpl::new());
    Rc::new(RefCell::new(WasmScheduler::new(atom_table, modules, bifs)))
}

/// A no-op waker: the host pump (not a waker thread) advances the future.
fn noop_waker() -> Waker {
    fn clone(_: *const ()) -> RawWaker {
        RawWaker::new(std::ptr::null(), &VTABLE)
    }
    fn noop(_: *const ()) {}
    static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);
    // SAFETY: every vtable function is a no-op over a null data pointer and never
    // dereferences it, so the constructed waker is sound to use and drop.
    unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) }
}

/// Poll a `CallFuture` once with a no-op waker.
fn poll_once<R>(future: &mut Pin<Box<super::super::CallFuture<R>>>) -> Poll<Result<R, ActorError>> {
    let waker = noop_waker();
    let mut cx = Context::from_waker(&waker);
    future.as_mut().poll(&mut cx)
}

/// Build a self-contained owned `{n}` single-element tuple term graph, exactly as
/// a host would marshal a request value off-heap before handing it to the actor.
fn owned_tuple1(value: i64) -> OwnedTerm {
    let mut context = ProcessContext::new();
    let element = Term::try_small_int(value).expect("small int fits");
    let tuple = context
        .alloc_tuple(&[element])
        .expect("tuple allocation succeeds");
    context
        .take_detached_result(tuple)
        .expect("tuple is a detached heap allocation")
}

/// Read the single small-int element from an owned `{n}` tuple term graph.
fn tuple1_value(owned: &OwnedTerm) -> Option<i64> {
    let tuple = Tuple::new(owned.root())?;
    tuple.get(0)?.as_small_int()
}

/// Read the first small-int element from an owned `{a, ...}` tuple term graph.
fn tuple_first(owned: &OwnedTerm) -> Option<i64> {
    let tuple = Tuple::new(owned.root())?;
    tuple.get(0)?.as_small_int()
}

/// A reply transform standing in for a host callback: given request `{n}`, reply
/// with the compound graph `{n + 1, ok}` so the test proves a multi-object term
/// graph (tuple + atom) survives the boundary in both directions.
fn increment_reply() -> ReplyFn {
    Arc::new(|request: &OwnedTerm| {
        let n = tuple1_value(request).unwrap_or(0);
        let mut context = ProcessContext::new();
        let incremented = Term::try_small_int(n + 1).unwrap_or(Term::small_int(0));
        let ok = Term::atom(Atom::OK);
        let reply = context
            .alloc_tuple(&[incremented, ok])
            .unwrap_or(Term::small_int(n + 1));
        context
            .take_detached_result(reply)
            .unwrap_or_else(|| OwnedTerm::immediate(Term::small_int(n + 1)))
    })
}

#[test]
fn dyn_actor_call_async_round_trips_an_opaque_term_graph() {
    let scheduler = scheduler();
    let actor =
        spawn_actor_cooperative::<DynActor, _>(&scheduler, || DynActor::new(increment_reply()));

    // Issue the call with an opaque `{41}` request graph. Nothing has run yet.
    let mut future = Box::pin(actor.sender.call_async(WireTerm::new(owned_tuple1(41))));
    assert!(
        matches!(poll_once(&mut future), Poll::Pending),
        "future is pending before any host turn runs"
    );

    let mut resolved = None;
    for _ in 0..8 {
        scheduler.borrow_mut().run_until_idle();
        if let Poll::Ready(result) = poll_once(&mut future) {
            resolved = Some(result);
            break;
        }
    }

    let reply = resolved
        .expect("future resolved within the pumped turns")
        .expect("the dynamic actor replied (no timeout)");
    // The reply is the transform's `{42, ok}` graph, deep-copied back across the
    // boundary into a fresh owned term.
    let tuple = Tuple::new(reply.owned().root()).expect("reply is a tuple graph");
    assert_eq!(
        tuple.arity(),
        2,
        "reply tuple has the transform's two fields"
    );
    assert_eq!(
        tuple_first(reply.owned()),
        Some(42),
        "reply carries n + 1 from the host transform"
    );
    assert_eq!(
        tuple.get(1).and_then(Term::as_atom),
        Some(Atom::OK),
        "reply carries the transform's `ok` atom"
    );
}

#[test]
fn dyn_actor_call_async_rejects_on_timeout_when_no_reply_arrives() {
    // A live pid that is not a DynActor never decodes the call envelope, so no
    // reply is produced and the call can only resolve via its timeout — the
    // Promise-rejection path. Fired deterministically through the WR-4 wheel.
    let scheduler = scheduler();
    let target = scheduler
        .borrow_mut()
        .spawn_native_root(Box::new(|| Box::new(ParkForever)));

    let handle = CoopSenderHandle::<DynActor>::attach(&scheduler, target);
    let delay = Duration::from_secs(10);
    let mut future = Box::pin(handle.call_async_timeout(WireTerm::new(owned_tuple1(7)), delay));

    scheduler.borrow_mut().run_until_idle();
    assert!(
        matches!(poll_once(&mut future), Poll::Pending),
        "future is pending while the request is outstanding and the timeout is armed"
    );

    let start = Instant::now();
    let _early = scheduler
        .borrow_mut()
        .tick_native_timers_at(start + Duration::from_secs(5));
    scheduler.borrow_mut().run_until_idle();
    assert!(
        matches!(poll_once(&mut future), Poll::Pending),
        "future stays pending before the timeout deadline"
    );

    let _fired = scheduler
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

    assert!(
        matches!(rejected, Some(Err(ActorError::Timeout))),
        "the dynamic call_async future rejected with Timeout when no reply arrived, got {:?}",
        rejected.as_ref().map(Result::is_ok)
    );
}

/// A native handler that parks forever — a live pid that never replies.
struct ParkForever;

impl crate::native::native_process::NativeHandler for ParkForever {
    fn handle(
        &mut self,
        _ctx: &mut crate::native::native_process::NativeContext<'_>,
    ) -> crate::native::native_process::NativeOutcome {
        crate::native::native_process::NativeOutcome::Wait
    }
}
