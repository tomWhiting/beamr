//! Heap resource terms for raw file descriptor ownership.
//!
//! `FdResource` stores one heap-owned strong `Arc<FdInner>` reference as a raw
//! word. GC and process-exit cleanup must retain/release that raw reference
//! explicitly because heap words are not Rust values and will not drop on their
//! own.

use std::os::fd::RawFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use crate::io::ring::{CompletionRing, IoOp};
use crate::term::Term;
use crate::term::boxed::{BoxedHeader, BoxedTag};

/// Payload words in an FdResource boxed object.
pub const FD_RESOURCE_PAYLOAD_WORDS: usize = 1;
/// Total heap words in an FdResource boxed object.
pub const FD_RESOURCE_WORDS: usize = 1 + FD_RESOURCE_PAYLOAD_WORDS;

/// File descriptor lifecycle state stored in [`FdInner`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum FdState {
    Open = 0,
    Closing = 1,
    Closed = 2,
}

impl FdState {
    fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Open,
            1 => Self::Closing,
            _ => Self::Closed,
        }
    }
}

/// Reference-counted lifecycle manager for a raw file descriptor.
#[derive(Debug)]
pub struct FdInner {
    fd: RawFd,
    owner_pid: u64,
    state: AtomicU8,
}

impl FdInner {
    /// Creates a new open FD lifecycle manager.
    pub fn new(fd: RawFd, owner_pid: u64) -> Self {
        Self {
            fd,
            owner_pid,
            state: AtomicU8::new(FdState::Open as u8),
        }
    }

    /// Returns the managed raw file descriptor.
    #[must_use]
    pub fn fd(&self) -> RawFd {
        self.fd
    }

    /// Returns the PID that owns this resource.
    #[must_use]
    pub fn owner_pid(&self) -> u64 {
        self.owner_pid
    }

    /// Returns the current lifecycle state.
    #[must_use]
    pub fn state(&self) -> FdState {
        FdState::from_u8(self.state.load(Ordering::Acquire))
    }

    /// BIF-initiated async close. Returns true only for the transition that owns
    /// the close operation.
    pub fn explicit_close(&self, ring: &dyn CompletionRing) -> bool {
        if self.begin_close() {
            let _op_id = ring.submit(IoOp::Close { fd: self.fd });
            true
        } else {
            false
        }
    }

    /// Synchronous fallback close used when no completion ring is available.
    pub fn close_synchronously(&self) -> bool {
        if self.begin_close() {
            self.close_fd_synchronously();
            true
        } else {
            false
        }
    }

    fn begin_close(&self) -> bool {
        self.fd >= 0
            && self
                .state
                .compare_exchange(
                    FdState::Open as u8,
                    FdState::Closing as u8,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
    }

    fn close_fd_synchronously(&self) {
        // SAFETY: this path owns the Open -> Closing transition for `self.fd`, so
        // no other FdInner path in this process will close the same descriptor.
        let _result = unsafe { libc::close(self.fd) };
        self.state.store(FdState::Closed as u8, Ordering::Release);
    }
}

impl Drop for FdInner {
    fn drop(&mut self) {
        if self.begin_close() {
            self.close_fd_synchronously();
        }
    }
}

/// Borrowed accessor for an FdResource boxed term.
#[derive(Copy, Clone, Debug)]
pub struct FdResource {
    ptr: *const u64,
}

impl FdResource {
    /// Validates and builds an FdResource accessor.
    pub fn new(term: Term) -> Option<Self> {
        if !term.is_boxed() {
            return None;
        }
        let ptr = term.heap_ptr()?;
        // SAFETY: boxed terms point to a header word in caller-owned heap storage.
        let header = unsafe { *ptr };
        if BoxedHeader::tag(header) != Some(BoxedTag::FdResource) {
            return None;
        }
        if BoxedHeader::size(header) != FD_RESOURCE_PAYLOAD_WORDS {
            return None;
        }
        // SAFETY: validated FdResource layout has one payload word containing a
        // raw Arc pointer. Reject null before exposing accessors.
        if unsafe { *ptr.add(1) } == 0 {
            return None;
        }
        Some(Self { ptr })
    }

    /// Returns the raw file descriptor.
    #[must_use]
    pub fn fd(self) -> RawFd {
        self.inner_ref().fd()
    }

    /// Returns the owning process PID.
    #[must_use]
    pub fn owner_pid(self) -> u64 {
        self.inner_ref().owner_pid()
    }

