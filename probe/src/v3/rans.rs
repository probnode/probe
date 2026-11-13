//! Static rANS, the entropy coder of Probe v3 (specification 9.4): 32-bit states, 16-bit
//! renormalisation words, per-context frequency tables fixed per segment with a per-context
//! precision (2^5 .. 2^12), two states interleaved over one word stream, raw bits carried by the
//! same states.
//!
//! Stream layout (one per block, lane or column set): `x0 u32 LE, x1 u32 LE`, then the 16-bit
//! words (LE) in the order the decoder reads them. Operation k of the stream (a symbol or a raw
//! chunk of 1..16 bits) uses state k & 1. A context whose table holds one symbol costs no
//! operation at all. The decoder must consume every word and end with both states at 2^16: a
//! stream that ends early or late is corrupt.
//!
//! The precision of a context is the writer's choice (stored with its table): the smallest
//! 2^log, log in 5..=12, whose coding cost on the segment's counts is within 0.2 % (or 64 bits)
//! of the cost at 2^12. Small tables keep the decoder's working set in the L1 / L2 cache.

use crate::{put_uvarint, uvarint};

/// The largest table log.
pub(crate) const MAX_LOG: u32 = 12;
const MIN_LOG: u32 = 5;
const L: u32 = 1 << 16;

/// One recorded coding operation: a symbol `(ctx << 8) | sym`, or a raw chunk
/// `RAW | n << 16 | value` with n in 1..=16. `RAW` with n = 0 marks a value the coder cannot
/// represent; encoding it fails.
pub(crate) type Op = u32;
pub(crate) const RAW: u32 = 0x8000_0000;
const BAD: Op = RAW;

#[inline(always)]
pub(crate) fn op_sym(ctx: usize, sym: u32) -> Op {
    ((ctx as u32) << 8) | sym
}

/// Appends the operations of an integer u (< 2^63) in context ctx: a bucket symbol
/// (n = bitlen(u + 1) - 1, with the 2 bits after the leading one) and n - 2 raw low bits.
/// A larger u records an operation that makes the encoding fail.
#[inline]
pub(crate) fn put_int(ops: &mut Vec<Op>, ctx: usize, u: u64) {
    if u >= 1 << 63 {
        ops.push(BAD);
        return;
    }
    let x = u + 1;
    let n = 63 - x.leading_zeros();
    if n <= 2 {
        ops.push(op_sym(ctx, n * 4 + (x as u32 & ((1 << n) - 1))));
    } else {
        ops.push(op_sym(ctx, n * 4 + ((x >> (n - 2)) as u32 & 3)));
        put_raw(ops, n - 2, x & ((1u64 << (n - 2)) - 1));
    }
}

/// Appends n raw bits (n <= 64) of v, most significant chunk first.
#[inline]
fn put_raw(ops: &mut Vec<Op>, n: u32, v: u64) {
    let mut left = n;
    while left > 0 {
        let c = left.min(16);
        left -= c;
        ops.push(RAW | (c << 16) | ((v >> left) as u32 & ((1 << c) - 1)));
    }
}

/// Adds the symbol operations of ops to per-context counts.
pub(crate) fn count_ops(counts: &mut [[u64; 256]], ops: &[Op]) {
    for &o in ops {
        if o & RAW == 0 {
            if let Some(c) = counts.get_mut((o >> 8) as usize) {
                c[(o & 0xFF) as usize] += 1;
            }
        }
    }
}

/// Normalises symbol counts to frequencies summing to 2^log (every used symbol at least 1;
/// 2^log must be at least the number of used symbols). Deterministic: floor scaling, the
/// remainder to the largest fractional parts (ties: lower symbol), an excess taken from the
/// largest frequencies (ties: lower symbol).
fn normalize(counts: &[u64; 256], log: u32) -> [u32; 256] {
    let m = 1u32 << log;
    let total: u64 = counts.iter().sum();
    let mut f = [0u32; 256];
    if total == 0 {
        return f;
    }
    let mut sum = 0u32;
    let mut frac = [0u64; 256];
    for s in 0..256 {
        if counts[s] > 0 {
            let q = counts[s] as u128 * m as u128;
            let v = (q / total as u128) as u32;
            frac[s] = (q % total as u128) as u64;
            f[s] = v.max(1);
            sum += f[s];
        }
    }
    if sum < m {
        // the deficit is below the number of used symbols (each floor loses less than 1)
        let mut used: Vec<usize> = (0..256).filter(|&s| counts[s] > 0).collect();
        used.sort_by(|&a, &b| frac[b].cmp(&frac[a]).then(a.cmp(&b)));
        let mut i = 0;
        while sum < m {
            f[used[i % used.len()]] += 1;
            sum += 1;
            i += 1;
        }
    }
    while sum > m {
        let mut b = usize::MAX;
        for s in 0..256 {
            if f[s] > 1 && (b == usize::MAX || f[s] > f[b]) {
                b = s;
            }
        }
        f[b] -= 1;
        sum -= 1;
    }
    f
}

