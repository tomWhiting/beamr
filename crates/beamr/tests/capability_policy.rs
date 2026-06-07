use std::sync::Arc;

use beamr::atom::AtomTable;
use beamr::loader::{ImportEntry, load_beam_chunks, resolve_imports};
use beamr::module::{ModuleRegistry, ResolvedImportTarget};
use beamr::native::bifs::register_gate1_bifs;
use beamr::native::meridian_ffi::register_meridian_ffi;
use beamr::native::stdlib_stubs::register_stdlib_stubs;
use beamr::native::{
    AllCapabilitiesPolicy, BifRegistryImpl, Capability, CapabilitySet, LeastAuthorityPolicy,
    ProcessContext, denial_stub,
};
use beamr::scheduler::{Scheduler, SchedulerConfig};
use beamr::term::Term;

fn parsed_with_import(
    atoms: &AtomTable,
    module_name: &str,
    function_name: &str,
    arity: u8,
) -> beamr::loader::ParsedModule {
    let mut parsed =
        load_beam_chunks(include_bytes!("fixtures/hello.beam"), atoms).expect("fixture parses");
    parsed.imports = vec![ImportEntry {
        module: atoms.intern(module_name),
        function: atoms.intern(function_name),
        arity,
    }];
    parsed
}

#[test]
fn least_authority_grants_pure_and_denies_external_io() {
    let atoms = AtomTable::with_common_atoms();
    let registry = ModuleRegistry::new();
    let bifs = BifRegistryImpl::new();
    register_gate1_bifs(&bifs, &atoms).expect("gate1 registration");
    register_meridian_ffi(&bifs, &atoms).expect("meridian ffi registration");

    let pure = parsed_with_import(&atoms, "erlang", "+", 2);
    let (resolved, report) = resolve_imports(&pure, &registry, &bifs, &LeastAuthorityPolicy);
    assert!(report.is_empty());
    assert!(matches!(
        resolved.first().and_then(Option::as_ref).map(|entry| entry.target),
        Some(ResolvedImportTarget::Native(entry)) if entry.function as usize != denial_stub as usize
    ));

    let external = parsed_with_import(&atoms, "meridian_ffi", "run_cmd", 1);
    let (resolved, report) = resolve_imports(&external, &registry, &bifs, &LeastAuthorityPolicy);
    assert!(report.has_denied());
    assert_eq!(
        report.denied_imports()[0].capability,
        Capability::ExternalIo
    );
    assert!(report.to_string().contains("capability denied"));
    assert!(matches!(
        resolved.first().and_then(Option::as_ref).map(|entry| entry.target),
        Some(ResolvedImportTarget::Native(entry)) if entry.function as usize == denial_stub as usize
    ));
}

#[test]
fn all_capabilities_resolves_external_io_normally() {
    let atoms = AtomTable::with_common_atoms();
    let registry = ModuleRegistry::new();
    let bifs = BifRegistryImpl::new();
    register_meridian_ffi(&bifs, &atoms).expect("meridian ffi registration");

    let external = parsed_with_import(&atoms, "meridian_ffi", "run_cmd", 1);
    let (resolved, report) = resolve_imports(&external, &registry, &bifs, &AllCapabilitiesPolicy);

    assert!(report.is_empty());
    assert!(matches!(
        resolved.first().and_then(Option::as_ref).map(|entry| entry.target),
        Some(ResolvedImportTarget::Native(entry)) if entry.function as usize != denial_stub as usize
    ));
}

#[test]
fn custom_capability_set_grants_clock_and_denies_entropy() {
    let atoms = AtomTable::with_common_atoms();
    let registry = ModuleRegistry::new();
    let bifs = BifRegistryImpl::new();
    register_gate1_bifs(&bifs, &atoms).expect("gate1 registration");
    register_stdlib_stubs(&bifs, &atoms).expect("stdlib registration");
    let policy = CapabilitySet::from_slice(&[Capability::Pure, Capability::Clock]);

    let clock = parsed_with_import(&atoms, "erlang", "send_after", 3);
    let (resolved, report) = resolve_imports(&clock, &registry, &bifs, &policy);
    assert!(report.is_empty());
    assert!(matches!(
        resolved.first().and_then(Option::as_ref).map(|entry| entry.target),
        Some(ResolvedImportTarget::Native(entry)) if entry.function as usize != denial_stub as usize
    ));

    let entropy = parsed_with_import(&atoms, "rand", "uniform", 0);
    let (resolved, report) = resolve_imports(&entropy, &registry, &bifs, &policy);
    assert!(report.has_denied());
    assert_eq!(report.denied_imports()[0].capability, Capability::Entropy);
    assert!(matches!(
        resolved.first().and_then(Option::as_ref).map(|entry| entry.target),
        Some(ResolvedImportTarget::Native(entry)) if entry.function as usize == denial_stub as usize
    ));
}

#[test]
fn calling_denied_stub_returns_undef() {
    let mut context = ProcessContext::new();
    assert_eq!(
        denial_stub(&[Term::small_int(1)], &mut context),
        Err(Term::atom(beamr::atom::Atom::UNDEF))
    );
}

#[test]
fn scheduler_policy_can_deny_external_io_at_load_time() {
    let atoms = Arc::new(AtomTable::with_common_atoms());
    let modules = Arc::new(ModuleRegistry::new());
    let bifs = Arc::new(BifRegistryImpl::new());
    register_meridian_ffi(&bifs, &atoms).expect("meridian ffi registration");

    let scheduler = Scheduler::with_code_server_and_policy(
        SchedulerConfig {
            thread_count: Some(1),
            ..SchedulerConfig::default()
        },
        modules,
        atoms.clone(),
        bifs.clone(),
        Arc::new(LeastAuthorityPolicy),
    )
    .expect("scheduler starts");

    let parsed = parsed_with_import(&atoms, "meridian_ffi", "run_cmd", 1);
    let (_resolved, report) = resolve_imports(
        &parsed,
        &ModuleRegistry::new(),
        bifs.as_ref(),
        &LeastAuthorityPolicy,
    );

    assert!(report.has_denied());
    scheduler.shutdown();
}