    /// Returns the current FD lifecycle state.
    #[must_use]
    pub fn state(self) -> FdState {
        self.inner_ref().state()
    }

    /// Clones the resource lifecycle Arc for use outside the heap term.
    #[must_use]
    pub fn inner(self) -> Arc<FdInner> {
        clone_fd_inner_from_raw_word(self.arc_ptr_word())
    }

    fn inner_ref(self) -> &'static FdInner {
        let ptr = self.arc_ptr_word() as *const FdInner;
        // SAFETY: FdResource heap words own a strong `Arc<FdInner>` reference, so
        // the pointed-to FdInner remains live while the heap object is live.
        unsafe { &*ptr }
    }

    fn arc_ptr_word(self) -> u64 {
        // SAFETY: validated FdResource payload word one stores the raw Arc ptr.
        unsafe { *self.ptr.add(1) }
    }
}

/// Writes an FdResource layout (`header, raw Arc<FdInner> pointer`) into `heap`.
pub fn write_fd_resource(heap: &mut [u64], fd_inner: Arc<FdInner>) -> Option<Term> {
    if heap.len() < FD_RESOURCE_WORDS {
        return None;
    }

    heap[0] = BoxedHeader::new(BoxedTag::FdResource, FD_RESOURCE_PAYLOAD_WORDS);
    heap[1] = Arc::into_raw(fd_inner) as u64;

    Some(Term::boxed_ptr(heap.as_ptr()))
}

pub(crate) fn clone_fd_inner_from_raw_word(raw: u64) -> Arc<FdInner> {
    let ptr = raw as *const FdInner;
    // SAFETY: FdResource writers store pointers produced by `Arc::into_raw` for
    // `Arc<FdInner>`. Reconstitute the heap-owned strong reference only long
    // enough to clone it, then convert it back to raw so ownership remains in the
    // heap word.
    let arc = unsafe { Arc::from_raw(ptr) };
    let cloned = Arc::clone(&arc);
    let _raw = Arc::into_raw(arc);
    cloned
}

pub(crate) fn retain_fd_inner_arc(ptr: *const u64) {
    let raw = read_raw_word(ptr, 1);
    let arc_ptr = raw as *const FdInner;
    // SAFETY: FdResource word one stores a raw `Arc<FdInner>` pointer created by
    // `Arc::into_raw`. Rebuild the source strong reference temporarily, clone it
    // for the copied FdResource, then put both strong references back into raw
    // form so the two heap objects own independent Arc counts.
    let source = unsafe { Arc::from_raw(arc_ptr) };
    let copied = Arc::clone(&source);
    let _source_raw = Arc::into_raw(source);
    let copied_raw = Arc::into_raw(copied);
    write_raw_word(ptr, 1, copied_raw as u64);
}

pub(crate) fn release_fd_inner_arc(ptr: *const u64) {
    let raw = read_raw_word(ptr, 1);
    if raw == 0 {
        return;
    }
    let arc_ptr = raw as *const FdInner;
    // SAFETY: FdResource word one stores a raw `Arc<FdInner>` pointer created by
    // `Arc::into_raw`. Rebuild exactly that heap-owned strong reference and let
    // it drop to release this heap object's ownership of the FD lifecycle.
    let _source = unsafe { Arc::from_raw(arc_ptr) };
    write_raw_word(ptr, 1, 0);
}

pub(crate) fn close_owned_resource_at(ptr: *const u64, owner_pid: u64) {
    let raw = read_raw_word(ptr, 1);
    if raw == 0 {
        return;
    }
    let inner = clone_fd_inner_from_raw_word(raw);
    if inner.owner_pid() == owner_pid {
        let _closed = inner.close_synchronously();
    }
}

fn read_raw_word(ptr: *const u64, offset: usize) -> u64 {
    // SAFETY: caller provides a live FdResource pointer and an offset within the
    // fixed resource layout.
    unsafe { *ptr.add(offset) }
}

