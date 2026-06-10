//! Per-module constant pool for decoded BEAM literals.
//!
//! Literal terms are materialised once while a module is loaded. Boxed/list
//! roots point into storage owned by this pool, so repeated literal reads during
//! interpretation do not allocate and do not leak process-independent heap
//! blocks.

use std::fmt;

use crate::atom::AtomTable;
use crate::error::LoadError;
use crate::loader::Literal;
use crate::term::bigint_convert;
use crate::term::bigint_math::BigIntValue;
use crate::term::binary::{packed_word_count, write_binary};
use crate::term::boxed::{
    BoxedHeader, BoxedTag, write_bigint, write_cons, write_export_fun, write_float, write_map,
    write_tuple,
};
use crate::term::{Term, compare};

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum BlockKind {
    Boxed,
    List,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum RootEntry {
    Immediate(Term),
    Boxed { block: usize },
    List { block: usize },
}

/// Module-owned storage for pre-materialised literal terms.
///
/// Each boxed/list literal allocation is stored as one `Box<[u64]>` heap block.
/// Top-level literal indices are represented by `roots`; immediate literals have
/// roots without storage blocks, while nested boxed/list values may add storage
/// blocks that are not exposed as top-level literal indices.
#[derive(Debug, Default)]
pub struct ConstantPool {
    blocks: Vec<Box<[u64]>>,
    block_kinds: Vec<BlockKind>,
    roots: Vec<RootEntry>,
}

impl ConstantPool {
    /// Creates an empty pool.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            blocks: Vec::new(),
            block_kinds: Vec::new(),
            roots: Vec::new(),
        }
    }

    /// Returns the term for top-level literal `index`.
    ///
    /// Boxed/list terms point into this pool's owned blocks and remain valid for
    /// the lifetime of the pool.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<Term> {
        self.roots
            .get(index)
            .and_then(|root| self.term_for_root(*root))
    }

    /// Returns the number of top-level literal roots in the pool.
    #[must_use]
    pub fn len(&self) -> usize {
        self.roots.len()
    }

    /// Returns true when the pool has no top-level literal roots.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    /// Returns the number of owned heap blocks in the pool.
    #[must_use]
    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    fn term_for_root(&self, root: RootEntry) -> Option<Term> {
        match root {
            RootEntry::Immediate(term) => Some(term),
            RootEntry::Boxed { block } => self
                .blocks
                .get(block)
                .map(|words| Term::boxed_ptr(words.as_ptr())),
            RootEntry::List { block } => self
                .blocks
                .get(block)
                .map(|words| Term::list_ptr(words.as_ptr())),
        }
    }

    fn push_root(&self, root: Term) -> Result<RootEntry, LoadError> {
        if root.is_boxed() || root.is_list() {
            let ptr = root.heap_ptr().ok_or_else(|| {
                LoadError::ValidationError("constant pool pointer root has no heap pointer".into())
            })?;
            let block = self
                .blocks
                .iter()
                .position(|words| words.as_ptr() == ptr)
                .ok_or_else(|| {
                    LoadError::ValidationError(
                        "constant pool root does not match owned storage".into(),
                    )
                })?;
            if root.is_boxed() {
                Ok(RootEntry::Boxed { block })
            } else {
                Ok(RootEntry::List { block })
            }
        } else {
            Ok(RootEntry::Immediate(root))
        }
    }

    fn push_block(&mut self, words: Vec<u64>, kind: BlockKind) -> Result<usize, LoadError> {
        if words.is_empty() {
            return Err(LoadError::ValidationError(
                "constant pool heap block cannot be empty".into(),
            ));
        }
        self.blocks.push(words.into_boxed_slice());
        self.block_kinds.push(kind);
        Ok(self.blocks.len() - 1)
    }

    #[cfg(test)]
    fn owns_term(&self, term: Term) -> bool {
        let Some(ptr) = term.heap_ptr() else {
            return false;
        };
        let ptr = ptr as usize;
        let word_size = std::mem::size_of::<u64>();
        self.blocks.iter().any(|block| {
            let start = block.as_ptr() as usize;
            let Some(byte_len) = block.len().checked_mul(word_size) else {
                return false;
            };
            let Some(end) = start.checked_add(byte_len) else {
                return false;
            };
            ptr >= start && ptr < end
        })
    }
}

