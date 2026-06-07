use crate::error::LoadError;

/// Maximum recursion depth for the External Term Format literal decoder.
pub(crate) const MAX_ETF_DEPTH: usize = 256;

/// Hard ceiling on any length-prefixed table/collection count read from untrusted bytes.
pub(crate) const MAX_TABLE_ENTRIES: usize = 16_777_216;

/// Maximum newly interned atoms allowed while loading one module.
pub(crate) const MAX_ATOMS_PER_MODULE: usize = 65_536;

/// Default decoded-node budget for one module load.
pub(crate) const DEFAULT_DECODE_NODE_BUDGET: usize = 1_000_000;

/// Default decoded/allocation byte budget for one module load.
pub(crate) const DEFAULT_DECODE_BYTE_BUDGET: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
pub(crate) struct DecodeBudget {
    pub(crate) depth_remaining: usize,
    pub(crate) nodes_remaining: usize,
    pub(crate) bytes_remaining: usize,
    pub(crate) atoms_remaining: usize,
}

impl DecodeBudget {
    pub(crate) fn new(
        depth_remaining: usize,
        nodes_remaining: usize,
        bytes_remaining: usize,
        atoms_remaining: usize,
    ) -> Self {
        Self {
            depth_remaining,
            nodes_remaining,
            bytes_remaining,
            atoms_remaining,
        }
    }

    pub(crate) fn charge_node(&mut self) -> Result<(), LoadError> {
        if self.nodes_remaining == 0 {
            return Err(LoadError::DecodeError(
                "decode node budget exhausted".into(),
            ));
        }
        self.nodes_remaining -= 1;
        Ok(())
    }

    pub(crate) fn charge_bytes(&mut self, n: usize) -> Result<(), LoadError> {
        if n > self.bytes_remaining {
            return Err(LoadError::DecodeError(
                "decode byte budget exhausted".into(),
            ));
        }
        self.bytes_remaining -= n;
        Ok(())
    }

    pub(crate) fn charge_atom(&mut self) -> Result<(), LoadError> {
        if self.atoms_remaining == 0 {
            return Err(LoadError::DecodeError(
                "decode atom budget exhausted".into(),
            ));
        }
        self.atoms_remaining -= 1;
        Ok(())
    }

    pub(crate) fn descend(&mut self) -> Result<(), LoadError> {
        if self.depth_remaining == 0 {
            return Err(LoadError::DecodeError("ETF nesting exceeds limit".into()));
        }
        self.depth_remaining -= 1;
        Ok(())
    }

    pub(crate) fn ascend(&mut self) {
        self.depth_remaining = self.depth_remaining.saturating_add(1);
    }
}

impl Default for DecodeBudget {
    fn default() -> Self {
        Self::new(
            MAX_ETF_DEPTH,
            DEFAULT_DECODE_NODE_BUDGET,
            DEFAULT_DECODE_BYTE_BUDGET,
            MAX_ATOMS_PER_MODULE,
        )
    }
}
