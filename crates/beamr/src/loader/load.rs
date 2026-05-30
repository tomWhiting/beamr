use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use crate::atom::{Atom, AtomTable};
use crate::error::LoadError;
use crate::module::{Module, ModuleRegistry, ResolvedImport, ResolvedImportTarget};
use crate::native::BifRegistry;

use super::decode::{
    ExportEntry, ImportEntry, Instruction, LambdaEntry, LineInfo, Literal, decode_atom_chunk,
    decode_code_chunk, decode_export_chunk, decode_import_chunk, decode_lambda_chunk,
    decode_line_chunk, decode_literal_chunk, decode_string_chunk,
};
use super::parser::parse_beam_chunks;
use super::validate::validate_module;

#[derive(Debug, Clone, PartialEq, Eq)]
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
}

impl UnresolvedImportReport {
    /// Creates a grouped report from unresolved import entries.
    #[must_use]
    pub fn new(entries: Vec<UnresolvedImportEntry>) -> Self {
        let mut report = Self::default();
        for entry in entries {
            report.push(entry);
        }
        report
    }

    /// Returns true when no imports are unresolved.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries_by_module.values().all(Vec::is_empty)
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
        modules.sort_by_key(|atom| format!("{atom:?}"));
        modules
            .into_iter()
            .flat_map(|module| self.entries_for(module).iter().copied())
            .collect()
    }

    fn push(&mut self, entry: UnresolvedImportEntry) {
        self.entries_by_module
            .entry(entry.module)
            .or_default()
            .push(entry);
    }
}

impl fmt::Display for UnresolvedImportReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_empty() {
            return formatter.write_str("no unresolved imports");
        }

        let mut modules: Vec<_> = self.entries_by_module.keys().copied().collect();
        modules.sort_by_key(|atom| format!("{atom:?}"));
        for (module_index, module) in modules.into_iter().enumerate() {
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
        Ok(())
    }
}

pub fn load_beam_chunks(bytes: &[u8], atom_table: &AtomTable) -> Result<ParsedModule, LoadError> {
    let chunks = parse_beam_chunks(bytes)?;

    let atom_chunk = find_chunk(&chunks, b"AtU8")
        .or_else(|| find_chunk(&chunks, b"Atom"))
        .ok_or_else(|| LoadError::MissingChunk("Atom/AtU8".into()))?;
    let atoms = decode_atom_chunk(atom_chunk, atom_table)?;
    let name = atoms
        .first()
        .copied()
        .ok_or_else(|| LoadError::DecodeError("atom chunk is empty".into()))?;

    let literals = match find_chunk(&chunks, b"LitT") {
        Some(bytes) => decode_literal_chunk(bytes, atom_table)?,
        None => Vec::new(),
    };

    let code_chunk =
        find_chunk(&chunks, b"Code").ok_or_else(|| LoadError::MissingChunk("Code".into()))?;
    let instructions = decode_code_chunk(code_chunk, &atoms, &literals)?;

    let imports = match find_chunk(&chunks, b"ImpT") {
        Some(bytes) => decode_import_chunk(bytes, &atoms)?,
        None => Vec::new(),
    };
    let exports = match find_chunk(&chunks, b"ExpT") {
        Some(bytes) => decode_export_chunk(bytes, &atoms)?,
        None => Vec::new(),
    };
    let lambdas = match find_chunk(&chunks, b"FunT") {
        Some(bytes) => decode_lambda_chunk(bytes, &atoms)?,
        None => Vec::new(),
    };
    let string_table = find_chunk(&chunks, b"StrT")
        .map(decode_string_chunk)
        .unwrap_or_default();
    let line_info = match find_chunk(&chunks, b"Line") {
        Some(bytes) => decode_line_chunk(bytes)?,
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
    let parsed = load_beam_chunks(bytes, atom_table)?;
    let (resolved_by_index, report) = resolve_imports(&parsed, module_registry, bif_registry);
    validate_module(&parsed, &resolved_by_index)?;
    let module = module_from_parsed(parsed, resolved_by_index.into_iter().flatten().collect());
    let module = module_registry.insert(module);
    Ok((module, report))
}

fn resolve_imports(
    parsed: &ParsedModule,
    module_registry: &ModuleRegistry,
    bif_registry: &impl BifRegistry,
) -> (Vec<Option<ResolvedImport>>, UnresolvedImportReport) {
    let mut resolved = Vec::with_capacity(parsed.imports.len());
    let mut unresolved = Vec::new();

    for import in &parsed.imports {
        let target = module_registry
            .lookup(import.module)
            .and_then(|module| {
                module
                    .exports
                    .get(&(import.function, import.arity))
                    .copied()
                    .map(|label| ResolvedImportTarget::Code {
                        module: import.module,
                        label,
                    })
            })
            .or_else(|| {
                bif_registry
                    .lookup(import.module, import.function, import.arity)
                    .map(ResolvedImportTarget::Native)
            });

        match target {
            Some(target) => resolved.push(Some(ResolvedImport {
                module: import.module,
                function: import.function,
                arity: import.arity,
                target,
            })),
            None => {
                unresolved.push(UnresolvedImportEntry::new(
                    import.module,
                    import.function,
                    import.arity,
                ));
                resolved.push(None);
            }
        }
    }

    (resolved, UnresolvedImportReport::new(unresolved))
}

fn module_from_parsed(parsed: ParsedModule, resolved_imports: Vec<ResolvedImport>) -> Module {
    let exports = parsed
        .exports
        .into_iter()
        .map(|export| ((export.function, export.arity), export.label))
        .collect();

    Module {
        name: parsed.name,
        exports,
        code: parsed.instructions,
        literals: parsed.literals,
        resolved_imports,
        lambdas: parsed.lambdas,
        string_table: parsed.string_table,
        line_info: parsed.line_info,
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
        assert!(!report.is_empty());
    }

    #[test]
    fn resolved_cross_module_import_points_to_code_label() {
        let atoms = AtomTable::new();
        let callee = atoms.intern("callee");
        let function = atoms.intern("run");
        let registry = ModuleRegistry::new();
        let mut target = Module {
            name: callee,
            exports: std::collections::HashMap::new(),
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
}
