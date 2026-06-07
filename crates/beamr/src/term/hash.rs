//! Deterministic hashing for ETS term keys.
//!
//! `Term` intentionally does not implement [`std::hash::Hash`] globally. ETS
//! hash tables need hashing that is consistent with exact term equality, so this
//! module provides an explicit hasher plus an ETS-only key newtype.

use std::hash::{Hash, Hasher};

use super::{
    Term,
    binary_ref::BinaryRef,
    boxed::{BigInt, Closure, Cons, Float, Map, Reference, Tuple},
    compare,
};

/// ETS-only key wrapper for hash maps keyed by arbitrary BEAM terms.
#[derive(Copy, Clone, Debug)]
pub struct EtsKey(Term);

impl EtsKey {
    #[must_use]
    pub const fn new(term: Term) -> Self {
        Self(term)
    }

    #[must_use]
    pub const fn term(self) -> Term {
        self.0
    }
}

impl PartialEq for EtsKey {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Eq for EtsKey {}

impl Hash for EtsKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(term_hash(self.0));
    }
}

/// Computes a deterministic hash consistent with exact term equality.
#[must_use]
pub fn term_hash(term: Term) -> u64 {
    let mut state = StableHasher::default();
    hash_term(term, &mut state);
    state.finish()
}

#[derive(Default)]
struct StableHasher {
    hash: u64,
}

impl StableHasher {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    fn mix(&mut self, value: u64) {
        let mut bytes = value.to_le_bytes();
        for byte in &mut bytes {
            self.hash ^= u64::from(*byte);
            self.hash = self.hash.wrapping_mul(Self::PRIME);
        }
    }
}

impl Hasher for StableHasher {
    fn finish(&self) -> u64 {
        if self.hash == 0 {
            Self::OFFSET_BASIS
        } else {
            self.hash
        }
    }

    fn write(&mut self, bytes: &[u8]) {
        if self.hash == 0 {
            self.hash = Self::OFFSET_BASIS;
        }
        for byte in bytes {
            self.hash ^= u64::from(*byte);
            self.hash = self.hash.wrapping_mul(Self::PRIME);
        }
    }

    fn write_u8(&mut self, i: u8) {
        self.write(&[i]);
    }

    fn write_u64(&mut self, i: u64) {
        self.mix(i);
    }

    fn write_usize(&mut self, i: usize) {
        self.mix(i as u64);
    }
}

#[derive(Copy, Clone)]
enum HashKind {
    SmallInt = 1,
    Atom = 2,
    Pid = 3,
    Nil = 4,
    Tuple = 5,
    Float = 6,
    BigInt = 7,
    Closure = 8,
    Map = 9,
    Reference = 10,
    Binary = 11,
    List = 12,
    Other = 13,
}

fn hash_term(term: Term, state: &mut StableHasher) {
    if let Some(value) = term.as_small_int() {
        hash_kind(HashKind::SmallInt, state);
        state.write_u64(value as u64);
    } else if let Some(atom) = term.as_atom() {
        hash_kind(HashKind::Atom, state);
        state.write_u64(u64::from(atom.index()));
    } else if let Some(pid) = term.as_pid() {
        hash_kind(HashKind::Pid, state);
        state.write_u64(pid);
    } else if term.is_nil() {
        hash_kind(HashKind::Nil, state);
    } else if let Some(tuple) = Tuple::new(term) {
        hash_tuple(tuple, state);
    } else if let Some(float) = Float::new(term) {
        hash_kind(HashKind::Float, state);
        state.write_u64(float.value().to_bits());
    } else if let Some(bigint) = BigInt::new(term) {
        hash_bigint(bigint, state);
    } else if let Some(closure) = Closure::new(term) {
        hash_closure(closure, state);
    } else if let Some(map) = Map::new(term) {
        hash_map(map, state);
    } else if let Some(reference) = Reference::new(term) {
        hash_kind(HashKind::Reference, state);
        state.write_u64(reference.id());
    } else if let Some(binary) = BinaryRef::new(term) {
        hash_kind(HashKind::Binary, state);
        state.write(binary.as_bytes());
    } else if term.is_list() {
        hash_list(term, state);
    } else {
        hash_kind(HashKind::Other, state);
        state.write_u64(term.raw());
    }
}

fn hash_kind(kind: HashKind, state: &mut StableHasher) {
    state.write_u8(kind as u8);
}

fn hash_tuple(tuple: Tuple, state: &mut StableHasher) {
    hash_kind(HashKind::Tuple, state);
    state.write_usize(tuple.arity());
    for index in 0..tuple.arity() {
        if let Some(element) = tuple.get(index) {
            hash_term(element, state);
        }
    }
}

fn hash_bigint(bigint: BigInt, state: &mut StableHasher) {
    hash_kind(HashKind::BigInt, state);
    let limbs = normalized_limbs(bigint);
    let negative = bigint.is_negative() && !limbs.is_empty();
    state.write_u8(u8::from(negative));
    state.write_usize(limbs.len());
    for limb in limbs {
        state.write_u64(*limb);
    }
}

fn normalized_limbs(bigint: BigInt) -> &'static [u64] {
    let limbs = bigint.limbs();
    let significant_len = limbs
        .iter()
        .rposition(|limb| *limb != 0)
        .map_or(0, |index| index + 1);
    &limbs[..significant_len]
}

fn hash_closure(closure: Closure, state: &mut StableHasher) {
    hash_kind(HashKind::Closure, state);
    match closure.module() {
        Some(module) => {
            state.write_u8(1);
            state.write_u64(u64::from(module.index()));
        }
        None => state.write_u8(0),
    }
    state.write_u64(closure.function_index());
    state.write_u8(closure.arity());
    state.write_u64(closure.generation());
    state.write_u64(closure.unique_id());
    state.write_usize(closure.num_free());
    for index in 0..closure.num_free() {
        if let Some(free_var) = closure.free_var(index) {
            hash_term(free_var, state);
        }
    }
}

