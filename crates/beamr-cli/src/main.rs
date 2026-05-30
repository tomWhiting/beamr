use std::env;
use std::fs;
use std::process::ExitCode;

fn main() -> ExitCode {
    // BEAM: env::args() panics on non-UTF-8 argv; args_os never panics.
    // A non-UTF-8 argument is a user error, not a crash (R4: never panic).
    let args: Vec<String> = match env::args_os()
        .skip(1)
        .map(|a| a.into_string())
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(args) => args,
        Err(bad) => {
            eprintln!("beamr: argument is not valid UTF-8: {}", bad.to_string_lossy());
            return ExitCode::FAILURE;
        }
    };

    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("beamr: {e}");
            e.exit_code()
        }
    }
}

fn run(args: &[String]) -> Result<(), CliError> {
    let config = parse_args(args)?;

    match config.command {
        Command::Run { ref beam_path, ref entry } => {
            let bytes = read_beam_file(beam_path)?;
            execute(&bytes, entry.as_deref())?;
            Ok(())
        }
        Command::Help => {
            print_help();
            Ok(())
        }
        Command::Version => {
            println!("beamr {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Command::Imports { ref beam_path } => {
            let bytes = read_beam_file(beam_path)?;
            report_imports(&bytes)?;
            Ok(())
        }
    }
}

#[derive(Debug)]
struct Config {
    command: Command,
}

#[derive(Debug)]
enum Command {
    Run {
        beam_path: String,
        entry: Option<String>,
    },
    Imports {
        beam_path: String,
    },
    Help,
    Version,
}

fn parse_args(args: &[String]) -> Result<Config, CliError> {
    if args.is_empty() {
        return Err(CliError::NoArgs);
    }

    let first = args[0].as_str();

    match first {
        "--help" | "-h" | "help" => {
            return Ok(Config { command: Command::Help });
        }
        "--version" | "-V" | "version" => {
            return Ok(Config { command: Command::Version });
        }
        "imports" => {
            let beam_path = args.get(1)
                .ok_or_else(|| CliError::MissingArg("imports requires a .beam file path".into()))?
                .clone();
            validate_beam_path(&beam_path)?;
            return Ok(Config { command: Command::Imports { beam_path } });
        }
        _ => {}
    }

    if first.starts_with('-') {
        return Err(CliError::UnknownFlag(first.to_string()));
    }

    let beam_path = first.to_string();
    validate_beam_path(&beam_path)?;

    let entry = args.get(1).cloned();

    if let Some(ref e) = entry {
        validate_entry(e)?;
    }

    Ok(Config {
        command: Command::Run { beam_path, entry },
    })
}

fn validate_beam_path(path: &str) -> Result<(), CliError> {
    if !path.ends_with(".beam") {
        return Err(CliError::NotBeamFile(path.to_string()));
    }
    Ok(())
}

fn validate_entry(entry: &str) -> Result<(), CliError> {
    // BEAM: entry format is module:function/arity, e.g. hello:main/0
    if !entry.contains(':') || !entry.contains('/') {
        return Err(CliError::InvalidEntry(entry.to_string()));
    }
    let parts: Vec<&str> = entry.splitn(2, ':').collect();
    if parts.len() != 2 || parts[0].is_empty() {
        return Err(CliError::InvalidEntry(entry.to_string()));
    }
    let func_arity: Vec<&str> = parts[1].splitn(2, '/').collect();
    if func_arity.len() != 2 || func_arity[0].is_empty() {
        return Err(CliError::InvalidEntry(entry.to_string()));
    }
    func_arity[1].parse::<u8>()
        .map_err(|_| CliError::InvalidEntry(entry.to_string()))?;
    Ok(())
}

fn read_beam_file(path: &str) -> Result<Vec<u8>, CliError> {
    fs::read(path).map_err(|e| CliError::IoError(path.to_string(), e.to_string()))
}

fn execute(_bytes: &[u8], _entry: Option<&str>) -> Result<(), CliError> {
    // Pipeline (wired once Tom lands core types):
    // 1. AtomTable::with_common_atoms()
    // 2. BifRegistryImpl::new() + register_gate1_bifs()
    // 3. ModuleRegistry::new()
    // 4. load_module(bytes, &atom_table, &module_registry, &bif_registry)
    // 5. Determine entry: explicit or <module_name>:main/0
    // 6. module_registry.lookup_mfa(module, function, arity)
    // 7. Process::new() with entry code pointer
    // 8. Interpreter::run(&mut process) until termination
    // 9. Print result term to stdout
    Err(CliError::NotImplemented(
        "execution pipeline awaiting core crate implementation".into(),
    ))
}

fn report_imports(_bytes: &[u8]) -> Result<(), CliError> {
    // Pipeline (wired once Tom lands core types):
    // 1. AtomTable::with_common_atoms()
    // 2. BifRegistryImpl::new() + register_gate1_bifs()
    // 3. ModuleRegistry::new() (empty — no pre-loaded modules)
    // 4. load_module(bytes, &atom_table, &module_registry, &bif_registry)
    // 5. Print unresolved import report
    Err(CliError::NotImplemented(
        "import report awaiting core crate implementation".into(),
    ))
}

fn print_help() {
    println!(
        "beamr {} — BEAM bytecode runner for Gleam",
        env!("CARGO_PKG_VERSION")
    );
    println!();
    println!("USAGE:");
    println!("  beamr <file.beam> [module:function/arity]");
    println!("  beamr imports <file.beam>");
    println!("  beamr --help | --version");
    println!();
    println!("COMMANDS:");
    println!("  <file.beam>              Load and execute a .beam file");
    println!("  imports <file.beam>      Show unresolved imports for a .beam file");
    println!();
    println!("OPTIONS:");
    println!("  module:function/arity    Entry point (default: <module>:main/0)");
    println!("  -h, --help               Show this help");
    println!("  -V, --version            Show version");
    println!();
    println!("EXAMPLES:");
    println!("  beamr hello.beam                     Run hello:main/0");
    println!("  beamr hello.beam hello:greet/1       Run hello:greet/1");
    println!("  beamr imports hello.beam             List unresolved imports");
}

#[derive(Debug)]
enum CliError {
    NoArgs,
    MissingArg(String),
    UnknownFlag(String),
    NotBeamFile(String),
    InvalidEntry(String),
    IoError(String, String),
    NotImplemented(String),
}

impl CliError {
    fn exit_code(&self) -> ExitCode {
        match self {
            Self::IoError(..) => ExitCode::from(2),
            Self::NotImplemented(_) => ExitCode::from(3),
            _ => ExitCode::FAILURE,
        }
    }
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoArgs => write!(f, "no arguments provided. Run 'beamr --help' for usage"),
            Self::MissingArg(msg) => write!(f, "{msg}"),
            Self::UnknownFlag(flag) => write!(f, "unknown flag: {flag}"),
            Self::NotBeamFile(path) => write!(f, "not a .beam file: {path}"),
            Self::InvalidEntry(entry) => {
                write!(f, "invalid entry point '{entry}' — expected module:function/arity")
            }
            Self::IoError(path, err) => write!(f, "cannot read '{path}': {err}"),
            Self::NotImplemented(msg) => write!(f, "{msg}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_help_flag() {
        let args = vec!["--help".into()];
        let config = parse_args(&args).unwrap();
        assert!(matches!(config.command, Command::Help));
    }

    #[test]
    fn parse_short_help_flag() {
        let args = vec!["-h".into()];
        let config = parse_args(&args).unwrap();
        assert!(matches!(config.command, Command::Help));
    }

    #[test]
    fn parse_version_flag() {
        let args = vec!["--version".into()];
        let config = parse_args(&args).unwrap();
        assert!(matches!(config.command, Command::Version));
    }

    #[test]
    fn parse_short_version_flag() {
        let args = vec!["-V".into()];
        let config = parse_args(&args).unwrap();
        assert!(matches!(config.command, Command::Version));
    }

    #[test]
    fn parse_beam_file_only() {
        let args = vec!["hello.beam".into()];
        let config = parse_args(&args).unwrap();
        match config.command {
            Command::Run { beam_path, entry } => {
                assert_eq!(beam_path, "hello.beam");
                assert!(entry.is_none());
            }
            _ => panic!("expected Run command"),
        }
    }

    #[test]
    fn parse_beam_file_with_entry() {
        let args = vec!["hello.beam".into(), "hello:main/0".into()];
        let config = parse_args(&args).unwrap();
        match config.command {
            Command::Run { beam_path, entry } => {
                assert_eq!(beam_path, "hello.beam");
                assert_eq!(entry.as_deref(), Some("hello:main/0"));
            }
            _ => panic!("expected Run command"),
        }
    }

    #[test]
    fn parse_beam_file_with_higher_arity() {
        let args = vec!["mod.beam".into(), "mod:func/3".into()];
        let config = parse_args(&args).unwrap();
        match config.command {
            Command::Run { entry, .. } => {
                assert_eq!(entry.as_deref(), Some("mod:func/3"));
            }
            _ => panic!("expected Run command"),
        }
    }

    #[test]
    fn parse_imports_command() {
        let args = vec!["imports".into(), "hello.beam".into()];
        let config = parse_args(&args).unwrap();
        match config.command {
            Command::Imports { beam_path } => assert_eq!(beam_path, "hello.beam"),
            _ => panic!("expected Imports command"),
        }
    }

    #[test]
    fn reject_non_beam_file() {
        let args = vec!["hello.txt".into()];
        let err = parse_args(&args).unwrap_err();
        assert!(matches!(err, CliError::NotBeamFile(_)));
    }

    #[test]
    fn reject_invalid_entry_no_colon() {
        let args = vec!["hello.beam".into(), "nocolon".into()];
        let err = parse_args(&args).unwrap_err();
        assert!(matches!(err, CliError::InvalidEntry(_)));
    }

    #[test]
    fn reject_invalid_entry_no_slash() {
        let args = vec!["hello.beam".into(), "mod:func".into()];
        let err = parse_args(&args).unwrap_err();
        assert!(matches!(err, CliError::InvalidEntry(_)));
    }

    #[test]
    fn reject_invalid_entry_bad_arity() {
        let args = vec!["hello.beam".into(), "mod:func/abc".into()];
        let err = parse_args(&args).unwrap_err();
        assert!(matches!(err, CliError::InvalidEntry(_)));
    }

    #[test]
    fn reject_invalid_entry_empty_module() {
        let args = vec!["hello.beam".into(), ":func/0".into()];
        let err = parse_args(&args).unwrap_err();
        assert!(matches!(err, CliError::InvalidEntry(_)));
    }

    #[test]
    fn reject_invalid_entry_empty_function() {
        let args = vec!["hello.beam".into(), "mod:/0".into()];
        let err = parse_args(&args).unwrap_err();
        assert!(matches!(err, CliError::InvalidEntry(_)));
    }

    #[test]
    fn reject_unknown_flag() {
        let args = vec!["--foo".into()];
        let err = parse_args(&args).unwrap_err();
        assert!(matches!(err, CliError::UnknownFlag(_)));
    }

    #[test]
    fn reject_no_args() {
        let args: Vec<String> = vec![];
        let err = parse_args(&args).unwrap_err();
        assert!(matches!(err, CliError::NoArgs));
    }

    #[test]
    fn reject_imports_missing_path() {
        let args = vec!["imports".into()];
        let err = parse_args(&args).unwrap_err();
        assert!(matches!(err, CliError::MissingArg(_)));
    }

    #[test]
    fn reject_imports_non_beam() {
        let args = vec!["imports".into(), "hello.txt".into()];
        let err = parse_args(&args).unwrap_err();
        assert!(matches!(err, CliError::NotBeamFile(_)));
    }

    #[test]
    fn validate_entry_format_valid() {
        assert!(validate_entry("hello:main/0").is_ok());
        assert!(validate_entry("my_mod:my_func/3").is_ok());
        assert!(validate_entry("gleam@hello:main/0").is_ok());
    }

    #[test]
    fn validate_entry_format_invalid() {
        assert!(validate_entry("nocolon").is_err());
        assert!(validate_entry(":func/0").is_err());
        assert!(validate_entry("mod:/0").is_err());
        assert!(validate_entry("mod:func/").is_err());
        assert!(validate_entry("mod:func/999").is_err());
    }

    #[test]
    fn error_display_no_args() {
        let msg = format!("{}", CliError::NoArgs);
        assert!(msg.contains("no arguments"));
    }

    #[test]
    fn error_display_not_beam() {
        let msg = format!("{}", CliError::NotBeamFile("x.txt".into()));
        assert!(msg.contains("not a .beam file"));
        assert!(msg.contains("x.txt"));
    }

    #[test]
    fn error_display_unknown_flag() {
        let msg = format!("{}", CliError::UnknownFlag("--bad".into()));
        assert!(msg.contains("--bad"));
    }

    #[test]
    fn error_display_invalid_entry() {
        let msg = format!("{}", CliError::InvalidEntry("bad".into()));
        assert!(msg.contains("module:function/arity"));
    }

    #[test]
    fn error_display_io() {
        let msg = format!("{}", CliError::IoError("x.beam".into(), "not found".into()));
        assert!(msg.contains("x.beam"));
        assert!(msg.contains("not found"));
    }

    #[test]
    fn exit_code_io_error() {
        let code = CliError::IoError("x".into(), "y".into()).exit_code();
        assert_eq!(code, ExitCode::from(2));
    }

    #[test]
    fn exit_code_user_error() {
        let code = CliError::NoArgs.exit_code();
        assert_eq!(code, ExitCode::FAILURE);
    }

    #[test]
    fn read_beam_file_missing() {
        let err = read_beam_file("/nonexistent/path.beam").unwrap_err();
        assert!(matches!(err, CliError::IoError(..)));
    }

    #[test]
    fn parse_help_subcommand() {
        let args = vec!["help".into()];
        let config = parse_args(&args).unwrap();
        assert!(matches!(config.command, Command::Help));
    }

    #[test]
    fn parse_version_subcommand() {
        let args = vec!["version".into()];
        let config = parse_args(&args).unwrap();
        assert!(matches!(config.command, Command::Version));
    }

    #[test]
    fn validate_entry_max_arity() {
        assert!(validate_entry("mod:func/255").is_ok());
    }

    #[test]
    fn reject_entry_arity_overflow() {
        assert!(validate_entry("mod:func/256").is_err());
    }

    #[test]
    fn exit_code_not_implemented() {
        let code = CliError::NotImplemented("x".into()).exit_code();
        assert_eq!(code, ExitCode::from(3));
    }
}
