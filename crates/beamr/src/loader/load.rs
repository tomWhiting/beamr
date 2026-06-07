use std::collections::HashMap;
use std::fmt;
use std::hash::Hasher;
use std::sync::Arc;

use crate::atom::{Atom, AtomTable};
use crate::error::LoadError;
use crate::module::{Module, ModuleRegistry, ResolvedImport, ResolvedImportTarget};
use crate::native::BifRegistry;

use super::decode::budget::DecodeBudget;
use super::decode::{
    ExportEntry, ImportEntry, Instruction, LambdaEntry, LineInfo, Literal, decode_atom_chunk,
    decode_code_chunk, decode_export_chunk, decode_import_chunk, decode_lambda_chunk,
    decode_line_chunk, decode_literal_chunk, decode_string_chunk,
};
use super::parser::parse_beam_chunks;
use super::validate::validate_module;

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedModule {
    pub name: Atom,
    pub atoms: Vec<Atom>,
    pub instructions: Vec<Instruction>,
    pub imports: Vec<ImportEntry>,
    pub exports: Vec<ExportEntry>,
    pub lambdas: Vec<LambdaEntry>,
    pub literals: Vec<Literal>,
    pub string_table: Vec<u8>,
    pub line_info: Vec<LineInfo>,
}

/// One unresolved import produced by loader import resolution.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct UnresolvedImportEntry {
    /// Imported module atom.
    pub module: Atom,
    /// Imported function atom.
    pub function: Atom,
    /// Imported arity.
    pub arity: u8,
}

impl UnresolvedImportEntry {
    /// Creates an unresolved import entry.
    #[must_use]
    pub const fn new(module: Atom, function: Atom, arity: u8) -> Self {
        Self {
            module,
            function,
            arity,
        }
    }
}

/// Backward-compatible alias for native coverage helpers.
pub type UnresolvedImport = UnresolvedImportEntry;

/// Unresolved imports grouped by imported module atom.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UnresolvedImportReport {
    entries_by_module: HashMap<Atom, Vec<UnresolvedImportEntry>>,
    deferred_by_module: HashMap<Atom, Vec<UnresolvedImportEntry>>,
}

impl UnresolvedImportReport {
    /// Creates a grouped report from unresolved import entries.
    #[must_use]
    pub fn new(entries: Vec<UnresolvedImportEntry>) -> Self {
        Self::with_deferred(entries, Vec::new())
    }

    /// Creates a grouped report from unresolved and deferred import entries.
    #[must_use]
    pub fn with_deferred(
        entries: Vec<UnresolvedImportEntry>,
        deferred: Vec<UnresolvedImportEntry>,
    ) -> Self {
        let mut report = Self::default();
        for entry in entries {
            report.push(entry);
        }
        for entry in deferred {
            report.push_deferred(entry);
        }
        report
    }

