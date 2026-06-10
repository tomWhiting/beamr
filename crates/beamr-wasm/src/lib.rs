//! JavaScript bindings for the cooperative Beamr WASM runtime.

use std::sync::Arc;

use beamr::atom::AtomTable;
use beamr::loader::{UnresolvedImport, load_module_with_origin};
use beamr::module::{ModuleOrigin, ModuleRegistry};
use beamr::native::bifs::register_gate1_bifs;
use beamr::native::etf_bifs::register_etf_bifs;
use beamr::native::exception_bifs::register_exception_bifs;
use beamr::native::process_bifs::register_gate2_bifs;
use beamr::native::stdlib_stubs::register_stdlib_stubs;
use beamr::native::{BifRegistryImpl, NativeRegistrationError};
use beamr::process::Process;
use beamr::scheduler::{WasmRunSummary, WasmScheduler};
use beamr::term::json::{term_to_value, value_to_term};
use beamr::term::{Term, format::format_term};
use serde_json::{Value, json};
use wasm_bindgen::prelude::*;

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
    scheduler: WasmScheduler,
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
        let scheduler = WasmScheduler::new(
            Arc::clone(&atom_table),
            Arc::clone(&module_registry),
            Arc::clone(&bif_registry),
        );
        Ok(Self {
            atom_table,
            module_registry,
            bif_registry,
            scheduler,
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
        let unresolved = unresolved_imports_to_json(unresolved.imports());
        let result = json!({
            "ok": true,
            "module": self.atom_table.resolve(module.name).unwrap_or("#<unknown>"),
            "unresolved": unresolved,
        });
        json_to_js(&result)
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
            .spawn(module, function, args)
            .map_err(|error| JsValue::from_str(&error.to_string()))?;
        Ok(pid)
    }

    /// Run one cooperative scheduling round and return a JSON summary.
    pub fn run_step(&mut self) -> Result<JsValue, JsValue> {
        let summary = self.scheduler.run_until_idle();
        let exits = self
            .scheduler
            .exit_results()
            .into_iter()
            .map(|(pid, term)| json!({ "pid": pid, "value": self.term_to_json_or_fallback(term) }))
            .collect::<Vec<_>>();
        let value = summary_to_json(summary, exits);
        json_to_js(&value)
    }

    fn json_args_to_terms(&self, value: &Value) -> Result<Vec<Term>, JsValue> {
        let Value::Array(values) = value else {
            return Err(JsValue::from_str("spawn arguments must be a JSON array"));
        };
        let mut scratch = Process::new(0, beamr::process::heap::DEFAULT_HEAP_SIZE);
        let mut context = beamr::native::ProcessContext::new();
        context.set_atom_table(Some(Arc::clone(&self.atom_table)));
        context.attach_process(&mut scratch, 0);
        let mut terms = Vec::with_capacity(values.len());
        for value in values {
            let term = value_to_term(value, &mut context)
                .map_err(|error| JsValue::from_str(&error.to_string()))?;
            terms.push(term);
        }
        context.detach_process();
        Ok(terms)
    }

    fn term_to_json_or_fallback(&self, term: Term) -> Value {
        match term_to_value(term, self.atom_table.as_ref()) {
            Ok(value) => value,
            Err(_) => Value::String(format_term(term, self.atom_table.as_ref())),
        }
    }
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

fn unresolved_imports_to_json(imports: Vec<UnresolvedImport>) -> Vec<Value> {
    imports
        .into_iter()
        .map(|import| {
            json!({
                "module": import.module.index(),
                "function": import.function.index(),
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

fn json_to_js(value: &Value) -> Result<JsValue, JsValue> {
    Ok(JsValue::from_str(&value.to_string()))
}

fn registration_error_to_js(error: NativeRegistrationError) -> JsValue {
    JsValue::from_str(&error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_vm_initializes() {
        let vm = WasmVm::new();
        assert!(vm.is_ok());
    }
}
