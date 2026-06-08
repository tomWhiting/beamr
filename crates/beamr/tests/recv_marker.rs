use std::sync::Arc;

use beamr::atom::AtomTable;
use beamr::interpreter::{ExecutionResult, NativeServices, run_with_native_services};
use beamr::loader::{Instruction, load_beam_chunks, prepare_module};
use beamr::module::{Module, ModuleRegistry};
use beamr::native::BifRegistryImpl;
use beamr::native::bifs::register_gate1_bifs;
use beamr::native::gate3_bifs::register_gate3_bifs;
use beamr::native::process_bifs::register_gate2_bifs;
use beamr::process::{CodePosition, ExitReason, Process};
use beamr::term::Term;

const ROUNDTRIP_BEAM: &[u8] = include_bytes!("fixtures/recv_marker_fixture.beam");
const TIMEOUT_BEAM: &[u8] = include_bytes!("fixtures/recv_marker_timeout_fixture.beam");

fn bif_registry(atom_table: &AtomTable) -> BifRegistryImpl {
    let registry = BifRegistryImpl::new();
    register_gate1_bifs(&registry, atom_table).expect("gate1 bifs register");
    register_gate2_bifs(&registry, atom_table).expect("gate2 bifs register");
    register_gate3_bifs(&registry, atom_table).expect("gate3 bifs register");
    registry
}

fn load_fixture(bytes: &[u8], atoms: &AtomTable, bifs: &BifRegistryImpl) -> Module {
    let registry = ModuleRegistry::new();
    let (module, unresolved) =
        prepare_module(bytes, atoms, &registry, bifs).expect("recv_marker fixture prepares");
    assert!(
        unresolved.is_empty(),
        "recv_marker fixture has unresolved imports: {unresolved}"
    );
    module
}

fn exported_entry(module: &Module, atoms: &AtomTable, function: &str, arity: u8) -> usize {
    let function = atoms.intern(function);
    let label = *module
        .exports
        .get(&(function, arity))
        .expect("export exists");
    module
        .code
        .iter()
        .position(|instruction| matches!(instruction, Instruction::Label { label: candidate } if *candidate == label))
        .expect("export label exists")
}

fn call(module: &Module, atoms: Arc<AtomTable>, function: &str, args: &[Term]) -> Term {
    let arity = u8::try_from(args.len()).expect("test arity fits u8");
    let entry_ip = exported_entry(module, &atoms, function, arity);
    let mut process = Process::new(1, 4096);
    for (index, arg) in args.iter().enumerate() {
        process.set_x_reg(
            u16::try_from(index).expect("test register index fits u16"),
            *arg,
        );
    }
    process.set_code_position(Some(CodePosition {
        module: module.name,
        instruction_pointer: entry_ip,
    }));
    let services = NativeServices {
        atom_table: Some(atoms),
        local_node: None,
        timers: None,
        spawn_facility: None,
        remote_spawn_facility: None,
        link_facility: None,
        supervision_facility: None,
        io_sink: None,
        code_management_facility: None,
        process_info_facility: None,
        system_info_facility: None,
        group_leader_facility: None,
        ets_facility: None,
        io_facility: None,
        io_message_facility: None,
        net_kernel: None,
        distribution_send: None,
        file_io_facility: None,
        tcp_io_facility: None,
    };

    assert_eq!(
        run_with_native_services(&mut process, module, &ModuleRegistry::new(), &services),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    process.x_reg(0)
}

#[test]
fn otp_24_recv_marker_fixture_decodes_all_marker_opcodes() {
    let atoms = AtomTable::with_common_atoms();
    let parsed = load_beam_chunks(ROUNDTRIP_BEAM, &atoms).expect("roundtrip fixture parses");

    assert!(
        parsed
            .instructions
            .iter()
            .any(|instruction| matches!(instruction, Instruction::RecvMarkerReserve { .. })),
        "fixture should decode recv_marker_reserve"
    );
    assert!(
        parsed
            .instructions
            .iter()
            .any(|instruction| matches!(instruction, Instruction::RecvMarkerClear { .. })),
        "fixture should decode recv_marker_clear"
    );
}

#[test]
#[ignore] // fixture is AI-generated with incomplete code section (labels 7,9 missing)
fn compiled_recv_marker_receive_returns_sent_message() {
    let atoms = Arc::new(AtomTable::with_common_atoms());
    let bifs = bif_registry(&atoms);
    let module = load_fixture(ROUNDTRIP_BEAM, &atoms, &bifs);

    assert_eq!(
        call(
            &module,
            Arc::clone(&atoms),
            "roundtrip",
            &[Term::small_int(42)]
        ),
        Term::small_int(42)
    );
}

#[test]
#[ignore] // fixture is AI-generated with incomplete code section
fn compiled_recv_marker_receive_timeout_path_returns_timeout_atom() {
    let atoms = Arc::new(AtomTable::with_common_atoms());
    let timeout = atoms.intern("timeout");
    let bifs = bif_registry(&atoms);
    let module = load_fixture(TIMEOUT_BEAM, &atoms, &bifs);

    assert_eq!(call(&module, atoms, "await", &[]), Term::atom(timeout));
}