    /// Returns true when no imports are truly unresolved.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries_by_module.values().all(Vec::is_empty)
    }

    /// Returns true when at least one import was deferred until call time.
    #[must_use]
    pub fn has_deferred(&self) -> bool {
        self.deferred_by_module
            .values()
            .any(|entries| !entries.is_empty())
    }

    /// Returns the grouped unresolved entries keyed by module atom.
    #[must_use]
    pub fn entries_by_module(&self) -> &HashMap<Atom, Vec<UnresolvedImportEntry>> {
        &self.entries_by_module
    }

    /// Returns unresolved entries for one imported module.
    #[must_use]
    pub fn entries_for(&self, module: Atom) -> &[UnresolvedImportEntry] {
        self.entries_by_module
            .get(&module)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Returns all unresolved import entries in deterministic module-bucket order.
    #[must_use]
    pub fn imports(&self) -> Vec<UnresolvedImportEntry> {
        let mut modules: Vec<_> = self.entries_by_module.keys().copied().collect();
        modules.sort_by_key(|atom| atom.index());
        modules
            .into_iter()
            .flat_map(|module| self.entries_for(module).iter().copied())
            .collect()
    }

    /// Returns the grouped deferred entries keyed by module atom.
    #[must_use]
    pub fn deferred_by_module(&self) -> &HashMap<Atom, Vec<UnresolvedImportEntry>> {
        &self.deferred_by_module
    }

    /// Returns deferred entries for one imported module.
    #[must_use]
    pub fn deferred_for(&self, module: Atom) -> &[UnresolvedImportEntry] {
        self.deferred_by_module
            .get(&module)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Returns all deferred import entries in deterministic module-bucket order.
    #[must_use]
    pub fn deferred_imports(&self) -> Vec<UnresolvedImportEntry> {
        let mut modules: Vec<_> = self.deferred_by_module.keys().copied().collect();
        modules.sort_by_key(|atom| atom.index());
        modules
            .into_iter()
            .flat_map(|module| self.deferred_for(module).iter().copied())
            .collect()
    }

    fn push(&mut self, entry: UnresolvedImportEntry) {
        self.entries_by_module
            .entry(entry.module)
            .or_default()
            .push(entry);
    }

    fn push_deferred(&mut self, entry: UnresolvedImportEntry) {
        self.deferred_by_module
            .entry(entry.module)
            .or_default()
            .push(entry);
    }
}

impl fmt::Display for UnresolvedImportReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_empty() && !self.has_deferred() {
            return formatter.write_str("no unresolved imports");
        }

        let mut modules: Vec<_> = self.entries_by_module.keys().copied().collect();
        modules.sort_by_key(|atom| atom.index());
        for (module_index, module) in modules.iter().copied().enumerate() {
            if module_index > 0 {
                formatter.write_str("; ")?;
            }
            write!(formatter, "{module:?}: ")?;
            for (entry_index, entry) in self.entries_for(module).iter().enumerate() {
                if entry_index > 0 {
                    formatter.write_str(", ")?;
                }
                write!(formatter, "{:?}/{}", entry.function, entry.arity)?;
            }
        }
        if !self.deferred_by_module.is_empty() {
            if !modules.is_empty() {
                formatter.write_str("; ")?;
            }
            formatter.write_str("deferred: ")?;
            let mut deferred_modules: Vec<_> = self.deferred_by_module.keys().copied().collect();
            deferred_modules.sort_by_key(|atom| atom.index());
            for (module_index, module) in deferred_modules.into_iter().enumerate() {
                if module_index > 0 {
                    formatter.write_str("; ")?;
                }
                write!(formatter, "{module:?}: ")?;
                for (entry_index, entry) in self.deferred_for(module).iter().enumerate() {
                    if entry_index > 0 {
                        formatter.write_str(", ")?;
                    }
                    write!(formatter, "{:?}/{}", entry.function, entry.arity)?;
                }
            }
        }
        Ok(())
    }
}

pub fn load_beam_chunks(bytes: &[u8], atom_table: &AtomTable) -> Result<ParsedModule, LoadError> {
    let chunks = parse_beam_chunks(bytes)?;

    let mut budget = DecodeBudget::default();

    let atom_chunk = find_chunk(&chunks, b"AtU8")
        .or_else(|| find_chunk(&chunks, b"Atom"))
        .ok_or_else(|| LoadError::MissingChunk("Atom/AtU8".into()))?;
    let atoms = decode_atom_chunk(atom_chunk, atom_table, &mut budget)?;
    let name = atoms
        .first()
        .copied()
        .ok_or_else(|| LoadError::DecodeError("atom chunk is empty".into()))?;

    let literals = match find_chunk(&chunks, b"LitT") {
        Some(bytes) => decode_literal_chunk(bytes, atom_table, &mut budget)?,
        None => Vec::new(),
    };

    let code_chunk =
        find_chunk(&chunks, b"Code").ok_or_else(|| LoadError::MissingChunk("Code".into()))?;
    let instructions = decode_code_chunk(code_chunk, &atoms, &literals)?;

    let imports = match find_chunk(&chunks, b"ImpT") {
        Some(bytes) => decode_import_chunk(bytes, &atoms, &mut budget)?,
        None => Vec::new(),
    };
    let exports = match find_chunk(&chunks, b"ExpT") {
        Some(bytes) => decode_export_chunk(bytes, &atoms, &mut budget)?,
        None => Vec::new(),
    };
    let lambdas = match find_chunk(&chunks, b"FunT") {
        Some(bytes) => assign_lambda_unique_ids(
            name,
            decode_lambda_chunk(bytes, &atoms, &mut budget)?,
            atom_table,
        )?,
        None => Vec::new(),
    };
    let string_table = find_chunk(&chunks, b"StrT")
        .map(decode_string_chunk)
        .unwrap_or_default();
    let line_info = match find_chunk(&chunks, b"Line") {
        Some(bytes) => decode_line_chunk(bytes, &mut budget)?,
        None => Vec::new(),
    };

    Ok(ParsedModule {
        name,
        atoms,
        instructions,
        imports,
        exports,
        lambdas,
        literals,
        string_table,
        line_info,
    })
}

