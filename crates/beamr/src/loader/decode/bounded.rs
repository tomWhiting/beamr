use crate::error::LoadError;

use super::budget::MAX_TABLE_ENTRIES;

/// Bounds-checked byte cursor shared by chunk decoders and the ETF sub-decoder.
pub(super) struct BoundedCursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> BoundedCursor<'a> {
    pub(super) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    pub(super) fn remaining(&self) -> &'a [u8] {
        &self.bytes[self.offset..]
    }

    pub(super) fn remaining_len(&self) -> usize {
        self.remaining().len()
    }

    pub(super) fn ensure_count(
        &self,
        count: usize,
        min_elem_bytes: usize,
        label: &str,
    ) -> Result<(), LoadError> {
        if count > MAX_TABLE_ENTRIES {
            return Err(LoadError::DecodeError(format!("{label} exceeds limit")));
        }
        let required = count
            .checked_mul(min_elem_bytes)
            .ok_or_else(|| LoadError::DecodeError(format!("{label} byte requirement overflows")))?;
        if required > self.remaining_len() {
            return Err(LoadError::DecodeError(format!(
                "{label} impossible for payload size"
            )));
        }
        Ok(())
    }

    pub(super) fn advance(&mut self, len: usize) -> Result<(), LoadError> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or_else(|| LoadError::DecodeError("cursor offset overflow".into()))?;
        if end > self.bytes.len() {
            return Err(LoadError::DecodeError("truncated chunk data".into()));
        }
        self.offset = end;
        Ok(())
    }

    pub(super) fn expect_empty(&self, name: &str) -> Result<(), LoadError> {
        if self.remaining().is_empty() {
            Ok(())
        } else {
            Err(LoadError::DecodeError(format!(
                "trailing {name} chunk data"
            )))
        }
    }

    pub(super) fn read_u8(&mut self) -> Result<u8, LoadError> {
        let byte = self
            .bytes
            .get(self.offset)
            .copied()
            .ok_or_else(|| LoadError::DecodeError("truncated chunk data".into()))?;
        self.offset += 1;
        Ok(byte)
    }

    pub(super) fn read_bytes(&mut self, len: usize) -> Result<&'a [u8], LoadError> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or_else(|| LoadError::DecodeError("cursor offset overflow".into()))?;
        let slice = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| LoadError::DecodeError("truncated chunk data".into()))?;
        self.offset = end;
        Ok(slice)
    }

    pub(super) fn read_u16(&mut self) -> Result<u16, LoadError> {
        let bytes = self.read_bytes(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    pub(super) fn read_u32(&mut self) -> Result<u32, LoadError> {
        let bytes = self.read_bytes(4)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    pub(super) fn read_i32(&mut self) -> Result<i32, LoadError> {
        let bytes = self.read_bytes(4)?;
        Ok(i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }
}
