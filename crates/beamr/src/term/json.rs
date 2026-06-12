//! Conversion between BEAM terms and `serde_json::Value`.

use std::fmt;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use serde_json::{Map as JsonObject, Number, Value};

use crate::atom::{Atom, AtomTable};
use crate::native::ProcessContext;
use crate::term::{
    Tag, Term,
    binary_ref::BinaryRef,
    boxed::{BigInt, Cons, Float, Map, Tuple},
};

/// Error raised while converting between BEAM terms and JSON values.
#[derive(Clone, Debug, PartialEq)]
pub enum JsonTermError {
    /// An atom term could not be resolved through the provided atom table.
    UnknownAtom(Atom),
    /// A boxed term used a layout this bridge does not represent as JSON.
    UnsupportedTerm(&'static str),
    /// A list tail was neither another cons cell nor `Term::NIL`.
    ImproperListTail(Term),
    /// A BEAM map key converted to a JSON value that cannot be an object key.
    NonStringMapKey(Value),
    /// A boxed float was NaN or infinite, which JSON numbers cannot encode.
    NonFiniteFloat(f64),
    /// A JSON number cannot be represented with the supported BEAM numeric terms.
    UnsupportedNumber(Number),
    /// Object key conversion requires a configured atom table in the process context.
    MissingAtomTable,
    /// A process heap allocation unexpectedly failed to write its boxed layout.
    AllocationFailed(&'static str),
}

impl fmt::Display for JsonTermError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownAtom(atom) => write!(formatter, "unknown atom {atom:?}"),
            Self::UnsupportedTerm(term_type) => {
                write!(formatter, "unsupported term type {term_type}")
            }
            Self::ImproperListTail(tail) => write!(formatter, "improper list tail {tail:?}"),
            Self::NonStringMapKey(key) => write!(
                formatter,
                "map key converted to non-string JSON value {key:?}"
            ),
            Self::NonFiniteFloat(value) => write!(
                formatter,
                "non-finite float cannot be represented as JSON: {value}"
            ),
            Self::UnsupportedNumber(number) => {
                write!(formatter, "unsupported JSON number {number}")
            }
            Self::MissingAtomTable => {
                formatter.write_str("process context is missing an atom table")
            }
            Self::AllocationFailed(term_type) => {
                write!(formatter, "failed to allocate {term_type} term")
            }
        }
    }
}

impl std::error::Error for JsonTermError {}

/// Convert a BEAM term to a `serde_json::Value`.
pub fn term_to_value(term: Term, atom_table: &AtomTable) -> Result<Value, JsonTermError> {
    match term.tag() {
        Tag::SmallInt => Ok(Value::Number(Number::from(
            term.as_small_int()
                .ok_or(JsonTermError::UnsupportedTerm("small_int"))?,
        ))),
        Tag::Atom => atom_to_value(
            term.as_atom()
                .ok_or(JsonTermError::UnsupportedTerm("atom"))?,
            atom_table,
        ),
        Tag::Pid => Ok(Value::String(format!(
            "<0.{}.0>",
            term.as_pid().ok_or(JsonTermError::UnsupportedTerm("pid"))?
        ))),
        Tag::Nil => Ok(Value::Array(Vec::new())),
        Tag::List => list_to_value(term, atom_table),
        Tag::Boxed => boxed_to_value(term, atom_table),
    }
}

/// Convert a `serde_json::Value` to a BEAM term.
pub fn value_to_term(value: &Value, context: &mut ProcessContext) -> Result<Term, JsonTermError> {
    match value {
        Value::Null => {
            let atom_table = context
                .atom_table()
                .ok_or(JsonTermError::MissingAtomTable)?;
            Ok(Term::atom(atom_table.intern("null")))
        }
        Value::Bool(true) => Ok(Term::atom(Atom::TRUE)),
        Value::Bool(false) => Ok(Term::atom(Atom::FALSE)),
        Value::Number(number) => number_to_term(number, context),
        Value::String(string) => string_to_binary_term(string, context),
        Value::Array(elements) => array_to_list_term(elements, context),
        Value::Object(object) => object_to_map_term(object, context),
    }
}