impl Clone for ConstantPool {
    fn clone(&self) -> Self {
        let mut blocks = self.blocks.to_vec();
        let mappings: Vec<_> = self
            .blocks
            .iter()
            .zip(blocks.iter())
            .map(|(original, cloned)| (original.as_ptr(), cloned.as_ptr(), original.len()))
            .collect();

        for (block, kind) in blocks.iter_mut().zip(self.block_kinds.iter().copied()) {
            rebase_block_terms(block, kind, &mappings);
        }

        Self {
            blocks,
            block_kinds: self.block_kinds.clone(),
            roots: self.roots.clone(),
        }
    }
}

fn rebase_block_terms(
    block: &mut [u64],
    kind: BlockKind,
    mappings: &[(*const u64, *const u64, usize)],
) {
    match kind {
        BlockKind::List => {
            for cell in block.chunks_exact_mut(2) {
                rebase_term_word(&mut cell[0], mappings);
                rebase_term_word(&mut cell[1], mappings);
            }
        }
        BlockKind::Boxed => rebase_boxed_block_terms(block, mappings),
    }
}

fn rebase_boxed_block_terms(block: &mut [u64], mappings: &[(*const u64, *const u64, usize)]) {
    let Some((header, payload)) = block.split_first_mut() else {
        return;
    };
    match BoxedHeader::tag(*header) {
        Some(BoxedTag::Tuple) => {
            for word in payload.iter_mut().take(BoxedHeader::size(*header)) {
                rebase_term_word(word, mappings);
            }
        }
        Some(BoxedTag::Map) => {
            let len = payload.first().copied().unwrap_or(0) as usize;
            for word in payload.iter_mut().skip(1).take(len.saturating_mul(2)) {
                rebase_term_word(word, mappings);
            }
        }
        Some(BoxedTag::Closure) => {
            let num_free = payload.get(3).copied().unwrap_or(0) as usize;
            for word in payload.iter_mut().skip(6).take(num_free) {
                rebase_term_word(word, mappings);
            }
        }
        Some(BoxedTag::MatchContext) => {
            if let Some(word) = payload.get_mut(2) {
                rebase_term_word(word, mappings);
            }
        }
        Some(BoxedTag::SubBinary) => {
            if let Some(word) = payload.first_mut() {
                rebase_term_word(word, mappings);
            }
        }
        Some(
            BoxedTag::Float
            | BoxedTag::BigInt
            | BoxedTag::Reference
            | BoxedTag::Binary
            | BoxedTag::BinaryBuilder
            | BoxedTag::ProcBin
            | BoxedTag::FdResource
            | BoxedTag::ExternalPid
            | BoxedTag::ExternalReference,
        )
        | None => {}
    }
}

fn rebase_term_word(word: &mut u64, mappings: &[(*const u64, *const u64, usize)]) {
    if let Some(rebased) = rebase_pool_pointer(*word, mappings) {
        *word = rebased.raw();
    }
}

fn rebase_pool_pointer(raw: u64, mappings: &[(*const u64, *const u64, usize)]) -> Option<Term> {
    let term = Term::from_raw(raw);
    if !term.is_boxed() && !term.is_list() {
        return None;
    }

    let ptr = term.heap_ptr()? as usize;
    let word_size = std::mem::size_of::<u64>();
    for &(original, cloned, len) in mappings {
        let start = original as usize;
        let byte_len = len.checked_mul(word_size)?;
        let end = start.checked_add(byte_len)?;
        if ptr < start || ptr >= end {
            continue;
        }

        let offset = ptr.checked_sub(start)?;
        if !offset.is_multiple_of(word_size) {
            return None;
        }
        let word_offset = offset / word_size;
        // SAFETY: `word_offset < len` because `ptr` was verified to be inside
        // the original block range. `cloned` is the corresponding cloned block.
        let rebased_ptr = unsafe { cloned.add(word_offset) };
        return Some(if term.is_boxed() {
            Term::boxed_ptr(rebased_ptr)
        } else {
            Term::list_ptr(rebased_ptr)
        });
    }

    None
}

