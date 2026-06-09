use std::fmt;

use camino::Utf8PathBuf;
use gleam_core::ast::{Definition, Publicity, TypeAst, TypeAstConstructorName, UntypedModule};
use gleam_core::parse;
use gleam_core::warning::WarningEmitter;

use crate::format::{FunctionSignature, TypeDescriptor};

#[derive(Clone, Debug)]
pub struct ParsedModule {
    module: UntypedModule,
}

#[derive(Clone, Debug)]
pub struct TypedModule {
    module: UntypedModule,
}

#[derive(Clone, Debug, Default)]
pub struct GleamTypeExtractor;

#[derive(Debug)]
pub enum ExtractError {
    Parse(String),
    TypeCheck(String),
    MissingAnnotation { function: String },
    UnsupportedType(String),
    ArityTooLarge { function: String, arity: usize },
}

impl GleamTypeExtractor {
    pub fn new() -> Self {
        Self
    }

    pub fn parse_module(source: &str) -> Result<ParsedModule, ExtractError> {
        let path = Utf8PathBuf::from("beamr_sidecar.gleam");
        let warnings = WarningEmitter::null();
        let parsed = parse::parse_module(path, source, &warnings)
            .map_err(|error| ExtractError::Parse(format!("{error:?}")))?;
        Ok(ParsedModule {
            module: parsed.module,
        })
    }

    pub fn type_check(parsed: ParsedModule) -> Result<TypedModule, ExtractError> {
        for targeted in &parsed.module.definitions {
            if let Definition::Function(function) = &targeted.definition {
                let Some((_, name)) = function.name.as_ref() else {
                    continue;
                };
                if matches!(function.publicity, Publicity::Private) {
                    continue;
                }
                let has_all_argument_annotations = function
                    .arguments
                    .iter()
                    .all(|argument| argument.annotation.is_some());
                if !has_all_argument_annotations || function.return_annotation.is_none() {
                    return Err(ExtractError::TypeCheck(format!(
                        "function {name} is missing explicit type annotations"
                    )));
                }
            }
        }
        Ok(TypedModule {
            module: parsed.module,
        })
    }

    pub fn extract_signatures(typed: TypedModule) -> Vec<FunctionSignature> {
        typed
            .module
            .definitions
            .iter()
            .filter_map(|targeted| {
                let Definition::Function(function) = &targeted.definition else {
                    return None;
                };
                if matches!(function.publicity, Publicity::Private) {
                    return None;
                }
                let name = function.name.as_ref()?.1.to_string();
                let arity = u8::try_from(function.arguments.len()).ok()?;
                let param_types = function
                    .arguments
                    .iter()
                    .map(|argument| {
                        argument
                            .annotation
                            .as_ref()
                            .and_then(type_ast_to_descriptor)
                    })
                    .collect::<Option<Vec<_>>>()?;
                let return_type = function
                    .return_annotation
                    .as_ref()
                    .and_then(type_ast_to_descriptor)?;
                Some(FunctionSignature {
                    name,
                    arity,
                    param_types,
                    return_type,
                })
            })
            .collect()
    }
}

impl fmt::Display for ExtractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(message) => write!(f, "failed to parse Gleam module: {message}"),
            Self::TypeCheck(message) => write!(f, "failed to type check Gleam module: {message}"),
            Self::MissingAnnotation { function } => {
                write!(f, "function {function} is missing type annotations")
            }
            Self::UnsupportedType(message) => write!(f, "unsupported Gleam type: {message}"),
            Self::ArityTooLarge { function, arity } => {
                write!(f, "function {function}/{arity} arity exceeds sidecar limit")
            }
        }
    }
}

impl std::error::Error for ExtractError {}

fn type_ast_to_descriptor(type_ast: &TypeAst) -> Option<TypeDescriptor> {
    match type_ast {
        TypeAst::Constructor(constructor) => {
            let (module, name) = constructor_name(&constructor.name)?;
            let arguments = constructor
                .arguments
                .iter()
                .map(type_ast_to_descriptor)
                .collect::<Option<Vec<_>>>()?;
            named_type_to_descriptor(module, name, arguments)
        }
        TypeAst::Fn(function) => {
            let arguments = function
                .arguments
                .iter()
                .map(type_ast_to_descriptor)
                .collect::<Option<Vec<_>>>()?;
            let return_ = type_ast_to_descriptor(&function.return_)?;
            Some(TypeDescriptor::Fn(arguments, Box::new(return_)))
        }
        TypeAst::Tuple(tuple) => tuple
            .elements
            .iter()
            .map(type_ast_to_descriptor)
            .collect::<Option<Vec<_>>>()
            .map(TypeDescriptor::Tuple),
        TypeAst::Var(var) => Some(TypeDescriptor::CustomType {
            module: String::new(),
            name: var.name.to_string(),
            type_params: Vec::new(),
        }),
        TypeAst::Hole(_) => None,
    }
}

fn constructor_name(name: &TypeAstConstructorName) -> Option<(String, String)> {
    match name {
        TypeAstConstructorName::Unqualified { name, .. } => Some((String::new(), name.to_string())),
        TypeAstConstructorName::Qualified { module, name, .. } => {
            let (name, _) = name.as_ref()?;
            Some((module.to_string(), name.to_string()))
        }
    }
}

fn named_type_to_descriptor(
    module: String,
    name: String,
    arguments: Vec<TypeDescriptor>,
) -> Option<TypeDescriptor> {
    match (module.as_str(), name.as_str(), arguments.as_slice()) {
        ("", "Int", []) => Some(TypeDescriptor::Int),
        ("", "Float", []) => Some(TypeDescriptor::Float),
        ("", "String", []) => Some(TypeDescriptor::String),
        ("", "BitArray", []) => Some(TypeDescriptor::BitArray),
        ("", "Bool", []) => Some(TypeDescriptor::Bool),
        ("", "Nil", []) => Some(TypeDescriptor::Nil),
        ("", "List", [inner]) => Some(TypeDescriptor::List(Box::new(inner.clone()))),
        ("", "Result", [ok, error]) => Some(TypeDescriptor::Result(
            Box::new(ok.clone()),
            Box::new(error.clone()),
        )),
        _ => Some(TypeDescriptor::CustomType {
            module,
            name,
            type_params: arguments,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::GleamTypeExtractor;
    use crate::format::TypeDescriptor;

    #[test]
    fn extracts_annotated_add_signature() {
        let source = "pub fn add(a: Int, b: Int) -> Int { a + b }";
        let parsed = GleamTypeExtractor::parse_module(source).expect("parse module");
        let typed = GleamTypeExtractor::type_check(parsed).expect("type check module");
        let signatures = GleamTypeExtractor::extract_signatures(typed);

        assert_eq!(signatures.len(), 1);
        assert_eq!(signatures[0].name, "add");
        assert_eq!(signatures[0].arity, 2);
        assert_eq!(
            signatures[0].param_types,
            vec![TypeDescriptor::Int, TypeDescriptor::Int]
        );
        assert_eq!(signatures[0].return_type, TypeDescriptor::Int);
    }
}