fn atom_to_value(atom: Atom, atom_table: &AtomTable) -> Result<Value, JsonTermError> {
    match atom {
        Atom::TRUE => Ok(Value::Bool(true)),
        Atom::FALSE => Ok(Value::Bool(false)),
        Atom::NIL | Atom::UNDEFINED => Ok(Value::Null),
        other => {
            let name = atom_table
                .resolve(other)
                .ok_or(JsonTermError::UnknownAtom(other))?;
            if name == "null" {
                Ok(Value::Null)
            } else {
                Ok(Value::String(name.to_owned()))
            }
        }
    }
}

fn list_to_value(term: Term, atom_table: &AtomTable) -> Result<Value, JsonTermError> {
    let mut elements = Vec::new();
    let mut tail = term;
    loop {
        if tail.is_nil() {
            return Ok(Value::Array(elements));
        }

        let cons = Cons::new(tail).ok_or(JsonTermError::ImproperListTail(tail))?;
        elements.push(term_to_value(cons.head(), atom_table)?);
        tail = cons.tail();
    }
}

fn boxed_to_value(term: Term, atom_table: &AtomTable) -> Result<Value, JsonTermError> {
    if let Some(binary) = BinaryRef::new(term) {
        return binary_to_value(binary);
    }
    if let Some(tuple) = Tuple::new(term) {
        return tuple_to_value(tuple, atom_table);
    }
    if let Some(map) = Map::new(term) {
        return map_to_value(map, atom_table);
    }
    if let Some(float) = Float::new(term) {
        return float_to_value(float.value());
    }
    if let Some(bigint) = BigInt::new(term) {
        return bigint_to_value(bigint);
    }

    Err(JsonTermError::UnsupportedTerm("boxed"))
}

fn binary_to_value(binary: BinaryRef) -> Result<Value, JsonTermError> {
    match std::str::from_utf8(binary.as_bytes()) {
        Ok(text) => Ok(Value::String(text.to_owned())),
        Err(_) => Ok(Value::String(BASE64_STANDARD.encode(binary.as_bytes()))),
    }
}

fn tuple_to_value(tuple: Tuple, atom_table: &AtomTable) -> Result<Value, JsonTermError> {
    let mut values = Vec::with_capacity(tuple.arity());
    for index in 0..tuple.arity() {
        let element = tuple
            .get(index)
            .ok_or(JsonTermError::UnsupportedTerm("tuple"))?;
        values.push(term_to_value(element, atom_table)?);
    }
    Ok(Value::Array(values))
}

fn map_to_value(map: Map, atom_table: &AtomTable) -> Result<Value, JsonTermError> {
    let mut object = JsonObject::new();
    for index in 0..map.len() {
        let key = map
            .key(index)
            .ok_or(JsonTermError::UnsupportedTerm("map"))?;
        let key_name = map_key_to_string(key, atom_table)?;
        let value = map
            .value(index)
            .ok_or(JsonTermError::UnsupportedTerm("map"))?;
        object.insert(key_name, term_to_value(value, atom_table)?);
    }
    Ok(Value::Object(object))
}

fn map_key_to_string(term: Term, atom_table: &AtomTable) -> Result<String, JsonTermError> {
    if let Some(atom) = term.as_atom() {
        return atom_table
            .resolve(atom)
            .map(str::to_owned)
            .ok_or(JsonTermError::UnknownAtom(atom));
    }

    let key_value = term_to_value(term, atom_table)?;
    let Value::String(key_name) = key_value else {
        return Err(JsonTermError::NonStringMapKey(key_value));
    };
    Ok(key_name)
}

fn float_to_value(value: f64) -> Result<Value, JsonTermError> {
    Number::from_f64(value)
        .map(Value::Number)
        .ok_or(JsonTermError::NonFiniteFloat(value))
}

