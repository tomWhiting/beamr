//! Reduction-boundary hook — the bit that's ours.
//!
//! At every process yield (budget exhausted or blocking on receive), the hook
//! fires if a registrant is present. The core provides the seam; what runs in it
//! is registered from outside. The hook receives copied process metadata only and
//! returns a scheduling decision.

use std::sync::{Arc, RwLock};

use crate::atom::Atom;

/// Copied metadata supplied to a reduction-boundary hook callback.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct HookEvent {
    /// Yielding process identifier.
    pub pid: u64,
    /// Current module atom from the process MFA metadata.
    pub module: Atom,
    /// Current function atom from the process MFA metadata.
    pub function: Atom,
    /// Current function arity.
    pub arity: u8,
    /// Reductions consumed in the just-finished scheduler slice.
    pub reductions_consumed: u32,
}

/// Scheduling decision returned by a hook callback.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum HookDecision {
    /// Preserve normal scheduler handling for this yield point.
    Continue,
    /// Hold the process until it is explicitly resumed.
    Suspend,
}

type HookCallback = dyn Fn(HookEvent) -> HookDecision + Send + Sync + 'static;

/// Per-VM reduction-boundary hook registration slot.
#[derive(Clone, Default)]
pub struct Hook {
    callback: Arc<RwLock<Option<Arc<HookCallback>>>>,
}

impl Hook {
    /// Create an empty hook registration slot.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register or replace the hook callback for this VM instance.
    pub fn register<F>(&self, callback: F)
    where
        F: Fn(HookEvent) -> HookDecision + Send + Sync + 'static,
    {
        let mut slot = self.callback.write().unwrap_or_else(|error| error.into_inner());
        *slot = Some(Arc::new(callback));
    }

    /// Remove the hook callback. After this returns, the registration slot is `None`.
    pub fn unregister(&self) {
        let mut slot = self.callback.write().unwrap_or_else(|error| error.into_inner());
        *slot = None;
    }

    /// Return true if a hook callback is currently registered.
    #[must_use]
    pub fn is_registered(&self) -> bool {
        self.callback
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .is_some()
    }

    /// Invoke the callback if registered, otherwise return [`HookDecision::Continue`].
    ///
    /// The `None` path performs no function-pointer call; it only checks the slot.
    #[must_use]
    pub fn invoke(&self, event: HookEvent) -> HookDecision {
        let callback = self
            .callback
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        match callback {
            Some(callback) => callback(event),
            None => HookDecision::Continue,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::{Hook, HookDecision, HookEvent};
    use crate::atom::Atom;

    fn event() -> HookEvent {
        HookEvent {
            pid: 7,
            module: Atom::OK,
            function: Atom::ERROR,
            arity: 2,
            reductions_consumed: 42,
        }
    }

    #[test]
    fn hook_register_replace_and_unregister_hook() {
        let hook = Hook::new();
        assert!(!hook.is_registered());
        assert_eq!(hook.invoke(event()), HookDecision::Continue);

        hook.register(|_| HookDecision::Suspend);
        assert!(hook.is_registered());
        assert_eq!(hook.invoke(event()), HookDecision::Suspend);

        hook.register(|_| HookDecision::Continue);
        assert_eq!(hook.invoke(event()), HookDecision::Continue);

        hook.unregister();
        assert!(!hook.is_registered());
        assert_eq!(hook.invoke(event()), HookDecision::Continue);
    }

    #[test]
    fn hook_receives_copied_metadata_at_yield() {
        let hook = Hook::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_by_hook = Arc::clone(&seen);
        hook.register(move |event| {
            seen_by_hook
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(event);
            HookDecision::Continue
        });

        assert_eq!(hook.invoke(event()), HookDecision::Continue);

        assert_eq!(
            seen.lock().unwrap_or_else(|error| error.into_inner()).as_slice(),
            &[event()]
        );
    }

    #[test]
    fn unregistered_hook_does_not_call_previous_callback() {
        let hook = Hook::new();
        let calls = Arc::new(Mutex::new(0_u32));
        let calls_by_hook = Arc::clone(&calls);
        hook.register(move |_| {
            *calls_by_hook
                .lock()
                .unwrap_or_else(|error| error.into_inner()) += 1;
            HookDecision::Continue
        });
        hook.unregister();

        assert_eq!(hook.invoke(event()), HookDecision::Continue);
        assert_eq!(*calls.lock().unwrap_or_else(|error| error.into_inner()), 0);
    }
}
