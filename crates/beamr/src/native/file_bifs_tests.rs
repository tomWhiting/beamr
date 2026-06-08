use std::collections::VecDeque;
use std::io;
use std::os::fd::RawFd;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::atom::Atom;
use crate::io::resource::{FdInner, FdResource, FdState};
use crate::io::{CompletionRing, IoCompletion, IoOp, IoResult};
use crate::native::{FileIoCompletion, FileIoContinuation, FileIoFacility, ProcessContext};
use crate::process::Process;
use crate::term::Term;
use crate::term::binary::Binary;
use crate::term::boxed::Tuple;

use super::{file_close, file_open, file_read, file_write};

const PID: u64 = 42;
const CURRENT_POSITION: u64 = u64::MAX;

#[derive(Default)]
struct MockRing {
    next_op_id: Mutex<u64>,
    submitted: Mutex<Vec<IoOp>>,
}

impl MockRing {
    fn submitted(&self) -> Vec<IoOp> {
        self.submitted.lock().expect("submitted lock").clone()
    }
}

impl CompletionRing for MockRing {
    fn submit(&self, op: IoOp) -> u64 {
        self.submitted.lock().expect("submitted lock").push(op);
        let mut next = self.next_op_id.lock().expect("next op id lock");
        let op_id = *next;
        *next += 1;
        op_id
    }

    fn poll_completions(&self, _timeout: std::time::Duration) -> Vec<IoCompletion> {
        Vec::new()
    }

    fn pending_count(&self) -> usize {
        self.submitted.lock().map(|ops| ops.len()).unwrap_or(0)
    }

    fn shutdown(&self) {}
}

#[derive(Default)]
struct MockFileIoFacility {
    ring: MockRing,
    pending: Mutex<Vec<(u64, u64, FileIoContinuation)>>,
    completions: Mutex<VecDeque<FileIoCompletion>>,
}

impl MockFileIoFacility {
    fn push_completion(&self, continuation: FileIoContinuation, result: io::Result<IoResult>) {
        self.completions
            .lock()
            .expect("completions lock")
            .push_back(FileIoCompletion {
                op_id: 1,
                continuation,
                completion: IoCompletion { op_id: 1, result },
            });
    }

    fn submitted(&self) -> Vec<IoOp> {
        self.ring.submitted()
    }

    fn tracked(&self) -> Vec<(u64, u64, FileIoContinuation)> {
        self.pending.lock().expect("pending lock").clone()
    }
}

impl FileIoFacility for MockFileIoFacility {
    fn submit_file_io(&self, pid: u64, op: IoOp, continuation: FileIoContinuation) -> u64 {
        let op_id = self.ring.submit(op);
        self.track_submitted_file_io(pid, op_id, continuation);
        op_id
    }

    fn track_submitted_file_io(&self, pid: u64, op_id: u64, continuation: FileIoContinuation) {
        self.pending
            .lock()
            .expect("pending lock")
            .push((pid, op_id, continuation));
    }

    fn take_file_io_completion(&self, _pid: u64) -> Option<FileIoCompletion> {
        self.completions
            .lock()
            .expect("completions lock")
            .pop_front()
    }

    fn cancel_pending_file_io_for_pid(&self, pid: u64) {
        self.pending
            .lock()
            .expect("pending lock")
            .retain(|(pending_pid, _, _)| *pending_pid != pid);
    }

    fn ring(&self) -> &dyn CompletionRing {
        &self.ring
    }
}

fn heap_context<'a>(
    process: &'a mut Process,
    facility: Arc<MockFileIoFacility>,
) -> ProcessContext<'a> {
    let mut context = ProcessContext::new();
    context.set_file_io_facility(Some(facility));
    context.attach_process(process, 0);
    context
}

fn binary(context: &mut ProcessContext<'_>, bytes: &[u8]) -> Term {
    context.alloc_binary(bytes).expect("binary allocation")
}

