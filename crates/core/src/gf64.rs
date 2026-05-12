//! GF(2^64), the finite field of 64-bit patterns where XOR is addition.
//!
//! Provides decodable residue fingerprints: from the two-word residual
//! ⟨t ⊕ s, t³ ⊕ s³⟩ the pair {t, s} is recovered by deriving the product
//! and factoring the resulting quadratic.
//!
//! An element is a `u64`, bit i ↔ coefficient of xⁱ in
//! GF(2)[x] / (x⁶⁴ + x⁴ + x³ + x + 1).  Addition is XOR; multiplication is
//! carryless multiplication followed by reduction, dispatched to the CPU's
//! polynomial-multiply instruction (ARM PMULL / x86 PCLMULQDQ) with a
//! portable shift/XOR loop as fallback.

/// Low part of the reduction polynomial: x⁶⁴ ≡ x⁴ + x³ + x + 1.
const POLY_LOW: u64 = (1 << 4) | (1 << 3) | (1 << 1) | 1;

/// Carryless (polynomial) multiply of two 64-bit polynomials → 128 bits.
///
/// Dispatches to the hardware polynomial-multiply instruction (ARM `PMULL`
/// or x86 `PCLMULQDQ`) when available, falling back to the portable
/// shift/XOR loop otherwise.
#[inline]
fn clmul(a: u64, b: u64) -> u128 {
    #[cfg(target_arch = "aarch64")]
    {
        if hw_clmul_available() {
            // SAFETY: feature presence checked at runtime above.
            return unsafe { clmul_pmull(a, b) };
        }
    }
    #[cfg(target_arch = "x86_64")]
    {
        if hw_clmul_available() {
            // SAFETY: feature presence checked at runtime above.
            return unsafe { clmul_pclmul(a, b) };
        }
    }
    clmul_portable(a, b)
}

/// Whether the CPU exposes the ARM `PMULL` polynomial multiplier.
#[cfg(target_arch = "aarch64")]
#[inline]
fn hw_clmul_available() -> bool {
    static AVAIL: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAIL.get_or_init(|| std::arch::is_aarch64_feature_detected!("aes"))
}

/// One 64×64→128 polynomial multiply via ARM `PMULL`.
///
/// # Safety
/// The `aes` target feature must be present at runtime.
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "aes")]
#[inline]
unsafe fn clmul_pmull(a: u64, b: u64) -> u128 {
    core::arch::aarch64::vmull_p64(a, b)
}

/// Whether the CPU exposes the x86 `PCLMULQDQ` carry-less multiply.
#[cfg(target_arch = "x86_64")]
#[inline]
fn hw_clmul_available() -> bool {
    static AVAIL: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAIL.get_or_init(|| {
        std::is_x86_feature_detected!("pclmulqdq")
            && std::is_x86_feature_detected!("sse4.1")
    })
}

/// One 64×64→128 polynomial multiply via x86 `PCLMULQDQ`.
///
/// # Safety
/// The `pclmulqdq` and `sse4.1` target features must be present at runtime.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "pclmulqdq", enable = "sse4.1")]
#[inline]
unsafe fn clmul_pclmul(a: u64, b: u64) -> u128 {
    use core::arch::x86_64::*;
    let va = _mm_set_epi64x(0, a as i64);
    let vb = _mm_set_epi64x(0, b as i64);
    let r = _mm_clmulepi64_si128::<0x00>(va, vb);
    let lo = _mm_cvtsi128_si64(r) as u64;
    let hi = _mm_extract_epi64::<1>(r) as u64;
    (u128::from(hi) << 64) | u128::from(lo)
}

/// Portable shift/XOR carryless multiply for targets without a hardware
/// polynomial multiplier.
#[inline]
fn clmul_portable(a: u64, b: u64) -> u128 {
    let mut acc: u128 = 0;
    let a = a as u128;
    let mut b = b;
    let mut shift = 0u32;
    while b != 0 {
        let tz = b.trailing_zeros();
        shift += tz;
        acc ^= a << shift;
        b >>= tz;
        b >>= 1; // clear the bit we just used (separate shift: tz may be 63)
        shift += 1;
    }
    acc
}