/// floor(log2(v) * 2^16) for v >= 1, in integers (the same on every machine).
fn log2_q16(v: u32) -> u64 {
    let n = 31 - v.leading_zeros();
    let mut y: u64 = ((v as u64) << 30) >> n; // v / 2^n in [1, 2), Q30
    let mut frac = 0u64;
    for i in (0..16).rev() {
        y = (y * y) >> 30;
        if y >= 2 << 30 {
            y >>= 1;
            frac |= 1 << i;
        }
    }
    ((n as u64) << 16) | frac
}

/// Cost (bits x 2^16) to code the counts with frequencies f of total 2^log.
fn cost(counts: &[u64; 256], f: &[u32; 256], log: u32) -> u128 {
    let mut c = 0u128;
    for s in 0..256 {
        if counts[s] > 0 {
            c += counts[s] as u128 * (((log as u64) << 16) - log2_q16(f[s])) as u128;
        }
    }
    c
}

/// The frequency tables of a set of contexts, each with its table log.
#[derive(Clone, Debug)]
pub(crate) struct Tables {
    pub(crate) freq: Vec<[u32; 256]>,
    pub(crate) log: Vec<u32>,
}

impl Tables {
    /// Tables from per-context symbol counts, with the precision rule of the module doc.
    /// Deterministic: integer arithmetic only.
    pub(crate) fn from_counts(counts: &[[u64; 256]]) -> Tables {
        let mut freq = Vec::with_capacity(counts.len());
        let mut logs = Vec::with_capacity(counts.len());
        for c in counts {
            let nsym = c.iter().filter(|&&x| x > 0).count();
            if nsym <= 1 {
                freq.push(normalize(c, MAX_LOG));
                logs.push(MAX_LOG);
                continue;
            }
            let full = normalize(c, MAX_LOG);
            let best = cost(c, &full, MAX_LOG);
            let mut pick = (MAX_LOG, full);
            for log in MIN_LOG..MAX_LOG {
                if (1usize << log) < nsym {
                    continue;
                }
                let f = normalize(c, log);
                if cost(c, &f, log).saturating_sub(best) <= (best / 500).max(64 << 16) {
                    pick = (log, f);
                    break;
                }
            }
            logs.push(pick.0);
            freq.push(pick.1);
        }
        Tables { freq, log: logs }
    }

    /// Serialised: uv(n_contexts), then per context uv(n_symbols) [u8 log] and per symbol
    /// uv(symbol gap) uv(freq - 1).
    pub(crate) fn write(&self, out: &mut Vec<u8>) {
        put_uvarint(out, self.freq.len() as u64);
        for (f, &log) in self.freq.iter().zip(&self.log) {
            let n = f.iter().filter(|&&x| x > 0).count();
            put_uvarint(out, n as u64);
            if n == 0 {
                continue;
            }
            out.push(log as u8);
            let mut prev = 0usize;
            for (s, &x) in f.iter().enumerate() {
                if x > 0 {
                    put_uvarint(out, (s - prev) as u64);
                    put_uvarint(out, (x - 1) as u64);
                    prev = s;
                }
            }
        }
    }

