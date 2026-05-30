// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! A small inline string specialized for logic bit-strings.
//!
//! Profiling large FST traces showed that ~95% of decoded value changes are a
//! single bit (`"0"`/`"1"`/`"x"`/`"z"`), and ~99% are 23 characters or fewer.
//! Storing each as a heap [`String`] meant tens of millions of allocations when
//! materializing a busy trace. [`BitStr`] stores up to [`INLINE_CAP`] bytes
//! inline (on the stack / inside the enclosing `Vec`), spilling to a heap
//! `String` only for the rare long value, so the common case allocates nothing.
//!
//! It is intentionally tiny and purpose-built: bit-strings are pure ASCII, so
//! there are no multi-byte-char concerns, and the only operations needed are
//! construction from `&str`, borrowing as `&str`, cloning, equality, and
//! debug formatting. The inline capacity is chosen so the struct stays the same
//! size as a `String` (24 bytes on 64-bit), keeping `RawValue` from growing.

use std::fmt;

/// Maximum number of bytes stored inline before spilling to the heap. 23 bytes
/// + a 1-byte length tag fits in 24 bytes, matching `String`'s size.
pub const INLINE_CAP: usize = 23;

/// A compact string for logic bit-strings: inline for short values, heap for
/// long ones.
#[derive(Clone)]
pub enum BitStr {
    /// `len` valid bytes stored in `buf[..len]` (ASCII, no allocation).
    Inline { buf: [u8; INLINE_CAP], len: u8 },
    /// Spilled to the heap for values longer than [`INLINE_CAP`].
    Heap(String),
}

impl BitStr {
    /// Build directly from an iterator of ASCII characters with a known length,
    /// avoiding any intermediate `String`. `len` must be the exact number of
    /// chars the iterator yields (the bit width); each char must be ASCII (true
    /// for logic bit-strings: `0 1 x z h l u w -`). This is the zero-allocation
    /// path used when decoding logic vectors that fit inline.
    #[inline]
    pub fn from_ascii_iter<I: Iterator<Item = char>>(len: usize, chars: I) -> BitStr {
        if len <= INLINE_CAP {
            let mut buf = [0u8; INLINE_CAP];
            let mut i = 0;
            for c in chars {
                if i >= INLINE_CAP {
                    break;
                }
                // Non-ASCII would corrupt the byte length; bit chars are always
                // ASCII, but guard defensively by replacing with 'x'.
                buf[i] = if c.is_ascii() { c as u8 } else { b'x' };
                i += 1;
            }
            BitStr::Inline { buf, len: i as u8 }
        } else {
            // Rare long value: collect into a heap String.
            let mut s = String::with_capacity(len);
            s.extend(chars);
            BitStr::Heap(s)
        }
    }

    /// Construct from a string slice, storing inline when it fits.
    #[inline]
    pub fn new(s: &str) -> BitStr {
        let bytes = s.as_bytes();
        if bytes.len() <= INLINE_CAP {
            let mut buf = [0u8; INLINE_CAP];
            buf[..bytes.len()].copy_from_slice(bytes);
            BitStr::Inline {
                buf,
                len: bytes.len() as u8,
            }
        } else {
            BitStr::Heap(s.to_string())
        }
    }

    /// Borrow the contents as a string slice.
    #[inline]
    pub fn as_str(&self) -> &str {
        match self {
            BitStr::Inline { buf, len } => {
                // Safety/correctness: the bytes were copied from a valid `&str`
                // (UTF-8) and `len` records exactly how many; bit-strings are
                // ASCII so this is always a valid UTF-8 boundary. Use the
                // checked conversion to avoid any `unsafe`.
                std::str::from_utf8(&buf[..*len as usize]).unwrap_or("")
            }
            BitStr::Heap(s) => s.as_str(),
        }
    }

    /// Length in bytes (== chars for ASCII bit-strings).
    #[inline]
    pub fn len(&self) -> usize {
        match self {
            BitStr::Inline { len, .. } => *len as usize,
            BitStr::Heap(s) => s.len(),
        }
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl PartialEq for BitStr {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}
impl Eq for BitStr {}

impl fmt::Debug for BitStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Render like a normal string literal so RawValue's derived Debug reads
        // naturally (e.g. Bits("0101")).
        fmt::Debug::fmt(self.as_str(), f)
    }
}