/// Reduce a 128-bit polynomial mod x⁶⁴ + x⁴ + x³ + x + 1.
#[inline]
fn reduce(mut x: u128) -> u64 {
    // Two folds suffice: the first can push bits past bit 63 again, but
    // the second fold's high half is < x⁴ shifted ≤ 4 and fits.
    for _ in 0..2 {
        let hi = (x >> 64) as u64;
        if hi == 0 {
            break;
        }
        let lo = x as u64;
        x = (lo as u128)
            ^ ((hi as u128) << 4)
            ^ ((hi as u128) << 3)
            ^ ((hi as u128) << 1)
            ^ (hi as u128);
    }
    debug_assert_eq!(x >> 64, 0);
    x as u64
}

/// Field multiplication.
#[inline]
pub(crate) fn mul(a: u64, b: u64) -> u64 {
    reduce(clmul(a, b))
}

/// Field squaring.
#[inline]
pub(crate) fn sq(a: u64) -> u64 {
    mul(a, a)
}

/// Field cube: a³ = a²·a.
#[inline]
pub(crate) fn cube(a: u64) -> u64 {
    mul(sq(a), a)
}

/// Multiplicative inverse via Fermat: a⁻¹ = a^(2⁶⁴ − 2).
///
/// Panics in debug builds on a zero input; callers must guard against it.
pub(crate) fn inv(a: u64) -> u64 {
    debug_assert_ne!(a, 0, "gf64: inverse of zero");
    let mut result = 1u64;
    let mut base = a;
    let mut e: u64 = !1; // 2^64 - 2
    while e != 0 {
        if e & 1 == 1 {
            result = mul(result, base);
        }
        base = sq(base);
        e >>= 1;
    }
    result
}

/// Field division: a / b = a · b⁻¹.
#[inline]
pub(crate) fn div(a: u64, b: u64) -> u64 {
    mul(a, inv(b))
}

/// Montgomery batch inversion: replace every element of `xs` with its
/// inverse using one Fermat inversion plus 3(n−1) multiplies.
///
/// All inputs must be nonzero.
pub(crate) fn batch_inv(xs: &mut [u64]) {
    match xs.len() {
        0 => return,
        1 => { xs[0] = inv(xs[0]); return; }
        _ => {}
    }
    // prefix[i] = xs[0]·…·xs[i]
    let mut prefix = Vec::with_capacity(xs.len());
    let mut acc = 1u64;
    for &x in xs.iter() {
        debug_assert_ne!(x, 0, "gf64: batch_inv of zero");
        acc = mul(acc, x);
        prefix.push(acc);
    }
    let mut inv_acc = inv(acc);
    for i in (1..xs.len()).rev() {
        let xi = xs[i];
        xs[i] = mul(inv_acc, prefix[i - 1]);
        inv_acc = mul(inv_acc, xi);
    }
    xs[0] = inv_acc;
}

// -- The Artin–Schreier quadratic solver ---------------------------------------
//
// Roots of z² ⊕ z = c.  The map L(z) = z² ⊕ z is GF(2)-linear with kernel
// {0, 1}, so solving reduces to linear algebra over the precomputed RREF of
// L's 64×64 bit-matrix.  Solvable iff c is in L's image; the two roots are
// z and z ⊕ 1.

/// Precomputed solver for z² ⊕ z = c over GF(2⁶⁴).
struct QuadSolver {
    /// Row transform: echelon row r = XOR of the original equations
    /// selected by `transform[r]`'s bits.  Original equation r is
    /// "output bit r", so applying to a rhs c is `parity(transform[r] & c)`.
    transform: [u64; 64],
    /// RREF rows over the 64 unknowns (bit i = coefficient of zᵢ).
    rows: [u64; 64],
    /// Pivot column of each non-zero RREF row (u8::MAX = zero row).
    pivot: [u8; 64],
}

