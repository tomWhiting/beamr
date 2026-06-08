use std::collections::VecDeque;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::atom::{Atom, AtomTable};
use crate::io::{CompletionRing, IoCompletion, IoOp, IoResult, StatxData};
use crate::native::{
    BifRegistry, BifRegistryImpl, FileIoCompletion, FileIoContinuation, FileIoFacility,
    ProcessContext,
};
use crate::process::Process;
use crate::term::Term;
use crate::term::binary::Binary;
use crate::term::boxed::{Cons, Tuple};

use super::{del_dir, del_file, file_info, list_dir, make_dir, register_file_meta_bifs, rename};

const PID: u64 = 105;

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
}

impl FileIoFacility for MockFileIoFacility {
    fn submit_file_io(&self, _pid: u64, op: IoOp, _continuation: FileIoContinuation) -> u64 {
        self.ring.submit(op)
    }

    fn track_submitted_file_io(&self, _pid: u64, op_id: u64, continuation: FileIoContinuation) {
        self.completions
            .lock()
            .expect("completions lock")
            .push_back(FileIoCompletion {
                op_id,
                continuation,
                completion: IoCompletion {
                    op_id,
                    result: Ok(IoResult::Completed),
                },
            });
    }

    fn take_file_io_completion(&self, _pid: u64) -> Option<FileIoCompletion> {
        self.completions
            .lock()
            .expect("completions lock")
            .pop_front()
    }

    fn ring(&self) -> &dyn CompletionRing {
        &self.ring
    }
}

fn heap_context<'a>(
    process: &'a mut Process,
    atom_table: Arc<AtomTable>,
    facility: Arc<MockFileIoFacility>,
) -> ProcessContext<'a> {
    let mut context = ProcessContext::new();
    context.set_atom_table(Some(atom_table));
    context.set_file_io_facility(Some(facility));
    context.attach_process(process, 0);
    context
}

fn binary(context: &mut ProcessContext<'_>, bytes: &[u8]) -> Term {
    context.alloc_binary(bytes).expect("binary allocation")
}

fn tuple(term: Term) -> Tuple {
    Tuple::new(term).expect("tuple result")
}

fn temp_path(name: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!("beamr-{name}-{}", std::process::id()));
    path
}

fn remove_path(path: &PathBuf) {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_error) => {}
    }
    match fs::remove_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_error) => {}
    }
}

#[test]
fn registers_file_metadata_bifs() {
    let atom_table = AtomTable::with_common_atoms();
    let registry = BifRegistryImpl::new();
    register_file_meta_bifs(&registry, &atom_table).expect("registration");
    let erlang = atom_table.intern("erlang");

    for (name, arity) in [
        ("file_info", 1),
        ("list_dir", 1),
        ("make_dir", 1),
        ("del_file", 1),
        ("del_dir", 1),
        ("rename", 2),
    ] {
        assert!(
            registry
                .lookup(erlang, atom_table.intern(name), arity)
                .is_some(),
            "missing erlang:{name}/{arity}"
        );
    }
}

#[test]
fn file_info_submits_statx_and_finishes_record_tuple() {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let facility = Arc::new(MockFileIoFacility::default());
    let mut process = Process::new(PID, 256);
    let mut context = heap_context(&mut process, Arc::clone(&atom_table), Arc::clone(&facility));
    let filename = binary(&mut context, b"/tmp/beamr-stat-test");

    let result = file_info(&[filename], &mut context).expect("submit placeholder");
    assert_eq!(result, Term::atom(Atom::OK));
    assert!(context.take_suspend().is_some());
    assert!(matches!(
        facility.submitted().as_slice(),
        [IoOp::Statx {
            dir_fd,
            path,
            flags,
            mask,
        }] if *dir_fd == libc::AT_FDCWD
            && path == &PathBuf::from("/tmp/beamr-stat-test")
            && *flags == libc::AT_SYMLINK_NOFOLLOW
            && *mask == super::statx_basic_stats_mask()
    ));

    facility.push_completion(
        FileIoContinuation::FileInfo,
        Ok(IoResult::StatResult(StatxData {
            mode: libc::S_IFREG | 0o644,
            size: 12,
            dev_major: 1,
            dev_minor: 2,
            inode: 99,
            nlink: 1,
            uid: 501,
            gid: 20,
            atime_sec: 10,
            mtime_sec: 11,
            ctime_sec: 12,
            ..StatxData::default()
        })),
    );
    let result = file_info(&[filename], &mut context).expect("file_info tuple");
    let tuple = tuple(result);
    assert_eq!(tuple.arity(), 14);
    assert_eq!(
        tuple.get(0),
        Some(Term::atom(atom_table.intern("file_info")))
    );
    assert_eq!(tuple.get(1), Some(Term::small_int(12)));
    assert_eq!(tuple.get(2), Some(Term::atom(atom_table.intern("regular"))));
    assert_eq!(
        tuple.get(3),
        Some(Term::atom(atom_table.intern("read_write")))
    );
    assert_eq!(tuple.get(4), Some(Term::small_int(10)));
    assert_eq!(tuple.get(5), Some(Term::small_int(11)));
    assert_eq!(tuple.get(6), Some(Term::small_int(12)));
    assert_eq!(tuple.get(8), Some(Term::small_int(1)));
    assert_eq!(tuple.get(11), Some(Term::small_int(99)));
}

