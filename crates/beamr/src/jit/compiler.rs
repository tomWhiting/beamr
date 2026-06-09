//! Cranelift-backed BEAM JIT compiler scaffold.

use crate::atom::Atom;
use crate::loader::Instruction;
use crate::scheduler::lock_or_recover;
use cranelift_codegen::ir::InstBuilder;
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module, default_libcall_names};
use std::error::Error;
use std::fmt;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use super::types::NativeCode;

/// Error returned when scaffold JIT compilation cannot produce native code.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum JitError {
    /// The scaffold compiler has no translator for this opcode yet.
    UnsupportedOpcode { opcode: String },
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
    module: Mutex<JITModule>,
    next_function_id: AtomicU64,
}

impl JitCompiler {
    /// Creates a compiler with Cranelift ISA settings for the host target.
    pub fn new(_settings: JitSettings) -> Result<Self, JitError> {
        let mut flag_builder = settings::builder();
        flag_builder
            .set("use_colocated_libcalls", "false")
            .map_err(|error| JitError::CraneliftError(error.to_string()))?;
        flag_builder
            .set("is_pic", "false")
            .map_err(|error| JitError::CraneliftError(error.to_string()))?;
        let isa_builder = cranelift_native::builder()
            .map_err(|error| JitError::CraneliftError(error.to_owned()))?;
        let isa = isa_builder
            .finish(settings::Flags::new(flag_builder))
            .map_err(|error| JitError::CraneliftError(error.to_string()))?;
        let builder = JITBuilder::with_isa(isa, default_libcall_names());
        Ok(Self {
            module: Mutex::new(JITModule::new(builder)),
            next_function_id: AtomicU64::new(0),
        })
    }

    /// Compiles a BEAM instruction slice into callable native code.
    pub fn compile(
        &self,
        instructions: &[Instruction],
        module: Atom,
        function: Atom,
        arity: u8,
    ) -> Result<NativeCode, JitError> {
        validate_scaffold_instructions(instructions)?;

        let unique_id = self.next_function_id.fetch_add(1, Ordering::Relaxed);
        let name = format!("beamr_jit_{module:?}_{function:?}_{arity}_{unique_id}");

        let mut jit_module = lock_or_recover(&self.module);
        let mut ctx = jit_module.make_context();
        let mut signature = jit_module.make_signature();
        signature.returns.push(cranelift_codegen::ir::AbiParam::new(
            cranelift_codegen::ir::types::I64,
        ));
        ctx.func.signature = signature.clone();

        let mut builder_context = FunctionBuilderContext::new();
        {
            let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_context);
            let entry = builder.create_block();
            builder.switch_to_block(entry);
            builder.seal_block(entry);
            let zero = builder.ins().iconst(cranelift_codegen::ir::types::I64, 0);
            builder.ins().return_(&[zero]);
            builder.finalize();
        }

        let func_id = jit_module
            .declare_function(&name, Linkage::Local, &signature)
            .map_err(|error| JitError::CraneliftError(error.to_string()))?;
        jit_module
            .define_function(func_id, &mut ctx)
            .map_err(|error| JitError::CraneliftError(error.to_string()))?;
        jit_module.clear_context(&mut ctx);
        jit_module
            .finalize_definitions()
            .map_err(|error| JitError::CraneliftError(error.to_string()))?;
        let call_ptr = jit_module.get_finalized_function(func_id);
        Ok(NativeCode::new(call_ptr, Vec::new()))
    }
}

fn validate_scaffold_instructions(instructions: &[Instruction]) -> Result<(), JitError> {
    if instructions.is_empty() {
        return Err(JitError::EmptyFunction);
    }

    for instruction in instructions {
        match instruction {
            Instruction::Return => {}
            other => {
                return Err(JitError::UnsupportedOpcode {
                    opcode: opcode_name(other),
                });
            }
        }
    }

    Ok(())
}

fn opcode_name(instruction: &Instruction) -> String {
    match instruction {
        Instruction::Generic { opcode, name, .. } => format!("{name} ({opcode})"),
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{JitCompiler, JitError, JitSettings};
    use crate::atom::Atom;
    use crate::loader::Instruction;

    #[test]
    fn compiles_return_only_function() {
        let compiler = JitCompiler::new(JitSettings).unwrap();
        let native = compiler
            .compile(&[Instruction::Return], Atom::MODULE, Atom::OK, 0)
            .unwrap();

        assert!(!native.call_ptr().is_null());
        assert!(native.stack_maps().is_empty());
    }

    #[test]
    fn reports_unsupported_opcode() {
        let compiler = JitCompiler::new(JitSettings).unwrap();
        let error = compiler
            .compile(
                &[Instruction::Generic {
                    opcode: 255,
                    name: "unknown",
                    operands: Vec::new(),
                }],
                Atom::MODULE,
                Atom::OK,
                0,
            )
            .unwrap_err();

        assert_eq!(
            error,
            JitError::UnsupportedOpcode {
                opcode: "unknown (255)".to_owned()
            }
        );
    }
}
