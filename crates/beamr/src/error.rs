//! Crate-wide error types for beamr.
//!
//! All runtime failures are represented as values, never panics.
//! Process-level errors become exit reasons; loader errors prevent
//! module registration; interpreter errors halt the faulting process.

use std::error::Error;
use std::fmt;

use crate::atom::AtomTable;
use crate::namespace::NamespaceId;
use crate::term::format::format_term;

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
    /// A second old code version would be created before the existing old
    /// version was purged.
    OldCodeStillRunning,
    /// The requested module namespace does not exist.
    UnknownNamespace { namespace: NamespaceId },
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
            Self::OldCodeStillRunning => formatter
                .write_str("old code is still running and must be purged before loading again"),
            Self::UnknownNamespace { namespace } => {
                write!(formatter, "unknown module namespace {:?}", namespace)
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
    /// A distributed send could not reach the target node.
    NoConnection,
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
            } => {
                let fallback = AtomTable::with_common_atoms();
                write!(
                    formatter,
                    "undefined function {}:{}/{}",
                    fallback.resolve(*module).unwrap_or("#<unknown atom>"),
                    fallback.resolve(*function).unwrap_or("#<unknown atom>"),
                    arity
                )
            }
            Self::Badarith => formatter.write_str("arithmetic operation failed"),
            Self::Badarg => formatter.write_str("bad argument"),
            Self::Badfun { term } => {
                let fallback = AtomTable::with_common_atoms();
                write!(
                    formatter,
                    "bad function term {}",
                    format_term(*term, &fallback)
                )
            }
            Self::Badarity { fun, args } => {
                let fallback = AtomTable::with_common_atoms();
                let formatted_args = args
                    .iter()
                    .map(|arg| format_term(*arg, &fallback))
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(
                    formatter,
                    "bad arity for function {} with args [{}]",
                    format_term(*fun, &fallback),
                    formatted_args
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
            Self::NoConnection => formatter.write_str("distributed send failed: noconnection"),
        }
    }
}

impl ExecError {
    /// Format this execution error for user-facing diagnostics using atom-name
    /// resolution from `atom_table`.
    #[must_use]
    pub fn format_with_atoms(&self, atom_table: &AtomTable) -> String {
        match self {
            Self::Undef {
                module,
                function,
                arity,
            } => format!(
                "undefined function {}:{}/{}",
                atom_table.resolve(*module).unwrap_or("#<unknown atom>"),
                atom_table.resolve(*function).unwrap_or("#<unknown atom>"),
                arity
            ),
            Self::Badfun { term } => {
                format!("bad function term {}", format_term(*term, atom_table))
            }
            Self::Badarity { fun, args } => {
                let formatted_args = args
                    .iter()
                    .map(|arg| format_term(*arg, atom_table))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "bad arity for function {} with args [{}]",
                    format_term(*fun, atom_table),
                    formatted_args
                )
            }
            _ => self.to_string(),
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
    use crate::atom::{Atom, AtomTable};
    use crate::term::Term;
    use crate::term::boxed::{write_cons, write_tuple};

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
    fn exec_error_format_with_atoms_resolves_undef_mfa() {
        let table = AtomTable::with_common_atoms();
        let module = table.intern("my_mod");
        let function = table.intern("my_fun");
        let error = ExecError::Undef {
            module,
            function,
            arity: 2,
        };

        assert_eq!(
            error.format_with_atoms(&table),
            "undefined function my_mod:my_fun/2"
        );
    }

    #[test]
    fn exec_error_format_with_atoms_formats_badfun_and_badarity_terms() {
        let table = AtomTable::with_common_atoms();
        let mut tuple_heap = [0_u64; 3];
        let fun = match write_tuple(&mut tuple_heap, &[Term::atom(Atom::OK), Term::small_int(1)]) {
            Some(term) => term,
            None => Term::NIL,
        };
        let mut args_heap = [0_u64; 2];
        let args = match write_cons(&mut args_heap, Term::atom(Atom::BADARG), Term::NIL) {
            Some(term) => term,
            None => Term::NIL,
        };

        assert_eq!(
            ExecError::Badfun { term: fun }.format_with_atoms(&table),
            "bad function term {ok, 1}"
        );
        assert_eq!(
            ExecError::Badarity {
                fun,
                args: vec![args],
            }
            .format_with_atoms(&table),
            "bad arity for function {ok, 1} with args [[badarg]]"
        );
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
