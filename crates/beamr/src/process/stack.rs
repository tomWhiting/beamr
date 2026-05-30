//! Call stack frames.
//!
//! The stack holds return addresses and Y-register slots. `allocate` pushes a
//! frame with N Y-register slots; `deallocate` pops it. Tail calls
//! (`call_last`, `call_ext_last`) deallocate before jumping, preventing stack
//! growth in recursive functions.

use std::fmt;

use crate::atom::Atom;
use crate::term::Term;

/// Default maximum call-stack depth in frames.
pub const DEFAULT_STACK_FRAME_LIMIT: usize = 10_000;

/// Saved return location for a stack frame.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ReturnPoint {
    /// Module to return to.
    pub module: Atom,
    /// Instruction pointer to resume at in `module`.
    pub ip: usize,
}

/// One call-stack frame with its own Y-register slots.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StackFrame {
    return_module: Atom,
    return_ip: usize,
    y_slots: u16,
    y_regs: Vec<Term>,
}

impl StackFrame {
    fn new(return_module: Atom, return_ip: usize, y_slots: u16) -> Self {
        Self {
            return_module,
            return_ip,
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
}

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

    /// Push a frame saving `module:ip` and allocating `y_slots` Y-registers.
    pub fn push_frame(&mut self, module: Atom, ip: usize, y_slots: u16) -> Result<(), StackError> {
        if self.frames.len() >= self.frame_limit {
            return Err(StackError::StackOverflow {
                limit: self.frame_limit,
            });
        }

        self.frames.push(StackFrame::new(module, ip, y_slots));
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

    /// Number of frames on the stack.
    #[must_use]
    pub fn len(&self) -> usize {
        self.frames.len()
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
    use super::{ReturnPoint, Stack, StackError};
    use crate::atom::Atom;
    use crate::term::Term;

    #[test]
    fn new_stack_is_empty() {
        let stack = Stack::new();

        assert!(stack.is_empty());
        assert_eq!(stack.len(), 0);
    }

    #[test]
    fn push_and_pop_round_trips_return_point() {
        let mut stack = Stack::new();

        stack
            .push_frame(Atom::OK, 42, 2)
            .expect("frame should fit on empty stack");
        let return_point = stack.pop_frame().expect("frame should pop");

        assert_eq!(
            return_point,
            ReturnPoint {
                module: Atom::OK,
                ip: 42,
            }
        );
        assert!(stack.is_empty());
    }

    #[test]
    fn y_registers_start_as_nil() {
        let mut stack = Stack::new();

        stack
            .push_frame(Atom::OK, 0, 2)
            .expect("frame should fit on empty stack");

        assert_eq!(stack.y_reg(0), Ok(Term::NIL));
        assert_eq!(stack.y_reg(1), Ok(Term::NIL));
    }

    #[test]
    fn y_registers_are_isolated_by_frame() {
        let mut stack = Stack::new();

        stack
            .push_frame(Atom::OK, 0, 1)
            .expect("first frame should fit");
        stack
            .set_y_reg(0, Term::small_int(10))
            .expect("Y0 exists in first frame");
        stack
            .push_frame(Atom::ERROR, 1, 1)
            .expect("second frame should fit");
        stack
            .set_y_reg(0, Term::small_int(20))
            .expect("Y0 exists in second frame");

        assert_eq!(stack.y_reg(0), Ok(Term::small_int(20)));
        let _ = stack.pop_frame().expect("second frame should pop");
        assert_eq!(stack.y_reg(0), Ok(Term::small_int(10)));
    }

    #[test]
    fn pushing_beyond_frame_limit_returns_overflow() {
        let mut stack = Stack::with_frame_limit(1);

        stack
            .push_frame(Atom::OK, 0, 0)
            .expect("first frame should fit");
        let error = stack
            .push_frame(Atom::OK, 1, 0)
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
            .push_frame(Atom::OK, 0, 1)
            .expect("frame should fit on empty stack");

        assert_eq!(
            stack.y_reg(1),
            Err(StackError::YRegisterOutOfBounds { index: 1, slots: 1 })
        );
    }
}
