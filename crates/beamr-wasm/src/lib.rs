//! JavaScript bindings for the cooperative Beamr WASM runtime.

mod convert;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use beamr::atom::AtomTable;
use beamr::loader::{UnresolvedImport, load_module_with_origin};
use beamr::module::{ModuleOrigin, ModuleRegistry};
use beamr::native::bifs::register_gate1_bifs;

use beamr::native::etf_bifs::register_etf_bifs;
use beamr::native::exception_bifs::register_exception_bifs;
use beamr::native::process_bifs::register_gate2_bifs;
use beamr::native::stdlib_stubs::register_stdlib_stubs;
use beamr::native::{
    BifRegistryImpl, Capability, NativeKey, NativeRegistrationError, WasmAsyncNifFacility,
};
use beamr::scheduler::{WasmAsyncCompletion, WasmRunSummary, WasmScheduler};
use beamr::term::json::term_to_value;
use beamr::term::{Term, format::format_term};
use convert::{
    js_value_to_owned_term, js_value_to_term_in_context, term_to_js_value, terms_from_json_array,
    terms_to_js_array,
};
use js_sys::{Function, Promise, Reflect};
use serde_json::{Value, json};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

/// Construct a new Beamr VM handle for JavaScript hosts.
#[wasm_bindgen]
pub fn create_vm() -> Result<WasmVm, JsValue> {
    WasmVm::new()
}

/// A single-node Beamr VM driven cooperatively by JavaScript.
#[wasm_bindgen]
pub struct WasmVm {
    atom_table: Arc<AtomTable>,
    module_registry: Arc<ModuleRegistry>,
    bif_registry: Arc<BifRegistryImpl>,
    scheduler: Rc<RefCell<WasmScheduler>>,
    timer_handles: Rc<RefCell<BTreeMap<u64, HostTimer>>>,
    async_bridge: Rc<HostAsyncNifs>,
    js_callbacks: Rc<HostJsCallbacks>,
}

