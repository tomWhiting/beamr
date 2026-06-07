//! Runtime ETF decoder that allocates decoded terms on the calling process heap.

use crate::atom::AtomTable;
use crate::process::Process;
use crate::term::Term;
use crate::term::binary::{packed_word_count, write_binary};
use crate::term::boxed::{
    write_bigint, write_cons, write_float, write_map, write_reference, write_tuple,
};

use super::tags;

/// Maximum recursion depth for runtime ETF decoding.
pub const MAX_ETF_DEPTH: usize = 256;

/// Budget for decoding untrusted ETF bytes directly onto a process heap.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RuntimeDecodeBudget {
    /// Maximum allowed nesting depth.
    pub max_depth: usize,
    /// Maximum process-heap words this decode may allocate.
    pub max_heap_words: usize,
    /// Current recursive nesting depth.
    pub current_depth: usize,
    /// Process-heap words charged by this decode.
    pub heap_words_used: usize,
}

impl RuntimeDecodeBudget {
    /// Build a budget with the default nesting limit and caller-provided heap limit.
    #[must_use]
    pub const fn new(max_heap_words: usize) -> Self {
        Self::with_limits(MAX_ETF_DEPTH, max_heap_words)
    }

    /// Build a budget with explicit depth and heap limits.
    #[must_use]
    pub const fn with_limits(max_depth: usize, max_heap_words: usize) -> Self {
        Self {
            max_depth,
            max_heap_words,
            current_depth: 0,
            heap_words_used: 0,
        }
    }

    /// Build the default runtime budget for a process, using its remaining heap capacity.
    #[must_use]
    pub fn for_process(process: &Process) -> Self {
        Self::new(process.heap().available())
    }

    /// Enter one recursive ETF level.
    pub fn descend(&mut self) -> Result<(), DecodeError> {
        let Some(next_depth) = self.current_depth.checked_add(1) else {
            return Err(DecodeError::DepthExceeded);
        };
        if next_depth > self.max_depth {
            return Err(DecodeError::DepthExceeded);
        }
        self.current_depth = next_depth;
        Ok(())
    }

    /// Leave one recursive ETF level.
    pub fn ascend(&mut self) {
        self.current_depth = self.current_depth.saturating_sub(1);
    }

    /// Charge process-heap words against this decode budget.
    pub fn charge_heap(&mut self, words: usize) -> Result<(), DecodeError> {
        let Some(next_used) = self.heap_words_used.checked_add(words) else {
            return Err(DecodeError::HeapBudgetExceeded);
        };
        if next_used > self.max_heap_words {
            return Err(DecodeError::HeapBudgetExceeded);
        }
        self.heap_words_used = next_used;
        Ok(())
    }
}

impl Default for RuntimeDecodeBudget {
    fn default() -> Self {
        Self::new(usize::MAX)
    }
}

/// Runtime ETF decode failures.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum DecodeError {
    DepthExceeded,
    HeapBudgetExceeded,
    InvalidTag(u8),
    Truncated,
    InvalidAtom,
    InvalidUtf8,
}

/// Decode one complete ETF value and allocate boxed results on `process`'s heap.
pub fn decode_term(
    bytes: &[u8],
    process: &mut Process,
    atom_table: &AtomTable,
) -> Result<Term, DecodeError> {
    let mut cursor = Cursor::new(bytes);
    let version = cursor.read_u8()?;
    if version != tags::VERSION {
        return Err(DecodeError::InvalidTag(version));
    }

    let mut budget = RuntimeDecodeBudget::for_process(process);
    let term = decode_term_inner(&mut cursor, process, atom_table, &mut budget)?;
    if !cursor.is_empty() {
        return Err(DecodeError::InvalidTag(cursor.peek_u8()?));
    }
    Ok(term)
}

fn decode_term_inner(
    cursor: &mut Cursor<'_>,
    process: &mut Process,
    atom_table: &AtomTable,
    budget: &mut RuntimeDecodeBudget,
) -> Result<Term, DecodeError> {
    budget.descend()?;
    let result = decode_payload(cursor, process, atom_table, budget);
    budget.ascend();
    result
}

