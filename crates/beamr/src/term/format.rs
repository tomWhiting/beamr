//! User-facing term formatting with atom-name resolution.
//!
//! This formatter is intended for diagnostics and CLI/native output. It is not a
//! serialization format and deliberately avoids raw `Debug` output for runtime
//! terms so opaque atom indices and heap words do not leak into user-facing text.

use crate::atom::{Atom, AtomTable};
use crate::term::binary_ref::BinaryRef;
use crate::term::boxed::{
    BigInt, Closure, Cons, ExternalPid, ExternalReference, Float, Map, Reference, Tuple,
};
use crate::term::{Tag, Term};

const MAX_DEPTH: usize = 64;
const MAX_LIST_ELEMENTS: usize = 1024;

/// Format a BEAM term for user-facing output using `atom_table` to resolve atom
/// names.
#[must_use]
pub fn format_term(term: Term, atom_table: &AtomTable) -> String {
    format_term_at_depth(term, atom_table, 0)
}

fn format_term_at_depth(term: Term, atom_table: &AtomTable, depth: usize) -> String {
    if depth >= MAX_DEPTH {
        return "#<term depth limit>".to_owned();
    }

    match term.tag() {
        Tag::SmallInt => term
            .as_small_int()
            .map(|value| value.to_string())
            .unwrap_or_else(|| "#<invalid small integer>".to_owned()),
        Tag::Atom => term
            .as_atom()
            .map(|atom| format_atom(atom, atom_table))
            .unwrap_or_else(|| "#<invalid atom>".to_owned()),
        Tag::Nil => "[]".to_owned(),
        Tag::Pid => term
            .as_pid()
            .map(|pid| format!("<0.{pid}.0>"))
            .unwrap_or_else(|| "#<invalid pid>".to_owned()),
        Tag::List => format_list(term, atom_table, depth + 1),
        Tag::Boxed => format_boxed(term, atom_table, depth + 1),
    }
}

fn format_atom(atom: Atom, atom_table: &AtomTable) -> String {
    atom_table
        .resolve(atom)
        .map(str::to_owned)
        .unwrap_or_else(|| "#<unknown atom>".to_owned())
}

fn format_list(term: Term, atom_table: &AtomTable, depth: usize) -> String {
    let mut elements = Vec::new();
    let mut current = term;
    let mut count = 0usize;

    loop {
        if current.is_nil() {
            return format!("[{}]", elements.join(", "));
        }

        let Some(cons) = Cons::new(current) else {
            let tail = format_term_at_depth(current, atom_table, depth + 1);
            if elements.is_empty() {
                return format!("[| {tail}]");
            }
            return format!("[{} | {}]", elements.join(", "), tail);
        };

        if count >= MAX_LIST_ELEMENTS {
            elements.push("#<list element limit>".to_owned());
            return format!("[{} | #<list tail>]", elements.join(", "));
        }

        elements.push(format_term_at_depth(cons.head(), atom_table, depth + 1));
        current = cons.tail();
        count += 1;
    }
}

fn format_boxed(term: Term, atom_table: &AtomTable, depth: usize) -> String {
    if let Some(tuple) = Tuple::new(term) {
        return format_tuple(tuple, atom_table, depth);
    }
    if let Some(binary) = BinaryRef::new(term) {
        return format_binary(binary);
    }
    if let Some(float) = Float::new(term) {
        return format_float(float.value());
    }
    if let Some(bigint) = BigInt::new(term) {
        return bigint_to_decimal_string(bigint);
    }
    if let Some(closure) = Closure::new(term) {
        return format_closure(closure, atom_table);
    }
    if let Some(map) = Map::new(term) {
        return format_map(map, atom_table, depth);
    }
    if let Some(reference) = Reference::new(term) {
        return format!("#Ref<0.{}>", reference.id());
    }
    if let Some(pid) = ExternalPid::new(term) {
        let node = pid
            .node()
            .map(|atom| format_atom(atom, atom_table))
            .unwrap_or_else(|| "#<unknown atom>".to_owned());
        return format!("#Pid<{node}.{}.{}>", pid.pid_number(), pid.serial());
    }
    if let Some(reference) = ExternalReference::new(term) {
        let node = reference
            .node()
            .map(|atom| format_atom(atom, atom_table))
            .unwrap_or_else(|| "#<unknown atom>".to_owned());
        return format!("#Ref<{node}.{}>", reference.id());
    }

    "#<opaque boxed term>".to_owned()
}

fn format_tuple(tuple: Tuple, atom_table: &AtomTable, depth: usize) -> String {
    let mut elements = Vec::with_capacity(tuple.arity());
    for index in 0..tuple.arity() {
        let element = tuple
            .get(index)
            .map(|term| format_term_at_depth(term, atom_table, depth + 1))
            .unwrap_or_else(|| "#<missing tuple element>".to_owned());
        elements.push(element);
    }
    format!("{{{}}}", elements.join(", "))
}

fn format_binary(binary: BinaryRef) -> String {
    let bytes = binary.as_bytes();
    match std::str::from_utf8(bytes) {
        Ok(text) => format!("<<\"{}\">>", escape_string(text)),
        Err(_) => format!("<<{} bytes>>", binary.len()),
    }
}

