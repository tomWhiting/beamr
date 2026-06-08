//! ETS match specification parsing and evaluation.
//!
//! This module implements the internal match-spec compiler used by ETS-facing
//! code. It intentionally supports only the initial ETS subset: tuple heads,
//! comparison guards, simple type-test guards, and body construction from
//! variables/literals/tuples/lists plus the `$_` and `$$` special returns.

use std::cmp::Ordering;

use crate::atom::AtomTable;
use crate::native::ProcessContext;
use crate::term::binary_ref::BinaryRef;
use crate::term::boxed::{BigInt, Cons, Tuple};
use crate::term::{Term, compare};

/// Parsed, executable ETS match specification.
#[derive(Clone, Debug)]
pub struct CompiledMatchSpec {
    spec: MatchSpec,
}

/// Structured match specification containing one or more clauses.
#[derive(Clone, Debug)]
pub struct MatchSpec {
    clauses: Vec<MatchClause>,
}

#[derive(Clone, Debug)]
struct MatchClause {
    head: Vec<Pattern>,
    guards: Vec<Guard>,
    body: Vec<BodyExpr>,
    max_variable: usize,
}

#[derive(Clone, Debug)]
enum Pattern {
    Variable(usize),
    DontCare,
    Literal(Term),
    Tuple(Vec<Pattern>),
    List(Vec<Pattern>),
}

#[derive(Clone, Debug)]
enum Guard {
    Compare {
        op: CompareOp,
        left: GuardExpr,
        right: GuardExpr,
    },
    TypeTest {
        test: TypeTest,
        value: GuardExpr,
    },
}

#[derive(Copy, Clone, Debug)]
enum CompareOp {
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
}

#[derive(Copy, Clone, Debug)]
enum TypeTest {
    Atom,
    Integer,
    Binary,
    Tuple,
    List,
}

#[derive(Clone, Debug)]
enum GuardExpr {
    Variable(usize),
    Literal(Term),
}

#[derive(Clone, Debug)]
enum BodyExpr {
    Variable(usize),
    Literal(Term),
    Tuple(Vec<BodyExpr>),
    List(Vec<BodyExpr>),
    ReturnObject,
    ReturnBindings,
}

/// Match-spec parse/compile error.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MatchSpecError {
    BadSpec,
    BadClause,
    BadHead,
    BadGuard,
    BadBody,
    BadList,
    UnknownGuard,
}

trait TermAllocator {
    fn alloc_tuple(&mut self, elements: &[Term]) -> Result<Term, Term>;
    fn alloc_list(&mut self, elements: &[Term]) -> Result<Term, Term>;
}

struct ProcessAllocator<'context, 'process> {
    context: &'context mut ProcessContext<'process>,
}

impl TermAllocator for ProcessAllocator<'_, '_> {
    fn alloc_tuple(&mut self, elements: &[Term]) -> Result<Term, Term> {
        self.context.alloc_tuple(elements)
    }

    fn alloc_list(&mut self, elements: &[Term]) -> Result<Term, Term> {
        self.context.alloc_list(elements)
    }
}

struct LeakingAllocator;

impl TermAllocator for LeakingAllocator {
    fn alloc_tuple(&mut self, elements: &[Term]) -> Result<Term, Term> {
        let leaked = Box::leak(vec![0_u64; 1 + elements.len()].into_boxed_slice());
        crate::term::boxed::write_tuple(leaked, elements)
            .ok_or_else(|| Term::atom(crate::atom::Atom::BADARG))
    }

    fn alloc_list(&mut self, elements: &[Term]) -> Result<Term, Term> {
        let mut tail = Term::NIL;
        for element in elements.iter().rev().copied() {
            let mut words = vec![0_u64; 2].into_boxed_slice();
            tail = crate::term::boxed::write_cons(&mut words, element, tail)
                .ok_or_else(|| Term::atom(crate::atom::Atom::BADARG))?;
            let _leaked: &'static mut [u64] = Box::leak(words);
        }
        Ok(tail)
    }
}

impl CompiledMatchSpec {
    /// Compile a term representation such as `[{MatchHead, Guards, Body}]`.
    pub fn compile(spec: Term, atom_table: &AtomTable) -> Result<Self, MatchSpecError> {
        Ok(Self {
            spec: MatchSpec::parse(spec, atom_table)?,
        })
    }