    /// Parses `write`'s bytes; returns the tables and the bytes read.
    pub(crate) fn read(b: &[u8]) -> Result<(Tables, usize), String> {
        let mut p = 0usize;
        let next = |p: &mut usize| -> Result<u64, String> {
            let (v, k) = uvarint(&b[*p..]).ok_or("rans tables: bad varint")?;
            *p += k;
            Ok(v)
        };
        let nctx = next(&mut p)? as usize;
        if nctx > 4096 {
            return Err("rans tables: too many contexts".into());
        }
        let mut freq = Vec::with_capacity(nctx);
        let mut logs = Vec::with_capacity(nctx);
        for _ in 0..nctx {
            let n = next(&mut p)?;
            if n > 256 {
                return Err("rans tables: too many symbols".into());
            }
            let mut f = [0u32; 256];
            let mut log = MAX_LOG;
            if n > 0 {
                if p >= b.len() {
                    return Err("rans tables: cut".into());
                }
                log = b[p] as u32;
                p += 1;
                if !(MIN_LOG..=MAX_LOG).contains(&log) {
                    return Err(format!("rans tables: table log {}", log));
                }
            }
            let mut s = 0u64;
            let mut sum = 0u64;
            for i in 0..n {
                let gap = next(&mut p)?;
                if (i > 0 && gap == 0) || gap > 255 || s + gap > 255 {
                    return Err("rans tables: bad symbol".into());
                }
                s += gap;
                let x = next(&mut p)?;
                if x >= 1 << log {
                    return Err("rans tables: bad frequency".into());
                }
                f[s as usize] = x as u32 + 1;
                sum += x + 1;
            }
            if n > 0 && sum != 1 << log {
                return Err(format!("rans tables: frequencies sum to {}", sum));
            }
            freq.push(f);
            logs.push(log);
        }
        Ok((Tables { freq, log: logs }, p))
    }
}

/// Encoder side of a table set.
pub(crate) struct EncTables {
    freq: Vec<[u32; 256]>,
    cum: Vec<[u32; 256]>,
    log: Vec<u32>,
    single: Vec<bool>,
}

impl EncTables {
    pub(crate) fn new(t: &Tables) -> EncTables {
        let mut cum = Vec::with_capacity(t.freq.len());
        let mut single = Vec::with_capacity(t.freq.len());
        for (f, &log) in t.freq.iter().zip(&t.log) {
            let mut c = [0u32; 256];
            let mut a = 0;
            for s in 0..256 {
                c[s] = a;
                a += f[s];
            }
            cum.push(c);
            single.push(f.iter().any(|&x| x == 1 << log));
        }
        EncTables {
            freq: t.freq.clone(),
            cum,
            log: t.log.clone(),
            single,
        }
    }

    /// Encodes ops (in decoder order) into one stream appended to out. Errors on a symbol
    /// without frequency and on a value the coder cannot represent.
    pub(crate) fn encode(&self, ops: &[Op], out: &mut Vec<u8>) -> Result<(), String> {
        let mut kept: Vec<Op> = Vec::with_capacity(ops.len());
        for &o in ops {
            if o == BAD {
                return Err("rans: integer out of range (>= 2^63)".into());
            }
            if o & RAW != 0 {
                kept.push(o);
                continue;
            }
            match self.single.get((o >> 8) as usize) {
                Some(true) => {}
                Some(false) => kept.push(o),
                None => return Err(format!("rans: context {} out of range", o >> 8)),
            }
        }
        let mut x = [L, L];
        let mut words: Vec<u16> = Vec::with_capacity(kept.len());
        for k in (0..kept.len()).rev() {
            let o = kept[k];
            let st = k & 1;
            let mut s = x[st];
            if o & RAW != 0 {
                let n = (o >> 16) & 31;
                let v = o & 0xFFFF;
                let xmax = 1u64 << (32 - n);
                if s as u64 >= xmax {
                    words.push(s as u16);
                    s >>= 16;
                }
                s = (s << n) | v;
            } else {
                let ctx = (o >> 8) as usize;
                let sym = (o & 0xFF) as usize;
                let f = self.freq[ctx][sym];
                if f == 0 {
                    return Err(format!(
                        "rans: symbol {} without frequency in context {}",
                        sym, ctx
                    ));
                }
                let log = self.log[ctx];
                let xmax = (f as u64) << (32 - log);
                if s as u64 >= xmax {
                    words.push(s as u16);
                    s >>= 16;
                }
                s = ((s / f) << log) + (s % f) + self.cum[ctx][sym];
            }
            x[st] = s;
        }
        out.extend_from_slice(&x[0].to_le_bytes());
        out.extend_from_slice(&x[1].to_le_bytes());
        for w in words.iter().rev() {
            out.extend_from_slice(&w.to_le_bytes());
        }
        Ok(())
    }
}

/// One context of the decoder: its slot table's offset, its log, and the symbol of a
/// single-symbol context (>= 0), -1 = table, -2 = unused.
#[derive(Clone, Copy)]
struct Ctx {
    base: u32,
    log: u32,
    single: i32,
}

