use std::fmt;
use std::io;

pub const MAGIC: &[u8; 12] = b"GLEAM_TYPES\0";
pub const FORMAT_VERSION: u8 = 1;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GleamTypes {
    pub module_name: String,
    pub functions: Vec<FunctionSignature>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FunctionSignature {
    pub name: String,
    pub arity: u8,
    pub param_types: Vec<TypeDescriptor>,
    pub return_type: TypeDescriptor,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TypeDescriptor {
    Int,
    Float,
    String,
    BitArray,
    Bool,
    Atom,
    Nil,
    List(Box<TypeDescriptor>),
    Tuple(Vec<TypeDescriptor>),
    Result(Box<TypeDescriptor>, Box<TypeDescriptor>),
    CustomType {
        module: String,
        name: String,
        type_params: Vec<TypeDescriptor>,
    },
    Fn(Vec<TypeDescriptor>, Box<TypeDescriptor>),
}

#[derive(Debug)]
pub enum TypeError {
    Io(io::Error),
    NotFound,
    InvalidMagic,
    UnsupportedVersion(u8),
    Malformed(String),
}

impl GleamTypes {
    pub fn new(module_name: impl Into<String>) -> Self {
        Self {
            module_name: module_name.into(),
            functions: Vec::new(),
        }
    }

    pub fn add_function(
        &mut self,
        name: impl Into<String>,
        arity: u8,
        param_types: Vec<TypeDescriptor>,
        return_type: TypeDescriptor,
    ) {
        self.functions.push(FunctionSignature {
            name: name.into(),
            arity,
            param_types,
            return_type,
        });
    }

    pub fn serialize(&self) -> Vec<u8> {
        let mut output = Vec::new();
        output.extend_from_slice(MAGIC);
        write_u8(&mut output, FORMAT_VERSION);
        write_string(&mut output, &self.module_name);
        write_u32(&mut output, self.functions.len() as u32);
        for function in &self.functions {
            write_string(&mut output, &function.name);
            write_u8(&mut output, function.arity);
            write_u32(&mut output, function.param_types.len() as u32);
            for type_ in &function.param_types {
                write_type(&mut output, type_);
            }
            write_type(&mut output, &function.return_type);
        }
        output
    }

    pub fn deserialize(bytes: &[u8]) -> Result<Self, TypeError> {
        let mut reader = Reader::new(bytes);
        let magic = reader.read_exact(MAGIC.len())?;
        if magic != MAGIC {
            return Err(TypeError::InvalidMagic);
        }
        let version = reader.read_u8()?;
        if version != FORMAT_VERSION {
            return Err(TypeError::UnsupportedVersion(version));
        }
        let module_name = reader.read_string()?;
        let function_count = reader.read_u32()? as usize;
        let mut functions = Vec::with_capacity(function_count);
        for _ in 0..function_count {
            let name = reader.read_string()?;
            let arity = reader.read_u8()?;
            let param_count = reader.read_u32()? as usize;
            if usize::from(arity) != param_count {
                return Err(TypeError::Malformed(format!(
                    "function {name}/{arity} has {param_count} parameter types"
                )));
            }
            let mut param_types = Vec::with_capacity(param_count);
            for _ in 0..param_count {
                param_types.push(reader.read_type()?);
            }
            let return_type = reader.read_type()?;
            functions.push(FunctionSignature {
                name,
                arity,
                param_types,
                return_type,
            });
        }
        if !reader.is_empty() {
            return Err(TypeError::Malformed(
                "trailing bytes in type sidecar".into(),
            ));
        }
        Ok(Self {
            module_name,
            functions,
        })
    }
}

impl fmt::Display for TypeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "type sidecar I/O error: {error}"),
            Self::NotFound => f.write_str("type sidecar not found"),
            Self::InvalidMagic => f.write_str("invalid type sidecar magic"),
            Self::UnsupportedVersion(version) => {
                write!(f, "unsupported type sidecar version {version}")
            }
            Self::Malformed(message) => write!(f, "malformed type sidecar: {message}"),
        }
    }
}

impl std::error::Error for TypeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::NotFound
            | Self::InvalidMagic
            | Self::UnsupportedVersion(_)
            | Self::Malformed(_) => None,
        }
    }
}

impl From<io::Error> for TypeError {
    fn from(error: io::Error) -> Self {
        if error.kind() == io::ErrorKind::NotFound {
            Self::NotFound
        } else {
            Self::Io(error)
        }
    }
}

