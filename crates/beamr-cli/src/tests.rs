use super::args::{parse_args, parse_entry};
use super::{CliError, CliSuccess, Command, EntryPoint, run_cli};
use beamr::error::{ExecError, LoadError};
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn parses_help_flags() {
    assert_eq!(parse_args(["--help"]).expect("help parses"), Command::Help);
    assert_eq!(parse_args(["-h"]).expect("help parses"), Command::Help);
}

#[test]
fn parses_version_flags() {
    assert_eq!(
        parse_args(["--version"]).expect("version parses"),
        Command::Version
    );
    assert_eq!(
        parse_args(["-V"]).expect("version parses"),
        Command::Version
    );
}

#[test]
fn parses_beam_file_only_as_run_without_entry() {
    assert_eq!(
        parse_args(["hello.beam"]).expect("run parses"),
        Command::Run {
            path: "hello.beam".into(),
            entry: None,
            args: Vec::new(),
            dirs: Vec::new(),
        }
    );
}

#[test]
fn parses_beam_file_and_entry_as_run_with_entry() {
    assert_eq!(
        parse_args(["hello.beam", "hello:main/0"]).expect("run with entry parses"),
        Command::Run {
            path: "hello.beam".into(),
            entry: Some(EntryPoint {
                module: "hello".into(),
                function: "main".into(),
                arity: 0,
            }),
            args: Vec::new(),
            dirs: Vec::new(),
        }
    );
}

#[test]
fn parses_entry_flag_and_runtime_args() {
    assert_eq!(
        parse_args(["hello.beam", "--entry", "hello:add/2", "--", "17", "25"])
            .expect("run with --entry and args parses"),
        Command::Run {
            path: "hello.beam".into(),
            entry: Some(EntryPoint {
                module: "hello".into(),
                function: "add".into(),
                arity: 2,
            }),
            args: vec!["17".into(), "25".into()],
            dirs: Vec::new(),
        }
    );
}

#[test]
fn parses_imports_command() {
    assert_eq!(
        parse_args(["imports", "hello.beam"]).expect("imports parses"),
        Command::Imports {
            path: "hello.beam".into(),
        }
    );
}

#[test]
fn parses_compile_command_with_verbose() {
    assert_eq!(
        parse_args(["compile", "/tmp/beams", "--verbose"]).expect("compile verbose parses"),
        Command::Compile {
            dir: "/tmp/beams".into(),
            verbose: true,
        }
    );
    assert_eq!(
        parse_args(["compile", "/tmp/beams"]).expect("compile parses"),
        Command::Compile {
            dir: "/tmp/beams".into(),
            verbose: false,
        }
    );
}

#[test]
fn rejects_non_beam_path() {
    let error = parse_args(["hello.txt"]).expect_err("non-beam path should fail");

    assert!(matches!(error, CliError::InvalidBeamPath(_)));
    assert!(error.to_string().contains(".beam"));
}

#[test]
fn validates_entry_format() {
    assert_eq!(
        parse_entry("hello:main/255").expect("valid entry parses"),
        EntryPoint {
            module: "hello".into(),
            function: "main".into(),
            arity: 255,
        }
    );

    for invalid in [
        "bad-entry",
        ":main/0",
        "hello:/0",
        "hel/lo:main/0",
        "hello:main/+1",
        "hello:main/",
        "hello:main/256",
        "hello:main/not-a-number",
        "hello:main/0/1",
        "hello:main:again/0",
    ] {
        assert!(
            matches!(parse_entry(invalid), Err(CliError::InvalidEntry(_))),
            "{invalid} should be rejected"
        );
    }
}

#[test]
fn rejects_invalid_run_entry() {
    let error = parse_args(["hello.beam", "bad-entry"]).expect_err("invalid entry should fail");

    assert!(matches!(error, CliError::InvalidEntry(_)));
    assert!(error.to_string().contains("invalid entry point"));
}