/// Materialises all decoded literals into a module-owned constant pool.
pub fn materialise_literals(
    literals: &[Literal],
    atom_table: Option<&AtomTable>,
) -> Result<ConstantPool, LoadError> {
    let mut pool = ConstantPool::new();
    for literal in literals {
        let root = materialise_literal(&mut pool, literal, atom_table)?;
        pool.roots.push(root);
    }
    Ok(pool)
}

fn materialise_literal(
    pool: &mut ConstantPool,
    literal: &Literal,
    atom_table: Option<&AtomTable>,
) -> Result<RootEntry, LoadError> {
    match literal {
        // i64 literals that cannot live in a small-integer immediate become
        // single-limb bignum boxes, matching the runtime's promote-on-overflow
        // representation so `=:=` against computed values stays canonical.
        Literal::Integer(value) => match Term::try_small_int(*value) {
            Some(term) => Ok(RootEntry::Immediate(term)),
            None => {
                let limbs = [value.unsigned_abs()];
                let block = pool.push_block(vec![0; 3 + limbs.len()], BlockKind::Boxed)?;
                let term = write_bigint(&mut pool.blocks[block], *value < 0, &limbs)
                    .ok_or_else(write_failed)?;
                pool.push_root(term)
            }
        },
        Literal::Float(value) => {
            let block = pool.push_block(vec![0; 2], BlockKind::Boxed)?;
            let term = write_float(&mut pool.blocks[block], *value).ok_or_else(write_failed)?;
            pool.push_root(term)
        }
        Literal::BigInteger(bytes) => {
            let value = bigint_literal_value(bytes)?;
            // Demote non-minimal encodings that fit a small immediate.
            if let Some(term) = value.to_small_i64().and_then(Term::try_small_int) {
                return Ok(RootEntry::Immediate(term));
            }
            let limbs = value.limbs();
            let block = pool.push_block(vec![0; 3 + limbs.len()], BlockKind::Boxed)?;
            let term = write_bigint(&mut pool.blocks[block], value.is_negative(), limbs)
                .ok_or_else(write_failed)?;
            pool.push_root(term)
        }
        Literal::Atom(atom) => Ok(RootEntry::Immediate(Term::atom(*atom))),
        Literal::Binary(bytes) => {
            let block = pool.push_block(
                vec![0; 2 + packed_word_count(bytes.len())],
                BlockKind::Boxed,
            )?;
            let term = write_binary(&mut pool.blocks[block], bytes).ok_or_else(write_failed)?;
            pool.push_root(term)
        }
        // STRING_EXT is a compact wire encoding for a proper list of byte-sized
        // integers, not a binary — it must materialise as cons cells.
        Literal::String(bytes) => {
            if bytes.is_empty() {
                return Ok(RootEntry::Immediate(Term::NIL));
            }
            let block = pool.push_block(vec![0; bytes.len() * 2], BlockKind::List)?;
            let mut result = Term::NIL;
            for (index, byte) in bytes.iter().enumerate().rev() {
                let start = index * 2;
                result = write_cons(
                    &mut pool.blocks[block][start..start + 2],
                    Term::small_int(i64::from(*byte)),
                    result,
                )
                .ok_or_else(write_failed)?;
            }
            pool.push_root(result)
        }
        Literal::Nil => Ok(RootEntry::Immediate(Term::NIL)),
        Literal::ExportFun {
            module,
            function,
            arity,
        } => {
            let block = pool.push_block(vec![0; 7], BlockKind::Boxed)?;
            let term = write_export_fun(&mut pool.blocks[block], *module, *function, *arity)
                .ok_or_else(write_failed)?;
            pool.push_root(term)
        }
        Literal::Tuple(elements) => {
            let terms = materialise_literal_terms(pool, elements, atom_table)?;
            let block = pool.push_block(vec![0; 1 + terms.len()], BlockKind::Boxed)?;
            let term = write_tuple(&mut pool.blocks[block], &terms).ok_or_else(write_failed)?;
            pool.push_root(term)
        }
        Literal::List(elements, tail) => {
            let mut result = materialise_literal_term(pool, tail, atom_table)?;
            if elements.is_empty() {
                return Ok(RootEntry::Immediate(result));
            }
            let block = pool.push_block(vec![0; elements.len() * 2], BlockKind::List)?;
            for (index, element) in elements.iter().enumerate().rev() {
                let head = materialise_literal_term(pool, element, atom_table)?;
                let start = index * 2;
                result = write_cons(&mut pool.blocks[block][start..start + 2], head, result)
                    .ok_or_else(write_failed)?;
            }
            pool.push_root(result)
        }
        Literal::Map(entries) => {
            let mut pairs = Vec::with_capacity(entries.len());
            for (key, value) in entries {
                pairs.push((
                    materialise_literal_term(pool, key, atom_table)?,
                    materialise_literal_term(pool, value, atom_table)?,
                ));
            }
            pairs.sort_by(|(left, _), (right, _)| {
                atom_table.map_or_else(
                    || compare::raw_cmp(*left, *right),
                    |table| compare::cmp(*left, *right, table),
                )
            });
            let keys: Vec<_> = pairs.iter().map(|(key, _)| *key).collect();
            let values: Vec<_> = pairs.iter().map(|(_, value)| *value).collect();
            let block =
                pool.push_block(vec![0; 2 + keys.len() + values.len()], BlockKind::Boxed)?;
            let term =
                write_map(&mut pool.blocks[block], &keys, &values).ok_or_else(write_failed)?;
            pool.push_root(term)
        }
    }
}

