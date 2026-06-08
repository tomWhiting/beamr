use std::sync::atomic::{AtomicU32, Ordering};

use dashmap::DashMap;
use dashmap::mapref::entry::Entry;

/// Index into the global atom table.
///
/// Atoms are intentionally opaque outside the `beamr` crate. External users
/// compare atoms by value rather than depending on their table indices.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Atom(u32);

impl Atom {
    pub const OK: Self = Self(0);
    pub const ERROR: Self = Self(1);
    pub const TRUE: Self = Self(2);
    pub const FALSE: Self = Self(3);
    pub const NIL: Self = Self(4);
    pub const UNDEFINED: Self = Self(5);
    pub const NORMAL: Self = Self(6);
    pub const KILL: Self = Self(7);
    pub const EXIT: Self = Self(8);
    pub const BADARG: Self = Self(9);
    pub const BADARITH: Self = Self(10);
    pub const BADMATCH: Self = Self(11);
    pub const FUNCTION_CLAUSE: Self = Self(12);
    pub const CASE_CLAUSE: Self = Self(13);
    pub const IF_CLAUSE: Self = Self(14);
    pub const UNDEF: Self = Self(15);
    pub const BADFUN: Self = Self(16);
    pub const BADARITY: Self = Self(17);
    pub const NOPROC: Self = Self(18);
    pub const TIMEOUT: Self = Self(19);
    pub const KILLED: Self = Self(20);
    pub const DOWN: Self = Self(21);
    pub const PROCESS: Self = Self(22);
    pub const TRAP_EXIT: Self = Self(23);
    pub const BADKEY: Self = Self(24);
    pub const FLUSH: Self = Self(25);
    pub const INFO: Self = Self(26);
    pub const UTF8: Self = Self(27);
    pub const LATIN1: Self = Self(28);
    pub const MODULE: Self = Self(29);
    pub const THROW: Self = Self(30);
    pub const EXIT_CLASS: Self = Self(31);
    pub const LINE: Self = Self(32);
    pub const ENOENT: Self = Self(33);
    pub const EACCES: Self = Self(34);
    pub const EEXIST: Self = Self(35);
    pub const EISDIR: Self = Self(36);
    pub const ENOTDIR: Self = Self(37);
    pub const ENOSPC: Self = Self(38);
    pub const EMFILE: Self = Self(39);
    pub const ENFILE: Self = Self(40);
    pub const EBADF: Self = Self(41);
    pub const EPIPE: Self = Self(42);
    pub const EAGAIN: Self = Self(43);
    pub const EINVAL: Self = Self(44);
    pub const ENOTEMPTY: Self = Self(45);
    pub const EXDEV: Self = Self(46);
    pub const ELOOP: Self = Self(47);
    pub const EROFS: Self = Self(48);
    pub const ENAMETOOLONG: Self = Self(49);
    pub const EPERM: Self = Self(50);
    pub const UNKNOWN_ERROR: Self = Self(51);
    pub const CLOSED: Self = Self(52);
    pub const INCOMPLETE: Self = Self(53);
    pub const READ: Self = Self(54);
    pub const WRITE: Self = Self(55);
    pub const APPEND: Self = Self(56);
    pub const CREATE: Self = Self(57);
    pub const TRUNCATE: Self = Self(58);
    pub const BOF: Self = Self(59);
    pub const CUR: Self = Self(60);
    pub const EOF: Self = Self(61);
    pub const ECONNREFUSED: Self = Self(62);
    pub const ECONNRESET: Self = Self(63);
    pub const EINPROGRESS: Self = Self(64);

    pub(crate) const fn new(index: u32) -> Self {
        Self(index)
    }

    pub(crate) const fn index(self) -> u32 {
        self.0
    }
}

