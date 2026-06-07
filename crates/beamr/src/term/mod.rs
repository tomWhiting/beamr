//! Term representation — what all data is made of.
//!
//! A term is a single 64-bit machine word with low-bit tagging.
//! Immediates (small integers, atoms, pids, nil) fit entirely in
//! the word. Boxed values (tuples, lists, binaries, floats, big
//! integers, closures, maps, references) are tagged pointers into
//! the process-local heap.
pub mod binary;
pub mod binary_ref;
pub mod boxed;
pub mod compare;
pub mod hash;
#[cfg(feature = "json")]
pub mod json;
pub mod shared_binary;
pub mod sub_binary;

use crate::atom::Atom;

const TAG_BITS: u32 = 3;
const TAG_MASK: u64 = (1 << TAG_BITS) - 1;
const PAYLOAD_BITS: u32 = u64::BITS - TAG_BITS;

const SMALL_INT_TAG: u64 = 0b000;
const ATOM_TAG: u64 = 0b001;
const PID_TAG: u64 = 0b010;
const NIL_TAG: u64 = 0b011;
const BOXED_TAG: u64 = 0b100;
const LIST_TAG: u64 = 0b101;

const SMALL_INT_MIN: i64 = -(1_i64 << (PAYLOAD_BITS - 1));
const SMALL_INT_MAX: i64 = (1_i64 << (PAYLOAD_BITS - 1)) - 1;
const UNSIGNED_PAYLOAD_MAX: u64 = (1_u64 << PAYLOAD_BITS) - 1;

/// Primary tag for a [`Term`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Tag {
    /// Signed small integer stored directly in the term payload.
    SmallInt,
    /// Atom index stored directly in the term payload.
    Atom,
    /// Local process identifier stored directly in the term payload.
    Pid,
    /// Distinguished empty list value.
    Nil,
    /// Boxed heap pointer tag, reserved for boxed values.
    Boxed,
    /// List heap pointer tag, reserved for cons cells.
    List,
}

/// A single tagged BEAM term word.
///
/// The low three bits hold the primary tag. The remaining bits hold immediate
/// payload data or, for future boxed/list terms, tagged heap pointer data.
#[derive(Copy, Clone, Debug)]
pub struct Term(u64);

impl PartialEq for Term {
    fn eq(&self, other: &Self) -> bool {
        compare::partial_eq(self, other)
    }
}

impl Eq for Term {}

impl PartialOrd for Term {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Term {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        compare::raw_cmp(*self, *other)
    }
}

impl Term {
    /// Distinguished empty list / nil value.
    pub const NIL: Self = Self(NIL_TAG);

    /// Minimum integer that can be represented as an immediate small integer.
    pub const SMALL_INT_MIN: i64 = SMALL_INT_MIN;

    /// Maximum integer that can be represented as an immediate small integer.
    pub const SMALL_INT_MAX: i64 = SMALL_INT_MAX;

    /// Maximum local pid payload that can be represented as an immediate pid.
    pub const PID_MAX: u64 = UNSIGNED_PAYLOAD_MAX;

    /// Creates a small integer term.
    ///
    /// This infallible constructor is for compile-time in-range constants. Use
    /// [`Term::try_small_int`] for runtime arithmetic that may produce
    /// out-of-range values needing big-integer boxing in a later phase. Passing
    /// an out-of-range value is a programming error: it is caught by
    /// `debug_assert!` in debug/test builds; in release builds the value is
    /// truncated by the raw tagged-word encoding rather than panicking, so a
    /// runtime overflow can never abort the whole VM.
    #[must_use]
    pub fn small_int(value: i64) -> Self {
        debug_assert!(
            (SMALL_INT_MIN..=SMALL_INT_MAX).contains(&value),
            "small integer value is outside the immediate range"
        );
        Self(((value as u64) << TAG_BITS) | SMALL_INT_TAG)
    }

    /// Attempts to create a small integer term without truncating out-of-range
    /// values.
    pub const fn try_small_int(value: i64) -> Option<Self> {
        if value < SMALL_INT_MIN || value > SMALL_INT_MAX {
            None
        } else {
            Some(Self(((value as u64) << TAG_BITS) | SMALL_INT_TAG))
        }
    }

    /// Creates an atom term.
    pub const fn atom(atom: Atom) -> Self {
        Self(((atom.index() as u64) << TAG_BITS) | ATOM_TAG)
    }

    /// Creates an immediate local pid term.
    ///
    /// This infallible constructor is for known in-range pid literals. Use
    /// [`Term::try_pid`] for fallible construction from arbitrary `u64` values.
    /// Passing an out-of-range value is a programming error: it is caught by
    /// `debug_assert!` in debug/test builds; in release builds the high bits are
    /// truncated by the raw tagged-word encoding rather than panicking, so an
    /// untrusted-reachable pid can never abort the whole VM.
    #[must_use]
    pub fn pid(pid: u64) -> Self {
        debug_assert!(
            pid <= UNSIGNED_PAYLOAD_MAX,
            "pid value is outside the immediate range"
        );
        Self((pid << TAG_BITS) | PID_TAG)
    }

