/// Native function interface — how Gleam reaches into Rust.
///
/// A registry where Rust functions are registered under MFA names.
/// When the interpreter hits a call to a registered native, it invokes
/// the Rust function directly — same process, no IPC, no serialisation.
/// BIFs (built-in, ship with the VM) and NIFs (registered by the host)
/// use the same mechanism but have different ownership (per D6).
pub mod bifs;
