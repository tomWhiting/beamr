use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::{env, fmt};

use beamr::atom::{Atom, AtomTable};
use beamr::error::{ExecError, LoadError};
use beamr::interpreter::{self, ExecutionResult};
use beamr::loader::{Instruction, UnresolvedImportReport, load_module};
use beamr::module::{Module, ModuleRegistry};
use beamr::native::{
    BifRegistryImpl, NativeRegistrationError, bifs::register_gate1_bifs,
    process_bifs::register_gate2_bifs,
    gate3_bifs::register_gate3_bifs,
    stdlib_stubs::register_stdlib_stubs,
};
use beamr::process::heap::DEFAULT_HEAP_SIZE;
use beamr::process::registry::ProcessTable;
use beamr::process::{CodePosition, ExitReason, Process};
use beamr::term::{Tag, Term};

const USAGE: &str = "Usage:\n  beamr <file.beam> [--entry module:function/arity] [--dir <path>]... [-- <arg>...]\n  beamr <file.beam> [module:function/arity] [--dir <path>]... [-- <arg>...]\n  beamr imports <file.beam>\n  beamr --help|-h\n  beamr --version|-V";

fn main() -> ExitCode {
    let outcome = run_cli(env::args().skip(1));
    match outcome {
        Ok(CliSuccess::Stdout(message)) => {
            print!("{message}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("beamr: {error}");
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
    Exec(ExecError),
    NativeRegistration(NativeRegistrationError),
    UnresolvedImports(String),
    ArityMismatch {
        expected: u8,
        actual: usize,
    },
    InvalidTerm(String),
    ProcessExit(ExitReason),
    ProcessDidNotComplete(&'static str),
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
    let code_pointer = module_registry
        .lookup_mfa(module_atom, function_atom, arity)
        .map_err(CliError::Exec)?;
    let instruction_pointer =
        label_ip(&code_pointer.module, code_pointer.label).map_err(CliError::Exec)?;
    let process_table = ProcessTable::new();
    let pid = process_table.spawn();
    let mut process = Process::new(pid, DEFAULT_HEAP_SIZE);
    process.set_code_position(Some(CodePosition {
        module: code_pointer.module.name,
        instruction_pointer,
    }));
    for (index, arg) in args.into_iter().enumerate() {
        let register = u8::try_from(index).map_err(|_| CliError::Exec(ExecError::Badarg))?;
        process.set_x_reg(register, arg);
    }

    match interpreter::run_with_registry(&mut process, &code_pointer.module, &module_registry)
        .map_err(CliError::Exec)?
    {
        ExecutionResult::Exited(ExitReason::Normal) => Ok(CliSuccess::Stdout(format!(
            "{}\n",
            format_term(process.x_reg(0), &atom_table)
        ))),
        ExecutionResult::Exited(reason) => Err(CliError::ProcessExit(reason)),
        ExecutionResult::Yielded => Err(CliError::ProcessDidNotComplete("yielded")),
        ExecutionResult::Waiting => Err(CliError::ProcessDidNotComplete("waiting")),
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
    let mut bif_registry = BifRegistryImpl::new();
    register_gate1_bifs(&mut bif_registry, &atom_table).map_err(CliError::NativeRegistration)?;
    register_gate2_bifs(&mut bif_registry, &atom_table).map_err(CliError::NativeRegistration)?;
    register_gate3_bifs(&mut bif_registry, &atom_table).map_err(CliError::NativeRegistration)?;
    register_stdlib_stubs(&mut bif_registry, &atom_table).map_err(CliError::NativeRegistration)?;
    let module_registry = ModuleRegistry::new();

    // Load all .beam files from --dir directories first so they are
    // available in the module registry when the main module resolves imports.
    for dir in dirs {
        load_beam_dir(dir, &atom_table, &module_registry, &bif_registry)?;
    }

    let bytes = std::fs::read(path).map_err(|source| CliError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    let (module, report) = load_module(&bytes, &atom_table, &module_registry, &bif_registry)
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
            let _ = load_module(&bytes, atom_table, module_registry, bif_registry);
        }
    }
    Ok(())
}

fn parse_runtime_args(args: &[String], atom_table: &AtomTable) -> Result<Vec<Term>, CliError> {
    args.iter()
        .map(|arg| parse_runtime_arg(arg, atom_table))
        .collect()
}

fn parse_runtime_arg(arg: &str, atom_table: &AtomTable) -> Result<Term, CliError> {
    match arg.parse::<i64>() {
        Ok(value) => {
            Term::try_small_int(value).ok_or_else(|| CliError::InvalidTerm(arg.to_owned()))
        }
        Err(_) => Ok(Term::atom(atom_table.intern(arg))),
    }
}

fn label_ip(module: &Module, label: u32) -> Result<usize, ExecError> {
    module
        .code
        .iter()
        .position(|instruction| matches!(instruction, Instruction::Label { label: seen } if *seen == label))
        .ok_or(ExecError::InvalidLabel { label })
}

fn format_import_report(report: &UnresolvedImportReport, atom_table: &AtomTable) -> String {
    let mut output = String::new();
    for import in report.imports() {
        output.push_str(&format_atom(import.module, atom_table));
        output.push(':');
        output.push_str(&format_atom(import.function, atom_table));
        output.push('/');
        output.push_str(&import.arity.to_string());
        output.push('\n');
    }
    output
}

fn format_term(term: Term, atom_table: &AtomTable) -> String {
    match term.tag() {
        Tag::SmallInt => term
            .as_small_int()
            .map(|value| value.to_string())
            .unwrap_or_else(|| format!("{term:?}")),
        Tag::Atom => term
            .as_atom()
            .map(|atom| format_atom(atom, atom_table))
            .unwrap_or_else(|| format!("{term:?}")),
        Tag::Nil => "[]".to_owned(),
        Tag::Pid => term
            .as_pid()
            .map(|pid| format!("<0.{pid}.0>"))
            .unwrap_or_else(|| format!("{term:?}")),
        Tag::Boxed | Tag::List => format!("{term:?}"),
    }
}

fn format_atom(atom: Atom, atom_table: &AtomTable) -> String {
    atom_table
        .resolve(atom)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{atom:?}"))
}

impl CliError {
    const fn exit_code(&self) -> u8 {
        match self {
            Self::Load(_) | Self::Io { .. } => 2,
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
            | Self::ProcessDidNotComplete(_)
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
            Self::Exec(error) => write!(formatter, "exec: {error}"),
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
            Self::ProcessExit(reason) => write!(formatter, "process exited with {reason:?}"),
            Self::ProcessDidNotComplete(state) => {
                write!(formatter, "process did not complete: {state}")
            }
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

        let exec_error = CliError::Exec(ExecError::Badarith);
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
            parse_args(["hello.beam", "--dir", "/tmp/beams"])
                .expect("--dir with beam file parses"),
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