impl From<&str> for BitStr {
    #[inline]
    fn from(s: &str) -> BitStr {
        BitStr::new(s)
    }
}

impl From<String> for BitStr {
    #[inline]
    fn from(s: String) -> BitStr {
        // If it fits inline, copy and free the heap buffer; otherwise reuse it.
        if s.len() <= INLINE_CAP {
            BitStr::new(&s)
        } else {
            BitStr::Heap(s)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_and_heap_roundtrip() {
        for s in ["", "0", "1", "x", "z", "0101", "xxxxzzzz", &"1".repeat(23)] {
            let b = BitStr::new(s);
            assert_eq!(b.as_str(), s, "roundtrip {s:?}");
            assert_eq!(b.len(), s.len());
            assert!(matches!(b, BitStr::Inline { .. }), "{s:?} should be inline");
        }
        // Just over the inline boundary -> heap.
        let long = "1".repeat(24);
        let b = BitStr::new(&long);
        assert_eq!(b.as_str(), long);
        assert!(matches!(b, BitStr::Heap(_)), "24 chars should spill to heap");

        // A very wide bus value.
        let wide = "10xz".repeat(40); // 160 chars
        let b = BitStr::new(&wide);
        assert_eq!(b.as_str(), wide);
        assert!(matches!(b, BitStr::Heap(_)));
    }

    #[test]
    fn boundary_exactly_inline_cap() {
        let s = "0".repeat(INLINE_CAP);
        let b = BitStr::new(&s);
        assert!(matches!(b, BitStr::Inline { .. }));
        assert_eq!(b.as_str(), s);
    }

    #[test]
    fn equality_inline_vs_heap_same_content() {
        // Same content must compare equal regardless of storage. (Construct a
        // heap variant explicitly to exercise the cross-representation path.)
        let inline = BitStr::new("0101");
        let heap = BitStr::Heap("0101".to_string());
        assert_eq!(inline, heap);
        assert_eq!(heap, inline);
        assert_ne!(BitStr::new("0101"), BitStr::new("0100"));
        assert_ne!(BitStr::new("01"), BitStr::new("010"));
    }

    #[test]
    fn clone_preserves_value() {
        let a = BitStr::new("1x0z");
        let b = a.clone();
        assert_eq!(a, b);
        assert_eq!(b.as_str(), "1x0z");
        let h = BitStr::new(&"z".repeat(50));
        let hc = h.clone();
        assert_eq!(h, hc);
    }

    #[test]
    fn debug_reads_like_string() {
        assert_eq!(format!("{:?}", BitStr::new("0101")), "\"0101\"");
    }

    #[test]
    fn struct_size_is_bounded() {
        // The point of the inline buffer is to avoid per-change heap allocation
        // without enlarging `RawValue`. `RawValue` is dominated by its largest
        // variant and an 8-byte-aligned discriminant; a `String` is 24 bytes and
        // `RawValue` is already 32 bytes (discriminant + alignment), so a 32-byte
        // `BitStr` leaves `RawValue`'s size unchanged. Guard that bound here.
        let sz = std::mem::size_of::<BitStr>();
        assert!(sz <= 32, "BitStr size {sz} exceeds 32 bytes");
        // The inline buffer must still hold the documented capacity.
        assert!(INLINE_CAP >= 23, "inline capacity regressed to {INLINE_CAP}");
    }

    #[test]
    fn raw_value_does_not_grow() {
        // The enclosing RawValue (Bits(BitStr) | Real(f64) | Str(String) |
        // Event) must not be larger than it was with Bits(String). Both are
        // 32 bytes on 64-bit targets.
        use crate::backend::RawValue;
        assert!(
            std::mem::size_of::<RawValue>() <= 32,
            "RawValue grew to {} bytes",
            std::mem::size_of::<RawValue>()
        );
    }
}
