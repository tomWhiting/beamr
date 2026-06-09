use std::fs;
use std::path::Path;

use gleam_types::GleamTypes;
pub use gleam_types::{FunctionSignature, TypeDescriptor, TypeError};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GleamTypeReader {
    sidecar: GleamTypes,
}

impl GleamTypeReader {
    pub fn load(path: &Path) -> Result<Self, TypeError> {
        let bytes = fs::read(path).map_err(TypeError::from)?;
        let sidecar = GleamTypes::deserialize(&bytes)?;
        Ok(Self { sidecar })
    }

    pub fn module_name(&self) -> &str {
        &self.sidecar.module_name
    }

    pub fn function_signature(&self, function: &str, arity: u8) -> Option<FunctionSignature> {
        self.sidecar
            .functions
            .iter()
            .find(|signature| signature.name == function && signature.arity == arity)
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{GleamTypeReader, TypeDescriptor, TypeError};
    use gleam_types::GleamTypes;

    #[test]
    fn loads_and_queries_function_signature() {
        let path = temp_path("load_query");
        let mut types = GleamTypes::new("math");
        types.add_function(
            "add",
            2,
            vec![TypeDescriptor::Int, TypeDescriptor::Int],
            TypeDescriptor::Int,
        );
        fs::write(&path, types.serialize()).expect("write sidecar");

        let reader = GleamTypeReader::load(&path).expect("load sidecar");
        let signature = reader
            .function_signature("add", 2)
            .expect("signature should exist");

        assert_eq!(
            signature.param_types,
            vec![TypeDescriptor::Int, TypeDescriptor::Int]
        );
        assert_eq!(signature.return_type, TypeDescriptor::Int);
        assert!(reader.function_signature("add", 1).is_none());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn missing_file_returns_not_found() {
        let path = temp_path("missing");
        let _ = fs::remove_file(&path);
        let error = GleamTypeReader::load(&path).expect_err("missing file should fail");
        assert!(matches!(error, TypeError::NotFound));
    }

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "beamr_gleam_types_{name}_{}_{}.gleam_types",
            std::process::id(),
            unique_suffix()
        ))
    }

    fn unique_suffix() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time after epoch")
            .as_nanos()
    }
}
