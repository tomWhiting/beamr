import meridian_ffi

/// Test workflow that calls run_step_norn and returns the result.
pub fn run() {
  case meridian_ffi.run_step_norn("test", "default", "hello", "") {
    Ok(result) -> result
    Error(reason) -> reason
  }
}
