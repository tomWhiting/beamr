use crate::atom::Atom;
use crate::ets::OwnedTerm;
use crate::term::Term;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

/// Unique, monotonically increasing ETS table identifier.
pub type EtsTableId = u64;

/// Table storage semantics requested at creation time.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum EtsTableType {
    Set,
    OrderedSet,
    Bag,
    DuplicateBag,
}

/// Access protection policy for a table.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Protection {
    /// Any process may read or write.
    Public,
    /// Any process may read; only the owner may write.
    Protected,
    /// Only the owner may read or write.
    Private,
}

/// Access operation checked against a table protection policy.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum AccessOp {
    Read,
    Write,
}

impl fmt::Display for AccessOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read => f.write_str("read"),
            Self::Write => f.write_str("write"),
        }
    }
}

/// Atomic ETS owner pid stored inside otherwise stable table metadata.
pub struct EtsOwner(AtomicU64);

impl EtsOwner {
    #[must_use]
    pub const fn new(pid: u64) -> Self {
        Self(AtomicU64::new(pid))
    }

    #[must_use]
    pub fn get(&self) -> u64 {
        self.0.load(Ordering::Acquire)
    }

    pub fn set(&self, pid: u64) {
        self.0.store(pid, Ordering::Release);
    }
}

impl fmt::Debug for EtsOwner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.get().fmt(f)
    }
}

impl PartialEq<u64> for EtsOwner {
    fn eq(&self, other: &u64) -> bool {
        self.get() == *other
    }
}

/// ETS ownership heir configured at table creation.
#[derive(Debug)]
pub struct EtsHeir {
    pub pid: u64,
    pub data: OwnedTerm,
}

/// Metadata common to all ETS table implementations.
#[derive(Debug)]
pub struct EtsTableMetadata {
    pub name: Option<Atom>,
    pub id: EtsTableId,
    pub table_type: EtsTableType,
    pub protection: Protection,
    /// Owning process identifier.
    pub owner: EtsOwner,
    /// 1-based tuple element position used as the key.
    pub keypos: usize,
    /// Hint that table reads should be allowed to proceed concurrently.
    pub read_concurrency: bool,
    /// Hint that table writes should be allowed to proceed concurrently.
    pub write_concurrency: bool,
    /// Optional ownership heir and transfer payload.
    pub heir: Option<EtsHeir>,
}

impl EtsTableMetadata {
    /// Construct metadata with the default key position (1).
    #[must_use]
    pub const fn new(
        name: Option<Atom>,
        id: EtsTableId,
        table_type: EtsTableType,
        protection: Protection,
        owner: u64,
    ) -> Self {
        Self {
            name,
            id,
            table_type,
            protection,
            owner: EtsOwner::new(owner),
            keypos: 1,
            read_concurrency: false,
            write_concurrency: false,
            heir: None,
        }
    }
}

/// Error returned by ETS registry/table operations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EtsError {
    AccessDenied {
        table_id: EtsTableId,
        caller_pid: u64,
        operation: AccessOp,
    },
    Badarg,
    InvalidBoxedTerm,
    AllocationFailed,
}

impl fmt::Display for EtsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AccessDenied {
                table_id,
                caller_pid,
                operation,
            } => write!(
                f,
                "ETS access denied: pid {caller_pid} cannot {operation} table {table_id}"
            ),
            Self::Badarg => f.write_str("bad argument"),
            Self::InvalidBoxedTerm => f.write_str("invalid boxed term for ETS copy"),
            Self::AllocationFailed => f.write_str("failed to allocate memory for ETS term copy"),
        }
    }
}

impl std::error::Error for EtsError {}

/// Trait implemented by concrete ETS table types.
///
/// Raw table operations do not carry a caller PID; the scheduler/BIF layer must
/// call [`EtsTable::check_access`] before invoking reads or writes.
pub trait EtsTable: Send + Sync {
    fn metadata(&self) -> &EtsTableMetadata;
    fn transfer_owner(&self, new_owner: u64) {
        self.metadata().owner.set(new_owner);
    }
    fn insert(&self, tuple: Term) -> Result<(), EtsError>;
    fn lookup(&self, key: Term) -> Vec<Term>;
    fn delete_key(&self, key: Term) -> bool;
    fn delete_object(&self, tuple: Term) -> bool;
    fn tab2list(&self) -> Vec<Term>;

