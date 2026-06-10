//! Process-level replay debugger with deterministic single-instruction stepping.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::atom::{Atom, AtomTable};
use crate::error::ExecError;
use crate::interpreter::{ExecutionResult, InstructionOutcome, NativeServices, opcodes};
use crate::mailbox::Mailbox;
use crate::module::{Module, ModuleRegistry};
use crate::process::{CodePosition, ExitReason, Process};
use crate::term::{Term, format::format_term};

/// Outcome from a single debugger step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReplayStepOutcome {
    /// The instruction completed and execution may continue.
    Continue,
    /// The instruction reached a scheduler boundary or terminal state.
    Boundary(ExecutionResult),
}

/// Complete process snapshot retained by the replay debugger.
#[derive(Clone, Debug)]
pub struct ProcessSnapshot {
    instruction_count: usize,
    process: Process,
}

impl ProcessSnapshot {
    /// Instruction count at which this snapshot was captured.
    #[must_use]
    pub const fn instruction_count(&self) -> usize {
        self.instruction_count
    }

    /// Code position captured in this snapshot.
    #[must_use]
    pub fn code_position(&self) -> Option<CodePosition> {
        self.process.code_position()
    }

    /// Snapshot process state.
    #[must_use]
    pub const fn process(&self) -> &Process {
        &self.process
    }
}

/// Formatted register entry for debugger inspection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisterInspection {
    /// Register family (`x` or `y`).
    pub kind: RegisterKind,
    /// Register index within the family.
    pub index: usize,
    /// Raw VM term for programmatic consumers.
    pub term: Term,
    /// User-facing formatted representation.
    pub formatted: String,
}

/// Register family exposed by [`RegisterInspection`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RegisterKind {
    /// X register.
    X,
    /// Y register from the current stack frame.
    Y,
}

/// One stack frame in top-to-bottom inspection order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StackFrameInspection {
    /// Saved return module atom.
    pub return_module: Atom,
    /// Saved return module name.
    pub return_module_name: String,
    /// Saved return instruction pointer.
    pub return_ip: usize,
    /// Function containing the saved return IP, if known.
    pub function: Option<FunctionInspection>,
    /// Y-register locals in this frame.
    pub locals: Vec<RegisterInspection>,
}

/// Function metadata resolved from module bytecode tables.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FunctionInspection {
    /// Function atom.
    pub function: Atom,
    /// Function name.
    pub function_name: String,
    /// Function arity.
    pub arity: u8,
}

/// Heap usage and boxed-object summary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeapInspection {
    /// Young-generation words currently used.
    pub young_used: usize,
    /// Young-generation word capacity.
    pub young_capacity: usize,
    /// Old-generation words currently used.
    pub old_used: usize,
    /// Old-generation word capacity.
    pub old_capacity: usize,
    /// Total used words across generations.
    pub total_used: usize,
    /// Total word capacity across generations.
    pub total_capacity: usize,
    /// Young-generation high-water mark.
    pub high_water_mark: usize,
    /// Boxed object count grouped by boxed tag name.
    pub boxed_objects_by_tag: BTreeMap<String, usize>,
}

/// Read-only mailbox inspection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MailboxInspection {
    /// Total messages visible to the mailbox, including undrained arrivals.
    pub message_count: usize,
    /// Messages currently in the owner-side scan list.
    pub scan_messages: Vec<String>,
    /// Number of messages still in the arrival queue and not formatted here.
    pub arrival_count: usize,
}

/// Single-process replay debugger.
pub struct ReplayDebugger {
    process: Process,
    initial_module: Arc<Module>,
    registry: Option<Arc<ModuleRegistry>>,
    services: NativeServices,
    snapshot_granularity: usize,
    instruction_count: usize,
    snapshots: Vec<ProcessSnapshot>,
}

impl ReplayDebugger {
    /// Create a debugger with snapshots captured after every instruction.
    #[must_use]
    pub fn new(process: Process, initial_module: Arc<Module>) -> Self {
        Self::with_snapshot_granularity(process, initial_module, 1)
    }

