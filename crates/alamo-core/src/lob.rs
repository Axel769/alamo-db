//! Large values — §4.7.
//!
//! Inline cap per cell: **64 KB**. Above the threshold the value does not go
//! into the column buffer but here; the column keeps a truncated prefix + handle.
//!
//! The arena lives **inside the chunk** because a stream does not rewind:
//! "fetch the full value" cannot mean re-running the query. It is however a
//! separate unit, so it is the first thing that can be spilled to disk.

/// Inline cap per cell. Constant to be reconfirmed with the M0 measurements
/// (open decision 7 of §14).
pub const LOB_INLINE_CAP: usize = 64 * 1024;

/// How many bytes stay in the column as a preview when the value ends up in
/// the arena.
pub const LOB_PREFIX_LEN: usize = 256;

/// Opaque reference to a value in the chunk's arena.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LobHandle(pub u32);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LobArena {
    data: Vec<u8>,
    /// (offset, length) per handle.
    spans: Vec<(u64, u32)>,
}

impl LobArena {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, value: &[u8]) -> LobHandle {
        let offset = self.data.len() as u64;
        self.data.extend_from_slice(value);
        self.spans.push((offset, value.len() as u32));
        LobHandle((self.spans.len() - 1) as u32)
    }

    pub fn get(&self, handle: LobHandle) -> Option<&[u8]> {
        let &(offset, len) = self.spans.get(handle.0 as usize)?;
        let start = offset as usize;
        Some(&self.data[start..start + len as usize])
    }

    pub fn len(&self) -> usize {
        self.spans.len()
    }

    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    /// §4.8: bytes computed from the layout, never a hand-kept counter.
    pub fn byte_size(&self) -> usize {
        self.data.capacity() + self.spans.capacity() * std::mem::size_of::<(u64, u32)>()
    }
}