const COMMON_ATOMS: &[(&str, Atom)] = &[
    ("ok", Atom::OK),
    ("error", Atom::ERROR),
    ("true", Atom::TRUE),
    ("false", Atom::FALSE),
    ("nil", Atom::NIL),
    ("undefined", Atom::UNDEFINED),
    ("normal", Atom::NORMAL),
    ("kill", Atom::KILL),
    ("EXIT", Atom::EXIT),
    ("badarg", Atom::BADARG),
    ("badarith", Atom::BADARITH),
    ("badmatch", Atom::BADMATCH),
    ("function_clause", Atom::FUNCTION_CLAUSE),
    ("case_clause", Atom::CASE_CLAUSE),
    ("if_clause", Atom::IF_CLAUSE),
    ("undef", Atom::UNDEF),
    ("badfun", Atom::BADFUN),
    ("badarity", Atom::BADARITY),
    ("noproc", Atom::NOPROC),
    ("timeout", Atom::TIMEOUT),
    ("killed", Atom::KILLED),
    ("DOWN", Atom::DOWN),
    ("process", Atom::PROCESS),
    ("trap_exit", Atom::TRAP_EXIT),
    ("badkey", Atom::BADKEY),
    ("flush", Atom::FLUSH),
    ("info", Atom::INFO),
    ("utf8", Atom::UTF8),
    ("latin1", Atom::LATIN1),
    ("module", Atom::MODULE),
    ("throw", Atom::THROW),
    ("exit", Atom::EXIT_CLASS),
    ("line", Atom::LINE),
    ("enoent", Atom::ENOENT),
    ("eacces", Atom::EACCES),
    ("eexist", Atom::EEXIST),
    ("eisdir", Atom::EISDIR),
    ("enotdir", Atom::ENOTDIR),
    ("enospc", Atom::ENOSPC),
    ("emfile", Atom::EMFILE),
    ("enfile", Atom::ENFILE),
    ("ebadf", Atom::EBADF),
    ("epipe", Atom::EPIPE),
    ("eagain", Atom::EAGAIN),
    ("einval", Atom::EINVAL),
    ("enotempty", Atom::ENOTEMPTY),
    ("exdev", Atom::EXDEV),
    ("eloop", Atom::ELOOP),
    ("erofs", Atom::EROFS),
    ("enametoolong", Atom::ENAMETOOLONG),
    ("eperm", Atom::EPERM),
    ("unknown_error", Atom::UNKNOWN_ERROR),
    ("closed", Atom::CLOSED),
    ("incomplete", Atom::INCOMPLETE),
    ("read", Atom::READ),
    ("write", Atom::WRITE),
    ("append", Atom::APPEND),
    ("create", Atom::CREATE),
    ("truncate", Atom::TRUNCATE),
    ("bof", Atom::BOF),
    ("cur", Atom::CUR),
    ("eof", Atom::EOF),
    ("econnrefused", Atom::ECONNREFUSED),
    ("econnreset", Atom::ECONNRESET),
    ("einprogress", Atom::EINPROGRESS),
];

/// Concurrent intern table for atom strings.
///
/// The table stores atoms in both directions. Strings are leaked when first
/// interned because runtime atoms are never freed in this design, which lets
/// `resolve` return a borrowed `&str` without tying it to a map guard.
pub struct AtomTable {
    by_name: DashMap<String, Atom>,
    by_index: DashMap<u32, &'static str>,
    next_index: AtomicU32,
}

impl AtomTable {
    /// Create an empty atom table.
    #[must_use]
    pub fn new() -> Self {
        Self {
            by_name: DashMap::new(),
            by_index: DashMap::new(),
            next_index: AtomicU32::new(0),
        }
    }

    /// Create an atom table with the common atoms at stable indices.
    #[must_use]
    pub fn with_common_atoms() -> Self {
        let table = Self::new();

        for &(name, atom) in COMMON_ATOMS {
            table.by_name.insert(name.to_owned(), atom);
            table.by_index.insert(atom.index(), name);
        }

        table
            .next_index
            .store(COMMON_ATOMS.len() as u32, Ordering::Relaxed);
        table
    }

    /// Intern `name`, returning its existing or newly assigned atom.
    pub fn intern(&self, name: &str) -> Atom {
        if let Some(atom) = self.by_name.get(name).map(|entry| *entry) {
            return atom;
        }

        match self.by_name.entry(name.to_owned()) {
            Entry::Occupied(entry) => *entry.get(),
            Entry::Vacant(entry) => {
                let index = self.next_index.fetch_add(1, Ordering::Relaxed);
                let atom = Atom::new(index);
                let stored_name: &'static str = Box::leak(entry.key().clone().into_boxed_str());
                self.by_index.insert(index, stored_name);
                entry.insert(atom);
                atom
            }
        }
    }

    /// Look up an atom by name without interning it.
    ///
    /// Returns `Some(atom)` if the name has already been interned,
    /// `None` otherwise. Used by `binary_to_existing_atom` which must
    /// not create new atom table entries.
    #[must_use]
    pub fn lookup(&self, name: &str) -> Option<Atom> {
        self.by_name.get(name).map(|entry| *entry)
    }

    /// Resolve an atom back to its original string.
    #[must_use]
    pub fn resolve(&self, atom: Atom) -> Option<&str> {
        self.by_index.get(&atom.index()).map(|entry| *entry)
    }

    /// Return the number of atoms currently interned in this table.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    /// Return whether this table currently contains no atoms.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }

    /// Return the maximum atom count representable by this table.
    #[must_use]
    pub const fn limit(&self) -> usize {
        u32::MAX as usize
    }
}