/// Parses, resolves, validates, registers, and returns a BEAM module.
pub fn load_module(
    bytes: &[u8],
    atom_table: &AtomTable,
    module_registry: &ModuleRegistry,
    bif_registry: &impl BifRegistry,
) -> Result<(Arc<Module>, UnresolvedImportReport), LoadError> {
    let (module, report) = prepare_module(bytes, atom_table, module_registry, bif_registry)?;
    let module = module_registry.insert(module);
    Ok((module, report))
}

/// Parses, resolves, and validates a BEAM module without registering it.
pub fn prepare_module(
    bytes: &[u8],
    atom_table: &AtomTable,
    module_registry: &ModuleRegistry,
    bif_registry: &impl BifRegistry,
) -> Result<(Module, UnresolvedImportReport), LoadError> {
    let parsed = load_beam_chunks(bytes, atom_table)?;
    let (resolved_by_index, report) = resolve_imports(&parsed, module_registry, bif_registry);
    validate_module(&parsed, &resolved_by_index)?;
    let module = module_from_parsed(parsed, resolved_by_index.into_iter().flatten().collect());
    Ok((module, report))
}

fn resolve_imports(
    parsed: &ParsedModule,
    module_registry: &ModuleRegistry,
    bif_registry: &impl BifRegistry,
) -> (Vec<Option<ResolvedImport>>, UnresolvedImportReport) {
    let mut resolved = Vec::with_capacity(parsed.imports.len());
    let mut unresolved = Vec::new();
    let mut deferred = Vec::new();

    for import in &parsed.imports {
        if let Some(entry) = bif_registry.lookup(import.module, import.function, import.arity) {
            resolved.push(Some(ResolvedImport {
                module: import.module,
                function: import.function,
                arity: import.arity,
                target: ResolvedImportTarget::Native(entry),
            }));
            continue;
        }

        let Some(module) = module_registry.lookup(import.module) else {
            deferred.push(UnresolvedImportEntry::new(
                import.module,
                import.function,
                import.arity,
            ));
            resolved.push(Some(ResolvedImport {
                module: import.module,
                function: import.function,
                arity: import.arity,
                target: ResolvedImportTarget::Deferred {
                    module: import.module,
                    function: import.function,
                    arity: import.arity,
                },
            }));
            continue;
        };

        match module
            .exports
            .get(&(import.function, import.arity))
            .copied()
        {
            Some(label) => {
                resolved.push(Some(ResolvedImport {
                    module: import.module,
                    function: import.function,
                    arity: import.arity,
                    target: ResolvedImportTarget::Code {
                        module: import.module,
                        label,
                    },
                }));
            }
            None => {
                unresolved.push(UnresolvedImportEntry::new(
                    import.module,
                    import.function,
                    import.arity,
                ));
                resolved.push(Some(ResolvedImport {
                    module: import.module,
                    function: import.function,
                    arity: import.arity,
                    target: ResolvedImportTarget::Unresolved {
                        module: import.module,
                        function: import.function,
                        arity: import.arity,
                    },
                }));
            }
        }
    }

    (
        resolved,
        UnresolvedImportReport::with_deferred(unresolved, deferred),
    )
}

fn module_from_parsed(parsed: ParsedModule, resolved_imports: Vec<ResolvedImport>) -> Module {
    let exports = parsed
        .exports
        .into_iter()
        .map(|export| ((export.function, export.arity), export.label))
        .collect();
    let label_index = parsed
        .instructions
        .iter()
        .enumerate()
        .filter_map(|(ip, instruction)| match instruction {
            Instruction::Label { label } => Some((*label, ip)),
            _ => None,
        })
        .collect();

    Module {
        name: parsed.name,
        generation: 0,
        exports,
        label_index,
        code: parsed.instructions,
        literals: parsed.literals,
        resolved_imports,
        lambdas: parsed.lambdas,
        string_table: parsed.string_table,
        line_info: parsed.line_info,
    }
}

fn assign_lambda_unique_ids(
    module_name: Atom,
    mut lambdas: Vec<LambdaEntry>,
    atom_table: &AtomTable,
) -> Result<Vec<LambdaEntry>, LoadError> {
    for lambda in &mut lambdas {
        lambda.unique_id = lambda_unique_id(
            atom_table,
            module_name,
            lambda.function,
            lambda.arity,
            lambda.num_free,
        )?;
    }
    Ok(lambdas)
}