    /// Attempts to create an immediate local pid term without truncating high
    /// bits.
    pub const fn try_pid(pid: u64) -> Option<Self> {
        if pid > UNSIGNED_PAYLOAD_MAX {
            None
        } else {
            Some(Self((pid << TAG_BITS) | PID_TAG))
        }
    }

    /// Returns the primary tag for this term.
    pub const fn tag(self) -> Tag {
        match self.0 & TAG_MASK {
            SMALL_INT_TAG => Tag::SmallInt,
            ATOM_TAG => Tag::Atom,
            PID_TAG => Tag::Pid,
            NIL_TAG => {
                if self.0 == Self::NIL.0 {
                    Tag::Nil
                } else {
                    Tag::Boxed
                }
            }
            BOXED_TAG => Tag::Boxed,
            LIST_TAG => Tag::List,
            _ => Tag::Boxed,
        }
    }

    /// Returns `true` when this term is an immediate small integer.
    pub const fn is_small_int(self) -> bool {
        matches!(self.tag(), Tag::SmallInt)
    }

    /// Returns `true` when this term is an immediate atom.
    pub const fn is_atom(self) -> bool {
        matches!(self.tag(), Tag::Atom)
    }

    /// Returns `true` when this term is an immediate pid.
    pub const fn is_pid(self) -> bool {
        matches!(self.tag(), Tag::Pid)
    }

    /// Returns `true` only for the canonical empty list / nil value.
    pub const fn is_nil(self) -> bool {
        self.0 == Self::NIL.0
    }

    /// Returns `true` when this term carries the boxed heap pointer tag.
    pub const fn is_boxed(self) -> bool {
        matches!(self.tag(), Tag::Boxed)
    }

    /// Returns `true` when this term carries the list heap pointer tag.
    pub const fn is_list(self) -> bool {
        matches!(self.tag(), Tag::List)
    }

    /// Creates a boxed heap-pointer term from a word-aligned heap address.
    ///
    /// The pointer must be aligned so its low tag bits are zero; heap words
    /// (`u64`) satisfy this requirement. This constructor is intentionally
    /// crate-visible so boxed term modules can build terms without exposing raw
    /// bit manipulation outside `beamr::term`.
    pub(crate) fn boxed_ptr(ptr: *const u64) -> Self {
        Self::tagged_ptr(ptr, BOXED_TAG)
    }

    /// Creates a list heap-pointer term from a pointer to a cons cell head.
    pub(crate) fn list_ptr(ptr: *const u64) -> Self {
        Self::tagged_ptr(ptr, LIST_TAG)
    }

    /// Returns the untagged heap pointer for a boxed or list term.
    pub(crate) fn heap_ptr(self) -> Option<*const u64> {
        if self.is_boxed() || self.is_list() {
            Some((self.0 & !TAG_MASK) as *const u64)
        } else {
            None
        }
    }

    /// Returns the raw encoded word for heap layout storage.
    pub(crate) const fn raw(self) -> u64 {
        self.0
    }

    /// Reconstructs a term from its raw encoded word.
    pub(crate) const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    /// Decodes this term as a small integer, if it is one.
    pub const fn as_small_int(self) -> Option<i64> {
        if self.is_small_int() {
            Some((self.0 as i64) >> TAG_BITS)
        } else {
            None
        }
    }

    /// Decodes this term as an atom, if it is one.
    pub const fn as_atom(self) -> Option<Atom> {
        if self.is_atom() {
            Some(Atom::new((self.0 >> TAG_BITS) as u32))
        } else {
            None
        }
    }

    /// Decodes this term as local pid data, if it is one.
    pub const fn as_pid(self) -> Option<u64> {
        if self.is_pid() {
            Some(self.0 >> TAG_BITS)
        } else {
            None
        }
    }

    fn tagged_ptr(ptr: *const u64, tag: u64) -> Self {
        let raw = ptr as u64;
        debug_assert_eq!(raw & TAG_MASK, 0, "heap term pointers must be aligned");

        Self(raw | tag)
    }
}

#[cfg(test)]
mod tests {
    use super::{Tag, Term};
    use crate::atom::Atom;

    #[test]
    fn term_is_one_machine_word_with_private_tagged_value() {
        assert_eq!(std::mem::size_of::<Term>(), 8);
        assert_eq!(Term::small_int(1).tag(), Tag::SmallInt);
    }

    #[test]
    fn small_int_round_trips_and_preserves_sign() {
        for value in [0, 42, -1, Term::SMALL_INT_MAX, Term::SMALL_INT_MIN] {
            let term = Term::small_int(value);

            assert_eq!(term.as_small_int(), Some(value));
            assert!(term.is_small_int());
            assert_eq!(term.tag(), Tag::SmallInt);
        }
    }

