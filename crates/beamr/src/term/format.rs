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
    if let Some(text) = format_printable_charlist(term) {
        return text;
    }
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

/// OTP's `io_lib:printable_list/1` in latin1 mode: a proper list whose
/// elements are all printable latin1 character codes renders as a
/// double-quoted, Erlang-escaped string, matching the Erlang shell and
/// `erlang:display/1` (e.g. `[104,105]` prints as `"hi"`). Returns `None`
/// for anything else so the caller falls back to element-wise rendering.
fn format_printable_charlist(term: Term) -> Option<String> {
    let mut out = String::from("\"");
    let mut current = term;
    let mut count = 0usize;
    while !current.is_nil() {
        let cons = Cons::new(current)?;
        if count >= MAX_LIST_ELEMENTS {
            return None;
        }
        let code = u32::try_from(cons.head().as_small_int()?).ok()?;
        push_escaped_latin1_char(&mut out, code)?;
        current = cons.tail();
        count += 1;
    }
    out.push('"');
    Some(out)
}

/// Appends `code` to `out` with Erlang string escaping, or returns `None`
/// when the character is not printable latin1 (so the list is not a
/// printable charlist). The printable set and escapes follow
/// `io_lib:printable_latin1_list/1` and `io_lib:write_string/1`.
fn push_escaped_latin1_char(out: &mut String, code: u32) -> Option<()> {
    match code {
        8 => out.push_str("\\b"),
        9 => out.push_str("\\t"),
        10 => out.push_str("\\n"),
        11 => out.push_str("\\v"),
        12 => out.push_str("\\f"),
        13 => out.push_str("\\r"),
        27 => out.push_str("\\e"),
        34 => out.push_str("\\\""),
        92 => out.push_str("\\\\"),
        32..=126 | 160..=255 => out.push(char::from_u32(code)?),
        _ => return None,
    }
    Some(())
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
    let value = crate::term::bigint_math::BigIntValue::from_bigint(bigint);
    // Radix 10 is always valid, so the conversion cannot return `None`.
    crate::term::bigint_convert::to_string_radix(&value, 10).unwrap_or_else(|| "0".to_owned())
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

    /// Builds a proper list of small integers on leaked cells.
    fn int_list(codes: &[i64]) -> Term {
        let mut tail = Term::NIL;
        for code in codes.iter().rev() {
            let cell = Box::leak(Box::new([0_u64; 2]));
            tail = write_cons(cell, Term::small_int(*code), tail).expect("cons fits");
        }
        tail
    }

    // Expected strings verified against OTP 28:
    // `io:format("~p~n", [List])` for each list below.
    #[test]
    fn formats_printable_charlists_as_strings_like_the_otp_shell() {
        let table = AtomTable::with_common_atoms();

        assert_eq!(
            format_term(int_list(&[104, 101, 108, 108, 111]), &table),
            "\"hello\""
        );
        // Escaped control characters keep the list printable.
        assert_eq!(
            format_term(int_list(&[104, 101, 108, 10]), &table),
            "\"hel\\n\""
        );
        assert_eq!(format_term(int_list(&[34, 92]), &table), "\"\\\"\\\\\"");
        assert_eq!(format_term(int_list(&[27]), &table), "\"\\e\"");
        // Printable latin1 above 159 renders as the character itself.
        assert_eq!(format_term(int_list(&[200, 232]), &table), "\"Èè\"");
    }

    #[test]
    fn non_printable_and_improper_lists_keep_element_rendering() {
        let table = AtomTable::with_common_atoms();

        // 7 (BEL) and 0 are outside the printable latin1 set.
        assert_eq!(format_term(int_list(&[7]), &table), "[7]");
        assert_eq!(
            format_term(int_list(&[104, 0, 108]), &table),
            "[104, 0, 108]"
        );

        let cell = Box::leak(Box::new([0_u64; 2]));
        let improper =
            write_cons(cell, Term::small_int(104), Term::small_int(105)).expect("cons fits");
        assert_eq!(format_term(improper, &table), "[104 | 105]");
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