fn decode_payload(
    cursor: &mut Cursor<'_>,
    process: &mut Process,
    atom_table: &AtomTable,
    budget: &mut RuntimeDecodeBudget,
) -> Result<Term, DecodeError> {
    let tag = cursor.read_u8()?;
    match tag {
        tags::SMALL_INTEGER_EXT => Ok(Term::small_int(i64::from(cursor.read_u8()?))),
        tags::INTEGER_EXT => Ok(Term::small_int(i64::from(cursor.read_i32()?))),
        tags::NEW_FLOAT_EXT => decode_float(cursor, process, budget),
        tags::ATOM_UTF8_EXT => {
            let len = cursor.read_u16()? as usize;
            decode_atom(cursor, len, atom_table)
        }
        tags::SMALL_ATOM_UTF8_EXT => {
            let len = cursor.read_u8()? as usize;
            decode_atom(cursor, len, atom_table)
        }
        tags::ATOM_EXT => {
            let len = cursor.read_u16()? as usize;
            decode_latin1_atom(cursor, len, atom_table)
        }
        tags::SMALL_TUPLE_EXT => {
            let arity = cursor.read_u8()? as usize;
            decode_tuple(cursor, process, atom_table, budget, arity)
        }
        tags::LARGE_TUPLE_EXT => {
            let arity = cursor.read_u32()? as usize;
            decode_tuple(cursor, process, atom_table, budget, arity)
        }
        tags::NIL_EXT => Ok(Term::NIL),
        tags::STRING_EXT => decode_string(cursor, process, budget),
        tags::LIST_EXT => decode_list(cursor, process, atom_table, budget),
        tags::BINARY_EXT => decode_binary(cursor, process, budget),
        tags::SMALL_BIG_EXT => {
            let len = cursor.read_u8()? as usize;
            decode_big(cursor, process, budget, len)
        }
        tags::LARGE_BIG_EXT => {
            let len = cursor.read_u32()? as usize;
            decode_big(cursor, process, budget, len)
        }
        tags::MAP_EXT => decode_map(cursor, process, atom_table, budget),
        tags::EXPORT_EXT => decode_export(cursor, process, atom_table, budget),
        tags::NEW_PID_EXT => decode_pid(cursor, process, atom_table, budget, true),
        tags::PID_EXT => decode_pid(cursor, process, atom_table, budget, false),
        tags::NEWER_REFERENCE_EXT => decode_newer_reference(cursor, process, atom_table, budget),
        tags::REFERENCE_EXT => decode_reference(cursor, process, atom_table, budget),
        other => Err(DecodeError::InvalidTag(other)),
    }
}

fn decode_float(
    cursor: &mut Cursor<'_>,
    process: &mut Process,
    budget: &mut RuntimeDecodeBudget,
) -> Result<Term, DecodeError> {
    let value = f64::from_bits(cursor.read_u64()?);
    alloc_boxed(process, budget, 2, |heap| write_float(heap, value))
}

fn decode_atom(
    cursor: &mut Cursor<'_>,
    len: usize,
    atom_table: &AtomTable,
) -> Result<Term, DecodeError> {
    let bytes = cursor.read_bytes(len)?;
    let name = std::str::from_utf8(bytes).map_err(|_| DecodeError::InvalidUtf8)?;
    Ok(Term::atom(atom_table.intern(name)))
}

fn decode_latin1_atom(
    cursor: &mut Cursor<'_>,
    len: usize,
    atom_table: &AtomTable,
) -> Result<Term, DecodeError> {
    let bytes = cursor.read_bytes(len)?;
    let mut name = String::with_capacity(bytes.len());
    for byte in bytes {
        name.push(char::from(*byte));
    }
    Ok(Term::atom(atom_table.intern(&name)))
}

fn decode_tuple(
    cursor: &mut Cursor<'_>,
    process: &mut Process,
    atom_table: &AtomTable,
    budget: &mut RuntimeDecodeBudget,
    arity: usize,
) -> Result<Term, DecodeError> {
    let words = checked_words(1, arity)?;
    let mut elements = Vec::new();
    for _ in 0..arity {
        elements.push(decode_term_inner(cursor, process, atom_table, budget)?);
    }
    alloc_boxed(process, budget, words, |heap| write_tuple(heap, &elements))
}

