//! Per-connection atom cache support for BEAM distribution messages.
//!
//! Distribution keeps atom caching scoped to a connection. An outbound side uses
//! one [`AtomCache`] to translate local atoms into remote cache indices, while
//! the inbound side uses a separate [`AtomCache`] to translate indices announced
//! by the peer back into local atoms.

use std::collections::{HashMap, VecDeque};

use crate::atom::{Atom, AtomTable};
use crate::term::Term;

/// Number of entries in the BEAM distribution atom cache.
pub const ATOM_CACHE_SIZE: usize = 256;

/// Payload tag for an atom already present in the connection atom cache.
pub const ATOM_CACHE_REF: u8 = 0x52;

/// Header-entry tag for a newly announced connection atom cache entry.
pub const NEW_ATOM_CACHE_REF: u8 = 0x4e;

/// Per-connection bidirectional atom cache.
///
/// Lookups refresh recency, so a full cache evicts the entry that has been
/// neither inserted nor read for the longest time.
#[derive(Debug, Clone)]
pub struct AtomCache {
    slots: [Option<Atom>; ATOM_CACHE_SIZE],
    by_atom: HashMap<Atom, u8>,
    lru: VecDeque<u8>,
}

impl AtomCache {
    /// Create an empty atom cache.
    #[must_use]
    pub fn new() -> Self {
        Self {
            slots: [None; ATOM_CACHE_SIZE],
            by_atom: HashMap::with_capacity(ATOM_CACHE_SIZE),
            lru: VecDeque::with_capacity(ATOM_CACHE_SIZE),
        }
    }

    /// Insert `atom`, returning the cache index assigned to it.
    ///
    /// Inserting an already cached atom is idempotent and refreshes recency.
    /// When all 256 slots are full, the least-recently-used slot is evicted.
    pub fn insert(&mut self, atom: Atom) -> u8 {
        if let Some(index) = self.by_atom.get(&atom).copied() {
            self.touch(index);
            return index;
        }

        let index = match self.free_index() {
            Some(index) => index,
            None => self.evict_lru(),
        };
        self.replace_slot(index, atom);
        index
    }

    /// Install `atom` at an explicit peer-announced cache `index`.
    ///
    /// This is used while decoding distribution cache headers. Existing entries
    /// at the index, or existing entries for the same atom at another index, are
    /// removed before the new mapping is made most-recently-used.
    pub fn insert_at(&mut self, index: u8, atom: Atom) {
        if let Some(existing_index) = self.by_atom.get(&atom).copied() {
            self.clear_slot(existing_index);
        }
        self.replace_slot(index, atom);
    }

    /// Retrieve a cached atom by cache index.
    pub fn lookup_by_index(&mut self, index: u8) -> Option<Atom> {
        let atom = self.slots[usize::from(index)]?;
        self.touch(index);
        Some(atom)
    }

    /// Retrieve a cached index by atom.
    pub fn lookup_by_atom(&mut self, atom: Atom) -> Option<u8> {
        let index = self.by_atom.get(&atom).copied()?;
        self.touch(index);
        Some(index)
    }

    /// Return the number of occupied slots.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_atom.len()
    }

    /// Return whether the cache is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_atom.is_empty()
    }

    fn free_index(&self) -> Option<u8> {
        self.slots
            .iter()
            .position(Option::is_none)
            .and_then(|index| u8::try_from(index).ok())
    }

    fn evict_lru(&mut self) -> u8 {
        while let Some(index) = self.lru.pop_front() {
            if self.slots[usize::from(index)].is_some() {
                self.clear_slot(index);
                return index;
            }
        }

        // The cache is full when this method is called, so the LRU queue should
        // contain at least one occupied slot. Fall back to slot zero without
        // panicking if the queue was externally desynchronised by a future edit.
        self.clear_slot(0);
        0
    }

    fn replace_slot(&mut self, index: u8, atom: Atom) {
        self.clear_slot(index);
        self.slots[usize::from(index)] = Some(atom);
        self.by_atom.insert(atom, index);
        self.touch(index);
    }

    fn clear_slot(&mut self, index: u8) {
        if let Some(old_atom) = self.slots[usize::from(index)].take() {
            self.by_atom.remove(&old_atom);
        }
        self.remove_from_lru(index);
    }

    fn touch(&mut self, index: u8) {
        self.remove_from_lru(index);
        self.lru.push_back(index);
    }

    fn remove_from_lru(&mut self, index: u8) {
        self.lru.retain(|candidate| *candidate != index);
    }
}

