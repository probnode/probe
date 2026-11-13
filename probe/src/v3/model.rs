//! Models M1 and M2 of Probe v3 (specification 9.5, 9.6): how one block (one market, one segment)
//! becomes coding operations, and back. They keep v1's prediction structure (the block's book,
//! best prices, 8-entry size caches, per-token last prices) but code multi-symbol values with
//! per-segment static tables: event type, clocks, counts, prices and sizes are each one bucket
//! symbol plus raw low bits, and the binary decisions of a level are joined into one symbol.
//!
//! The encoders (`State::enc_event`, `State::enc_body`, `ColState::enc`) and the decoders
//! (`State::dec_event`, `State::dec_body`, `ColState::dec`) must stay mirror images: every context
//! and every state update in one has its twin in the other.

use super::rans::{op_sym, put_int, Dec, Op};
use crate::walker::{
    CoreEvent, CoreLevel, ET_BOOK, ET_CHECKPOINT, ET_OTHER, ET_PRICE_CHG, ET_TICK_SIZE, ET_TRADE,
};

// Contexts of model M1 (stable numbering; the registry pins it with codec 16).
const C_ET: usize = 0; // [prev et 6]
const C_DTS: usize = C_ET + 6; // [et 6][bucket 3]
const C_LAT: usize = C_DTS + 18; // [et 4]
const C_CONT: usize = C_LAT + 4; // [prev cont 2]
const C_OUT: usize = C_CONT + 2; // [et 6][prev out 2]
const C_NLV: usize = C_OUT + 12; // [et0 / et5][side]
const C_BOOKDP: usize = C_NLV + 4; // [kind 3][side 2][first 2]
const C_SZMODE: usize = C_BOOKDP + 12; // [kind 3][side 2][cur > 0]: 0 abs lit, 1 abs hit, 2 delta lit, 3 delta hit
const C_SZIDX: usize = C_SZMODE + 12; // [kind 3][path 2]
const C_SZDEC: usize = C_SZIDX + 6; // [kind 3][path 2][side 2]: trailing decimal zeros of a literal (0..=6)
const C_SZLIT: usize = C_SZDEC + 12; // [kind 3][path 2][side 2][min(zeros, 3)]: the literal / 10^zeros
const C_NENT: usize = C_SZLIT + 48;
const C_MIRROR: usize = C_NENT + 1; // [later entry]
const C_PCHEAD: usize = C_MIRROR + 2; // [prev out][prev side tok 0][prev side tok 1]: out * 2 + side
const C_PCPRICE: usize = C_PCHEAD + 8; // [side 2][bucket of the token's previous |d| 3]
const C_TRSIDE: usize = C_PCPRICE + 6; // [tok 2][prev trade side 2]
const C_TRPRICE: usize = C_TRSIDE + 4;
const C_TRSIZE: usize = C_TRPRICE + 1;
const C_TRFEE: usize = C_TRSIZE + 1;
/// Number of contexts of model M1 (160).
pub(crate) const N_CTX: usize = C_TRFEE + 1;

/// Model M2: joint (event type, outcome, cont) symbol `et * 4 + out * 2 + cont`:
/// [prev et 6][last out 2].
const C2_EOC: usize = N_CTX;
/// Model M2: joint SIZE symbol (0 abs literal, 1 delta literal, 2 + k abs hit, 10 + k delta hit):
/// [kind 3][side 2][level exists 2].
const C2_SZM: usize = C2_EOC + 12;
/// Number of contexts of model M2 (184: M1's, plus the two joint ones).
pub(crate) const N_CTX2: usize = C2_SZM + 12;

