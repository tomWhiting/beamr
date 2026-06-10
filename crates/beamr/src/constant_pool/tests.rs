use crate::atom::AtomTable;
use crate::error::LoadError;
use crate::loader::Literal;
use crate::term::binary::Binary;
use crate::term::boxed::{BigInt, Cons, Float, Map, Tuple};
use crate::term::{Term, compare};

use super::materialise_literals;

#[test]
fn string_literal_materialises_as_proper_list_of_small_ints() {
    let literals = vec![Literal::String(vec![1, 2, 3])];
    let pool = materialise_literals(&literals, None).expect("pool");

    let mut current = pool.get(0).expect("string literal");
    let mut elements = Vec::new();
    while !current.is_nil() {
        let cons = Cons::new(current).expect("string literal must be cons cells");
        elements.push(cons.head().as_small_int().expect("byte element"));
        current = cons.tail();
    }
    assert_eq!(elements, vec![1, 2, 3]);
}

#[test]
fn empty_string_literal_materialises_as_nil() {
    let literals = vec![Literal::String(Vec::new())];
    let pool = materialise_literals(&literals, None).expect("pool");
    assert!(pool.get(0).expect("empty string literal").is_nil());
}

#[test]
fn export_fun_literal_materialises_as_callable_closure() {
    let atoms = AtomTable::new();
    let module = atoms.intern("erlang");
    let function = atoms.intern("integer_to_binary");
    let literals = vec![Literal::ExportFun {
        module,
        function,
        arity: 1,
    }];
    let pool = materialise_literals(&literals, None).expect("pool");

    let term = pool.get(0).expect("export fun literal");
    let closure = crate::term::boxed::Closure::new(term).expect("closure layout");
    assert!(closure.is_export());
    assert_eq!(closure.module(), Some(module));
    assert_eq!(closure.export_function(), Some(function));
    assert_eq!(closure.arity(), 1);
    assert_eq!(closure.num_free(), 0);
}

#[test]
fn materialises_literal_storage_and_returns_stable_roots() {
    let atoms = AtomTable::new();
    let key = atoms.intern("key");
    let value = atoms.intern("value");
    let literals = vec![
        Literal::Float(1.5),
        Literal::Tuple(vec![Literal::Integer(7), Literal::Atom(value)]),
        Literal::List(
            vec![Literal::Integer(1), Literal::Integer(2)],
            Box::new(Literal::Nil),
        ),
        Literal::Binary(b"bin".to_vec()),
        Literal::Map(vec![(
            Literal::Atom(key),
            Literal::String(b"bytes".to_vec()),
        )]),
    ];

    let pool = materialise_literals(&literals, Some(&atoms)).expect("pool");
    assert_eq!(pool.len(), literals.len());
    assert!(pool.block_count() >= literals.len());

    assert_eq!(
        Float::new(pool.get(0).expect("float")).map(|f| f.value()),
        Some(1.5)
    );
    assert_eq!(
        Tuple::new(pool.get(1).expect("tuple")).map(|t| t.arity()),
        Some(2)
    );
    assert!(Cons::new(pool.get(2).expect("list")).is_some());
    assert_eq!(
        Binary::new(pool.get(3).expect("binary")).map(|b| b.as_bytes()),
        Some(&b"bin"[..])
    );
    assert_eq!(
        Map::new(pool.get(4).expect("map")).map(|m| m.len()),
        Some(1)
    );
}

#[test]
fn repeated_get_returns_the_same_pointer() {
    let literals = vec![Literal::Tuple(vec![Literal::Integer(42)])];
    let pool = materialise_literals(&literals, None).expect("pool");

    let first = pool.get(0).expect("first");
    let second = pool.get(0).expect("second");

    assert_eq!(first.raw(), second.raw());
    assert_eq!(first.heap_ptr(), second.heap_ptr());
}