#[test]
fn rejects_unknown_flag() {
    let error = parse_args(["--unknown"]).expect_err("unknown flag should fail");

    assert!(matches!(&error, CliError::UnknownFlag(flag) if flag == "--unknown"));
    assert!(error.to_string().contains("--unknown"));
}

#[test]
fn rejects_unknown_flag_after_imports_as_flag() {
    let error = parse_args(["imports", "--unknown"])
        .expect_err("unknown flag should be detected before path validation");

    assert!(matches!(&error, CliError::UnknownFlag(flag) if flag == "--unknown"));
}

#[test]
fn error_display_formats_io_load_and_exec_errors() {
    let io_error = CliError::Io {
        path: "missing.beam".into(),
        source: std::io::Error::new(std::io::ErrorKind::NotFound, "No such file or directory"),
    };
    assert_eq!(
        io_error.to_string(),
        "cannot read 'missing.beam': No such file or directory"
    );
    assert_eq!(io_error.exit_code(), 2);

    let load_error = CliError::Load(LoadError::InvalidFormat);
    assert_eq!(load_error.to_string(), "load: invalid BEAM file format");
    assert_eq!(load_error.exit_code(), 2);

    let exec_error = CliError::Exec(ExecError::Badarith.to_string());
    assert_eq!(exec_error.to_string(), "exec: arithmetic operation failed");
    assert_eq!(exec_error.exit_code(), 1);
}

#[test]
fn malformed_beam_bytes_return_load_error_without_panicking() {
    let path = write_temp_beam("not a valid beam file");

    let error = run_cli([path.to_string_lossy().into_owned()])
        .expect_err("garbage .beam bytes should fail as a load error");

    assert!(matches!(error, CliError::Load(_)));
    assert_eq!(error.exit_code(), 2);
    let _ = std::fs::remove_file(path);
}

#[test]
fn imports_report_for_fixture_is_informational_and_omits_gate1_bifs() {
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../beamr/tests/fixtures/hello.beam")
        .to_string_lossy()
        .into_owned();
    let result =
        run_cli(["imports", fixture.as_str()]).expect("imports report should be informational");

    let CliSuccess::Stdout(output) = result;
    assert!(!output.contains("erlang:get_module_info/1"));
    assert!(!output.contains("erlang:get_module_info/2"));
    assert!(!output.contains("erlang:display/1"));
    assert!(output.lines().all(|line| {
        let Some((_module, function_and_arity)) = line.split_once(':') else {
            return false;
        };
        function_and_arity.split_once('/').is_some()
    }));
}

#[test]
fn parses_dir_flag_with_beam_file() {
    assert_eq!(
        parse_args(["hello.beam", "--dir", "/tmp/beams"]).expect("--dir with beam file parses"),
        Command::Run {
            path: "hello.beam".into(),
            entry: None,
            args: Vec::new(),
            dirs: vec!["/tmp/beams".into()],
        }
    );
}

#[test]
fn parses_multiple_dir_flags() {
    assert_eq!(
        parse_args([
            "hello.beam",
            "--dir",
            "/tmp/a",
            "--dir",
            "/tmp/b",
            "hello:main/0"
        ])
        .expect("multiple --dir flags parse"),
        Command::Run {
            path: "hello.beam".into(),
            entry: Some(EntryPoint {
                module: "hello".into(),
                function: "main".into(),
                arity: 0,
            }),
            args: Vec::new(),
            dirs: vec!["/tmp/a".into(), "/tmp/b".into()],
        }
    );
}

#[test]
fn rejects_dir_without_value() {
    let error =
        parse_args(["hello.beam", "--dir"]).expect_err("--dir without value should fail");

    assert!(matches!(error, CliError::MissingDirValue(_)));
}

fn write_temp_beam(contents: &str) -> std::path::PathBuf {
    let mut path = std::env::temp_dir();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be after Unix epoch")
        .as_nanos();
    path.push(format!("beamr-cli-test-{nanos}.beam"));
    std::fs::write(&path, contents).expect("temp .beam fixture should be writable");
    path
}
