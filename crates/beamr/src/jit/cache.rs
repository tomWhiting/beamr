//! Generation-aware native-code cache for JIT compiled functions.

use crate::atom::Atom;
use dashmap::DashMap;

use super::NativeCode;

/// Module/function/arity/generation key for compiled native code.
#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
pub struct JitCacheKey {
    /// Module atom.
    pub module: Atom,
    /// Function atom.
    pub function: Atom,
    /// Function arity.
    pub arity: u8,
    /// Module generation compiled by this cache entry.
    pub generation: u64,
}

impl JitCacheKey {
    /// Creates a new generation-aware JIT cache key.
    #[must_use]
    pub fn new(module: Atom, function: Atom, arity: u8, generation: u64) -> Self {
        Self {
            module,
            function,
            arity,
            generation,
        }
    }
}

/// Thread-safe cache of JIT compiled native code.
#[derive(Debug, Default)]
pub struct JitCache {
    entries: DashMap<JitCacheKey, NativeCode>,
}

impl JitCache {
    /// Creates an empty JIT code cache.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: DashMap::new(),
        }
    }

    /// Looks up native code for an exact MFA and module generation.
    #[must_use]
    pub fn lookup(
        &self,
        module: Atom,
        function: Atom,
        arity: u8,
        generation: u64,
    ) -> Option<NativeCode> {
        self.entries
            .get(&JitCacheKey::new(module, function, arity, generation))
            .map(|entry| entry.value().clone())
    }

    /// Inserts or replaces compiled native code for a cache key.
    pub fn insert(&self, key: JitCacheKey, code: NativeCode) {
        self.entries.insert(key, code);
    }

    /// Invalidates every cache entry for one module generation.
    ///
    /// Returns the number of entries removed. Any `NativeCode` clones already handed to callers
    /// remain alive until those references are dropped.
    pub fn invalidate_generation(&self, module: Atom, generation: u64) -> usize {
        let keys: Vec<_> = self
            .entries
            .iter()
            .filter_map(|entry| {
                let key = *entry.key();
                (key.module == module && key.generation == generation).then_some(key)
            })
            .collect();

        keys.into_iter()
            .filter(|key| self.entries.remove(key).is_some())
            .count()
    }

    /// Invalidates every cache entry for a module across all generations.
    ///
    /// Returns the number of entries removed. This is used for force-purge semantics where both
    /// current and old code for a module must leave the cache.
    pub fn invalidate_module(&self, module: Atom) -> usize {
        let keys: Vec<_> = self
            .entries
            .iter()
            .filter_map(|entry| {
                let key = *entry.key();
                (key.module == module).then_some(key)
            })
            .collect();

        keys.into_iter()
            .filter(|key| self.entries.remove(key).is_some())
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::{JitCache, JitCacheKey};
    use crate::atom::Atom;
    use crate::jit::{JitCompiler, JitSettings};
    use crate::loader::Instruction;

    fn compile_return(module: Atom, function: Atom, arity: u8) -> crate::jit::NativeCode {
        let compiler = JitCompiler::new(JitSettings).expect("host JIT compiler should initialize");
        compiler
            .compile(&[Instruction::Return], module, function, arity)
            .expect("return-only function should compile")
    }

    #[test]
    fn insert_and_lookup_round_trip() {
        let cache = JitCache::new();
        let key = JitCacheKey::new(Atom::MODULE, Atom::OK, 0, 1);
        let code = compile_return(key.module, key.function, key.arity);
        let call_ptr = code.call_ptr();

        cache.insert(key, code);

        let cached = cache
            .lookup(key.module, key.function, key.arity, key.generation)
            .expect("inserted code should be cached");
        assert_eq!(cached.call_ptr(), call_ptr);
        assert!(cached.stack_maps().is_empty());
    }

    #[test]
    fn lookup_requires_matching_generation() {
        let cache = JitCache::new();
        let key = JitCacheKey::new(Atom::MODULE, Atom::OK, 0, 1);
        let code = compile_return(key.module, key.function, key.arity);

        cache.insert(key, code);

        assert!(
            cache
                .lookup(key.module, key.function, key.arity, key.generation + 1)
                .is_none()
        );
    }

    #[test]
    fn invalidate_generation_evicts_only_matching_module_generation() {
        let cache = JitCache::new();
        let old_key = JitCacheKey::new(Atom::MODULE, Atom::OK, 0, 1);
        let new_key = JitCacheKey::new(Atom::MODULE, Atom::OK, 0, 2);
        let other_module_key = JitCacheKey::new(Atom::new(999), Atom::OK, 0, 1);
        cache.insert(
            old_key,
            compile_return(old_key.module, old_key.function, old_key.arity),
        );
        cache.insert(
            new_key,
            compile_return(new_key.module, new_key.function, new_key.arity),
        );
        cache.insert(
            other_module_key,
            compile_return(
                other_module_key.module,
                other_module_key.function,
                other_module_key.arity,
            ),
        );

        assert_eq!(cache.invalidate_generation(Atom::MODULE, 1), 1);

        assert!(
            cache
                .lookup(
                    old_key.module,
                    old_key.function,
                    old_key.arity,
                    old_key.generation
                )
                .is_none()
        );
        assert!(
            cache
                .lookup(
                    new_key.module,
                    new_key.function,
                    new_key.arity,
                    new_key.generation
                )
                .is_some()
        );
        assert!(
            cache
                .lookup(
                    other_module_key.module,
                    other_module_key.function,
                    other_module_key.arity,
                    other_module_key.generation,
                )
                .is_some()
        );
    }

    #[test]
    fn invalidate_module_evicts_all_generations_for_module() {
        let cache = JitCache::new();
        let old_key = JitCacheKey::new(Atom::MODULE, Atom::OK, 0, 1);
        let new_key = JitCacheKey::new(Atom::MODULE, Atom::OK, 0, 2);
        cache.insert(
            old_key,
            compile_return(old_key.module, old_key.function, old_key.arity),
        );
        cache.insert(
            new_key,
            compile_return(new_key.module, new_key.function, new_key.arity),
        );

        assert_eq!(cache.invalidate_module(Atom::MODULE), 2);

        assert!(
            cache
                .lookup(
                    old_key.module,
                    old_key.function,
                    old_key.arity,
                    old_key.generation
                )
                .is_none()
        );
        assert!(
            cache
                .lookup(
                    new_key.module,
                    new_key.function,
                    new_key.arity,
                    new_key.generation
                )
                .is_none()
        );
    }
}