fn decode_string(
    cursor: &mut Cursor<'_>,
    process: &mut Process,
    budget: &mut RuntimeDecodeBudget,
) -> Result<Term, DecodeError> {
    let len = cursor.read_u16()? as usize;
    let bytes = cursor.read_bytes(len)?;
    let mut tail = Term::NIL;
    if bytes.len().checked_mul(2).is_none() {
        return Err(DecodeError::HeapBudgetExceeded);
    }
    for byte in bytes.iter().rev().copied() {
        tail = alloc_boxed(process, budget, 2, |heap| {
            write_cons(heap, Term::small_int(i64::from(byte)), tail)
        })?;
    }
    Ok(tail)
}

fn decode_list(
    cursor: &mut Cursor<'_>,
    process: &mut Process,
    atom_table: &AtomTable,
    budget: &mut RuntimeDecodeBudget,
) -> Result<Term, DecodeError> {
    let len = cursor.read_u32()? as usize;
    if len.checked_mul(2).is_none() {
        return Err(DecodeError::HeapBudgetExceeded);
    }
    let mut elements = Vec::new();
    for _ in 0..len {
        elements.push(decode_term_inner(cursor, process, atom_table, budget)?);
    }
    let mut tail = decode_term_inner(cursor, process, atom_table, budget)?;
    for element in elements.iter().rev().copied() {
        tail = alloc_boxed(process, budget, 2, |heap| write_cons(heap, element, tail))?;
    }
    Ok(tail)
}

fn decode_binary(
    cursor: &mut Cursor<'_>,
    process: &mut Process,
    budget: &mut RuntimeDecodeBudget,
) -> Result<Term, DecodeError> {
    let len = cursor.read_u32()? as usize;
    let bytes = cursor.read_bytes(len)?;
    let words = checked_words(2, packed_word_count(bytes.len()))?;
    alloc_boxed(process, budget, words, |heap| write_binary(heap, bytes))
}

fn decode_big(
    cursor: &mut Cursor<'_>,
    process: &mut Process,
    budget: &mut RuntimeDecodeBudget,
    len: usize,
) -> Result<Term, DecodeError> {
    let sign = cursor.read_u8()?;
    let negative = match sign {
        0 => false,
        1 => true,
        _ => return Err(DecodeError::InvalidAtom),
    };
    let bytes = cursor.read_bytes(len)?;

    if let Some(term) = small_big_term(negative, bytes) {
        return Ok(term);
    }

    let mut limbs = Vec::new();
    for chunk in bytes.chunks(std::mem::size_of::<u64>()) {
        let mut limb = 0_u64;
        for (index, byte) in chunk.iter().copied().enumerate() {
            limb |= u64::from(byte) << (index * u8::BITS as usize);
        }
        limbs.push(limb);
    }
    while limbs.last().copied() == Some(0) && limbs.len() > 1 {
        limbs.pop();
    }
    let words = checked_words(3, limbs.len())?;
    alloc_boxed(process, budget, words, |heap| {
        write_bigint(heap, negative, &limbs)
    })
}

fn small_big_term(negative: bool, bytes: &[u8]) -> Option<Term> {
    if bytes.len() > std::mem::size_of::<u64>() {
        return None;
    }

    let mut magnitude = 0_u64;
    for (index, byte) in bytes.iter().copied().enumerate() {
        magnitude |= u64::from(byte) << (index * u8::BITS as usize);
    }

    if negative {
        let min_magnitude = (Term::SMALL_INT_MAX as u64) + 1;
        if magnitude == min_magnitude {
            Term::try_small_int(Term::SMALL_INT_MIN)
        } else {
            i64::try_from(magnitude)
                .ok()
                .and_then(|value| Term::try_small_int(-value))
        }
    } else {
        i64::try_from(magnitude).ok().and_then(Term::try_small_int)
    }
}