/// Decoder side of a table set: per context with two or more symbols a packed slot table of
/// 2^log entries (`sym | (freq - 1) << 8 | (slot - cum) << 20`).
pub(crate) struct DecTables {
    tab: Vec<u32>,
    ctx: Vec<Ctx>,
}

impl DecTables {
    pub(crate) fn new(t: &Tables) -> DecTables {
        let mut tab = Vec::new();
        let mut ctx = Vec::with_capacity(t.freq.len());
        for (f, &log) in t.freq.iter().zip(&t.log) {
            let used = f.iter().filter(|&&x| x > 0).count();
            if used == 0 {
                ctx.push(Ctx {
                    base: 0,
                    log,
                    single: -2,
                });
                continue;
            }
            if used == 1 {
                let s = f.iter().position(|&x| x > 0).unwrap_or(0);
                ctx.push(Ctx {
                    base: 0,
                    log,
                    single: s as i32,
                });
                continue;
            }
            ctx.push(Ctx {
                base: tab.len() as u32,
                log,
                single: -1,
            });
            for (s, &x) in f.iter().enumerate() {
                for k in 0..x {
                    tab.push(s as u32 | ((x - 1) << 8) | (k << 20));
                }
            }
        }
        DecTables { tab, ctx }
    }
}

/// A stream decoder over a byte slice.
///
/// The two states rotate: an operation decodes with `x0`, then `x0 = x1, x1 = new`, so operation
/// k uses the state of operation k - 2 (= state k & 1 of the encoder), and both stay in registers.
/// Corrupt input never panics: an unused context or a context number past the table set marks
/// the decoder bad, and reading past the end shows in `overrun` / `finish`.
pub(crate) struct Dec<'a> {
    x0: u32,
    x1: u32,
    buf: &'a [u8],
    pos: usize,
    t: &'a DecTables,
    pub(crate) bad: bool,
}

impl<'a> Dec<'a> {
    pub(crate) fn new(buf: &'a [u8], t: &'a DecTables) -> Dec<'a> {
        let mut d = Dec {
            x0: L,
            x1: L,
            buf,
            pos: 8,
            t,
            bad: false,
        };
        if buf.len() < 8 {
            d.bad = true;
            d.pos = buf.len();
            return d;
        }
        d.x0 = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        d.x1 = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
        if d.x0 < L || d.x1 < L {
            d.bad = true;
        }
        d
    }

    /// Renormalises y without a data-dependent branch: the next word is read always (0 past
    /// the end) and taken when y < 2^16. Reading past the end shows in finish() / overrun().
    #[inline(always)]
    fn renorm(&mut self, y: u32) -> u32 {
        let p = self.pos;
        let w = match self.buf.get(p..p + 2) {
            Some(b) => u16::from_le_bytes([b[0], b[1]]) as u32,
            None => 0,
        };
        let need = y < L;
        self.pos = p + 2 * need as usize;
        if need {
            (y << 16) | w
        } else {
            y
        }
    }

    /// The decoder has read past the end of its stream, or met an impossible symbol.
    #[inline(always)]
    pub(crate) fn broken(&self) -> bool {
        self.bad || self.pos > self.buf.len()
    }

    /// One symbol of context ctx.
    #[inline(always)]
    pub(crate) fn get(&mut self, ctx: usize) -> u32 {
        let c = match self.t.ctx.get(ctx) {
            Some(c) => *c,
            None => {
                self.bad = true;
                return 0;
            }
        };
        if c.single >= 0 {
            return c.single as u32;
        }
        if c.single == -2 {
            self.bad = true;
            return 0;
        }
        let x = self.x0;
        let e = self.t.tab[c.base as usize + (x & ((1 << c.log) - 1)) as usize];
        let y = self.renorm((((e >> 8) & 0xFFF) + 1) * (x >> c.log) + (e >> 20));
        self.x0 = self.x1;
        self.x1 = y;
        e & 0xFF
    }

    /// n raw bits, 1 <= n <= 16.
    #[inline(always)]
    fn raw16(&mut self, n: u32) -> u32 {
        let x = self.x0;
        let v = x & ((1 << n) - 1);
        let y = self.renorm(x >> n);
        self.x0 = self.x1;
        self.x1 = y;
        v
    }

    /// n raw bits, n <= 64.
    #[inline(always)]
    fn raw(&mut self, n: u32) -> u64 {
        if n <= 16 {
            return self.raw16(n) as u64;
        }
        let mut v = 0u64;
        let mut left = n;
        while left > 0 {
            let c = left.min(16);
            left -= c;
            v = (v << c) | self.raw16(c) as u64;
        }
        v
    }

    /// An integer of put_int.
    #[inline(always)]
    pub(crate) fn int(&mut self, ctx: usize) -> u64 {
        let s = self.get(ctx);
        let n = s >> 2;
        let m = (s & 3) as u64;
        let x = if n <= 2 {
            (1u64 << n) | m
        } else {
            ((4 | m) << (n - 2)) | self.raw(n - 2)
        };
        x.wrapping_sub(1)
    }

    /// Every word consumed, states back at the encoder's start.
    pub(crate) fn finish(&self) -> bool {
        !self.broken() && self.pos == self.buf.len() && self.x0 == L && self.x1 == L
    }
}

