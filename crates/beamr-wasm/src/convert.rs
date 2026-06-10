//! JavaScript value conversion for the WASM host boundary.
//!
//! This module deliberately converts by value. Non-UTF-8 binaries are copied
//! into `Uint8Array` instances and JavaScript objects are traversed into BEAM
//! maps rather than wrapped as opaque host references.

use std::sync::Arc;

use beamr::atom::{Atom, AtomTable};
use beamr::ets::OwnedTerm;
use beamr::native::ProcessContext;
use beamr::term::binary::Binary;
use beamr::term::boxed::{Cons, Float, Map, Tuple};
use beamr::term::{Tag, Term};
use js_sys::{Array, Object, Reflect, Uint8Array};
use serde_json::Value;
use wasm_bindgen::JsValue;

const MAX_CONVERSION_DEPTH: usize = 256;

/// Convert a direct JavaScript value into an owned BEAM term.
///
/// The returned [`OwnedTerm`] keeps any detached heap allocations alive until a
/// caller can copy them into the target process heap.
pub fn js_value_to_owned_term(
    value: JsValue,
    atom_table: &Arc<AtomTable>,
) -> Result<OwnedTerm, JsValue> {
    let mut context = ProcessContext::new();
    context.set_atom_table(Some(Arc::clone(atom_table)));
    let term = js_value_to_term_in_context(value, &mut context)?;
    Ok(context
        .take_detached_result(term)
        .unwrap_or_else(|| OwnedTerm::immediate(term)))
}

/// Convert a direct JavaScript value into a BEAM term allocated in `context`.
pub fn js_value_to_term_in_context(
    value: JsValue,
    context: &mut ProcessContext<'_>,
) -> Result<Term, JsValue> {
    value_to_term(value, context, 0)
}

/// Convert a JSON array into owned BEAM terms for the legacy `spawn` API.
pub fn terms_from_json_array(
    value: &Value,
    atom_table: &Arc<AtomTable>,
) -> Result<Vec<OwnedTerm>, JsValue> {
    let Value::Array(values) = value else {
        return Err(JsValue::from_str("arguments must be a JSON array"));
    };

    values
        .iter()
        .map(|value| {
            let mut context = ProcessContext::new();
            context.set_atom_table(Some(Arc::clone(atom_table)));
            let term = json_value_to_term(value, &mut context, 0)?;
            Ok(context
                .take_detached_result(term)
                .unwrap_or_else(|| OwnedTerm::immediate(term)))
        })
        .collect()
}

/// Convert BEAM terms to a JavaScript array of direct host values.
pub fn terms_to_js_array(args: &[Term], atom_table: &AtomTable) -> Result<JsValue, JsValue> {
    let array = Array::new();
    for term in args {
        array.push(&term_to_js_value(*term, atom_table)?);
    }
    Ok(array.into())
}

/// Convert a BEAM term into a JavaScript value.
pub fn term_to_js_value(term: Term, atom_table: &AtomTable) -> Result<JsValue, JsValue> {
    term_to_js_value_at_depth(term, atom_table, 0)
}

fn value_to_term(
    value: JsValue,
    context: &mut ProcessContext<'_>,
    depth: usize,
) -> Result<Term, JsValue> {
    check_depth(depth)?;

    if value.is_null() {
        return Ok(Term::atom(Atom::NIL));
    }
    if value.is_undefined() {
        return Err(JsValue::from_str(
            "cannot convert JavaScript undefined to a BEAM term",
        ));
    }
    if let Some(boolean) = value.as_bool() {
        return Ok(Term::atom(if boolean { Atom::TRUE } else { Atom::FALSE }));
    }
    if let Some(number) = value.as_f64() {
        return number_to_term(number, context);
    }
    if let Some(string) = value.as_string() {
        return context
            .alloc_binary(string.as_bytes())
            .map_err(|_| JsValue::from_str("failed to allocate binary term"));
    }
    if Array::is_array(&value) {
        return array_to_term(&Array::from(&value), context, depth + 1);
    }
    if value.is_object() {
        return object_to_term(value, context, depth + 1);
    }

    Err(JsValue::from_str(
        "unsupported JavaScript value for BEAM term conversion",
    ))
}

