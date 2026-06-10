use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::{env, fmt};

use beamr::atom::AtomTable;
use beamr::error::LoadError;
use beamr::io::StdoutSink;
use beamr::jit::{AotCompiler, AotError, NativeCodeBundle};
use beamr::loader::{UnresolvedImportReport, embedded_module_bytes, load_module_with_origin};
use beamr::module::{ModuleOrigin, ModuleRegistry};
use beamr::native::{
    BifRegistryImpl, NativeRegistrationError,
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

#[derive(Debug)]
enum CliError {
    Usage(String),
    UnknownFlag(String),
    InvalidBeamPath(PathBuf),
    InvalidEntry(String),
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Load(LoadError),
    Aot(AotError),
    Exec(String),
    Scheduler(String),
    NativeRegistration(NativeRegistrationError),
    UnresolvedImports(String),
    ArityMismatch {
        expected: u8,
        actual: usize,
    },
    InvalidTerm(String),
    ProcessExit(String),
    MissingDirValue(String),
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

fn parse_args<I, S>(args: I) -> Result<Command, CliError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let args: Vec<String> = args.into_iter().map(Into::into).collect();
    let (command_args, runtime_args) = match args.iter().position(|arg| arg == "--") {
        Some(separator) => (
            args[..separator].to_vec(),
            args[separator.saturating_add(1)..].to_vec(),
        ),
        None => (args, Vec::new()),
    };

    // Extract --dir values first, collecting into dirs and building
    // a filtered list of remaining args.
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut filtered_args: Vec<String> = Vec::new();
    let mut iter = command_args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--dir" {
            let value = iter.next().ok_or_else(|| {
                CliError::MissingDirValue("--dir requires a directory path".into())
            })?;
            dirs.push(PathBuf::from(value));
        } else {
            filtered_args.push(arg.clone());
        }
    }

    for (index, arg) in filtered_args.iter().enumerate() {
        if arg.starts_with('-') {
            match arg.as_str() {
                "--help" | "-h" if filtered_args.len() == 1 && runtime_args.is_empty() => {
                    return Ok(Command::Help);
                }
                "--version" | "-V" if filtered_args.len() == 1 && runtime_args.is_empty() => {
                    return Ok(Command::Version);
                }
                "--entry" if index == 1 => continue,
                "--verbose"
                    if filtered_args
                        .first()
                        .is_some_and(|command| command == "compile") =>
                {
                    continue;
                }
                "--help" | "-h" | "--version" | "-V" => {
                    return Err(CliError::Usage(format!(
                        "flag '{arg}' cannot be combined with other arguments\n{USAGE}"
                    )));
                }
                _ => return Err(CliError::UnknownFlag(arg.clone())),
            }
        }
    }

    match filtered_args.as_slice() {
        [] => Err(CliError::Usage(format!("missing .beam file\n{USAGE}"))),
        [command] if command == "imports" => Err(CliError::Usage(format!(
            "imports requires a .beam file\n{USAGE}"
        ))),
        [command] if command == "compile" => Err(CliError::Usage(format!(
            "compile requires a directory\n{USAGE}"
        ))),
        [file] => {
            validate_beam_path(file)?;
            Ok(Command::Run {
                path: PathBuf::from(file),
                entry: None,
                args: runtime_args,
                dirs,
            })
        }
        [command, file] if command == "imports" => {
            validate_beam_path(file)?;
            Ok(Command::Imports {
                path: PathBuf::from(file),
            })
        }
        [command, dir] if command == "compile" => Ok(Command::Compile {
            dir: PathBuf::from(dir),
            verbose: false,
        }),
        [command, dir, flag] if command == "compile" && flag == "--verbose" => {
            Ok(Command::Compile {
                dir: PathBuf::from(dir),
                verbose: true,
            })
        }
        [command, flag, dir] if command == "compile" && flag == "--verbose" => {
            Ok(Command::Compile {
                dir: PathBuf::from(dir),
                verbose: true,
            })
        }
        [file, entry] => {
            validate_beam_path(file)?;
            Ok(Command::Run {
                path: PathBuf::from(file),
                entry: Some(parse_entry(entry)?),
                args: runtime_args,
                dirs,
            })
        }
        [file, flag, entry] if flag == "--entry" => {
            validate_beam_path(file)?;
            Ok(Command::Run {
                path: PathBuf::from(file),
                entry: Some(parse_entry(entry)?),
                args: runtime_args,
                dirs,
            })
        }
        _ => Err(CliError::Usage(format!("too many arguments\n{USAGE}"))),
    }
}