fn materialise_literal_terms(
    pool: &mut ConstantPool,
    literals: &[Literal],
    atom_table: Option<&AtomTable>,
) -> Result<Vec<Term>, LoadError> {
    let mut terms = Vec::with_capacity(literals.len());
    for literal in literals {
        terms.push(materialise_literal_term(pool, literal, atom_table)?);
    }
    Ok(terms)
}

fn materialise_literal_term(
    pool: &mut ConstantPool,
    literal: &Literal,
    atom_table: Option<&AtomTable>,
) -> Result<Term, LoadError> {
    let root = materialise_literal(pool, literal, atom_table)?;
    pool.term_for_root(root).ok_or_else(|| {
        LoadError::ValidationError("constant pool root does not resolve to a term".into())
    })
}

/// Parses a decoded big-integer literal: one sign byte (0 positive,
/// 1 negative) followed by little-endian magnitude bytes.
fn bigint_literal_value(bytes: &[u8]) -> Result<BigIntValue, LoadError> {
    let (sign, magnitude) = bytes.split_first().ok_or_else(|| {
        LoadError::ValidationError("big integer literal is missing its sign byte".into())
    })?;
    let negative = match sign {
        0 => false,
        1 => true,
        other => {
            return Err(LoadError::ValidationError(format!(
                "big integer literal has invalid sign byte {other}"
            )));
        }
    };
    Ok(bigint_convert::from_sign_magnitude_le(negative, magnitude))
}

fn write_failed() -> LoadError {
    LoadError::ValidationError("constant pool heap writer failed".into())
}

impl fmt::Display for ConstantPool {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "ConstantPool {{ literals: {}, blocks: {} }}",
            self.roots.len(),
            self.blocks.len()
        )
    }
}

#[cfg(test)]
mod tests;
