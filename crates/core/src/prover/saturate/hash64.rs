// crates/core/src/saturate/hash64.rs
//
// Pass-through hashing for keys that are ALREADY uniform 64-bit content
// hashes (AtomId / SentenceId / SymbolId / ClauseKey / coin values).
// The prover's hot loops probe these maps millions of times per run;
// SipHash showed up at ~11% of CPU in profiles purely re-hashing
// values that xxh64 already distributed.  Multi-word keys (tuples with
// polarity/arity tags) fold in with a rotate-xor — fine for uniform
// inputs, NOT a general-purpose or DoS-resistant hasher.

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};

#[derive(Default, Clone, Copy)]
pub(crate) struct ContentHasher(u64);

impl Hasher for ContentHasher {
    #[inline]
    fn finish(&self) -> u64 { self.0 }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = self.0.rotate_left(8) ^ u64::from(b);
        }
    }

    #[inline]
    fn write_u8(&mut self, v: u8) { self.0 = self.0.rotate_left(8) ^ u64::from(v); }
    #[inline]
    fn write_u16(&mut self, v: u16) { self.0 = self.0.rotate_left(16) ^ u64::from(v); }
    #[inline]
    fn write_u32(&mut self, v: u32) { self.0 = self.0.rotate_left(16) ^ u64::from(v); }
    #[inline]
    fn write_u64(&mut self, v: u64) { self.0 = self.0.rotate_left(32) ^ v; }
    #[inline]
    fn write_usize(&mut self, v: usize) { self.write_u64(v as u64); }
}

pub(crate) type BuildContentHasher = BuildHasherDefault<ContentHasher>;
pub(crate) type Map64<K, V> = HashMap<K, V, BuildContentHasher>;
pub(crate) type Set64<K> = HashSet<K, BuildContentHasher>;