#[wasm_bindgen]
impl WasmVm {
    /// Create a VM with common atoms and wasm-safe BIF registrations.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Result<WasmVm, JsValue> {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let module_registry = Arc::new(ModuleRegistry::new());
        let bif_registry = Arc::new(BifRegistryImpl::new());
        register_wasm_safe_bifs(&bif_registry, &atom_table).map_err(registration_error_to_js)?;
        let scheduler = Rc::new(RefCell::new(WasmScheduler::new(
            Arc::clone(&atom_table),
            Arc::clone(&module_registry),
            Arc::clone(&bif_registry),
        )));
        let async_bridge = Rc::new(HostAsyncNifs::new(
            Arc::clone(&atom_table),
            Rc::downgrade(&scheduler),
        ));
        let js_callbacks = Rc::new(HostJsCallbacks::new(
            Arc::clone(&atom_table),
            Rc::downgrade(&scheduler),
        ));
        let facility: Rc<dyn WasmAsyncNifFacility> = Rc::new(HostWasmFacility {
            async_nifs: Rc::clone(&async_bridge),
            js_callbacks: Rc::clone(&js_callbacks),
            js_callback_module: atom_table.intern("wasm_ffi"),
            js_callback_function: atom_table.intern("js_callback"),
        });
        scheduler
            .borrow_mut()
            .set_wasm_async_nif_facility(Some(facility));
        Ok(Self {
            atom_table,
            module_registry,
            bif_registry,
            scheduler,
            timer_handles: Rc::new(RefCell::new(BTreeMap::new())),
            async_bridge,
            js_callbacks,
        })
    }

    /// Load a caller-provided `.beam` module byte buffer.
    pub fn load_module(&mut self, bytes: &[u8]) -> Result<JsValue, JsValue> {
        let (module, unresolved) = load_module_with_origin(
            bytes,
            self.atom_table.as_ref(),
            self.module_registry.as_ref(),
            self.bif_registry.as_ref(),
            ModuleOrigin::Preloaded,
        )
        .map_err(|error| JsValue::from_str(&error.to_string()))?;
        let unresolved = unresolved_imports_to_json(unresolved.imports(), self.atom_table.as_ref());
        let result = json!({
            "ok": true,
            "module": self.atom_table.resolve(module.name).unwrap_or("#<unknown>"),
            "unresolved": unresolved,
        });
        json_to_js(&result)
    }

    /// Send a JavaScript value to a BEAM process mailbox by local PID.
    pub fn send_message(&mut self, pid: u64, value: JsValue) -> Result<(), JsValue> {
        let message = js_value_to_owned_term(value, &self.atom_table)?;
        self.scheduler
            .borrow_mut()
            .send_owned(pid, &message)
            .map_err(|error| JsValue::from_str(&error.to_string()))?;
        self.sync_host_timers()?;
        Ok(())
    }

    /// Register a JavaScript function for `wasm_ffi:js_callback/{N}` calls.
    pub fn register_js_callback(&mut self, name: &str, callback: Function) {
        self.js_callbacks.register(name, callback);
    }

    /// Register `wasm_ffi:js_callback/Arity` for a previously registered JS callback.
    ///
    /// The BEAM call shape is `wasm_ffi:js_callback(Name, Arg1, ..., ArgN)`, so
    /// the registered native arity must include the leading callback name.
    pub fn register_js_callback_nif(&mut self, arity: u8) -> Result<(), JsValue> {
        let module_atom = self.atom_table.intern("wasm_ffi");
        let function_atom = self.atom_table.intern("js_callback");
        self.bif_registry
            .register(
                module_atom,
                function_atom,
                arity,
                js_callback_nif,
                Capability::ExternalIo,
            )
            .map_err(registration_error_to_js)
    }

    /// Register a JavaScript Promise-returning native under module/function/arity.
    pub fn register_async_nif(
        &mut self,
        module: &str,
        function: &str,
        arity: u8,
        callback: Function,
    ) -> Result<(), JsValue> {
        let module_atom = self.atom_table.intern(module);
        let function_atom = self.atom_table.intern(function);
        self.async_bridge
            .register((module_atom, function_atom, arity), callback);
        self.bif_registry
            .register(
                module_atom,
                function_atom,
                arity,
                wasm_async_nif_stub,
                Capability::ExternalIo,
            )
            .map_err(registration_error_to_js)
    }

    /// Spawn an exported function. Arguments are encoded as a JSON array string.
    pub fn spawn(&mut self, module: &str, function: &str, args_json: &str) -> Result<u64, JsValue> {
        let args_value: Value = serde_json::from_str(args_json)
            .map_err(|error| JsValue::from_str(&format!("invalid args JSON: {error}")))?;
        let args = self.json_args_to_terms(&args_value)?;
        let module = self.atom_table.intern(module);
        let function = self.atom_table.intern(function);
        let pid = self
            .scheduler
            .borrow_mut()
            .spawn_owned(module, function, args)
            .map_err(|error| JsValue::from_str(&error.to_string()))?;
        Ok(pid)
    }

    /// Run one cooperative scheduling round and return a JSON summary.
    pub fn run_step(&mut self) -> Result<JsValue, JsValue> {
        let summary = self.scheduler.borrow_mut().run_until_idle();
        self.sync_host_timers()?;
        let exits = self
            .scheduler
            .borrow()
            .exit_results()
            .into_iter()
            .map(|(pid, term)| json!({ "pid": pid, "value": self.term_to_json_or_fallback(term) }))
            .collect::<Vec<_>>();
        let value = summary_to_json(summary, exits);
        json_to_js(&value)
    }

    /// Called by tests or custom hosts to drive an already-fired timer manually.
    pub fn timer_fired(&mut self, pid: u64, timer_id: u64) -> Result<bool, JsValue> {
        self.timer_handles.borrow_mut().remove(&timer_id);
        let fired = self.scheduler.borrow_mut().timer_fired(pid, timer_id);
        self.sync_host_timers()?;
        Ok(fired)
    }

    fn json_args_to_terms(&self, value: &Value) -> Result<Vec<beamr::ets::OwnedTerm>, JsValue> {
        terms_from_json_array(value, &self.atom_table)
    }

    fn term_to_json_or_fallback(&self, term: Term) -> Value {
        term_to_json_or_fallback(term, self.atom_table.as_ref())
    }

    fn sync_host_timers(&mut self) -> Result<(), JsValue> {
        let cancellations = self
            .scheduler
            .borrow_mut()
            .take_pending_timer_cancellations();
        for timer_id in cancellations {
            self.clear_host_timer(timer_id);
        }
        let schedules = self.scheduler.borrow_mut().take_pending_timer_schedules();
        for schedule in schedules {
            self.schedule_host_timer(schedule.pid, schedule.timer_id, schedule.milliseconds)?;
        }
        Ok(())
    }

    fn schedule_host_timer(
        &mut self,
        pid: u64,
        timer_id: u64,
        milliseconds: u64,
    ) -> Result<(), JsValue> {
        self.clear_host_timer(timer_id);
        let scheduler = Rc::clone(&self.scheduler);
        let handles = Rc::clone(&self.timer_handles);
        let callback = Closure::<dyn FnMut()>::new(move || {
            handles.borrow_mut().remove(&timer_id);
            let _fired = scheduler.borrow_mut().timer_fired(pid, timer_id);
        });
        let handle = set_timeout(&callback, milliseconds)?;
        self.timer_handles.borrow_mut().insert(
            timer_id,
            HostTimer {
                handle,
                _callback: callback,
            },
        );
        Ok(())
    }

    fn clear_host_timer(&mut self, timer_id: u64) {
        if let Some(timer) = self.timer_handles.borrow_mut().remove(&timer_id) {
            clear_timeout(timer.handle);
        }
    }
}