    /// Create a debugger with configurable checkpoint spacing.
    #[must_use]
    pub fn with_snapshot_granularity(
        process: Process,
        initial_module: Arc<Module>,
        snapshot_granularity: usize,
    ) -> Self {
        let snapshot_granularity = snapshot_granularity.max(1);
        let mut debugger = Self {
            process,
            initial_module,
            registry: None,
            services: empty_native_services(),
            snapshot_granularity,
            instruction_count: 0,
            snapshots: Vec::new(),
        };
        debugger.record_snapshot();
        debugger
    }

    /// Attach a module registry for cross-module calls.
    #[must_use]
    pub fn with_registry(mut self, registry: Arc<ModuleRegistry>) -> Self {
        self.registry = Some(registry);
        self
    }

    /// Attach native services, including a replay driver when native calls need recorded decisions.
    #[must_use]
    pub fn with_native_services(mut self, services: NativeServices) -> Self {
        self.services = services;
        self
    }

    /// Current instruction count from the beginning of replay.
    #[must_use]
    pub const fn instruction_count(&self) -> usize {
        self.instruction_count
    }

    /// Current code position.
    #[must_use]
    pub fn code_position(&self) -> Option<CodePosition> {
        self.process.code_position()
    }

    /// Current process state.
    #[must_use]
    pub const fn process(&self) -> &Process {
        &self.process
    }

    /// Mutable process state for test setup and debugger integration hooks.
    pub fn process_mut(&mut self) -> &mut Process {
        &mut self.process
    }

    /// Retained snapshots.
    #[must_use]
    pub fn snapshots(&self) -> &[ProcessSnapshot] {
        &self.snapshots
    }

    /// Execute exactly one instruction and record a snapshot according to granularity.
    pub fn step_forward(&mut self) -> Result<ReplayStepOutcome, ExecError> {
        if self.process.code_position().is_none() {
            self.process.set_code_position(Some(CodePosition {
                module: self.initial_module.name,
                instruction_pointer: 0,
            }));
        }

        let position = self
            .process
            .code_position()
            .ok_or(ExecError::InvalidOperand("code position"))?;
        let module_arc = self.current_module_for_position(position)?;
        let module = module_arc.as_ref();
        let instruction = module
            .code
            .get(position.instruction_pointer)
            .ok_or(ExecError::InvalidOperand("instruction pointer"))?;
        let next_ip = position
            .instruction_pointer
            .checked_add(1)
            .ok_or(ExecError::InvalidOperand("instruction pointer"))?;

        let outcome = opcodes::dispatch_with_services(
            &mut self.process,
            module,
            instruction,
            next_ip,
            &self.services,
            self.registry.as_deref(),
        )?;
        let step_outcome = self.apply_outcome(module.name, next_ip, outcome);
        self.instruction_count = self.instruction_count.saturating_add(1);
        if self
            .instruction_count
            .is_multiple_of(self.snapshot_granularity)
            || matches!(step_outcome, ReplayStepOutcome::Boundary(_))
        {
            self.record_snapshot();
        }
        Ok(step_outcome)
    }

    /// Restore the previous instruction state.
    pub fn step_backward(&mut self) -> Result<(), ExecError> {
        if self.instruction_count == 0 {
            return Ok(());
        }
        self.restore_instruction_count(self.instruction_count - 1)
    }