fn bigint_to_value(bigint: BigInt) -> Result<Value, JsonTermError> {
    if bigint.limb_count() == 0 {
        return Ok(Value::Number(Number::from(0)));
    }

    if let Some(value) = bigint_to_i128(bigint) {
        if let Ok(signed) = i64::try_from(value) {
            return Ok(Value::Number(Number::from(signed)));
        }
        if let Ok(unsigned) = u64::try_from(value) {
            return Ok(Value::Number(Number::from(unsigned)));
        }
        return Ok(Value::String(value.to_string()));
    }

    Ok(Value::String(bigint_to_decimal_string(bigint)))
}

fn bigint_to_i128(bigint: BigInt) -> Option<i128> {
    let mut magnitude = 0_u128;
    for (index, limb) in bigint.limbs().iter().copied().enumerate() {
        let shift = index.checked_mul(u64::BITS as usize)?;
        let shifted = u128::from(limb).checked_shl(shift as u32)?;
        magnitude = magnitude.checked_add(shifted)?;
    }

    if bigint.is_negative() {
        if magnitude == (i128::MAX as u128) + 1 {
            Some(i128::MIN)
        } else {
            i128::try_from(magnitude).ok().map(|value| -value)
        }
    } else {
        i128::try_from(magnitude).ok()
    }
}

fn bigint_to_decimal_string(bigint: BigInt) -> String {
    let mut limbs = bigint.limbs().to_vec();
    while limbs.last().copied() == Some(0) {
        limbs.pop();
    }

    if limbs.is_empty() {
        return "0".to_owned();
    }

    let mut digits = Vec::new();
    while limbs.iter().any(|limb| *limb != 0) {
        let remainder = div_rem_limbs_by_10(&mut limbs);
        digits.push(char::from(b'0' + remainder as u8));
        while limbs.last().copied() == Some(0) {
            limbs.pop();
        }
    }

    if bigint.is_negative() {
        digits.push('-');
    }
    digits.iter().rev().collect()
}

fn div_rem_limbs_by_10(limbs: &mut [u64]) -> u64 {
    let mut remainder = 0_u128;
    for limb in limbs.iter_mut().rev() {
        let value = (remainder << u64::BITS) | u128::from(*limb);
        *limb = (value / 10) as u64;
        remainder = value % 10;
    }
    remainder as u64
}

fn number_to_term(number: &Number, context: &mut ProcessContext) -> Result<Term, JsonTermError> {
    if let Some(value) = number.as_i64() {
        if let Some(term) = Term::try_small_int(value) {
            return Ok(term);
        }
        return allocate_bigint_from_i128(i128::from(value), context);
    }

    if let Some(value) = number.as_u64() {
        if let Ok(signed) = i64::try_from(value)
            && let Some(term) = Term::try_small_int(signed)
        {
            return Ok(term);
        }
        return allocate_bigint_from_u64(value, context);
    }

    let value = number
        .as_f64()
        .ok_or_else(|| JsonTermError::UnsupportedNumber(number.clone()))?;
    allocate_float_term(value, context)
}

fn allocate_bigint_from_i128(
    value: i128,
    context: &mut ProcessContext,
) -> Result<Term, JsonTermError> {
    let negative = value.is_negative();
    let magnitude = value.unsigned_abs();
    let limbs = limbs_from_u128(magnitude);
    allocate_bigint_term(negative, &limbs, context)
}

fn allocate_bigint_from_u64(
    value: u64,
    context: &mut ProcessContext,
) -> Result<Term, JsonTermError> {
    allocate_bigint_term(false, &[value], context)
}

fn allocate_bigint_term(
    negative: bool,
    limbs: &[u64],
    context: &mut ProcessContext,
) -> Result<Term, JsonTermError> {
    context
        .alloc_bigint(negative, limbs)
        .map_err(|_| JsonTermError::AllocationFailed("bigint"))
}