/// Codes columns of integers (u64 < 2^63) as one stream: each column is one context, values in
/// row-major order. Layout: uv(rows), tables, then the stream (codec 18).
pub(crate) fn encode_columns(cols: &[Vec<u64>]) -> Result<Vec<u8>, String> {
    let n = cols.first().map(|c| c.len()).unwrap_or(0);
    if cols.iter().any(|c| c.len() != n) {
        return Err("columns: lengths differ".into());
    }
    let mut ops = Vec::with_capacity(n * cols.len() * 2);
    for i in 0..n {
        for (c, col) in cols.iter().enumerate() {
            put_int(&mut ops, c, col[i]);
        }
    }
    let mut counts = vec![[0u64; 256]; cols.len()];
    count_ops(&mut counts, &ops);
    let t = Tables::from_counts(&counts);
    let mut out = Vec::new();
    put_uvarint(&mut out, n as u64);
    t.write(&mut out);
    EncTables::new(&t).encode(&ops, &mut out)?;
    Ok(out)
}

/// Decodes encode_columns' bytes into ncols columns.
pub(crate) fn decode_columns(b: &[u8], ncols: usize) -> Result<Vec<Vec<u64>>, String> {
    let (n, k) = uvarint(b).ok_or("columns: bad count")?;
    let (t, k2) = Tables::read(&b[k..])?;
    if t.freq.len() != ncols {
        return Err(format!(
            "columns: {} contexts, want {}",
            t.freq.len(),
            ncols
        ));
    }
    if n > b.len() as u64 * 64 + 64 {
        return Err("columns: count too large".into());
    }
    let dt = DecTables::new(&t);
    let mut d = Dec::new(&b[k + k2..], &dt);
    let mut cols: Vec<Vec<u64>> = (0..ncols).map(|_| Vec::with_capacity(n as usize)).collect();
    for _ in 0..n {
        for (c, col) in cols.iter_mut().enumerate() {
            col.push(d.int(c));
        }
        if d.broken() {
            return Err("columns: corrupt stream".into());
        }
    }
    if !d.finish() {
        return Err("columns: stream not consumed exactly".into());
    }
    Ok(cols)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_roundtrip() {
        let mut cols = vec![Vec::new(), Vec::new(), Vec::new()];
        let mut x: u64 = 12345;
        for i in 0..5000u64 {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            cols[0].push(x >> 60);
            cols[1].push(if i % 7 == 0 { x >> 3 } else { i % 3 });
            cols[2].push(7);
        }
        cols[1][17] = (1 << 62) + 5;
        let b = encode_columns(&cols).unwrap();
        assert_eq!(decode_columns(&b, 3).unwrap(), cols);
        let empty: Vec<Vec<u64>> = vec![Vec::new(), Vec::new()];
        let b = encode_columns(&empty).unwrap();
        assert_eq!(decode_columns(&b, 2).unwrap(), empty);
    }

    #[test]
    fn out_of_range_integer_is_an_error() {
        assert!(encode_columns(&[vec![1u64 << 63]]).is_err());
    }

    #[test]
    fn corrupt_columns_never_panic() {
        let cols = vec![(0..300u64).collect::<Vec<_>>(), vec![5; 300]];
        let b = encode_columns(&cols).unwrap();
        for k in 0..b.len() {
            for x in [0x01u8, 0x80, 0xFF] {
                let mut c = b.clone();
                c[k] ^= x;
                let _ = decode_columns(&c, 2);
            }
            let _ = decode_columns(&b[..k], 2);
        }
    }
}
