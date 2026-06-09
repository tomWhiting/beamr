//! Cranelift-backed BEAM JIT compiler scaffold.

use crate::loader::decode::chunks::LambdaEntry;
use cranelift_jit::JITModule;
use std::error::Error;
use std::fmt;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

mod dispatch;
mod dispatch_call;
mod dispatch_core;
mod dispatch_data;
mod dispatch_helpers;
mod ir_helpers;
mod ir_typed;

/// Module-level metadata needed by closure-aware JIT lowering.
#[derive(Clone, Copy)]
pub struct ModuleCompileMetadata<'a> {
    /// Decoded lambda table for make_fun closure metadata.
    pub lambdas: &'a [LambdaEntry],
    /// Registry/module generation written into created closures.
    pub generation: u64,
}

impl<'a> ModuleCompileMetadata<'a> {
    const EMPTY: Self = Self {
        lambdas: &[],
        generation: 0,
    };
}

/// Error returned when scaffold JIT compilation cannot produce native code.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum JitError {
    /// The scaffold compiler has no translator for this opcode yet.
    UnsupportedOpcode { opcode: String },
    /// An opcode is supported in principle but has an operand shape this JIT ABI cannot lower yet.
    UnsupportedOperand { operand: String },
    /// A branch target references a label that is not present in the compiled instruction slice.
    UnknownLabel { label: u32 },
    /// Cranelift failed while declaring, defining, or finalizing code.
    CraneliftError(String),
    /// No BEAM instructions were provided.
    EmptyFunction,
}

impl fmt::Display for JitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedOpcode { opcode } => {
                write!(formatter, "unsupported JIT opcode: {opcode}")
            }
            Self::UnsupportedOperand { operand } => {
                write!(formatter, "unsupported JIT operand: {operand}")
            }
            Self::UnknownLabel { label } => write!(formatter, "unknown JIT label: {label}"),
            Self::CraneliftError(error) => write!(formatter, "Cranelift JIT error: {error}"),
            Self::EmptyFunction => {
                write!(formatter, "cannot JIT compile an empty instruction slice")
            }
        }
    }
}

impl Error for JitError {}

/// Required host Cranelift settings for the Beamr JIT scaffold.
#[derive(Clone, Debug, Default)]
pub struct JitSettings;

/// Compiler that owns Cranelift JIT code memory for emitted functions.
pub struct JitCompiler {
    module: Arc<Mutex<JITModule>>,
    next_function_id: AtomicU64,
}

#[cfg(test)]
mod compiler_tests;
