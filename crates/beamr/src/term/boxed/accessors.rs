//! Borrowed accessor structs for reading boxed term layouts.

use crate::atom::Atom;
use crate::term::{Term, shared_binary::SharedBinary};

use super::{BoxedHeader, BoxedTag};

/// Borrowed accessor for a tuple boxed term.
#[derive(Copy, Clone, Debug)]
pub struct Tuple {
    ptr: *const u64,
}

impl Tuple {
    pub fn new(term: Term) -> Option<Self> {
        let ptr = header_ptr(term, BoxedTag::Tuple)?;
        Some(Self { ptr })
    }

    pub fn arity(self) -> usize {
        BoxedHeader::size(self.header())
    }

    pub fn get(self, index: usize) -> Option<Term> {
        if index < self.arity() {
            Some(Term::from_raw(self.word(1 + index)))
        } else {
            None
        }
    }

    fn header(self) -> u64 {
        self.word(0)
    }

    fn word(self, offset: usize) -> u64 {
        // SAFETY: instances are only built from term pointers to stack/heap word
        // arrays created by this module; callers must keep the backing storage
        // alive while using the borrowed accessor.
        unsafe { *self.ptr.add(offset) }
    }
}

/// Borrowed accessor for a list cons cell.
#[derive(Copy, Clone, Debug)]
pub struct Cons {
    ptr: *const u64,
}

impl Cons {
    pub fn new(term: Term) -> Option<Self> {
        if !term.is_list() {
            return None;
        }

        Some(Self {
            ptr: term.heap_ptr()?,
        })
    }

    pub fn head(self) -> Term {
        Term::from_raw(self.word(0))
    }

    pub fn tail(self) -> Term {
        Term::from_raw(self.word(1))
    }

    fn word(self, offset: usize) -> u64 {
        // SAFETY: see Tuple::word; cons accessors read the fixed two-word cell.
        unsafe { *self.ptr.add(offset) }
    }
}

/// Borrowed accessor for a boxed float.
#[derive(Copy, Clone, Debug)]
pub struct Float {
    ptr: *const u64,
}

impl Float {
    pub fn new(term: Term) -> Option<Self> {
        let ptr = header_ptr(term, BoxedTag::Float)?;
        Some(Self { ptr })
    }

    pub fn value(self) -> f64 {
        // SAFETY: float payload is one u64 word immediately after the header.
        f64::from_bits(unsafe { *self.ptr.add(1) })
    }
}

/// Borrowed accessor for a boxed big integer storage layout.
#[derive(Copy, Clone, Debug)]
pub struct BigInt {
    ptr: *const u64,
}

impl BigInt {
    pub fn new(term: Term) -> Option<Self> {
        let ptr = header_ptr(term, BoxedTag::BigInt)?;
        Some(Self { ptr })
    }

    pub fn is_negative(self) -> bool {
        self.word(1) == super::BIGINT_NEGATIVE_SIGN
    }

    pub fn limb_count(self) -> usize {
        self.word(2) as usize
    }

    pub fn limbs(self) -> &'static [u64] {
        let count = self.limb_count();
        // SAFETY: the limb count is written by write_bigint, and the returned
        // borrow points into caller-owned heap storage that must outlive use.
        unsafe { std::slice::from_raw_parts(self.ptr.add(3), count) }
    }

    fn word(self, offset: usize) -> u64 {
        // SAFETY: see Tuple::word.
        unsafe { *self.ptr.add(offset) }
    }
}

/// Borrowed accessor for a boxed closure.
#[derive(Copy, Clone, Debug)]
pub struct Closure {
    ptr: *const u64,
}

impl Closure {
    pub fn new(term: Term) -> Option<Self> {
        let ptr = header_ptr(term, BoxedTag::Closure)?;
        // SAFETY: `header_ptr` returned a boxed closure header pointer.
        let header = unsafe { *ptr };
        let size = BoxedHeader::size(header);
        if size < 6 {
            return None;
        }

        // SAFETY: closure payloads of size at least six contain the num_free
        // word at offset four. Reject inconsistent sizes before exposing the
        // accessor so metadata/free-var reads stay within the boxed object.
        let num_free = unsafe { *ptr.add(4) } as usize;
        if size != 6 + num_free {
            return None;
        }

        Some(Self { ptr })
    }