impl Default for AtomCache {
    fn default() -> Self {
        Self::new()
    }
}

/// A single cache entry announced in a distribution atom-cache header.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct AtomCacheHeaderEntry {
    /// Cache index the peer should use for subsequent [`ATOM_CACHE_REF`] terms.
    pub index: u8,
    /// UTF-8 atom name to intern and store at `index`.
    pub name: String,
}

/// Header entries accompanying one distribution message.
#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct AtomCacheHeader {
    entries: Vec<AtomCacheHeaderEntry>,
}

impl AtomCacheHeader {
    /// Create an empty atom-cache header.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Return all newly announced cache entries.
    #[must_use]
    pub fn entries(&self) -> &[AtomCacheHeaderEntry] {
        &self.entries
    }

    fn push(&mut self, index: u8, name: &str) {
        self.entries.push(AtomCacheHeaderEntry {
            index,
            name: name.to_owned(),
        });
    }

    fn can_encode_another_entry(&self) -> bool {
        self.entries.len() < usize::from(u8::MAX)
    }

    fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Errors from distribution atom-cache encoding.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum AtomCacheEncodeError {
    /// The atom is not present in the local atom table.
    AtomResolveFailed,
    /// A header contained more entries than the one-byte count can represent.
    TooManyHeaderEntries,
    /// An atom name is too long for this internal header format.
    AtomNameTooLong,
}

/// Errors from distribution atom-cache decoding.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum AtomCacheDecodeError {
    /// The input ended before the expected field was complete.
    Truncated,
    /// The header entry tag is not supported by this codec.
    UnsupportedHeaderTag(u8),
    /// A cache reference pointed at an empty slot.
    MissingCacheIndex(u8),
    /// The atom name bytes were not valid UTF-8.
    InvalidUtf8,
    /// Bytes remained after a complete payload atom reference.
    TrailingBytes,
    /// The payload atom tag is not [`ATOM_CACHE_REF`].
    UnsupportedPayloadTag(u8),
}

/// Encode a single atom occurrence for a distribution payload.
///
/// Cached atoms are emitted as [`ATOM_CACHE_REF`] plus the one-byte cache index.
/// Uncached atoms are inserted into `cache`, announced through `header` as a
/// [`NEW_ATOM_CACHE_REF`] entry, and the payload still uses the newly assigned
/// one-byte cache index.
pub fn encode_atom_reference(
    atom: Atom,
    cache: &mut AtomCache,
    atom_table: &AtomTable,
    header: &mut AtomCacheHeader,
    out: &mut Vec<u8>,
) -> Result<u8, AtomCacheEncodeError> {
    let index = if let Some(index) = cache.lookup_by_atom(atom) {
        index
    } else {
        if !header.can_encode_another_entry() {
            return Err(AtomCacheEncodeError::TooManyHeaderEntries);
        }
        let name = atom_table
            .resolve(atom)
            .ok_or(AtomCacheEncodeError::AtomResolveFailed)?;
        if name.len() > usize::from(u16::MAX) {
            return Err(AtomCacheEncodeError::AtomNameTooLong);
        }
        let index = cache.insert(atom);
        header.push(index, name);
        index
    };

    out.push(ATOM_CACHE_REF);
    out.push(index);
    Ok(index)
}

/// Encode atom-cache header entries into the distribution message header.
///
/// The internal format is one byte of entry count followed by repeated
/// `NEW_ATOM_CACHE_REF, index, u16-name-len, utf8-name` records.
pub fn encode_header(header: &AtomCacheHeader) -> Result<Vec<u8>, AtomCacheEncodeError> {
    let entry_count =
        u8::try_from(header.len()).map_err(|_| AtomCacheEncodeError::TooManyHeaderEntries)?;
    let mut out = Vec::new();
    out.push(entry_count);

    for entry in header.entries() {
        let name_bytes = entry.name.as_bytes();
        let name_len =
            u16::try_from(name_bytes.len()).map_err(|_| AtomCacheEncodeError::AtomNameTooLong)?;
        out.push(NEW_ATOM_CACHE_REF);
        out.push(entry.index);
        out.extend_from_slice(&name_len.to_be_bytes());
        out.extend_from_slice(name_bytes);
    }

    Ok(out)
}

