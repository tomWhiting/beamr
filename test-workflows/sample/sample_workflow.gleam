/// Sample Gleam workflow for testing beamr-meridian NIF wiring.
///
/// Exercises the core NIF surface: read_file, run_cmd, write_file.

import gleam/result
import gleam/string
import meridian_ffi

pub type WorkflowResult {
  WorkflowResult(
    file_content: String,
    cmd_output: String,
    written: Bool,
  )
}

pub type WorkflowError {
  ReadFailed(reason: String)
  CmdFailed(reason: String)
  WriteFailed(reason: String)
}

/// Entry point — read a file, run a command, write output.
pub fn run(input_path: String) -> Result(WorkflowResult, WorkflowError) {
  use content <- result.try(
    meridian_ffi.read_file(input_path)
    |> result.map_error(ReadFailed)
  )

  use _cmd_result <- result.try(
    meridian_ffi.run_cmd("echo 'hello from gleam'")
    |> result.map_error(CmdFailed)
  )

  let output = string.concat([
    "Input: ", content, "\nCmd ran successfully",
  ])

  use _ <- result.try(
    meridian_ffi.write_file("/tmp/gleam-workflow-output.txt", output)
    |> result.map_error(WriteFailed)
  )

  Ok(WorkflowResult(
    file_content: content,
    cmd_output: "hello from gleam",
    written: True,
  ))
}