fn json_value_to_term(
    value: &Value,
    context: &mut ProcessContext<'_>,
    depth: usize,
) -> Result<Term, JsValue> {
    check_depth(depth)?;
    match value {
        Value::Null => Ok(Term::atom(Atom::NIL)),
        Value::Bool(true) => Ok(Term::atom(Atom::TRUE)),
        Value::Bool(false) => Ok(Term::atom(Atom::FALSE)),
        Value::Number(number) => number
            .as_f64()
            .ok_or_else(|| JsValue::from_str("unsupported JSON number"))
            .and_then(|value| number_to_term(value, context)),
        Value::String(string) => context
            .alloc_binary(string.as_bytes())
            .map_err(|_| JsValue::from_str("failed to allocate binary term")),
        Value::Array(elements) => {
            let mut tail = Term::NIL;
            for value in elements.iter().rev() {
                let head = json_value_to_term(value, context, depth + 1)?;
                tail = context
                    .alloc_cons(head, tail)
                    .map_err(|_| JsValue::from_str("failed to allocate cons term"))?;
            }
            Ok(tail)
        }
        Value::Object(object) => {
            let mut pairs = Vec::with_capacity(object.len());
            for (key, value) in object {
                let key_term = context
                    .alloc_binary(key.as_bytes())
                    .map_err(|_| JsValue::from_str("failed to allocate map key binary"))?;
                let value_term = json_value_to_term(value, context, depth + 1)?;
                pairs.push((key_term, value_term));
            }
            alloc_sorted_map(pairs, context)
        }
    }
}

fn number_to_term(value: f64, context: &mut ProcessContext<'_>) -> Result<Term, JsValue> {
    if !value.is_finite() {
        return Err(JsValue::from_str(
            "cannot convert non-finite JavaScript number",
        ));
    }
    if value.fract() == 0.0 && value >= i64::MIN as f64 && value <= i64::MAX as f64 {
        let integer = value as i64;
        if let Some(term) = Term::try_small_int(integer) {
            return Ok(term);
        }
    }
    context
        .alloc_float(value)
        .map_err(|_| JsValue::from_str("failed to allocate float term"))
}

fn array_to_term(
    array: &Array,
    context: &mut ProcessContext<'_>,
    depth: usize,
) -> Result<Term, JsValue> {
    let mut tail = Term::NIL;
    for index in (0..array.length()).rev() {
        let head = value_to_term(array.get(index), context, depth)?;
        tail = context
            .alloc_cons(head, tail)
            .map_err(|_| JsValue::from_str("failed to allocate cons term"))?;
    }
    Ok(tail)
}

fn object_to_term(
    value: JsValue,
    context: &mut ProcessContext<'_>,
    depth: usize,
) -> Result<Term, JsValue> {
    let object = Object::from(value);
    let keys = Object::keys(&object);
    let mut pairs = Vec::with_capacity(keys.length() as usize);
    for index in 0..keys.length() {
        let key_value = keys.get(index);
        let key = key_value
            .as_string()
            .ok_or_else(|| JsValue::from_str("JavaScript object key was not a string"))?;
        let property = Reflect::get(&object, &key_value)?;
        let key_term = context
            .alloc_binary(key.as_bytes())
            .map_err(|_| JsValue::from_str("failed to allocate map key binary"))?;
        let value_term = value_to_term(property, context, depth)?;
        pairs.push((key_term, value_term));
    }
    alloc_sorted_map(pairs, context)
}

fn alloc_sorted_map(
    mut pairs: Vec<(Term, Term)>,
    context: &mut ProcessContext<'_>,
) -> Result<Term, JsValue> {
    pairs.sort_by_key(|(key, _)| *key);
    let keys = pairs.iter().map(|(key, _)| *key).collect::<Vec<_>>();
    let values = pairs.iter().map(|(_, value)| *value).collect::<Vec<_>>();
    context
        .alloc_map(&keys, &values)
        .map_err(|_| JsValue::from_str("failed to allocate map term"))
}

fn term_to_js_value_at_depth(
    term: Term,
    atom_table: &AtomTable,
    depth: usize,
) -> Result<JsValue, JsValue> {
    check_depth(depth)?;
    match term.tag() {
        Tag::SmallInt => term
            .as_small_int()
            .map(|value| JsValue::from_f64(value as f64))
            .ok_or_else(|| JsValue::from_str("invalid small integer term")),
        Tag::Atom => atom_to_js_value(term, atom_table),
        Tag::Nil => Ok(Array::new().into()),
        Tag::List => list_to_js_value(term, atom_table, depth + 1),
        Tag::Boxed => boxed_to_js_value(term, atom_table, depth + 1),
        Tag::Pid => Err(JsValue::from_str(
            "cannot convert pid term to JavaScript value",
        )),
    }
}

fn atom_to_js_value(term: Term, atom_table: &AtomTable) -> Result<JsValue, JsValue> {
    let atom = term
        .as_atom()
        .ok_or_else(|| JsValue::from_str("invalid atom term"))?;
    let name = atom_table
        .resolve(atom)
        .ok_or_else(|| JsValue::from_str("atom is not present in the atom table"))?;
    Ok(JsValue::from_str(name))
}

