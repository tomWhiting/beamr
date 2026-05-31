/// External NIF declarations for meridian_ffi module.
/// These map to native Rust BIFs registered by beamr-meridian.

@external(erlang, "meridian_ffi", "run_step_norn")
pub fn run_step_norn(
  name: String,
  profile: String,
  instruction: String,
  schema: String,
) -> Result(String, String)
