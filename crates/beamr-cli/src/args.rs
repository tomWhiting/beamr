//! Command-line argument parsing for the beamr CLI.

use std::path::PathBuf;

use super::errors::CliError;
use super::{Command, EntryPoint, USAGE};

pub fn parse_args<I, S>(args: I) -> Result<Command, CliError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let args: Vec<String> = args.into_iter().map(Into::into).collect();
    let (command_args, runtime_args) = match args.iter().position(|arg| arg == "--") {
        Some(separator) => (
            args[..separator].to_vec(),
            args[separator.saturating_add(1)..].to_vec(),
        ),
        None => (args, Vec::new()),
    };

    // Extract --dir and --log values first, collecting into command metadata and
    // building a filtered list of remaining args.
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut log: Option<PathBuf> = None;
    let mut filtered_args: Vec<String> = Vec::new();
    let mut iter = command_args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--dir" {
            let value = iter.next().ok_or_else(|| {
                CliError::MissingDirValue("--dir requires a directory path".into())
            })?;
            dirs.push(PathBuf::from(value));
        } else if arg == "--log" {
            let value = iter
                .next()
                .ok_or_else(|| CliError::MissingLogValue("--log requires an output path".into()))?;
            log = Some(PathBuf::from(value));
        } else {
            filtered_args.push(arg.clone());
        }
    }

    if log.is_some()
        && filtered_args
            .first()
            .is_none_or(|command| command != "record")
    {
        return Err(CliError::Usage(format!(
            "--log is only supported with record\n{USAGE}"
        )));
    }

    for (index, arg) in filtered_args.iter().enumerate() {
        if arg.starts_with('-') {
            match arg.as_str() {
                "--help" | "-h" if filtered_args.len() == 1 && runtime_args.is_empty() => {
                    return Ok(Command::Help);
                }
                "--version" | "-V" if filtered_args.len() == 1 && runtime_args.is_empty() => {
                    return Ok(Command::Version);
                }
                "--entry"
                    if index == 1
                        || filtered_args
                            .first()
                            .is_some_and(|command| command == "record") =>
                {
                    continue;
                }
                "--verbose"
                    if filtered_args
                        .first()
                        .is_some_and(|command| command == "compile") =>
                {
                    continue;
                }
                "--help" | "-h" | "--version" | "-V" => {
                    return Err(CliError::Usage(format!(
                        "flag '{arg}' cannot be combined with other arguments\n{USAGE}"
                    )));
                }
                _ => return Err(CliError::UnknownFlag(arg.clone())),
            }
        }
    }

    match filtered_args.as_slice() {
        [] => Err(CliError::Usage(format!("missing .beam file\n{USAGE}"))),
        [command] if command == "imports" => Err(CliError::Usage(format!(
            "imports requires a .beam file\n{USAGE}"
        ))),
        [command] if command == "compile" => Err(CliError::Usage(format!(
            "compile requires a directory\n{USAGE}"
        ))),
        [command] if command == "replay" => Err(CliError::Usage(format!(
            "replay requires a log file\n{USAGE}"
        ))),
        [command] if command == "record" => Err(CliError::Usage(format!(
            "record requires a .beam file\n{USAGE}"
        ))),
        [file] => {
            validate_beam_path(file)?;
            Ok(Command::Run {
                path: PathBuf::from(file),
                entry: None,
                args: runtime_args,
                dirs,
            })
        }
        [command, file] if command == "imports" => {
            validate_beam_path(file)?;
            Ok(Command::Imports {
                path: PathBuf::from(file),
            })
        }
        [command, log_file] if command == "replay" => Ok(Command::Replay {
            log: PathBuf::from(log_file),
        }),
        [command, dir] if command == "compile" => Ok(Command::Compile {
            dir: PathBuf::from(dir),
            verbose: false,
        }),
        [command, dir, flag] if command == "compile" && flag == "--verbose" => {
            Ok(Command::Compile {
                dir: PathBuf::from(dir),
                verbose: true,
            })
        }
        [command, flag, dir] if command == "compile" && flag == "--verbose" => {
            Ok(Command::Compile {
                dir: PathBuf::from(dir),
                verbose: true,
            })
        }
        [file, entry] => {
            validate_beam_path(file)?;
            Ok(Command::Run {
                path: PathBuf::from(file),
                entry: Some(parse_entry(entry)?),
                args: runtime_args,
                dirs,
            })
        }
        [file, flag, entry] if flag == "--entry" => {
            validate_beam_path(file)?;
            Ok(Command::Run {
                path: PathBuf::from(file),
                entry: Some(parse_entry(entry)?),
                args: runtime_args,
                dirs,
            })
        }
        [command, file, flag, entry] if command == "record" && flag == "--entry" => {
            validate_beam_path(file)?;
            let log = log.ok_or_else(|| {
                CliError::Usage(format!("record requires --log <output>\n{USAGE}"))
            })?;
            Ok(Command::Record {
                path: PathBuf::from(file),
                entry: parse_entry(entry)?,
                log,
                args: runtime_args,
                dirs,
            })
        }
        _ => Err(CliError::Usage(format!("too many arguments\n{USAGE}"))),
    }
}

fn validate_beam_path(path: &str) -> Result<(), CliError> {
    if path.ends_with(".beam") {
        Ok(())
    } else {
        Err(CliError::InvalidBeamPath(PathBuf::from(path)))
    }
}

pub fn parse_entry(entry: &str) -> Result<EntryPoint, CliError> {
    let invalid_entry = || CliError::InvalidEntry(entry.to_owned());
    let (module, function_and_arity) = entry.split_once(':').ok_or_else(invalid_entry)?;
    let (function, arity) = function_and_arity
        .split_once('/')
        .ok_or_else(invalid_entry)?;

    if module.is_empty()
        || function.is_empty()
        || arity.is_empty()
        || module.contains('/')
        || function_and_arity.contains(':')
        || arity.contains('/')
        || !arity.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(CliError::InvalidEntry(entry.to_owned()));
    }

    let arity = arity
        .parse::<u8>()
        .map_err(|_| CliError::InvalidEntry(entry.to_owned()))?;

    Ok(EntryPoint {
        module: module.to_owned(),
        function: function.to_owned(),
        arity,
    })
}