/// Decode a distribution atom-cache header and update `cache` with all entries.
pub fn decode_header(
    bytes: &[u8],
    cache: &mut AtomCache,
    atom_table: &AtomTable,
) -> Result<AtomCacheHeader, AtomCacheDecodeError> {
    let mut cursor = Cursor::new(bytes);
    let entry_count = cursor.read_u8()?;
    let mut header = AtomCacheHeader::new();

    for _ in 0..entry_count {
        let tag = cursor.read_u8()?;
        if tag != NEW_ATOM_CACHE_REF {
            return Err(AtomCacheDecodeError::UnsupportedHeaderTag(tag));
        }
        let index = cursor.read_u8()?;
        let name_len = usize::from(cursor.read_u16()?);
        let name_bytes = cursor.read_bytes(name_len)?;
        let name =
            std::str::from_utf8(name_bytes).map_err(|_| AtomCacheDecodeError::InvalidUtf8)?;
        let atom = atom_table.intern(name);
        cache.insert_at(index, atom);
        header.push(index, name);
    }

    if cursor.has_remaining() {
        return Err(AtomCacheDecodeError::TrailingBytes);
    }

    Ok(header)
}

/// Decode one cached atom reference from a distribution payload.
pub fn decode_atom_reference(
    bytes: &[u8],
    cache: &mut AtomCache,
) -> Result<Term, AtomCacheDecodeError> {
    let mut cursor = Cursor::new(bytes);
    let tag = cursor.read_u8()?;
    if tag != ATOM_CACHE_REF {
        return Err(AtomCacheDecodeError::UnsupportedPayloadTag(tag));
    }
    let index = cursor.read_u8()?;
    if cursor.has_remaining() {
        return Err(AtomCacheDecodeError::TrailingBytes);
    }
    let atom = cache
        .lookup_by_index(index)
        .ok_or(AtomCacheDecodeError::MissingCacheIndex(index))?;
    Ok(Term::atom(atom))
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn read_u8(&mut self) -> Result<u8, AtomCacheDecodeError> {
        let byte = self
            .bytes
            .get(self.offset)
            .copied()
            .ok_or(AtomCacheDecodeError::Truncated)?;
        self.offset += 1;
        Ok(byte)
    }

    fn read_u16(&mut self) -> Result<u16, AtomCacheDecodeError> {
        let high = self.read_u8()?;
        let low = self.read_u8()?;
        Ok(u16::from_be_bytes([high, low]))
    }

    fn read_bytes(&mut self, len: usize) -> Result<&'a [u8], AtomCacheDecodeError> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or(AtomCacheDecodeError::Truncated)?;
        let slice = self
            .bytes
            .get(self.offset..end)
            .ok_or(AtomCacheDecodeError::Truncated)?;
        self.offset = end;
        Ok(slice)
    }

    fn has_remaining(&self) -> bool {
        self.offset < self.bytes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ATOM_CACHE_REF, ATOM_CACHE_SIZE, AtomCache, AtomCacheDecodeError, AtomCacheHeader,
        NEW_ATOM_CACHE_REF, decode_atom_reference, decode_header, encode_atom_reference,
        encode_header,
    };
    use crate::atom::AtomTable;

    fn atom_names(count: usize) -> Vec<String> {
        (0..count).map(|index| format!("atom_{index}")).collect()
    }

    #[test]
    fn insert_256_atoms_then_257th_evicts_first() {
        let table = AtomTable::new();
        let mut cache = AtomCache::new();
        let names = atom_names(ATOM_CACHE_SIZE + 1);
        let atoms: Vec<_> = names.iter().map(|name| table.intern(name)).collect();

        for (expected_index, atom) in atoms.iter().take(ATOM_CACHE_SIZE).copied().enumerate() {
            let index = cache.insert(atom);
            assert_eq!(usize::from(index), expected_index);
        }

        for (expected_index, atom) in atoms.iter().take(ATOM_CACHE_SIZE).copied().enumerate() {
            let expected_index = u8::try_from(expected_index).ok();
            assert_eq!(cache.lookup_by_atom(atom), expected_index);
        }

        let replacement_index = cache.insert(atoms[ATOM_CACHE_SIZE]);
        assert_eq!(replacement_index, 0);
        assert_eq!(cache.lookup_by_atom(atoms[0]), None);
        assert_eq!(cache.lookup_by_index(0), Some(atoms[ATOM_CACHE_SIZE]));
    }

    #[test]
    fn lookup_refreshes_lru_recency() {
        let table = AtomTable::new();
        let mut cache = AtomCache::new();
        let names = atom_names(ATOM_CACHE_SIZE + 1);
        let atoms: Vec<_> = names.iter().map(|name| table.intern(name)).collect();

        for atom in atoms.iter().take(ATOM_CACHE_SIZE).copied() {
            cache.insert(atom);
        }

        assert_eq!(cache.lookup_by_atom(atoms[0]), Some(0));
        let replacement_index = cache.insert(atoms[ATOM_CACHE_SIZE]);

        assert_eq!(replacement_index, 1);
        assert_eq!(cache.lookup_by_atom(atoms[0]), Some(0));
        assert_eq!(cache.lookup_by_atom(atoms[1]), None);
    }

    #[test]
    fn encode_new_atom_header_then_decode_populates_peer_cache() {
        let sender_table = AtomTable::new();
        let receiver_table = AtomTable::new();
        let atom = sender_table.intern("distributed_atom");
        let mut sender_cache = AtomCache::new();
        let mut receiver_cache = AtomCache::new();
        let mut header = AtomCacheHeader::new();
        let mut payload = Vec::new();

        let index = encode_atom_reference(
            atom,
            &mut sender_cache,
            &sender_table,
            &mut header,
            &mut payload,
        );
        assert_eq!(index, Ok(0));
        assert_eq!(payload, vec![ATOM_CACHE_REF, 0]);
        assert_eq!(header.entries().len(), 1);

        let encoded_header = encode_header(&header);
        assert_eq!(
            encoded_header.as_ref().map(|bytes| bytes.first().copied()),
            Ok(Some(1))
        );
        assert_eq!(
            encoded_header.as_ref().map(|bytes| bytes.get(1).copied()),
            Ok(Some(NEW_ATOM_CACHE_REF))
        );

        assert!(encoded_header.is_ok());
        let decoded_header = encoded_header
            .as_ref()
            .map_or(Err(AtomCacheDecodeError::Truncated), |bytes| {
                decode_header(bytes, &mut receiver_cache, &receiver_table)
            });
        assert!(decoded_header.is_ok());

        let decoded = decode_atom_reference(&payload, &mut receiver_cache);
        let received_atom = receiver_table.lookup("distributed_atom");
        assert!(received_atom.is_some());
        assert_eq!(decoded.ok().and_then(|term| term.as_atom()), received_atom);
        assert_eq!(
            received_atom.and_then(|atom| receiver_cache.lookup_by_atom(atom)),
            Some(0)
        );
    }

    #[test]
    fn cached_atom_reference_does_not_add_header_entry() {
        let table = AtomTable::new();
        let atom = table.intern("cached_atom");
        let mut cache = AtomCache::new();
        let mut first_header = AtomCacheHeader::new();
        let mut first_payload = Vec::new();
        let mut second_header = AtomCacheHeader::new();
        let mut second_payload = Vec::new();

        assert_eq!(
            encode_atom_reference(
                atom,
                &mut cache,
                &table,
                &mut first_header,
                &mut first_payload
            ),
            Ok(0)
        );
        assert_eq!(
            encode_atom_reference(
                atom,
                &mut cache,
                &table,
                &mut second_header,
                &mut second_payload,
            ),
            Ok(0)
        );

        assert_eq!(first_header.entries().len(), 1);
        assert!(second_header.entries().is_empty());
        assert_eq!(second_payload, vec![ATOM_CACHE_REF, 0]);
    }

    #[test]
    fn too_many_new_atoms_fails_without_partial_payload_or_cache_insert() {
        let table = AtomTable::new();
        let atoms: Vec<_> = atom_names(usize::from(u8::MAX) + 1)
            .iter()
            .map(|name| table.intern(name))
            .collect();
        let mut cache = AtomCache::new();
        let mut header = AtomCacheHeader::new();
        let mut payload = Vec::new();

        for atom in atoms.iter().take(usize::from(u8::MAX)).copied() {
            assert!(
                encode_atom_reference(atom, &mut cache, &table, &mut header, &mut payload).is_ok()
            );
        }

        let payload_len = payload.len();
        let uncached_atom = atoms[usize::from(u8::MAX)];
        assert_eq!(
            encode_atom_reference(uncached_atom, &mut cache, &table, &mut header, &mut payload),
            Err(super::AtomCacheEncodeError::TooManyHeaderEntries)
        );
        assert_eq!(payload.len(), payload_len);
        assert_eq!(header.entries().len(), usize::from(u8::MAX));
        assert_eq!(cache.lookup_by_atom(uncached_atom), None);
    }

    #[test]
    fn missing_cached_atom_reference_fails() {
        let mut cache = AtomCache::new();
        assert_eq!(
            decode_atom_reference(&[ATOM_CACHE_REF, 42], &mut cache),
            Err(AtomCacheDecodeError::MissingCacheIndex(42))
        );
    }
}
