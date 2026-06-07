//! VM introspection BIFs — `erlang:system_info/1`.

use crate::atom::{Atom, AtomTable};
use crate::native::{BifRegistryImpl, Capability, NativeRegistrationError, ProcessContext};
use crate::term::Term;

/// Default process limit reported to OTP compatibility code.
pub const DEFAULT_PROCESS_LIMIT: usize = 262_144;
/// OTP release compatibility claim used by stdlib probes.
pub const OTP_RELEASE: &[u8] = b"27";
/// BEAM word size reported for the supported 64-bit runtime target.
pub const WORDSIZE_BYTES: usize = 8;

/// Narrow interface used by `system_info/1` to query runtime metrics.
///
/// Implementations are provided by the scheduler and injected into
/// [`ProcessContext`] before BIF execution so native code does not receive direct
/// access to scheduler internals.
pub trait SystemInfoFacility: Send + Sync {
    /// Number of normal scheduler threads.
    fn scheduler_count(&self) -> usize;

    /// Number of currently alive processes.
    fn process_count(&self) -> usize;

    /// Maximum number of alive processes supported by this runtime.
    fn process_limit(&self) -> usize {
        DEFAULT_PROCESS_LIMIT
    }

    /// Current atom table size.
    fn atom_count(&self) -> usize;

    /// Maximum atom count supported by this runtime.
    fn atom_limit(&self) -> usize;
}

/// Registers `erlang:system_info/1`.
pub fn register_system_info_bifs(
    registry: &BifRegistryImpl,
    atom_table: &AtomTable,
) -> Result<(), NativeRegistrationError> {
    let erlang = atom_table.intern("erlang");
    let system_info = atom_table.intern("system_info");
    registry.register(erlang, system_info, 1, bif_system_info, Capability::Pure)
}

/// erlang:system_info/1 — return a small OTP-compatible subset of VM metadata.
pub fn bif_system_info(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [item_term] = args else {
        return Err(badarg());
    };
    let item = item_term.as_atom().ok_or_else(badarg)?;
    let item_name = {
        let table = context.atom_table().ok_or_else(badarg)?;
        table.resolve(item).ok_or_else(badarg)?.to_owned()
    };

    match item_name.as_str() {
        "schedulers" => facility_small_int(context, SystemInfoMetric::Schedulers),
        "process_count" => facility_small_int(context, SystemInfoMetric::ProcessCount),
        "process_limit" => facility_small_int(context, SystemInfoMetric::ProcessLimit),
        "wordsize" => small_int(WORDSIZE_BYTES),
        "otp_release" => context.alloc_binary(OTP_RELEASE),
        "version" => context.alloc_binary(env!("CARGO_PKG_VERSION").as_bytes()),
        "system_architecture" => context.alloc_binary(system_architecture().as_bytes()),
        "atom_count" => facility_small_int(context, SystemInfoMetric::AtomCount),
        "atom_limit" => facility_small_int(context, SystemInfoMetric::AtomLimit),
        _ => Err(badarg()),
    }
}

enum SystemInfoMetric {
    Schedulers,
    ProcessCount,
    ProcessLimit,
    AtomCount,
    AtomLimit,
}

fn facility_small_int(context: &ProcessContext, metric: SystemInfoMetric) -> Result<Term, Term> {
    let facility = context.system_info_facility().ok_or_else(badarg)?;
    let value = match metric {
        SystemInfoMetric::Schedulers => facility.scheduler_count(),
        SystemInfoMetric::ProcessCount => facility.process_count(),
        SystemInfoMetric::ProcessLimit => facility.process_limit(),
        SystemInfoMetric::AtomCount => facility.atom_count(),
        SystemInfoMetric::AtomLimit => facility.atom_limit(),
    };
    small_int(value)
}

fn small_int(value: usize) -> Result<Term, Term> {
    let value = i64::try_from(value).map_err(|_| badarg())?;
    Term::try_small_int(value).ok_or_else(badarg)
}