#[test]
fn cloned_pool_rebases_all_nested_pointers_to_cloned_blocks() {
    let literals = vec![Literal::Tuple(vec![
        Literal::List(
            vec![Literal::Tuple(vec![Literal::Integer(1)])],
            Box::new(Literal::Nil),
        ),
        Literal::Map(vec![
            (
                Literal::Tuple(vec![Literal::Integer(2)]),
                Literal::String(b"value".to_vec()),
            ),
            (
                Literal::Integer(3),
                Literal::Tuple(vec![Literal::Integer(4)]),
            ),
        ]),
    ])];
    let pool = materialise_literals(&literals, None).expect("pool");
    let cloned = pool.clone();
    let original = pool.get(0).expect("original tuple");
    let copied = cloned.get(0).expect("cloned tuple");

    assert_ne!(original.heap_ptr(), copied.heap_ptr());
    let tuple = Tuple::new(copied).expect("cloned tuple view");
    let list = tuple.get(0).expect("list element");
    let cons = Cons::new(list).expect("cloned cons");
    let nested_tuple = cons.head();
    assert_ne!(
        nested_tuple.heap_ptr(),
        pool.get(0).expect("original").heap_ptr()
    );
    assert!(Tuple::new(nested_tuple).is_some());
    assert!(cloned.owns_term(nested_tuple));
    assert!(cloned.owns_term(cons.tail()) || cons.tail().is_nil());

    let map = Map::new(tuple.get(1).expect("map element")).expect("cloned map");
    for index in 0..map.len() {
        let key = map.key(index).expect("map key");
        let value = map.value(index).expect("map value");
        assert!(cloned.owns_term(key) || !key.is_boxed() && !key.is_list());
        assert!(cloned.owns_term(value) || !value.is_boxed() && !value.is_list());
    }
}

#[test]
fn map_literals_are_sorted_with_atom_table_order() {
    let atoms = AtomTable::new();
    let b = atoms.intern("b");
    let a = atoms.intern("a");
    let literals = vec![Literal::Map(vec![
        (Literal::Atom(b), Literal::Integer(2)),
        (Literal::Atom(a), Literal::Integer(1)),
    ])];

    let pool = materialise_literals(&literals, Some(&atoms)).expect("pool");
    let map = Map::new(pool.get(0).expect("map")).expect("map view");
    let first_key = map.key(0).expect("first key");
    assert!(compare::cmp(first_key, Term::atom(b), &atoms).is_lt());
}

#[test]
fn big_integer_literals_materialise_with_sign_and_limbs() {
    // 10^20 in sign+little-endian-magnitude form, positive then negative.
    let magnitude = 100_000_000_000_000_000_000_u128.to_le_bytes()[..9].to_vec();
    let mut positive = vec![0_u8];
    positive.extend_from_slice(&magnitude);
    let mut negative = vec![1_u8];
    negative.extend_from_slice(&magnitude);
    let literals = vec![Literal::BigInteger(positive), Literal::BigInteger(negative)];

    let pool = materialise_literals(&literals, None).expect("pool");
    let expected = [
        100_000_000_000_000_000_000_u128 as u64,
        (100_000_000_000_000_000_000_u128 >> 64) as u64,
    ];
    let positive = BigInt::new(pool.get(0).expect("positive")).expect("bigint box");
    assert!(!positive.is_negative());
    assert_eq!(positive.limbs(), expected);
    let negative = BigInt::new(pool.get(1).expect("negative")).expect("bigint box");
    assert!(negative.is_negative());
    assert_eq!(negative.limbs(), expected);
}

#[test]
fn big_integer_literal_in_small_range_demotes_to_immediate() {
    let literals = vec![Literal::BigInteger(vec![1, 42, 0, 0, 0, 0, 0, 0, 0, 0])];
    let pool = materialise_literals(&literals, None).expect("pool");
    assert_eq!(pool.get(0), Some(Term::small_int(-42)));
}

#[test]
fn big_integer_literal_with_invalid_sign_is_rejected() {
    for bad in [Literal::BigInteger(vec![2, 1]), Literal::BigInteger(vec![])] {
        assert!(matches!(
            materialise_literals(&[bad], None),
            Err(LoadError::ValidationError(_))
        ));
    }
}

#[test]
fn integer_literal_beyond_small_range_materialises_as_bignum_box() {
    let positive_value = Term::SMALL_INT_MAX + 1;
    let negative_value = Term::SMALL_INT_MIN - 1;
    let literals = vec![
        Literal::Integer(positive_value),
        Literal::Integer(negative_value),
        // -(SMALL_INT_MAX + 1) is exactly SMALL_INT_MIN: still immediate.
        Literal::Integer(-positive_value),
    ];
    let pool = materialise_literals(&literals, None).expect("pool");

    let positive = BigInt::new(pool.get(0).expect("positive")).expect("bigint box");
    assert!(!positive.is_negative());
    assert_eq!(positive.limbs(), [positive_value.unsigned_abs()]);
    let negative = BigInt::new(pool.get(1).expect("negative")).expect("bigint box");
    assert!(negative.is_negative());
    assert_eq!(negative.limbs(), [negative_value.unsigned_abs()]);
    assert_eq!(pool.get(2), Some(Term::small_int(Term::SMALL_INT_MIN)));
}