fn validate_beam_path(path: &str) -> Result<(), CliError> {
    if path.ends_with(".beam") {
        Ok(())
    } else {
        Err(CliError::InvalidBeamPath(PathBuf::from(path)))
    }
}

fn parse_entry(entry: &str) -> Result<EntryPoint, CliError> {
    let invalid_entry = || CliError::InvalidEntry(entry.to_owned());
    let (module, function_and_arity) = entry.split_once(':').ok_or_else(invalid_entry)?;
    let (function, arity) = function_and_arity
        .split_once('/')
        .ok_or_else(invalid_entry)?;

    if module.is_empty()
        || function.is_empty()
        || arity.is_empty()
        || module.contains('/')
        || function_and_arity.contains(':')
        || arity.contains('/')
        || !arity.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(CliError::InvalidEntry(entry.to_owned()));
    }

    let arity = arity
        .parse::<u8>()
        .map_err(|_| CliError::InvalidEntry(entry.to_owned()))?;

    Ok(EntryPoint {
        module: module.to_owned(),
        function: function.to_owned(),
        arity,
    })
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
    let scheduler = Scheduler::new(
        SchedulerConfig {
            thread_count: Some(1),
            ..SchedulerConfig::default()
        },
        Arc::clone(&registry),
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
    atom_table: AtomTable,
    module_registry: ModuleRegistry,
    module: std::sync::Arc<beamr::module::Module>,
    report: UnresolvedImportReport,
}