#[inline(always)]
fn zz(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

#[inline(always)]
fn unzz(u: u64) -> i64 {
    ((u >> 1) as i64) ^ -((u & 1) as i64)
}

#[inline(always)]
fn dts_bucket(prev: u64) -> usize {
    if prev == 0 {
        0
    } else if prev < 1024 {
        1
    } else {
        2
    }
}

/// The most trailing decimal zeros a size literal is split by.
const MAX_DEC: usize = 6;
const POW10: [u64; MAX_DEC + 1] = [1, 10, 100, 1_000, 10_000, 100_000, 1_000_000];

/// (e, v / 10^e) with e = trailing decimal zeros of v, at most MAX_DEC (0 for v = 0).
#[inline]
fn dec_split(v: i64) -> (usize, i64) {
    let mut e = 0;
    let mut q = v;
    while e < MAX_DEC && q != 0 && q % 10 == 0 {
        q /= 10;
        e += 1;
    }
    (e, q)
}

#[inline(always)]
fn dp_bucket(prev: u64) -> usize {
    if prev == 0 {
        0
    } else if prev <= 2 {
        1
    } else {
        2
    }
}

/// Event types that carry an outcome bit of their own.
#[inline(always)]
fn has_out(et: usize) -> bool {
    et == ET_BOOK as usize
        || et == ET_TRADE as usize
        || et == ET_TICK_SIZE as usize
        || et == ET_CHECKPOINT as usize
}

/// An 8-entry move-to-front size cache: a value moves to the front, a new value is inserted at the
/// front and entry 7 drops.
#[derive(Clone, Copy)]
struct Mtf {
    v: [i64; 8],
    n: usize,
}

impl Mtf {
    const EMPTY: Mtf = Mtf { v: [0; 8], n: 0 };
    #[inline]
    fn find(&self, v: i64) -> Option<usize> {
        (0..self.n).find(|&i| self.v[i] == v)
    }
    #[inline]
    fn push(&mut self, v: i64) {
        let mut i = 0;
        while i < self.n && self.v[i] != v {
            i += 1;
        }
        if i == self.n {
            if self.n < 8 {
                self.n += 1;
            }
            i = self.n - 1;
        }
        self.v.copy_within(0..i, 1);
        self.v[0] = v;
    }
}

/// The fields an event type does not carry must be zero, or a transcode would not be lossless.
pub(crate) fn check_fields(e: &CoreEvent) -> Result<(), String> {
    let et = e.et as usize;
    let ok = et <= ET_CHECKPOINT as usize
        && (et == ET_BOOK as usize || e.cont == 0)
        && (et == ET_TRADE as usize || e.fee == 0)
        && (et <= ET_TICK_SIZE as usize || e.lat == 0)
        && (et != ET_CHECKPOINT as usize || e.ts == 0)
        && (!(et == ET_PRICE_CHG as usize || et == ET_OTHER as usize) || e.out == 0)
        && e.out <= 1
        && e.cont <= 1
        && e.fee >= 0;
    if ok {
        Ok(())
    } else {
        Err(format!("event {:?} carries a field its type has not", e))
    }
}

/// The prediction state of one block, shared by encoder and decoder.
pub(crate) struct State {
    u: i64,
    prev_et: usize,
    prev_ts: i64,
    prev_dts: u64,
    prev_lat: [i64; 4],
    prev_cont: usize,
    prev_out: [usize; 6],
    prev_pc_out: usize,
    prev_side: [usize; 2],
    prev_tr_sd: [usize; 2],
    prev_dp: [u64; 2],
    last_px: [i64; 2],
    last_trade: [i64; 2],
    book: [[Vec<i64>; 2]; 2],
    best: [[i64; 2]; 2],
    cache: [[Mtf; 2]; 2],
    touched: Vec<u32>,
    /// model M2: SIZE as one joint symbol (mode + cache index)
    m2: bool,
    /// the decoder refuses to produce more levels than this (the block index's count)
    lv_cap: usize,
}

impl State {
    pub(crate) fn new() -> State {
        State {
            u: 1,
            prev_et: 0,
            prev_ts: 0,
            prev_dts: 0,
            prev_lat: [0; 4],
            prev_cont: 0,
            prev_out: [0; 6],
            prev_pc_out: 0,
            prev_side: [0; 2],
            prev_tr_sd: [0; 2],
            prev_dp: [0; 2],
            last_px: [0; 2],
            last_trade: [0; 2],
            book: Default::default(),
            best: [[0; 2]; 2],
            cache: [[Mtf::EMPTY; 2]; 2],
            touched: Vec::new(),
            m2: false,
            lv_cap: usize::MAX,
        }
    }

    /// Block start, model M1. `u` must be in 1..=2^20 (the caller checks).
    pub(crate) fn reset(&mut self, u: i64, ts0: i64, lv_cap: usize) {
        self.m2 = false;
        self.lv_cap = lv_cap;
        self.u = u;
        self.prev_et = 0;
        self.prev_ts = ts0;
        self.prev_dts = 0;
        self.prev_lat = [0; 4];
        self.prev_cont = 0;
        self.prev_out = [0; 6];
        self.prev_pc_out = 0;
        self.prev_side = [0; 2];
        self.prev_tr_sd = [0; 2];
        self.prev_dp = [0; 2];
        self.last_px = [0; 2];
        self.last_trade = [0; 2];
        for &k in &self.touched {
            let ts = (k >> 24) as usize;
            let p = (k & 0xFF_FFFF) as usize;
            if let Some(slot) = self.book[ts >> 1][ts & 1].get_mut(p) {
                *slot = 0;
            }
        }
        self.touched.clear();
        let n = u as usize + 1;
        for t in 0..2 {
            for s in 0..2 {
                if self.book[t][s].len() < n {
                    self.book[t][s].resize(n, 0);
                }
                self.cache[t][s] = Mtf::EMPTY;
            }
            self.best[t] = [-1, u + 1];
        }
    }

    /// Block start, model M2 (the event-level fields live in the segment's columns).
    pub(crate) fn reset_m2(&mut self, u: i64, lv_cap: usize) {
        self.reset(u, 0, lv_cap);
        self.m2 = true;
    }

    #[inline]
    fn has(&self, tok: usize, side: usize) -> bool {
        let b = self.best[tok][side];
        b >= 0 && b <= self.u
    }

    /// Writes a level and keeps the side's best price. `p` is in `[0, U]`.
    #[inline]
    fn set(&mut self, tok: usize, side: usize, p: i64, z: i64) {
        let pi = p as usize;
        let b = &mut self.book[tok][side];
        if b[pi] == 0 && z != 0 {
            self.touched
                .push(((((tok << 1) | side) as u32) << 24) | p as u32);
        }
        b[pi] = z;
        let best = &mut self.best[tok][side];
        if side == 0 {
            if z > 0 && p > *best {
                *best = p;
            } else if z == 0 && p == *best {
                while *best >= 0 && b[*best as usize] == 0 {
                    *best -= 1;
                }
            }
        } else if z > 0 && p < *best {
            *best = p;
        } else if z == 0 && p == *best {
            while *best <= self.u && b[*best as usize] == 0 {
                *best += 1;
            }
        }
    }

    /// The price an explicit price_change entry is coded against, and the sign:
    /// p = base + sign * d.
    #[inline]
    fn pc_base(&self, out: usize, side: usize) -> (i64, i64) {
        if self.has(out, side) {
            if side == 0 {
                (self.best[out][0], -1)
            } else {
                (self.best[out][1], 1)
            }
        } else {
            (self.last_px[out], 1)
        }
    }

    // ---------------------------------------------------------------- encoder

    /// The SIZE of a level (tok, side, p) with true size z: chooses the cheapest of absolute /
    /// delta x cache hit / literal. Every choice decodes to the same size.
    fn enc_size(
        &mut self,
        ops: &mut Vec<Op>,
        kind: usize,
        tok: usize,
        side: usize,
        p: i64,
        z: i64,
    ) {
        let cur = self.book[tok][side][p as usize];
        let has = (cur > 0) as usize;
        let h0 = self.cache[tok][0].find(z);
        let (h1, v1) = if cur > 0 {
            (self.cache[tok][1].find(z - cur), z - cur)
        } else {
            (None, 0)
        };
        // mode: 0 abs lit, 1 abs hit (index a), 2 delta lit, 3 delta hit (index b)
        let (mode, v, hit) = match (h0, h1) {
            (Some(a), Some(b)) => {
                if b < a {
                    (3u32, v1, b)
                } else {
                    (1, z, a)
                }
            }
            (Some(a), None) => (1, z, a),
            (None, Some(b)) => (3, v1, b),
            (None, None) => {
                if cur > 0 && 64 - zz(v1).leading_zeros() < 64 - (z as u64).leading_zeros() {
                    (2, v1, 0)
                } else {
                    (0, z, 0)
                }
            }
        };
        let path = mode >> 1;
        if self.m2 {
            // one symbol: 0 abs literal, 1 delta literal, 2 + k abs hit k, 10 + k delta hit k
            let sym = match mode {
                1 => 2 + hit as u32,
                3 => 10 + hit as u32,
                m => m >> 1,
            };
            ops.push(op_sym(C2_SZM + kind * 4 + side * 2 + has, sym));
        } else {
            ops.push(op_sym(C_SZMODE + kind * 4 + side * 2 + has, mode));
            if mode == 1 || mode == 3 {
                put_int(ops, C_SZIDX + kind * 2 + path as usize, hit as u64);
            }
        }
        if mode == 0 || mode == 2 {
            // a literal: e = its trailing decimal zeros (sizes are decimal: 100 shares, 5.00),
            // then the quotient (zigzag for a delta) in a context of min(e, 3)
            let k = kind * 4 + (path as usize) * 2 + side;
            let (e, q) = dec_split(v);
            ops.push(op_sym(C_SZDEC + k, e as u32));
            put_int(
                ops,
                C_SZLIT + k * 4 + e.min(3),
                if path == 0 { q as u64 } else { zz(q) },
            );
        }
        self.cache[tok][path as usize].push(v);
    }

    fn enc_book(
        &mut self,
        ops: &mut Vec<Op>,
        e: &CoreEvent,
        lv: &[CoreLevel],
    ) -> Result<(), String> {
        let (kind, k5) = if e.et == ET_CHECKPOINT {
            (2usize, 1usize)
        } else {
            (0, 0)
        };
        let tok = e.out as usize;
        let nb = lv.iter().take_while(|l| l.side == 0).count();
        let na = lv.len() - nb;
        put_int(ops, C_NLV + k5 * 2, nb as u64);
        put_int(ops, C_NLV + k5 * 2 + 1, na as u64);
        for (side, part) in [(0usize, &lv[..nb]), (1, &lv[nb..])] {
            let mut prev: i64 = -1;
            for (i, l) in part.iter().enumerate() {
                if l.side as usize != side
                    || l.out as usize != tok
                    || l.mirror
                    || l.p <= prev
                    || l.p > self.u
                    || l.z < 0
                {
                    return Err(format!("book level {:?} not in the v1 shape", l));
                }
                let first = (i == 0) as usize;
                put_int(
                    ops,
                    C_BOOKDP + kind * 4 + side * 2 + first,
                    (l.p - prev - 1) as u64,
                );
                prev = l.p;
                self.enc_size(ops, kind, tok, side, l.p, l.z);
                self.set(tok, side, l.p, l.z);
            }
        }
        Ok(())
    }

    fn enc_pc(&mut self, ops: &mut Vec<Op>, lv: &[CoreLevel]) -> Result<(), String> {
        put_int(ops, C_NENT, lv.len() as u64);
        let mut prev_explicit: Option<CoreLevel> = None;
        for (i, l) in lv.iter().enumerate() {
            if let Some(e) = prev_explicit {
                ops.push(op_sym(C_MIRROR + (i > 1) as usize, l.mirror as u32));
                if l.mirror {
                    let m = CoreLevel {
                        out: 1 - e.out,
                        side: 1 - e.side,
                        p: self.u - e.p,
                        z: e.z,
                        mirror: true,
                    };
                    if *l != m {
                        return Err(format!("mirror level {:?} is not the mirror of {:?}", l, e));
                    }
                    self.set(m.out as usize, m.side as usize, m.p, m.z);
                    prev_explicit = None;
                    continue;
                }
            } else if l.mirror {
                return Err("mirror level without an explicit entry before it".into());
            }
            let (out, side) = (l.out as usize, l.side as usize);
            if out > 1 || side > 1 || l.p < 0 || l.p > self.u || l.z < 0 {
                return Err(format!("price_change level {:?} out of range", l));
            }
            let ctx = C_PCHEAD + self.prev_pc_out * 4 + self.prev_side[0] * 2 + self.prev_side[1];
            ops.push(op_sym(ctx, (out * 2 + side) as u32));
            self.prev_pc_out = out;
            self.prev_side[out] = side;
            let (base, sg) = self.pc_base(out, side);
            let d = (l.p - base) * sg;
            put_int(
                ops,
                C_PCPRICE + side * 3 + dp_bucket(self.prev_dp[out]),
                zz(d),
            );
            self.prev_dp[out] = d.unsigned_abs();
            self.last_px[out] = l.p;
            self.last_px[1 - out] = self.u - l.p;
            self.enc_size(ops, 1, out, side, l.p, l.z);
            self.set(out, side, l.p, l.z);
            prev_explicit = Some(*l);
        }
        Ok(())
    }

    fn enc_trade(
        &mut self,
        ops: &mut Vec<Op>,
        e: &CoreEvent,
        lv: &[CoreLevel],
    ) -> Result<(), String> {
        if lv.len() != 1 || lv[0].out != e.out || lv[0].mirror || lv[0].side > 1 || lv[0].z < 0 {
            return Err("trade without its one level".into());
        }
        let tok = e.out as usize;
        let l = lv[0];
        ops.push(op_sym(
            C_TRSIDE + tok * 2 + self.prev_tr_sd[tok],
            l.side as u32,
        ));
        self.prev_tr_sd[tok] = l.side as usize;
        put_int(ops, C_TRPRICE, zz(l.p.wrapping_sub(self.last_trade[tok])));
        self.last_trade[tok] = l.p;
        put_int(ops, C_TRSIZE, l.z as u64);
        put_int(ops, C_TRFEE, e.fee as u64);
        Ok(())
    }

    /// The book-dependent part of an event: levels, sizes, trades.
    fn enc_levels(
        &mut self,
        ops: &mut Vec<Op>,
        e: &CoreEvent,
        lv: &[CoreLevel],
    ) -> Result<(), String> {
        match e.et {
            ET_BOOK | ET_CHECKPOINT => self.enc_book(ops, e, lv),
            ET_PRICE_CHG => self.enc_pc(ops, lv),
            ET_TRADE => self.enc_trade(ops, e, lv),
            _ if lv.is_empty() => Ok(()),
            _ => Err(format!("event type {} with levels", e.et)),
        }
    }

    /// One event with its levels (model M1).
    pub(crate) fn enc_event(
        &mut self,
        ops: &mut Vec<Op>,
        e: &CoreEvent,
        lv: &[CoreLevel],
    ) -> Result<(), String> {
        check_fields(e)?;
        let et = e.et as usize;
        ops.push(op_sym(C_ET + self.prev_et, et as u32));
        self.prev_et = et;
        if et != ET_CHECKPOINT as usize {
            let u = zz(e.ts.wrapping_sub(self.prev_ts));
            put_int(ops, C_DTS + et * 3 + dts_bucket(self.prev_dts), u);
            self.prev_dts = u;
            self.prev_ts = e.ts;
        }
        if et <= ET_TICK_SIZE as usize {
            put_int(ops, C_LAT + et, zz(e.lat.wrapping_sub(self.prev_lat[et])));
            self.prev_lat[et] = e.lat;
        }
        if et == ET_BOOK as usize {
            ops.push(op_sym(C_CONT + self.prev_cont, e.cont as u32));
            self.prev_cont = e.cont as usize;
        }
        if has_out(et) {
            ops.push(op_sym(C_OUT + et * 2 + self.prev_out[et], e.out as u32));
            self.prev_out[et] = e.out as usize;
        }
        self.enc_levels(ops, e, lv)
    }

    /// The block part of one event (model M2; reset_m2 first).
    pub(crate) fn enc_body(
        &mut self,
        ops: &mut Vec<Op>,
        e: &CoreEvent,
        lv: &[CoreLevel],
    ) -> Result<(), String> {
        self.enc_levels(ops, e, lv)
    }

    // ---------------------------------------------------------------- decoder

    #[inline]
    fn dec_size(
        &mut self,
        d: &mut Dec,
        kind: usize,
        tok: usize,
        side: usize,
        p: i64,
    ) -> Option<i64> {
        let cur = self.book[tok][side][p as usize];
        let has = (cur > 0) as usize;
        let mode = if self.m2 {
            let s = d.get(C2_SZM + kind * 4 + side * 2 + has);
            if s >= 18 {
                return None;
            }
            if s >= 2 {
                // a cache hit
                let path = (s >= 10) as usize;
                let i = (s - 2 - 8 * path as u32) as usize;
                let c = &self.cache[tok][path];
                if i >= c.n {
                    return None;
                }
                let v = c.v[i];
                self.cache[tok][path].push(v);
                let z = if path == 1 { cur.checked_add(v)? } else { v };
                if z < 0 || (path == 1 && cur == 0) {
                    return None;
                }
                return Some(z);
            }
            s * 2 // literal: mode 0 (abs) or 2 (delta)
        } else {
            d.get(C_SZMODE + kind * 4 + side * 2 + has)
        };
        if mode > 3 {
            return None;
        }
        let path = (mode >> 1) as usize;
        let v = match mode {
            1 | 3 => {
                let i = d.int(C_SZIDX + kind * 2 + path);
                let c = &self.cache[tok][path];
                if i >= c.n as u64 {
                    return None;
                }
                c.v[i as usize]
            }
            _ => {
                let k = kind * 4 + path * 2 + side;
                let e = d.get(C_SZDEC + k) as usize;
                if e > MAX_DEC {
                    return None;
                }
                let u = d.int(C_SZLIT + k * 4 + e.min(3));
                if u > (1u64 << 62) / POW10[e] {
                    return None;
                }
                let q = if path == 0 { u as i64 } else { unzz(u) };
                q * POW10[e] as i64
            }
        };
        self.cache[tok][path].push(v);
        let z = if path == 1 { cur.checked_add(v)? } else { v };
        if z < 0 || (path == 1 && cur == 0) {
            return None;
        }
        Some(z)
    }

    fn dec_book(
        &mut self,
        d: &mut Dec,
        et: u8,
        tok: usize,
        lvs: &mut Vec<CoreLevel>,
    ) -> Option<()> {
        let (kind, k5) = if et == ET_CHECKPOINT {
            (2usize, 1usize)
        } else {
            (0, 0)
        };
        let nb = d.int(C_NLV + k5 * 2);
        let na = d.int(C_NLV + k5 * 2 + 1);
        let lim = (self.u + 1) as u64;
        if nb > lim || na > lim {
            return None;
        }
        for (side, cnt) in [nb, na].into_iter().enumerate() {
            let mut p: i64 = -1;
            for i in 0..cnt {
                let first = (i == 0) as usize;
                let dp = d.int(C_BOOKDP + kind * 4 + side * 2 + first);
                if dp > self.u as u64 {
                    return None;
                }
                p += dp as i64 + 1;
                if p > self.u || lvs.len() >= self.lv_cap {
                    return None;
                }
                let z = self.dec_size(d, kind, tok, side, p)?;
                self.set(tok, side, p, z);
                lvs.push(CoreLevel {
                    out: tok as u8,
                    side: side as u8,
                    p,
                    z,
                    mirror: false,
                });
            }
        }
        Some(())
    }

    fn dec_pc(&mut self, d: &mut Dec, lvs: &mut Vec<CoreLevel>) -> Option<()> {
        let n = d.int(C_NENT);
        if n > 1 << 24 {
            return None;
        }
        let mut prev_explicit: Option<usize> = None;
        for i in 0..n {
            if lvs.len() >= self.lv_cap || d.broken() {
                return None;
            }
            if let Some(pe) = prev_explicit {
                if d.get(C_MIRROR + (i > 1) as usize) == 1 {
                    let e = lvs[pe];
                    let m = CoreLevel {
                        out: 1 - e.out,
                        side: 1 - e.side,
                        p: self.u - e.p,
                        z: e.z,
                        mirror: true,
                    };
                    self.set(m.out as usize, m.side as usize, m.p, m.z);
                    lvs.push(m);
                    prev_explicit = None;
                    continue;
                }
            }
            let ctx = C_PCHEAD + self.prev_pc_out * 4 + self.prev_side[0] * 2 + self.prev_side[1];
            let h = d.get(ctx) as usize;
            let (out, side) = ((h >> 1) & 1, h & 1);
            self.prev_pc_out = out;
            self.prev_side[out] = side;
            let dd = unzz(d.int(C_PCPRICE + side * 3 + dp_bucket(self.prev_dp[out])));
            self.prev_dp[out] = dd.unsigned_abs();
            let (base, sg) = self.pc_base(out, side);
            let p = base.wrapping_add(dd.wrapping_mul(sg));
            if p < 0 || p > self.u {
                return None;
            }
            self.last_px[out] = p;
            self.last_px[1 - out] = self.u - p;
            let z = self.dec_size(d, 1, out, side, p)?;
            self.set(out, side, p, z);
            prev_explicit = Some(lvs.len());
            lvs.push(CoreLevel {
                out: out as u8,
                side: side as u8,
                p,
                z,
                mirror: false,
            });
        }
        Some(())
    }

    fn dec_trade(
        &mut self,
        d: &mut Dec,
        ev: &mut CoreEvent,
        lvs: &mut Vec<CoreLevel>,
    ) -> Option<()> {
        let tok = ev.out as usize;
        let side = d.get(C_TRSIDE + tok * 2 + self.prev_tr_sd[tok]) as usize & 1;
        self.prev_tr_sd[tok] = side;
        self.last_trade[tok] = self.last_trade[tok].wrapping_add(unzz(d.int(C_TRPRICE)));
        let z = d.int(C_TRSIZE);
        let fee = d.int(C_TRFEE);
        if z > 1 << 62 || fee > i64::MAX as u64 || lvs.len() >= self.lv_cap {
            return None;
        }
        ev.fee = fee as i64;
        lvs.push(CoreLevel {
            out: tok as u8,
            side: side as u8,
            p: self.last_trade[tok],
            z: z as i64,
            mirror: false,
        });
        Some(())
    }

    fn dec_levels(
        &mut self,
        d: &mut Dec,
        ev: &mut CoreEvent,
        lvs: &mut Vec<CoreLevel>,
    ) -> Option<()> {
        ev.lv0 = lvs.len() as u32;
        match ev.et {
            ET_BOOK | ET_CHECKPOINT => self.dec_book(d, ev.et, ev.out as usize, lvs)?,
            ET_PRICE_CHG => self.dec_pc(d, lvs)?,
            ET_TRADE => self.dec_trade(d, ev, lvs)?,
            _ => {}
        }
        ev.nlv = lvs.len() as u32 - ev.lv0;
        if d.broken() {
            return None;
        }
        Some(())
    }

    /// Decodes one event (model M1); its levels are appended to lvs.
    #[inline]
    pub(crate) fn dec_event(&mut self, d: &mut Dec, lvs: &mut Vec<CoreLevel>) -> Option<CoreEvent> {
        let et = d.get(C_ET + self.prev_et) as usize;
        if et > ET_CHECKPOINT as usize {
            return None;
        }
        self.prev_et = et;
        let mut ev = CoreEvent {
            et: et as u8,
            ..Default::default()
        };
        if et != ET_CHECKPOINT as usize {
            let u = d.int(C_DTS + et * 3 + dts_bucket(self.prev_dts));
            self.prev_dts = u;
            self.prev_ts = self.prev_ts.wrapping_add(unzz(u));
            ev.ts = self.prev_ts;
        }
        if et <= ET_TICK_SIZE as usize {
            let l = self.prev_lat[et].wrapping_add(unzz(d.int(C_LAT + et)));
            self.prev_lat[et] = l;
            ev.lat = l;
        }
        if et == ET_BOOK as usize {
            self.prev_cont = d.get(C_CONT + self.prev_cont) as usize & 1;
            ev.cont = self.prev_cont as u8;
        }
        if has_out(et) {
            let o = d.get(C_OUT + et * 2 + self.prev_out[et]) as usize & 1;
            self.prev_out[et] = o;
            ev.out = o as u8;
        }
        self.dec_levels(d, &mut ev, lvs)?;
        Some(ev)
    }

    /// The block part of one event whose column fields are in ev (model M2); its levels are
    /// appended to lvs.
    #[inline]
    pub(crate) fn dec_body(
        &mut self,
        d: &mut Dec,
        ev: &mut CoreEvent,
        lvs: &mut Vec<CoreLevel>,
    ) -> Option<()> {
        self.dec_levels(d, ev, lvs)
    }
}

/// The event-column state of one block (model M2), reset at the block start.
#[derive(Clone, Copy, Default)]
pub(crate) struct ColState {
    prev_et: usize,
    last_out: usize,
    prev_ts: i64,
    prev_dts: u64,
    prev_lat: [i64; 4],
}

impl ColState {
    #[inline]
    pub(crate) fn reset(&mut self, ts0: i64) {
        *self = ColState {
            prev_ts: ts0,
            ..Default::default()
        };
    }

    /// The column operations of one event.
    pub(crate) fn enc(&mut self, ops: &mut Vec<Op>, e: &CoreEvent) -> Result<(), String> {
        check_fields(e)?;
        let et = e.et as usize;
        ops.push(op_sym(
            C2_EOC + self.prev_et * 2 + self.last_out,
            (et * 4 + e.out as usize * 2 + e.cont as usize) as u32,
        ));
        self.prev_et = et;
        if has_out(et) {
            self.last_out = e.out as usize;
        }
        if et != ET_CHECKPOINT as usize {
            let u = zz(e.ts.wrapping_sub(self.prev_ts));
            put_int(ops, C_DTS + et * 3 + dts_bucket(self.prev_dts), u);
            self.prev_dts = u;
            self.prev_ts = e.ts;
        }
        if et <= ET_TICK_SIZE as usize {
            put_int(ops, C_LAT + et, zz(e.lat.wrapping_sub(self.prev_lat[et])));
            self.prev_lat[et] = e.lat;
        }
        Ok(())
    }

    /// One event's column fields (et, out, cont, ts, lat; the rest zero).
    #[inline(always)]
    pub(crate) fn dec(&mut self, d: &mut Dec) -> Option<CoreEvent> {
        let s = d.get(C2_EOC + self.prev_et * 2 + self.last_out) as usize;
        let (et, out, cont) = (s >> 2, (s >> 1) & 1, s & 1);
        if et > ET_CHECKPOINT as usize
            || (cont == 1 && et != ET_BOOK as usize)
            || (out == 1 && (et == ET_PRICE_CHG as usize || et == ET_OTHER as usize))
        {
            return None;
        }
        self.prev_et = et;
        let mut ev = CoreEvent {
            et: et as u8,
            out: out as u8,
            cont: cont as u8,
            ..Default::default()
        };
        if has_out(et) {
            self.last_out = out;
        }
        if et != ET_CHECKPOINT as usize {
            let u = d.int(C_DTS + et * 3 + dts_bucket(self.prev_dts));
            self.prev_dts = u;
            self.prev_ts = self.prev_ts.wrapping_add(unzz(u));
            ev.ts = self.prev_ts;
        }
        if et <= ET_TICK_SIZE as usize {
            let l = self.prev_lat[et].wrapping_add(unzz(d.int(C_LAT + et)));
            self.prev_lat[et] = l;
            ev.lat = l;
        }
        if d.broken() {
            return None;
        }
        Some(ev)
    }
}
