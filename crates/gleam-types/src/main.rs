use std::env;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use gleam_types::{GleamTypeExtractor, GleamTypes};

fn main() {
    if let Err(error) = run(env::args_os()) {
        eprintln!("gleam-types: {error}");
        std::process::exit(1);
    }
}

fn run(args: impl IntoIterator<Item = OsString>) -> Result<(), CliError> {
    let mut args = args.into_iter();
    let _program = args.next();
    let Some(project_dir) = args.next() else {
        return Err(CliError::Usage("usage: gleam-types <project-dir>".into()));
    };
    if args.next().is_some() {
        return Err(CliError::Usage("usage: gleam-types <project-dir>".into()));
    }

    let project_dir = PathBuf::from(project_dir);
    let src_dir = project_dir.join("src");
    let build_dir = project_dir.join("build");
    let sources = collect_files_with_extension(&src_dir, "gleam")?;
    let beams = collect_files_with_extension(&build_dir, "beam")?;

    let mut extracted = 0usize;
    let mut skipped = 0usize;
    for source_path in sources {
        let module_name = module_name(&src_dir, &source_path)?;
        let Some(beam_path) = find_beam(&beams, &module_name) else {
            eprintln!("warning: no .beam found for Gleam module {module_name}; skipping");
            skipped = skipped.saturating_add(1);
            continue;
        };
        let source = fs::read_to_string(&source_path)?;
        let parsed = GleamTypeExtractor::parse_module(&source)?;
        let typed = GleamTypeExtractor::type_check(parsed)?;
        let signatures = GleamTypeExtractor::extract_signatures(typed);
        let mut sidecar = GleamTypes::new(module_name);
        for signature in signatures {
            sidecar.add_function(
                signature.name,
                signature.arity,
                signature.param_types,
                signature.return_type,
            );
        }
        fs::write(beam_path.with_extension("gleam_types"), sidecar.serialize())?;
        extracted = extracted.saturating_add(1);
    }

    println!("Extracted types for {extracted} modules ({skipped} skipped)");
    Ok(())
}

fn collect_files_with_extension(dir: &Path, extension: &str) -> Result<Vec<PathBuf>, CliError> {
    let mut files = Vec::new();
    if !dir.exists() {
        return Ok(files);
    }
    collect_files_with_extension_into(dir, extension, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_files_with_extension_into(
    dir: &Path,
    extension: &str,
    files: &mut Vec<PathBuf>,
) -> Result<(), CliError> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_files_with_extension_into(&path, extension, files)?;
        } else if path.extension().and_then(|value| value.to_str()) == Some(extension) {
            files.push(path);
        }
    }
    Ok(())
}

fn module_name(src_dir: &Path, source_path: &Path) -> Result<String, CliError> {
    let relative = source_path
        .strip_prefix(src_dir)
        .map_err(|_| CliError::InvalidPath(source_path.to_path_buf()))?;
    let without_extension = relative.with_extension("");
    let components = without_extension
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    Ok(components.join("/"))
}

fn find_beam<'a>(beams: &'a [PathBuf], module_name: &str) -> Option<&'a Path> {
    let expected_suffix = module_name.replace('/', "_");
    beams.iter().find_map(|path| {
        let stem = path.file_stem()?.to_str()?;
        if stem == module_name || stem == expected_suffix {
            Some(path.as_path())
        } else {
            None
        }
    })
}

#[derive(Debug)]
enum CliError {
    Usage(String),
    Io(io::Error),
    Extract(gleam_types::ExtractError),
    InvalidPath(PathBuf),
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(message) => f.write_str(message),
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::Extract(error) => write!(f, "{error}"),
            Self::InvalidPath(path) => write!(f, "invalid source path {}", path.display()),
        }
    }
}

impl std::error::Error for CliError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Extract(error) => Some(error),
            Self::Usage(_) | Self::InvalidPath(_) => None,
        }
    }
}

impl From<io::Error> for CliError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<gleam_types::ExtractError> for CliError {
    fn from(error: gleam_types::ExtractError) -> Self {
        Self::Extract(error)
    }
}
