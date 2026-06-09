use std::fmt;

use crate::format::{FunctionSignature, TypeDescriptor};

#[derive(Clone, Debug)]
pub struct ParsedModule {
    functions: Vec<ParsedFunction>,
}

#[derive(Clone, Debug)]
pub struct TypedModule {
    functions: Vec<FunctionSignature>,
}

#[derive(Clone, Debug)]
struct ParsedFunction {
    name: String,
    arguments: Vec<TypeDescriptor>,
    return_type: TypeDescriptor,
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
        parse_public_functions(source).map(|functions| ParsedModule { functions })
    }

    pub fn type_check(parsed: ParsedModule) -> Result<TypedModule, ExtractError> {
        let functions = parsed
            .functions
            .into_iter()
            .map(|function| {
                let arity = u8::try_from(function.arguments.len()).map_err(|_| {
                    ExtractError::ArityTooLarge {
                        function: function.name.clone(),
                        arity: function.arguments.len(),
                    }
                })?;
                Ok(FunctionSignature {
                    name: function.name,
                    arity,
                    param_types: function.arguments,
                    return_type: function.return_type,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(TypedModule { functions })
    }

    pub fn extract_signatures(typed: TypedModule) -> Vec<FunctionSignature> {
        typed.functions
    }
}

fn parse_public_functions(source: &str) -> Result<Vec<ParsedFunction>, ExtractError> {
    let mut functions = Vec::new();
    for declaration in source.split("pub fn ").skip(1) {
        let (name, after_name) = parse_identifier(declaration)
            .ok_or_else(|| ExtractError::Parse("expected public function name".into()))?;
        let after_name = after_name.trim_start();
        let Some(after_open) = after_name.strip_prefix('(') else {
            return Err(ExtractError::Parse(format!(
                "expected argument list for function {name}"
            )));
        };
        let (argument_list, after_arguments) = split_balanced(after_open, '(', ')')
            .map_err(|message| ExtractError::Parse(format!("{name}: {message}")))?;
        let after_arguments = after_arguments.trim_start();
        let Some(after_arrow) = after_arguments.strip_prefix("->") else {
            return Err(ExtractError::MissingAnnotation {
                function: name.to_string(),
            });
        };
        let return_source = type_source_before_body(after_arrow).ok_or_else(|| {
            ExtractError::Parse(format!("expected function body for function {name}"))
        })?;
        let arguments = parse_arguments(argument_list, name)?;
        let return_type = parse_type(return_source)
            .map_err(|message| ExtractError::UnsupportedType(format!("{name}: {message}")))?;
        functions.push(ParsedFunction {
            name: name.to_string(),
            arguments,
            return_type,
        });
    }
    Ok(functions)
}

fn parse_identifier(input: &str) -> Option<(&str, &str)> {
    let input = input.trim_start();
    let end = input
        .char_indices()
        .find_map(|(index, character)| {
            (!(character == '_' || character.is_ascii_alphanumeric())).then_some(index)
        })
        .unwrap_or(input.len());
    (end > 0).then(|| input.split_at(end))
}

fn split_balanced(input: &str, open: char, close: char) -> Result<(&str, &str), String> {
    let mut depth = 1usize;
    for (index, character) in input.char_indices() {
        match character {
            value if value == open => depth = depth.saturating_add(1),
            value if value == close => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    let (balanced, rest) = input.split_at(index);
                    return Ok((balanced, &rest[character.len_utf8()..]));
                }
            }
            _ => {}
        }
    }
    Err("unterminated balanced type expression".into())
}

fn type_source_before_body(input: &str) -> Option<&str> {
    let mut depth = 0usize;
    for (index, character) in input.char_indices() {
        match character {
            '(' | '<' => depth = depth.saturating_add(1),
            ')' | '>' => depth = depth.saturating_sub(1),
            '{' if depth == 0 => return Some(input[..index].trim()),
            _ => {}
        }
    }
    None
}

fn parse_arguments(
    argument_list: &str,
    function: &str,
) -> Result<Vec<TypeDescriptor>, ExtractError> {
    split_top_level(argument_list, ',')
        .into_iter()
        .filter(|argument| !argument.trim().is_empty())
        .map(|argument| {
            let (_, type_source) =
                argument
                    .split_once(':')
                    .ok_or_else(|| ExtractError::MissingAnnotation {
                        function: function.to_string(),
                    })?;
            parse_type(type_source)
                .map_err(|message| ExtractError::UnsupportedType(format!("{function}: {message}")))
        })
        .collect()
}

fn split_top_level(input: &str, delimiter: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut depth = 0usize;
    for (index, character) in input.char_indices() {
        match character {
            '(' | '<' => depth = depth.saturating_add(1),
            ')' | '>' => depth = depth.saturating_sub(1),
            value if value == delimiter && depth == 0 => {
                parts.push(input[start..index].trim());
                start = index + character.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(input[start..].trim());
    parts
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

fn parse_type(input: &str) -> Result<TypeDescriptor, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("empty type annotation".into());
    }
    if input == "_" {
        return Err("type holes are not supported in sidecars".into());
    }

    if let Some((arguments, return_type)) = split_function_type(input)? {
        let arguments = if arguments.trim().is_empty() {
            Vec::new()
        } else {
            split_top_level(arguments, ',')
                .into_iter()
                .map(parse_type)
                .collect::<Result<Vec<_>, _>>()?
        };
        return Ok(TypeDescriptor::Fn(
            arguments,
            Box::new(parse_type(return_type)?),
        ));
    }

    if input.starts_with('#') && input.ends_with(')') {
        let Some(elements) = input
            .strip_prefix("#(")
            .and_then(|value| value.strip_suffix(')'))
        else {
            return Err("malformed tuple type".into());
        };
        return split_top_level(elements, ',')
            .into_iter()
            .filter(|element| !element.trim().is_empty())
            .map(parse_type)
            .collect::<Result<Vec<_>, _>>()
            .map(TypeDescriptor::Tuple);
    }

    let (name, arguments) = if let Some(open_index) = top_level_generic_open(input) {
        let name = input[..open_index].trim();
        let Some(generic_source) = input
            .get(open_index + 1..)
            .and_then(|value| value.strip_suffix(')'))
            .or_else(|| {
                input
                    .get(open_index + 1..)
                    .and_then(|value| value.strip_suffix('>'))
            })
        else {
            return Err(format!("malformed generic type {input}"));
        };
        let arguments = split_top_level(generic_source, ',')
            .into_iter()
            .map(parse_type)
            .collect::<Result<Vec<_>, _>>()?;
        (name, arguments)
    } else {
        (input, Vec::new())
    };

    let (module, name) = match name.rsplit_once('.') {
        Some((module, name)) => (module.replace('.', "/"), name.to_string()),
        None => (String::new(), name.to_string()),
    };
    Ok(named_type_to_descriptor(module, name, arguments))
}

fn split_function_type(input: &str) -> Result<Option<(&str, &str)>, String> {
    let Some(after_fn) = input.strip_prefix("fn(") else {
        return Ok(None);
    };
    let (arguments, rest) = split_balanced(after_fn, '(', ')')?;
    let Some(return_type) = rest.trim_start().strip_prefix("->") else {
        return Err("function type missing return arrow".into());
    };
    Ok(Some((arguments, return_type)))
}

fn top_level_generic_open(input: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (index, character) in input.char_indices() {
        match character {
            '(' | '<' if depth == 0 => return Some(index),
            '(' | '<' => depth = depth.saturating_add(1),
            ')' | '>' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    None
}

fn named_type_to_descriptor(
    module: String,
    name: String,
    arguments: Vec<TypeDescriptor>,
) -> TypeDescriptor {
    match (module.as_str(), name.as_str(), arguments.as_slice()) {
        ("", "Int", []) => TypeDescriptor::Int,
        ("", "Float", []) => TypeDescriptor::Float,
        ("", "String", []) => TypeDescriptor::String,
        ("", "BitArray", []) => TypeDescriptor::BitArray,
        ("", "Bool", []) => TypeDescriptor::Bool,
        ("", "Nil", []) => TypeDescriptor::Nil,
        ("", "List", [inner]) => TypeDescriptor::List(Box::new(inner.clone())),
        ("", "Result", [ok, error]) => {
            TypeDescriptor::Result(Box::new(ok.clone()), Box::new(error.clone()))
        }
        _ => TypeDescriptor::CustomType {
            module,
            name,
            type_params: arguments,
        },
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
