/// Pattern match instruction support.
///
/// BEAM compiles Gleam's `case` and function clause patterns into
/// a sequence of test-and-branch instructions (is_tuple, test_arity,
/// is_eq, etc.). This module supports the interpreter in executing
/// those pattern match sequences, including guard evaluation.

pub(crate) fn _scaffold() {}