    /// Restore state at an exact instruction count using checkpoints and deterministic replay.
    pub fn restore_instruction_count(&mut self, target: usize) -> Result<(), ExecError> {
        if target == self.instruction_count {
            return Ok(());
        }
        let checkpoint_index = self
            .snapshots
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, snapshot)| (snapshot.instruction_count <= target).then_some(index))
            .ok_or(ExecError::InvalidOperand("snapshot checkpoint"))?;
        let checkpoint = self.snapshots[checkpoint_index].clone();
        self.process = checkpoint.process;
        self.instruction_count = checkpoint.instruction_count;
        self.snapshots.truncate(checkpoint_index + 1);
        while self.instruction_count < target {
            self.step_forward()?;
        }
        Ok(())
    }

    /// Inspect current X registers and current-frame Y registers as formatted terms.
    #[must_use]
    pub fn inspect_registers(&self, atom_table: &AtomTable) -> Vec<RegisterInspection> {
        let mut registers = self
            .process
            .x_regs()
            .iter()
            .enumerate()
            .filter(|&(_index, term)| !term.is_nil())
            .map(|(index, term)| inspect_register(RegisterKind::X, index, *term, atom_table))
            .collect::<Vec<_>>();

        if let Ok(frame) = self.process.stack().current_frame() {
            registers.extend(
                frame
                    .y_regs()
                    .enumerate()
                    .filter(|&(_index, term)| !term.is_nil())
                    .map(|(index, term)| {
                        inspect_register(RegisterKind::Y, index, *term, atom_table)
                    }),
            );
        }
        registers
    }

    /// Inspect stack frames with function names and local variables.
    #[must_use]
    pub fn inspect_stack(&self, atom_table: &AtomTable) -> Vec<StackFrameInspection> {
        self.process
            .stack()
            .frames_from_top()
            .map(|frame| {
                let function = frame.pinned_module().function_at_ip(frame.return_ip()).map(
                    |(function, arity)| FunctionInspection {
                        function,
                        function_name: format_term(Term::atom(function), atom_table),
                        arity,
                    },
                );
                let locals = frame
                    .y_regs()
                    .enumerate()
                    .filter(|&(_index, term)| !term.is_nil())
                    .map(|(index, term)| {
                        inspect_register(RegisterKind::Y, index, *term, atom_table)
                    })
                    .collect();
                StackFrameInspection {
                    return_module: frame.return_module(),
                    return_module_name: format_term(Term::atom(frame.return_module()), atom_table),
                    return_ip: frame.return_ip(),
                    function,
                    locals,
                }
            })
            .collect()
    }

    /// Inspect heap usage and boxed object counts.
    #[must_use]
    pub fn inspect_heap(&self) -> HeapInspection {
        let heap = self.process.heap();
        let mut boxed_objects_by_tag = BTreeMap::new();
        heap.visit_boxed_objects(|_ptr, tag, _words| {
            *boxed_objects_by_tag.entry(format!("{tag:?}")).or_insert(0) += 1;
        });
        HeapInspection {
            young_used: heap.young_used(),
            young_capacity: heap.young_capacity(),
            old_used: heap.old_used(),
            old_capacity: heap.old_capacity(),
            total_used: heap.total_used(),
            total_capacity: heap.total_capacity(),
            high_water_mark: heap.high_water_mark(),
            boxed_objects_by_tag,
        }
    }

    /// Inspect mailbox state without draining arrived messages.
    #[must_use]
    pub fn inspect_mailbox(&self, atom_table: &AtomTable) -> MailboxInspection {
        inspect_mailbox(self.process.mailbox(), atom_table)
    }

    fn current_module_for_position(
        &mut self,
        position: CodePosition,
    ) -> Result<Arc<Module>, ExecError> {
        if let Some(current) = self.process.current_module()
            && current.name == position.module
        {
            return Ok(Arc::clone(current));
        }

        let module = self
            .registry
            .as_deref()
            .and_then(|registry| registry.lookup(position.module))
            .or_else(|| {
                (self.initial_module.name == position.module)
                    .then(|| Arc::clone(&self.initial_module))
            })
            .ok_or(ExecError::InvalidOperand("code position module"))?;
        self.process.set_current_module(Arc::clone(&module));
        Ok(module)
    }

    fn apply_outcome(
        &mut self,
        module: Atom,
        next_ip: usize,
        outcome: InstructionOutcome,
    ) -> ReplayStepOutcome {
        match outcome {
            InstructionOutcome::Continue => {
                self.process.set_code_position(Some(CodePosition {
                    module,
                    instruction_pointer: next_ip,
                }));
                ReplayStepOutcome::Continue
            }
            InstructionOutcome::NativeContinuation => ReplayStepOutcome::Continue,
            InstructionOutcome::Jump(target) => {
                self.process.set_code_position(Some(target));
                ReplayStepOutcome::Continue
            }
            InstructionOutcome::Yield => ReplayStepOutcome::Boundary(ExecutionResult::Yielded),
            InstructionOutcome::Waiting => ReplayStepOutcome::Boundary(ExecutionResult::Waiting),
            InstructionOutcome::Exit(reason) => {
                self.process.set_code_position(None);
                self.process.clear_current_module();
                ReplayStepOutcome::Boundary(ExecutionResult::Exited(reason))
            }
            InstructionOutcome::OnLoadComplete => {
                self.process.set_code_position(None);
                self.process.clear_current_module();
                ReplayStepOutcome::Boundary(ExecutionResult::Exited(ExitReason::Normal))
            }
            InstructionOutcome::DirtyCall {
                entry,
                args,
                module,
                function,
                arity,
                kind,
            } => ReplayStepOutcome::Boundary(ExecutionResult::DirtyCall {
                entry,
                args,
                module,
                function,
                arity,
                kind,
            }),
        }
    }

    fn record_snapshot(&mut self) {
        if self
            .snapshots
            .last()
            .is_some_and(|snapshot| snapshot.instruction_count == self.instruction_count)
        {
            return;
        }
        self.process.mailbox_mut().drain_arrival();
        self.snapshots.push(ProcessSnapshot {
            instruction_count: self.instruction_count,
            process: self.process.clone(),
        });
    }
}