fn system_architecture() -> String {
    format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::process::Process;
    use crate::term::binary::Binary;

    struct TestSystemInfoFacility;

    impl SystemInfoFacility for TestSystemInfoFacility {
        fn scheduler_count(&self) -> usize {
            4
        }

        fn process_count(&self) -> usize {
            12
        }

        fn process_limit(&self) -> usize {
            262_144
        }

        fn atom_count(&self) -> usize {
            44
        }

        fn atom_limit(&self) -> usize {
            u32::MAX as usize
        }
    }

    fn context<'process>(
        process: &'process mut Process,
        atom_table: Arc<AtomTable>,
    ) -> ProcessContext<'process> {
        let mut context = ProcessContext::new();
        context.set_atom_table(Some(atom_table));
        context.set_system_info_facility(Some(Arc::new(TestSystemInfoFacility)));
        context.attach_process(process, 0);
        context
    }

    fn call_system_info(item: &str) -> Term {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let item_atom = atom_table.intern(item);
        let mut process = Process::new(1, 128);
        let mut context = context(&mut process, atom_table);
        bif_system_info(&[Term::atom(item_atom)], &mut context).expect("system_info succeeds")
    }

    fn call_system_info_binary_bytes(item: &str) -> Vec<u8> {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let item_atom = atom_table.intern(item);
        let mut process = Process::new(1, 128);
        let mut context = context(&mut process, atom_table);
        let term =
            bif_system_info(&[Term::atom(item_atom)], &mut context).expect("system_info succeeds");
        Binary::new(term)
            .expect("system_info item returns binary")
            .as_bytes()
            .to_vec()
    }

    #[test]
    fn numeric_items_return_small_integers() {
        assert_eq!(call_system_info("schedulers").as_small_int(), Some(4));
        assert_eq!(call_system_info("process_count").as_small_int(), Some(12));
        assert_eq!(
            call_system_info("process_limit").as_small_int(),
            Some(262_144)
        );
        assert_eq!(
            call_system_info("wordsize").as_small_int(),
            Some(i64::try_from(WORDSIZE_BYTES).unwrap_or(i64::MAX))
        );
        assert_eq!(call_system_info("atom_count").as_small_int(), Some(44));
        assert_eq!(
            call_system_info("atom_limit").as_small_int(),
            Some(i64::from(u32::MAX))
        );
    }

    #[test]
    fn binary_items_return_expected_bytes() {
        assert_eq!(
            call_system_info_binary_bytes("otp_release").as_slice(),
            b"27"
        );
        assert_eq!(
            call_system_info_binary_bytes("version").as_slice(),
            env!("CARGO_PKG_VERSION").as_bytes()
        );
        assert_eq!(
            call_system_info_binary_bytes("system_architecture").as_slice(),
            system_architecture().as_bytes()
        );
    }

    #[test]
    fn badarg_for_unknown_item_non_atom_wrong_arity_and_missing_facility() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let unknown = atom_table.intern("does_not_exist");
        let mut process = Process::new(1, 128);
        let mut context = context(&mut process, Arc::clone(&atom_table));
        assert_eq!(
            bif_system_info(&[Term::atom(unknown)], &mut context),
            Err(badarg())
        );
        assert_eq!(
            bif_system_info(&[Term::small_int(1)], &mut context),
            Err(badarg())
        );
        assert_eq!(bif_system_info(&[], &mut context), Err(badarg()));

        let scheduler_item = atom_table.intern("schedulers");
        let mut no_facility_process = Process::new(2, 128);
        let mut no_facility = ProcessContext::new();
        no_facility.set_atom_table(Some(atom_table));
        no_facility.attach_process(&mut no_facility_process, 0);
        assert_eq!(
            bif_system_info(&[Term::atom(scheduler_item)], &mut no_facility),
            Err(badarg())
        );
    }

    #[test]
    fn registers_erlang_system_info_one() {
        let atom_table = AtomTable::new();
        let registry = BifRegistryImpl::new();
        register_system_info_bifs(&registry, &atom_table).expect("registration succeeds");

        let erlang = atom_table.intern("erlang");
        let system_info = atom_table.intern("system_info");
        let entry = registry
            .lookup(erlang, system_info, 1)
            .expect("system_info/1 registered");
        assert_eq!(entry.capability, Capability::Pure);
        assert!(!entry.is_dirty);
    }
}
