//! Integration test: load all OTP fixture .beam files without errors.
//!
//! Verifies that the loader can decode every fixture in
//! `tests/fixtures/otp/` — including modules that use EXPORT_EXT
//! literals (tag 113). Unresolved imports are expected (since we
//! don't provide the full OTP runtime) but load crashes are not.

use beamr::atom::AtomTable;
use beamr::loader::load_beam_chunks;

/// All five OTP fixture files must load without decode errors.
/// We use `load_beam_chunks` (parse-only, no import resolution)
/// so the test is self-contained and does not depend on BIF
/// registration or module registry state.
#[test]
fn all_otp_fixtures_load_without_errors() {
    let fixtures_dir =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/otp");

    let expected_files = [
        "gleam_erlang_ffi.beam",
        "gleam_otp_external.beam",
        "gleam@erlang@process.beam",
        "gleam@otp@actor.beam",
        "gleam@otp@supervisor.beam",
    ];

    let atom_table = AtomTable::with_common_atoms();
    let mut loaded_count = 0;

    for filename in &expected_files {
        let path = fixtures_dir.join(filename);
        let bytes = std::fs::read(&path).unwrap_or_else(|err| {
            panic!("failed to read fixture {filename}: {err}");
        });

        let parsed = load_beam_chunks(&bytes, &atom_table).unwrap_or_else(|err| {
            panic!("failed to load fixture {filename}: {err}");
        });

        // Sanity: the module should have at least one atom (its own name)
        // and at least one instruction.
        assert!(
            !parsed.atoms.is_empty(),
            "{filename}: atom table should not be empty"
        );
        assert!(
            !parsed.instructions.is_empty(),
            "{filename}: code section should not be empty"
        );

        loaded_count += 1;
    }

    assert_eq!(
        loaded_count,
        expected_files.len(),
        "all fixture files should have been loaded"
    );
}

/// gleam@otp@actor.beam specifically exercises EXPORT_EXT (tag 113)
/// in its literal table. Verify it loads and has a non-empty literal
/// table (which is where the EXPORT_EXT terms live).
#[test]
fn gleam_otp_actor_has_export_ext_literals() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/otp/gleam@otp@actor.beam");

    let bytes = std::fs::read(&path).expect("fixture should be readable");
    let atom_table = AtomTable::with_common_atoms();

    let parsed = load_beam_chunks(&bytes, &atom_table).expect("actor module should load");

    // The module must have literals (EXPORT_EXT values are stored there).
    assert!(
        !parsed.literals.is_empty(),
        "gleam@otp@actor should have literals including EXPORT_EXT terms"
    );
}
