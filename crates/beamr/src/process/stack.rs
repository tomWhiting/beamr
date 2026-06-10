//! Call stack frames.
//!
//! The stack holds return addresses and Y-register slots. `allocate` pushes a
//! frame with N Y-register slots; `deallocate` pops it. Tail calls
//! (`call_last`, `call_ext_last`) deallocate before jumping, preventing stack
//! growth in recursive functions.

use std::fmt;
use std::sync::Arc;

use crate::atom::Atom;
use crate::module::Module;
use crate::term::Term;

/// Default maximum call-stack depth in frames.
pub const DEFAULT_STACK_FRAME_LIMIT: usize = 10_000;

/// Saved return location for a stack frame.
#[derive(Clone, Debug)]
pub struct ReturnPoint {
    /// Module to return to.
    pub module: Atom,
    /// Instruction pointer to resume at in `module`.
    pub ip: usize,
    /// Pinned module version to resume after returning.
    pub module_version: Arc<Module>,
}

impl PartialEq for ReturnPoint {
    fn eq(&self, other: &Self) -> bool {
        self.module == other.module
            && self.ip == other.ip
            && Arc::ptr_eq(&self.module_version, &other.module_version)
    }
}

impl Eq for ReturnPoint {}

/// One call-stack frame with its own Y-register slots.
#[derive(Clone, Debug)]
pub struct StackFrame {
    return_module: Atom,
    return_ip: usize,
    pinned_module: Arc<Module>,
    y_slots: u16,
    y_regs: Vec<Term>,
}

impl StackFrame {
    fn new(
        return_module: Atom,
        return_ip: usize,
        pinned_module: Arc<Module>,
        y_slots: u16,
    ) -> Self {
        Self {
            return_module,
            return_ip,
            pinned_module,
            y_slots,
            y_regs: vec![Term::NIL; usize::from(y_slots)],
        }
    }

    /// Return module saved by this frame.
    #[must_use]
    pub const fn return_module(&self) -> Atom {
        self.return_module
    }

    /// Return instruction pointer saved by this frame.
    #[must_use]
    pub const fn return_ip(&self) -> usize {
        self.return_ip
    }

    /// Pinned module version saved by this frame.
    #[must_use]
    pub const fn pinned_module(&self) -> &Arc<Module> {
        &self.pinned_module
    }

    /// Number of Y-register slots owned by this frame.
    #[must_use]
    pub const fn y_slots(&self) -> u16 {
        self.y_slots
    }

    /// Iterator over all Y-register slots in this frame.
    pub(crate) fn y_regs(&self) -> impl Iterator<Item = &Term> {
        self.y_regs.iter()
    }

    /// Mutable iterator over all Y-register slots in this frame.
    pub(crate) fn y_regs_mut(&mut self) -> impl Iterator<Item = &mut Term> {
        self.y_regs.iter_mut()
    }

    /// Read a Y-register in this frame.
    pub fn y_reg(&self, n: u16) -> Result<Term, StackError> {
        self.y_regs
            .get(usize::from(n))
            .copied()
            .ok_or(StackError::YRegisterOutOfBounds {
                index: n,
                slots: self.y_slots,
            })
    }

    /// Write a Y-register in this frame.
    pub fn set_y_reg(&mut self, n: u16, value: Term) -> Result<(), StackError> {
        let slots = self.y_slots;
        let Some(slot) = self.y_regs.get_mut(usize::from(n)) else {
            return Err(StackError::YRegisterOutOfBounds { index: n, slots });
        };
        *slot = value;
        Ok(())
    }

    /// Shrink this frame to `remaining` Y-register slots.
    ///
    /// BEAM `trim N, Remaining` discards the N lowest-numbered Y registers
    /// and renumbers the survivors down: the old `y(N+k)` becomes `y(k)`.
    pub fn trim_y_regs(&mut self, remaining: u16) -> Result<(), StackError> {
        if remaining > self.y_slots {
            return Err(StackError::YRegisterOutOfBounds {
                index: remaining,
                slots: self.y_slots,
            });
        }

        let discard = usize::from(self.y_slots - remaining);
        self.y_regs.drain(..discard);
        self.y_slots = remaining;
        Ok(())
    }
}

impl PartialEq for StackFrame {
    fn eq(&self, other: &Self) -> bool {
        self.return_module == other.return_module
            && self.return_ip == other.return_ip
            && Arc::ptr_eq(&self.pinned_module, &other.pinned_module)
            && self.y_slots == other.y_slots
            && self.y_regs == other.y_regs
    }
}