fn list_to_js_value(term: Term, atom_table: &AtomTable, depth: usize) -> Result<JsValue, JsValue> {
    let array = Array::new();
    let mut tail = term;
    loop {
        if tail.is_nil() {
            return Ok(array.into());
        }
        let cons = Cons::new(tail)
            .ok_or_else(|| JsValue::from_str("cannot convert improper list to JavaScript array"))?;
        array.push(&term_to_js_value_at_depth(cons.head(), atom_table, depth)?);
        tail = cons.tail();
    }
}

fn boxed_to_js_value(term: Term, atom_table: &AtomTable, depth: usize) -> Result<JsValue, JsValue> {
    if let Some(binary) = Binary::new(term) {
        return binary_to_js_value(binary);
    }
    if let Some(tuple) = Tuple::new(term) {
        return tuple_to_js_value(tuple, atom_table, depth);
    }
    if let Some(map) = Map::new(term) {
        return map_to_js_value(map, atom_table, depth);
    }
    if let Some(float) = Float::new(term) {
        return Ok(JsValue::from_f64(float.value()));
    }
    Err(JsValue::from_str(
        "unsupported boxed term for JavaScript conversion",
    ))
}

fn binary_to_js_value(binary: Binary) -> Result<JsValue, JsValue> {
    match std::str::from_utf8(binary.as_bytes()) {
        Ok(text) => Ok(JsValue::from_str(text)),
        Err(_) => {
            let length = u32::try_from(binary.len())
                .map_err(|_| JsValue::from_str("binary is too large for Uint8Array"))?;
            let array = Uint8Array::new_with_length(length);
            array.copy_from(binary.as_bytes());
            Ok(array.into())
        }
    }
}

fn tuple_to_js_value(
    tuple: Tuple,
    atom_table: &AtomTable,
    depth: usize,
) -> Result<JsValue, JsValue> {
    let array = Array::new();
    for index in 0..tuple.arity() {
        let element = tuple
            .get(index)
            .ok_or_else(|| JsValue::from_str("invalid tuple element"))?;
        array.push(&term_to_js_value_at_depth(element, atom_table, depth)?);
    }
    Ok(array.into())
}

fn map_to_js_value(map: Map, atom_table: &AtomTable, depth: usize) -> Result<JsValue, JsValue> {
    let object = Object::new();
    for index in 0..map.len() {
        let key = map
            .key(index)
            .ok_or_else(|| JsValue::from_str("invalid map key"))?;
        let key_name = map_key_to_string(key, atom_table)?;
        let value = map
            .value(index)
            .ok_or_else(|| JsValue::from_str("invalid map value"))?;
        Reflect::set(
            &object,
            &JsValue::from_str(&key_name),
            &term_to_js_value_at_depth(value, atom_table, depth)?,
        )?;
    }
    Ok(object.into())
}

fn map_key_to_string(term: Term, atom_table: &AtomTable) -> Result<String, JsValue> {
    if let Some(atom) = term.as_atom() {
        return atom_table
            .resolve(atom)
            .map(str::to_owned)
            .ok_or_else(|| JsValue::from_str("map atom key is not present in the atom table"));
    }
    if let Some(binary) = Binary::new(term) {
        return std::str::from_utf8(binary.as_bytes())
            .map(str::to_owned)
            .map_err(|_| JsValue::from_str("map binary key is not valid UTF-8"));
    }
    Err(JsValue::from_str(
        "map key cannot be converted to a JavaScript property name",
    ))
}

fn check_depth(depth: usize) -> Result<(), JsValue> {
    if depth > MAX_CONVERSION_DEPTH {
        Err(JsValue::from_str(
            "JavaScript/Term conversion exceeded maximum depth",
        ))
    } else {
        Ok(())
    }
}