    /// Evaluate against one object, allocating constructed body terms in the process heap.
    pub fn eval_with_context(
        &self,
        tuple: Term,
        context: &mut ProcessContext<'_>,
    ) -> Result<Option<Term>, Term> {
        let atom_table = context
            .atom_table_arc()
            .ok_or_else(|| Term::atom(crate::atom::Atom::BADARG))?;
        let mut allocator = ProcessAllocator { context };
        self.eval_with_allocator(tuple, atom_table.as_ref(), &mut allocator)
    }

    /// Evaluate against one object using a permanent fallback heap for constructed results.
    ///
    /// Production callers should prefer [`Self::eval_with_context`] so newly
    /// constructed terms live on the calling process heap. This convenience API
    /// exists for direct compiler tests and for bodies that return existing terms.
    #[must_use]
    pub fn eval(&self, tuple: Term) -> Option<Term> {
        let atom_table = AtomTable::with_common_atoms();
        let mut allocator = LeakingAllocator;
        self.eval_with_allocator(tuple, &atom_table, &mut allocator)
            .ok()
            .flatten()
    }

    fn eval_with_allocator(
        &self,
        object: Term,
        atom_table: &AtomTable,
        allocator: &mut dyn TermAllocator,
    ) -> Result<Option<Term>, Term> {
        for clause in &self.spec.clauses {
            let mut bindings = vec![None; clause.max_variable.saturating_add(1)];
            if !clause.matches_head(object, &mut bindings) {
                continue;
            }
            if !clause
                .guards
                .iter()
                .all(|guard| guard.eval(&bindings, atom_table))
            {
                continue;
            }
            return clause.eval_body(object, &bindings, allocator).map(Some);
        }
        Ok(None)
    }
}

impl MatchSpec {
    /// Parse a structured, but not yet executable, match specification.
    pub fn parse(spec: Term, atom_table: &AtomTable) -> Result<Self, MatchSpecError> {
        let clause_terms = proper_list(spec)?;
        if clause_terms.is_empty() {
            return Err(MatchSpecError::BadSpec);
        }

        let mut clauses = Vec::with_capacity(clause_terms.len());
        for clause_term in clause_terms {
            let tuple = Tuple::new(clause_term).ok_or(MatchSpecError::BadClause)?;
            if tuple.arity() != 3 {
                return Err(MatchSpecError::BadClause);
            }
            let head_term = tuple.get(0).ok_or(MatchSpecError::BadClause)?;
            let guards_term = tuple.get(1).ok_or(MatchSpecError::BadClause)?;
            let body_term = tuple.get(2).ok_or(MatchSpecError::BadClause)?;
            clauses.push(MatchClause::parse(
                head_term,
                guards_term,
                body_term,
                atom_table,
            )?);
        }

        Ok(Self { clauses })
    }
}

impl MatchClause {
    fn parse(
        head_term: Term,
        guards_term: Term,
        body_term: Term,
        atom_table: &AtomTable,
    ) -> Result<Self, MatchSpecError> {
        let head_tuple = Tuple::new(head_term).ok_or(MatchSpecError::BadHead)?;
        let mut max_variable = 0;
        let mut head = Vec::with_capacity(head_tuple.arity());
        for index in 0..head_tuple.arity() {
            let element = head_tuple.get(index).ok_or(MatchSpecError::BadHead)?;
            head.push(parse_pattern(element, atom_table, &mut max_variable)?);
        }

        let guard_terms = proper_list(guards_term).map_err(|_| MatchSpecError::BadGuard)?;
        let mut guards = Vec::with_capacity(guard_terms.len());
        for guard_term in guard_terms {
            guards.push(parse_guard(guard_term, atom_table, &mut max_variable)?);
        }

        let body_terms = proper_list(body_term).map_err(|_| MatchSpecError::BadBody)?;
        if body_terms.is_empty() {
            return Err(MatchSpecError::BadBody);
        }
        let mut body = Vec::with_capacity(body_terms.len());
        for expr_term in body_terms {
            body.push(parse_body_expr(expr_term, atom_table, &mut max_variable)?);
        }

        Ok(Self {
            head,
            guards,
            body,
            max_variable,
        })
    }