fn inspect_register(
    kind: RegisterKind,
    index: usize,
    term: Term,
    atom_table: &AtomTable,
) -> RegisterInspection {
    RegisterInspection {
        kind,
        index,
        term,
        formatted: format_term(term, atom_table),
    }
}

fn inspect_mailbox(mailbox: &Mailbox, atom_table: &AtomTable) -> MailboxInspection {
    let scan_messages = mailbox
        .scan_iter()
        .map(|term| format_term(*term, atom_table))
        .collect::<Vec<_>>();
    let message_count = mailbox.message_count();
    let arrival_count = message_count.saturating_sub(scan_messages.len());
    MailboxInspection {
        message_count,
        scan_messages,
        arrival_count,
    }
}

fn empty_native_services() -> NativeServices {
    NativeServices {
        atom_table: None,
        local_node: None,
        net_kernel: None,
        distribution_send: None,
        timers: None,
        spawn_facility: None,
        remote_spawn_facility: None,
        link_facility: None,
        distribution_control_facility: None,
        global_name_facility: None,
        group_leader_facility: None,
        supervision_facility: None,
        process_info_facility: None,
        io_sink: None,
        code_management_facility: None,
        system_info_facility: None,
        ets_facility: None,
        pg_facility: None,
        io_facility: None,
        io_message_facility: None,
        file_io_facility: None,
        tcp_io_facility: None,
        jit_cache: None,
        replay_driver: None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::loader::Instruction;
    use crate::loader::decode::Operand;
    use crate::module::ModuleOrigin;
    use crate::term::boxed::{Cons, Tuple};

    fn module(code: Vec<Instruction>) -> Arc<Module> {
        Arc::new(Module {
            name: Atom::OK,
            generation: 0,
            origin: ModuleOrigin::Preloaded,
            exports: HashMap::new(),
            label_index: code
                .iter()
                .enumerate()
                .filter_map(|(ip, instruction)| match instruction {
                    Instruction::Label { label } => Some((*label, ip)),
                    _ => None,
                })
                .collect(),
            code,
            literals: Vec::new(),
            constant_pool: Default::default(),
            resolved_imports: Vec::new(),
            lambdas: Vec::new(),
            string_table: Vec::new(),
            function_table: vec![(0, Atom::OK, 0)],
            line_table: Vec::new(),
            line_info: Vec::new(),
        })
    }

    fn move_int(value: i64) -> Instruction {
        Instruction::Move {
            source: Operand::Integer(value),
            destination: Operand::X(0),
        }
    }

    #[test]
    fn step_forward_ten_and_back_five_restores_each_state() {
        let code = (0..12).map(move_int).collect();
        let module = module(code);
        let process = Process::new(1, 233);
        let mut debugger = ReplayDebugger::with_snapshot_granularity(process, module, 3);

        for expected in 0..10 {
            assert_eq!(debugger.step_forward(), Ok(ReplayStepOutcome::Continue));
            assert_eq!(debugger.process().x_reg(0), Term::small_int(expected));
            assert_eq!(debugger.instruction_count(), (expected + 1) as usize);
        }

        for expected in (4..=8).rev() {
            assert!(debugger.step_backward().is_ok());
            assert_eq!(debugger.process().x_reg(0), Term::small_int(expected));
        }
        assert_eq!(debugger.instruction_count(), 5);
        assert!(
            debugger
                .snapshots()
                .iter()
                .any(|snapshot| snapshot.instruction_count() == 3)
        );
    }

    #[test]
    fn inspect_paused_state_reports_registers_stack_heap_and_mailbox() {
        let module = module(vec![
            move_int(41),
            Instruction::Allocate {
                stack_need: Operand::Unsigned(2),
                live: Operand::Unsigned(1),
            },
            Instruction::Move {
                source: Operand::Integer(7),
                destination: Operand::Y(0),
            },
            Instruction::PutTuple2 {
                destination: Operand::X(1),
                elements: Operand::List(vec![Operand::X(0), Operand::Y(0)]),
            },
        ]);
        let mut process = Process::new(1, 233);
        process
            .mailbox_mut()
            .push_owned_for_test(Term::small_int(99));
        let mut debugger = ReplayDebugger::new(process, Arc::clone(&module));
        for _ in 0..4 {
            assert!(debugger.step_forward().is_ok());
        }

        let atoms = AtomTable::with_common_atoms();
        let registers = debugger.inspect_registers(&atoms);
        assert!(registers.iter().any(|register| {
            register.kind == RegisterKind::X && register.index == 0 && register.formatted == "41"
        }));
        assert!(registers.iter().any(|register| {
            register.kind == RegisterKind::Y && register.index == 0 && register.formatted == "7"
        }));

        let stack = debugger.inspect_stack(&atoms);
        assert_eq!(stack.len(), 1);
        assert_eq!(
            stack[0].function.as_ref().map(|function| function.arity),
            Some(0)
        );
        assert_eq!(stack[0].locals[0].formatted, "7");

        let heap = debugger.inspect_heap();
        assert_eq!(heap.total_used, 3);
        assert_eq!(heap.boxed_objects_by_tag.get("Tuple"), Some(&1));

        let mailbox = debugger.inspect_mailbox(&atoms);
        assert_eq!(mailbox.message_count, 1);
        assert_eq!(mailbox.scan_messages, vec!["99".to_owned()]);
    }

    #[test]
    fn backward_restores_boxed_terms_to_snapshot_heap() {
        let module = module(vec![
            Instruction::PutTuple2 {
                destination: Operand::X(0),
                elements: Operand::List(vec![Operand::Integer(1)]),
            },
            Instruction::Move {
                source: Operand::Integer(2),
                destination: Operand::X(0),
            },
        ]);
        let process = Process::new(1, 233);
        let mut debugger = ReplayDebugger::new(process, module);
        assert!(debugger.step_forward().is_ok());
        let tuple = debugger.process().x_reg(0);
        assert_eq!(
            Tuple::new(tuple).and_then(|tuple| tuple.get(0)),
            Some(Term::small_int(1))
        );
        assert!(debugger.step_forward().is_ok());
        assert!(debugger.step_backward().is_ok());
        let restored = debugger.process().x_reg(0);
        assert_eq!(
            Tuple::new(restored).and_then(|tuple| tuple.get(0)),
            Some(Term::small_int(1))
        );
    }

    #[test]
    fn backward_restores_cons_cells_with_rebased_tail_terms() {
        let module = module(vec![
            Instruction::PutList {
                head: Operand::Integer(2),
                tail: Operand::Atom(None),
                destination: Operand::X(0),
            },
            Instruction::PutList {
                head: Operand::Integer(1),
                tail: Operand::X(0),
                destination: Operand::X(0),
            },
            Instruction::Move {
                source: Operand::Integer(3),
                destination: Operand::X(0),
            },
        ]);
        let process = Process::new(1, 233);
        let mut debugger = ReplayDebugger::new(process, module);
        assert!(debugger.step_forward().is_ok());
        assert!(debugger.step_forward().is_ok());
        let list = debugger.process().x_reg(0);
        let cons = Cons::new(list).expect("outer cons");
        assert_eq!(cons.head(), Term::small_int(1));
        let tail = Cons::new(cons.tail()).expect("inner cons");
        assert_eq!(tail.head(), Term::small_int(2));
        assert_eq!(tail.tail(), Term::NIL);

        assert!(debugger.step_forward().is_ok());
        assert!(debugger.step_backward().is_ok());
        let restored = debugger.process().x_reg(0);
        let cons = Cons::new(restored).expect("restored outer cons");
        assert_eq!(cons.head(), Term::small_int(1));
        let tail = Cons::new(cons.tail()).expect("restored inner cons");
        assert_eq!(tail.head(), Term::small_int(2));
        assert_eq!(tail.tail(), Term::NIL);
    }
}