impl Eq for StackFrame {}

/// Stack operation errors.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum StackError {
    /// Pushing another frame would exceed the configured frame limit.
    StackOverflow { limit: usize },
    /// A pop or Y-register access was attempted with no current frame.
    StackUnderflow,
    /// A Y-register index exceeded the current frame's slot count.
    YRegisterOutOfBounds { index: u16, slots: u16 },
}

impl fmt::Display for StackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StackOverflow { limit } => {
                write!(f, "stack overflow: frame limit {limit} reached")
            }
            Self::StackUnderflow => f.write_str("stack underflow"),
            Self::YRegisterOutOfBounds { index, slots } => {
                write!(f, "Y register {index} out of bounds for {slots} slots")
            }
        }
    }
}

impl std::error::Error for StackError {}

/// Per-process call stack.
#[derive(Clone, Debug)]
pub struct Stack {
    frames: Vec<StackFrame>,
    frame_limit: usize,
}

impl Stack {
    /// Create an empty stack with the default frame limit.
    #[must_use]
    pub fn new() -> Self {
        Self::with_frame_limit(DEFAULT_STACK_FRAME_LIMIT)
    }

    /// Create an empty stack with a custom frame limit.
    #[must_use]
    pub const fn with_frame_limit(frame_limit: usize) -> Self {
        Self {
            frames: Vec::new(),
            frame_limit,
        }
    }

    /// Push a frame saving `module:ip`, pinning the caller module version, and allocating `y_slots` Y-registers.
    pub fn push_frame(
        &mut self,
        module: Atom,
        ip: usize,
        pinned_module: Arc<Module>,
        y_slots: u16,
    ) -> Result<(), StackError> {
        if self.frames.len() >= self.frame_limit {
            return Err(StackError::StackOverflow {
                limit: self.frame_limit,
            });
        }

        self.frames
            .push(StackFrame::new(module, ip, pinned_module, y_slots));
        Ok(())
    }

    /// Pop the current frame and return its saved return point.
    pub fn pop_frame(&mut self) -> Result<ReturnPoint, StackError> {
        let Some(frame) = self.frames.pop() else {
            return Err(StackError::StackUnderflow);
        };

        Ok(ReturnPoint {
            module: frame.return_module,
            ip: frame.return_ip,
            module_version: frame.pinned_module,
        })
    }

    /// Return the current frame.
    pub fn current_frame(&self) -> Result<&StackFrame, StackError> {
        self.frames.last().ok_or(StackError::StackUnderflow)
    }

    /// Return the current frame mutably.
    pub fn current_frame_mut(&mut self) -> Result<&mut StackFrame, StackError> {
        self.frames.last_mut().ok_or(StackError::StackUnderflow)
    }

    /// Read a Y-register from the current frame.
    pub fn y_reg(&self, n: u16) -> Result<Term, StackError> {
        self.current_frame()?.y_reg(n)
    }

    /// Write a Y-register in the current frame.
    pub fn set_y_reg(&mut self, n: u16, value: Term) -> Result<(), StackError> {
        self.current_frame_mut()?.set_y_reg(n, value)
    }

    /// Shrink the current frame to `remaining` lowest-numbered Y-register slots.
    pub fn trim_y_regs(&mut self, remaining: u16) -> Result<(), StackError> {
        self.current_frame_mut()?.trim_y_regs(remaining)
    }

    /// Number of frames on the stack.
    #[must_use]
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// Drop frames above `len`, preserving all lower frames.
    pub fn truncate(&mut self, len: usize) {
        self.frames.truncate(len);
    }

    /// Returns true when the stack has no frames.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Configured maximum frame count.
    #[must_use]
    pub const fn frame_limit(&self) -> usize {
        self.frame_limit
    }

    /// Iterator over every module version pinned by stack frames.
    pub fn pinned_modules(&self) -> impl Iterator<Item = &Arc<Module>> {
        self.frames.iter().map(StackFrame::pinned_module)
    }

    /// Iterator over call frames from newest to oldest.
    pub fn frames_from_top(&self) -> impl Iterator<Item = &StackFrame> {
        self.frames.iter().rev()
    }

    /// Iterator over every Y-register in every stack frame.
    pub(crate) fn y_regs(&self) -> impl Iterator<Item = &Term> {
        self.frames.iter().flat_map(StackFrame::y_regs)
    }

    /// Mutable iterator over every Y-register in every stack frame.
    pub(crate) fn y_regs_mut(&mut self) -> impl Iterator<Item = &mut Term> {
        self.frames.iter_mut().flat_map(StackFrame::y_regs_mut)
    }
}