    fn matches_head(&self, object: Term, bindings: &mut [Option<Term>]) -> bool {
        let Some(tuple) = Tuple::new(object) else {
            return false;
        };
        if tuple.arity() != self.head.len() {
            return false;
        }

        for (index, pattern) in self.head.iter().enumerate() {
            let Some(value) = tuple.get(index) else {
                return false;
            };
            if !match_pattern(pattern, value, bindings) {
                return false;
            }
        }
        true
    }

    fn eval_body(
        &self,
        object: Term,
        bindings: &[Option<Term>],
        allocator: &mut dyn TermAllocator,
    ) -> Result<Term, Term> {
        let mut result = Term::NIL;
        for expr in &self.body {
            result = expr.eval(object, bindings, allocator)?;
        }
        Ok(result)
    }
}

impl Guard {
    fn eval(&self, bindings: &[Option<Term>], atom_table: &AtomTable) -> bool {
        match self {
            Self::Compare { op, left, right } => {
                let Some(left) = left.eval(bindings) else {
                    return false;
                };
                let Some(right) = right.eval(bindings) else {
                    return false;
                };
                match op {
                    CompareOp::Eq => compare::numeric_eq(left, right),
                    CompareOp::Ne => !compare::numeric_eq(left, right),
                    CompareOp::Lt => compare::cmp(left, right, atom_table) == Ordering::Less,
                    CompareOp::Gt => compare::cmp(left, right, atom_table) == Ordering::Greater,
                    CompareOp::Le => compare::cmp(left, right, atom_table) != Ordering::Greater,
                    CompareOp::Ge => compare::cmp(left, right, atom_table) != Ordering::Less,
                }
            }
            Self::TypeTest { test, value } => {
                let Some(value) = value.eval(bindings) else {
                    return false;
                };
                match test {
                    TypeTest::Atom => value.is_atom(),
                    TypeTest::Integer => value.is_small_int() || BigInt::new(value).is_some(),
                    TypeTest::Binary => BinaryRef::new(value).is_some(),
                    TypeTest::Tuple => Tuple::new(value).is_some(),
                    TypeTest::List => value.is_list() || value.is_nil(),
                }
            }
        }
    }
}

impl GuardExpr {
    fn eval(&self, bindings: &[Option<Term>]) -> Option<Term> {
        match self {
            Self::Variable(index) => bindings.get(*index).copied().flatten(),
            Self::Literal(term) => Some(*term),
        }
    }
}

impl BodyExpr {
    fn eval(
        &self,
        object: Term,
        bindings: &[Option<Term>],
        allocator: &mut dyn TermAllocator,
    ) -> Result<Term, Term> {
        match self {
            Self::Variable(index) => bindings.get(*index).copied().flatten().ok_or_else(badarg),
            Self::Literal(term) => Ok(*term),
            Self::Tuple(elements) => {
                let evaluated = eval_body_exprs(elements, object, bindings, allocator)?;
                allocator.alloc_tuple(&evaluated)
            }
            Self::List(elements) => {
                let evaluated = eval_body_exprs(elements, object, bindings, allocator)?;
                allocator.alloc_list(&evaluated)
            }
            Self::ReturnObject => Ok(object),
            Self::ReturnBindings => {
                let mut values = Vec::new();
                for value in bindings.iter().skip(1).copied().flatten() {
                    values.push(value);
                }
                allocator.alloc_list(&values)
            }
        }
    }
}

fn eval_body_exprs(
    exprs: &[BodyExpr],
    object: Term,
    bindings: &[Option<Term>],
    allocator: &mut dyn TermAllocator,
) -> Result<Vec<Term>, Term> {
    let mut evaluated = Vec::with_capacity(exprs.len());
    for expr in exprs {
        evaluated.push(expr.eval(object, bindings, allocator)?);
    }
    Ok(evaluated)
}

fn badarg() -> Term {
    Term::atom(crate::atom::Atom::BADARG)
}