    fn check_access(&self, caller_pid: u64, operation: AccessOp) -> Result<(), EtsError> {
        let metadata = self.metadata();
        let allowed = match (metadata.protection, operation) {
            (Protection::Public, _) => true,
            (Protection::Protected, AccessOp::Read) => true,
            (Protection::Protected, AccessOp::Write) | (Protection::Private, _) => {
                metadata.owner == caller_pid
            }
        };

        if allowed {
            Ok(())
        } else {
            Err(EtsError::AccessDenied {
                table_id: metadata.id,
                caller_pid,
                operation,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyTable {
        metadata: EtsTableMetadata,
    }

    impl DummyTable {
        fn new(protection: Protection) -> Self {
            Self {
                metadata: EtsTableMetadata {
                    name: None,
                    id: 42,
                    table_type: EtsTableType::Set,
                    protection,
                    owner: EtsOwner::new(7),
                    keypos: 1,
                    read_concurrency: false,
                    write_concurrency: false,
                    heir: None,
                },
            }
        }
    }

    impl EtsTable for DummyTable {
        fn metadata(&self) -> &EtsTableMetadata {
            &self.metadata
        }

        fn insert(&self, _tuple: Term) -> Result<(), EtsError> {
            Ok(())
        }

        fn lookup(&self, _key: Term) -> Vec<Term> {
            Vec::new()
        }

        fn delete_key(&self, _key: Term) -> bool {
            false
        }

        fn delete_object(&self, _tuple: Term) -> bool {
            false
        }

        fn tab2list(&self) -> Vec<Term> {
            Vec::new()
        }
    }

    #[test]
    fn metadata_fields_are_accessible() {
        let metadata = EtsTableMetadata {
            name: Some(Atom::new(9)),
            id: 12,
            table_type: EtsTableType::Bag,
            protection: Protection::Protected,
            owner: EtsOwner::new(34),
            keypos: 2,
            read_concurrency: true,
            write_concurrency: true,
            heir: None,
        };

        assert_eq!(metadata.name, Some(Atom::new(9)));
        assert_eq!(metadata.id, 12);
        assert_eq!(metadata.table_type, EtsTableType::Bag);
        assert_eq!(metadata.protection, Protection::Protected);
        assert_eq!(metadata.owner, 34);
        assert_eq!(metadata.keypos, 2);
        assert!(metadata.read_concurrency);
        assert!(metadata.write_concurrency);
    }

    #[test]
    fn private_table_rejects_non_owner_read_and_write() {
        let table = DummyTable::new(Protection::Private);

        assert_eq!(
            table.check_access(8, AccessOp::Read),
            Err(EtsError::AccessDenied {
                table_id: 42,
                caller_pid: 8,
                operation: AccessOp::Read,
            })
        );
        assert_eq!(
            table.check_access(8, AccessOp::Write),
            Err(EtsError::AccessDenied {
                table_id: 42,
                caller_pid: 8,
                operation: AccessOp::Write,
            })
        );
        assert_eq!(table.check_access(7, AccessOp::Read), Ok(()));
        assert_eq!(table.check_access(7, AccessOp::Write), Ok(()));
    }

    #[test]
    fn protected_table_allows_non_owner_read_but_rejects_write() {
        let table = DummyTable::new(Protection::Protected);

        assert_eq!(table.check_access(8, AccessOp::Read), Ok(()));
        assert_eq!(
            table.check_access(8, AccessOp::Write),
            Err(EtsError::AccessDenied {
                table_id: 42,
                caller_pid: 8,
                operation: AccessOp::Write,
            })
        );
        assert_eq!(table.check_access(7, AccessOp::Write), Ok(()));
    }

    #[test]
    fn public_table_allows_all_access() {
        let table = DummyTable::new(Protection::Public);

        assert_eq!(table.check_access(8, AccessOp::Read), Ok(()));
        assert_eq!(table.check_access(8, AccessOp::Write), Ok(()));
        assert_eq!(table.check_access(7, AccessOp::Read), Ok(()));
        assert_eq!(table.check_access(7, AccessOp::Write), Ok(()));
    }
}