#[test]
fn list_dir_returns_ok_list_of_filename_binaries() {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let facility = Arc::new(MockFileIoFacility::default());
    let mut process = Process::new(PID, 256);
    let mut context = heap_context(&mut process, atom_table, Arc::clone(&facility));
    let dirname = binary(&mut context, b"/tmp/beamr-list-test");

    let result = list_dir(&[dirname], &mut context).expect("submit placeholder");
    assert_eq!(result, Term::atom(Atom::OK));
    assert!(matches!(
        facility.submitted().as_slice(),
        [IoOp::ListDir { path }] if path == &PathBuf::from("/tmp/beamr-list-test")
    ));

    facility.push_completion(
        FileIoContinuation::ListDir,
        Ok(IoResult::DirList(vec![
            b"a.txt".to_vec(),
            b"b.bin".to_vec(),
        ])),
    );
    let result = list_dir(&[dirname], &mut context).expect("list_dir result");
    let tuple = tuple(result);
    assert_eq!(tuple.get(0), Some(Term::atom(Atom::OK)));
    let mut tail = tuple.get(1).expect("list");
    let mut names = Vec::new();
    while tail != Term::NIL {
        let cons = Cons::new(tail).expect("cons");
        names.push(
            Binary::new(cons.head())
                .expect("filename binary")
                .as_bytes()
                .to_vec(),
        );
        tail = cons.tail();
    }
    assert_eq!(names, vec![b"a.txt".to_vec(), b"b.bin".to_vec()]);
}

#[test]
fn metadata_operations_submit_and_finish_ok() {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let facility = Arc::new(MockFileIoFacility::default());
    let mut process = Process::new(PID, 256);
    let mut context = heap_context(&mut process, atom_table, Arc::clone(&facility));
    let dir = binary(&mut context, b"/tmp/beamr-meta-dir");
    let file = binary(&mut context, b"/tmp/beamr-meta-dir/file");
    let renamed = binary(&mut context, b"/tmp/beamr-meta-dir/renamed");

    assert_eq!(
        make_dir(&[dir], &mut context).expect("make_dir submit"),
        Term::atom(Atom::OK)
    );
    facility.push_completion(FileIoContinuation::MakeDir, Ok(IoResult::Completed));
    assert_eq!(
        make_dir(&[dir], &mut context).expect("make_dir completion"),
        Term::atom(Atom::OK)
    );

    assert_eq!(
        rename(&[file, renamed], &mut context).expect("rename submit"),
        Term::atom(Atom::OK)
    );
    facility.push_completion(FileIoContinuation::Rename, Ok(IoResult::Completed));
    assert_eq!(
        rename(&[file, renamed], &mut context).expect("rename completion"),
        Term::atom(Atom::OK)
    );

    assert_eq!(
        del_file(&[renamed], &mut context).expect("del_file submit"),
        Term::atom(Atom::OK)
    );
    facility.push_completion(FileIoContinuation::DelFile, Ok(IoResult::Completed));
    assert_eq!(
        del_file(&[renamed], &mut context).expect("del_file completion"),
        Term::atom(Atom::OK)
    );

    assert_eq!(
        del_dir(&[dir], &mut context).expect("del_dir submit"),
        Term::atom(Atom::OK)
    );
    facility.push_completion(FileIoContinuation::DelDir, Ok(IoResult::Completed));
    assert_eq!(
        del_dir(&[dir], &mut context).expect("del_dir completion"),
        Term::atom(Atom::OK)
    );

    assert!(matches!(
        facility.submitted().as_slice(),
        [
            IoOp::MakeDir { .. },
            IoOp::Rename { .. },
            IoOp::DelFile { .. },
            IoOp::DelDir { .. },
        ]
    ));
}

#[test]
fn blocking_helpers_cover_real_directory_cycle() {
    let dir = temp_path("meta-cycle-dir");
    let file = dir.join("file.txt");
    let renamed = dir.join("renamed.txt");
    remove_path(&file);
    remove_path(&renamed);
    remove_path(&dir);

    fs::create_dir(&dir).expect("create temp dir");
    fs::write(&file, b"beamr").expect("create file");
    let entries = super::read_dir_entries(&dir).expect("list temp dir");
    assert!(entries.contains(&b"file.txt".to_vec()));
    fs::rename(&file, &renamed).expect("rename file");
    fs::remove_file(&renamed).expect("remove file");
    fs::remove_dir(&dir).expect("remove dir");
}