fn write_type(output: &mut Vec<u8>, type_: &TypeDescriptor) {
    match type_ {
        TypeDescriptor::Int => write_u8(output, 0),
        TypeDescriptor::Float => write_u8(output, 1),
        TypeDescriptor::String => write_u8(output, 2),
        TypeDescriptor::BitArray => write_u8(output, 3),
        TypeDescriptor::Bool => write_u8(output, 4),
        TypeDescriptor::Atom => write_u8(output, 5),
        TypeDescriptor::Nil => write_u8(output, 6),
        TypeDescriptor::List(inner) => {
            write_u8(output, 7);
            write_type(output, inner);
        }
        TypeDescriptor::Tuple(elements) => {
            write_u8(output, 8);
            write_u32(output, elements.len() as u32);
            for element in elements {
                write_type(output, element);
            }
        }
        TypeDescriptor::Result(ok, error) => {
            write_u8(output, 9);
            write_type(output, ok);
            write_type(output, error);
        }
        TypeDescriptor::CustomType {
            module,
            name,
            type_params,
        } => {
            write_u8(output, 10);
            write_string(output, module);
            write_string(output, name);
            write_u32(output, type_params.len() as u32);
            for param in type_params {
                write_type(output, param);
            }
        }
        TypeDescriptor::Fn(arguments, return_) => {
            write_u8(output, 11);
            write_u32(output, arguments.len() as u32);
            for argument in arguments {
                write_type(output, argument);
            }
            write_type(output, return_);
        }
    }
}

fn write_string(output: &mut Vec<u8>, value: &str) {
    write_u64(output, value.len() as u64);
    output.extend_from_slice(value.as_bytes());
}

fn write_u8(output: &mut Vec<u8>, value: u8) {
    output.push(value);
}

fn write_u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn write_u64(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_le_bytes());
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn is_empty(&self) -> bool {
        self.offset == self.bytes.len()
    }

    fn read_exact(&mut self, len: usize) -> Result<&'a [u8], TypeError> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or_else(|| TypeError::Malformed("sidecar offset overflow".into()))?;
        let slice = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| TypeError::Malformed("truncated type sidecar".into()))?;
        self.offset = end;
        Ok(slice)
    }

    fn read_u8(&mut self) -> Result<u8, TypeError> {
        self.read_exact(1).map(|bytes| bytes[0])
    }

    fn read_u32(&mut self) -> Result<u32, TypeError> {
        let bytes = self.read_exact(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_u64(&mut self) -> Result<u64, TypeError> {
        let bytes = self.read_exact(8)?;
        Ok(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    fn read_string(&mut self) -> Result<String, TypeError> {
        let len = usize::try_from(self.read_u64()?)
            .map_err(|_| TypeError::Malformed("string length overflows usize".into()))?;
        let bytes = self.read_exact(len)?;
        std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| TypeError::Malformed("string is not valid UTF-8".into()))
    }

    fn read_type(&mut self) -> Result<TypeDescriptor, TypeError> {
        match self.read_u8()? {
            0 => Ok(TypeDescriptor::Int),
            1 => Ok(TypeDescriptor::Float),
            2 => Ok(TypeDescriptor::String),
            3 => Ok(TypeDescriptor::BitArray),
            4 => Ok(TypeDescriptor::Bool),
            5 => Ok(TypeDescriptor::Atom),
            6 => Ok(TypeDescriptor::Nil),
            7 => Ok(TypeDescriptor::List(Box::new(self.read_type()?))),
            8 => self.read_type_vec().map(TypeDescriptor::Tuple),
            9 => Ok(TypeDescriptor::Result(
                Box::new(self.read_type()?),
                Box::new(self.read_type()?),
            )),
            10 => {
                let module = self.read_string()?;
                let name = self.read_string()?;
                let type_params = self.read_type_vec()?;
                Ok(TypeDescriptor::CustomType {
                    module,
                    name,
                    type_params,
                })
            }
            11 => {
                let arguments = self.read_type_vec()?;
                let return_ = Box::new(self.read_type()?);
                Ok(TypeDescriptor::Fn(arguments, return_))
            }
            tag => Err(TypeError::Malformed(format!("unknown type tag {tag}"))),
        }
    }

    fn read_type_vec(&mut self) -> Result<Vec<TypeDescriptor>, TypeError> {
        let count = self.read_u32()? as usize;
        let mut types = Vec::with_capacity(count);
        for _ in 0..count {
            types.push(self.read_type()?);
        }
        Ok(types)
    }
}

#[cfg(test)]
mod tests {
    use super::{GleamTypes, TypeDescriptor};

    #[test]
    fn round_trips_result_signature() {
        let mut types = GleamTypes::new("example");
        types.add_function(
            "parse",
            1,
            vec![TypeDescriptor::Int],
            TypeDescriptor::Result(
                Box::new(TypeDescriptor::String),
                Box::new(TypeDescriptor::Nil),
            ),
        );

        let decoded = GleamTypes::deserialize(&types.serialize()).expect("sidecar round trip");
        assert_eq!(decoded, types);
    }
}
