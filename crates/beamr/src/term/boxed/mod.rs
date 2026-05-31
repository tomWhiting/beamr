//! Boxed term headers, writers, and accessors.
//!
//! Header-tagged boxed values start with a word encoding the boxed type and
//! payload size, followed by payload words. Cons cells are the exception: they
//! are identified by the list primary tag on the pointing [`Term`] and contain
//! exactly two words (head, tail) with no header.

mod accessors;

pub use accessors::{BigInt, Closure, Cons, Float, Map, Reference, Tuple};

use crate::{atom::Atom, term::Term};

const HEADER_TAG_BITS: u32 = 8;
const HEADER_TAG_MASK: u64 = (1 << HEADER_TAG_BITS) - 1;
const BIGINT_NEGATIVE_SIGN: u64 = 1;

/// Header tag for a heap boxed value.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[repr(u8)]
pub enum BoxedTag {
    Tuple = 0x10,
    Float = 0x11,
    BigInt = 0x12,
    Closure = 0x13,
    Map = 0x14,
    Reference = 0x15,
    Binary = 0x16,
    BinaryBuilder = 0x17,
    MatchContext = 0x18,
}

impl BoxedTag {
    const fn from_bits(bits: u64) -> Option<Self> {
        match bits {
            bits if bits == Self::Tuple as u64 => Some(Self::Tuple),
            bits if bits == Self::Float as u64 => Some(Self::Float),
            bits if bits == Self::BigInt as u64 => Some(Self::BigInt),
            bits if bits == Self::Closure as u64 => Some(Self::Closure),
            bits if bits == Self::Map as u64 => Some(Self::Map),
            bits if bits == Self::Reference as u64 => Some(Self::Reference),
            bits if bits == Self::Binary as u64 => Some(Self::Binary),
            bits if bits == Self::BinaryBuilder as u64 => Some(Self::BinaryBuilder),
            bits if bits == Self::MatchContext as u64 => Some(Self::MatchContext),
            _ => None,
        }
    }
}

/// Constructor and extractor for boxed heap headers.
pub struct BoxedHeader;

impl BoxedHeader {
    /// Builds a header word from a boxed tag and payload size in words.
    #[allow(clippy::new_ret_no_self)]
    pub const fn new(tag: BoxedTag, size: usize) -> u64 {
        ((size as u64) << HEADER_TAG_BITS) | tag as u64
    }

    /// Extracts the boxed type tag from a header word.
    pub const fn tag(header_word: u64) -> Option<BoxedTag> {
        BoxedTag::from_bits(header_word & HEADER_TAG_MASK)
    }

    /// Extracts the payload size, in words, from a header word.
    pub const fn size(header_word: u64) -> usize {
        (header_word >> HEADER_TAG_BITS) as usize
    }
}

/// Writes a tuple layout (`header, elements...`) into `heap`.
pub fn write_tuple(heap: &mut [u64], elements: &[Term]) -> Option<Term> {
    if heap.len() < 1 + elements.len() {
        return None;
    }

    heap[0] = BoxedHeader::new(BoxedTag::Tuple, elements.len());
    for (slot, element) in heap[1..].iter_mut().zip(elements.iter()) {
        *slot = element.raw();
    }

    Some(Term::boxed_ptr(heap.as_ptr()))
}

/// Writes a cons cell layout (`head, tail`) into `heap`.
pub fn write_cons(heap: &mut [u64], head: Term, tail: Term) -> Option<Term> {
    if heap.len() < 2 {
        return None;
    }

    heap[0] = head.raw();
    heap[1] = tail.raw();

    Some(Term::list_ptr(heap.as_ptr()))
}

/// Writes a float layout (`header, f64 bits`) into `heap`.
pub fn write_float(heap: &mut [u64], value: f64) -> Option<Term> {
    if heap.len() < 2 {
        return None;
    }

    heap[0] = BoxedHeader::new(BoxedTag::Float, 1);
    heap[1] = value.to_bits();

    Some(Term::boxed_ptr(heap.as_ptr()))
}

/// Writes a big integer layout (`header, sign, limb_count, limbs...`) into `heap`.
pub fn write_bigint(heap: &mut [u64], negative: bool, limbs: &[u64]) -> Option<Term> {
    if heap.len() < 3 + limbs.len() {
        return None;
    }

    heap[0] = BoxedHeader::new(BoxedTag::BigInt, 2 + limbs.len());
    heap[1] = u64::from(negative);
    heap[2] = limbs.len() as u64;
    heap[3..3 + limbs.len()].copy_from_slice(limbs);

    Some(Term::boxed_ptr(heap.as_ptr()))
}