#[cfg(all(test, target_arch = "wasm32"))]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn atom_table() -> Arc<AtomTable> {
        Arc::new(AtomTable::with_common_atoms())
    }

    fn binary_context_key(context: &mut ProcessContext<'_>, text: &str) -> Term {
        context
            .alloc_binary(text.as_bytes())
            .expect("test key binary allocation succeeds")
    }

    fn list_to_vec(mut term: Term) -> Vec<Term> {
        let mut values = Vec::new();
        while !term.is_nil() {
            let cons = Cons::new(term).expect("converted JavaScript array is a proper list");
            values.push(cons.head());
            term = cons.tail();
        }
        values
    }

    #[wasm_bindgen_test]
    fn converts_complex_nested_js_object_to_term() {
        let table = atom_table();
        let input = Object::new();
        let nested = Object::new();
        let array = Array::new();
        array.push(&JsValue::from_f64(1.0));
        array.push(&JsValue::from_bool(true));
        assert!(Reflect::set(&nested, &JsValue::from_str("items"), &array).is_ok());
        assert!(Reflect::set(&nested, &JsValue::from_str("missing"), &JsValue::NULL).is_ok());
        assert!(
            Reflect::set(
                &input,
                &JsValue::from_str("name"),
                &JsValue::from_str("beamr")
            )
            .is_ok()
        );
        assert!(Reflect::set(&input, &JsValue::from_str("nested"), &nested).is_ok());

        let owned = js_value_to_owned_term(input.into(), &table)
            .expect("complex JavaScript object converts to an owned term");
        let term = owned.root();
        let map = Map::new(term).expect("top-level object converts to map");
        assert_eq!(map.len(), 2);

        let mut key_context = ProcessContext::new();
        let name = map
            .get(binary_context_key(&mut key_context, "name"))
            .expect("name key is present");
        let name_binary = Binary::new(name).expect("string value converts to binary");
        assert_eq!(name_binary.as_bytes(), b"beamr");

        let nested = map
            .get(binary_context_key(&mut key_context, "nested"))
            .expect("nested key is present");
        let nested_map = Map::new(nested).expect("nested object converts to map");
        assert_eq!(nested_map.len(), 2);
        assert_eq!(
            nested_map.get(binary_context_key(&mut key_context, "missing")),
            Some(Term::atom(Atom::NIL))
        );

        let items = nested_map
            .get(binary_context_key(&mut key_context, "items"))
            .expect("array-valued key is present");
        let items = list_to_vec(items);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0], Term::small_int(1));
        assert_eq!(items[1], Term::atom(Atom::TRUE));
    }

    #[wasm_bindgen_test]
    fn converts_terms_to_js_values() {
        let table = atom_table();
        let mut context = ProcessContext::new();
        context.set_atom_table(Some(Arc::clone(&table)));
        let utf8 = context
            .alloc_binary("hello".as_bytes())
            .unwrap_or(Term::NIL);
        let bytes = context.alloc_binary(&[0xff, 0x00]).unwrap_or(Term::NIL);
        let list = context
            .alloc_list(&[Term::small_int(7), Term::atom(Atom::TRUE)])
            .unwrap_or(Term::NIL);
        let tuple = context
            .alloc_tuple(&[utf8, Term::small_int(9)])
            .unwrap_or(Term::NIL);
        let key = context
            .alloc_binary("tuple".as_bytes())
            .unwrap_or(Term::NIL);
        let map = context.alloc_map(&[key], &[tuple]).unwrap_or(Term::NIL);

        let utf8_js = term_to_js_value(utf8, table.as_ref()).unwrap_or(JsValue::UNDEFINED);
        assert_eq!(utf8_js.as_string().as_deref(), Some("hello"));

        let bytes_js = term_to_js_value(bytes, table.as_ref()).unwrap_or(JsValue::UNDEFINED);
        assert!(bytes_js.is_instance_of::<Uint8Array>());
        let bytes_array = Uint8Array::from(bytes_js);
        assert_eq!(bytes_array.length(), 2);
        assert_eq!(bytes_array.get_index(0), 0xff);
        assert_eq!(bytes_array.get_index(1), 0x00);

        let list_js = term_to_js_value(list, table.as_ref()).unwrap_or(JsValue::UNDEFINED);
        assert!(Array::is_array(&list_js));
        let list_array = Array::from(&list_js);
        assert_eq!(list_array.length(), 2);
        assert_eq!(list_array.get(0).as_f64(), Some(7.0));
        assert_eq!(list_array.get(1).as_string().as_deref(), Some("true"));

        let tuple_js = term_to_js_value(tuple, table.as_ref()).unwrap_or(JsValue::UNDEFINED);
        assert!(Array::is_array(&tuple_js));
        let tuple_array = Array::from(&tuple_js);
        assert_eq!(tuple_array.length(), 2);
        assert_eq!(tuple_array.get(0).as_string().as_deref(), Some("hello"));
        assert_eq!(tuple_array.get(1).as_f64(), Some(9.0));

        let map_js = term_to_js_value(map, table.as_ref()).unwrap_or(JsValue::UNDEFINED);
        let nested_tuple_js =
            Reflect::get(&map_js, &JsValue::from_str("tuple")).unwrap_or(JsValue::UNDEFINED);
        assert!(Array::is_array(&nested_tuple_js));
    }

    #[wasm_bindgen_test]
    fn documents_boolean_atom_round_trip_as_atom_names() {
        let table = atom_table();
        let owned = js_value_to_owned_term(JsValue::from_bool(true), &table)
            .expect("boolean converts to an owned atom term");
        let term = owned.root();
        assert_eq!(term, Term::atom(Atom::TRUE));
        let js = term_to_js_value(term, table.as_ref()).unwrap_or(JsValue::UNDEFINED);
        assert_eq!(js.as_string().as_deref(), Some("true"));
    }
}