fn match_pattern(pattern: &Pattern, value: Term, bindings: &mut [Option<Term>]) -> bool {
    match pattern {
        Pattern::Variable(index) => {
            let Some(slot) = bindings.get_mut(*index) else {
                return false;
            };
            match *slot {
                Some(bound) => compare::exact_eq(bound, value),
                None => {
                    *slot = Some(value);
                    true
                }
            }
        }
        Pattern::DontCare => true,
        Pattern::Literal(term) => compare::exact_eq(*term, value),
        Pattern::Tuple(elements) => {
            let Some(tuple) = Tuple::new(value) else {
                return false;
            };
            tuple.arity() == elements.len()
                && elements.iter().enumerate().all(|(index, element)| {
                    tuple
                        .get(index)
                        .is_some_and(|value| match_pattern(element, value, bindings))
                })
        }
        Pattern::List(elements) => proper_list(value).is_ok_and(|values| {
            values.len() == elements.len()
                && elements
                    .iter()
                    .zip(values)
                    .all(|(pattern, value)| match_pattern(pattern, value, bindings))
        }),
    }
}

fn parse_pattern(
    term: Term,
    atom_table: &AtomTable,
    max_variable: &mut usize,
) -> Result<Pattern, MatchSpecError> {
    match classify_atom(term, atom_table) {
        AtomClass::Variable(index) => {
            *max_variable = (*max_variable).max(index);
            Ok(Pattern::Variable(index))
        }
        AtomClass::DontCare => Ok(Pattern::DontCare),
        AtomClass::SpecialReturn => Ok(Pattern::Literal(term)),
        AtomClass::Literal => {
            if let Some(tuple) = Tuple::new(term) {
                let mut elements = Vec::with_capacity(tuple.arity());
                for index in 0..tuple.arity() {
                    elements.push(parse_pattern(
                        tuple.get(index).ok_or(MatchSpecError::BadHead)?,
                        atom_table,
                        max_variable,
                    )?);
                }
                Ok(Pattern::Tuple(elements))
            } else if term.is_list() || term.is_nil() {
                let values = proper_list(term).map_err(|_| MatchSpecError::BadHead)?;
                let mut elements = Vec::with_capacity(values.len());
                for value in values {
                    elements.push(parse_pattern(value, atom_table, max_variable)?);
                }
                Ok(Pattern::List(elements))
            } else {
                Ok(Pattern::Literal(term))
            }
        }
    }
}

fn parse_guard(
    term: Term,
    atom_table: &AtomTable,
    max_variable: &mut usize,
) -> Result<Guard, MatchSpecError> {
    let tuple = Tuple::new(term).ok_or(MatchSpecError::BadGuard)?;
    let op_term = tuple.get(0).ok_or(MatchSpecError::BadGuard)?;
    let Some(op_name) = atom_name(op_term, atom_table) else {
        return Err(MatchSpecError::BadGuard);
    };

    if let Some(op) = parse_compare_op(op_name) {
        if tuple.arity() != 3 {
            return Err(MatchSpecError::BadGuard);
        }
        return Ok(Guard::Compare {
            op,
            left: parse_guard_expr(
                tuple.get(1).ok_or(MatchSpecError::BadGuard)?,
                atom_table,
                max_variable,
            )?,
            right: parse_guard_expr(
                tuple.get(2).ok_or(MatchSpecError::BadGuard)?,
                atom_table,
                max_variable,
            )?,
        });
    }

    if let Some(test) = parse_type_test(op_name) {
        if tuple.arity() != 2 {
            return Err(MatchSpecError::BadGuard);
        }
        return Ok(Guard::TypeTest {
            test,
            value: parse_guard_expr(
                tuple.get(1).ok_or(MatchSpecError::BadGuard)?,
                atom_table,
                max_variable,
            )?,
        });
    }

    Err(MatchSpecError::UnknownGuard)
}

fn parse_guard_expr(
    term: Term,
    atom_table: &AtomTable,
    max_variable: &mut usize,
) -> Result<GuardExpr, MatchSpecError> {
    match classify_atom(term, atom_table) {
        AtomClass::Variable(index) => {
            *max_variable = (*max_variable).max(index);
            Ok(GuardExpr::Variable(index))
        }
        AtomClass::DontCare | AtomClass::SpecialReturn => Err(MatchSpecError::BadGuard),
        AtomClass::Literal => Ok(GuardExpr::Literal(term)),
    }
}