fn load_context(path: &Path, dirs: &[PathBuf]) -> Result<LoadContext, CliError> {
    let atom_table = AtomTable::with_common_atoms();
    let bif_registry = BifRegistryImpl::new();
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
            &bif_registry,
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
            &bif_registry,
            ModuleOrigin::Filesystem(path.to_path_buf()),
        )
    }
    .map_err(CliError::Load)?;

    Ok(LoadContext {
        atom_table,
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
    for import in report.imports() {
        output.push_str(&format_term(Term::atom(import.module), atom_table));
        output.push(':');
        output.push_str(&format_term(Term::atom(import.function), atom_table));
        output.push('/');
        output.push_str(&import.arity.to_string());
        output.push('\n');
    }
    output
}

impl CliError {
    const fn exit_code(&self) -> u8 {
        match self {
            Self::Load(_) | Self::Aot(_) | Self::Io { .. } | Self::Scheduler(_) => 2,
            Self::Usage(_)
            | Self::UnknownFlag(_)
            | Self::InvalidBeamPath(_)
            | Self::InvalidEntry(_)
            | Self::Exec(_)
            | Self::NativeRegistration(_)
            | Self::UnresolvedImports(_)
            | Self::ArityMismatch { .. }
            | Self::InvalidTerm(_)
            | Self::ProcessExit(_)
            | Self::MissingDirValue(_) => 1,
        }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(message) => formatter.write_str(message),
            Self::UnknownFlag(flag) => write!(formatter, "unknown flag '{flag}'"),
            Self::InvalidBeamPath(path) => write!(
                formatter,
                "expected a .beam file path, got '{}'",
                path.display()
            ),
            Self::InvalidEntry(entry) => write!(
                formatter,
                "invalid entry point '{entry}'; expected module:function/arity with arity 0..255"
            ),
            Self::Io { path, source } => {
                write!(formatter, "cannot read '{}': {source}", path.display())
            }
            Self::Load(error) => write!(formatter, "load: {error}"),
            Self::Aot(error) => write!(formatter, "aot: {error}"),
            Self::Exec(detail) => write!(formatter, "exec: {detail}"),
            Self::Scheduler(message) => write!(formatter, "scheduler: {message}"),
            Self::NativeRegistration(error) => write!(formatter, "native registration: {error}"),
            Self::UnresolvedImports(report) => {
                formatter.write_str("unresolved imports")?;
                if !report.is_empty() {
                    formatter.write_str(":\n")?;
                    formatter.write_str(report.trim_end())?;
                }
                Ok(())
            }
            Self::ArityMismatch { expected, actual } => write!(
                formatter,
                "arity mismatch: entry expects {expected} argument(s), got {actual}"
            ),
            Self::InvalidTerm(term) => write!(formatter, "invalid term literal '{term}'"),
            Self::ProcessExit(detail) => formatter.write_str(detail),
            Self::MissingDirValue(message) => formatter.write_str(message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CliError, CliSuccess, Command, EntryPoint, parse_args, parse_entry, run_cli};
    use beamr::error::{ExecError, LoadError};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn parses_help_flags() {
        assert_eq!(parse_args(["--help"]).expect("help parses"), Command::Help);
        assert_eq!(parse_args(["-h"]).expect("help parses"), Command::Help);
    }

    #[test]
    fn parses_version_flags() {
        assert_eq!(
            parse_args(["--version"]).expect("version parses"),
            Command::Version
        );
        assert_eq!(
            parse_args(["-V"]).expect("version parses"),
            Command::Version
        );
    }

    #[test]
    fn parses_beam_file_only_as_run_without_entry() {
        assert_eq!(
            parse_args(["hello.beam"]).expect("run parses"),
            Command::Run {
                path: "hello.beam".into(),
                entry: None,
                args: Vec::new(),
                dirs: Vec::new(),
            }
        );
    }

    #[test]
    fn parses_beam_file_and_entry_as_run_with_entry() {
        assert_eq!(
            parse_args(["hello.beam", "hello:main/0"]).expect("run with entry parses"),
            Command::Run {
                path: "hello.beam".into(),
                entry: Some(EntryPoint {
                    module: "hello".into(),
                    function: "main".into(),
                    arity: 0,
                }),
                args: Vec::new(),
                dirs: Vec::new(),
            }
        );
    }

    #[test]
    fn parses_entry_flag_and_runtime_args() {
        assert_eq!(
            parse_args(["hello.beam", "--entry", "hello:add/2", "--", "17", "25"])
                .expect("run with --entry and args parses"),
            Command::Run {
                path: "hello.beam".into(),
                entry: Some(EntryPoint {
                    module: "hello".into(),
                    function: "add".into(),
                    arity: 2,
                }),
                args: vec!["17".into(), "25".into()],
                dirs: Vec::new(),
            }
        );
    }

    #[test]
    fn parses_imports_command() {
        assert_eq!(
            parse_args(["imports", "hello.beam"]).expect("imports parses"),
            Command::Imports {
                path: "hello.beam".into(),
            }
        );
    }

    #[test]
    fn parses_compile_command_with_verbose() {
        assert_eq!(
            parse_args(["compile", "/tmp/beams", "--verbose"]).expect("compile verbose parses"),
            Command::Compile {
                dir: "/tmp/beams".into(),
                verbose: true,
            }
        );
        assert_eq!(
            parse_args(["compile", "/tmp/beams"]).expect("compile parses"),
            Command::Compile {
                dir: "/tmp/beams".into(),
                verbose: false,
            }
        );
    }

    #[test]
    fn rejects_non_beam_path() {
        let error = parse_args(["hello.txt"]).expect_err("non-beam path should fail");

        assert!(matches!(error, CliError::InvalidBeamPath(_)));
        assert!(error.to_string().contains(".beam"));
    }

    #[test]
    fn validates_entry_format() {
        assert_eq!(
            parse_entry("hello:main/255").expect("valid entry parses"),
            EntryPoint {
                module: "hello".into(),
                function: "main".into(),
                arity: 255,
            }
        );

        for invalid in [
            "bad-entry",
            ":main/0",
            "hello:/0",
            "hel/lo:main/0",
            "hello:main/+1",
            "hello:main/",
            "hello:main/256",
            "hello:main/not-a-number",
            "hello:main/0/1",
            "hello:main:again/0",
        ] {
            assert!(
                matches!(parse_entry(invalid), Err(CliError::InvalidEntry(_))),
                "{invalid} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_invalid_run_entry() {
        let error = parse_args(["hello.beam", "bad-entry"]).expect_err("invalid entry should fail");

        assert!(matches!(error, CliError::InvalidEntry(_)));
        assert!(error.to_string().contains("invalid entry point"));
    }

    #[test]
    fn rejects_unknown_flag() {
        let error = parse_args(["--unknown"]).expect_err("unknown flag should fail");

        assert!(matches!(&error, CliError::UnknownFlag(flag) if flag == "--unknown"));
        assert!(error.to_string().contains("--unknown"));
    }

    #[test]
    fn rejects_unknown_flag_after_imports_as_flag() {
        let error = parse_args(["imports", "--unknown"])
            .expect_err("unknown flag should be detected before path validation");

        assert!(matches!(&error, CliError::UnknownFlag(flag) if flag == "--unknown"));
    }

    #[test]
    fn error_display_formats_io_load_and_exec_errors() {
        let io_error = CliError::Io {
            path: "missing.beam".into(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "No such file or directory"),
        };
        assert_eq!(
            io_error.to_string(),
            "cannot read 'missing.beam': No such file or directory"
        );
        assert_eq!(io_error.exit_code(), 2);

        let load_error = CliError::Load(LoadError::InvalidFormat);
        assert_eq!(load_error.to_string(), "load: invalid BEAM file format");
        assert_eq!(load_error.exit_code(), 2);

        let exec_error = CliError::Exec(ExecError::Badarith.to_string());
        assert_eq!(exec_error.to_string(), "exec: arithmetic operation failed");
        assert_eq!(exec_error.exit_code(), 1);
    }

    #[test]
    fn malformed_beam_bytes_return_load_error_without_panicking() {
        let path = write_temp_beam("not a valid beam file");

        let error = run_cli([path.to_string_lossy().into_owned()])
            .expect_err("garbage .beam bytes should fail as a load error");

        assert!(matches!(error, CliError::Load(_)));
        assert_eq!(error.exit_code(), 2);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn imports_report_for_fixture_is_informational_and_omits_gate1_bifs() {
        let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../beamr/tests/fixtures/hello.beam")
            .to_string_lossy()
            .into_owned();
        let result =
            run_cli(["imports", fixture.as_str()]).expect("imports report should be informational");

        let CliSuccess::Stdout(output) = result;
        assert!(!output.contains("erlang:get_module_info/1"));
        assert!(!output.contains("erlang:get_module_info/2"));
        assert!(!output.contains("erlang:display/1"));
        assert!(output.lines().all(|line| {
            let Some((_module, function_and_arity)) = line.split_once(':') else {
                return false;
            };
            function_and_arity.split_once('/').is_some()
        }));
    }

    #[test]
    fn parses_dir_flag_with_beam_file() {
        assert_eq!(
            parse_args(["hello.beam", "--dir", "/tmp/beams"]).expect("--dir with beam file parses"),
            Command::Run {
                path: "hello.beam".into(),
                entry: None,
                args: Vec::new(),
                dirs: vec!["/tmp/beams".into()],
            }
        );
    }

    #[test]
    fn parses_multiple_dir_flags() {
        assert_eq!(
            parse_args([
                "hello.beam",
                "--dir",
                "/tmp/a",
                "--dir",
                "/tmp/b",
                "hello:main/0"
            ])
            .expect("multiple --dir flags parse"),
            Command::Run {
                path: "hello.beam".into(),
                entry: Some(EntryPoint {
                    module: "hello".into(),
                    function: "main".into(),
                    arity: 0,
                }),
                args: Vec::new(),
                dirs: vec!["/tmp/a".into(), "/tmp/b".into()],
            }
        );
    }

    #[test]
    fn rejects_dir_without_value() {
        let error =
            parse_args(["hello.beam", "--dir"]).expect_err("--dir without value should fail");

        assert!(matches!(error, CliError::MissingDirValue(_)));
    }

    fn write_temp_beam(contents: &str) -> std::path::PathBuf {
        let mut path = std::env::temp_dir();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after Unix epoch")
            .as_nanos();
        path.push(format!("beamr-cli-test-{nanos}.beam"));
        std::fs::write(&path, contents).expect("temp .beam fixture should be writable");
        path
    }
}