    pub fn module(self) -> Option<Atom> {
        Term::from_raw(self.word(1)).as_atom()
    }

    pub fn function_index(self) -> u64 {
        self.word(2)
    }

    pub fn arity(self) -> u8 {
        self.word(3) as u8
    }

    pub fn num_free(self) -> usize {
        self.word(4) as usize
    }

    pub fn generation(self) -> u64 {
        self.word(5)
    }

    pub fn unique_id(self) -> u64 {
        self.word(6)
    }

    pub fn free_var(self, index: usize) -> Option<Term> {
        if index < self.num_free() {
            Some(Term::from_raw(self.word(7 + index)))
        } else {
            None
        }
    }

    fn word(self, offset: usize) -> u64 {
        // SAFETY: see Tuple::word.
        unsafe { *self.ptr.add(offset) }
    }
}

/// Borrowed accessor for a flatmap boxed term.
#[derive(Copy, Clone, Debug)]
pub struct Map {
    ptr: *const u64,
}

impl Map {
    pub fn new(term: Term) -> Option<Self> {
        let ptr = header_ptr(term, BoxedTag::Map)?;
        Some(Self { ptr })
    }

    pub fn len(self) -> usize {
        self.word(1) as usize
    }

    pub fn is_empty(self) -> bool {
        self.len() == 0
    }

    pub fn key(self, index: usize) -> Option<Term> {
        if index < self.len() {
            Some(Term::from_raw(self.word(2 + index)))
        } else {
            None
        }
    }

    pub fn value(self, index: usize) -> Option<Term> {
        if index < self.len() {
            Some(Term::from_raw(self.word(2 + self.len() + index)))
        } else {
            None
        }
    }

    pub fn get(self, key: Term) -> Option<Term> {
        (0..self.len()).find_map(|index| {
            if self.key(index) == Some(key) {
                self.value(index)
            } else {
                None
            }
        })
    }

    fn word(self, offset: usize) -> u64 {
        // SAFETY: see Tuple::word.
        unsafe { *self.ptr.add(offset) }
    }
}

/// Borrowed accessor for an off-heap reference-counted binary.
#[derive(Copy, Clone, Debug)]
pub struct ProcBin {
    ptr: *const u64,
}

impl ProcBin {
    pub fn new(term: Term) -> Option<Self> {
        let ptr = header_ptr(term, BoxedTag::ProcBin)?;
        // SAFETY: `header_ptr` returned a boxed ProcBin header pointer.
        let header = unsafe { *ptr };
        if BoxedHeader::size(header) != 2 {
            return None;
        }
        // SAFETY: validated ProcBin layout has two payload words; word two is
        // the raw Arc pointer and must be present/non-null before access.
        if unsafe { *ptr.add(2) } == 0 {
            return None;
        }

        Some(Self { ptr })
    }

    pub fn as_bytes(self) -> &'static [u8] {
        SharedBinary::bytes_from_raw_word(self.arc_ptr_word())
    }

    pub fn len(self) -> usize {
        self.as_bytes().len()
    }

    pub fn is_empty(self) -> bool {
        self.len() == 0
    }

    pub fn shared_binary(self) -> SharedBinary {
        SharedBinary::clone_from_raw_word(self.arc_ptr_word())
    }

    fn arc_ptr_word(self) -> u64 {
        // SAFETY: ProcBin payload word two stores the raw `Arc<Vec<u8>>` pointer.
        unsafe { *self.ptr.add(2) }
    }
}

/// Borrowed accessor for a boxed reference.
#[derive(Copy, Clone, Debug)]
pub struct Reference {
    ptr: *const u64,
}

impl Reference {
    pub fn new(term: Term) -> Option<Self> {
        let ptr = header_ptr(term, BoxedTag::Reference)?;
        Some(Self { ptr })
    }

    pub fn id(self) -> u64 {
        // SAFETY: reference payload is one u64 id immediately after the header.
        unsafe { *self.ptr.add(1) }
    }
}

fn header_ptr(term: Term, expected_tag: BoxedTag) -> Option<*const u64> {
    if !term.is_boxed() {
        return None;
    }

    let ptr = term.heap_ptr()?;
    // SAFETY: boxed terms point to a header word in caller-owned heap storage.
    let header = unsafe { *ptr };
    if BoxedHeader::tag(header) == Some(expected_tag) {
        Some(ptr)
    } else {
        None
    }
}