/// Computes a deterministic lambda identifier for hot-code closure resolution.
///
/// This intentionally hashes the module and function names instead of atom
/// indices so the value survives recompilation with different atom-table
/// population order. The tuple `(module, function, arity, num_free)` is unique
/// for generated Gleam closures in practice; Erlang modules that generate two
/// closures with the same tuple collide and require positional disambiguation
/// that this loader does not retain.
pub fn lambda_unique_id(
    atom_table: &AtomTable,
    module_name: Atom,
    function_name: Atom,
    arity: u8,
    num_free: u32,
) -> Result<u64, LoadError> {
    let module_name = atom_table
        .resolve(module_name)
        .ok_or_else(|| LoadError::DecodeError("module atom is not interned".into()))?;
    let function_name = atom_table
        .resolve(function_name)
        .ok_or_else(|| LoadError::DecodeError("lambda function atom is not interned".into()))?;
    let mut hasher = DeterministicHasher::default();
    hash_bytes_with_len(&mut hasher, module_name.as_bytes());
    hash_bytes_with_len(&mut hasher, function_name.as_bytes());
    hasher.write(&[arity]);
    hasher.write(&num_free.to_be_bytes());
    Ok(hasher.finish())
}

fn hash_bytes_with_len(hasher: &mut DeterministicHasher, bytes: &[u8]) {
    hasher.write(&bytes.len().to_be_bytes());
    hasher.write(bytes);
}

struct DeterministicHasher(u64);

impl Default for DeterministicHasher {
    fn default() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl Hasher for DeterministicHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        let mut hash = self.0;
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        self.0 = hash;
    }
}

