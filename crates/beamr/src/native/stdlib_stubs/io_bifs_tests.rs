use std::sync::{Arc, Mutex};

use crate::atom::{Atom, AtomTable};
use crate::native::io_message::IoMessageFacility;
use crate::native::{ProcessContext, SelectFacility};
use crate::process::Process;
use crate::term::Term;
use crate::term::binary::{self, Binary};
use crate::term::boxed::{Tuple, write_cons};

use super::{bif_init_stop, io_bifs};

fn binary(bytes: &[u8]) -> Term {
    let data_words = binary::packed_word_count(bytes.len());
    let heap = Box::leak(vec![0u64; 2 + data_words].into_boxed_slice());
    binary::write_binary(heap, bytes).expect("binary heap sized correctly")
}

fn assert_binary(term: Term, expected: &[u8]) {
    let binary = Binary::new(term).expect("binary term");
    assert_eq!(binary.as_bytes(), expected);
}

fn list(elements: &[Term]) -> Term {
    let mut tail = Term::NIL;
    for element in elements.iter().rev() {
        tail = write_cons(Box::leak(Box::new([0u64; 2])), *element, tail).expect("cons");
    }
    tail
}

fn atom_context(process: &mut Process) -> ProcessContext<'_> {
    let table = Arc::new(AtomTable::with_common_atoms());
    let mut ctx = ProcessContext::new();
    ctx.set_atom_table(Some(table));
    ctx.attach_process(process, 0);
    ctx
}

#[derive(Default)]
struct RecordingIoMessages(Mutex<Vec<(u64, u64, Term)>>);

impl RecordingIoMessages {
    fn assert_request(
        &self,
        index: usize,
        sender_pid: u64,
        target_pid: u64,
        request_name: &str,
        payload: &[u8],
        ctx: &ProcessContext,
    ) {
        let messages = self.0.lock().expect("recorded messages lock");
        let (sender, target, message) = messages.get(index).copied().expect("recorded io request");
        assert_eq!(sender, sender_pid);
        assert_eq!(target, target_pid);
        let tuple = Tuple::new(message).expect("io_request tuple");
        assert_eq!(tuple.arity(), 4);
        assert_eq!(
            atom_name(tuple.get(0).expect("request tag"), ctx),
            "io_request"
        );
        assert_eq!(tuple.get(1), Some(Term::pid(sender_pid)));
        let request = Tuple::new(tuple.get(3).expect("request payload")).expect("request tuple");
        assert_eq!(request.arity(), 3);
        assert_eq!(
            atom_name(request.get(0).expect("request name"), ctx),
            request_name
        );
        assert_eq!(
            atom_name(request.get(1).expect("request encoding"), ctx),
            "unicode"
        );
        assert_binary(request.get(2).expect("request bytes"), payload);
    }
}

impl IoMessageFacility for RecordingIoMessages {
    fn send_message(&self, sender_pid: u64, target_pid: u64, message: Term) -> bool {
        self.0
            .lock()
            .expect("recorded messages lock")
            .push((sender_pid, target_pid, message));
        true
    }
}

struct EmptySelect;

impl SelectFacility for EmptySelect {
    fn message_count(&self) -> usize {
        0
    }

    fn peek_message(&self, _index: usize) -> Option<Term> {
        None
    }

    fn remove_message(&self, _index: usize) {}
}

fn atom_name(term: Term, ctx: &ProcessContext) -> String {
    let atom = term.as_atom().expect("atom term");
    ctx.atom_table()
        .expect("atom table")
        .resolve(atom)
        .expect("known atom")
        .to_owned()
}

#[test]
fn io_put_chars_uses_group_leader_protocol() {
    let mut process = Process::new(1, 256);
    process.set_group_leader(Term::pid(7));
    let mut ctx = atom_context(&mut process);
    let messages = Arc::new(RecordingIoMessages::default());
    let io_messages: Arc<dyn IoMessageFacility> = messages.clone();
    ctx.set_io_message_facility(Some(io_messages));
    ctx.set_select_facility(Some(Arc::new(EmptySelect)));

    assert_eq!(
        io_bifs::bif_io_format_2(&[binary(b"~s"), list(&[binary(b"hello")])], &mut ctx),
        Ok(Term::atom(Atom::OK))
    );
    assert!(ctx.take_suspend().is_some());
    messages.assert_request(0, 1, 7, "put_chars", b"hello", &ctx);
}

#[test]
fn io_format_helpers_and_init_stop_are_covered() {
    let mut process = Process::new(1, 256);
    let mut ctx = atom_context(&mut process);

    assert_binary(
        io_bifs::bif_io_lib_format_2(
            &[
                binary(b"~s ~s"),
                list(&[binary(b"hello"), binary(b"world")]),
            ],
            &mut ctx,
        )
        .expect("io_lib format"),
        b"hello world",
    );
    assert_binary(
        io_bifs::bif_io_lib_format_2(
            &[
                list(&[
                    Term::small_int(i64::from(b'~')),
                    Term::small_int(i64::from(b's')),
                ]),
                list(&[binary(b"iodata-format")]),
            ],
            &mut ctx,
        )
        .expect("io_lib format accepts Erlang string format"),
        b"iodata-format",
    );

    assert_eq!(
        bif_init_stop(&[Term::small_int(0)], &mut ctx),
        Ok(Term::atom(Atom::OK))
    );
    assert!(ctx.take_shutdown_request());
}

#[test]
fn io_get_line_sends_get_line_request() {
    let mut process = Process::new(2, 256);
    process.set_group_leader(Term::pid(7));
    let mut ctx = atom_context(&mut process);
    let messages = Arc::new(RecordingIoMessages::default());
    let io_messages: Arc<dyn IoMessageFacility> = messages.clone();
    ctx.set_io_message_facility(Some(io_messages));
    ctx.set_select_facility(Some(Arc::new(EmptySelect)));

    assert_eq!(
        io_bifs::bif_io_get_line_1(&[binary(b"> ")], &mut ctx),
        Ok(Term::atom(Atom::OK))
    );
    assert!(ctx.take_suspend().is_some());
    messages.assert_request(0, 2, 7, "get_line", b"> ", &ctx);
}