fn parse_body_expr(
    term: Term,
    atom_table: &AtomTable,
    max_variable: &mut usize,
) -> Result<BodyExpr, MatchSpecError> {
    match classify_atom(term, atom_table) {
        AtomClass::Variable(index) => {
            *max_variable = (*max_variable).max(index);
            Ok(BodyExpr::Variable(index))
        }
        AtomClass::DontCare => Ok(BodyExpr::Literal(term)),
        AtomClass::SpecialReturn => match atom_name(term, atom_table) {
            Some("$_") => Ok(BodyExpr::ReturnObject),
            Some("$$") => Ok(BodyExpr::ReturnBindings),
            _ => Err(MatchSpecError::BadBody),
        },
        AtomClass::Literal => {
            if let Some(tuple) = Tuple::new(term) {
                let constructor = if tuple.arity() == 1 {
                    Tuple::new(tuple.get(0).ok_or(MatchSpecError::BadBody)?)
                } else {
                    Some(tuple)
                };
                let tuple = constructor.ok_or(MatchSpecError::BadBody)?;
                let mut elements = Vec::with_capacity(tuple.arity());
                for index in 0..tuple.arity() {
                    elements.push(parse_body_expr(
                        tuple.get(index).ok_or(MatchSpecError::BadBody)?,
                        atom_table,
                        max_variable,
                    )?);
                }
                Ok(BodyExpr::Tuple(elements))
            } else if term.is_list() || term.is_nil() {
                let values = proper_list(term).map_err(|_| MatchSpecError::BadBody)?;
                let mut elements = Vec::with_capacity(values.len());
                for value in values {
                    elements.push(parse_body_expr(value, atom_table, max_variable)?);
                }
                Ok(BodyExpr::List(elements))
            } else {
                Ok(BodyExpr::Literal(term))
            }
        }
    }
}

fn parse_compare_op(name: &str) -> Option<CompareOp> {
    match name {
        "==" => Some(CompareOp::Eq),
        "/=" => Some(CompareOp::Ne),
        "<" => Some(CompareOp::Lt),
        ">" => Some(CompareOp::Gt),
        "=<" => Some(CompareOp::Le),
        ">=" => Some(CompareOp::Ge),
        _ => None,
    }
}