struct HostTimer {
    handle: i32,
    _callback: Closure<dyn FnMut()>,
}

struct HostAsyncNifs {
    atom_table: Arc<AtomTable>,
    callbacks: RefCell<BTreeMap<NativeKey, Function>>,
    scheduler: Weak<RefCell<WasmScheduler>>,
}

impl HostAsyncNifs {
    fn new(atom_table: Arc<AtomTable>, scheduler: Weak<RefCell<WasmScheduler>>) -> Self {
        Self {
            atom_table,
            callbacks: RefCell::new(BTreeMap::new()),
            scheduler,
        }
    }

    fn register(&self, key: NativeKey, callback: Function) {
        self.callbacks.borrow_mut().insert(key, callback);
    }
}

impl HostAsyncNifs {
    fn start_async_nif(
        &self,
        mfa: NativeKey,
        args: &[Term],
        context: &mut beamr::native::ProcessContext<'_>,
    ) -> Result<Term, Term> {
        let Some(callback) = self.callbacks.borrow().get(&mfa).cloned() else {
            return Err(Term::atom(beamr::atom::Atom::UNDEF));
        };
        self.start_callback(callback, args, context, HostCallbackArguments::SingleArray)
    }

    fn start_callback(
        &self,
        callback: Function,
        args: &[Term],
        context: &mut beamr::native::ProcessContext<'_>,
        arguments: HostCallbackArguments,
    ) -> Result<Term, Term> {
        let Some(pid) = context.pid() else {
            return Err(Term::atom(beamr::atom::Atom::BADARG));
        };
        let args_array = terms_to_js_array(args, self.atom_table.as_ref())
            .map_err(|_| Term::atom(beamr::atom::Atom::BADARG))?;
        let value = match arguments {
            HostCallbackArguments::SingleArray => callback.call1(&JsValue::UNDEFINED, &args_array),
            HostCallbackArguments::Positional => Reflect::apply(
                &callback,
                &JsValue::UNDEFINED,
                args_array.unchecked_ref::<js_sys::Array>(),
            ),
        }
        .map_err(|_| Term::atom(beamr::atom::Atom::BADARG))?;
        if is_promise_like(&value) {
            self.start_promise_completion(pid, Promise::resolve(&value));
            context.request_suspend(None);
            Ok(Term::NIL)
        } else {
            js_value_to_term_in_context(value, context)
                .map_err(|_| Term::atom(beamr::atom::Atom::BADARG))
        }
    }

