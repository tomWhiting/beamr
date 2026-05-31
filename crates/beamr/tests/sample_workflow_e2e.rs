//! End-to-end test for the sample Gleam workflow.
//!
//! The fixtures are expected at repository-root `test-workflows/sample/`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use beamr::atom::{Atom, AtomTable};
use beamr::loader::load_module;
use beamr::module::ModuleRegistry;
use beamr::native::BifRegistryImpl;
use beamr::native::bifs::register_gate1_bifs;
use beamr::native::gate3_bifs::register_gate3_bifs;
use beamr::native::gleam_ffi::register_gleam_ffi_bifs;
use beamr::native::meridian_ffi::register_meridian_ffi;
use beamr::native::otp_stubs::{init_otp_atoms, register_otp_stubs};
use beamr::native::process_bifs::register_gate2_bifs;
use beamr::native::selector_ffi::register_selector_bifs;
use beamr::native::stdlib_stubs::register_stdlib_stubs;
use beamr::process::ExitReason;
use beamr::scheduler::{Scheduler, SchedulerConfig};
use beamr::term::Term;
use beamr::term::binary::{self, Binary};
use beamr::term::boxed::Tuple;

fn full_bif_registry(atom_table: &AtomTable) -> BifRegistryImpl {
    let mut registry = BifRegistryImpl::new();
    register_gate1_bifs(&mut registry, atom_table).expect("gate1");
    register_gate2_bifs(&mut registry, atom_table).expect("gate2");
    register_gate3_bifs(&mut registry, atom_table).expect("gate3");
    register_stdlib_stubs(&mut registry, atom_table).expect("stdlib");
    register_selector_bifs(&mut registry, atom_table).expect("selector");
    register_gleam_ffi_bifs(&mut registry, atom_table).expect("gleam_ffi");
    register_meridian_ffi(&mut registry, atom_table).expect("meridian_ffi");
    init_otp_atoms(atom_table);
    register_otp_stubs(&mut registry, atom_table).expect("otp_stubs");
    registry
}

#[test]
#[ignore] // 2 unresolved: erlang:fun_info/2, io_lib_format:fwrite_g/1
fn sample_workflow_run_completes_end_to_end() {
    let sample_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("test-workflows/sample");
    if !sample_dir.is_dir() {
        eprintln!(
            "skipping sample workflow E2E: missing fixtures at {}",
            sample_dir.display()
        );
        return;
    }

    let atom_table = AtomTable::with_common_atoms();
    let bif_registry = full_bif_registry(&atom_table);
    let module_registry = Arc::new(ModuleRegistry::new());
    load_all_beams(&sample_dir, &atom_table, &module_registry, &bif_registry);

    let input_path = std::env::temp_dir().join(format!(
        "beamr-sample-workflow-input-{}.txt",
        std::process::id()
    ));
    let output_path = Path::new("/tmp/gleam-workflow-output.txt");
    let _ = std::fs::remove_file(output_path);
    std::fs::write(&input_path, "sample content\n").expect("write sample input file");

    let path_arg = make_binary(input_path.to_string_lossy().as_bytes());
    let scheduler = Scheduler::new(
        SchedulerConfig {
            thread_count: Some(1),
        },
        Arc::clone(&module_registry),
    )
    .expect("scheduler starts");

    let pid = scheduler
        .spawn(
            atom_table.intern("sample_workflow"),
            atom_table.intern("run"),
            vec![path_arg],
        )
        .expect("spawn sample_workflow:run/1");
    let (reason, result) = scheduler.run_until_exit(pid);
    scheduler.shutdown();

    let _ = std::fs::remove_file(&input_path);

    assert_eq!(reason, ExitReason::Normal, "result: {result:?}");
    assert_return_tuple(result, &atom_table);
    assert!(output_path.exists(), "workflow should write output to /tmp");
    let _ = std::fs::remove_file(output_path);
}

fn load_all_beams(
    dir: &Path,
    atom_table: &AtomTable,
    module_registry: &ModuleRegistry,
    bif_registry: &BifRegistryImpl,
) {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("read sample dir")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "beam"))
        .collect();
    paths.sort();
    assert!(
        !paths.is_empty(),
        "sample directory contains no .beam files"
    );

    for path in paths {
        let bytes = std::fs::read(&path).unwrap_or_else(|err| {
            panic!("failed to read {}: {err}", path.display());
        });
        let (_module, unresolved) = load_module(&bytes, atom_table, module_registry, bif_registry)
            .unwrap_or_else(|err| panic!("failed to load {}: {err}", path.display()));
        let imports = unresolved.imports();
        if !imports.is_empty() {
            eprintln!(
                "WARN: {} has {} unresolved import(s): {}",
                path.display(),
                imports.len(),
                imports
                    .iter()
                    .map(|import| format!(
                        "{}:{}/{}",
                        atom_table.resolve(import.module).unwrap_or("<unknown>"),
                        atom_table.resolve(import.function).unwrap_or("<unknown>"),
                        import.arity
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
}

fn assert_return_tuple(result: Term, atom_table: &AtomTable) {
    let outer = Tuple::new(result).expect("result should be {ok, Inner}");
    assert_eq!(outer.arity(), 2);
    assert_eq!(outer.get(0), Some(Term::atom(Atom::OK)));

    let inner = Tuple::new(outer.get(1).expect("inner tuple")).expect("inner tuple");
    assert_eq!(inner.arity(), 4);
    assert_eq!(
        inner.get(0),
        Some(Term::atom(atom_table.intern("sample_workflow")))
    );
    let content = Binary::new(inner.get(1).expect("content binary")).expect("content binary");
    assert_eq!(content.as_bytes(), b"sample content\n");
    let cmd_output =
        Binary::new(inner.get(2).expect("cmd output binary")).expect("cmd output binary");
    assert!(
        !cmd_output.as_bytes().is_empty(),
        "real shell command output should be present"
    );
    assert_eq!(inner.get(3), Some(Term::atom(Atom::TRUE)));
}

fn make_binary(bytes: &[u8]) -> Term {
    let data_words = binary::packed_word_count(bytes.len());
    let heap: &mut [u64] = Box::leak(vec![0u64; 2 + data_words].into_boxed_slice());
    binary::write_binary(heap, bytes).expect("binary heap sized correctly")
}