fn escape_string(text: &str) -> String {
    text.chars().flat_map(char::escape_default).collect()
}

fn format_float(value: f64) -> String {
    let formatted = value.to_string();
    if formatted.contains('.') || formatted.contains('e') || formatted.contains('E') {
        formatted
    } else {
        format!("{formatted}.0")
    }
}

fn format_closure(closure: Closure, atom_table: &AtomTable) -> String {
    let module = closure
        .module()
        .map(|atom| format_atom(atom, atom_table))
        .unwrap_or_else(|| "#<unknown atom>".to_owned());
    format!(
        "fun {}:#{}/{}",
        module,
        closure.function_index(),
        closure.arity()
    )
}

fn format_map(map: Map, atom_table: &AtomTable, depth: usize) -> String {
    let mut entries = Vec::with_capacity(map.len());
    for index in 0..map.len() {
        let key = map
            .key(index)
            .map(|term| format_term_at_depth(term, atom_table, depth + 1))
            .unwrap_or_else(|| "#<missing map key>".to_owned());
        let value = map
            .value(index)
            .map(|term| format_term_at_depth(term, atom_table, depth + 1))
            .unwrap_or_else(|| "#<missing map value>".to_owned());
        entries.push(format!("{key} => {value}"));
    }
    format!("#{{{}}}", entries.join(", "))
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

#[cfg(test)]
mod tests {
    use super::format_term;
    use crate::atom::{Atom, AtomTable};
    use crate::term::Term;
    use crate::term::binary::write_binary;
    use crate::term::boxed::{write_bigint, write_closure, write_cons, write_map, write_tuple};

    #[test]
    fn formats_common_atom_by_name() {
        let table = AtomTable::with_common_atoms();

        assert_eq!(format_term(Term::atom(Atom::BADARG), &table), "badarg");
    }

    #[test]
    fn formats_proper_and_improper_lists() {
        let table = AtomTable::with_common_atoms();
        let mut cell3 = [0_u64; 2];
        let mut cell2 = [0_u64; 2];
        let mut cell1 = [0_u64; 2];
        let list3 = match write_cons(&mut cell3, Term::small_int(3), Term::NIL) {
            Some(term) => term,
            None => Term::NIL,
        };
        let list2 = match write_cons(&mut cell2, Term::small_int(2), list3) {
            Some(term) => term,
            None => Term::NIL,
        };
        let list1 = match write_cons(&mut cell1, Term::small_int(1), list2) {
            Some(term) => term,
            None => Term::NIL,
        };

        assert_eq!(format_term(list1, &table), "[1, 2, 3]");

        let mut improper2 = [0_u64; 2];
        let mut improper1 = [0_u64; 2];
        let tail = match write_cons(&mut improper2, Term::small_int(2), Term::small_int(3)) {
            Some(term) => term,
            None => Term::NIL,
        };
        let list = match write_cons(&mut improper1, Term::small_int(1), tail) {
            Some(term) => term,
            None => Term::NIL,
        };

        assert_eq!(format_term(list, &table), "[1, 2 | 3]");
    }

    #[test]
    fn formats_tuple_binary_map_pid_nil_closure_and_bigint() {
        let table = AtomTable::with_common_atoms();
        let module = table.intern("module");
        let function = table.intern("function");
        let key = table.intern("key");
        let value = table.intern("value");

        let mut tuple_heap = [0_u64; 3];
        let tuple = match write_tuple(
            &mut tuple_heap,
            &[Term::atom(Atom::OK), Term::small_int(42)],
        ) {
            Some(term) => term,
            None => Term::NIL,
        };
        assert_eq!(format_term(tuple, &table), "{ok, 42}");

        let mut text_heap = [0_u64; 3];
        let text = match write_binary(&mut text_heap, b"hello") {
            Some(term) => term,
            None => Term::NIL,
        };
        assert_eq!(format_term(text, &table), "<<\"hello\">>");

        let mut bytes_heap = [0_u64; 3];
        let bytes = match write_binary(&mut bytes_heap, &[0, 159, 146, 150, 255]) {
            Some(term) => term,
            None => Term::NIL,
        };
        assert_eq!(format_term(bytes, &table), "<<5 bytes>>");

        let mut map_heap = [0_u64; 4];
        let map = match write_map(&mut map_heap, &[Term::atom(key)], &[Term::atom(value)]) {
            Some(term) => term,
            None => Term::NIL,
        };
        assert_eq!(format_term(map, &table), "#{key => value}");

        assert_eq!(format_term(Term::pid(7), &table), "<0.7.0>");
        assert_eq!(format_term(Term::NIL, &table), "[]");

        let mut closure_heap = [0_u64; 7];
        let closure = match write_closure(&mut closure_heap, module, 3, 2, 0, 0, &[]) {
            Some(term) => term,
            None => Term::NIL,
        };
        assert_eq!(format_term(closure, &table), "fun module:#3/2");

        let mut bigint_heap = [0_u64; 4];
        let bigint = match write_bigint(&mut bigint_heap, true, &[123]) {
            Some(term) => term,
            None => Term::NIL,
        };
        assert_eq!(format_term(bigint, &table), "-123");

        let _ = function;
    }
}
