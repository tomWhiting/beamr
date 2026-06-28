//! Dynamic, term-carrying [`Actor`] for untyped hosts (WR-8).
//!
//! [`super::Actor`] is generic over associated `Call`/`Reply`/`Cast` types, but a
//! JavaScript host is untyped: it hands the VM a request value and awaits a reply
//! value. [`DynActor`] bridges that gap. Its `Call`/`Reply`/`Cast` are all the
//! same opaque payload — a [`WireTerm`], an `Arc`-shared [`OwnedTerm`] graph — so
//! a request marshalled from a host value (e.g. a `JsValue` via the `beamr-wasm`
//! term codec) crosses the actor boundary unchanged, and the reply the actor
//! produces crosses back the same way.
//!
//! The reply is computed by a pure [`ReplyFn`] transform supplied at spawn time:
//! `Fn(&OwnedTerm) -> OwnedTerm`. The transform is `Send + Sync + 'static` so it
//! is captured by the restart-capable spawn factory exactly like any other
//! actor; a host (such as `beamr-wasm`) supplies a transform that invokes a
//! registered host callback to compute the reply, and a native test supplies a
//! plain Rust transform. The actor itself reuses the WR-6 envelope machinery
//! verbatim — it is just an [`super::Actor`] whose message types happen to be
//! opaque terms — so [`super::CoopSenderHandle::call_async`] / its
//! [`super::CallFuture`] drive it with no new correlation, timeout, or wire code.
//!
//! # Term marshalling
//!
//! On the wire a [`WireTerm`] is the term graph itself. [`WireTerm::encode`]
//! deep-copies the owned graph onto the running process heap via
//! [`super::super::native_process::NativeContext::alloc_owned_term`];
//! [`WireTerm::decode`] deep-copies the received heap term back into a fresh,
//! self-contained [`OwnedTerm`] via [`copy_term_to_ets`]. Both reuse the same
//! deep-copier ETS delivery uses, so no term ever dangles across the boundary.

use std::sync::Arc;

use super::{Actor, ActorContext, ActorMessage};
use crate::ets::{OwnedTerm, copy_term_to_ets};
use crate::native::native_process::NativeContext;
use crate::term::Term;

/// A pure transform from a request term graph to a reply term graph.
///
/// `Send + Sync + 'static` so it can be captured by the restart-capable native
/// spawn factory (Decision: the dynamic actor is restart-capable like any other).
pub type ReplyFn = Arc<dyn Fn(&OwnedTerm) -> OwnedTerm + Send + Sync + 'static>;

/// An opaque term payload that crosses the dynamic actor boundary.
///
/// `Arc<OwnedTerm>` so it is `Clone` (the [`ActorMessage`] bound) without copying
/// the graph, and `Send + Sync + 'static` so a captured request survives a
/// restart factory.
#[derive(Clone, Debug)]
pub struct WireTerm(Arc<OwnedTerm>);

impl WireTerm {
    /// Wrap an owned term graph for transport to the actor.
    #[must_use]
    pub fn new(owned: OwnedTerm) -> Self {
        Self(Arc::new(owned))
    }

    /// The wrapped owned term graph.
    #[must_use]
    pub fn owned(&self) -> &OwnedTerm {
        &self.0
    }
}

impl ActorMessage for WireTerm {
    fn encode(&self, ctx: &mut NativeContext<'_>) -> Option<Term> {
        ctx.alloc_owned_term(&self.0)
    }

    fn decode(term: Term) -> Option<Self> {
        copy_term_to_ets(term).ok().map(Self::new)
    }
}

/// A dynamic actor: every inbound call/cast term is run through `reply` and the
/// resulting term is returned (calls) or discarded (casts).
pub struct DynActor {
    reply: ReplyFn,
}

impl DynActor {
    /// Build a dynamic actor that answers each call by running `reply`.
    #[must_use]
    pub fn new(reply: ReplyFn) -> Self {
        Self { reply }
    }
}

impl Actor for DynActor {
    type Call = WireTerm;
    type Reply = WireTerm;
    type Cast = WireTerm;

    fn handle_call(&mut self, request: WireTerm, _ctx: &mut ActorContext<'_, '_>) -> WireTerm {
        WireTerm::new((self.reply)(request.owned()))
    }

    fn handle_cast(&mut self, request: WireTerm, _ctx: &mut ActorContext<'_, '_>) {
        // A cast still runs the transform (so a host handler observes the
        // message) but the produced reply is discarded — fire-and-forget.
        let _discarded = (self.reply)(request.owned());
    }
}

#[cfg(test)]
#[path = "actor_dynamic_tests.rs"]
mod tests;