fn find_chunk<'a>(chunks: &'a [([u8; 4], &'a [u8])], tag: &[u8; 4]) -> Option<&'a [u8]> {
    chunks
        .iter()
        .find_map(|(chunk_tag, bytes)| (chunk_tag == tag).then_some(*bytes))
}

#[cfg(test)]
mod tests {
    use crate::atom::{Atom, AtomTable};
    use crate::error::LoadError;
    use crate::loader::load_beam_chunks;
    use crate::module::{Module, ModuleRegistry, ResolvedImportTarget};
    use crate::native::{BifRegistry, NativeEntry, ProcessContext};
    use crate::term::Term;

    use super::{UnresolvedImportEntry, UnresolvedImportReport, load_module};

    struct EmptyBifs;

    impl BifRegistry for EmptyBifs {
        fn lookup(&self, _module: Atom, _function: Atom, _arity: u8) -> Option<NativeEntry> {
            None
        }
    }

    struct OneBif {
        module: Atom,
        function: Atom,
        arity: u8,
    }

    impl BifRegistry for OneBif {
        fn lookup(&self, module: Atom, function: Atom, arity: u8) -> Option<NativeEntry> {
            (module == self.module && function == self.function && arity == self.arity).then_some(
                NativeEntry {
                    function: native_ok,
                    is_dirty: false,
                },
            )
        }
    }

    fn native_ok(_args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
        Ok(Term::small_int(0))
    }

    #[test]
    fn unresolved_report_groups_by_module_and_displays() {
        let atoms = AtomTable::new();
        let erlang = atoms.intern("erlang");
        let unknown = atoms.intern("unknown");
        let report =
            UnresolvedImportReport::new(vec![UnresolvedImportEntry::new(erlang, unknown, 0)]);

        assert_eq!(report.entries_for(erlang).len(), 1);
        assert!(report.entries_by_module().contains_key(&erlang));
        assert!(report.deferred_for(erlang).is_empty());
        assert!(report.to_string().contains("/0"));
    }

    #[test]
    fn load_module_rejects_garbage_bytes_as_invalid_format() {
        let atoms = AtomTable::new();
        let registry = ModuleRegistry::new();

        assert_eq!(
            load_module(b"garbage", &atoms, &registry, &EmptyBifs).map(|_| ()),
            Err(LoadError::InvalidFormat)
        );
    }

    #[test]
    fn fixture_load_registers_module_with_nonfatal_unresolved_imports() {
        let atoms = AtomTable::new();
        let registry = ModuleRegistry::new();
        let bytes = include_bytes!("../../tests/fixtures/hello.beam");

        let (module, report) =
            load_module(bytes, &atoms, &registry, &EmptyBifs).expect("fixture should load");

        assert!(std::sync::Arc::ptr_eq(
            &registry.lookup(module.name).expect("registered module"),
            &module
        ));
        for (ip, instruction) in module.code.iter().enumerate() {
            if let crate::loader::Instruction::Label { label } = instruction {
                assert_eq!(module.label_index.get(label).copied(), Some(ip));
            }
        }
        assert!(!report.deferred_imports().is_empty());
        assert!(report.is_empty());
    }

    #[test]
    fn resolved_cross_module_import_points_to_code_label() {
        let atoms = AtomTable::new();
        let callee = atoms.intern("callee");
        let function = atoms.intern("run");
        let registry = ModuleRegistry::new();
        let mut target = Module {
            name: callee,
            generation: 0,
            exports: std::collections::HashMap::new(),
            label_index: std::collections::HashMap::new(),
            code: Vec::new(),
            literals: Vec::new(),
            resolved_imports: Vec::new(),
            lambdas: Vec::new(),
            string_table: Vec::new(),
            line_info: Vec::new(),
        };
        target.exports.insert((function, 0), 42);
        registry.insert(target);
        let mut parsed =
            load_beam_chunks(include_bytes!("../../tests/fixtures/hello.beam"), &atoms)
                .expect("fixture parses");
        parsed.imports = vec![crate::loader::ImportEntry {
            module: callee,
            function,
            arity: 0,
        }];

        let (resolved, report) = super::resolve_imports(&parsed, &registry, &EmptyBifs);

        assert!(report.is_empty());
        assert!(report.deferred_imports().is_empty());
        assert!(matches!(
            resolved
                .first()
                .and_then(|entry| entry.as_ref())
                .map(|entry| entry.target),
            Some(ResolvedImportTarget::Code { label: 42, .. })
        ));
    }

    #[test]
    fn resolved_bif_import_points_to_native_entry() {
        let atoms = AtomTable::new();
        let erlang = atoms.intern("erlang");
        let now = atoms.intern("now");
        let registry = ModuleRegistry::new();
        let mut parsed =
            load_beam_chunks(include_bytes!("../../tests/fixtures/hello.beam"), &atoms)
                .expect("fixture parses");
        parsed.imports = vec![crate::loader::ImportEntry {
            module: erlang,
            function: now,
            arity: 0,
        }];

        let (resolved, report) = super::resolve_imports(
            &parsed,
            &registry,
            &OneBif {
                module: erlang,
                function: now,
                arity: 0,
            },
        );

        assert!(report.is_empty());
        assert!(matches!(
            resolved
                .first()
                .and_then(|entry| entry.as_ref())
                .map(|entry| entry.target),
            Some(ResolvedImportTarget::Native(_))
        ));
    }

    #[test]
    fn missing_loaded_export_is_unresolved_not_deferred() {
        let atoms = AtomTable::new();
        let callee = atoms.intern("callee");
        let function = atoms.intern("run");
        let registry = ModuleRegistry::new();
        registry.insert(Module {
            name: callee,
            generation: 0,
            exports: std::collections::HashMap::new(),
            label_index: std::collections::HashMap::new(),
            code: Vec::new(),
            literals: Vec::new(),
            resolved_imports: Vec::new(),
            lambdas: Vec::new(),
            string_table: Vec::new(),
            line_info: Vec::new(),
        });
        let mut parsed =
            load_beam_chunks(include_bytes!("../../tests/fixtures/hello.beam"), &atoms)
                .expect("fixture parses");
        parsed.imports = vec![crate::loader::ImportEntry {
            module: callee,
            function,
            arity: 0,
        }];

        let (resolved, report) = super::resolve_imports(&parsed, &registry, &EmptyBifs);

        assert!(matches!(
            resolved
                .first()
                .and_then(|entry| entry.as_ref())
                .map(|entry| entry.target),
            Some(ResolvedImportTarget::Unresolved { module, function: unresolved_function, arity: 0 })
                if module == callee && unresolved_function == function
        ));
        assert_eq!(
            report.imports(),
            vec![UnresolvedImportEntry::new(callee, function, 0)]
        );
        assert!(report.deferred_imports().is_empty());
    }

    #[test]
    fn missing_import_module_is_deferred_and_kept_resolved_by_index() {
        let atoms = AtomTable::new();
        let callee = atoms.intern("callee");
        let function = atoms.intern("run");
        let registry = ModuleRegistry::new();
        let mut parsed =
            load_beam_chunks(include_bytes!("../../tests/fixtures/hello.beam"), &atoms)
                .expect("fixture parses");
        parsed.imports = vec![crate::loader::ImportEntry {
            module: callee,
            function,
            arity: 0,
        }];

        let (resolved, report) = super::resolve_imports(&parsed, &registry, &EmptyBifs);

        assert!(report.imports().is_empty());
        assert!(report.is_empty());
        assert_eq!(
            report.deferred_imports(),
            vec![UnresolvedImportEntry::new(callee, function, 0)]
        );
        assert!(matches!(
            resolved
                .first()
                .and_then(|entry| entry.as_ref())
                .map(|entry| entry.target),
            Some(ResolvedImportTarget::Deferred { module, function: deferred_function, arity: 0 })
                if module == callee && deferred_function == function
        ));
    }
}