fn decode_map(
    cursor: &mut Cursor<'_>,
    process: &mut Process,
    atom_table: &AtomTable,
    budget: &mut RuntimeDecodeBudget,
) -> Result<Term, DecodeError> {
    let len = cursor.read_u32()? as usize;
    let words = checked_words(
        2,
        len.checked_mul(2).ok_or(DecodeError::HeapBudgetExceeded)?,
    )?;
    let mut keys = Vec::new();
    let mut values = Vec::new();
    for _ in 0..len {
        keys.push(decode_term_inner(cursor, process, atom_table, budget)?);
        values.push(decode_term_inner(cursor, process, atom_table, budget)?);
    }
    alloc_boxed(process, budget, words, |heap| {
        write_map(heap, &keys, &values)
    })
}

fn decode_export(
    cursor: &mut Cursor<'_>,
    process: &mut Process,
    atom_table: &AtomTable,
    budget: &mut RuntimeDecodeBudget,
) -> Result<Term, DecodeError> {
    let module = decode_term_inner(cursor, process, atom_table, budget)?;
    let function = decode_term_inner(cursor, process, atom_table, budget)?;
    let arity = decode_term_inner(cursor, process, atom_table, budget)?;
    let elements = [module, function, arity];
    alloc_boxed(process, budget, 1 + elements.len(), |heap| {
        write_tuple(heap, &elements)
    })
}

fn decode_pid(
    cursor: &mut Cursor<'_>,
    process: &mut Process,
    atom_table: &AtomTable,
    budget: &mut RuntimeDecodeBudget,
    new_pid: bool,
) -> Result<Term, DecodeError> {
    let _node = decode_term_inner(cursor, process, atom_table, budget)?;
    let id = u64::from(cursor.read_u32()?);
    let _serial = cursor.read_u32()?;
    if new_pid {
        let _creation = cursor.read_u32()?;
    } else {
        let _creation = cursor.read_u8()?;
    }
    Term::try_pid(id).ok_or(DecodeError::InvalidAtom)
}

fn decode_reference(
    cursor: &mut Cursor<'_>,
    process: &mut Process,
    atom_table: &AtomTable,
    budget: &mut RuntimeDecodeBudget,
) -> Result<Term, DecodeError> {
    let _node = decode_term_inner(cursor, process, atom_table, budget)?;
    let id = u64::from(cursor.read_u32()?);
    let _creation = cursor.read_u8()?;
    alloc_boxed(process, budget, 2, |heap| write_reference(heap, id))
}

fn decode_newer_reference(
    cursor: &mut Cursor<'_>,
    process: &mut Process,
    atom_table: &AtomTable,
    budget: &mut RuntimeDecodeBudget,
) -> Result<Term, DecodeError> {
    let len = cursor.read_u16()? as usize;
    let _node = decode_term_inner(cursor, process, atom_table, budget)?;
    let _creation = cursor.read_u32()?;
    let mut folded = 0_u64;
    for index in 0..len {
        let word = u64::from(cursor.read_u32()?);
        if index == 0 {
            folded = word;
        } else if index == 1 {
            folded = (folded << u32::BITS) | word;
        }
    }
    alloc_boxed(process, budget, 2, |heap| write_reference(heap, folded))
}

fn alloc_boxed<F>(
    process: &mut Process,
    budget: &mut RuntimeDecodeBudget,
    words: usize,
    write: F,
) -> Result<Term, DecodeError>
where
    F: FnOnce(&mut [u64]) -> Option<Term>,
{
    budget.charge_heap(words)?;
    let heap = process
        .heap_mut()
        .alloc_slice(words)
        .map_err(|_| DecodeError::HeapBudgetExceeded)?;
    write(heap).ok_or(DecodeError::HeapBudgetExceeded)
}

fn checked_words(base: usize, payload: usize) -> Result<usize, DecodeError> {
    base.checked_add(payload)
        .ok_or(DecodeError::HeapBudgetExceeded)
}

struct Cursor<'bytes> {
    bytes: &'bytes [u8],
    offset: usize,
}

impl<'bytes> Cursor<'bytes> {
    const fn new(bytes: &'bytes [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn is_empty(&self) -> bool {
        self.offset == self.bytes.len()
    }