fn parse_type_test(name: &str) -> Option<TypeTest> {
    match name {
        "is_atom" => Some(TypeTest::Atom),
        "is_integer" => Some(TypeTest::Integer),
        "is_binary" => Some(TypeTest::Binary),
        "is_tuple" => Some(TypeTest::Tuple),
        "is_list" => Some(TypeTest::List),
        _ => None,
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum AtomClass {
    Variable(usize),
    DontCare,
    SpecialReturn,
    Literal,
}

fn classify_atom(term: Term, atom_table: &AtomTable) -> AtomClass {
    let Some(name) = atom_name(term, atom_table) else {
        return AtomClass::Literal;
    };
    if name == "_" {
        AtomClass::DontCare
    } else if name == "$_" || name == "$$" {
        AtomClass::SpecialReturn
    } else if let Some(index) = parse_variable_name(name) {
        AtomClass::Variable(index)
    } else {
        AtomClass::Literal
    }
}

fn parse_variable_name(name: &str) -> Option<usize> {
    let digits = name.strip_prefix('$')?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let index = digits.parse::<usize>().ok()?;
    (index > 0).then_some(index)
}

fn atom_name(term: Term, atom_table: &AtomTable) -> Option<&str> {
    atom_table.resolve(term.as_atom()?)
}

fn proper_list(term: Term) -> Result<Vec<Term>, MatchSpecError> {
    let mut elements = Vec::new();
    let mut tail = term;
    while !tail.is_nil() {
        let cons = Cons::new(tail).ok_or(MatchSpecError::BadList)?;
        elements.push(cons.head());
        tail = cons.tail();
    }
    Ok(elements)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::atom::Atom;
    use crate::native::ProcessContext;
    use crate::process::Process;
    use crate::term::boxed::{self, Tuple};

    struct TestHeap {
        tuple_words: Vec<Box<[u64]>>,
        cons_words: Vec<Box<[u64]>>,
    }

    impl TestHeap {
        fn new() -> Self {
            Self {
                tuple_words: Vec::new(),
                cons_words: Vec::new(),
            }
        }

        fn tuple(&mut self, elements: &[Term]) -> Term {
            let mut words = vec![0_u64; 1 + elements.len()].into_boxed_slice();
            let term = boxed::write_tuple(&mut words, elements).expect("test tuple fits");
            self.tuple_words.push(words);
            term
        }

        fn list(&mut self, elements: &[Term]) -> Term {
            let mut tail = Term::NIL;
            for element in elements.iter().rev().copied() {
                let mut words = vec![0_u64; 2].into_boxed_slice();
                tail = boxed::write_cons(&mut words, element, tail).expect("test cons fits");
                self.cons_words.push(words);
            }
            tail
        }

        fn dotted_list(&mut self, head: Term, tail: Term) -> Term {
            let mut words = vec![0_u64; 2].into_boxed_slice();
            let term = boxed::write_cons(&mut words, head, tail).expect("test cons fits");
            self.cons_words.push(words);
            term
        }

        fn bigint(&mut self, negative: bool, limbs: &[u64]) -> Term {
            let mut words = vec![0_u64; 3 + limbs.len()].into_boxed_slice();
            let term = boxed::write_bigint(&mut words, negative, limbs).expect("test bigint fits");
            self.tuple_words.push(words);
            term
        }
    }

    fn atom(table: &AtomTable, name: &str) -> Term {
        Term::atom(table.intern(name))
    }

    fn compile_parse_acceptance(heap: &mut TestHeap, table: &AtomTable) -> CompiledMatchSpec {
        let head = heap.tuple(&[atom(table, "$1"), atom(table, "$2")]);
        let guard = heap.tuple(&[atom(table, ">"), atom(table, "$1"), Term::small_int(10)]);
        let guards = heap.list(&[guard]);
        let body = heap.list(&[atom(table, "$2")]);
        let clause = heap.tuple(&[head, guards, body]);
        let spec = heap.list(&[clause]);
        CompiledMatchSpec::compile(spec, table).expect("spec compiles")
    }

    #[test]
    fn parses_and_evaluates_acceptance_example() {
        let table = AtomTable::with_common_atoms();
        let mut heap = TestHeap::new();
        let compiled = compile_parse_acceptance(&mut heap, &table);
        let object = heap.tuple(&[Term::small_int(11), atom(&table, "ok")]);

        assert_eq!(compiled.eval(object), Some(atom(&table, "ok")));
    }

    #[test]
    fn guard_failure_returns_no_match() {
        let table = AtomTable::with_common_atoms();
        let mut heap = TestHeap::new();
        let head = heap.tuple(&[atom(&table, "$1"), atom(&table, "$2")]);
        let guard = heap.tuple(&[atom(&table, ">"), atom(&table, "$1"), Term::small_int(0)]);
        let guards = heap.list(&[guard]);
        let body_tuple = heap.tuple(&[atom(&table, "$2"), atom(&table, "$1")]);
        let body_constructor = heap.tuple(&[body_tuple]);
        let body = heap.list(&[body_constructor]);
        let clause = heap.tuple(&[head, guards, body]);
        let spec = heap.list(&[clause]);
        let compiled = CompiledMatchSpec::compile(spec, &table).expect("spec compiles");
        let object = heap.tuple(&[Term::small_int(-1), atom(&table, "hello")]);

        assert_eq!(compiled.eval(object), None);
    }

    #[test]
    fn body_tuple_constructs_result() {
        let table: Arc<AtomTable> = Arc::new(AtomTable::with_common_atoms());
        let mut heap = TestHeap::new();
        let head = heap.tuple(&[atom(&table, "$1"), atom(&table, "$2")]);
        let guard = heap.tuple(&[atom(&table, ">"), atom(&table, "$1"), Term::small_int(0)]);
        let guards = heap.list(&[guard]);
        let body_tuple = heap.tuple(&[atom(&table, "$2"), atom(&table, "$1")]);
        let body_constructor = heap.tuple(&[body_tuple]);
        let body = heap.list(&[body_constructor]);
        let clause = heap.tuple(&[head, guards, body]);
        let spec = heap.list(&[clause]);
        let compiled = CompiledMatchSpec::compile(spec, &table).expect("spec compiles");
        let object = heap.tuple(&[Term::small_int(5), atom(&table, "hello")]);

        let mut process = Process::new(1, 233);
        let mut context = ProcessContext::new();
        context.attach_process(&mut process, 0);
        context.set_atom_table(Some(Arc::clone(&table)));

        let result = compiled
            .eval_with_context(object, &mut context)
            .expect("evaluation does not raise")
            .expect("object matches");
        let tuple = Tuple::new(result).expect("body result is tuple");
        assert_eq!(tuple.get(0), Some(atom(&table, "hello")));
        assert_eq!(tuple.get(1), Some(Term::small_int(5)));
    }

    #[test]
    fn return_object_special_body_returns_whole_match() {
        let table = AtomTable::with_common_atoms();
        let mut heap = TestHeap::new();
        let head = heap.tuple(&[atom(&table, "$1"), atom(&table, "_")]);
        let guards = heap.list(&[]);
        let body = heap.list(&[atom(&table, "$_")]);
        let clause = heap.tuple(&[head, guards, body]);
        let spec = heap.list(&[clause]);
        let compiled = CompiledMatchSpec::compile(spec, &table).expect("spec compiles");
        let object = heap.tuple(&[Term::small_int(1), atom(&table, "ignored")]);

        assert_eq!(compiled.eval(object), Some(object));
    }

    #[test]
    fn return_bindings_special_body_returns_variables_by_index() {
        let table = AtomTable::with_common_atoms();
        let mut heap = TestHeap::new();
        let head = heap.tuple(&[atom(&table, "$2"), atom(&table, "$1")]);
        let guards = heap.list(&[]);
        let body = heap.list(&[atom(&table, "$$")]);
        let clause = heap.tuple(&[head, guards, body]);
        let spec = heap.list(&[clause]);
        let compiled = CompiledMatchSpec::compile(spec, &table).expect("spec compiles");
        let object = heap.tuple(&[atom(&table, "two"), atom(&table, "one")]);
        let result = compiled.eval(object).expect("object matches");
        let values = proper_list(result).expect("result is proper list");

        assert_eq!(values, vec![atom(&table, "one"), atom(&table, "two")]);
    }

    #[test]
    fn repeated_variable_requires_exact_equality() {
        let table = AtomTable::with_common_atoms();
        let mut heap = TestHeap::new();
        let head = heap.tuple(&[atom(&table, "$1"), atom(&table, "$1")]);
        let guards = heap.list(&[]);
        let body = heap.list(&[atom(&table, "$1")]);
        let clause = heap.tuple(&[head, guards, body]);
        let spec = heap.list(&[clause]);
        let compiled = CompiledMatchSpec::compile(spec, &table).expect("spec compiles");
        let matching = heap.tuple(&[Term::small_int(3), Term::small_int(3)]);
        let non_matching = heap.tuple(&[Term::small_int(3), Term::small_int(4)]);

        assert_eq!(compiled.eval(matching), Some(Term::small_int(3)));
        assert_eq!(compiled.eval(non_matching), None);
    }

    #[test]
    fn is_integer_type_test_accepts_bigints() {
        let table = AtomTable::with_common_atoms();
        let mut heap = TestHeap::new();
        let head = heap.tuple(&[atom(&table, "$1")]);
        let guard = heap.tuple(&[atom(&table, "is_integer"), atom(&table, "$1")]);
        let guards = heap.list(&[guard]);
        let body = heap.list(&[atom(&table, "$1")]);
        let clause = heap.tuple(&[head, guards, body]);
        let spec = heap.list(&[clause]);
        let compiled = CompiledMatchSpec::compile(spec, &table).expect("spec compiles");
        let bigint = heap.bigint(false, &[u64::MAX, 1]);
        let object = heap.tuple(&[bigint]);

        assert_eq!(compiled.eval(object), Some(bigint));
    }

    #[test]
    fn malformed_lists_are_rejected() {
        let table = AtomTable::with_common_atoms();
        let mut heap = TestHeap::new();
        let head = heap.tuple(&[atom(&table, "$1")]);
        let bad_guards = heap.dotted_list(Term::small_int(1), Term::atom(Atom::OK));
        let body = heap.list(&[atom(&table, "$1")]);
        let clause = heap.tuple(&[head, bad_guards, body]);
        let spec = heap.list(&[clause]);

        assert_eq!(
            CompiledMatchSpec::compile(spec, &table).err(),
            Some(MatchSpecError::BadGuard)
        );
    }
}
