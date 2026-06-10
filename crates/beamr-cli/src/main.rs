use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::env;

use beamr::atom::AtomTable;
use beamr::io::StdoutSink;
use beamr::jit::{AotCompiler, NativeCodeBundle};
use beamr::loader::{UnresolvedImportReport, embedded_module_bytes, load_module_with_origin};
use beamr::module::{ModuleOrigin, ModuleRegistry};
use beamr::native::{
    BifRegistryImpl,
    bifs::register_gate1_bifs,
    gate3_bifs::register_gate3_bifs,
    gleam_ffi::register_gleam_ffi_bifs,
    meridian_ffi::register_meridian_ffi,
    otp_stubs::{init_otp_atoms, register_otp_stubs},
    process_bifs::register_gate2_bifs,
    selector_ffi::register_selector_bifs,
    stdlib_stubs::register_stdlib_stubs,
};
use beamr::process::ExitReason;
use beamr::scheduler::{Scheduler, SchedulerConfig};
use beamr::term::{Term, format::format_term};

const USAGE: &str = "Usage:\n  beamr <file.beam> [--entry module:function/arity] [--dir <path>]... [-- <arg>...]\n  beamr <file.beam> [module:function/arity] [--dir <path>]... [-- <arg>...]\n  beamr imports <file.beam>\n  beamr compile <dir> [--verbose]\n  beamr --help|-h\n  beamr --version|-V";