fn limbs_from_u128(value: u128) -> Vec<u64> {
    let low = value as u64;
    let high = (value >> u64::BITS) as u64;
    if high == 0 {
        vec![low]
    } else {
        vec![low, high]
    }
}

fn allocate_float_term(value: f64, context: &mut ProcessContext) -> Result<Term, JsonTermError> {
    context
        .alloc_float(value)
        .map_err(|_| JsonTermError::AllocationFailed("float"))
}

fn string_to_binary_term(
    string: &str,
    context: &mut ProcessContext,
) -> Result<Term, JsonTermError> {
    context
        .alloc_binary(string.as_bytes())
        .map_err(|_| JsonTermError::AllocationFailed("binary"))
}

fn array_to_list_term(
    elements: &[Value],
    context: &mut ProcessContext,
) -> Result<Term, JsonTermError> {
    let mut tail = Term::NIL;
    for value in elements.iter().rev() {
        let head = value_to_term(value, context)?;
        tail = context
            .alloc_cons(head, tail)
            .map_err(|_| JsonTermError::AllocationFailed("cons"))?;
    }
    Ok(tail)
}

fn object_to_map_term(
    object: &JsonObject<String, Value>,
    context: &mut ProcessContext,
) -> Result<Term, JsonTermError> {
    let mut pairs = Vec::with_capacity(object.len());
    for (key, value) in object {
        let key_term = string_to_binary_term(key, context)?;
        let value_term = value_to_term(value, context)?;
        pairs.push((key_term, value_term));
    }
    pairs.sort_by_key(|(key, _)| *key);

    let keys = pairs.iter().map(|(key, _)| *key).collect::<Vec<_>>();
    let values = pairs.iter().map(|(_, value)| *value).collect::<Vec<_>>();
    context
        .alloc_map(&keys, &values)
        .map_err(|_| JsonTermError::AllocationFailed("map"))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::*;
    use crate::process::Process;
    use crate::term::boxed::{write_bigint, write_cons, write_float, write_map, write_tuple};

    fn atom_table() -> AtomTable {
        AtomTable::with_common_atoms()
    }

    fn context() -> (Arc<AtomTable>, Process) {
        (
            Arc::new(AtomTable::with_common_atoms()),
            Process::new(42, 512),
        )
    }

    fn attach_context<'process>(
        table: &Arc<AtomTable>,
        process: &'process mut Process,
    ) -> ProcessContext<'process> {
        let mut context = ProcessContext::new();
        context.set_atom_table(Some(Arc::clone(table)));
        context.attach_process(process, 0);
        context
    }

    fn binary_term(process: &mut Process, bytes: &[u8]) -> Term {
        let table = Arc::new(AtomTable::with_common_atoms());
        let mut context = attach_context(&table, process);
        context
            .alloc_binary(bytes)
            .expect("test binary allocation should fit")
    }

    #[test]
    fn term_to_value_converts_immediates() {
        let table = atom_table();

        assert_eq!(term_to_value(Term::small_int(42), &table), Ok(json!(42)));
        assert_eq!(
            term_to_value(Term::atom(Atom::TRUE), &table),
            Ok(json!(true))
        );
        assert_eq!(
            term_to_value(Term::atom(Atom::FALSE), &table),
            Ok(json!(false))
        );
        assert_eq!(
            term_to_value(Term::atom(Atom::NIL), &table),
            Ok(Value::Null)
        );
        assert_eq!(
            term_to_value(Term::atom(Atom::UNDEFINED), &table),
            Ok(Value::Null)
        );
        assert_eq!(term_to_value(Term::atom(Atom::OK), &table), Ok(json!("ok")));
        assert_eq!(term_to_value(Term::NIL, &table), Ok(json!([])));
        assert_eq!(term_to_value(Term::pid(7), &table), Ok(json!("<0.7.0>")));
    }

    #[test]
    fn term_to_value_handles_unknown_atoms_without_panicking() {
        let table = atom_table();

        assert_eq!(
            term_to_value(Term::atom(Atom::new(999_999)), &table),
            Err(JsonTermError::UnknownAtom(Atom::new(999_999)))
        );
    }

    #[test]
    fn term_to_value_converts_binaries() {
        let table = atom_table();
        let mut process = Process::new(7, 64);

        assert_eq!(
            term_to_value(binary_term(&mut process, b"hello"), &table),
            Ok(json!("hello"))
        );
        assert_eq!(
            term_to_value(binary_term(&mut process, &[0xff, 0x00]), &table),
            Ok(json!("/wA="))
        );
    }

    #[test]
    fn term_to_value_converts_tuple_list_map_float_and_bigint() {
        let table = atom_table();
        let mut process = Process::new(8, 64);
        let mut tuple_heap = [0_u64; 3];
        let tuple = write_tuple(
            &mut tuple_heap,
            &[Term::atom(Atom::OK), Term::small_int(42)],
        )
        .expect("tuple should fit");
        assert_eq!(term_to_value(tuple, &table), Ok(json!(["ok", 42])));

        let mut second_cell = [0_u64; 2];
        let mut first_cell = [0_u64; 2];
        let second = write_cons(&mut second_cell, Term::small_int(2), Term::NIL)
            .expect("second cons should fit");
        let list =
            write_cons(&mut first_cell, Term::small_int(1), second).expect("first cons should fit");
        assert_eq!(term_to_value(list, &table), Ok(json!([1, 2])));

        let keys = [Term::atom(Atom::OK)];
        let values = [binary_term(&mut process, b"value")];
        let mut map_heap = [0_u64; 4];
        let map = write_map(&mut map_heap, &keys, &values).expect("map should fit");
        assert_eq!(term_to_value(map, &table), Ok(json!({"ok": "value"})));

        let mut float_heap = [0_u64; 2];
        let float = write_float(&mut float_heap, 1.5).expect("float should fit");
        assert_eq!(term_to_value(float, &table), Ok(json!(1.5)));

        let mut bigint_heap = [0_u64; 4];
        let bigint = write_bigint(&mut bigint_heap, false, &[Term::SMALL_INT_MAX as u64 + 1])
            .expect("bigint should fit");
        assert_eq!(
            term_to_value(bigint, &table),
            Ok(json!(Term::SMALL_INT_MAX + 1))
        );
    }

    #[test]
    fn term_to_value_converts_nested_structures_recursively() {
        let table = atom_table();
        let mut tuple_heap = [0_u64; 3];
        let tuple = write_tuple(
            &mut tuple_heap,
            &[Term::atom(Atom::OK), Term::small_int(42)],
        )
        .expect("tuple should fit");
        let keys = [Term::atom(Atom::INFO)];
        let values = [tuple];
        let mut map_heap = [0_u64; 4];
        let map = write_map(&mut map_heap, &keys, &values).expect("map should fit");

        assert_eq!(term_to_value(map, &table), Ok(json!({"info": ["ok", 42]})));
    }

    #[test]
    fn value_to_term_converts_json_scalars() {
        let (table, mut process) = context();
        let mut context = attach_context(&table, &mut process);

        assert_eq!(
            value_to_term(&json!(42), &mut context),
            Ok(Term::small_int(42))
        );
        let null_term = value_to_term(&Value::Null, &mut context).expect("null");
        assert!(null_term.is_atom());
        assert_eq!(table.resolve(null_term.as_atom().unwrap()), Some("null"));
        assert_eq!(
            value_to_term(&json!(true), &mut context),
            Ok(Term::atom(Atom::TRUE))
        );
        assert_eq!(
            value_to_term(&json!(false), &mut context),
            Ok(Term::atom(Atom::FALSE))
        );

        let binary = value_to_term(&json!("hello"), &mut context).expect("string to binary");
        assert_eq!(term_to_value(binary, &table), Ok(json!("hello")));

        let float = value_to_term(&json!(1.25), &mut context).expect("float to term");
        assert_eq!(term_to_value(float, &table), Ok(json!(1.25)));
    }

    #[test]
    fn value_to_term_converts_arrays_to_proper_lists() {
        let (table, mut process) = context();
        let mut context = attach_context(&table, &mut process);
        let term = value_to_term(&json!([1, 2, 3]), &mut context).expect("array to list");

        assert_eq!(term_to_value(term, &table), Ok(json!([1, 2, 3])));
        let first = Cons::new(term).expect("first cons");
        let second = Cons::new(first.tail()).expect("second cons");
        let third = Cons::new(second.tail()).expect("third cons");
        assert_eq!(first.head(), Term::small_int(1));
        assert_eq!(second.head(), Term::small_int(2));
        assert_eq!(third.head(), Term::small_int(3));
        assert_eq!(third.tail(), Term::NIL);
    }

    #[test]
    fn value_to_term_converts_objects_to_binary_keyed_maps() {
        let (table, mut process) = context();
        let mut context = attach_context(&table, &mut process);
        let term = value_to_term(&json!({"key": "value"}), &mut context).expect("object to map");
        let map = Map::new(term).expect("map accessor");
        let key = map.key(0).expect("first key");
        let key_binary = crate::term::binary::Binary::new(key).expect("key is a binary");
        assert_eq!(key_binary.as_bytes(), b"key");
    }

    #[test]
    fn map_atom_keys_use_atom_names_even_for_json_special_atoms() {
        let table = atom_table();
        let keys = [Term::atom(Atom::TRUE), Term::atom(Atom::NIL)];
        let values = [Term::small_int(1), Term::small_int(2)];
        let mut map_heap = [0_u64; 6];
        let map = write_map(&mut map_heap, &keys, &values).expect("map should fit");

        assert_eq!(term_to_value(map, &table), Ok(json!({"true": 1, "nil": 2})));
    }

    #[test]
    fn round_trip_preserves_object_keys_named_like_special_atoms() {
        let (table, mut process) = context();
        let mut context = attach_context(&table, &mut process);
        let value = json!({"true": "bool-name", "nil": "nil-name"});
        let term = value_to_term(&value, &mut context).expect("object to term");

        assert_eq!(term_to_value(term, &table), Ok(value));
    }

    #[test]
    fn value_to_term_requires_atom_table_for_null() {
        let mut context = ProcessContext::new();

        assert_eq!(
            value_to_term(&Value::Null, &mut context),
            Err(JsonTermError::MissingAtomTable)
        );
    }

    #[test]
    fn value_to_term_objects_work_without_atom_table() {
        let mut process = Process::new(43, 128);
        let mut context = ProcessContext::new();
        context.attach_process(&mut process, 0);
        let term = value_to_term(&json!({"key": "value"}), &mut context);
        assert!(term.is_ok());
    }

    #[test]
    fn round_trip_preserves_representable_json_shapes() {
        let (table, mut process) = context();
        let mut context = attach_context(&table, &mut process);
        let values = [
            json!(true),
            json!(false),
            json!(42),
            json!(1.25),
            json!("hello"),
            json!([1, "two", true]),
            json!({"key": "value", "nested": [1, 2]}),
        ];

        for value in values {
            let term = value_to_term(&value, &mut context).expect("value to term");
            assert_eq!(term_to_value(term, &table), Ok(value));
        }
    }

    #[test]
    fn null_round_trips_as_null_atom() {
        let (table, mut process) = context();
        let mut context = attach_context(&table, &mut process);
        let term = value_to_term(&Value::Null, &mut context).expect("null to atom");

        assert!(term.is_atom());
        assert_eq!(table.resolve(term.as_atom().unwrap()), Some("null"));
        assert_eq!(term_to_value(term, &table), Ok(Value::Null));
    }
}
