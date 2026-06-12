//! Term copying support for ETS-owned storage.
//!
//! ETS entries cannot point into a process heap. [`OwnedTerm`] keeps every boxed
//! object in its own boxed word allocation so pointers embedded in the copied
//! root term remain stable for the lifetime of the entry.

use crate::ets::EtsError;
use crate::process::heap::Heap;
use crate::term::Term;
use crate::term::binary::Binary;
use crate::term::boxed::{
    self, BigInt, Closure, Cons, Float, Map, ProcBin, Reference, SubBinary, Tuple,
};

/// A term whose boxed/list objects are owned by ETS rather than a process heap.
#[derive(Debug)]
pub struct OwnedTerm {
    root: Term,
    allocations: Vec<Box<[u64]>>,
}

impl OwnedTerm {
    /// Build an owned term from already-detached heap allocations.
    ///
    /// This is used by dirty native calls whose `ProcessContext` allocated
    /// return terms without an attached process heap. The caller must provide a
    /// root term that points only into these allocations or is immediate.
    pub(crate) fn from_allocations(root: Term, allocations: Vec<Box<[u64]>>) -> Self {
        Self { root, allocations }
    }

    /// Wraps an immediate term (small int, atom, nil, pid) that owns no
    /// heap storage.
    ///
    /// # Panics
    ///
    /// Panics in debug builds when `term` points into a heap, because such a
    /// term would dangle as soon as its source heap is freed.
    #[must_use]
    pub fn immediate(term: Term) -> Self {
        debug_assert!(
            term.heap_ptr().is_none(),
            "OwnedTerm::immediate requires a non-heap term"
        );
        Self {
            root: term,
            allocations: Vec::new(),
        }
    }

    /// Root term value for table-side comparisons and traversal.
    #[must_use]
    pub const fn root(&self) -> Term {
        self.root
    }

    /// Deep-copy this ETS-owned term into a process heap for delivery to a caller.
    pub fn copy_to_heap(&self, heap: &mut Heap) -> Result<Term, EtsError> {
        copy_term_to_heap(self.root, heap)
    }

    #[must_use]
    pub fn allocation_count(&self) -> usize {
        self.allocations.len()
    }

    /// Total heap words `copy_to_heap` will allocate for this term: the
    /// owned-side copier and the heap-side copier write identical boxed
    /// layouts object for object.
    #[must_use]
    pub fn total_words(&self) -> usize {
        self.allocations
            .iter()
            .map(|allocation| allocation.len())
            .sum()
    }
}

/// Deep-copy a process term into ETS-owned memory.
pub fn copy_term_to_ets(term: Term) -> Result<OwnedTerm, EtsError> {
    let mut copier = EtsCopier {
        allocations: Vec::new(),
    };
    let root = copier.copy_term(term)?;
    Ok(OwnedTerm {
        root,
        allocations: copier.allocations,
    })
}

/// Deep-copy any term into a process heap.
pub fn copy_term_to_heap(term: Term, heap: &mut Heap) -> Result<Term, EtsError> {
    if term.is_list() {
        copy_cons_to_heap(term, heap)
    } else if term.is_boxed() {
        copy_boxed_to_heap(term, heap)
    } else {
        Ok(term)
    }
}

struct EtsCopier {
    allocations: Vec<Box<[u64]>>,
}

impl EtsCopier {
    fn copy_term(&mut self, term: Term) -> Result<Term, EtsError> {
        if term.is_list() {
            self.copy_cons(term)
        } else if term.is_boxed() {
            self.copy_boxed(term)
        } else {
            Ok(term)
        }
    }

    fn copy_cons(&mut self, term: Term) -> Result<Term, EtsError> {
        let cons = Cons::new(term).ok_or(EtsError::InvalidBoxedTerm)?;
        let head = self.copy_term(cons.head())?;
        let tail = self.copy_term(cons.tail())?;
        self.write_words(2, |words| boxed::write_cons(words, head, tail))
    }

    fn copy_boxed(&mut self, term: Term) -> Result<Term, EtsError> {
        if let Some(tuple) = Tuple::new(term) {
            return self.copy_tuple(tuple);
        }
        if let Some(float) = Float::new(term) {
            return self.copy_float(float);
        }
        if let Some(bigint) = BigInt::new(term) {
            return self.copy_bigint(bigint);
        }
        if let Some(closure) = Closure::new(term) {
            return self.copy_closure(closure);
        }
        if let Some(map) = Map::new(term) {
            return self.copy_map(map);
        }
        if let Some(reference) = Reference::new(term) {
            return self.copy_reference(reference);
        }
        if let Some(binary) = Binary::new(term) {
            return self.copy_binary(binary.as_bytes());
        }
        if let Some(proc_bin) = ProcBin::new(term) {
            return self.copy_binary(proc_bin.as_bytes());
        }
        if let Some(sub_binary) = SubBinary::new(term) {
            return self.copy_binary(sub_binary.as_bytes());
        }

        Err(EtsError::InvalidBoxedTerm)
    }

