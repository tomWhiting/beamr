//! Garbage collection — each process cleans its own room.
//!
//! Per-process generational copying GC. Young generation (nursery) is collected
//! frequently; old generation is compacted rarely. Collection takes only
//! `&mut Process`, never a registry/table/scheduler lock, so collecting one
//! process cannot pause or mutate another process.
pub mod major;
pub mod minor;

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::Arc;

use crate::process::{Process, heap::HeapFull};
use crate::term::{
    Term,
    boxed::{BoxedHeader, BoxedTag},
};

/// Major-GC shrink threshold after full compaction.
pub const MAJOR_SHRINK_THRESHOLD: f64 = 0.25;

/// Result returned by GC entry points.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct GcStats {
    /// Number of live objects copied during this collection.
    pub copied_objects: usize,
    /// Number of machine words copied during this collection.
    pub copied_words: usize,
    /// Young words used when the collection started.
    pub young_before: usize,
    /// Old words used when the collection started.
    pub old_before: usize,
    /// Young words used when the collection completed.
    pub young_after: usize,
    /// Old words used when the collection completed.
    pub old_after: usize,
}

impl GcStats {
    fn new(process: &Process) -> Self {
        Self {
            copied_objects: 0,
            copied_words: 0,
            young_before: process.heap().young_used(),
            old_before: process.heap().old_used(),
            young_after: process.heap().young_used(),
            old_after: process.heap().old_used(),
        }
    }

    fn finish(&mut self, process: &Process) {
        self.young_after = process.heap().young_used();
        self.old_after = process.heap().old_used();
    }

    pub(crate) fn record_copy(&mut self, words: usize) {
        self.copied_objects += 1;
        self.copied_words += words;
    }
}

/// GC/allocation error.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum GcError {
    /// Allocation still could not be satisfied after permitted GC/growth.
    HeapFull(HeapFull),
    /// Object header did not match any known boxed layout.
    InvalidObjectHeader(u64),
}

impl fmt::Display for GcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HeapFull(error) => write!(f, "{error}"),
            Self::InvalidObjectHeader(header) => {
                write!(f, "invalid boxed object header word {header:#x}")
            }
        }
    }
}

impl std::error::Error for GcError {}

impl From<HeapFull> for GcError {
    fn from(error: HeapFull) -> Self {
        Self::HeapFull(error)
    }
}

pub(crate) type ForwardingMap = HashMap<usize, Term>;

/// Collect only the target process's nursery into old space.
pub fn collect_minor(process: &mut Process) -> Result<GcStats, GcError> {
    collect_minor_with_live(process, 256)
}

/// Collect only the target process's nursery using a live X-register prefix.
pub fn collect_minor_with_live(process: &mut Process, live_x: usize) -> Result<GcStats, GcError> {
    minor::collect(process, live_x)
}

/// Fully compact the target process heap into fresh old space.
pub fn collect_major(process: &mut Process) -> Result<GcStats, GcError> {
    major::collect(process)
}

/// Allocate in the process nursery, running per-process GC on HeapFull.
///
/// The policy is: try nursery allocation, minor collect and retry, grow the
/// nursery as needed, and run a full compaction only when promotion pressure
/// during minor GC requires old-space compaction. The function does not touch
/// any process except `process`.
pub fn alloc(process: &mut Process, words: usize) -> Result<*mut u64, GcError> {
    match process.heap_mut().alloc(words) {
        Ok(ptr) => return Ok(ptr),
        Err(_heap_full) => {}
    }

    ensure_space(process, words, 256)?;

    process.heap_mut().alloc(words).map_err(GcError::from)
}

/// Ensure `words` nursery words are available, collecting and growing if needed.
pub fn ensure_space(process: &mut Process, words: usize, live_x: usize) -> Result<(), GcError> {
    if process.heap().available() >= words {
        return Ok(());
    }

    match collect_minor_with_live(process, live_x) {
        Ok(_stats) => {}
        Err(GcError::HeapFull(_)) => {
            collect_major(process)?;
        }
        Err(error) => return Err(error),
    }

    if process.heap().available() >= words {
        return Ok(());
    }

    while process.heap().available() < words {
        process.heap_mut().grow_to_next_capacity_with_max()?;
    }
    Ok(())
}

pub(crate) fn new_stats(process: &Process) -> GcStats {
    GcStats::new(process)
}

pub(crate) fn finish_stats(stats: &mut GcStats, process: &Process) {
    stats.finish(process);
}

