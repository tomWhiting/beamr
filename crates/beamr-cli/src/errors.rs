//! CLI error type, exit-code mapping, and user-facing formatting.

use std::fmt;
use std::path::PathBuf;

use beamr::error::LoadError;
use beamr::jit::AotError;
use beamr::native::NativeRegistrationError;
use beamr::replay::ReplayLogFileError;

#[derive(Debug)]
pub enum CliError {
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
    MissingLogValue(String),
    ReplayLog(ReplayLogFileError),
    ReplayLogMissingTranscript,
}

impl CliError {
    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::Load(_)
            | Self::Aot(_)
            | Self::Io { .. }
            | Self::Scheduler(_)
            | Self::ReplayLog(_) => 2,
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
            | Self::MissingDirValue(_)
            | Self::MissingLogValue(_)
            | Self::ReplayLogMissingTranscript => 1,
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
            Self::MissingDirValue(message) | Self::MissingLogValue(message) => {
                formatter.write_str(message)
            }
            Self::ReplayLog(error) => write!(formatter, "replay log: {error}"),
            Self::ReplayLogMissingTranscript => formatter.write_str(
                "replay log does not contain a recorded CLI transcript; use beamr record to create replayable logs",
            ),
        }
    }
}