    fn start_promise_completion(&self, pid: u64, promise: Promise) {
        let scheduler = self.scheduler.clone();
        let atom_table = Arc::clone(&self.atom_table);
        wasm_bindgen_futures::spawn_local(async move {
            let completion = match JsFuture::from(promise).await {
                Ok(value) => js_value_to_owned_term(value, &atom_table)
                    .map(WasmAsyncCompletion::Ok)
                    .unwrap_or_else(|_| {
                        WasmAsyncCompletion::Error(beamr::ets::OwnedTerm::immediate(Term::atom(
                            beamr::atom::Atom::BADARG,
                        )))
                    }),
                Err(error) => js_value_to_owned_term(error, &atom_table)
                    .map(WasmAsyncCompletion::Error)
                    .unwrap_or_else(|_| {
                        WasmAsyncCompletion::Error(beamr::ets::OwnedTerm::immediate(Term::atom(
                            beamr::atom::Atom::ERROR,
                        )))
                    }),
            };
            if let Some(scheduler) = scheduler.upgrade() {
                let _completed = scheduler.borrow_mut().complete_async(pid, completion);
            }
        });
    }
}

struct HostJsCallbacks {
    atom_table: Arc<AtomTable>,
    callbacks: RefCell<BTreeMap<String, Function>>,
    async_nifs: Rc<HostAsyncNifs>,
}

impl HostJsCallbacks {
    fn new(atom_table: Arc<AtomTable>, scheduler: Weak<RefCell<WasmScheduler>>) -> Self {
        let async_nifs = Rc::new(HostAsyncNifs::new(Arc::clone(&atom_table), scheduler));
        Self {
            atom_table,
            callbacks: RefCell::new(BTreeMap::new()),
            async_nifs,
        }
    }

    fn register(&self, name: &str, callback: Function) {
        self.callbacks
            .borrow_mut()
            .insert(name.to_owned(), callback);
    }

    fn start_js_callback(
        &self,
        args: &[Term],
        context: &mut beamr::native::ProcessContext<'_>,
    ) -> Result<Term, Term> {
        let Some((name_term, callback_args)) = args.split_first() else {
            return Err(Term::atom(beamr::atom::Atom::BADARG));
        };
        let name_value = term_to_js_value(*name_term, self.atom_table.as_ref())
            .map_err(|_| Term::atom(beamr::atom::Atom::BADARG))?;
        let Some(name) = name_value.as_string() else {
            return Err(Term::atom(beamr::atom::Atom::BADARG));
        };
        let Some(callback) = self.callbacks.borrow().get(&name).cloned() else {
            return Err(Term::atom(beamr::atom::Atom::UNDEF));
        };
        self.async_nifs.start_callback(
            callback,
            callback_args,
            context,
            HostCallbackArguments::Positional,
        )
    }
}

#[derive(Clone, Copy)]
enum HostCallbackArguments {
    SingleArray,
    Positional,
}

struct HostWasmFacility {
    async_nifs: Rc<HostAsyncNifs>,
    js_callbacks: Rc<HostJsCallbacks>,
    js_callback_module: beamr::atom::Atom,
    js_callback_function: beamr::atom::Atom,
}

impl WasmAsyncNifFacility for HostWasmFacility {
    fn start_async_nif(
        &self,
        mfa: NativeKey,
        args: &[Term],
        context: &mut beamr::native::ProcessContext<'_>,
    ) -> Result<Term, Term> {
        if mfa.0 == self.js_callback_module && mfa.1 == self.js_callback_function {
            self.js_callbacks.start_js_callback(args, context)
        } else {
            self.async_nifs.start_async_nif(mfa, args, context)
        }
    }
}

fn js_callback_nif(
    args: &[Term],
    context: &mut beamr::native::ProcessContext<'_>,
) -> Result<Term, Term> {
    wasm_async_nif_stub(args, context)
}

