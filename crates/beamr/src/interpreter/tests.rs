use super::{ExecutionResult, NativeServices, run, run_with_native_services, run_with_registry};
use crate::atom::{Atom, AtomTable};
use crate::capability::{
    CapabilityAuditEvent, CapabilityAuditSink, Sandbox, StderrViolationHandler, ViolationHandler,
};
use crate::error::ExecError;
use crate::jit::{JitCache, JitCacheKey, JitCompiler, JitSettings};
use crate::loader::decode::BinaryOp;
use crate::loader::decode::compact::Operand;
use crate::loader::{Instruction, Literal};
use crate::module::{Module, ModuleOrigin, ModuleRegistry, ResolvedImport, ResolvedImportTarget};
use crate::native::{Capability, CapabilitySet, ExceptionClass, NativeEntry, ProcessContext};
use crate::process::{CodePosition, ExitReason, Process};
use crate::scheduler::dirty::DirtySchedulerKind;
use crate::term::binary::{Binary, packed_word_count, write_binary};
use crate::term::boxed::{Cons, Tuple};
use crate::term::{Term, compare};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

fn module(name: Atom, code: Vec<Instruction>) -> Module {
    let label_index = code
        .iter()
        .enumerate()
        .filter_map(|(ip, instruction)| match instruction {
            Instruction::Label { label } => Some((*label, ip)),
            _ => None,
        })
        .collect();
    Module {
        name,
        generation: 0,
        origin: ModuleOrigin::Preloaded,
        exports: HashMap::new(),
        label_index,
        code,
        literals: Vec::new(),
        constant_pool: Default::default(),
        resolved_imports: Vec::new(),
        lambdas: Vec::new(),
        string_table: Vec::new(),
        function_table: Vec::new(),
        line_table: Vec::new(),
        line_info: Vec::new(),
    }
}

fn heap_binary(process: &mut Process, bytes: &[u8]) -> Term {
    let words = 2 + packed_word_count(bytes.len());
    let ptr = process.heap_mut().alloc(words).expect("test heap fits");
    // SAFETY: test helper immediately initialises the fresh heap allocation.
    let heap = unsafe { std::slice::from_raw_parts_mut(ptr, words) };
    write_binary(heap, bytes).expect("test binary fits")
}

#[derive(Default)]
struct CollectingAuditSink {
    events: Mutex<Vec<CapabilityAuditEvent>>,
}

impl CapabilityAuditSink for CollectingAuditSink {
    fn record(&self, event: CapabilityAuditEvent) {
        self.events.lock().expect("collector lock").push(event);
    }
}

#[derive(Default)]
struct CollectingViolationHandler {
    events: Mutex<Vec<CapabilityAuditEvent>>,
}

impl ViolationHandler for CollectingViolationHandler {
    fn on_violation(&self, event: CapabilityAuditEvent) {
        self.events.lock().expect("collector lock").push(event);
    }
}

fn empty_native_services() -> NativeServices {
    NativeServices::default()
}

fn native_services_with_jit_cache(jit_cache: Arc<JitCache>) -> NativeServices {
    NativeServices {
        jit_cache: Some(jit_cache),
        ..empty_native_services()
    }
}

