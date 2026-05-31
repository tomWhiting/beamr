/// FFI declarations for Meridian NIF functions.
/// Each maps to a Rust function registered under the "meridian_ffi" module in beamr.

import gleam/dynamic.{type Dynamic}

@external(erlang, "meridian_ffi", "read_file")
pub fn read_file(path: String) -> Result(String, String)

@external(erlang, "meridian_ffi", "read_json")
pub fn read_json(path: String) -> Result(Dynamic, String)

@external(erlang, "meridian_ffi", "run_cmd")
pub fn run_cmd(command: String) -> Result(Dynamic, String)

@external(erlang, "meridian_ffi", "write_file")
pub fn write_file(path: String, content: String) -> Result(Nil, String)

@external(erlang, "meridian_ffi", "commit")
pub fn commit(message: String) -> Result(Dynamic, String)

@external(erlang, "meridian_ffi", "run_step_norn")
pub fn run_step_norn(
  name: String,
  profile: String,
  instruction: String,
  output_schema: String,
) -> Result(Dynamic, String)