    #[test]
    fn small_int_checked_constructor_rejects_out_of_range_values() {
        assert_eq!(Term::try_small_int(Term::SMALL_INT_MAX + 1), None);
        assert_eq!(Term::try_small_int(Term::SMALL_INT_MIN - 1), None);
    }

    /// PR-7: the infallible `small_int` constructor previously `panic!`ed on
    /// out-of-range input. It now relies on `debug_assert!` and otherwise never
    /// panics; the boundary in-range values must round-trip, and out-of-range
    /// runtime values must flow through the explicit-error `try_small_int` path
    /// rather than aborting the VM.
    #[test]
    fn small_int_boundary_constructs_without_panicking_and_overflow_uses_error_path() {
        assert_eq!(
            Term::small_int(Term::SMALL_INT_MAX).as_small_int(),
            Some(Term::SMALL_INT_MAX)
        );
        assert_eq!(
            Term::small_int(Term::SMALL_INT_MIN).as_small_int(),
            Some(Term::SMALL_INT_MIN)
        );
        // Overflowing arithmetic must use the fallible path, which reports the
        // error instead of panicking.
        assert!(Term::try_small_int(Term::SMALL_INT_MAX + 1).is_none());
    }

    #[test]
    fn atom_round_trips_without_becoming_nil() {
        for atom in [Atom::OK, Atom::ERROR, Atom::NIL] {
            let term = Term::atom(atom);

            assert_eq!(term.as_atom(), Some(atom));
            assert!(term.is_atom());
            assert_eq!(term.tag(), Tag::Atom);
            assert!(!term.is_small_int());
            assert!(!term.is_pid());
            assert!(!term.is_nil());
        }
    }

    #[test]
    fn pid_round_trips() {
        for pid in [0, 12_345, Term::PID_MAX] {
            let term = Term::pid(pid);

            assert_eq!(term.as_pid(), Some(pid));
            assert!(term.is_pid());
            assert_eq!(term.tag(), Tag::Pid);
            assert!(!term.is_small_int());
            assert!(!term.is_atom());
        }
    }

    #[test]
    fn pid_checked_constructor_rejects_out_of_range_values() {
        assert_eq!(Term::try_pid(Term::PID_MAX + 1), None);
    }

    /// PR-7: the infallible `pid` constructor previously `panic!`ed on
    /// out-of-range input. It now relies on `debug_assert!` and otherwise never
    /// panics; the boundary in-range pid must round-trip, and an out-of-range
    /// pid must flow through the explicit-error `try_pid` path.
    #[test]
    fn pid_boundary_constructs_without_panicking_and_overflow_uses_error_path() {
        assert_eq!(Term::pid(Term::PID_MAX).as_pid(), Some(Term::PID_MAX));
        assert!(Term::try_pid(Term::PID_MAX + 1).is_none());
    }

    #[test]
    fn nil_is_distinguished_from_integer_atom_and_pid_values() {
        assert!(Term::NIL.is_nil());
        assert_eq!(Term::NIL.tag(), Tag::Nil);
        assert!(!Term::small_int(0).is_nil());
        assert!(!Term::atom(Atom::NIL).is_nil());
        assert!(!Term::pid(0).is_nil());
        assert_ne!(Term::NIL, Term::small_int(0));
    }

    #[test]
    fn tag_dispatch_and_predicates_agree_for_immediates() {
        let terms = [
            (Term::small_int(1), Tag::SmallInt),
            (Term::atom(Atom::OK), Tag::Atom),
            (Term::pid(1), Tag::Pid),
            (Term::NIL, Tag::Nil),
        ];

        for (term, tag) in terms {
            assert_eq!(term.tag(), tag);
            assert_eq!(term.is_small_int(), tag == Tag::SmallInt);
            assert_eq!(term.is_atom(), tag == Tag::Atom);
            assert_eq!(term.is_pid(), tag == Tag::Pid);
            assert_eq!(term.is_nil(), tag == Tag::Nil);
            assert_eq!(term.is_boxed(), tag == Tag::Boxed);
            assert_eq!(term.is_list(), tag == Tag::List);
        }
    }

    #[test]
    fn cross_type_extractors_return_none() {
        let integer = Term::small_int(42);
        let atom = Term::atom(Atom::OK);
        let pid = Term::pid(12_345);
        let nil = Term::NIL;

        assert_eq!(integer.as_atom(), None);
        assert_eq!(integer.as_pid(), None);
        assert_eq!(atom.as_small_int(), None);
        assert_eq!(atom.as_pid(), None);
        assert_eq!(pid.as_small_int(), None);
        assert_eq!(pid.as_atom(), None);
        assert_eq!(nil.as_small_int(), None);
        assert_eq!(nil.as_atom(), None);
        assert_eq!(nil.as_pid(), None);
    }
}