pub(crate) fn object_size(term: Term) -> Result<Option<usize>, GcError> {
    if term.is_list() {
        return Ok(Some(2));
    }

    if !term.is_boxed() {
        return Ok(None);
    }

    let Some(ptr) = term.heap_ptr() else {
        return Ok(None);
    };
    // SAFETY: boxed terms are constructed only from heap word pointers. GC calls
    // this before reclaiming source storage, while object headers are live.
    let header = unsafe { *ptr };
    let Some(_tag) = BoxedHeader::tag(header) else {
        return Err(GcError::InvalidObjectHeader(header));
    };
    Ok(Some(1 + BoxedHeader::size(header)))
}

pub(crate) fn term_from_ptr_like(original: Term, ptr: *const u64) -> Term {
    if original.is_list() {
        Term::list_ptr(ptr)
    } else {
        Term::boxed_ptr(ptr)
    }
}

pub(crate) fn rewrite_copied_object(
    term: Term,
    work_queue: &mut VecDeque<Term>,
    mut copy_term: impl FnMut(Term, &mut VecDeque<Term>) -> Result<Term, GcError>,
) -> Result<(), GcError> {
    let Some(ptr) = term.heap_ptr() else {
        return Ok(());
    };

    if term.is_list() {
        rewrite_word(ptr, 0, work_queue, &mut copy_term)?;
        rewrite_word(ptr, 1, work_queue, &mut copy_term)?;
        return Ok(());
    }

    let header = read_raw_word(ptr, 0);
    let Some(tag) = BoxedHeader::tag(header) else {
        return Err(GcError::InvalidObjectHeader(header));
    };

    match tag {
        BoxedTag::Tuple => {
            for offset in 1..=BoxedHeader::size(header) {
                rewrite_word(ptr, offset, work_queue, &mut copy_term)?;
            }
        }
        BoxedTag::Closure => {
            let num_free = read_raw_word(ptr, 4) as usize;
            for index in 0..num_free {
                rewrite_word(ptr, 7 + index, work_queue, &mut copy_term)?;
            }
        }
        BoxedTag::Map => {
            let len = read_raw_word(ptr, 1) as usize;
            for offset in 2..(2 + len * 2) {
                rewrite_word(ptr, offset, work_queue, &mut copy_term)?;
            }
        }
        BoxedTag::MatchContext => rewrite_word(ptr, 3, work_queue, &mut copy_term)?,
        BoxedTag::ProcBin => retain_proc_bin_arc(ptr),
        BoxedTag::Float
        | BoxedTag::BigInt
        | BoxedTag::Reference
        | BoxedTag::Binary
        | BoxedTag::BinaryBuilder => {}
    }

    Ok(())
}

fn rewrite_word(
    ptr: *const u64,
    offset: usize,
    work_queue: &mut VecDeque<Term>,
    copy_term: &mut impl FnMut(Term, &mut VecDeque<Term>) -> Result<Term, GcError>,
) -> Result<(), GcError> {
    let field = Term::from_raw(read_raw_word(ptr, offset));
    let rewritten = copy_term(field, work_queue)?;
    if rewritten.raw() != field.raw() {
        write_raw_word(ptr, offset, rewritten.raw());
    }
    Ok(())
}

fn retain_proc_bin_arc(ptr: *const u64) {
    let raw = read_raw_word(ptr, 2);
    let arc_ptr = raw as *const Vec<u8>;
    // SAFETY: ProcBin word two stores a raw `Arc<Vec<u8>>` pointer created by
    // `Arc::into_raw`. Rebuild the source strong reference temporarily, clone it
    // for the copied ProcBin, then put both strong references back into raw form
    // so the two heap objects own independent Arc counts.
    let source = unsafe { Arc::from_raw(arc_ptr) };
    let copied = Arc::clone(&source);
    let _source_raw = Arc::into_raw(source);
    let copied_raw = Arc::into_raw(copied);
    write_raw_word(ptr, 2, copied_raw as u64);
}

fn read_raw_word(ptr: *const u64, offset: usize) -> u64 {
    // SAFETY: caller provides a live copied object pointer and an offset within
    // the object's layout.
    unsafe { *ptr.add(offset) }
}

fn write_raw_word(ptr: *const u64, offset: usize, value: u64) {
    // SAFETY: copied objects live in this process's mutable heap during GC; no
    // aliases are used to read/write the same slot concurrently.
    unsafe { *(ptr as *mut u64).add(offset) = value }
}

#[cfg(test)]
pub(crate) mod tests;
