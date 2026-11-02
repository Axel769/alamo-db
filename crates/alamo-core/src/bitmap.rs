//! Packed-bit bitmap.
//!
//! Rule 1 of §4.2: the validity bitmap is **separate from the data**, bits are
//! packed **LSB-first**, and **1 = valid, 0 = NULL** — the opposite of
//! intuition, and the Arrow convention. Never `Option<T>` per element.
//!
//! The same type serves as storage for boolean columns (bit-packed).

/// Packed bits, LSB-first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Bitmap {
    bits: Vec<u8>,
    len: usize,
}

impl Bitmap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(cap_bits: usize) -> Self {
        Bitmap {
            bits: Vec::with_capacity(cap_bits.div_ceil(8)),
            len: 0,
        }
    }

    /// Appends one bit. `true` = valid (or, for bool columns, the value true).
    #[inline]
    pub fn push(&mut self, set: bool) {
        let byte = self.len >> 3;
        if byte == self.bits.len() {
            self.bits.push(0);
        }
        if set {
            self.bits[byte] |= 1u8 << (self.len & 7);
        }
        self.len += 1;
    }

    /// Appends `n` bits all equal to `set`. Fast path for columns without
    /// NULLs, which are the majority: whole bytes are filled in bulk
    /// (0xFF/0x00), bit by bit only the head and the tail.
    pub fn push_n(&mut self, set: bool, mut n: usize) {
        // Head: complete the current partial byte, if any.
        let head = ((8 - (self.len & 7)) & 7).min(n);
        for _ in 0..head {
            self.push(set);
        }
        n -= head;
        // Body: here `len` is a multiple of 8, so `bits.len() == len / 8`
        // and one byte per 8 bits can be appended directly.
        let full = n / 8;
        self.bits
            .resize(self.bits.len() + full, if set { 0xFF } else { 0x00 });
        self.len += full * 8;
        // Tail: the remaining bits below one byte.
        for _ in 0..(n & 7) {
            self.push(set);
        }
    }

    #[inline]
    pub fn get(&self, index: usize) -> bool {
        debug_assert!(index < self.len);
        (self.bits[index >> 3] >> (index & 7)) & 1 == 1
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Bytes used. §4.8: memory is a **property computed from the layout**,
    /// never a counter updated by hand.
    pub fn byte_size(&self) -> usize {
        self.bits.capacity()
    }

    /// View of the raw bytes — useful for the conversion to Arrow (§4.2).
    pub fn as_bytes(&self) -> &[u8] {
        &self.bits
    }

    pub fn clear(&mut self) {
        self.bits.clear();
        self.len = 0;
    }

    /// Iterator over the bits. Not the hot path: it serves the degradation to
    /// `Utf8` (§4.6) and the tests.
    pub fn iter(&self) -> impl Iterator<Item = bool> + '_ {
        (0..self.len).map(move |i| self.get(i))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lsb_first_packing() {
        let mut b = Bitmap::new();
        // 1,0,1 -> byte 0b0000_0101
        b.push(true);
        b.push(false);
        b.push(true);
        assert_eq!(b.as_bytes()[0], 0b0000_0101);
        assert_eq!(b.len(), 3);
    }

    #[test]
    fn push_n_matches_bitwise_push_across_byte_boundaries() {
        // Reference: the same sequence built bit by bit. `push_n` starts
        // mid-byte, crosses whole bytes and ends mid-byte.
        let mut fast = Bitmap::new();
        let mut slow = Bitmap::new();
        let runs = [
            (true, 3usize),
            (true, 45),
            (false, 13),
            (true, 8),
            (false, 100),
            (true, 1),
        ];
        for &(set, n) in &runs {
            fast.push_n(set, n);
            for _ in 0..n {
                slow.push(set);
            }
        }
        assert_eq!(fast, slow);
        assert_eq!(fast.len(), 170);
        assert!(fast.get(0) && !fast.get(50) && fast.get(169));
    }

    #[test]
    fn roundtrip_across_byte_boundary() {
        let mut b = Bitmap::new();
        let pattern: Vec<bool> = (0..37).map(|i| i % 3 == 0).collect();
        for &p in &pattern {
            b.push(p);
        }
        for (i, &p) in pattern.iter().enumerate() {
            assert_eq!(b.get(i), p, "bit {i}");
        }
    }
}
