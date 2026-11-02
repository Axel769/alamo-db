//! Buffer for variable-length values.
//!
//! Rule 2 of §4.2: **offsets + contiguous buffer**, never `Vec<String>`.
//! `offsets` has length `rows + 1`, the first element is 0, it is monotonic.
//! Value *i* is `data[offsets[i]..offsets[i+1]]`.
//! NULL = repeated offset (length 0) plus validity bit at 0.
//!
//! Offsets are `i32` for Arrow compatibility. From this follows the third
//! chunk-close trigger of §5: **a chunk cannot exceed 2 GB of text**, and
//! with large TEXT columns you get there sooner than it seems.

use crate::CoreError;

/// Cap above which a push fails. `i32::MAX` is the hard limit of the format;
/// the `ChunkBuilder` closes long before (see `ChunkLimits`).
pub const MAX_VARLEN_BYTES: usize = i32::MAX as usize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VarBuf {
    offsets: Vec<i32>,
    data: Vec<u8>,
}

impl Default for VarBuf {
    fn default() -> Self {
        Self::new()
    }
}

impl VarBuf {
    pub fn new() -> Self {
        VarBuf {
            offsets: vec![0],
            data: Vec::new(),
        }
    }

    pub fn with_capacity(rows: usize, bytes: usize) -> Self {
        let mut offsets = Vec::with_capacity(rows + 1);
        offsets.push(0);
        VarBuf {
            offsets,
            data: Vec::with_capacity(bytes),
        }
    }

    /// Appends a value. Error — not a panic and not silent truncation — if
    /// the buffer would exceed the `i32` range.
    #[inline]
    pub fn push(&mut self, value: &[u8]) -> Result<(), CoreError> {
        let end = self.data.len() + value.len();
        if end > MAX_VARLEN_BYTES {
            return Err(CoreError::OffsetOverflow {
                current: self.data.len(),
                adding: value.len(),
            });
        }
        self.data.extend_from_slice(value);
        self.offsets.push(end as i32);
        Ok(())
    }

    /// Appends an empty cell: repeated offset. Used both for NULLs and for
    /// empty strings — the validity bitmap tells them apart, not the buffer
    /// (§8.2: NULL, `"NULL"` and the empty string must stay three different
    /// things).
    #[inline]
    pub fn push_empty(&mut self) {
        self.offsets.push(self.data.len() as i32);
    }

    #[inline]
    pub fn get(&self, index: usize) -> &[u8] {
        let start = self.offsets[index] as usize;
        let end = self.offsets[index + 1] as usize;
        &self.data[start..end]
    }

    /// Length in **bytes** of element *i*, at zero cost from the offsets.
    /// It is the basis of the `len_p95` statistics of §4.3 / §9.6.2.
    #[inline]
    pub fn value_len(&self, index: usize) -> u32 {
        (self.offsets[index + 1] - self.offsets[index]) as u32
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.offsets.len() - 1
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[inline]
    pub fn data_len(&self) -> usize {
        self.data.len()
    }

    pub fn offsets(&self) -> &[i32] {
        &self.offsets
    }

    pub fn data(&self) -> &[u8] {
        &self.data
    }

    pub fn byte_size(&self) -> usize {
        self.data.capacity() + self.offsets.capacity() * std::mem::size_of::<i32>()
    }

    /// Gives back the excess capacity.
    ///
    /// A `Vec` grows by doubling: when filling ends the capacity can be almost
    /// twice the length, and on a TEXT column that double is RAM really in
    /// use — not an accounting detail. A closed chunk is immutable (§5), so
    /// the memory can be given back without losing anything: it costs a
    /// one-off `memcpy` per column.
    pub fn shrink(&mut self) {
        self.data.shrink_to_fit();
        self.offsets.shrink_to_fit();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_are_monotonic_and_start_at_zero() {
        let mut b = VarBuf::new();
        b.push(b"ab").unwrap();
        b.push_empty();
        b.push(b"cde").unwrap();
        assert_eq!(b.offsets(), &[0, 2, 2, 5]);
        assert_eq!(b.get(0), b"ab");
        assert_eq!(b.get(1), b"");
        assert_eq!(b.get(2), b"cde");
        assert_eq!(b.value_len(2), 3);
    }
}