impl QuadSolver {
    fn build() -> Self {
        // Column i of L = L(xⁱ) = (xⁱ)² ⊕ xⁱ.
        let mut col = [0u64; 64];
        for (i, c) in col.iter_mut().enumerate() {
            let basis = 1u64 << i;
            *c = sq(basis) ^ basis;
        }
        // Equation r (output bit r): row bitmask over unknowns.
        let mut rows = [0u64; 64];
        for r in 0..64 {
            let mut bits = 0u64;
            for (i, c) in col.iter().enumerate() {
                bits |= ((c >> r) & 1) << i;
            }
            rows[r] = bits;
        }
        // Gauss–Jordan to RREF, tracking the row transform.
        let mut transform = [0u64; 64];
        for (r, t) in transform.iter_mut().enumerate() {
            *t = 1u64 << r;
        }
        let mut pivot = [u8::MAX; 64];
        let mut rank = 0usize;
        for c in 0..64u8 {
            // Find a row at or below `rank` with bit c set.
            let Some(p) = (rank..64).find(|&r| (rows[r] >> c) & 1 == 1) else {
                continue;
            };
            rows.swap(rank, p);
            transform.swap(rank, p);
            // Eliminate everywhere else (Jordan: above and below).
            for r in 0..64 {
                if r != rank && (rows[r] >> c) & 1 == 1 {
                    rows[r] ^= rows[rank];
                    transform[r] ^= transform[rank];
                }
            }
            pivot[rank] = c;
            rank += 1;
        }
        // L's kernel is exactly {0, 1}: rank must be 63.
        debug_assert_eq!(rank, 63, "gf64: z²+z must have rank 63");
        Self { transform, rows, pivot }
    }

    /// One root of z² ⊕ z = c, or `None` when c is not in the image
    /// (the other root is the returned value ⊕ 1).
    fn solve(&self, c: u64) -> Option<u64> {
        let parity = |x: u64| (x.count_ones() & 1) as u64;
        let mut z = 0u64;
        for r in 0..64 {
            let y = parity(self.transform[r] & c);
            if self.pivot[r] == u8::MAX {
                // Zero row: consistency requirement.
                if y != 0 {
                    return None;
                }
            } else if y != 0 {
                // Free variable (kernel direction) set to 0: the pivot
                // variable equals the transformed rhs bit.
                z |= 1u64 << self.pivot[r];
            }
        }
        if sq(z) ^ z == c {
            Some(z)
        } else {
            None
        }
    }
}

/// In-place 64×64 bit-matrix transpose: after the call, bit j of row i =
/// the old bit i of row j.  Involutive.
fn transpose64(a: &mut [u64; 64]) {
    transpose64_hd(a);
    let mut b = [0u64; 64];
    for (i, out) in b.iter_mut().enumerate() {
        *out = a[63 - i].reverse_bits();
    }
    *a = b;
}

/// Bit-matrix transpose kernel: bit j of row i = old bit (63−i) of
/// row (63−j).
fn transpose64_hd(a: &mut [u64; 64]) {
    let mut j = 32usize;
    let mut m: u64 = 0x0000_0000_FFFF_FFFF;
    while j != 0 {
        let mut k = 0usize;
        while k < 64 {
            let t = (a[k] ^ (a[k + j] >> j)) & m;
            a[k] ^= t;
            a[k + j] ^= t << j;
            k = (k + j + 1) & !j;
        }
        j >>= 1;
        m ^= m << j;
    }
}

impl QuadSolver {
    /// Bitsliced batch solve of z² ⊕ z = c for 64 right-hand sides at once.
    ///
    /// Returns `(solutions, ok_mask)`: lane j holds a verified root iff bit j
    /// of `ok_mask` is set.  Lane-for-lane equivalent to [`Self::solve`].
    fn solve_block(&self, cs: &[u64; 64]) -> ([u64; 64], u64) {
        let mut planes = *cs;
        transpose64(&mut planes); // planes[i] = bit i of every lane
        let mut fail = 0u64;
        let mut zplanes = [0u64; 64];
        for r in 0..64 {
            // Y_r across all lanes: XOR of the planes transform[r] selects.
            let mut y = 0u64;
            let mut t = self.transform[r];
            while t != 0 {
                y ^= planes[t.trailing_zeros() as usize];
                t &= t - 1;
            }
            if self.pivot[r] == u8::MAX {
                fail |= y; // zero row: any 1 lane is inconsistent
            } else {
                zplanes[self.pivot[r] as usize] = y;
            }
        }
        transpose64(&mut zplanes); // back to one solution per lane
        // Per-lane algebraic verification.
        let mut ok = !fail;
        for (j, &z) in zplanes.iter().enumerate() {
            if (ok >> j) & 1 == 1 && sq(z) ^ z != cs[j] {
                ok &= !(1u64 << j);
            }
        }
        (zplanes, ok)
    }
}

fn solver() -> &'static QuadSolver {
    use std::sync::OnceLock;
    static SOLVER: OnceLock<QuadSolver> = OnceLock::new();
    SOLVER.get_or_init(QuadSolver::build)
}

