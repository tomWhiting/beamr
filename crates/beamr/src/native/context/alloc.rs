//! Heap allocators for native BIFs.
//!
//! Allocators that take term arguments root them for the duration of the
//! allocation (a reserve may trigger a collection that moves them); the
//! `_prereserved` variants assume `ensure_heap_space` was already called and
//! cannot collect.

#[cfg(feature = "threads")]
use std::sync::Arc;

#[cfg(feature = "threads")]
use crate::io::resource::{FD_RESOURCE_WORDS, FdInner, write_fd_resource};
use crate::term::Term;
use crate::term::boxed::{
    write_bigint, write_cons, write_external_pid, write_external_reference, write_float, write_map,
    write_reference, write_tuple,
};
use crate::term::shared_binary::{alloc_binary, alloc_binary_word_count};

use super::ProcessContext;

impl ProcessContext<'_> {
    /// Allocate a tuple on the calling process heap.
    pub fn alloc_tuple(&mut self, elements: &[Term]) -> Result<Term, Term> {
        let words = 1 + elements.len();
        if self.process.is_none() {
            // Detached contexts allocate owned blocks and never collect.
            let heap = self.alloc_words(words)?;
            return write_tuple(heap, elements)
                .ok_or_else(|| Term::atom(crate::atom::Atom::BADARG));
        }
        // Root the inputs: the reserve below may collect and move them.
        self.with_rooted(elements, |context, roots| {
            context.ensure_heap_space(words)?;
            let mut current = Vec::with_capacity(roots.len);
            for index in 0..roots.len {
                current.push(context.rooted(roots, index)?);
            }
            let heap = context.alloc_words_prereserved(words)?;
            write_tuple(heap, &current).ok_or_else(|| Term::atom(crate::atom::Atom::BADARG))
        })
    }

    /// Allocate a tuple using pre-reserved heap space (no GC trigger).
    /// Caller must have called `ensure_heap_space` for the total budget.
    pub fn alloc_tuple_prereserved(&mut self, elements: &[Term]) -> Result<Term, Term> {
        let words = 1 + elements.len();
        let heap = self.alloc_words_prereserved(words)?;
        write_tuple(heap, elements).ok_or_else(|| Term::atom(crate::atom::Atom::BADARG))
    }

    /// Allocate a reference on the calling process heap.
    pub fn alloc_reference(&mut self, id: u64) -> Result<Term, Term> {
        let heap = self.alloc_words(2)?;
        write_reference(heap, id).ok_or_else(|| Term::atom(crate::atom::Atom::BADARG))
    }

    /// Allocate a reference using pre-reserved heap space (no GC trigger).
    /// Caller must have called `ensure_heap_space` for the total budget.
    pub fn alloc_reference_prereserved(&mut self, id: u64) -> Result<Term, Term> {
        let heap = self.alloc_words_prereserved(2)?;
        write_reference(heap, id).ok_or_else(|| Term::atom(crate::atom::Atom::BADARG))
    }

    /// Allocate a remote PID on the calling process heap.
    pub fn alloc_external_pid(
        &mut self,
        node: crate::atom::Atom,
        pid_number: u64,
        serial: u64,
    ) -> Result<Term, Term> {
        let heap = self.alloc_words(4)?;
        write_external_pid(heap, node, pid_number, serial)
            .ok_or_else(|| Term::atom(crate::atom::Atom::BADARG))
    }

    /// Allocate a remote PID using pre-reserved heap space (no GC trigger).
    /// Caller must have called `ensure_heap_space` for the total budget.
    pub fn alloc_external_pid_prereserved(
        &mut self,
        node: crate::atom::Atom,
        pid_number: u64,
        serial: u64,
    ) -> Result<Term, Term> {
        let heap = self.alloc_words_prereserved(4)?;
        write_external_pid(heap, node, pid_number, serial)
            .ok_or_else(|| Term::atom(crate::atom::Atom::BADARG))
    }

    /// Allocate a remote reference on the calling process heap.
    pub fn alloc_external_reference(
        &mut self,
        node: crate::atom::Atom,
        id: u64,
    ) -> Result<Term, Term> {
        let heap = self.alloc_words(3)?;
        write_external_reference(heap, node, id)
            .ok_or_else(|| Term::atom(crate::atom::Atom::BADARG))
    }

    /// Allocate a remote reference using pre-reserved heap space (no GC trigger).
    /// Caller must have called `ensure_heap_space` for the total budget.
    pub fn alloc_external_reference_prereserved(
        &mut self,
        node: crate::atom::Atom,
        id: u64,
    ) -> Result<Term, Term> {
        let heap = self.alloc_words_prereserved(3)?;
        write_external_reference(heap, node, id)
            .ok_or_else(|| Term::atom(crate::atom::Atom::BADARG))
    }

    /// Allocate a cons cell on the calling process heap.
    pub fn alloc_cons(&mut self, head: Term, tail: Term) -> Result<Term, Term> {
        if self.process.is_none() {
            // Detached contexts allocate owned blocks and never collect.
            let heap = self.alloc_words(2)?;
            return write_cons(heap, head, tail)
                .ok_or_else(|| Term::atom(crate::atom::Atom::BADARG));
        }
        // Root the inputs: the reserve below may collect and move them.
        self.with_rooted(&[head, tail], |context, roots| {
            context.ensure_heap_space(2)?;
            let head = context.rooted(roots, 0)?;
            let tail = context.rooted(roots, 1)?;
            let heap = context.alloc_words_prereserved(2)?;
            write_cons(heap, head, tail).ok_or_else(|| Term::atom(crate::atom::Atom::BADARG))
        })
    }

    /// Allocate a float on the calling process heap.
    pub fn alloc_float(&mut self, value: f64) -> Result<Term, Term> {
        let heap = self.alloc_words(2)?;
        write_float(heap, value).ok_or_else(|| Term::atom(crate::atom::Atom::BADARG))
    }

    /// Allocate a binary on the calling process heap, promoting large binaries to ProcBin.
    pub fn alloc_binary(&mut self, bytes: &[u8]) -> Result<Term, Term> {
        let words = alloc_binary_word_count(bytes.len());
        let heap = self.alloc_words(words)?;
        alloc_binary(heap, bytes).ok_or_else(|| Term::atom(crate::atom::Atom::BADARG))
    }

    /// Allocate an FdResource on the calling process heap.
    #[cfg(feature = "threads")]
    pub fn alloc_fd_resource(&mut self, fd_inner: Arc<FdInner>) -> Result<Term, Term> {
        let heap = self.alloc_words(FD_RESOURCE_WORDS)?;
        write_fd_resource(heap, fd_inner).ok_or_else(|| Term::atom(crate::atom::Atom::BADARG))
    }

    /// Allocate a big integer on the calling process heap.
    pub fn alloc_bigint(&mut self, negative: bool, limbs: &[u64]) -> Result<Term, Term> {
        let words = 3 + limbs.len();
        let heap = self.alloc_words(words)?;
        write_bigint(heap, negative, limbs).ok_or_else(|| Term::atom(crate::atom::Atom::BADARG))
    }

    /// Allocate a proper list on the calling process heap.
    pub fn alloc_list(&mut self, elements: &[Term]) -> Result<Term, Term> {
        self.alloc_list_with_tail(elements, Term::NIL)
    }

    /// Allocate list cells for `elements`, ending in `tail`.
    ///
    /// The inputs are registered as GC roots for the duration of the call, so
    /// boxed terms stay valid across the collection the initial reserve may
    /// trigger.
    pub fn alloc_list_with_tail(&mut self, elements: &[Term], tail: Term) -> Result<Term, Term> {
        if self.process.is_none() {
            // Detached contexts allocate owned blocks and never collect.
            let mut tail = tail;
            for element in elements.iter().rev().copied() {
                let heap = self.alloc_words_prereserved(2)?;
                tail = write_cons(heap, element, tail)
                    .ok_or_else(|| Term::atom(crate::atom::Atom::BADARG))?;
            }
            return Ok(tail);
        }
        let mut rooted = Vec::with_capacity(elements.len() + 1);
        rooted.extend_from_slice(elements);
        rooted.push(tail);
        self.with_rooted(&rooted, |context, roots| {
            context.ensure_heap_space(elements.len() * 2)?;
            let mut tail = context.rooted(roots, elements.len())?;
            for index in (0..elements.len()).rev() {
                let element = context.rooted(roots, index)?;
                let heap = context.alloc_words_prereserved(2)?;
                tail = write_cons(heap, element, tail)
                    .ok_or_else(|| Term::atom(crate::atom::Atom::BADARG))?;
            }
            Ok(tail)
        })
    }

    /// Allocate a cons cell using pre-reserved heap space (no GC trigger).
    /// Caller must have called `ensure_heap_space` for the total budget.
    pub fn alloc_cons_prereserved(&mut self, head: Term, tail: Term) -> Result<Term, Term> {
        let heap = self.alloc_words_prereserved(2)?;
        write_cons(heap, head, tail).ok_or_else(|| Term::atom(crate::atom::Atom::BADARG))
    }

    /// Allocate a flatmap on the calling process heap.
    ///
    /// Keys and values are registered as GC roots for the duration of the
    /// call.
    pub fn alloc_map(&mut self, keys: &[Term], values: &[Term]) -> Result<Term, Term> {
        let words = 2 + keys.len() + values.len();
        if self.process.is_none() {
            // Detached contexts allocate owned blocks and never collect.
            let heap = self.alloc_words(words)?;
            return write_map(heap, keys, values)
                .ok_or_else(|| Term::atom(crate::atom::Atom::BADARG));
        }
        let mut rooted = Vec::with_capacity(keys.len() + values.len());
        rooted.extend_from_slice(keys);
        rooted.extend_from_slice(values);
        self.with_rooted(&rooted, |context, roots| {
            context.ensure_heap_space(words)?;
            let mut current_keys = Vec::with_capacity(keys.len());
            let mut current_values = Vec::with_capacity(values.len());
            for index in 0..keys.len() {
                current_keys.push(context.rooted(roots, index)?);
            }
            for index in 0..values.len() {
                current_values.push(context.rooted(roots, keys.len() + index)?);
            }
            let heap = context.alloc_words_prereserved(words)?;
            write_map(heap, &current_keys, &current_values)
                .ok_or_else(|| Term::atom(crate::atom::Atom::BADARG))
        })
    }

    /// Allocate a flatmap using pre-reserved heap space (no GC trigger).
    /// Caller must have called `ensure_heap_space` for the total budget.
    pub fn alloc_map_prereserved(&mut self, keys: &[Term], values: &[Term]) -> Result<Term, Term> {
        let words = 2 + keys.len() + values.len();
        let heap = self.alloc_words_prereserved(words)?;
        write_map(heap, keys, values).ok_or_else(|| Term::atom(crate::atom::Atom::BADARG))
    }
}
