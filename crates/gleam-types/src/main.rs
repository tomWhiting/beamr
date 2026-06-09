use std::env;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use gleam_types::{GleamTypeExtractor, GleamTypes};

fn main() {
    if let Err(error) = run(env::args_os()) {
        let mut stderr = io::stderr().lock();
        let _ = writeln!(stderr, "gleam-types: {error}");
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
            skipped = skipped.saturating_add(1);
            continue;
        };
        let source = fs::read_to_string(&source_path).map_err(|error| CliError::ReadSource {
            path: source_path.clone(),
            source: error,
        })?;
        let parsed =
            GleamTypeExtractor::parse_module(&source).map_err(|error| CliError::ExtractModule {
                module: module_name.clone(),
                source: error,
            })?;
        let typed =
            GleamTypeExtractor::type_check(parsed).map_err(|error| CliError::ExtractModule {
                module: module_name.clone(),
                source: error,
            })?;
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
        let sidecar_path = beam_path.with_extension("gleam_types");
        fs::write(&sidecar_path, sidecar.serialize()).map_err(|error| CliError::WriteSidecar {
            path: sidecar_path,
            source: error,
        })?;
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
    ReadSource {
        path: PathBuf,
        source: io::Error,
    },
    WriteSidecar {
        path: PathBuf,
        source: io::Error,
    },
    ExtractModule {
        module: String,
        source: gleam_types::ExtractError,
    },
    InvalidPath(PathBuf),
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(message) => f.write_str(message),
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::ReadSource { path, source } => {
                write!(
                    f,
                    "failed to read Gleam source {}: {source}",
                    path.display()
                )
            }
            Self::WriteSidecar { path, source } => {
                write!(
                    f,
                    "failed to write type sidecar {}: {source}",
                    path.display()
                )
            }
            Self::ExtractModule { module, source } => {
                write!(f, "failed to extract types for module {module}: {source}")
            }
            Self::InvalidPath(path) => write!(f, "invalid source path {}", path.display()),
        }
    }
}

impl std::error::Error for CliError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::ReadSource { source, .. } | Self::WriteSidecar { source, .. } => Some(source),
            Self::ExtractModule { source, .. } => Some(source),
            Self::Usage(_) | Self::InvalidPath(_) => None,
        }
    }
}

impl From<io::Error> for CliError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