/// Roots of z² ⊕ e1·z ⊕ e2 = 0 (e1 ≠ 0).
///
/// Returns the pair, or `None` when the quadratic has no roots in the field.
pub(crate) fn quad_roots(e1: u64, e2: u64) -> Option<(u64, u64)> {
    debug_assert_ne!(e1, 0);
    let c = div(e2, sq(e1));
    let w = solver().solve(c)?;
    let z1 = mul(e1, w);
    let z2 = z1 ^ e1;
    Some((z1, z2))
}

// -- Power-sum sketches (the decodable fingerprint words) -----------------------

/// A two-word power-sum sketch ⟨Σ xᵢ, Σ xᵢ³⟩ over the coins it has absorbed.
///
/// Componentwise XOR-linear; the residual of up to two unknown coins is
/// decodable (see [`decode`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Sketch {
    /// Power sum Σ xᵢ.
    pub(crate) s1: u64,
    /// Power sum Σ xᵢ³.
    pub(crate) s3: u64,
}

impl Sketch {
    /// Absorb (or, by XOR symmetry, remove) one coin.
    #[inline]
    pub(crate) fn toggle(&mut self, coin: u64) {
        self.s1 ^= coin;
        self.s3 ^= cube(coin);
    }

    /// Componentwise XOR — the residual constructor.
    #[inline]
    pub(crate) fn xor(self, other: Sketch) -> Sketch {
        Sketch { s1: self.s1 ^ other.s1, s3: self.s3 ^ other.s3 }
    }

    /// Whether both power-sum words are zero.
    pub(crate) fn is_zero(self) -> bool {
        self.s1 == 0 && self.s3 == 0
    }
}

/// Result of decoding a residual sketch with a known expected count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Decoded {
    /// Zero unknowns: the residual must be exactly zero.
    None,
    /// One unknown coin.
    One(u64),
    /// Two distinct unknown coins (unordered).
    Two(u64, u64),
    /// The residual is not a valid sketch of `expected` coins; the caller
    /// falls back to ordinary verification.
    Fail,
}

/// Decode a residual sketch known to contain `expected` unknown coins.
///
/// - `expected == 0`: match iff the residual is zero.
/// - `expected == 1`: the coin is `s1` itself; `s3` must equal its cube.
/// - `expected == 2`: sum e1 = `s1`, product e2 = (s3 ⊕ e1³) / e1; the coins
///   are the roots of z² ⊕ e1·z ⊕ e2.
/// - `expected >= 3`: beyond this sketch's capacity, so [`Decoded::Fail`].
pub(crate) fn decode(r: Sketch, expected: u32) -> Decoded {
    match expected {
        0 => {
            if r.is_zero() { Decoded::None } else { Decoded::Fail }
        }
        1 => {
            if r.s1 != 0 && cube(r.s1) == r.s3 {
                Decoded::One(r.s1)
            } else {
                Decoded::Fail
            }
        }
        2 => {
            let e1 = r.s1;
            if e1 == 0 {
                // Two distinct coins cannot XOR to zero: collision.
                return Decoded::Fail;
            }
            // t³ ⊕ s³ = e1³ ⊕ e1·e2  ⇒  e2 = (s3 ⊕ e1³) / e1
            let e2 = div(r.s3 ^ cube(e1), e1);
            let Some((z1, z2)) = quad_roots(e1, e2) else {
                return Decoded::Fail;
            };
            if z1 == 0 || z2 == 0 {
                return Decoded::Fail; // 0 is never a coin
            }
            if cube(z1) ^ cube(z2) != r.s3 {
                return Decoded::Fail;
            }
            Decoded::Two(z1, z2)
        }
        _ => Decoded::Fail,
    }
}

/// Batch size from which the bitsliced quadratic solver beats per-element
/// scalar solves.
const BITSLICE_MIN: usize = 16;

