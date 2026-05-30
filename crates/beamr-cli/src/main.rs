use std::env;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use beamr::atom::{Atom, AtomTable};
use beamr::error::{ExecError, LoadError};
use beamr::loader::{UnresolvedImportReport, load_module};
use beamr::module::ModuleRegistry;
use beamr::native::bifs::register_gate1_bifs;
use beamr::native::{BifRegistryImpl, NativeRegistrationError};
use beamr::process::registry::ProcessTable;

const USAGE: &str = "Usage:\n  beamr <file.beam> [module:function/arity]\n  beamr imports <file.beam>\n  beamr --help|-h\n  beamr --version|-V";

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
    ExecutionUnavailable,
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
        Command::Run { path, entry } => run_module(&path, entry.as_ref()),
    }
}

fn parse_args<I, S>(args: I) -> Result<Command, CliError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let args: Vec<String> = args.into_iter().map(Into::into).collect();

    for arg in &args {
        if arg.starts_with('-') {
            return match arg.as_str() {
                "--help" | "-h" if args.len() == 1 => Ok(Command::Help),
                "--version" | "-V" if args.len() == 1 => Ok(Command::Version),
                "--help" | "-h" | "--version" | "-V" => Err(CliError::Usage(format!(
                    "flag '{arg}' cannot be combined with other arguments\n{USAGE}"
                ))),
                _ => Err(CliError::UnknownFlag(arg.clone())),
            };
        }
    }

    match args.as_slice() {
        [] => Err(CliError::Usage(format!("missing .beam file\n{USAGE}"))),
        [command] if command == "imports" => Err(CliError::Usage(format!(
            "imports requires a .beam file\n{USAGE}"
        ))),
        [file] => {
            validate_beam_path(file)?;
            Ok(Command::Run {
                path: PathBuf::from(file),
                entry: None,
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
    let (module, function_and_arity) = entry
        .split_once(':')
        .ok_or_else(|| CliError::InvalidEntry(entry.to_owned()))?;
    let (function, arity) = function_and_arity
        .split_once('/')
        .ok_or_else(|| CliError::InvalidEntry(entry.to_owned()))?;

    if module.is_empty()
        || function.is_empty()
        || arity.is_empty()
        || function_and_arity.contains(':')
        || arity.contains('/')
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
    } = load_context(path)?;
    Ok(CliSuccess::Stdout(format_import_report(
        &report,
        &atom_table,
    )))
}

fn run_module(path: &Path, entry: Option<&EntryPoint>) -> Result<CliSuccess, CliError> {
    let LoadContext {
        atom_table,
        module_registry,
        module,
        report,
    } = load_context(path)?;

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

    let code_pointer = module_registry
        .lookup_mfa(module_atom, function_atom, arity)
        .map_err(CliError::Exec)?;
    let process_table = ProcessTable::new();
    let pid = process_table.spawn();
    let _ = (code_pointer, pid);

    Err(CliError::ExecutionUnavailable)
}

struct LoadContext {
    atom_table: AtomTable,
    module_registry: ModuleRegistry,
    module: std::sync::Arc<beamr::module::Module>,
    report: UnresolvedImportReport,
}

fn load_context(path: &Path) -> Result<LoadContext, CliError> {
    let bytes = std::fs::read(path).map_err(|source| CliError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    let atom_table = AtomTable::with_common_atoms();
    let mut bif_registry = BifRegistryImpl::new();
    register_gate1_bifs(&mut bif_registry, &atom_table).map_err(CliError::NativeRegistration)?;
    let module_registry = ModuleRegistry::new();
    let (module, report) = load_module(&bytes, &atom_table, &module_registry, &bif_registry)
        .map_err(CliError::Load)?;

    Ok(LoadContext {
        atom_table,
        module_registry,
        module,
        report,
    })
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
            | Self::ExecutionUnavailable => 1,
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
            Self::ExecutionUnavailable => formatter.write_str(
                "execution unavailable: interpreter execution is not implemented in this build",
            ),
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
        assert!(output.contains("erlang:get_module_info/1"));
        assert!(output.contains("erlang:get_module_info/2"));
        assert!(!output.contains("erlang:display/1"));
        assert!(output.lines().all(|line| {
            let Some((_module, function_and_arity)) = line.split_once(':') else {
                return false;
            };
            function_and_arity.split_once('/').is_some()
        }));
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