fn write_raw_word(ptr: *const u64, offset: usize, value: u64) {
    // SAFETY: callers only pass heap object pointers while they have exclusive
    // access to the owning process or GC destination region.
    unsafe { *(ptr as *mut u64).add(offset) = value }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use crate::io::ring::IoCompletion;

    #[derive(Default)]
    struct MockRing {
        submitted: Mutex<Vec<IoOp>>,
    }

    impl CompletionRing for MockRing {
        fn submit(&self, op: IoOp) -> u64 {
            if let Ok(mut submitted) = self.submitted.lock() {
                submitted.push(op);
                submitted.len() as u64
            } else {
                0
            }
        }

        fn poll_completions(&self, _timeout: std::time::Duration) -> Vec<IoCompletion> {
            Vec::new()
        }

        fn pending_count(&self) -> usize {
            self.submitted.lock().map(|ops| ops.len()).unwrap_or(0)
        }

        fn shutdown(&self) {}
    }

    fn pipe_read_fd() -> RawFd {
        let mut fds = [0; 2];
        // SAFETY: `fds` points to two valid RawFd slots for libc to initialize.
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        assert_eq!(rc, 0);
        // SAFETY: close the write end so tests only manage the read end.
        let _closed = unsafe { libc::close(fds[1]) };
        fds[0]
    }

    fn fd_is_closed(fd: RawFd) -> bool {
        let mut byte = [0_u8; 1];
        // SAFETY: `byte` is a valid writable buffer for one-byte read attempts.
        let rc = unsafe { libc::read(fd, byte.as_mut_ptr().cast(), 1) };
        rc == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EBADF)
    }

    #[test]
    fn write_and_access_fd_resource_round_trips_fd_and_owner() {
        let fd = pipe_read_fd();
        let inner = Arc::new(FdInner::new(fd, 42));
        let retained = Arc::clone(&inner);
        let mut heap = [0_u64; FD_RESOURCE_WORDS];
        let term = write_fd_resource(&mut heap, retained).expect("fd resource should fit");

        assert_eq!(BoxedHeader::tag(heap[0]), Some(BoxedTag::FdResource));
        assert_eq!(BoxedHeader::size(heap[0]), FD_RESOURCE_PAYLOAD_WORDS);
        assert_ne!(heap[1], 0);
        let resource = FdResource::new(term).expect("valid fd resource");
        assert_eq!(resource.fd(), fd);
        assert_eq!(resource.owner_pid(), 42);
        assert_eq!(resource.state(), FdState::Open);
        assert_eq!(Arc::strong_count(&inner), 2);

        release_fd_inner_arc(heap.as_ptr());
        drop(inner);
        assert!(fd_is_closed(fd));
    }

    #[test]
    fn write_fd_resource_rejects_too_small_heap_slice() {
        let fd = pipe_read_fd();
        let inner = Arc::new(FdInner::new(fd, 7));
        let mut heap = [0_u64; 1];
        assert!(write_fd_resource(&mut heap, inner).is_none());
        assert!(fd_is_closed(fd));
    }

    #[test]
    fn fd_resource_accessor_rejects_invalid_tag_size_and_null_pointer() {
        let mut heap = [0_u64; FD_RESOURCE_WORDS];
        heap[0] = BoxedHeader::new(BoxedTag::Tuple, FD_RESOURCE_PAYLOAD_WORDS);
        heap[1] = 1;
        assert!(FdResource::new(Term::boxed_ptr(heap.as_ptr())).is_none());

        heap[0] = BoxedHeader::new(BoxedTag::FdResource, 2);
        assert!(FdResource::new(Term::boxed_ptr(heap.as_ptr())).is_none());

        heap[0] = BoxedHeader::new(BoxedTag::FdResource, FD_RESOURCE_PAYLOAD_WORDS);
        heap[1] = 0;
        assert!(FdResource::new(Term::boxed_ptr(heap.as_ptr())).is_none());
    }

    #[test]
    fn last_arc_drop_closes_fd() {
        let fd = pipe_read_fd();
        let inner = Arc::new(FdInner::new(fd, 1));
        drop(inner);
        assert!(fd_is_closed(fd));
    }

    #[test]
    fn explicit_close_then_drop_does_not_submit_or_close_twice() {
        let fd = pipe_read_fd();
        let inner = Arc::new(FdInner::new(fd, 1));
        let ring = MockRing::default();

        assert!(inner.explicit_close(&ring));
        assert!(!inner.explicit_close(&ring));
        assert_eq!(inner.state(), FdState::Closing);
        let submitted_len = ring.submitted.lock().map(|ops| ops.len()).unwrap_or(0);
        assert_eq!(submitted_len, 1);
        drop(inner);

        // The mock ring accepted ownership of the async close but does not execute
        // it; close here so the test process does not leak the descriptor.
        // SAFETY: the descriptor was not synchronously closed by Drop after the
        // explicit close transitioned the state to Closing.
        let _closed = unsafe { libc::close(fd) };
    }
}