/// Writes a closure layout (`header, module, function_index, arity, num_free, free...`).
pub fn write_closure(
    heap: &mut [u64],
    module: Atom,
    function_index: u64,
    arity: u8,
    free_vars: &[Term],
) -> Option<Term> {
    if heap.len() < 5 + free_vars.len() {
        return None;
    }

    heap[0] = BoxedHeader::new(BoxedTag::Closure, 4 + free_vars.len());
    heap[1] = Term::atom(module).raw();
    heap[2] = function_index;
    heap[3] = u64::from(arity);
    heap[4] = free_vars.len() as u64;
    for (slot, free_var) in heap[5..].iter_mut().zip(free_vars.iter()) {
        *slot = free_var.raw();
    }

    Some(Term::boxed_ptr(heap.as_ptr()))
}

/// Writes a flatmap layout (`header, len, keys..., values...`) into `heap`.
pub fn write_map(heap: &mut [u64], keys: &[Term], values: &[Term]) -> Option<Term> {
    if keys.len() != values.len() || heap.len() < 2 + keys.len() + values.len() {
        return None;
    }

    heap[0] = BoxedHeader::new(BoxedTag::Map, 1 + keys.len() + values.len());
    heap[1] = keys.len() as u64;

    let key_start = 2;
    let value_start = key_start + keys.len();
    for (slot, key) in heap[key_start..value_start].iter_mut().zip(keys.iter()) {
        *slot = key.raw();
    }
    for (slot, value) in heap[value_start..value_start + values.len()]
        .iter_mut()
        .zip(values.iter())
    {
        *slot = value.raw();
    }

    Some(Term::boxed_ptr(heap.as_ptr()))
}