/// Batched [`decode`], exactly equivalent to mapping the scalar `decode`
/// over `rs`, writing results into `out`.
///
/// - `expected != 2`, or fewer than 2 residuals: scalar per element.
/// - `expected == 2`: each element's two divisions ride one Montgomery
///   batch inversion, and the quadratic solves go through the bitsliced
///   block solver once the batch reaches [`BITSLICE_MIN`].
pub(crate) fn decode_batch(rs: &[Sketch], expected: u32, out: &mut Vec<Decoded>) {
    out.clear();
    if expected != 2 || rs.len() < 2 {
        out.extend(rs.iter().map(|&r| decode(r, expected)));
        return;
    }
    let n = rs.len();
    out.resize(n, Decoded::Fail);

    // Lanes with a usable e1 (zero sum ⇒ Fail, exactly as scalar).
    let mut live: Vec<u32> = Vec::with_capacity(n);
    let mut invs: Vec<u64> = Vec::with_capacity(n);
    for (i, r) in rs.iter().enumerate() {
        if r.s1 != 0 {
            live.push(i as u32);
            invs.push(r.s1);
        }
    }
    batch_inv(&mut invs);

    // e2 = (s3 ⊕ e1³)·e1⁻¹;  c = e2·(e1⁻¹)²  — the normalized rhs of
    // w² ⊕ w = c (substitution z = e1·w, divisions inlined against the
    // shared inverse).
    let mut cs: Vec<u64> = Vec::with_capacity(live.len());
    for (k, &i) in live.iter().enumerate() {
        let r = rs[i as usize];
        let inv1 = invs[k];
        let e2 = mul(r.s3 ^ cube(r.s1), inv1);
        cs.push(mul(e2, sq(inv1)));
    }

    // Solve w² ⊕ w = c: bitsliced blocks at volume, scalar below.
    let qs = solver();
    let mut ws: Vec<Option<u64>> = Vec::with_capacity(cs.len());
    if cs.len() >= BITSLICE_MIN {
        for chunk in cs.chunks(64) {
            let mut block = [0u64; 64];
            block[..chunk.len()].copy_from_slice(chunk);
            let (zs, ok) = qs.solve_block(&block);
            for j in 0..chunk.len() {
                ws.push(((ok >> j) & 1 == 1).then(|| zs[j]));
            }
        }
    } else {
        ws.extend(cs.iter().map(|&c| qs.solve(c)));
    }

    // Scale back (z = e1·w), audit, emit.
    for (k, &i) in live.iter().enumerate() {
        let Some(w) = ws[k] else { continue }; // stays Fail
        let r = rs[i as usize];
        let z1 = mul(r.s1, w);
        let z2 = z1 ^ r.s1;
        if z1 == 0 || z2 == 0 {
            continue; // 0 is never a coin
        }
        if cube(z1) ^ cube(z2) != r.s3 {
            continue; // final algebraic audit
        }
        out[i as usize] = Decoded::Two(z1, z2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic pseudo-random u64 stream for property tests.
    fn rng(seed: u64) -> impl FnMut() -> u64 {
        let mut s = seed.wrapping_add(0x9E3779B97F4A7C15);
        move || {
            s = s.wrapping_add(0x9E3779B97F4A7C15);
            let mut z = s;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
            z ^ (z >> 31)
        }
    }

    #[test]
    fn hw_clmul_matches_portable() {
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut step = || { x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407); x };
        for _ in 0..20_000 {
            let (a, b) = (step(), step());
            assert_eq!(clmul(a, b), clmul_portable(a, b), "a={a:#x} b={b:#x}");
        }
        // Edge cases: zero, one, top bit, all ones.
        for &a in &[0u64, 1, 1 << 63, u64::MAX] {
            for &b in &[0u64, 1, 1 << 63, u64::MAX] {
                assert_eq!(clmul(a, b), clmul_portable(a, b));
            }
        }
    }

    #[test]
    #[ignore] // microbenchmark: cargo test --release ... clmul_bench -- --ignored --nocapture
    fn clmul_bench() {
        let mut x: u64 = 0xDEAD_BEEF_CAFE_F00D;
        let mut step = || { x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407); x };
        let pairs: Vec<(u64, u64)> = (0..1_000_000).map(|_| (step(), step())).collect();
        let t = std::time::Instant::now();
        let mut acc = 0u64;
        for &(a, b) in &pairs { acc ^= mul(a, b); }
        let hw = t.elapsed();
        let t = std::time::Instant::now();
        let mut acc2 = 0u64;
        for &(a, b) in &pairs { acc2 ^= reduce(clmul_portable(a, b)); }
        let portable = t.elapsed();
        assert_eq!(acc, acc2);
        eprintln!("1M field muls: dispatched {hw:?}, portable {portable:?} ({:.1}x)",
            portable.as_secs_f64() / hw.as_secs_f64());
    }

    #[test]
    fn batch_inv_matches_scalar() {
        let mut next = rng(7);
        for n in [1usize, 2, 3, 7, 64, 129] {
            let xs: Vec<u64> = (0..n).map(|_| loop {
                let v = next();
                if v != 0 { break v; }
            }).collect();
            let mut batched = xs.clone();
            batch_inv(&mut batched);
            for (x, b) in xs.iter().zip(&batched) {
                assert_eq!(*b, inv(*x), "n={n} x={x:#x}");
            }
        }
    }

    #[test]
    fn solve_block_matches_scalar_solve() {
        let mut next = rng(11);
        // Mix of solvable (c = w²⊕w for random w) and random rhs.
        let mut cs = [0u64; 64];
        for (j, c) in cs.iter_mut().enumerate() {
            let v = next();
            *c = if j % 2 == 0 { sq(v) ^ v } else { v };
        }
        let (zs, ok) = solver().solve_block(&cs);
        for (j, &c) in cs.iter().enumerate() {
            match solver().solve(c) {
                Some(z) => {
                    assert_eq!((ok >> j) & 1, 1, "lane {j} should solve");
                    // Either root is valid (z vs z⊕1); both satisfy.
                    assert!(zs[j] == z || zs[j] == z ^ 1, "lane {j}");
                    assert_eq!(sq(zs[j]) ^ zs[j], c);
                }
                None => assert_eq!((ok >> j) & 1, 0, "lane {j} should fail"),
            }
        }
    }

    #[test]
    fn decode_batch_matches_scalar_decode() {
        let mut next = rng(13);
        let mut mk_residual = |kind: u32| -> Sketch {
            match kind {
                // Valid two-coin residual.
                0 => {
                    let (a, b) = (next() | 1, next() | 2);
                    let mut s = Sketch::default();
                    s.toggle(a);
                    s.toggle(b);
                    s
                }
                // Garbage words.
                1 => Sketch { s1: next(), s3: next() },
                // Zero sum (collision shape).
                2 => Sketch { s1: 0, s3: next() },
                // Single-coin shape (wrong for expected=2).
                _ => {
                    let mut s = Sketch::default();
                    s.toggle(next() | 1);
                    s
                }
            }
        };
        // Batch sizes straddling both the Montgomery gate (≥2) and the
        // bitslice gate (≥BITSLICE_MIN), plus a multi-block size.
        for n in [0usize, 1, 2, 7, 15, 16, 40, 64, 129] {
            let rs: Vec<Sketch> = (0..n).map(|i| mk_residual(i as u32 % 4)).collect();
            for expected in [0u32, 1, 2, 3] {
                let mut batched = Vec::new();
                decode_batch(&rs, expected, &mut batched);
                let scalar: Vec<Decoded> =
                    rs.iter().map(|&r| decode(r, expected)).collect();
                assert_eq!(batched, scalar, "n={n} expected={expected}");
            }
        }
    }

    #[test]
    #[ignore] // microbenchmark: scalar map vs batched kernel at various sizes
    fn decode_batch_bench() {
        let mut next = rng(17);
        for n in [4usize, 16, 64, 256, 1024] {
            let rs: Vec<Sketch> = (0..n).map(|_| {
                let (a, b) = (next() | 1, next() | 2);
                let mut s = Sketch::default();
                s.toggle(a);
                s.toggle(b);
                s
            }).collect();
            let reps = 200_000 / n.max(1);
            let t = std::time::Instant::now();
            let mut acc = 0u64;
            for _ in 0..reps {
                for &r in &rs {
                    if let Decoded::Two(a, _) = decode(r, 2) { acc ^= a; }
                }
            }
            let scalar = t.elapsed();
            let t = std::time::Instant::now();
            let mut out = Vec::new();
            let mut acc2 = 0u64;
            for _ in 0..reps {
                decode_batch(&rs, 2, &mut out);
                for d in &out {
                    if let Decoded::Two(a, _) = d { acc2 ^= a; }
                }
            }
            let batched = t.elapsed();
            assert_eq!(acc, acc2);
            eprintln!(
                "n={n:>5}: scalar {:>10.1?}  batched {:>10.1?}  ({:.1}x)",
                scalar / reps as u32, batched / reps as u32,
                scalar.as_secs_f64() / batched.as_secs_f64());
        }
    }

    #[test]
    fn mul_known_vectors() {
        // x · x = x²
        assert_eq!(mul(2, 2), 4);
        // x⁶³ · x = x⁶⁴ ≡ x⁴+x³+x+1
        assert_eq!(mul(1 << 63, 2), POLY_LOW);
        // 1 is the multiplicative identity
        assert_eq!(mul(1, 0xDEAD_BEEF_CAFE_F00D), 0xDEAD_BEEF_CAFE_F00D);
        // 0 annihilates
        assert_eq!(mul(0, 0xFFFF_FFFF_FFFF_FFFF), 0);
    }

    #[test]
    fn field_laws_hold() {
        let mut r = rng(1);
        for _ in 0..500 {
            let (a, b, c) = (r(), r(), r());
            assert_eq!(mul(a, b), mul(b, a), "commutativity");
            assert_eq!(mul(a, mul(b, c)), mul(mul(a, b), c), "associativity");
            assert_eq!(mul(a, b ^ c), mul(a, b) ^ mul(a, c), "distributivity over XOR");
            // Freshman's dream: squaring distributes over addition.
            assert_eq!(sq(a ^ b), sq(a) ^ sq(b));
        }
    }

    #[test]
    fn inverse_roundtrip() {
        let mut r = rng(2);
        for _ in 0..200 {
            let a = r() | 1; // nonzero
            assert_eq!(mul(a, inv(a)), 1);
            assert_eq!(div(mul(a, 0x1234_5678_9ABC_DEF7), a), 0x1234_5678_9ABC_DEF7);
        }
    }

    #[test]
    fn quad_solver_roundtrip() {
        let mut r = rng(3);
        let mut solvable = 0;
        for _ in 0..500 {
            let a = r();
            let c = sq(a) ^ a;
            let z = solver().solve(c).expect("c is in the image by construction");
            assert!(z == a || z == a ^ 1, "root must be a or a⊕1");
            solvable += 1;
        }
        assert_eq!(solvable, 500);
        // Roughly half of random values are NOT in the image; check that
        // at least some are rejected (consistency bit works).
        let rejected = (0..500).filter(|_| solver().solve(r()).is_none()).count();
        assert!(rejected > 100, "expected ~half rejections, got {rejected}");
    }

    #[test]
    fn decode_one_unknown() {
        let mut r = rng(4);
        for _ in 0..200 {
            let t = r() | 1;
            let mut s = Sketch::default();
            s.toggle(t);
            assert_eq!(decode(s, 1), Decoded::One(t));
            assert_eq!(decode(s, 0), Decoded::Fail);
        }
        assert_eq!(decode(Sketch::default(), 0), Decoded::None);
    }

    #[test]
    fn decode_two_unknowns_tom_and_sue() {
        let mut r = rng(5);
        for _ in 0..500 {
            let (t, s) = (r() | 1, r() | 1);
            if t == s { continue; }
            let mut sk = Sketch::default();
            sk.toggle(t);
            sk.toggle(s);
            match decode(sk, 2) {
                Decoded::Two(a, b) => {
                    assert!(
                        (a == t && b == s) || (a == s && b == t),
                        "decode must recover exactly the two coins"
                    );
                }
                other => panic!("expected Two, got {other:?}"),
            }
        }
    }

    #[test]
    fn decode_rejects_garbage() {
        let mut r = rng(6);
        let mut fails = 0;
        for _ in 0..300 {
            // A random sketch is almost never a valid 2-coin residual
            // (s3 must satisfy the cube relation through the quadratic).
            let junk = Sketch { s1: r() | 1, s3: r() };
            if matches!(decode(junk, 2), Decoded::Fail) {
                fails += 1;
            }
        }
        // Tr(c)=0 admits ~half; the cube audit rejects the rest of the
        // mismatches only when inconsistent — but a random s3 paired with
        // a random s1 still yields *some* algebraically-consistent pairs
        // (any (e1, e2) is a valid quadratic when Tr passes).  What we
        // must guarantee is rejection of *structural* garbage:
        assert!(fails > 0, "the Tr consistency check must reject some inputs");
        // Sharp rejection: a 3-coin residual decoded as 2 must fail the
        // cube audit (it is not the sketch of any 2-element set... it may
        // rarely alias; assert overwhelmingly common rejection).
        let mut three_fail = 0;
        for _ in 0..300 {
            let mut sk = Sketch::default();
            sk.toggle(r() | 1);
            sk.toggle(r() | 1);
            sk.toggle(r() | 1);
            if matches!(decode(sk, 1), Decoded::Fail) {
                three_fail += 1;
            }
        }
        assert_eq!(three_fail, 300, "a 3-coin residual must never decode as 1");
    }
}