fn wasm_async_nif_stub(
    args: &[Term],
    context: &mut beamr::native::ProcessContext<'_>,
) -> Result<Term, Term> {
    let Some(mfa) = context.current_native() else {
        return Err(Term::atom(beamr::atom::Atom::UNDEF));
    };
    let Some(facility) = context.wasm_async_nif_facility() else {
        return Err(Term::atom(beamr::atom::Atom::UNDEF));
    };
    facility.start_async_nif(mfa, args, context)
}

fn is_promise_like(value: &JsValue) -> bool {
    Reflect::get(value, &JsValue::from_str("then"))
        .ok()
        .is_some_and(|then| then.is_function())
}

fn register_wasm_safe_bifs(
    registry: &BifRegistryImpl,
    atom_table: &AtomTable,
) -> Result<(), NativeRegistrationError> {
    register_gate1_bifs(registry, atom_table)?;
    register_gate2_bifs(registry, atom_table)?;
    register_exception_bifs(registry, atom_table)?;
    register_etf_bifs(registry, atom_table)?;
    register_stdlib_stubs(registry, atom_table)?;
    Ok(())
}

fn unresolved_imports_to_json(
    imports: Vec<UnresolvedImport>,
    atom_table: &AtomTable,
) -> Vec<Value> {
    imports
        .into_iter()
        .map(|import| {
            let module = atom_table
                .resolve(import.module)
                .map_or_else(|| format!("{:?}", import.module), str::to_owned);
            let function = atom_table
                .resolve(import.function)
                .map_or_else(|| format!("{:?}", import.function), str::to_owned);
            json!({
                "module": module,
                "function": function,
                "arity": import.arity,
            })
        })
        .collect()
}

fn summary_to_json(summary: WasmRunSummary, exits: Vec<Value>) -> Value {
    json!({
        "executed": summary.executed,
        "yielded": summary.yielded,
        "waiting": summary.waiting,
        "exited": summary.exited,
        "errored": summary.errored,
        "results": exits,
    })
}

fn term_to_json_or_fallback(term: Term, atom_table: &AtomTable) -> Value {
    match term_to_value(term, atom_table) {
        Ok(value) => value,
        Err(_) => Value::String(format_term(term, atom_table)),
    }
}

fn json_to_js(value: &Value) -> Result<JsValue, JsValue> {
    Ok(JsValue::from_str(&value.to_string()))
}

fn registration_error_to_js(error: NativeRegistrationError) -> JsValue {
    JsValue::from_str(&error.to_string())
}

fn set_timeout(callback: &Closure<dyn FnMut()>, milliseconds: u64) -> Result<i32, JsValue> {
    let global = js_sys::global();
    let set_timeout = Reflect::get(&global, &JsValue::from_str("setTimeout"))?
        .dyn_into::<Function>()
        .map_err(|_| JsValue::from_str("global setTimeout is not a function"))?;
    let delay = i32::try_from(milliseconds).unwrap_or(i32::MAX);
    let handle = set_timeout.call2(
        &global,
        callback.as_ref().unchecked_ref(),
        &JsValue::from_f64(f64::from(delay)),
    )?;
    handle
        .as_f64()
        .and_then(|value| i32::try_from(value as i64).ok())
        .ok_or_else(|| JsValue::from_str("setTimeout did not return a numeric handle"))
}

fn clear_timeout(handle: i32) {
    let global = js_sys::global();
    if let Ok(clear_timeout) = Reflect::get(&global, &JsValue::from_str("clearTimeout"))
        && let Ok(clear_timeout) = clear_timeout.dyn_into::<Function>()
    {
        let _ignored = clear_timeout.call1(&global, &JsValue::from_f64(f64::from(handle)));
    }
}

// wasm-bindgen types abort when constructed outside a wasm runtime, so this
// suite only runs on the wasm32 target (e.g. via `wasm-pack test`).
#[cfg(all(test, target_arch = "wasm32"))]
mod tests {
    use super::*;

    #[test]
    fn create_vm_initializes() {
        let vm = WasmVm::new();
        assert!(vm.is_ok());
    }
}