    fn peek_u8(&self) -> Result<u8, DecodeError> {
        self.bytes
            .get(self.offset)
            .copied()
            .ok_or(DecodeError::Truncated)
    }

    fn read_u8(&mut self) -> Result<u8, DecodeError> {
        let byte = self.peek_u8()?;
        self.offset += 1;
        Ok(byte)
    }

    fn read_u16(&mut self) -> Result<u16, DecodeError> {
        let bytes = self.read_array::<2>()?;
        Ok(u16::from_be_bytes(bytes))
    }

    fn read_u32(&mut self) -> Result<u32, DecodeError> {
        let bytes = self.read_array::<4>()?;
        Ok(u32::from_be_bytes(bytes))
    }

    fn read_i32(&mut self) -> Result<i32, DecodeError> {
        let bytes = self.read_array::<4>()?;
        Ok(i32::from_be_bytes(bytes))
    }

    fn read_u64(&mut self) -> Result<u64, DecodeError> {
        let bytes = self.read_array::<8>()?;
        Ok(u64::from_be_bytes(bytes))
    }

    fn read_array<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        let bytes = self.read_bytes(N)?;
        let mut array = [0_u8; N];
        array.copy_from_slice(bytes);
        Ok(array)
    }

    fn read_bytes(&mut self, len: usize) -> Result<&'bytes [u8], DecodeError> {
        let Some(end) = self.offset.checked_add(len) else {
            return Err(DecodeError::Truncated);
        };
        let Some(bytes) = self.bytes.get(self.offset..end) else {
            return Err(DecodeError::Truncated);
        };
        self.offset = end;
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atom::Atom;
    use crate::etf::encode::encode_term;
    use crate::term::binary::Binary;
    use crate::term::boxed::{Cons, Map, Tuple};

    fn atoms() -> AtomTable {
        AtomTable::with_common_atoms()
    }

    #[test]
    fn runtime_budget_tracks_depth_and_heap_words() {
        let mut budget = RuntimeDecodeBudget::with_limits(1, 2);
        assert_eq!(budget.descend(), Ok(()));
        assert_eq!(budget.descend(), Err(DecodeError::DepthExceeded));
        budget.ascend();
        assert_eq!(budget.charge_heap(2), Ok(()));
        assert_eq!(budget.charge_heap(1), Err(DecodeError::HeapBudgetExceeded));
    }

    #[test]
    fn decode_small_integer() {
        let table = atoms();
        let mut process = Process::new(1, 64);
        let term = decode_term(
            &[tags::VERSION, tags::SMALL_INTEGER_EXT, 42],
            &mut process,
            &table,
        )
        .expect("small integer decodes");
        assert_eq!(term, Term::small_int(42));
    }

    #[test]
    fn decode_tuple_on_process_heap() {
        let table = atoms();
        let mut process = Process::new(1, 64);
        let bytes = [
            tags::VERSION,
            tags::SMALL_TUPLE_EXT,
            2,
            tags::SMALL_ATOM_UTF8_EXT,
            2,
            b'o',
            b'k',
            tags::SMALL_INTEGER_EXT,
            42,
        ];
        let term = decode_term(&bytes, &mut process, &table).expect("tuple decodes");
        let tuple = Tuple::new(term).expect("tuple term");
        assert_eq!(tuple.arity(), 2);
        assert_eq!(tuple.get(0), Some(Term::atom(Atom::OK)));
        assert_eq!(tuple.get(1), Some(Term::small_int(42)));
    }

    #[test]
    fn decode_string_ext_as_cons_list() {
        let table = atoms();
        let mut process = Process::new(1, 64);
        let bytes = [tags::VERSION, tags::STRING_EXT, 0, 3, 1, 2, 3];
        let term = decode_term(&bytes, &mut process, &table).expect("string list decodes");
        assert_list(&[1, 2, 3], term);
    }

    #[test]
    fn decode_list_ext_as_cons_list() {
        let table = atoms();
        let mut process = Process::new(1, 64);
        let bytes = [
            tags::VERSION,
            tags::LIST_EXT,
            0,
            0,
            0,
            3,
            tags::SMALL_INTEGER_EXT,
            1,
            tags::SMALL_INTEGER_EXT,
            2,
            tags::SMALL_INTEGER_EXT,
            3,
            tags::NIL_EXT,
        ];
        let term = decode_term(&bytes, &mut process, &table).expect("list decodes");
        assert_list(&[1, 2, 3], term);
    }

    #[test]
    fn encode_decode_round_trip_for_tuple() {
        let table = atoms();
        let mut source_heap = [0_u64; 3];
        let original = write_tuple(
            &mut source_heap,
            &[Term::atom(Atom::OK), Term::small_int(42)],
        )
        .expect("source tuple");
        let bytes = encode_term(original, &table).expect("encode");
        let mut process = Process::new(1, 64);
        let decoded = decode_term(&bytes, &mut process, &table).expect("decode");
        assert_eq!(decoded, original);
    }

    #[test]
    fn decode_binary_and_map() {
        let table = atoms();
        let mut process = Process::new(1, 64);
        let binary_bytes = [tags::VERSION, tags::BINARY_EXT, 0, 0, 0, 3, 1, 2, 3];
        let binary = decode_term(&binary_bytes, &mut process, &table).expect("binary decodes");
        assert_eq!(
            Binary::new(binary).expect("binary term").as_bytes(),
            &[1, 2, 3]
        );

        let map_bytes = [
            tags::VERSION,
            tags::MAP_EXT,
            0,
            0,
            0,
            1,
            tags::SMALL_ATOM_UTF8_EXT,
            1,
            b'a',
            tags::SMALL_INTEGER_EXT,
            1,
        ];
        let map = decode_term(&map_bytes, &mut process, &table).expect("map decodes");
        let map = Map::new(map).expect("map term");
        assert_eq!(map.len(), 1);
        assert_eq!(map.value(0), Some(Term::small_int(1)));
    }

    #[test]
    fn decode_bigint_small_or_boxed() {
        let table = atoms();
        let mut process = Process::new(1, 64);
        let small = [tags::VERSION, tags::SMALL_BIG_EXT, 2, 0, 0x2a, 0];
        assert_eq!(
            decode_term(&small, &mut process, &table),
            Ok(Term::small_int(42))
        );

        let boxed = [
            tags::VERSION,
            tags::SMALL_BIG_EXT,
            8,
            0,
            0xff,
            0xff,
            0xff,
            0xff,
            0xff,
            0xff,
            0xff,
            0x7f,
        ];
        let decoded = decode_term(&boxed, &mut process, &table).expect("boxed bigint decodes");
        assert!(decoded.is_boxed());
    }

    #[test]
    fn invalid_inputs_return_decode_errors() {
        let table = atoms();
        let mut process = Process::new(1, 64);
        assert_eq!(
            decode_term(&[], &mut process, &table),
            Err(DecodeError::Truncated)
        );
        assert_eq!(
            decode_term(&[0], &mut process, &table),
            Err(DecodeError::InvalidTag(0))
        );
        assert_eq!(
            decode_term(&[tags::VERSION, 0], &mut process, &table),
            Err(DecodeError::InvalidTag(0))
        );
        assert_eq!(
            decode_term(
                &[tags::VERSION, tags::SMALL_ATOM_UTF8_EXT, 1, 0xff],
                &mut process,
                &table,
            ),
            Err(DecodeError::InvalidUtf8)
        );
    }

    #[test]
    fn decode_reports_heap_budget_exhaustion() {
        let table = atoms();
        let mut process = Process::new(1, 1);
        let bytes = [tags::VERSION, tags::NEW_FLOAT_EXT, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(
            decode_term(&bytes, &mut process, &table),
            Err(DecodeError::HeapBudgetExceeded)
        );
    }

    fn assert_list(expected: &[i64], mut term: Term) {
        for value in expected {
            let cons = Cons::new(term).expect("cons cell");
            assert_eq!(cons.head(), Term::small_int(*value));
            term = cons.tail();
        }
        assert_eq!(term, Term::NIL);
    }
}
