//! Crate-wide error types for beamr.
//!
//! All runtime failures are represented as values, never panics.
//! Process-level errors become exit reasons; loader errors prevent
//! module registration; interpreter errors halt the faulting process.

use std::error::Error;
use std::fmt;

/// Failures that can occur while loading and validating BEAM bytecode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadError {
    /// The input is not a valid BEAM file or uses an unsupported container shape.
    InvalidFormat,
    /// A required BEAM chunk is absent from the module being loaded.
    MissingChunk(String),
    /// Bytecode or chunk payload decoding failed.
    DecodeError(String),
    /// Decoded module contents failed semantic validation.
    ValidationError(String),
}

impl fmt::Display for LoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidFormat => formatter.write_str("invalid BEAM file format"),
            Self::MissingChunk(chunk) => write!(formatter, "missing required BEAM chunk: {chunk}"),
            Self::DecodeError(message) => {
                write!(formatter, "failed to decode BEAM data: {message}")
            }
            Self::ValidationError(message) => {
                write!(formatter, "BEAM module validation failed: {message}")
            }
        }
    }
}

impl Error for LoadError {}

/// Failures that can occur while executing BEAM code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecError {
    /// A pattern match failed.
    Badmatch,
    /// No function clause matched the provided arguments.
    FunctionClause,
    /// The target module, function, or arity is undefined.
    Undef {
        /// Module atom.
        module: crate::atom::Atom,
        /// Function atom.
        function: crate::atom::Atom,
        /// Function arity.
        arity: u8,
    },
    /// An arithmetic operation failed.
    Badarith,
    /// An argument or term type was invalid for the opcode.
    Badarg,
    /// Attempted to call a term that is not a closure.
    Badfun { term: crate::term::Term },
    /// Attempted to call a closure with the wrong number of arguments.
    Badarity {
        fun: crate::term::Term,
        args: Vec<crate::term::Term>,
    },
    /// User code exited explicitly.
    UserExit,
    /// Decoded instruction opcode is not known to the VM.
    UnknownOpcode { opcode: u8 },
    /// Decoded instruction is valid but belongs to a future implementation gate.
    UnsupportedOpcode { name: &'static str },
    /// Operand shape or value was invalid for the opcode.
    InvalidOperand(&'static str),
    /// A local label could not be resolved to an instruction pointer.
    InvalidLabel { label: u32 },
    /// Import table entry was missing.
    InvalidImport { index: usize },
    /// A heap check failed and GC must run before continuing.
    GcNeeded { requested: usize, available: usize },
    /// A boxed literal cannot be materialized by the no-allocation move opcode.
    UnsupportedLiteral,
    /// Stack operation failed.
    Stack(crate::process::stack::StackError),
    /// Heap allocation failed.
    HeapFull { requested: usize, available: usize },
}

impl fmt::Display for ExecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Badmatch => formatter.write_str("pattern match failed"),
            Self::FunctionClause => formatter.write_str("no matching function clause"),
            Self::Undef {
                module,
                function,
                arity,
            } => write!(
                formatter,
                "undefined function {module:?}:{function:?}/{arity}"
            ),
            Self::Badarith => formatter.write_str("arithmetic operation failed"),
            Self::Badarg => formatter.write_str("bad argument"),
            Self::Badfun { term } => write!(formatter, "bad function term {term:?}"),
            Self::Badarity { fun, args } => {
                write!(
                    formatter,
                    "bad arity for function {fun:?} with args {args:?}"
                )
            }
            Self::UserExit => formatter.write_str("process exited explicitly"),
            Self::UnknownOpcode { opcode } => write!(formatter, "unknown opcode {opcode}"),
            Self::UnsupportedOpcode { name } => write!(formatter, "unsupported opcode {name}"),
            Self::InvalidOperand(context) => write!(formatter, "invalid operand for {context}"),
            Self::InvalidLabel { label } => write!(formatter, "invalid code label {label}"),
            Self::InvalidImport { index } => write!(formatter, "invalid import index {index}"),
            Self::GcNeeded {
                requested,
                available,
            } => write!(
                formatter,
                "GC needed before allocating/checking {requested} heap words ({available} available)"
            ),
            Self::UnsupportedLiteral => formatter.write_str("unsupported boxed literal"),
            Self::Stack(error) => write!(formatter, "stack error: {error}"),
            Self::HeapFull {
                requested,
                available,
            } => write!(
                formatter,
                "heap full: requested {requested} words with {available} available"
            ),
        }
    }
}

impl Error for ExecError {}

impl From<crate::process::stack::StackError> for ExecError {
    fn from(error: crate::process::stack::StackError) -> Self {
        Self::Stack(error)
    }
}

impl From<crate::process::heap::HeapFull> for ExecError {
    fn from(error: crate::process::heap::HeapFull) -> Self {
        Self::HeapFull {
            requested: error.requested(),
            available: error.available(),
        }
    }
}

/// Top-level error type for beamr operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BeamrError {
    /// A module loading failure.
    Load(LoadError),
    /// A runtime execution failure.
    Exec(ExecError),
}

impl From<LoadError> for BeamrError {
    fn from(error: LoadError) -> Self {
        Self::Load(error)
    }
}

impl From<ExecError> for BeamrError {
    fn from(error: ExecError) -> Self {
        Self::Exec(error)
    }
}

impl fmt::Display for BeamrError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Load(error) => write!(formatter, "load error: {error}"),
            Self::Exec(error) => write!(formatter, "execution error: {error}"),
        }
    }
}

impl Error for BeamrError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Load(error) => Some(error),
            Self::Exec(error) => Some(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BeamrError, ExecError, LoadError};

    #[test]
    fn load_error_display_is_human_readable() {
        let formatted = LoadError::MissingChunk("Atom".into()).to_string();

        assert!(!formatted.is_empty());
        assert!(formatted.contains("missing required BEAM chunk"));
        assert!(formatted.contains("Atom"));
    }

    #[test]
    fn exec_error_display_is_human_readable() {
        let formatted = ExecError::Badarith.to_string();

        assert!(!formatted.is_empty());
        assert!(formatted.contains("arithmetic"));
    }

    #[test]
    fn beamr_error_wraps_load_errors() {
        let error = BeamrError::from(LoadError::InvalidFormat);

        assert!(matches!(error, BeamrError::Load(LoadError::InvalidFormat)));
        assert!(!error.to_string().is_empty());
    }

    #[test]
    fn beamr_error_wraps_exec_errors() {
        let error = BeamrError::from(ExecError::Badarith);

        assert!(matches!(error, BeamrError::Exec(ExecError::Badarith)));
        assert!(!error.to_string().is_empty());
    }
}