impl Default for AtomTable {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, sync::Arc, thread};

    use super::{Atom, AtomTable};

    #[test]
    fn intern_resolve_round_trip() {
        let table = AtomTable::new();
        let atom = table.intern("x");

        assert_eq!(table.resolve(atom), Some("x"));
    }

    #[test]
    fn intern_is_idempotent() {
        let table = AtomTable::new();
        let first = table.intern("x");
        let second = table.intern("x");

        assert_eq!(first, second);
    }

    #[test]
    fn different_strings_have_different_atoms() {
        let table = AtomTable::new();
        let hello = table.intern("hello");
        let world = table.intern("world");

        assert_ne!(hello, world);
    }

    #[test]
    fn common_atoms_have_stable_constants() {
        let table = AtomTable::with_common_atoms();

        assert_eq!(table.resolve(Atom::OK), Some("ok"));
        assert_eq!(table.resolve(Atom::ERROR), Some("error"));
        assert_eq!(table.resolve(Atom::TRUE), Some("true"));
        assert_eq!(table.resolve(Atom::FALSE), Some("false"));
        assert_eq!(table.resolve(Atom::NIL), Some("nil"));
        assert_eq!(table.resolve(Atom::EXIT), Some("EXIT"));
        assert_eq!(table.resolve(Atom::THROW), Some("throw"));
        assert_eq!(table.resolve(Atom::EXIT_CLASS), Some("exit"));
        assert_eq!(table.intern("throw"), Atom::THROW);
        assert_eq!(table.intern("exit"), Atom::EXIT_CLASS);
        assert_eq!(table.intern("ok"), Atom::OK);
        assert_eq!(table.resolve(Atom::ENOENT), Some("enoent"));
        assert_eq!(table.resolve(Atom::EACCES), Some("eacces"));
        assert_eq!(table.resolve(Atom::EEXIST), Some("eexist"));
        assert_eq!(table.resolve(Atom::EISDIR), Some("eisdir"));
        assert_eq!(table.resolve(Atom::ENOTDIR), Some("enotdir"));
        assert_eq!(table.resolve(Atom::ENOSPC), Some("enospc"));
        assert_eq!(table.resolve(Atom::EMFILE), Some("emfile"));
        assert_eq!(table.resolve(Atom::ENFILE), Some("enfile"));
        assert_eq!(table.resolve(Atom::EBADF), Some("ebadf"));
        assert_eq!(table.resolve(Atom::EPIPE), Some("epipe"));
        assert_eq!(table.resolve(Atom::EAGAIN), Some("eagain"));
        assert_eq!(table.resolve(Atom::EINVAL), Some("einval"));
        assert_eq!(table.resolve(Atom::UNKNOWN_ERROR), Some("unknown_error"));
        assert_eq!(table.resolve(Atom::CLOSED), Some("closed"));
        assert_eq!(table.resolve(Atom::INCOMPLETE), Some("incomplete"));
        assert_eq!(table.resolve(Atom::READ), Some("read"));
        assert_eq!(table.resolve(Atom::WRITE), Some("write"));
        assert_eq!(table.resolve(Atom::APPEND), Some("append"));
        assert_eq!(table.resolve(Atom::CREATE), Some("create"));
        assert_eq!(table.resolve(Atom::TRUNCATE), Some("truncate"));
        assert_eq!(table.resolve(Atom::BOF), Some("bof"));
        assert_eq!(table.resolve(Atom::CUR), Some("cur"));
        assert_eq!(table.resolve(Atom::EOF), Some("eof"));
    }

    #[test]
    fn invalid_atom_resolves_to_none() {
        let table = AtomTable::new();

        assert_eq!(table.resolve(Atom::new(999_999)), None);
    }

    #[test]
    fn concurrent_interning_same_strings_converges() {
        let table = Arc::new(AtomTable::new());
        let names: Arc<Vec<String>> =
            Arc::new((0..100).map(|index| format!("name_{index}")).collect());
        let mut handles = Vec::new();

        for _ in 0..8 {
            let table = Arc::clone(&table);
            let names = Arc::clone(&names);
            handles.push(thread::spawn(move || {
                names
                    .iter()
                    .map(|name| table.intern(name))
                    .collect::<Vec<_>>()
            }));
        }

        let mut unique_atoms = HashSet::new();
        for handle in handles {
            unique_atoms.extend(handle.join().expect("worker thread should not panic"));
        }

        assert_eq!(unique_atoms.len(), 100);
        for name in names.iter() {
            let atom = table.intern(name);
            assert!(unique_atoms.contains(&atom));
            assert_eq!(table.resolve(atom), Some(name.as_str()));
        }
    }

    #[test]
    fn concurrent_interning_distinct_strings_are_unique_and_resolvable() {
        let table = Arc::new(AtomTable::new());
        let mut handles = Vec::new();

        for thread_index in 0..8 {
            let table = Arc::clone(&table);
            handles.push(thread::spawn(move || {
                (0..100)
                    .map(|name_index| {
                        let name = format!("thread_{thread_index}_name_{name_index}");
                        let atom = table.intern(&name);
                        (name, atom)
                    })
                    .collect::<Vec<_>>()
            }));
        }

        let mut unique_atoms = HashSet::new();
        let mut inserted = Vec::new();
        for handle in handles {
            inserted.extend(handle.join().expect("worker thread should not panic"));
        }

        for (name, atom) in inserted {
            assert!(unique_atoms.insert(atom));
            assert_eq!(table.resolve(atom), Some(name.as_str()));
        }

        assert_eq!(unique_atoms.len(), 800);
    }
}