fn main() -> ExitCode {
    let outcome = run_cli(env::args().skip(1));
    match outcome {
        Ok(CliSuccess::Stdout(message)) => {
            print!("{message}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            let mut stderr = std::io::stderr().lock();
            let _ = std::io::Write::write_fmt(&mut stderr, format_args!("beamr: {error}\n"));
            ExitCode::from(error.exit_code())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Command {
    Run {
        path: PathBuf,
        entry: Option<EntryPoint>,
        args: Vec<String>,
        dirs: Vec<PathBuf>,
    },
    Imports {
        path: PathBuf,
    },
    Compile {
        dir: PathBuf,
        verbose: bool,
    },
    Help,
    Version,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EntryPoint {
    module: String,
    function: String,
    arity: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CliSuccess {
    Stdout(String),
}

fn run_cli<I, S>(args: I) -> Result<CliSuccess, CliError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    match parse_args(args)? {
        Command::Help => Ok(CliSuccess::Stdout(format!("{USAGE}\n"))),
        Command::Version => Ok(CliSuccess::Stdout(format!(
            "beamr {}\n",
            env!("CARGO_PKG_VERSION")
        ))),
        Command::Imports { path } => run_imports(&path),
        Command::Compile { dir, verbose } => run_compile(&dir, verbose),
        Command::Run {
            path,
            entry,
            args,
            dirs,
        } => run_module(&path, entry.as_ref(), &args, &dirs),
    }
}

fn run_imports(path: &Path) -> Result<CliSuccess, CliError> {
    let LoadContext {
        atom_table, report, ..
    } = load_context(path, &[])?;
    Ok(CliSuccess::Stdout(format_import_report(
        &report,
        &atom_table,
    )))
}

fn run_compile(dir: &Path, verbose: bool) -> Result<CliSuccess, CliError> {
    let started = std::time::Instant::now();
    let compiler = AotCompiler::new().map_err(CliError::Aot)?;
    let entries = std::fs::read_dir(dir).map_err(|source| CliError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    let mut beam_files = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| CliError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "beam") {
            beam_files.push(path);
        }
    }
    beam_files.sort();

    let atom_table = AtomTable::with_common_atoms();
    let mut total_compiled = 0usize;
    let mut total_skipped = 0usize;
    let mut output = String::new();

    for beam_path in beam_files {
        let result = compiler.compile_module(&beam_path).map_err(CliError::Aot)?;
        let native_path = beam_path.with_extension("beamr_native");
        let bundle = NativeCodeBundle::serialize(&result);
        std::fs::write(&native_path, bundle).map_err(|source| CliError::Io {
            path: native_path,
            source,
        })?;

        if verbose {
            let module = format_term(Term::atom(result.module()), &atom_table);
            for (function, arity, _) in result.compiled_functions() {
                output.push_str(&format!(
                    "{}:{}/{} compiled\n",
                    module,
                    format_term(Term::atom(*function), &atom_table),
                    arity
                ));
            }
            for (function, arity, reason) in result.skipped_functions() {
                output.push_str(&format!(
                    "{}:{}/{} skipped ({})\n",
                    module,
                    format_term(Term::atom(*function), &atom_table),
                    arity,
                    reason
                ));
            }
        }

        total_compiled += result.compiled_functions().len();
        total_skipped += result.skipped_functions().len();
    }

    output.push_str(&format!(
        "Compiled {} functions ({} skipped) in {:.1}s\n",
        total_compiled,
        total_skipped,
        started.elapsed().as_secs_f64()
    ));
    Ok(CliSuccess::Stdout(output))
}

fn run_module(
    path: &Path,
    entry: Option<&EntryPoint>,
    runtime_args: &[String],
    dirs: &[PathBuf],
) -> Result<CliSuccess, CliError> {
    let LoadContext {
        atom_table,
        bif_registry,
        module_registry,
        module,
        report,
    } = load_context(path, dirs)?;

    if !report.is_empty() {
        return Err(CliError::UnresolvedImports(format_import_report(
            &report,
            &atom_table,
        )));
    }

    let (module_atom, function_atom, arity) = match entry {
        Some(entry) => (
            atom_table.intern(&entry.module),
            atom_table.intern(&entry.function),
            entry.arity,
        ),
        None => (module.name, atom_table.intern("main"), 0),
    };

    if runtime_args.len() != usize::from(arity) {
        return Err(CliError::ArityMismatch {
            expected: arity,
            actual: runtime_args.len(),
        });
    }

    let args = parse_runtime_args(runtime_args, &atom_table)?;
    let registry = Arc::new(module_registry);
    // Share the load-time atom table and BIF registry with the scheduler so
    // runtime atom resolution and dynamic MFA dispatch (export funs) see the
    // same state the modules were loaded against.
    let scheduler = Scheduler::with_code_server(
        SchedulerConfig {
            thread_count: Some(1),
            ..SchedulerConfig::default()
        },
        Arc::clone(&registry),
        Arc::clone(&atom_table),
        bif_registry,
    )
    .map_err(CliError::Scheduler)?;
    scheduler.set_output_sink(Arc::new(StdoutSink));

    let pid = scheduler
        .spawn(module_atom, function_atom, args)
        .map_err(|error| CliError::Exec(error.format_with_atoms(&atom_table)))?;
    let (reason, result) = scheduler.run_until_exit(pid);
    let exit_exception = scheduler.take_exit_exception(pid);
    let exit_error = scheduler.take_exit_error(pid);
    scheduler.shutdown();

    match reason {
        ExitReason::Normal => Ok(CliSuccess::Stdout(format!(
            "{}\n",
            format_term(result.root(), &atom_table)
        ))),
        other => {
            let detail = exit_exception
                .map(|exception| exception.format_with_atoms(&atom_table))
                .or_else(|| exit_error.map(|error| error.format_with_atoms(&atom_table)))
                .unwrap_or_else(|| format_term(other.as_term(), &atom_table));
            Err(CliError::ProcessExit(detail))
        }
    }
}

struct LoadContext {
    atom_table: Arc<AtomTable>,
    bif_registry: Arc<BifRegistryImpl>,
    module_registry: ModuleRegistry,
    module: std::sync::Arc<beamr::module::Module>,
    report: UnresolvedImportReport,
}

fn load_context(path: &Path, dirs: &[PathBuf]) -> Result<LoadContext, CliError> {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let bif_registry = Arc::new(BifRegistryImpl::new());
    register_gate1_bifs(&bif_registry, &atom_table).map_err(CliError::NativeRegistration)?;
    register_gate2_bifs(&bif_registry, &atom_table).map_err(CliError::NativeRegistration)?;
    register_gate3_bifs(&bif_registry, &atom_table).map_err(CliError::NativeRegistration)?;
    register_stdlib_stubs(&bif_registry, &atom_table).map_err(CliError::NativeRegistration)?;
    register_selector_bifs(&bif_registry, &atom_table).map_err(CliError::NativeRegistration)?;
    register_gleam_ffi_bifs(&bif_registry, &atom_table).map_err(CliError::NativeRegistration)?;
    register_meridian_ffi(&bif_registry, &atom_table).map_err(CliError::NativeRegistration)?;
    init_otp_atoms(&atom_table);
    register_otp_stubs(&bif_registry, &atom_table).map_err(CliError::NativeRegistration)?;
    let module_registry = ModuleRegistry::new();

    // Load all .beam files from --dir directories first so they are
    // available in the module registry when the main module resolves imports.
    for dir in dirs {
        load_beam_dir(dir, &atom_table, &module_registry, &bif_registry)?;
    }

    let embedded_name = path.file_stem().and_then(|stem| stem.to_str());
    let embedded_bytes = embedded_name.and_then(embedded_module_bytes);
    let (module, report) = if let Some(bytes) = embedded_bytes {
        load_module_with_origin(
            &bytes,
            &atom_table,
            &module_registry,
            &*bif_registry,
            ModuleOrigin::Embedded,
        )
    } else {
        let bytes = std::fs::read(path).map_err(|source| CliError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        load_module_with_origin(
            &bytes,
            &atom_table,
            &module_registry,
            &*bif_registry,
            ModuleOrigin::Filesystem(path.to_path_buf()),
        )
    }
    .map_err(CliError::Load)?;

    Ok(LoadContext {
        atom_table,
        bif_registry,
        module_registry,
        module,
        report,
    })
}

/// Loads every `.beam` file in `dir` into the module registry.
/// Files that fail to load are silently skipped (they may be
/// modules with unsupported features that are not needed).
fn load_beam_dir(
    dir: &Path,
    atom_table: &AtomTable,
    module_registry: &ModuleRegistry,
    bif_registry: &BifRegistryImpl,
) -> Result<(), CliError> {
    let entries = std::fs::read_dir(dir).map_err(|source| CliError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| CliError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        let file_path = entry.path();
        if file_path.extension().is_some_and(|ext| ext == "beam") {
            let bytes = match std::fs::read(&file_path) {
                Ok(bytes) => bytes,
                Err(_) => continue,
            };
            // Best-effort: skip files that fail to decode.
            let _ = load_module_with_origin(
                &bytes,
                atom_table,
                module_registry,
                bif_registry,
                ModuleOrigin::Filesystem(file_path),
            );
        }
    }
    Ok(())
}

fn parse_runtime_args(args: &[String], atom_table: &AtomTable) -> Result<Vec<Term>, CliError> {
    args.iter()
        .map(|arg| parse_runtime_arg(arg, atom_table))
        .collect()
}

fn parse_runtime_arg(arg: &str, _atom_table: &AtomTable) -> Result<Term, CliError> {
    match arg.parse::<i64>() {
        Ok(value) => {
            Term::try_small_int(value).ok_or_else(|| CliError::InvalidTerm(arg.to_owned()))
        }
        Err(_) => {
            let bytes = arg.as_bytes();
            let words = 2 + bytes.len().div_ceil(8);
            let heap: &mut [u64] = Box::leak(vec![0u64; words].into_boxed_slice());
            beamr::term::binary::write_binary(heap, bytes)
                .ok_or_else(|| CliError::InvalidTerm(arg.to_owned()))
        }
    }
}

fn format_import_report(report: &UnresolvedImportReport, atom_table: &AtomTable) -> String {
    let mut output = String::new();
    // Deferred imports are module dependencies resolved at call time; they
    // must be loaded (e.g. via --dir) for the module to actually run, so
    // hiding them here would overstate what runs standalone.
    for import in report.imports().iter().chain(&report.deferred_imports()) {
        output.push_str(&format_term(Term::atom(import.module), atom_table));
        output.push(':');
        output.push_str(&format_term(Term::atom(import.function), atom_table));
        output.push('/');
        output.push_str(&import.arity.to_string());
        output.push('\n');
    }
    output
}

mod args;
mod errors;

use args::parse_args;
use errors::CliError;

#[cfg(test)]
mod tests;