fn list(context: &mut ProcessContext<'_>, values: &[Term]) -> Term {
    context.alloc_list(values).expect("list allocation")
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

fn tuple_reason(term: Term) -> (Term, Term) {
    let tuple = Tuple::new(term).expect("tuple result");
    (tuple.get(0).expect("tag"), tuple.get(1).expect("reason"))
}

fn error_reason(term: Term) -> Term {
    let (tag, reason) = tuple_reason(term);
    assert_eq!(tag, Term::atom(Atom::ERROR));
    reason
}

#[test]
fn open_resource_completion_returns_fd_resource() {
    let facility = Arc::new(MockFileIoFacility::default());
    let fd = pipe_read_fd();
    facility.push_completion(FileIoContinuation::Open, Ok(IoResult::Opened(fd)));
    let mut process = Process::new(PID, 64);
    let mut context = heap_context(&mut process, facility);

    let result = file_open(&[], &mut context).expect("open completion result");
    let resource = FdResource::new(result).expect("fd resource");
    assert_eq!(resource.fd(), fd);
    assert_eq!(resource.owner_pid(), PID);
}

#[test]
fn open_file_submits_openat_with_parsed_modes_and_suspends() {
    let facility = Arc::new(MockFileIoFacility::default());
    let mut process = Process::new(PID, 128);
    let mut context = heap_context(&mut process, Arc::clone(&facility));
    let filename = binary(&mut context, b"/tmp/beamr-open-test");
    let modes = list(
        &mut context,
        &[
            Term::atom(Atom::READ),
            Term::atom(Atom::WRITE),
            Term::atom(Atom::APPEND),
        ],
    );

    let result = file_open(&[filename, modes], &mut context).expect("open submit placeholder");
    assert_eq!(result, Term::atom(Atom::OK));
    assert!(context.take_suspend().is_some());

    let submitted = facility.submitted();
    assert_eq!(submitted.len(), 1);
    match &submitted[0] {
        IoOp::Openat {
            dir_fd,
            path,
            flags,
            mode,
        } => {
            assert_eq!(*dir_fd, libc::AT_FDCWD);
            assert_eq!(path, &PathBuf::from("/tmp/beamr-open-test"));
            assert_eq!(*mode, 0o644);
            assert_eq!(*flags & libc::O_ACCMODE, libc::O_RDWR);
            assert_ne!(*flags & libc::O_APPEND, 0);
        }
        other => panic!("expected Openat, got {other:?}"),
    }
}

#[test]
fn open_file_maps_nonexistent_error_to_enoent() {
    let facility = Arc::new(MockFileIoFacility::default());
    facility.push_completion(
        FileIoContinuation::Open,
        Err(io::Error::from_raw_os_error(libc::ENOENT)),
    );
    let mut process = Process::new(PID, 64);
    let mut context = heap_context(&mut process, facility);

    let result = file_open(&[], &mut context).expect("error tuple");
    assert_eq!(error_reason(result), Term::atom(Atom::ENOENT));
}

#[test]
fn close_file_submits_close_tracks_resource_and_marks_closed_on_completion() {
    let facility = Arc::new(MockFileIoFacility::default());
    let mut process = Process::new(PID, 128);
    let mut context = heap_context(&mut process, Arc::clone(&facility));
    let fd = pipe_read_fd();
    let resource = context
        .alloc_fd_resource(Arc::new(FdInner::new(fd, PID)))
        .expect("fd resource allocation");

    let result = file_close(&[resource], &mut context).expect("close submit placeholder");
    assert_eq!(result, Term::atom(Atom::OK));
    assert!(context.take_suspend().is_some());
    assert!(
        matches!(facility.submitted().as_slice(), [IoOp::Close { fd: submitted_fd }] if *submitted_fd == fd)
    );
    assert_eq!(facility.tracked().len(), 1);

    let inner = match &facility.tracked()[0].2 {
        FileIoContinuation::Close { fd } => Arc::clone(fd),
        other => panic!("expected close continuation, got {other:?}"),
    };
    facility.push_completion(
        FileIoContinuation::Close { fd: inner },
        Ok(IoResult::Closed),
    );
    let result = file_close(&[resource], &mut context).expect("close completion result");
    assert_eq!(result, Term::atom(Atom::OK));
    assert_eq!(
        FdResource::new(resource).expect("fd resource").state(),
        FdState::Closed
    );
}

#[test]
fn close_file_already_closed_returns_closed_error() {
    let facility = Arc::new(MockFileIoFacility::default());
    let mut process = Process::new(PID, 128);
    let mut context = heap_context(&mut process, Arc::clone(&facility));
    let inner = Arc::new(FdInner::new(-1, PID));
    inner.mark_closed();
    let resource = context
        .alloc_fd_resource(inner)
        .expect("fd resource allocation");

    let result = file_close(&[resource], &mut context).expect("closed error tuple");
    assert_eq!(error_reason(result), Term::atom(Atom::CLOSED));
    assert!(facility.submitted().is_empty());
}

#[test]
fn read_file_submits_current_position_read_and_returns_binary_or_eof() {
    let facility = Arc::new(MockFileIoFacility::default());
    let mut process = Process::new(PID, 128);
    let mut context = heap_context(&mut process, Arc::clone(&facility));
    let fd = pipe_read_fd();
    let resource = context
        .alloc_fd_resource(Arc::new(FdInner::new(fd, PID)))
        .expect("fd resource allocation");

    let result =
        file_read(&[resource, Term::small_int(5)], &mut context).expect("read submit placeholder");
    assert_eq!(result, Term::atom(Atom::OK));
    assert!(context.take_suspend().is_some());
    assert!(matches!(
        facility.submitted().as_slice(),
        [IoOp::Read {
            fd: submitted_fd,
            buf_len: 5,
            offset: CURRENT_POSITION,
        }] if *submitted_fd == fd
    ));

    facility.push_completion(
        FileIoContinuation::Read,
        Ok(IoResult::BytesRead(3, b"abc".to_vec())),
    );
    let result = file_read(&[resource, Term::small_int(5)], &mut context).expect("read result");
    let (tag, bytes) = tuple_reason(result);
    assert_eq!(tag, Term::atom(Atom::OK));
    assert_eq!(Binary::new(bytes).expect("binary").as_bytes(), b"abc");

    facility.push_completion(
        FileIoContinuation::Read,
        Ok(IoResult::BytesRead(0, Vec::new())),
    );
    let result = file_read(&[resource, Term::small_int(5)], &mut context).expect("eof result");
    let (tag, bytes) = tuple_reason(result);
    assert_eq!(tag, Term::atom(Atom::OK));
    assert_eq!(Binary::new(bytes).expect("binary").as_bytes(), b"");
}

#[test]
fn write_file_submits_current_position_write_and_reports_partial() {
    let facility = Arc::new(MockFileIoFacility::default());
    let mut process = Process::new(PID, 128);
    let mut context = heap_context(&mut process, Arc::clone(&facility));
    let fd = pipe_read_fd();
    let resource = context
        .alloc_fd_resource(Arc::new(FdInner::new(fd, PID)))
        .expect("fd resource allocation");
    let data = binary(&mut context, b"hello");

    let result = file_write(&[resource, data], &mut context).expect("write submit placeholder");
    assert_eq!(result, Term::atom(Atom::OK));
    assert!(context.take_suspend().is_some());
    assert!(matches!(
        facility.submitted().as_slice(),
        [IoOp::Write {
            fd: submitted_fd,
            data,
            offset: CURRENT_POSITION,
        }] if *submitted_fd == fd && data.as_slice() == b"hello"
    ));

    facility.push_completion(
        FileIoContinuation::Write { expected_len: 5 },
        Ok(IoResult::BytesWritten(5)),
    );
    let result = file_write(&[resource, data], &mut context).expect("full write result");
    assert_eq!(result, Term::atom(Atom::OK));

    facility.push_completion(
        FileIoContinuation::Write { expected_len: 5 },
        Ok(IoResult::BytesWritten(2)),
    );
    let result = file_write(&[resource, data], &mut context).expect("partial write result");
    let reason = error_reason(result);
    let tuple = Tuple::new(reason).expect("incomplete tuple");
    assert_eq!(tuple.get(0), Some(Term::atom(Atom::INCOMPLETE)));
    assert_eq!(tuple.get(1), Some(Term::small_int(2)));
}

#[test]
fn read_and_write_reject_closed_resource() {
    let facility = Arc::new(MockFileIoFacility::default());
    let mut process = Process::new(PID, 128);
    let mut context = heap_context(&mut process, Arc::clone(&facility));
    let inner = Arc::new(FdInner::new(-1, PID));
    inner.mark_closed();
    let resource = context
        .alloc_fd_resource(inner)
        .expect("fd resource allocation");
    let data = binary(&mut context, b"hello");

    let read_result =
        file_read(&[resource, Term::small_int(1)], &mut context).expect("closed read error tuple");
    assert_eq!(error_reason(read_result), Term::atom(Atom::CLOSED));

    let write_result =
        file_write(&[resource, data], &mut context).expect("closed write error tuple");
    assert_eq!(error_reason(write_result), Term::atom(Atom::CLOSED));
    assert!(facility.submitted().is_empty());
}