#[test]
fn single_return_exits_normally() {
    let module = module(Atom::OK, vec![Instruction::Return]);
    let mut process = Process::new(1, 32);

    assert_eq!(
        run(&mut process, &module),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
}

#[test]
fn call_chain_executes_in_sequence_and_returns() {
    let module = module(
        Atom::OK,
        vec![
            Instruction::Call {
                arity: Operand::Unsigned(0),
                label: Operand::Label(2),
            },
            Instruction::Return,
            Instruction::Label { label: 2 },
            Instruction::Move {
                source: Operand::Integer(42),
                destination: Operand::X(0),
            },
            Instruction::Return,
        ],
    );
    let mut process = Process::new(1, 32);

    assert_eq!(
        run(&mut process, &module),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert_eq!(process.x_reg(0), Term::small_int(42));
}

#[test]
fn tight_call_loop_yields_at_reduction_budget_and_resumes() {
    let module = module(
        Atom::OK,
        vec![
            Instruction::Label { label: 1 },
            Instruction::CallOnly {
                arity: Operand::Unsigned(0),
                label: Operand::Label(1),
            },
        ],
    );
    let mut process = Process::new(1, 32);
    process.reset_reductions(3);

    assert_eq!(run(&mut process, &module), Ok(ExecutionResult::Yielded));
    assert_eq!(process.reduction_counter(), 0);
    assert_eq!(
        process.code_position(),
        Some(CodePosition {
            module: Atom::OK,
            instruction_pointer: 0,
        })
    );
    process.reset_reductions(1);
    assert_eq!(run(&mut process, &module), Ok(ExecutionResult::Yielded));
}

#[test]
fn intra_module_calls_continue_on_pinned_version_after_reload() {
    let registry = ModuleRegistry::new();
    let module_v1 = registry.insert(module(
        Atom::OK,
        vec![
            Instruction::CallOnly {
                arity: Operand::Unsigned(0),
                label: Operand::Label(2),
            },
            Instruction::Label { label: 2 },
            Instruction::Move {
                source: Operand::Integer(11),
                destination: Operand::X(0),
            },
            Instruction::Return,
        ],
    ));
    let _module_v2 = registry.insert(module(
        Atom::OK,
        vec![
            Instruction::Label { label: 2 },
            Instruction::Move {
                source: Operand::Integer(22),
                destination: Operand::X(0),
            },
            Instruction::Return,
        ],
    ));
    let mut process = Process::new(1, 32);
    process.set_current_module(Arc::clone(&module_v1));

    assert_eq!(
        run_with_registry(&mut process, &module_v1, &registry),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert_eq!(process.x_reg(0), Term::small_int(11));
}

#[test]
fn external_return_restores_caller_version_and_qualified_self_call_upgrades() {
    let registry = ModuleRegistry::new();
    let import_b = ResolvedImport {
        module: Atom::ERROR,
        function: Atom::OK,
        arity: 0,
        target: ResolvedImportTarget::Code {
            module: Atom::ERROR,
            label: 1,
        },
    };
    let import_self = ResolvedImport {
        module: Atom::OK,
        function: Atom::OK,
        arity: 0,
        target: ResolvedImportTarget::Code {
            module: Atom::OK,
            label: 9,
        },
    };
    let mut a_v1_data = module(
        Atom::OK,
        vec![
            Instruction::CallExt {
                arity: Operand::Unsigned(0),
                import: Operand::Unsigned(0),
            },
            Instruction::CallExtOnly {
                arity: Operand::Unsigned(0),
                import: Operand::Unsigned(1),
            },
        ],
    );
    a_v1_data.resolved_imports.push(import_b);
    a_v1_data.resolved_imports.push(import_self);
    let a_v1 = registry.insert(a_v1_data);
    let mut b_v1_data = module(
        Atom::ERROR,
        vec![Instruction::Label { label: 1 }, Instruction::Return],
    );
    b_v1_data.exports.insert((Atom::OK, 0), 1);
    let _b_v1 = registry.insert(b_v1_data);
    let mut a_v2_data = module(
        Atom::OK,
        vec![
            Instruction::Label { label: 9 },
            Instruction::Move {
                source: Operand::Integer(2),
                destination: Operand::X(0),
            },
            Instruction::Return,
        ],
    );
    a_v2_data.exports.insert((Atom::OK, 0), 9);
    let _a_v2 = registry.insert(a_v2_data);
    let mut process = Process::new(1, 128);
    process.set_current_module(Arc::clone(&a_v1));

    assert_eq!(
        run_with_registry(&mut process, &a_v1, &registry),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert_eq!(process.x_reg(0), Term::small_int(2));
}

#[test]
fn interpreter_local_call_dispatches_to_jit_cached_target() {
    let mut target_module = module(
        Atom::OK,
        vec![
            Instruction::Call {
                arity: Operand::Unsigned(0),
                label: Operand::Label(2),
            },
            Instruction::Return,
            Instruction::Label { label: 2 },
            Instruction::Move {
                source: Operand::Integer(99),
                destination: Operand::X(0),
            },
            Instruction::Return,
        ],
    );
    target_module.function_table.push((2, Atom::ERROR, 0));
    let registry = ModuleRegistry::new();
    let module = registry.insert(target_module);
    let compiler = JitCompiler::new(JitSettings).expect("host JIT compiler should initialize");
    let native = compiler
        .compile(
            &[
                Instruction::Move {
                    source: Operand::Integer(42),
                    destination: Operand::X(0),
                },
                Instruction::Return,
            ],
            module.name,
            Atom::ERROR,
            0,
        )
        .expect("target function should compile");
    let jit_cache = Arc::new(JitCache::new());
    jit_cache.insert(
        JitCacheKey::new(module.name, Atom::ERROR, 0, module.generation()),
        native,
    );
    let services = native_services_with_jit_cache(jit_cache);
    let mut process = Process::new(1, 32);
    process.set_current_module(Arc::clone(&module));

    assert_eq!(
        run_with_native_services(&mut process, &module, &registry, &services),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert_eq!(process.x_reg(0), Term::small_int(42));
}

#[test]
fn func_info_and_move_cover_metadata_register_literals_and_stack() {
    let atoms = AtomTable::new();
    let module_atom = atoms.intern("sample");
    let function_atom = atoms.intern("main");
    let literals = vec![Literal::Integer(7)];
    let mut module = module(
        module_atom,
        vec![
            Instruction::FuncInfo {
                module: Operand::Atom(Some(module_atom)),
                function: Operand::Atom(Some(function_atom)),
                arity: Operand::Unsigned(0),
            },
            Instruction::AllocateZero {
                stack_need: Operand::Unsigned(1),
                live: Operand::Unsigned(0),
            },
            Instruction::Move {
                source: Operand::Literal(0),
                destination: Operand::X(0),
            },
            Instruction::Move {
                source: Operand::X(0),
                destination: Operand::Y(0),
            },
            Instruction::Move {
                source: Operand::Y(0),
                destination: Operand::X(1),
            },
            Instruction::Deallocate {
                words: Operand::Unsigned(1),
            },
            Instruction::Return,
        ],
    );
    module.constant_pool =
        crate::constant_pool::materialise_literals(&literals, Some(&atoms)).expect("literal pool");
    module.literals = literals;
    let mut process = Process::new(1, 32);
    let before_heap = process.heap().used();

    assert_eq!(
        run(&mut process, &module),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert_eq!(process.current_mfa(), Some((module_atom, function_atom, 0)));
    assert_eq!(process.x_reg(1), Term::small_int(7));
    assert_eq!(process.heap().used(), before_heap);
}

#[test]
fn swap_exchanges_x_registers_without_allocating() {
    let module = module(
        Atom::OK,
        vec![
            Instruction::Move {
                source: Operand::Integer(42),
                destination: Operand::X(0),
            },
            Instruction::Move {
                source: Operand::Integer(99),
                destination: Operand::X(1),
            },
            Instruction::Swap {
                left: Operand::X(0),
                right: Operand::X(1),
            },
            Instruction::Return,
        ],
    );
    let mut process = Process::new(2, 32);
    let before_heap = process.heap().used();

    assert_eq!(
        run(&mut process, &module),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert_eq!(process.x_reg(0), Term::small_int(99));
    assert_eq!(process.x_reg(1), Term::small_int(42));
    assert_eq!(process.heap().used(), before_heap);
}

#[test]
fn swap_exchanges_y_and_x_registers_without_clobbering() {
    let atoms = AtomTable::new();
    let hello = atoms.intern("hello");
    let world = atoms.intern("world");
    let module = module(
        Atom::OK,
        vec![
            Instruction::AllocateZero {
                stack_need: Operand::Unsigned(1),
                live: Operand::Unsigned(0),
            },
            Instruction::Move {
                source: Operand::Atom(Some(hello)),
                destination: Operand::Y(0),
            },
            Instruction::Move {
                source: Operand::Atom(Some(world)),
                destination: Operand::X(0),
            },
            Instruction::Swap {
                left: Operand::Y(0),
                right: Operand::X(0),
            },
            Instruction::Move {
                source: Operand::Y(0),
                destination: Operand::X(1),
            },
            Instruction::Deallocate {
                words: Operand::Unsigned(1),
            },
            Instruction::Return,
        ],
    );
    let mut process = Process::new(2, 32);

    assert_eq!(
        run(&mut process, &module),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert_eq!(process.x_reg(0), Term::atom(hello));
    assert_eq!(process.x_reg(1), Term::atom(world));
}

#[test]
fn swap_exchanges_y_registers_without_clobbering() {
    let module = module(
        Atom::OK,
        vec![
            Instruction::AllocateZero {
                stack_need: Operand::Unsigned(2),
                live: Operand::Unsigned(0),
            },
            Instruction::Move {
                source: Operand::Integer(42),
                destination: Operand::Y(0),
            },
            Instruction::Move {
                source: Operand::Integer(99),
                destination: Operand::Y(1),
            },
            Instruction::Swap {
                left: Operand::Y(0),
                right: Operand::Y(1),
            },
            Instruction::Move {
                source: Operand::Y(0),
                destination: Operand::X(0),
            },
            Instruction::Move {
                source: Operand::Y(1),
                destination: Operand::X(1),
            },
            Instruction::Deallocate {
                words: Operand::Unsigned(2),
            },
            Instruction::Return,
        ],
    );
    let mut process = Process::new(2, 32);

    assert_eq!(
        run(&mut process, &module),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert_eq!(process.x_reg(0), Term::small_int(99));
    assert_eq!(process.x_reg(1), Term::small_int(42));
}

#[test]
fn swap_same_register_is_no_op() {
    let module = module(
        Atom::OK,
        vec![
            Instruction::Move {
                source: Operand::Integer(42),
                destination: Operand::X(0),
            },
            Instruction::Swap {
                left: Operand::X(0),
                right: Operand::X(0),
            },
            Instruction::Return,
        ],
    );
    let mut process = Process::new(1, 32);

    assert_eq!(
        run(&mut process, &module),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert_eq!(process.x_reg(0), Term::small_int(42));
}

#[test]
fn trim_preserves_remaining_y_registers_and_deallocate_pops_trimmed_frame() {
    let module = module(
        Atom::OK,
        vec![
            Instruction::AllocateZero {
                stack_need: Operand::Unsigned(5),
                live: Operand::Unsigned(0),
            },
            Instruction::Move {
                source: Operand::Integer(10),
                destination: Operand::Y(0),
            },
            Instruction::Move {
                source: Operand::Integer(20),
                destination: Operand::Y(1),
            },
            Instruction::Move {
                source: Operand::Integer(30),
                destination: Operand::Y(2),
            },
            Instruction::Move {
                source: Operand::Integer(40),
                destination: Operand::Y(3),
            },
            Instruction::Move {
                source: Operand::Integer(50),
                destination: Operand::Y(4),
            },
            Instruction::Trim {
                words: Operand::Unsigned(2),
                remaining: Operand::Unsigned(3),
            },
            Instruction::Move {
                source: Operand::Y(0),
                destination: Operand::X(0),
            },
            Instruction::Move {
                source: Operand::Y(1),
                destination: Operand::X(1),
            },
            Instruction::Move {
                source: Operand::Y(2),
                destination: Operand::X(2),
            },
            Instruction::Deallocate {
                words: Operand::Unsigned(3),
            },
            Instruction::Return,
        ],
    );
    let mut process = Process::new(1, 32);

    assert_eq!(
        run(&mut process, &module),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert_eq!(process.x_reg(0), Term::small_int(30));
    assert_eq!(process.x_reg(1), Term::small_int(40));
    assert_eq!(process.x_reg(2), Term::small_int(50));
    assert_eq!(process.stack().len(), 0);
}

#[test]
fn trim_zero_words_is_noop_when_slot_count_matches() {
    let module = module(
        Atom::OK,
        vec![
            Instruction::AllocateZero {
                stack_need: Operand::Unsigned(3),
                live: Operand::Unsigned(0),
            },
            Instruction::Move {
                source: Operand::Integer(10),
                destination: Operand::Y(0),
            },
            Instruction::Move {
                source: Operand::Integer(20),
                destination: Operand::Y(1),
            },
            Instruction::Move {
                source: Operand::Integer(30),
                destination: Operand::Y(2),
            },
            Instruction::Trim {
                words: Operand::Unsigned(0),
                remaining: Operand::Unsigned(3),
            },
            Instruction::Move {
                source: Operand::Y(0),
                destination: Operand::X(0),
            },
            Instruction::Move {
                source: Operand::Y(1),
                destination: Operand::X(1),
            },
            Instruction::Move {
                source: Operand::Y(2),
                destination: Operand::X(2),
            },
            Instruction::Deallocate {
                words: Operand::Unsigned(3),
            },
            Instruction::Return,
        ],
    );
    let mut process = Process::new(1, 32);

    assert_eq!(
        run(&mut process, &module),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert_eq!(process.x_reg(0), Term::small_int(10));
    assert_eq!(process.x_reg(1), Term::small_int(20));
    assert_eq!(process.x_reg(2), Term::small_int(30));
}

#[test]
fn trim_rejects_mismatched_frame_size_and_empty_stack_errors() {
    let mismatch = module(
        Atom::OK,
        vec![
            Instruction::AllocateZero {
                stack_need: Operand::Unsigned(5),
                live: Operand::Unsigned(0),
            },
            Instruction::Trim {
                words: Operand::Unsigned(1),
                remaining: Operand::Unsigned(3),
            },
        ],
    );
    assert_eq!(
        run(&mut Process::new(1, 32), &mismatch),
        Err(ExecError::Badarg)
    );

    let empty_stack = module(
        Atom::OK,
        vec![Instruction::Trim {
            words: Operand::Unsigned(0),
            remaining: Operand::Unsigned(0),
        }],
    );
    assert!(matches!(
        run(&mut Process::new(1, 32), &empty_stack),
        Err(ExecError::Stack(_))
    ));
}

#[test]
fn stack_heap_and_data_opcodes_work() {
    let module = module(
        Atom::OK,
        vec![
            Instruction::TestHeap {
                heap_need: Operand::Unsigned(6),
                live: Operand::Unsigned(0),
            },
            Instruction::PutList {
                head: Operand::Integer(1),
                tail: Operand::Atom(None),
                destination: Operand::X(0),
            },
            Instruction::PutTuple2 {
                destination: Operand::X(1),
                elements: Operand::List(vec![
                    Operand::Integer(1),
                    Operand::Integer(2),
                    Operand::Integer(3),
                ]),
            },
            Instruction::GetTupleElement {
                source: Operand::X(1),
                index: Operand::Unsigned(0),
                destination: Operand::X(2),
            },
            Instruction::Return,
        ],
    );
    let mut process = Process::new(1, 8);

    assert_eq!(
        run(&mut process, &module),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    let cons = Cons::new(process.x_reg(0)).expect("put_list creates cons");
    assert_eq!(cons.head(), Term::small_int(1));
    assert_eq!(cons.tail(), Term::NIL);
    let tuple = Tuple::new(process.x_reg(1)).expect("put_tuple2 creates tuple");
    assert_eq!(tuple.arity(), 3);
    assert_eq!(process.x_reg(2), Term::small_int(1));
}

#[test]
fn put_list_survives_heap_exhaustion_via_gc_and_grow() {
    // Build 200 cons cells on a heap that starts at 233 words (room for ~116
    // cons cells). Without the ensure_space safety net in put_list, the 117th
    // cons cell would fail with HeapFull. With the fix, the heap grows via
    // GC and the full list builds successfully.
    let mut code = Vec::new();
    // Deliberately undercount the reservation: reserve only 4 words.
    code.push(Instruction::TestHeap {
        heap_need: Operand::Unsigned(4),
        live: Operand::Unsigned(0),
    });
    // Seed: x(0) = nil
    code.push(Instruction::Move {
        source: Operand::Atom(None),
        destination: Operand::X(0),
    });
    // Build 200 cons cells: x(0) = [i | x(0)]
    for i in 0..200 {
        code.push(Instruction::PutList {
            head: Operand::Integer(i),
            tail: Operand::X(0),
            destination: Operand::X(0),
        });
    }
    code.push(Instruction::Return);
    let module = module(Atom::OK, code);
    let mut process = Process::new(1, 233);

    let result = run(&mut process, &module);
    assert_eq!(result, Ok(ExecutionResult::Exited(ExitReason::Normal)));

    // Walk the list and verify all 200 elements are present (reversed: 199..0).
    let mut cursor = process.x_reg(0);
    for expected in (0..200).rev() {
        let cons = Cons::new(cursor).expect("list element is cons");
        assert_eq!(cons.head(), Term::small_int(expected));
        cursor = cons.tail();
    }
    assert_eq!(cursor, Term::NIL);
}

#[test]
fn put_tuple2_survives_heap_exhaustion_via_gc_and_grow() {
    // Build a 100-element tuple (101 heap words) on a near-full heap.
    // Without the ensure_space safety net, the alloc would fail with HeapFull.
    let elements: Vec<Operand> = (0..100).map(Operand::Integer).collect();
    let mut code = vec![
        // Fill most of the heap with dummy cons cells first.
        Instruction::TestHeap {
            heap_need: Operand::Unsigned(220),
            live: Operand::Unsigned(0),
        },
    ];
    // Burn 220 words (110 cons cells).
    code.push(Instruction::Move {
        source: Operand::Atom(None),
        destination: Operand::X(0),
    });
    for i in 0..110 {
        code.push(Instruction::PutList {
            head: Operand::Integer(i),
            tail: Operand::X(0),
            destination: Operand::X(0),
        });
    }
    // Now try to build a 100-element tuple — needs 101 words but only ~13
    // remain in the nursery. The ensure_space in put_tuple2 must trigger GC.
    code.push(Instruction::PutTuple2 {
        destination: Operand::X(1),
        elements: Operand::List(elements),
    });
    code.push(Instruction::Return);
    let module = module(Atom::OK, code);
    let mut process = Process::new(1, 233);

    let result = run(&mut process, &module);
    assert_eq!(result, Ok(ExecutionResult::Exited(ExitReason::Normal)));

    let tuple = Tuple::new(process.x_reg(1)).expect("put_tuple2 creates tuple");
    assert_eq!(tuple.arity(), 100);
    assert_eq!(tuple.get(0), Some(Term::small_int(0)));
    assert_eq!(tuple.get(99), Some(Term::small_int(99)));
}

#[test]
fn update_record_copies_tuple_and_applies_pairs() {
    let module = module(
        Atom::OK,
        vec![
            Instruction::PutTuple2 {
                destination: Operand::X(0),
                elements: Operand::List(vec![
                    Operand::Atom(Some(Atom::OK)),
                    Operand::Integer(1),
                    Operand::Integer(2),
                ]),
            },
            Instruction::Move {
                source: Operand::Integer(99),
                destination: Operand::X(2),
            },
            Instruction::UpdateRecord {
                operands: vec![
                    Operand::Atom(Some(Atom::OK)),
                    Operand::Unsigned(3),
                    Operand::X(0),
                    Operand::X(1),
                    Operand::Unsigned(2),
                    Operand::X(2),
                ],
            },
            Instruction::Return,
        ],
    );
    let mut process = Process::new(1, 16);

    assert_eq!(
        run(&mut process, &module),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    let tuple = Tuple::new(process.x_reg(1)).expect("update_record creates tuple");
    assert_eq!(tuple.arity(), 3);
    assert_eq!(tuple.get(0), Some(Term::atom(Atom::OK)));
    assert_eq!(tuple.get(1), Some(Term::small_int(99)));
    assert_eq!(tuple.get(2), Some(Term::small_int(2)));
}

#[test]
fn update_record_applies_multiple_pairs_and_supports_loader_list_shape() {
    let module = module(
        Atom::OK,
        vec![
            Instruction::PutTuple2 {
                destination: Operand::X(0),
                elements: Operand::List(vec![
                    Operand::Atom(Some(Atom::OK)),
                    Operand::Atom(Some(Atom::ERROR)),
                    Operand::Atom(Some(Atom::BADARG)),
                ]),
            },
            Instruction::UpdateRecord {
                operands: vec![
                    Operand::Atom(Some(Atom::OK)),
                    Operand::Unsigned(3),
                    Operand::X(0),
                    Operand::X(1),
                    Operand::List(vec![
                        Operand::Unsigned(1),
                        Operand::Atom(Some(Atom::TRUE)),
                        Operand::Unsigned(3),
                        Operand::Atom(Some(Atom::FALSE)),
                    ]),
                ],
            },
            Instruction::Return,
        ],
    );
    let mut process = Process::new(1, 16);

    assert_eq!(
        run(&mut process, &module),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    let tuple = Tuple::new(process.x_reg(1)).expect("update_record creates tuple");
    assert_eq!(tuple.get(0), Some(Term::atom(Atom::TRUE)));
    assert_eq!(tuple.get(1), Some(Term::atom(Atom::ERROR)));
    assert_eq!(tuple.get(2), Some(Term::atom(Atom::FALSE)));
}

#[test]
fn update_record_without_pairs_allocates_identical_copy() {
    let module = module(
        Atom::OK,
        vec![
            Instruction::PutTuple2 {
                destination: Operand::X(0),
                elements: Operand::List(vec![Operand::Integer(1), Operand::Integer(2)]),
            },
            Instruction::UpdateRecord {
                operands: vec![
                    Operand::Atom(Some(Atom::OK)),
                    Operand::Unsigned(2),
                    Operand::X(0),
                    Operand::X(1),
                ],
            },
            Instruction::Return,
        ],
    );
    let mut process = Process::new(1, 16);

    assert_eq!(
        run(&mut process, &module),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    let updated = Tuple::new(process.x_reg(1)).expect("copied tuple");
    assert_eq!(updated.arity(), 2);
    assert_eq!(updated.get(0), Some(Term::small_int(1)));
    assert_eq!(updated.get(1), Some(Term::small_int(2)));
    assert_ne!(process.x_reg(0).heap_ptr(), process.x_reg(1).heap_ptr());
}

#[test]
fn update_record_rejects_bad_source_and_invalid_operands() {
    let bad_source = module(
        Atom::OK,
        vec![Instruction::UpdateRecord {
            operands: vec![
                Operand::Atom(Some(Atom::OK)),
                Operand::Unsigned(3),
                Operand::Integer(1),
                Operand::X(0),
                Operand::Unsigned(1),
                Operand::Integer(2),
            ],
        }],
    );
    assert_eq!(
        run(&mut Process::new(1, 16), &bad_source),
        Err(ExecError::Badarg)
    );

    let invalid_pairs = module(
        Atom::OK,
        vec![Instruction::UpdateRecord {
            operands: vec![
                Operand::Atom(Some(Atom::OK)),
                Operand::Unsigned(3),
                Operand::X(0),
                Operand::X(1),
                Operand::Unsigned(1),
            ],
        }],
    );
    assert_eq!(
        run(&mut Process::new(1, 16), &invalid_pairs),
        Err(ExecError::InvalidOperand("update_record pairs"))
    );
}

#[test]
fn update_record_survives_gc_before_allocation() {
    let module = module(
        Atom::OK,
        vec![
            Instruction::PutTuple2 {
                destination: Operand::X(0),
                elements: Operand::List(vec![
                    Operand::Integer(1),
                    Operand::Integer(2),
                    Operand::Integer(3),
                ]),
            },
            Instruction::PutTuple2 {
                destination: Operand::X(2),
                elements: Operand::List(vec![Operand::Integer(9), Operand::Integer(8)]),
            },
            Instruction::UpdateRecord {
                operands: vec![
                    Operand::Atom(Some(Atom::OK)),
                    Operand::Unsigned(3),
                    Operand::X(0),
                    Operand::X(1),
                    Operand::Unsigned(2),
                    Operand::X(2),
                ],
            },
            Instruction::Return,
        ],
    );
    let mut process = Process::new(1, 7);

    assert_eq!(
        run(&mut process, &module),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert!(process.heap().old_used() > 0);
    let tuple = Tuple::new(process.x_reg(1)).expect("updated tuple survives gc");
    assert_eq!(tuple.get(0), Some(Term::small_int(1)));
    assert_eq!(tuple.get(2), Some(Term::small_int(3)));
    let nested = Tuple::new(tuple.get(1).expect("nested update value")).expect("nested tuple");
    assert_eq!(nested.get(0), Some(Term::small_int(9)));
    assert_eq!(nested.get(1), Some(Term::small_int(8)));
}

#[test]
fn bad_tuple_access_and_heap_exhaustion_report_errors() {
    let bad_tuple = module(
        Atom::OK,
        vec![Instruction::GetTupleElement {
            source: Operand::Integer(1),
            index: Operand::Unsigned(0),
            destination: Operand::X(0),
        }],
    );
    assert_eq!(
        run(&mut Process::new(1, 8), &bad_tuple),
        Err(ExecError::Badarg)
    );

    let heap_check = module(
        Atom::OK,
        vec![
            Instruction::TestHeap {
                heap_need: Operand::Unsigned(10),
                live: Operand::Unsigned(0),
            },
            Instruction::Return,
        ],
    );
    let mut process = Process::new(1, 8);
    assert_eq!(
        run(&mut process, &heap_check),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert!(process.heap().available() >= 10);
}

fn add_one(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [value] = args else {
        return Err(Term::atom(Atom::BADARG));
    };
    let Some(value) = value.as_small_int() else {
        return Err(Term::atom(Atom::BADARG));
    };
    Ok(Term::small_int(value + 1))
}

fn native_error_without_class(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [reason] = args else {
        return Err(Term::atom(Atom::BADARG));
    };
    Err(*reason)
}

fn native_throw(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [reason] = args else {
        return Err(Term::atom(Atom::BADARG));
    };
    context.set_exception_class(ExceptionClass::Throw);
    Err(*reason)
}

fn native_exit(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [reason] = args else {
        return Err(Term::atom(Atom::BADARG));
    };
    context.set_exception_class(ExceptionClass::Exit);
    Err(*reason)
}

fn native_import(function: crate::native::NativeFn) -> ResolvedImport {
    ResolvedImport {
        module: Atom::OK,
        function: Atom::OK,
        arity: 1,
        target: ResolvedImportTarget::Native(NativeEntry {
            function,
            dirty_kind: None,
            capability: Capability::Pure,
        }),
    }
}

fn native_increment(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let value = args[0].as_small_int().unwrap_or(0);
    Ok(Term::small_int(value + 1))
}

fn native_increment_module(capability: Capability) -> Module {
    let mut module = module(
        Atom::OK,
        vec![
            Instruction::Move {
                source: Operand::Integer(7),
                destination: Operand::X(0),
            },
            Instruction::CallExt {
                arity: Operand::Unsigned(1),
                import: Operand::Unsigned(0),
            },
            Instruction::Return,
        ],
    );
    module.resolved_imports.push(ResolvedImport {
        module: Atom::OK,
        function: Atom::OK,
        arity: 1,
        target: ResolvedImportTarget::Native(NativeEntry {
            function: native_increment,
            dirty_kind: None,
            capability,
        }),
    });
    module
}

#[test]
fn audit_sink_receives_granted_native_call_event() {
    let module = native_increment_module(Capability::Pure);
    let registry = ModuleRegistry::new();
    let mut process = Process::new(41, 32);
    let sink = Arc::new(CollectingAuditSink::default());
    let audit_sink: Arc<dyn CapabilityAuditSink> = sink.clone();
    let services = NativeServices {
        capability_audit_sink: Some(audit_sink),
        ..empty_native_services()
    };

    assert_eq!(
        run_with_native_services(&mut process, &module, &registry, &services),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    let events = sink.events.lock().expect("collector lock");
    assert_eq!(events.len(), 1);
    let event = &events[0];
    assert_eq!(event.pid, 41);
    assert_eq!(event.capability, Capability::Pure);
    assert_eq!(event.operation.module, Atom::OK);
    assert_eq!(event.operation.function, Atom::OK);
    assert_eq!(event.operation.arity, 1);
    assert!(event.granted);
    assert_eq!(event.process_capabilities, CapabilitySet::all());
}

#[test]
fn audit_sink_receives_denied_native_call_event() {
    let module = native_increment_module(Capability::ExternalIo);
    let registry = ModuleRegistry::new();
    let capabilities = CapabilitySet::from_slice(&[Capability::Pure, Capability::ProcessLocal]);
    let mut process = Process::with_capabilities(42, 32, capabilities.clone());
    let sink = Arc::new(CollectingAuditSink::default());
    let audit_sink: Arc<dyn CapabilityAuditSink> = sink.clone();
    let services = NativeServices {
        capability_audit_sink: Some(audit_sink),
        ..empty_native_services()
    };

    assert_eq!(
        run_with_native_services(&mut process, &module, &registry, &services),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    let events = sink.events.lock().expect("collector lock");
    assert_eq!(events.len(), 1);
    let event = &events[0];
    assert_eq!(event.pid, 42);
    assert_eq!(event.capability, Capability::ExternalIo);
    assert_eq!(event.operation.module, Atom::OK);
    assert_eq!(event.operation.function, Atom::OK);
    assert_eq!(event.operation.arity, 1);
    assert!(!event.granted);
    assert_eq!(event.process_capabilities, capabilities);
}

#[test]
fn custom_violation_handler_receives_denied_native_call_context() {
    let module = native_increment_module(Capability::ExternalIo);
    let registry = ModuleRegistry::new();
    let capabilities = CapabilitySet::from_slice(&[Capability::Pure, Capability::ProcessLocal]);
    let mut process = Process::with_capabilities(43, 32, capabilities.clone());
    let handler = Arc::new(CollectingViolationHandler::default());
    let violation_handler: Arc<dyn ViolationHandler> = handler.clone();
    let services = NativeServices {
        capability_violation_handler: Some(violation_handler),
        ..empty_native_services()
    };

    assert_eq!(
        run_with_native_services(&mut process, &module, &registry, &services),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    let events = handler.events.lock().expect("collector lock");
    assert_eq!(events.len(), 1);
    let event = &events[0];
    assert_eq!(event.pid, 43);
    assert_eq!(event.capability, Capability::ExternalIo);
    assert_eq!(event.operation.module, Atom::OK);
    assert_eq!(event.operation.function, Atom::OK);
    assert_eq!(event.operation.arity, 1);
    assert!(!event.granted);
    assert_eq!(event.process_capabilities, capabilities);
}

#[test]
fn stderr_violation_handler_accepts_denied_native_call_context() {
    let event = CapabilityAuditEvent {
        pid: 44,
        capability: Capability::ExternalIo,
        operation: crate::capability::CapabilityOperation {
            module: Atom::OK,
            function: Atom::OK,
            arity: 1,
        },
        granted: false,
        process_capabilities: CapabilitySet::from_slice(&[
            Capability::Pure,
            Capability::ProcessLocal,
        ]),
    };

    StderrViolationHandler.on_violation(event);
}

#[test]
fn dirty_native_returns_dirty_call_without_inline_execution() {
    let mut module = module(
        Atom::OK,
        vec![
            Instruction::Move {
                source: Operand::Integer(7),
                destination: Operand::X(0),
            },
            Instruction::CallExt {
                arity: Operand::Unsigned(1),
                import: Operand::Unsigned(0),
            },
            Instruction::Return,
        ],
    );
    let entry = NativeEntry {
        function: native_increment,
        dirty_kind: Some(DirtySchedulerKind::Cpu),
        capability: Capability::Pure,
    };
    module.resolved_imports.push(ResolvedImport {
        module: Atom::OK,
        function: Atom::OK,
        arity: 1,
        target: ResolvedImportTarget::Native(entry),
    });
    let mut process = Process::new(1, 32);

    let result = run(&mut process, &module).expect("dirty native yields");
    let ExecutionResult::DirtyCall {
        entry, args, kind, ..
    } = result
    else {
        panic!("expected dirty call, got {result:?}");
    };
    assert_eq!(entry.dirty_kind, Some(DirtySchedulerKind::Cpu));
    assert_eq!(args, vec![Term::small_int(7)]);
    assert_eq!(kind, DirtySchedulerKind::Cpu);
    assert_eq!(process.x_reg(0), Term::small_int(7));
}

#[test]
fn pure_sandbox_denies_native_calls() {
    let mut module = module(
        Atom::OK,
        vec![
            Instruction::Move {
                source: Operand::Integer(7),
                destination: Operand::X(0),
            },
            Instruction::CallExt {
                arity: Operand::Unsigned(1),
                import: Operand::Unsigned(0),
            },
            Instruction::Return,
        ],
    );
    module.resolved_imports.push(ResolvedImport {
        module: Atom::OK,
        function: Atom::OK,
        arity: 1,
        target: ResolvedImportTarget::Native(NativeEntry {
            function: native_increment,
            dirty_kind: Some(DirtySchedulerKind::Io),
            capability: Capability::ExternalIo,
        }),
    });
    let mut process = Process::with_capabilities(1, 32, Sandbox::Pure.capabilities());

    assert_eq!(
        run(&mut process, &module),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    let tuple = Tuple::new(process.x_reg(0)).expect("capability denied tuple");
    assert_eq!(tuple.arity(), 2);
    assert_eq!(tuple.get(0), Some(Term::atom(Atom::ERROR)));
    assert_eq!(tuple.get(1), Some(Term::atom(Atom::CAPABILITY_DENIED)));
}

#[test]
fn native_call_denied_by_process_capability_returns_error_tuple() {
    let mut module = module(
        Atom::OK,
        vec![
            Instruction::Move {
                source: Operand::Integer(7),
                destination: Operand::X(0),
            },
            Instruction::CallExt {
                arity: Operand::Unsigned(1),
                import: Operand::Unsigned(0),
            },
            Instruction::Return,
        ],
    );
    module.resolved_imports.push(ResolvedImport {
        module: Atom::OK,
        function: Atom::OK,
        arity: 1,
        target: ResolvedImportTarget::Native(NativeEntry {
            function: native_increment,
            dirty_kind: Some(DirtySchedulerKind::Io),
            capability: Capability::ExternalIo,
        }),
    });
    let mut process = Process::with_capabilities(
        1,
        32,
        CapabilitySet::from_slice(&[Capability::Pure, Capability::ProcessLocal]),
    );

    assert_eq!(
        run(&mut process, &module),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    let tuple = Tuple::new(process.x_reg(0)).expect("capability denied tuple");
    assert_eq!(tuple.arity(), 2);
    assert_eq!(tuple.get(0), Some(Term::atom(Atom::ERROR)));
    assert_eq!(tuple.get(1), Some(Term::atom(Atom::CAPABILITY_DENIED)));
}

fn try_native_class_module(function: crate::native::NativeFn, expected_class: Atom) -> Module {
    let mut module = module(
        Atom::OK,
        vec![
            Instruction::Try {
                destination: Operand::X(3),
                label: Operand::Label(10),
            },
            Instruction::Move {
                source: Operand::Integer(42),
                destination: Operand::X(0),
            },
            Instruction::CallExt {
                arity: Operand::Unsigned(1),
                import: Operand::Unsigned(0),
            },
            Instruction::TryEnd {
                source: Operand::X(3),
            },
            Instruction::Move {
                source: Operand::Integer(-100),
                destination: Operand::X(0),
            },
            Instruction::Return,
            Instruction::Label { label: 10 },
            Instruction::TryCase {
                source: Operand::X(3),
            },
            Instruction::SelectVal {
                value: Operand::X(0),
                fail: Operand::Label(99),
                list: Operand::List(vec![
                    Operand::Atom(Some(expected_class)),
                    Operand::Label(11),
                ]),
            },
            Instruction::Label { label: 11 },
            Instruction::Move {
                source: Operand::X(1),
                destination: Operand::X(0),
            },
            Instruction::Return,
            Instruction::Label { label: 99 },
            Instruction::Raise {
                stacktrace: Operand::X(2),
                reason: Operand::X(1),
            },
        ],
    );
    module.resolved_imports.push(native_import(function));
    module
}

#[test]
fn call_ext_invokes_registered_native_and_tail_call_deallocates() {
    let import = ResolvedImport {
        module: Atom::OK,
        function: Atom::OK,
        arity: 1,
        target: ResolvedImportTarget::Native(NativeEntry {
            function: add_one,
            dirty_kind: None,
            capability: Capability::Pure,
        }),
    };
    let mut module = module(
        Atom::OK,
        vec![
            Instruction::Move {
                source: Operand::Integer(41),
                destination: Operand::X(0),
            },
            Instruction::CallExt {
                arity: Operand::Unsigned(1),
                import: Operand::Unsigned(0),
            },
            Instruction::Return,
        ],
    );
    module.resolved_imports.push(import);
    let mut process = Process::new(1, 32);

    assert_eq!(
        run(&mut process, &module),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert_eq!(process.x_reg(0), Term::small_int(42));
    assert_eq!(process.stack().len(), 0);
}

#[test]
fn try_catches_throw_class_from_native_err() {
    let module = try_native_class_module(native_throw, Atom::THROW);
    let mut process = Process::new(1, 32);

    assert_eq!(
        run(&mut process, &module),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert_eq!(process.x_reg(0), Term::small_int(42));
}

#[test]
fn try_error_clause_does_not_catch_throw_class_from_native_err() {
    let module = try_native_class_module(native_throw, Atom::ERROR);
    let mut process = Process::new(1, 32);

    assert_eq!(
        run(&mut process, &module),
        Ok(ExecutionResult::Exited(ExitReason::Error))
    );
    let exception = process.current_exception().expect("propagated throw");
    assert_eq!(exception.class, Term::atom(Atom::THROW));
    assert_eq!(exception.reason, Term::small_int(42));
}

#[test]
fn try_catches_exit_class_from_native_err() {
    let module = try_native_class_module(native_exit, Atom::EXIT_CLASS);
    let mut process = Process::new(1, 32);

    assert_eq!(
        run(&mut process, &module),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert_eq!(process.x_reg(0), Term::small_int(42));
}

#[test]
fn native_err_without_exception_class_uses_error_class() {
    let mut module = module(
        Atom::OK,
        vec![
            Instruction::Move {
                source: Operand::Integer(7),
                destination: Operand::X(0),
            },
            Instruction::CallExt {
                arity: Operand::Unsigned(1),
                import: Operand::Unsigned(0),
            },
            Instruction::Return,
        ],
    );
    module
        .resolved_imports
        .push(native_import(native_error_without_class));
    let mut process = Process::new(1, 32);

    assert_eq!(
        run(&mut process, &module),
        Ok(ExecutionResult::Exited(ExitReason::Error))
    );
    let exception = process.current_exception().expect("native Err exception");
    assert_eq!(exception.class, Term::atom(Atom::ERROR));
    assert_eq!(exception.reason, Term::small_int(7));
}

fn exported_value_module(
    module_atom: Atom,
    function_atom: Atom,
    label: u32,
    padding: usize,
    value: i64,
) -> Module {
    let mut code = Vec::new();
    for _ in 0..padding {
        code.push(Instruction::Move {
            source: Operand::Integer(-1),
            destination: Operand::X(1),
        });
    }
    code.push(Instruction::Label { label });
    code.push(Instruction::Move {
        source: Operand::Integer(value),
        destination: Operand::X(0),
    });
    code.push(Instruction::Return);

    let mut module = module(module_atom, code);
    module.exports.insert((function_atom, 0), label);
    module
}

fn call_ext_caller(
    caller_atom: Atom,
    target_module: Atom,
    function_atom: Atom,
    target: ResolvedImportTarget,
) -> Module {
    let mut caller = module(
        caller_atom,
        vec![
            Instruction::CallExt {
                arity: Operand::Unsigned(0),
                import: Operand::Unsigned(0),
            },
            Instruction::Return,
        ],
    );
    caller.resolved_imports.push(ResolvedImport {
        module: target_module,
        function: function_atom,
        arity: 0,
        target,
    });
    caller
}

#[test]
fn call_ext_unresolved_target_returns_undef_without_index_drift() {
    let atoms = AtomTable::new();
    let caller_atom = atoms.intern("caller");
    let first_target = atoms.intern("first_target");
    let second_target = atoms.intern("second_target");
    let missing_atom = atoms.intern("missing");
    let foo_atom = atoms.intern("foo");
    let registry = ModuleRegistry::new();
    registry.insert(exported_value_module(second_target, foo_atom, 7, 0, 9));

    let mut caller = module(
        caller_atom,
        vec![
            Instruction::CallExt {
                arity: Operand::Unsigned(0),
                import: Operand::Unsigned(1),
            },
            Instruction::Return,
        ],
    );
    caller.resolved_imports.push(ResolvedImport {
        module: first_target,
        function: missing_atom,
        arity: 0,
        target: ResolvedImportTarget::Unresolved {
            module: first_target,
            function: missing_atom,
            arity: 0,
        },
    });
    caller.resolved_imports.push(ResolvedImport {
        module: second_target,
        function: foo_atom,
        arity: 0,
        target: ResolvedImportTarget::Code {
            module: second_target,
            label: 7,
        },
    });
    let caller = registry.insert(caller);
    let mut process = Process::new(1, 32);

    assert_eq!(
        run_with_registry(&mut process, &caller, &registry),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert_eq!(process.x_reg(0), Term::small_int(9));
}

#[test]
fn call_ext_unresolved_target_returns_undef() {
    let atoms = AtomTable::new();
    let caller_atom = atoms.intern("caller");
    let target_atom = atoms.intern("target");
    let missing_atom = atoms.intern("missing");
    let registry = ModuleRegistry::new();
    let caller = registry.insert(call_ext_caller(
        caller_atom,
        target_atom,
        missing_atom,
        ResolvedImportTarget::Unresolved {
            module: target_atom,
            function: missing_atom,
            arity: 0,
        },
    ));
    let mut process = Process::new(1, 32);

    assert!(matches!(
        run_with_registry(&mut process, &caller, &registry),
        Err(ExecError::Undef {
            module,
            function,
            arity: 0,
        }) if module == target_atom && function == missing_atom
    ));
}

#[test]
fn call_ext_denied_target_returns_mfa_rich_undef() {
    let atoms = AtomTable::new();
    let caller_atom = atoms.intern("caller");
    let target_atom = atoms.intern("meridian_ffi");
    let run_cmd_atom = atoms.intern("run_cmd");
    let registry = ModuleRegistry::new();
    let caller = registry.insert(call_ext_caller(
        caller_atom,
        target_atom,
        run_cmd_atom,
        ResolvedImportTarget::Denied {
            capability: Capability::ExternalIo,
        },
    ));
    let mut process = Process::new(1, 32);

    assert!(matches!(
        run_with_registry(&mut process, &caller, &registry),
        Err(ExecError::Undef {
            module,
            function,
            arity: 0,
        }) if module == target_atom && function == run_cmd_atom
    ));
}

#[test]
fn call_ext_code_target_uses_latest_export_ip_after_reload() {
    let atoms = AtomTable::new();
    let caller_atom = atoms.intern("caller");
    let target_atom = atoms.intern("target");
    let foo_atom = atoms.intern("foo");
    let registry = ModuleRegistry::new();

    registry.insert(exported_value_module(target_atom, foo_atom, 5, 0, 1));
    let caller = registry.insert(call_ext_caller(
        caller_atom,
        target_atom,
        foo_atom,
        ResolvedImportTarget::Code {
            module: target_atom,
            label: 5,
        },
    ));
    let mut first_process = Process::new(1, 32);

    assert_eq!(
        run_with_registry(&mut first_process, &caller, &registry),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert_eq!(first_process.x_reg(0), Term::small_int(1));

    registry.insert(exported_value_module(target_atom, foo_atom, 12, 4, 2));
    let mut second_process = Process::new(2, 32);

    assert_eq!(
        run_with_registry(&mut second_process, &caller, &registry),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert_eq!(second_process.x_reg(0), Term::small_int(2));
}

#[test]
fn call_ext_deferred_target_resolves_when_module_loads_later() {
    let atoms = AtomTable::new();
    let caller_atom = atoms.intern("caller");
    let target_atom = atoms.intern("target");
    let foo_atom = atoms.intern("foo");
    let registry = ModuleRegistry::new();
    let caller = registry.insert(call_ext_caller(
        caller_atom,
        target_atom,
        foo_atom,
        ResolvedImportTarget::Deferred {
            module: target_atom,
            function: foo_atom,
            arity: 0,
        },
    ));

    let mut missing_process = Process::new(1, 32);
    assert!(matches!(
        run_with_registry(&mut missing_process, &caller, &registry),
        Err(ExecError::Undef {
            module,
            function,
            arity: 0,
        }) if module == target_atom && function == foo_atom
    ));

    registry.insert(exported_value_module(target_atom, foo_atom, 12, 3, 7));
    let mut loaded_process = Process::new(2, 32);

    assert_eq!(
        run_with_registry(&mut loaded_process, &caller, &registry),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert_eq!(loaded_process.x_reg(0), Term::small_int(7));
}

fn add(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [left, right] = args else {
        return Err(Term::atom(Atom::BADARG));
    };
    let Some(left) = left.as_small_int() else {
        return Err(Term::atom(Atom::BADARG));
    };
    let Some(right) = right.as_small_int() else {
        return Err(Term::atom(Atom::BADARG));
    };
    Ok(Term::small_int(left + right))
}

fn native_add_import() -> ResolvedImport {
    ResolvedImport {
        module: Atom::OK,
        function: Atom::OK,
        arity: 2,
        target: ResolvedImportTarget::Native(NativeEntry {
            function: add,
            dirty_kind: None,
            capability: Capability::Pure,
        }),
    }
}

#[test]
fn call_ext_only_native_tail_call_exits_with_bif_result() {
    let mut module = module(
        Atom::OK,
        vec![
            Instruction::Move {
                source: Operand::Integer(40),
                destination: Operand::X(0),
            },
            Instruction::Move {
                source: Operand::Integer(2),
                destination: Operand::X(1),
            },
            Instruction::CallExtOnly {
                arity: Operand::Unsigned(2),
                import: Operand::Unsigned(0),
            },
        ],
    );
    module.resolved_imports.push(native_add_import());
    let mut process = Process::new(1, 32);

    assert_eq!(
        run(&mut process, &module),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert_eq!(process.x_reg(0), Term::small_int(42));
    assert_eq!(process.stack().len(), 0);
}

#[test]
fn call_ext_only_native_tail_call_does_not_fall_through_to_next_function() {
    let mut module = module(
        Atom::OK,
        vec![
            Instruction::Move {
                source: Operand::Integer(1),
                destination: Operand::X(0),
            },
            Instruction::Move {
                source: Operand::Integer(2),
                destination: Operand::X(1),
            },
            Instruction::CallExtOnly {
                arity: Operand::Unsigned(2),
                import: Operand::Unsigned(0),
            },
            Instruction::Label { label: 99 },
            Instruction::Move {
                source: Operand::Integer(99),
                destination: Operand::X(0),
            },
            Instruction::Return,
        ],
    );
    module.resolved_imports.push(native_add_import());
    let mut process = Process::new(1, 32);

    assert_eq!(
        run(&mut process, &module),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert_eq!(process.x_reg(0), Term::small_int(3));
}

#[test]
fn nested_tail_calls_propagate_native_bif_result() {
    let mut module = module(
        Atom::OK,
        vec![
            Instruction::Move {
                source: Operand::Integer(20),
                destination: Operand::X(0),
            },
            Instruction::Move {
                source: Operand::Integer(22),
                destination: Operand::X(1),
            },
            Instruction::CallOnly {
                arity: Operand::Unsigned(2),
                label: Operand::Label(10),
            },
            Instruction::Label { label: 10 },
            Instruction::CallExtOnly {
                arity: Operand::Unsigned(2),
                import: Operand::Unsigned(0),
            },
        ],
    );
    module.resolved_imports.push(native_add_import());
    let mut process = Process::new(1, 32);

    assert_eq!(
        run(&mut process, &module),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert_eq!(process.x_reg(0), Term::small_int(42));
}

#[test]
fn call_ext_last_native_tail_call_deallocates_then_returns_to_caller() {
    let mut module = module(
        Atom::OK,
        vec![
            Instruction::Call {
                arity: Operand::Unsigned(0),
                label: Operand::Label(10),
            },
            Instruction::Move {
                source: Operand::Integer(7),
                destination: Operand::X(1),
            },
            Instruction::Return,
            Instruction::Label { label: 10 },
            Instruction::Allocate {
                stack_need: Operand::Unsigned(1),
                live: Operand::Unsigned(0),
            },
            Instruction::Move {
                source: Operand::Integer(40),
                destination: Operand::X(0),
            },
            Instruction::Move {
                source: Operand::Integer(2),
                destination: Operand::X(1),
            },
            Instruction::CallExtLast {
                arity: Operand::Unsigned(2),
                import: Operand::Unsigned(0),
                deallocate: Operand::Unsigned(1),
            },
            Instruction::Move {
                source: Operand::Integer(99),
                destination: Operand::X(0),
            },
        ],
    );
    module.resolved_imports.push(native_add_import());
    let mut process = Process::new(1, 32);

    assert_eq!(
        run(&mut process, &module),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert_eq!(process.x_reg(0), Term::small_int(42));
    assert_eq!(process.x_reg(1), Term::small_int(7));
    assert_eq!(process.stack().len(), 0);
}

#[test]
fn branching_opcode_sequence_dispatches_like_case_expression() {
    let module = module(
        Atom::OK,
        vec![
            Instruction::PutTuple2 {
                destination: Operand::X(0),
                elements: Operand::List(vec![Operand::Atom(Some(Atom::OK)), Operand::Integer(42)]),
            },
            Instruction::TypeTest {
                op: crate::loader::decode::TypeTestOp::IsTuple,
                fail: Operand::Label(99),
                value: Operand::X(0),
            },
            Instruction::SelectTupleArity {
                value: Operand::X(0),
                fail: Operand::Label(99),
                list: Operand::List(vec![
                    Operand::Unsigned(2),
                    Operand::Label(10),
                    Operand::Unsigned(3),
                    Operand::Label(11),
                ]),
            },
            Instruction::Label { label: 10 },
            Instruction::Move {
                source: Operand::Integer(1),
                destination: Operand::X(1),
            },
            Instruction::Jump {
                target: Operand::Label(100),
            },
            Instruction::Label { label: 11 },
            Instruction::Move {
                source: Operand::Integer(2),
                destination: Operand::X(1),
            },
            Instruction::Jump {
                target: Operand::Label(100),
            },
            Instruction::Label { label: 99 },
            Instruction::Move {
                source: Operand::Integer(-1),
                destination: Operand::X(1),
            },
            Instruction::Label { label: 100 },
            Instruction::Return,
        ],
    );
    let mut process = Process::new(1, 32);

    assert_eq!(
        run(&mut process, &module),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert_eq!(process.x_reg(1), Term::small_int(1));
}

#[test]
fn select_val_and_comparison_sequence_dispatches_like_guarded_case_expression() {
    let module = module(
        Atom::OK,
        vec![
            Instruction::Move {
                source: Operand::Atom(Some(Atom::ERROR)),
                destination: Operand::X(0),
            },
            Instruction::SelectVal {
                value: Operand::X(0),
                fail: Operand::Label(99),
                list: Operand::List(vec![
                    Operand::Atom(Some(Atom::OK)),
                    Operand::Label(10),
                    Operand::Atom(Some(Atom::ERROR)),
                    Operand::Label(11),
                ]),
            },
            Instruction::Label { label: 10 },
            Instruction::Move {
                source: Operand::Integer(1),
                destination: Operand::X(1),
            },
            Instruction::Jump {
                target: Operand::Label(100),
            },
            Instruction::Label { label: 11 },
            Instruction::Comparison {
                op: crate::loader::decode::ComparisonOp::EqExact,
                fail: Operand::Label(99),
                left: Operand::X(0),
                right: Operand::Atom(Some(Atom::ERROR)),
            },
            Instruction::Move {
                source: Operand::Integer(2),
                destination: Operand::X(1),
            },
            Instruction::Jump {
                target: Operand::Label(100),
            },
            Instruction::Label { label: 99 },
            Instruction::Move {
                source: Operand::Integer(-1),
                destination: Operand::X(1),
            },
            Instruction::Label { label: 100 },
            Instruction::Return,
        ],
    );
    let mut process = Process::new(1, 16);

    assert_eq!(
        run(&mut process, &module),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert_eq!(process.x_reg(1), Term::small_int(2));
    assert!(compare::exact_eq(process.x_reg(0), Term::atom(Atom::ERROR)));
}

#[test]
fn guard_bif_failure_branches_without_exiting_process() {
    let mut module = module(
        Atom::OK,
        vec![
            Instruction::Bif {
                op: crate::loader::decode::BifOp::GcBif2,
                operands: vec![
                    Operand::Label(9),
                    Operand::Unsigned(0),
                    Operand::Atom(Some(Atom::OK)),
                    Operand::Integer(1),
                    Operand::X(0),
                ],
            },
            Instruction::Move {
                source: Operand::Integer(1),
                destination: Operand::X(1),
            },
            Instruction::Return,
            Instruction::Label { label: 9 },
            Instruction::Move {
                source: Operand::Integer(99),
                destination: Operand::X(1),
            },
            Instruction::Return,
        ],
    );
    module.resolved_imports.push(ResolvedImport {
        module: Atom::OK,
        function: Atom::OK,
        arity: 2,
        target: ResolvedImportTarget::Native(NativeEntry {
            function: add,
            dirty_kind: None,
            capability: Capability::Pure,
        }),
    });
    let mut process = Process::new(1, 16);

    assert_eq!(
        run(&mut process, &module),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert_eq!(process.x_reg(1), Term::small_int(99));
}

#[test]
fn unknown_opcode_reports_opcode_number() {
    let module = module(
        Atom::OK,
        vec![Instruction::Generic {
            opcode: 222,
            name: "mystery",
            operands: Vec::new(),
        }],
    );
    assert_eq!(
        run(&mut Process::new(1, 8), &module),
        Err(ExecError::UnknownOpcode { opcode: 222 })
    );
}

#[test]
fn call_ext_bif_error_preserves_exception_class_and_stacktrace() {
    let mut module = module(
        Atom::OK,
        vec![
            Instruction::Try {
                destination: Operand::X(10),
                label: Operand::Label(20),
            },
            Instruction::Move {
                source: Operand::Atom(Some(Atom::THROW)),
                destination: Operand::X(0),
            },
            Instruction::Move {
                source: Operand::Atom(Some(Atom::BADMATCH)),
                destination: Operand::X(1),
            },
            Instruction::Move {
                source: Operand::Integer(321),
                destination: Operand::X(2),
            },
            Instruction::CallExt {
                arity: Operand::Unsigned(3),
                import: Operand::Unsigned(0),
            },
            Instruction::Return,
            Instruction::Label { label: 20 },
            Instruction::TryCase {
                source: Operand::X(10),
            },
            Instruction::Return,
        ],
    );
    module.resolved_imports.push(ResolvedImport {
        module: Atom::OK,
        function: Atom::OK,
        arity: 3,
        target: ResolvedImportTarget::Native(NativeEntry {
            function: crate::native::exception_bifs::bif_raise_3,
            dirty_kind: None,
            capability: Capability::Pure,
        }),
    });
    let mut process = Process::new(1, 64);

    assert_eq!(
        run(&mut process, &module),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert_eq!(process.x_reg(0), Term::atom(Atom::THROW));
    assert_eq!(process.x_reg(1), Term::atom(Atom::BADMATCH));
    assert_eq!(process.x_reg(2), Term::small_int(321));
}

#[test]
fn proof_of_life_load_spawn_execute_exit_pipeline_fixture() {
    let atoms = AtomTable::new();
    let module_atom = atoms.intern("gleam_fib_fixture");
    let fib_atom = atoms.intern("fib");
    let mut module = module(
        module_atom,
        vec![
            Instruction::Label { label: 1 },
            Instruction::FuncInfo {
                module: Operand::Atom(Some(module_atom)),
                function: Operand::Atom(Some(fib_atom)),
                arity: Operand::Unsigned(1),
            },
            Instruction::Move {
                source: Operand::Integer(55),
                destination: Operand::X(0),
            },
            Instruction::Return,
        ],
    );
    module.exports.insert((fib_atom, 1), 1);
    let mut process = Process::new(42, 32);
    process.set_x_reg(0, Term::small_int(10));
    process.set_code_position(Some(CodePosition {
        module: module_atom,
        instruction_pointer: 0,
    }));

    assert_eq!(
        run(&mut process, &module),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert_eq!(process.x_reg(0), Term::small_int(55));
}

#[test]
fn interpreter_binary_opcodes_construct_and_match_binary_patterns() {
    let construct_module = module(
        Atom::OK,
        vec![
            Instruction::BinaryOp {
                op: BinaryOp::BsCreateBin,
                operands: vec![
                    Operand::X(0),
                    Operand::Unsigned(3),
                    Operand::List(vec![
                        Operand::Atom(None),
                        Operand::Integer(65),
                        Operand::Unsigned(8),
                        Operand::Unsigned(1),
                        Operand::Atom(None),
                    ]),
                    Operand::List(vec![
                        Operand::Atom(None),
                        Operand::Integer(66),
                        Operand::Unsigned(8),
                        Operand::Unsigned(1),
                        Operand::Atom(None),
                    ]),
                    Operand::List(vec![
                        Operand::Atom(None),
                        Operand::Integer(67),
                        Operand::Unsigned(8),
                        Operand::Unsigned(1),
                        Operand::Atom(None),
                    ]),
                ],
            },
            Instruction::Return,
        ],
    );
    let mut process = Process::new(1, 64);

    assert_eq!(
        run(&mut process, &construct_module),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert_eq!(
        Binary::new(process.x_reg(0))
            .expect("constructed binary")
            .as_bytes(),
        &[65, 66, 67]
    );

    let module = module(
        Atom::OK,
        vec![
            Instruction::BinaryOp {
                op: BinaryOp::BsStartMatch3,
                operands: vec![Operand::Label(9), Operand::X(0), Operand::X(1)],
            },
            Instruction::BinaryOp {
                op: BinaryOp::BsGetInteger2,
                operands: vec![
                    Operand::Label(9),
                    Operand::X(1),
                    Operand::Unsigned(8),
                    Operand::Unsigned(1),
                    Operand::Atom(None),
                    Operand::X(2),
                ],
            },
            Instruction::BinaryOp {
                op: BinaryOp::BsGetInteger2,
                operands: vec![
                    Operand::Label(9),
                    Operand::X(1),
                    Operand::Unsigned(8),
                    Operand::Unsigned(1),
                    Operand::Atom(None),
                    Operand::X(3),
                ],
            },
            Instruction::BinaryOp {
                op: BinaryOp::BsGetBinary2,
                operands: vec![
                    Operand::Label(9),
                    Operand::X(1),
                    Operand::Unsigned(8),
                    Operand::Unsigned(1),
                    Operand::Atom(None),
                    Operand::X(4),
                ],
            },
            Instruction::BinaryOp {
                op: BinaryOp::BsTestTail2,
                operands: vec![Operand::Label(9), Operand::X(1), Operand::Unsigned(0)],
            },
            Instruction::Return,
            Instruction::Label { label: 9 },
            Instruction::Move {
                source: Operand::Integer(-1),
                destination: Operand::X(2),
            },
            Instruction::Return,
        ],
    );
    let mut process = Process::new(1, 96);
    let source = heap_binary(&mut process, &[65, 66, 67]);
    process.set_x_reg(0, source);

    assert_eq!(
        run(&mut process, &module),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert_eq!(process.x_reg(2).as_small_int(), Some(65));
    assert_eq!(process.x_reg(3).as_small_int(), Some(66));
    assert_eq!(
        Binary::new(process.x_reg(4)).expect("rest").as_bytes(),
        &[67]
    );
}

#[test]
fn alloc_list_fun_entries_reserve_the_full_closure_base() {
    use crate::interpreter::opcodes::closures::CLOSURE_BASE_WORDS;
    use crate::loader::decode::compact::Allocation;

    // `{test_heap, {alloc, [{words,3},{funs,1}]}, Live}` must leave room for
    // a subsequent make_fun (closure base; free-variable words arrive in the
    // words component) plus the listed words, or the follow-up put_tuple2
    // starves mid-instruction-sequence.
    let mut process = Process::new(1, 2);
    crate::interpreter::opcodes::core::test_heap(
        &mut process,
        &Operand::Allocation(vec![Allocation::Words(3), Allocation::Funs(1)]),
        &Operand::Unsigned(0),
    )
    .expect("test_heap grows the heap");
    assert!(
        process.heap().available() >= 3 + CLOSURE_BASE_WORDS,
        "alloc list must reserve words + full closure base, available: {}",
        process.heap().available()
    );
}