impl Default for Stack {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{ReturnPoint, Stack, StackError, StackFrame};
    use std::collections::HashMap;
    use std::sync::Arc;

    use crate::atom::Atom;
    use crate::loader::Instruction;
    use crate::module::{Module, ModuleOrigin, ModuleRegistry, PurgeError};
    use crate::term::Term;

    fn test_module(name: Atom) -> Module {
        Module {
            name,
            generation: 0,
            origin: ModuleOrigin::Preloaded,
            exports: HashMap::new(),
            label_index: HashMap::from([(1, 0)]),
            code: vec![Instruction::Label { label: 1 }],
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

    fn module_arc(name: Atom) -> Arc<Module> {
        Arc::new(test_module(name))
    }

    #[test]
    fn new_stack_is_empty() {
        let stack = Stack::new();

        assert!(stack.is_empty());
        assert_eq!(stack.len(), 0);
    }

    #[test]
    fn push_and_pop_round_trips_return_point() {
        let mut stack = Stack::new();
        let module_version = module_arc(Atom::OK);

        stack
            .push_frame(Atom::OK, 42, Arc::clone(&module_version), 2)
            .expect("frame should fit on empty stack");
        let return_point = stack.pop_frame().expect("frame should pop");

        assert_eq!(
            return_point,
            ReturnPoint {
                module: Atom::OK,
                ip: 42,
                module_version,
            }
        );
        assert!(stack.is_empty());
    }

    #[test]
    fn y_registers_start_as_nil() {
        let mut stack = Stack::new();

        stack
            .push_frame(Atom::OK, 0, module_arc(Atom::OK), 2)
            .expect("frame should fit on empty stack");

        assert_eq!(stack.y_reg(0), Ok(Term::NIL));
        assert_eq!(stack.y_reg(1), Ok(Term::NIL));
    }

    #[test]
    fn y_registers_are_isolated_by_frame() {
        let mut stack = Stack::new();

        stack
            .push_frame(Atom::OK, 0, module_arc(Atom::OK), 1)
            .expect("first frame should fit");
        stack
            .set_y_reg(0, Term::small_int(10))
            .expect("Y0 exists in first frame");
        stack
            .push_frame(Atom::ERROR, 1, module_arc(Atom::ERROR), 1)
            .expect("second frame should fit");
        stack
            .set_y_reg(0, Term::small_int(20))
            .expect("Y0 exists in second frame");

        assert_eq!(stack.y_reg(0), Ok(Term::small_int(20)));
        let _ = stack.pop_frame().expect("second frame should pop");
        assert_eq!(stack.y_reg(0), Ok(Term::small_int(10)));
    }

    #[test]
    fn truncate_drops_frames_above_depth() {
        let mut stack = Stack::new();

        stack
            .push_frame(Atom::OK, 0, module_arc(Atom::OK), 1)
            .expect("first frame should fit");
        stack
            .set_y_reg(0, Term::small_int(10))
            .expect("Y0 exists in first frame");
        stack
            .push_frame(Atom::ERROR, 1, module_arc(Atom::ERROR), 1)
            .expect("second frame should fit");
        stack
            .set_y_reg(0, Term::small_int(20))
            .expect("Y0 exists in second frame");

        stack.truncate(1);

        assert_eq!(stack.len(), 1);
        assert_eq!(stack.y_reg(0), Ok(Term::small_int(10)));

        stack.truncate(0);

        assert!(stack.is_empty());
        assert_eq!(stack.y_reg(0), Err(StackError::StackUnderflow));
    }

    #[test]
    fn pushing_beyond_frame_limit_returns_overflow() {
        let mut stack = Stack::with_frame_limit(1);

        stack
            .push_frame(Atom::OK, 0, module_arc(Atom::OK), 0)
            .expect("first frame should fit");
        let error = stack
            .push_frame(Atom::OK, 1, module_arc(Atom::OK), 0)
            .expect_err("second frame should exceed custom limit");

        assert_eq!(error, StackError::StackOverflow { limit: 1 });
    }

    #[test]
    fn pop_on_empty_stack_returns_underflow() {
        let mut stack = Stack::new();

        assert_eq!(stack.pop_frame(), Err(StackError::StackUnderflow));
    }

    #[test]
    fn y_register_bounds_are_checked() {
        let mut stack = Stack::new();

        stack
            .push_frame(Atom::OK, 0, module_arc(Atom::OK), 1)
            .expect("frame should fit on empty stack");

        assert_eq!(
            stack.y_reg(1),
            Err(StackError::YRegisterOutOfBounds { index: 1, slots: 1 })
        );
    }

    #[test]
    fn stack_frame_trim_y_registers_keeps_high_slots_and_updates_slot_count() {
        let mut frame = StackFrame::new(Atom::OK, 7, module_arc(Atom::OK), 5);
        for (index, value) in (0_u16..).zip([10, 20, 30, 40, 50]) {
            frame
                .set_y_reg(index, Term::small_int(value))
                .expect("allocated Y register should exist");
        }

        frame.trim_y_regs(3).expect("trim within frame slots");

        assert_eq!(frame.y_slots(), 3);
        assert_eq!(frame.y_reg(0), Ok(Term::small_int(30)));
        assert_eq!(frame.y_reg(1), Ok(Term::small_int(40)));
        assert_eq!(frame.y_reg(2), Ok(Term::small_int(50)));
        assert_eq!(
            frame.y_reg(3),
            Err(StackError::YRegisterOutOfBounds { index: 3, slots: 3 })
        );
    }

    #[test]
    fn stack_trim_y_registers_keeps_high_slots_and_preserves_return_metadata() {
        let mut stack = Stack::new();
        let module_version = module_arc(Atom::OK);

        stack
            .push_frame(Atom::OK, 7, Arc::clone(&module_version), 5)
            .expect("frame should fit on empty stack");
        for (index, value) in (0_u16..).zip([10, 20, 30, 40, 50]) {
            stack
                .set_y_reg(index, Term::small_int(value))
                .expect("allocated Y register should exist");
        }

        stack.trim_y_regs(3).expect("trim within frame slots");

        let growth_error = stack
            .trim_y_regs(4)
            .expect_err("trim must not grow current frame");
        assert_eq!(
            growth_error,
            StackError::YRegisterOutOfBounds { index: 4, slots: 3 }
        );

        let frame = stack.current_frame().expect("trim must not pop frame");
        assert_eq!(frame.y_slots(), 3);
        assert_eq!(frame.return_module(), Atom::OK);
        assert_eq!(frame.return_ip(), 7);
        assert!(Arc::ptr_eq(frame.pinned_module(), &module_version));
        assert_eq!(stack.y_reg(0), Ok(Term::small_int(30)));
        assert_eq!(stack.y_reg(1), Ok(Term::small_int(40)));
        assert_eq!(stack.y_reg(2), Ok(Term::small_int(50)));
        assert_eq!(
            stack.y_reg(3),
            Err(StackError::YRegisterOutOfBounds { index: 3, slots: 3 })
        );
    }

    #[test]
    fn frames_pin_old_module_versions_across_reload_and_purge() {
        let registry = ModuleRegistry::new();
        let a_v1 = registry.insert(test_module(Atom::OK));
        let b_v1 = registry.insert(test_module(Atom::ERROR));
        let c_v1 = registry.insert(test_module(Atom::UNDEFINED));
        let mut stack = Stack::new();

        stack
            .push_frame(Atom::OK, 10, Arc::clone(&a_v1), 0)
            .expect("A frame should fit");
        stack
            .push_frame(Atom::ERROR, 20, Arc::clone(&b_v1), 0)
            .expect("B frame should fit");
        stack
            .push_frame(Atom::UNDEFINED, 30, Arc::clone(&c_v1), 0)
            .expect("C frame should fit");

        let _a_v2 = registry.insert(test_module(Atom::OK));
        let _b_v2 = registry.insert(test_module(Atom::ERROR));

        assert!(matches!(
            registry.purge_old(Atom::OK),
            Err(PurgeError::StillReferenced {
                module: Atom::OK,
                ..
            })
        ));
        assert!(matches!(
            registry.purge_old(Atom::ERROR),
            Err(PurgeError::StillReferenced {
                module: Atom::ERROR,
                ..
            })
        ));

        let c_return = stack.pop_frame().expect("C frame should pop");
        assert_eq!(c_return.module, Atom::UNDEFINED);
        assert!(Arc::ptr_eq(&c_return.module_version, &c_v1));

        let b_return = stack.pop_frame().expect("B frame should pop");
        assert_eq!(b_return.module, Atom::ERROR);
        assert!(Arc::ptr_eq(&b_return.module_version, &b_v1));

        let a_return = stack.pop_frame().expect("A frame should pop");
        assert_eq!(a_return.module, Atom::OK);
        assert!(Arc::ptr_eq(&a_return.module_version, &a_v1));
    }
}
