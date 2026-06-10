//! Utility stub BIFs for OTP modules — logger, unicode, sys, gleam_stdlib,
//! maps, lists, and timer.
//!
//! These are simple stubs with correct semantics registered under their
//! respective OTP module names. They satisfy imports from gleam_otp actor
//! and supervisor modules.
//!
//! Non-higher-order collection BIFs (maps, lists, timer) are in the
//! `collection_bifs` submodule to keep each file under 500 lines.

pub mod bitwise_bifs;
pub mod collection_bifs;
pub mod encoding_bifs;
pub mod gleam_stdlib_ffi;
pub mod gleam_stdlib_ffi2;
pub mod io_bifs;
pub mod json_bifs;
pub mod lists_bifs;
pub mod lists_hof_bifs;
pub mod maps_bifs;
pub mod math_bifs;
pub mod string_bifs;
pub mod type_conversion_bifs;
pub mod uri_bifs;
pub mod misc_bifs;
pub mod registrations;

pub use misc_bifs::{
    bif_binary_part, bif_characters_to_binary, bif_characters_to_list, bif_debug_options,
    bif_fun_info, bif_fwrite_g, bif_identity, bif_init_stop, bif_logger_warning,
    bif_rand_uniform,
};
pub use registrations::register_stdlib_stubs;

#[cfg(test)]
mod b033_tests;
#[cfg(test)]
mod b038_tests;
#[cfg(test)]
mod b127_tests;
#[cfg(test)]
mod bitwise_bifs_tests;
#[cfg(test)]
mod collection_bifs_tests;
#[cfg(test)]
mod gc_rooting_tests;
#[cfg(test)]
mod gleam_stdlib_ffi2_tests;
#[cfg(test)]
mod io_bifs_tests;
#[cfg(test)]
mod json_bifs_tests;
#[cfg(test)]
mod math_bifs_tests;
#[cfg(test)]
mod rand_bifs_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod type_conversion_bifs_tests;
#[cfg(test)]
mod uri_bifs_tests;