/// Writes a reference layout (`header, id`) into `heap`.
pub fn write_reference(heap: &mut [u64], id: u64) -> Option<Term> {
    if heap.len() < 2 {
        return None;
    }

    heap[0] = BoxedHeader::new(BoxedTag::Reference, 1);
    heap[1] = id;

    Some(Term::boxed_ptr(heap.as_ptr()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boxed_header_encodes_distinct_tags_and_payload_size() {
        let tags = [
            BoxedTag::Tuple,
            BoxedTag::Float,
            BoxedTag::BigInt,
            BoxedTag::Closure,
            BoxedTag::Map,
            BoxedTag::Reference,
            BoxedTag::Binary,
            BoxedTag::BinaryBuilder,
            BoxedTag::MatchContext,
        ];

        for (index, tag) in tags.iter().copied().enumerate() {
            let header = BoxedHeader::new(tag, index);
            assert_eq!(BoxedHeader::tag(header), Some(tag));
            assert_eq!(BoxedHeader::size(header), index);
            assert!((tag as u64) > 0b111);
        }

        for (left_index, left) in tags.iter().enumerate() {
            for right in &tags[left_index + 1..] {
                assert_ne!(*left as u8, *right as u8);
            }
        }
    }

    #[test]
    fn tuple_write_then_read_round_trip_and_bounds_check() {
        let elements = [Term::small_int(1), Term::atom(Atom::OK), Term::NIL];
        let mut heap = [0_u64; 4];
        let term = write_tuple(&mut heap, &elements).expect("tuple should fit");

        assert!(term.is_boxed());
        assert_eq!(BoxedHeader::tag(heap[0]), Some(BoxedTag::Tuple));
        assert_eq!(BoxedHeader::size(heap[0]), 3);
        assert_eq!(heap.len(), 4);

        let tuple = Tuple::new(term).expect("tuple accessor");
        assert_eq!(tuple.arity(), 3);
        assert_eq!(tuple.get(0), Some(elements[0]));
        assert_eq!(tuple.get(1), Some(elements[1]));
        assert_eq!(tuple.get(2), Some(elements[2]));
        assert_eq!(tuple.get(3), None);
    }

    #[test]
    fn empty_tuple_is_valid_one_word_boxed_value() {
        let mut heap = [0_u64; 1];
        let term = write_tuple(&mut heap, &[]).expect("empty tuple should fit");
        let tuple = Tuple::new(term).expect("tuple accessor");

        assert_eq!(BoxedHeader::tag(heap[0]), Some(BoxedTag::Tuple));
        assert_eq!(tuple.arity(), 0);
        assert_eq!(tuple.get(0), None);
    }

    #[test]
    fn cons_cell_write_then_read_round_trip_without_header() {
        let head = Term::small_int(1);
        let tail = Term::NIL;
        let mut heap = [0_u64; 2];
        let term = write_cons(&mut heap, head, tail).expect("cons should fit");

        assert!(term.is_list());
        assert!(!term.is_boxed());
        assert_ne!(term.tag(), Term::boxed_ptr(heap.as_ptr()).tag());
        assert_eq!(heap, [head.raw(), tail.raw()]);

        let cons = Cons::new(term).expect("cons accessor");
        assert_eq!(cons.head(), head);
        assert_eq!(cons.tail(), tail);
    }

    #[test]
    fn proper_list_has_three_cons_cells_ending_in_nil() {
        let mut cell3 = [0_u64; 2];
        let mut cell2 = [0_u64; 2];
        let mut cell1 = [0_u64; 2];

        let third = write_cons(&mut cell3, Term::small_int(3), Term::NIL).expect("third cell");
        let second = write_cons(&mut cell2, Term::small_int(2), third).expect("second cell");
        let first = write_cons(&mut cell1, Term::small_int(1), second).expect("first cell");

        assert_eq!(Cons::new(first).expect("first").head(), Term::small_int(1));
        let second_cons = Cons::new(Cons::new(first).expect("first").tail()).expect("second");
        assert_eq!(second_cons.head(), Term::small_int(2));
        let third_cons = Cons::new(second_cons.tail()).expect("third");
        assert_eq!(third_cons.head(), Term::small_int(3));
        assert_eq!(third_cons.tail(), Term::NIL);
    }

    #[test]
    fn float_write_then_read_round_trip() {
        for value in [3.125, 0.0, -1.5] {
            let mut heap = [0_u64; 2];
            let term = write_float(&mut heap, value).expect("float should fit");
            let float = Float::new(term).expect("float accessor");

            assert_eq!(BoxedHeader::tag(heap[0]), Some(BoxedTag::Float));
            assert_eq!(BoxedHeader::size(heap[0]), 1);
            assert_eq!(float.value(), value);
        }
    }

    #[test]
    fn bigint_write_then_read_round_trip() {
        let limbs = [0x0123_4567_89ab_cdef, 0xfedc_ba98_7654_3210];
        let mut heap = [0_u64; 5];
        let term = write_bigint(&mut heap, true, &limbs).expect("bigint should fit");
        let bigint = BigInt::new(term).expect("bigint accessor");

        assert_eq!(BoxedHeader::tag(heap[0]), Some(BoxedTag::BigInt));
        assert_eq!(BoxedHeader::size(heap[0]), 4);
        assert!(bigint.is_negative());
        assert_eq!(bigint.limb_count(), 2);
        assert_eq!(bigint.limbs(), limbs);
    }

    #[test]
    fn closure_write_then_read_round_trip_and_bounds_check() {
        let free_vars = [Term::small_int(42), Term::atom(Atom::ERROR)];
        let mut heap = [0_u64; 7];
        let term =
            write_closure(&mut heap, Atom::OK, 9, 2, &free_vars).expect("closure should fit");
        let closure = Closure::new(term).expect("closure accessor");

        assert_eq!(BoxedHeader::tag(heap[0]), Some(BoxedTag::Closure));
        assert_eq!(BoxedHeader::size(heap[0]), 6);
        assert_eq!(closure.module(), Some(Atom::OK));
        assert_eq!(closure.function_index(), 9);
        assert_eq!(closure.arity(), 2);
        assert_eq!(closure.num_free(), 2);
        assert_eq!(closure.free_var(0), Some(free_vars[0]));
        assert_eq!(closure.free_var(1), Some(free_vars[1]));
        assert_eq!(closure.free_var(2), None);
    }

    #[test]
    fn map_write_then_read_round_trip_and_linear_get() {
        let keys = [Term::small_int(1), Term::small_int(2)];
        let values = [Term::atom(Atom::OK), Term::atom(Atom::ERROR)];
        let mut heap = [0_u64; 6];
        let term = write_map(&mut heap, &keys, &values).expect("map should fit");
        let map = Map::new(term).expect("map accessor");

        assert_eq!(BoxedHeader::tag(heap[0]), Some(BoxedTag::Map));
        assert_eq!(BoxedHeader::size(heap[0]), 5);
        assert_eq!(map.len(), 2);
        assert_eq!(map.key(0), Some(keys[0]));
        assert_eq!(map.value(0), Some(values[0]));
        assert_eq!(map.get(keys[0]), Some(values[0]));
        assert_eq!(map.get(keys[1]), Some(values[1]));
        assert_eq!(map.get(Term::small_int(3)), None);
    }

    #[test]
    fn map_rejects_mismatched_key_value_counts() {
        let mut heap = [0_u64; 4];
        assert_eq!(write_map(&mut heap, &[Term::small_int(1)], &[]), None);
    }

    #[test]
    fn reference_write_then_read_round_trip() {
        let mut heap = [0_u64; 2];
        let term = write_reference(&mut heap, 0xfeed_face_cafe_beef).expect("reference should fit");
        let reference = Reference::new(term).expect("reference accessor");

        assert_eq!(BoxedHeader::tag(heap[0]), Some(BoxedTag::Reference));
        assert_eq!(BoxedHeader::size(heap[0]), 1);
        assert_eq!(reference.id(), 0xfeed_face_cafe_beef);
    }
}