fn hash_map(map: Map, state: &mut StableHasher) {
    hash_kind(HashKind::Map, state);
    let mut entries = map_entries(map);
    entries.sort_by(|left, right| compare::exact_cmp(left.key, right.key));
    state.write_usize(entries.len());
    for entry in entries {
        hash_term(entry.key, state);
        hash_term(entry.value, state);
    }
}

fn map_entries(map: Map) -> Vec<MapEntry> {
    let mut entries = Vec::with_capacity(map.len());
    for index in 0..map.len() {
        if let (Some(key), Some(value)) = (map.key(index), map.value(index)) {
            entries.push(MapEntry { key, value });
        }
    }
    entries
}

#[derive(Copy, Clone)]
struct MapEntry {
    key: Term,
    value: Term,
}

fn hash_list(mut term: Term, state: &mut StableHasher) {
    hash_kind(HashKind::List, state);
    loop {
        match Cons::new(term) {
            Some(cons) => {
                state.write_u8(1);
                hash_term(cons.head(), state);
                term = cons.tail();
            }
            None => {
                state.write_u8(0);
                hash_term(term, state);
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    use super::*;
    use crate::atom::Atom;
    use crate::term::{Term, binary::write_binary, boxed};

    #[test]
    fn equal_atoms_and_integers_hash_equal() {
        assert_eq!(
            term_hash(Term::atom(Atom::OK)),
            term_hash(Term::atom(Atom::OK))
        );
        assert_eq!(
            term_hash(Term::small_int(42)),
            term_hash(Term::small_int(42))
        );
    }

    #[test]
    fn variety_of_different_terms_hash_differently() {
        let mut binary_heap = [0_u64; 3];
        let binary = write_binary(&mut binary_heap, b"ok").expect("binary fits");
        let terms = [
            Term::small_int(42),
            Term::small_int(43),
            Term::atom(Atom::OK),
            Term::atom(Atom::ERROR),
            Term::NIL,
            binary,
        ];

        for (left_index, left) in terms.iter().copied().enumerate() {
            for right in terms[left_index + 1..].iter().copied() {
                assert_ne!(term_hash(left), term_hash(right));
            }
        }
    }

    #[test]
    fn independently_allocated_equal_tuples_lists_and_binaries_hash_equal() {
        let mut left_tuple_heap = [0_u64; 3];
        let mut right_tuple_heap = [0_u64; 3];
        let left_tuple = boxed::write_tuple(
            &mut left_tuple_heap,
            &[Term::atom(Atom::OK), Term::small_int(1)],
        )
        .expect("tuple fits");
        let right_tuple = boxed::write_tuple(
            &mut right_tuple_heap,
            &[Term::atom(Atom::OK), Term::small_int(1)],
        )
        .expect("tuple fits");
        assert_eq!(left_tuple, right_tuple);
        assert_eq!(term_hash(left_tuple), term_hash(right_tuple));

        let mut left_tail_heap = [0_u64; 2];
        let mut left_head_heap = [0_u64; 2];
        let mut right_tail_heap = [0_u64; 2];
        let mut right_head_heap = [0_u64; 2];
        let left_tail = boxed::write_cons(&mut left_tail_heap, Term::small_int(2), Term::NIL)
            .expect("tail fits");
        let left_list = boxed::write_cons(&mut left_head_heap, Term::small_int(1), left_tail)
            .expect("head fits");
        let right_tail = boxed::write_cons(&mut right_tail_heap, Term::small_int(2), Term::NIL)
            .expect("tail fits");
        let right_list = boxed::write_cons(&mut right_head_heap, Term::small_int(1), right_tail)
            .expect("head fits");
        assert_eq!(left_list, right_list);
        assert_eq!(term_hash(left_list), term_hash(right_list));

        let mut left_binary_heap = [0_u64; 3];
        let mut right_binary_heap = [0_u64; 3];
        let left_binary = write_binary(&mut left_binary_heap, b"same").expect("binary fits");
        let right_binary = write_binary(&mut right_binary_heap, b"same").expect("binary fits");
        assert_eq!(left_binary, right_binary);
        assert_eq!(term_hash(left_binary), term_hash(right_binary));
    }

    #[test]
    fn ets_key_hash_matches_equality_contract() {
        let mut left_heap = [0_u64; 3];
        let mut right_heap = [0_u64; 3];
        let left = boxed::write_tuple(&mut left_heap, &[Term::atom(Atom::OK), Term::small_int(1)])
            .expect("tuple fits");
        let right =
            boxed::write_tuple(&mut right_heap, &[Term::atom(Atom::OK), Term::small_int(1)])
                .expect("tuple fits");
        let left_key = EtsKey::new(left);
        let right_key = EtsKey::new(right);

        assert_eq!(left_key, right_key);
        let mut left_hasher = DefaultHasher::new();
        let mut right_hasher = DefaultHasher::new();
        left_key.hash(&mut left_hasher);
        right_key.hash(&mut right_hasher);
        assert_eq!(left_hasher.finish(), right_hasher.finish());
    }

    #[test]
    fn exact_distinct_numeric_terms_hash_differently() {
        let mut heap = [0_u64; 2];
        let float = boxed::write_float(&mut heap, 1.0).expect("float fits");
        assert_ne!(Term::small_int(1), float);
        assert_ne!(term_hash(Term::small_int(1)), term_hash(float));
    }
}