    fn copy_tuple(&mut self, tuple: Tuple) -> Result<Term, EtsError> {
        let elements = (0..tuple.arity())
            .map(|index| {
                tuple
                    .get(index)
                    .ok_or(EtsError::InvalidBoxedTerm)
                    .and_then(|element| self.copy_term(element))
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.write_words(1 + elements.len(), |words| {
            boxed::write_tuple(words, &elements)
        })
    }

    fn copy_float(&mut self, float: Float) -> Result<Term, EtsError> {
        self.write_words(2, |words| boxed::write_float(words, float.value()))
    }

    fn copy_bigint(&mut self, bigint: BigInt) -> Result<Term, EtsError> {
        let limbs = bigint.limbs();
        self.write_words(3 + limbs.len(), |words| {
            boxed::write_bigint(words, bigint.is_negative(), limbs)
        })
    }

    fn copy_closure(&mut self, closure: Closure) -> Result<Term, EtsError> {
        let module = closure.module().ok_or(EtsError::InvalidBoxedTerm)?;
        let free_vars = (0..closure.num_free())
            .map(|index| {
                closure
                    .free_var(index)
                    .ok_or(EtsError::InvalidBoxedTerm)
                    .and_then(|free_var| self.copy_term(free_var))
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.write_words(7 + free_vars.len(), |words| {
            boxed::write_closure(
                words,
                module,
                closure.function_index(),
                closure.arity(),
                closure.generation(),
                closure.unique_id(),
                &free_vars,
            )
        })
    }

    fn copy_map(&mut self, map: Map) -> Result<Term, EtsError> {
        let keys = (0..map.len())
            .map(|index| {
                map.key(index)
                    .ok_or(EtsError::InvalidBoxedTerm)
                    .and_then(|key| self.copy_term(key))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let values = (0..map.len())
            .map(|index| {
                map.value(index)
                    .ok_or(EtsError::InvalidBoxedTerm)
                    .and_then(|value| self.copy_term(value))
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.write_words(2 + keys.len() + values.len(), |words| {
            boxed::write_map(words, &keys, &values)
        })
    }

    fn copy_reference(&mut self, reference: Reference) -> Result<Term, EtsError> {
        self.write_words(2, |words| boxed::write_reference(words, reference.id()))
    }

    fn copy_binary(&mut self, bytes: &[u8]) -> Result<Term, EtsError> {
        let word_count = 2usize
            .checked_add(crate::term::binary::packed_word_count(bytes.len()))
            .ok_or(EtsError::AllocationFailed)?;
        self.write_words(word_count, |words| {
            crate::term::binary::write_binary(words, bytes)
        })
    }

    fn write_words(
        &mut self,
        word_count: usize,
        write: impl FnOnce(&mut [u64]) -> Option<Term>,
    ) -> Result<Term, EtsError> {
        let mut words = vec![0; word_count].into_boxed_slice();
        let term = write(&mut words).ok_or(EtsError::InvalidBoxedTerm)?;
        self.allocations.push(words);
        Ok(term)
    }
}

fn copy_cons_to_heap(term: Term, heap: &mut Heap) -> Result<Term, EtsError> {
    let cons = Cons::new(term).ok_or(EtsError::InvalidBoxedTerm)?;
    let head = copy_term_to_heap(cons.head(), heap)?;
    let tail = copy_term_to_heap(cons.tail(), heap)?;
    let words = heap
        .alloc_slice(2)
        .map_err(|_error| EtsError::AllocationFailed)?;
    boxed::write_cons(words, head, tail).ok_or(EtsError::InvalidBoxedTerm)
}

fn copy_boxed_to_heap(term: Term, heap: &mut Heap) -> Result<Term, EtsError> {
    if let Some(tuple) = Tuple::new(term) {
        return copy_tuple_to_heap(tuple, heap);
    }
    if let Some(float) = Float::new(term) {
        let words = heap
            .alloc_slice(2)
            .map_err(|_error| EtsError::AllocationFailed)?;
        return boxed::write_float(words, float.value()).ok_or(EtsError::InvalidBoxedTerm);
    }
    if let Some(bigint) = BigInt::new(term) {
        let limbs = bigint.limbs();
        let words = heap
            .alloc_slice(3 + limbs.len())
            .map_err(|_error| EtsError::AllocationFailed)?;
        return boxed::write_bigint(words, bigint.is_negative(), limbs)
            .ok_or(EtsError::InvalidBoxedTerm);
    }
    if let Some(closure) = Closure::new(term) {
        return copy_closure_to_heap(closure, heap);
    }
    if let Some(map) = Map::new(term) {
        return copy_map_to_heap(map, heap);
    }
    if let Some(reference) = Reference::new(term) {
        let words = heap
            .alloc_slice(2)
            .map_err(|_error| EtsError::AllocationFailed)?;
        return boxed::write_reference(words, reference.id()).ok_or(EtsError::InvalidBoxedTerm);
    }
    if let Some(binary) = Binary::new(term) {
        return copy_binary_to_heap(binary.as_bytes(), heap);
    }
    if let Some(proc_bin) = ProcBin::new(term) {
        return copy_binary_to_heap(proc_bin.as_bytes(), heap);
    }
    if let Some(sub_binary) = SubBinary::new(term) {
        return copy_binary_to_heap(sub_binary.as_bytes(), heap);
    }

    Err(EtsError::InvalidBoxedTerm)
}

fn copy_tuple_to_heap(tuple: Tuple, heap: &mut Heap) -> Result<Term, EtsError> {
    let elements = (0..tuple.arity())
        .map(|index| {
            tuple
                .get(index)
                .ok_or(EtsError::InvalidBoxedTerm)
                .and_then(|element| copy_term_to_heap(element, heap))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let words = heap
        .alloc_slice(1 + elements.len())
        .map_err(|_error| EtsError::AllocationFailed)?;
    boxed::write_tuple(words, &elements).ok_or(EtsError::InvalidBoxedTerm)
}

fn copy_closure_to_heap(closure: Closure, heap: &mut Heap) -> Result<Term, EtsError> {
    let module = closure.module().ok_or(EtsError::InvalidBoxedTerm)?;
    let free_vars = (0..closure.num_free())
        .map(|index| {
            closure
                .free_var(index)
                .ok_or(EtsError::InvalidBoxedTerm)
                .and_then(|free_var| copy_term_to_heap(free_var, heap))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let words = heap
        .alloc_slice(7 + free_vars.len())
        .map_err(|_error| EtsError::AllocationFailed)?;
    boxed::write_closure(
        words,
        module,
        closure.function_index(),
        closure.arity(),
        closure.generation(),
        closure.unique_id(),
        &free_vars,
    )
    .ok_or(EtsError::InvalidBoxedTerm)
}

fn copy_map_to_heap(map: Map, heap: &mut Heap) -> Result<Term, EtsError> {
    let keys = (0..map.len())
        .map(|index| {
            map.key(index)
                .ok_or(EtsError::InvalidBoxedTerm)
                .and_then(|key| copy_term_to_heap(key, heap))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let values = (0..map.len())
        .map(|index| {
            map.value(index)
                .ok_or(EtsError::InvalidBoxedTerm)
                .and_then(|value| copy_term_to_heap(value, heap))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let words = heap
        .alloc_slice(2 + keys.len() + values.len())
        .map_err(|_error| EtsError::AllocationFailed)?;
    boxed::write_map(words, &keys, &values).ok_or(EtsError::InvalidBoxedTerm)
}

fn copy_binary_to_heap(bytes: &[u8], heap: &mut Heap) -> Result<Term, EtsError> {
    let word_count = 2usize
        .checked_add(crate::term::binary::packed_word_count(bytes.len()))
        .ok_or(EtsError::AllocationFailed)?;
    let words = heap
        .alloc_slice(word_count)
        .map_err(|_error| EtsError::AllocationFailed)?;
    crate::term::binary::write_binary(words, bytes).ok_or(EtsError::InvalidBoxedTerm)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::binary::Binary;
    use crate::term::boxed::{Cons, Map, Tuple};
    use crate::term::shared_binary::{SharedBinary, write_proc_bin};
    use crate::term::sub_binary::write_sub_binary;

    #[test]
    fn tuple_copy_survives_source_heap_reset_and_copies_out_to_new_heap() {
        let mut source_heap = Heap::new(16);
        let original = {
            let words = source_heap.alloc_slice(3).expect("source tuple allocation");
            boxed::write_tuple(words, &[Term::small_int(1), Term::small_int(2)])
                .expect("source tuple write")
        };
        let original_ptr = original.heap_ptr().expect("tuple has heap pointer");

        let owned = copy_term_to_ets(original).expect("copy into ETS");
        let owned_ptr = owned
            .root()
            .heap_ptr()
            .expect("owned tuple has heap pointer");
        assert_ne!(owned_ptr, original_ptr);
        assert_eq!(owned.allocation_count(), 1);

        source_heap.reset_young();

        let mut target_heap = Heap::new(16);
        let copied = owned
            .copy_to_heap(&mut target_heap)
            .expect("copy out of ETS");
        let copied_ptr = copied.heap_ptr().expect("copied tuple has heap pointer");
        assert_ne!(copied_ptr, owned_ptr);
        assert!(target_heap.contains(copied_ptr));

        let tuple = Tuple::new(copied).expect("copied tuple accessor");
        assert_eq!(tuple.get(0), Some(Term::small_int(1)));
        assert_eq!(tuple.get(1), Some(Term::small_int(2)));
    }

    #[test]
    fn ets_owned_term_survives_source_heap_drop() {
        let owned = {
            let mut source_heap = Heap::new(16);
            let original = {
                let words = source_heap.alloc_slice(2).expect("source tuple allocation");
                boxed::write_tuple(words, &[Term::pid(55)]).expect("source tuple write")
            };
            copy_term_to_ets(original).expect("copy into ETS")
        };

        let mut target_heap = Heap::new(16);
        let copied = owned
            .copy_to_heap(&mut target_heap)
            .expect("copy out of ETS");
        let tuple = Tuple::new(copied).expect("copied tuple accessor");
        assert_eq!(tuple.get(0), Some(Term::pid(55)));
    }

    #[test]
    fn nested_list_map_and_binary_copy_are_independent_of_source_heap() {
        let mut source_heap = Heap::new(64);
        let source = {
            let binary_words = source_heap.alloc_slice(3).expect("binary allocation");
            let binary =
                crate::term::binary::write_binary(binary_words, b"ets").expect("binary write");
            let cons_tail_words = source_heap.alloc_slice(2).expect("tail cons allocation");
            let tail =
                boxed::write_cons(cons_tail_words, binary, Term::NIL).expect("tail cons write");
            let cons_head_words = source_heap.alloc_slice(2).expect("head cons allocation");
            let list = boxed::write_cons(cons_head_words, Term::small_int(7), tail)
                .expect("head cons write");
            let map_words = source_heap.alloc_slice(4).expect("map allocation");
            let map = boxed::write_map(map_words, &[Term::small_int(1)], &[Term::pid(99)])
                .expect("map write");
            let tuple_words = source_heap.alloc_slice(3).expect("tuple allocation");
            boxed::write_tuple(tuple_words, &[list, map]).expect("tuple write")
        };

        let owned = copy_term_to_ets(source).expect("copy into ETS");
        source_heap.reset_young();

        let mut target_heap = Heap::new(64);
        let copied = owned.copy_to_heap(&mut target_heap).expect("copy out");
        let tuple = Tuple::new(copied).expect("tuple accessor");
        let copied_list = tuple.get(0).expect("list element");
        let first = Cons::new(copied_list).expect("first cons");
        assert_eq!(first.head(), Term::small_int(7));
        let second = Cons::new(first.tail()).expect("second cons");
        let copied_binary = Binary::new(second.head()).expect("binary accessor");
        assert_eq!(copied_binary.as_bytes(), b"ets");
        assert_eq!(second.tail(), Term::NIL);

        let copied_map = Map::new(tuple.get(1).expect("map element")).expect("map accessor");
        assert_eq!(copied_map.key(0), Some(Term::small_int(1)));
        assert_eq!(copied_map.value(0), Some(Term::pid(99)));
    }

    #[test]
    fn proc_bin_and_sub_binary_copy_as_independent_inline_binaries() {
        let shared = SharedBinary::new(b"0123456789".to_vec());
        let (owned_proc_bin, owned_sub_binary) = {
            let mut proc_heap = Heap::new(8);
            let proc_bin = {
                let words = proc_heap.alloc_slice(3).expect("proc bin allocation");
                write_proc_bin(words, &shared).expect("proc bin write")
            };
            let mut sub_heap = Heap::new(8);
            let sub_binary = {
                let words = sub_heap.alloc_slice(5).expect("sub binary allocation");
                write_sub_binary(words, proc_bin, 2, 4).expect("sub binary write")
            };
            (
                copy_term_to_ets(proc_bin).expect("copy proc bin into ETS"),
                copy_term_to_ets(sub_binary).expect("copy sub binary into ETS"),
            )
        };

        let mut target_heap = Heap::new(16);
        let copied_proc_bin = owned_proc_bin
            .copy_to_heap(&mut target_heap)
            .expect("copy proc bin out");
        let copied_proc_binary =
            Binary::new(copied_proc_bin).expect("copied proc bin is inline binary");
        assert_eq!(copied_proc_binary.as_bytes(), b"0123456789");

        let copied_sub_binary = owned_sub_binary
            .copy_to_heap(&mut target_heap)
            .expect("copy sub binary out");
        let copied_sub_binary =
            Binary::new(copied_sub_binary).expect("copied sub binary is inline binary");
        assert_eq!(copied_sub_binary.as_bytes(), b"2345");
    }
}
