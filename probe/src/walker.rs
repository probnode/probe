//! The per-block walker (specification section 3.12): one market, local state, everything reset at
//! the block start, so that blocks decode independently.

use crate::rc::{gamma, unzigzag, Gamma, Rc, PROB_INIT};
use crate::uvarint;

pub(crate) const ET_BOOK: u8 = 0;
pub(crate) const ET_PRICE_CHG: u8 = 1;
pub(crate) const ET_TRADE: u8 = 2;
pub(crate) const ET_TICK_SIZE: u8 = 3;
pub(crate) const ET_OTHER: u8 = 4;
pub(crate) const ET_CHECKPOINT: u8 = 5;

/// The largest `U` (price units in 1.0) a block may declare. The specification's largest is 10000.
pub(crate) const MAX_U: i64 = 1 << 20;

/// One decoded level, in the units of its block's market row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CoreLevel {
    /// Price in the row's price units.
    pub p: i64,
    /// Size in the row's size units (trades: trade size units); 0 removes the level.
    pub z: i64,
    /// Outcome (0 or 1): the level's token is `tokens[out]` of the row.
    pub out: u8,
    /// 0 = bid / BUY, 1 = ask / SELL.
    pub side: u8,
    /// A price_change entry stored as the mirror of the previous explicit entry.
    pub mirror: bool,
}

/// One decoded event. Its levels are `levels[lv0 .. lv0 + nlv]` of the block it was decoded from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CoreEvent {
    /// Receive time in `unit_ns` units; 0 for a checkpoint.
    pub ts: i64,
    /// `floor(receive ms) - venue ms` for event types 0..3, else 0.
    pub lat: i64,
    /// `fee_rate_bps` of a trade, else 0.
    pub fee: i64,
    /// First level of the event in the block's level vector.
    pub lv0: u32,
    /// Number of levels of the event.
    pub nlv: u32,
    /// Event type 0..5.
    pub et: u8,
    /// Outcome of event types 0, 2, 3 and 5, else 0.
    pub out: u8,
    /// The `cont` bit of a type-0 event, else 0.
    pub cont: u8,
}

pub(crate) type WResult<T> = std::result::Result<T, String>;

fn corrupt<T>() -> WResult<T> {
    Err("corrupt block".into())
}

// Gamma context indices.
const G_DTS: usize = 0; // [et 6][bucket 3]
const G_LAT: usize = 18; // [et 4]
const G_NLV: usize = 22; // [et 0 / 5][side]
const G_BOOKDP: usize = 26; // [kind 3][side 2][first 2]
const G_NENT: usize = 38;
const G_PCPRICE: usize = 39; // [side 2][bucket 3]
const G_TRPRICE: usize = 45;
const G_TRSIZE: usize = 46;
const G_TRFEE: usize = 47;
const G_SZIDX: usize = 48; // [kind 3][path 2]
const G_SZLIT: usize = 54; // [kind 3][path 2][side 2]
const N_GAMMA: usize = 66;

/// An 8-entry size cache (3.12.3): a hit moves its entry to the front; a literal is inserted at
/// the front without searching for an equal entry, and entry 7 drops.
#[derive(Clone, Copy)]
struct Mtf {
    v: [i64; 8],
    n: usize,
}

impl Mtf {
    const EMPTY: Mtf = Mtf { v: [0; 8], n: 0 };

    #[inline]
    fn hit(&mut self, i: usize) -> i64 {
        let v = self.v[i];
        self.v.copy_within(0..i, 1);
        self.v[0] = v;
        v
    }

    #[inline]
    fn insert(&mut self, v: i64) {
        self.v.copy_within(0..7, 1);
        self.v[0] = v;
        if self.n < 8 {
            self.n += 1;
        }
    }
}

/// The outcome of decoding one event.
enum Step {
    Event(CoreEvent),
    /// The event's receive time is past the walker's limit; nothing of it was applied.
    Stop,
}

pub(crate) struct Walker {
    // bit contexts
    et: [[u32; 8]; 6],
    cont: [u32; 2],
    outcome: [[u32; 2]; 6],
    mirror: [u32; 2],
    pc_side: [[u32; 2]; 2],
    tr_side: [[u32; 2]; 2],
    sz_flag: [[u32; 2]; 3],
    sz_hit: [[[u32; 2]; 2]; 3],
    g: Vec<Gamma>,
    ar: Vec<u32>,

    u: i64,
    flags: u32,
    limit: i64,
    prev_et: usize,
    prev_ts: i64,
    prev_dts: u64,
    prev_lat: [i64; 4],
    prev_cont: usize,
    prev_out: usize,
    prev_side: [usize; 2],
    prev_tr_sd: [usize; 2],
    prev_dp: [u64; 2],
    last_px: [i64; 2],
    last_trade: [i64; 2],

    book: [[Vec<i64>; 2]; 2],
    best: [[i64; 2]; 2],
    cache: [[Mtf; 2]; 2],
    touched: Vec<u32>,
}

#[inline]
fn dts_bucket(prev: u64) -> usize {
    if prev == 0 {
        0
    } else if prev < 1024 {
        1
    } else {
        2
    }
}

#[inline]
fn dp_bucket(prev: u64) -> usize {
    if prev == 0 {
        0
    } else if prev <= 2 {
        1
    } else {
        2
    }
}

impl Walker {
    pub(crate) fn new() -> Self {
        Walker {
            et: [[PROB_INIT; 8]; 6],
            cont: [PROB_INIT; 2],
            outcome: [[PROB_INIT; 2]; 6],
            mirror: [PROB_INIT; 2],
            pc_side: [[PROB_INIT; 2]; 2],
            tr_side: [[PROB_INIT; 2]; 2],
            sz_flag: [[PROB_INIT; 2]; 3],
            sz_hit: [[[PROB_INIT; 2]; 2]; 3],
            g: vec![Gamma::new(); N_GAMMA],
            ar: Vec::new(),
            u: 1,
            flags: 0,
            limit: i64::MAX,
            prev_et: 0,
            prev_ts: 0,
            prev_dts: 0,
            prev_lat: [0; 4],
            prev_cont: 0,
            prev_out: 0,
            prev_side: [0; 2],
            prev_tr_sd: [0; 2],
            prev_dp: [0; 2],
            last_px: [0; 2],
            last_trade: [0; 2],
            book: Default::default(),
            best: [[0; 2]; 2],
            cache: [[Mtf::EMPTY; 2]; 2],
            touched: Vec::new(),
        }
    }

    fn reset(&mut self, flags: u32, u: i64, ts0: i64, limit: i64) {
        self.et = [[PROB_INIT; 8]; 6];
        self.cont = [PROB_INIT; 2];
        self.outcome = [[PROB_INIT; 2]; 6];
        self.mirror = [PROB_INIT; 2];
        self.pc_side = [[PROB_INIT; 2]; 2];
        self.tr_side = [[PROB_INIT; 2]; 2];
        self.sz_flag = [[PROB_INIT; 2]; 3];
        self.sz_hit = [[[PROB_INIT; 2]; 2]; 3];
        for g in self.g.iter_mut() {
            g.used = false;
        }
        self.ar.clear();
        self.u = u;
        self.flags = flags;
        self.limit = limit;
        self.prev_et = 0;
        self.prev_ts = ts0;
        self.prev_dts = 0;
        self.prev_lat = [0; 4];
        self.prev_cont = 0;
        self.prev_out = 0;
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
        // The books never shrink, so the slots cleared above stay in range; entries above U stay 0.
        let n = u as usize + 1;
        for (book, cache) in self.book.iter_mut().zip(self.cache.iter_mut()) {
            for (b, c) in book.iter_mut().zip(cache.iter_mut()) {
                if b.len() < n {
                    b.resize(n, 0);
                }
                *c = Mtf::EMPTY;
            }
        }
        self.best = [[-1, u + 1]; 2];
    }

    #[inline(always)]
    fn gm(&mut self, rc: &mut Rc, i: usize) -> u64 {
        gamma(rc, &mut self.g[i], &mut self.ar)
    }

    #[inline(always)]
    fn zgm(&mut self, rc: &mut Rc, i: usize) -> i64 {
        unzigzag(gamma(rc, &mut self.g[i], &mut self.ar))
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

    /// A SIZE symbol for the level (tok, side, p); `p` is in `[0, U]`.
    #[inline]
    fn size(&mut self, rc: &mut Rc, kind: usize, tok: usize, side: usize, p: i64) -> WResult<i64> {
        let cur = self.book[tok][side][p as usize];
        let mut path = 0usize;
        if cur > 0 {
            path = rc.bit(&mut self.sz_flag[kind][side]) as usize;
        }
        let v: i64;
        if rc.bit(&mut self.sz_hit[kind][path][side]) == 1 {
            let i = self.gm(rc, G_SZIDX + kind * 2 + path);
            let c = &mut self.cache[tok][path];
            if i >= c.n as u64 {
                return corrupt();
            }
            v = c.hit(i as usize);
        } else {
            if path == 1 {
                v = self.zgm(rc, G_SZLIT + kind * 4 + path * 2 + side);
            } else {
                let u = self.gm(rc, G_SZLIT + kind * 4 + path * 2 + side);
                if u > 1 << 62 {
                    return corrupt();
                }
                v = u as i64;
            }
            self.cache[tok][path].insert(v);
        }
        let z = if path == 1 {
            cur.checked_add(v).ok_or("corrupt block: size overflow")?
        } else {
            v
        };
        if z < 0 {
            return corrupt();
        }
        Ok(z)
    }

    /// One event: its levels are appended to lvs and applied to the block's book.
    fn event(&mut self, rc: &mut Rc, lvs: &mut Vec<CoreLevel>) -> WResult<Step> {
        let mut n = 1usize;
        for _ in 0..3 {
            n = (n << 1) | rc.bit(&mut self.et[self.prev_et][n]) as usize;
        }
        let et = n - 8;
        if et > ET_CHECKPOINT as usize {
            return corrupt();
        }
        self.prev_et = et;
        let mut ev = CoreEvent {
            et: et as u8,
            lv0: lvs.len() as u32,
            ..Default::default()
        };
        if et != ET_CHECKPOINT as usize {
            let u = self.gm(rc, G_DTS + et * 3 + dts_bucket(self.prev_dts));
            self.prev_dts = u;
            self.prev_ts = self.prev_ts.wrapping_add(unzigzag(u));
            ev.ts = self.prev_ts;
            if ev.ts > self.limit {
                return Ok(Step::Stop);
            }
        }
        if et <= ET_TICK_SIZE as usize {
            let mut d = self.zgm(rc, G_LAT + et);
            if self.flags & 16 != 0 {
                d = d.wrapping_add(self.prev_lat[et]);
                self.prev_lat[et] = d;
            }
            ev.lat = d;
        }
        if et == ET_BOOK as usize {
            self.prev_cont = rc.bit(&mut self.cont[self.prev_cont]) as usize;
            ev.cont = self.prev_cont as u8;
        }
        if et == ET_BOOK as usize
            || et == ET_TRADE as usize
            || et == ET_TICK_SIZE as usize
            || et == ET_CHECKPOINT as usize
        {
            self.prev_out = rc.bit(&mut self.outcome[et][self.prev_out]) as usize;
            ev.out = self.prev_out as u8;
        }
        match et as u8 {
            ET_BOOK | ET_CHECKPOINT => self.book0(rc, &ev, lvs)?,
            ET_PRICE_CHG => self.price_change(rc, lvs)?,
            ET_TRADE => {
                let tok = ev.out as usize;
                let side = rc.bit(&mut self.tr_side[tok][self.prev_tr_sd[tok]]) as usize;
                self.prev_tr_sd[tok] = side;
                let d = self.zgm(rc, G_TRPRICE);
                self.last_trade[tok] = self.last_trade[tok].wrapping_add(d);
                let z = self.gm(rc, G_TRSIZE);
                ev.fee = self.gm(rc, G_TRFEE) as i64;
                if z > 1 << 62 {
                    return corrupt();
                }
                lvs.push(CoreLevel {
                    out: tok as u8,
                    side: side as u8,
                    p: self.last_trade[tok],
                    z: z as i64,
                    mirror: false,
                });
            }
            _ => {}
        }
        ev.nlv = (lvs.len() - ev.lv0 as usize) as u32;
        if rc.overrun() {
            return corrupt();
        }
        Ok(Step::Event(ev))
    }

    /// A type-0 book diff or a type-5 checkpoint: n_bid, n_ask, then per side ascending prices
    /// (dp = p - prev_p - 1, the first one p), each followed by its SIZE.
    fn book0(&mut self, rc: &mut Rc, ev: &CoreEvent, lvs: &mut Vec<CoreLevel>) -> WResult<()> {
        let (kind, k5) = if ev.et == ET_CHECKPOINT {
            (2usize, 1usize)
        } else {
            (0, 0)
        };
        let tok = ev.out as usize;
        let nb = self.gm(rc, G_NLV + k5 * 2);
        let na = self.gm(rc, G_NLV + k5 * 2 + 1);
        let lim = (self.u + 1) as u64;
        if nb > lim || na > lim {
            return corrupt();
        }
        for (side, cnt) in [nb, na].into_iter().enumerate() {
            let mut p: i64 = -1;
            for i in 0..cnt {
                let first = (i == 0) as usize;
                let dp = self.gm(rc, G_BOOKDP + kind * 4 + side * 2 + first);
                if dp > self.u as u64 {
                    return corrupt();
                }
                p += dp as i64 + 1;
                if p > self.u {
                    return corrupt();
                }
                let z = self.size(rc, kind, tok, side, p)?;
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
        Ok(())
    }

    /// A type-1 event: n entries, each a mirror bit (after an explicit entry) or an explicit
    /// (outcome, side, price, SIZE).
    fn price_change(&mut self, rc: &mut Rc, lvs: &mut Vec<CoreLevel>) -> WResult<()> {
        let n = self.gm(rc, G_NENT);
        if n > 1 << 24 {
            return corrupt();
        }
        let mut prev_explicit: Option<usize> = None;
        for i in 0..n {
            if let Some(pe) = prev_explicit {
                let ctx = (i > 1) as usize;
                if rc.bit(&mut self.mirror[ctx]) == 1 {
                    let e = lvs[pe];
                    let m = CoreLevel {
                        out: 1 - e.out,
                        side: 1 - e.side,
                        p: self.u - e.p,
                        z: e.z,
                        mirror: true,
                    };
                    self.set(m.out as usize, m.side as usize, m.p, m.z);
                    self.last_px[m.out as usize] = m.p;
                    self.last_px[e.out as usize] = e.p;
                    lvs.push(m);
                    prev_explicit = None;
                    continue;
                }
            }
            let out = rc.bit(&mut self.outcome[ET_PRICE_CHG as usize][self.prev_out]) as usize;
            self.prev_out = out;
            let side = rc.bit(&mut self.pc_side[out][self.prev_side[out]]) as usize;
            self.prev_side[out] = side;
            let d = self.zgm(rc, G_PCPRICE + side * 3 + dp_bucket(self.prev_dp[out]));
            self.prev_dp[out] = d.unsigned_abs();
            let p = if self.flags & 64 == 0 {
                self.last_px[out].wrapping_add(d)
            } else if self.has(out, side) && side == 0 {
                self.best[out][0].wrapping_sub(d)
            } else if self.has(out, side) {
                self.best[out][1].wrapping_add(d)
            } else {
                self.last_px[out].wrapping_add(d)
            };
            if p < 0 || p > self.u {
                return corrupt();
            }
            self.last_px[out] = p;
            self.last_px[1 - out] = self.u - p;
            let z = self.size(rc, ET_PRICE_CHG as usize, out, side, p)?;
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
        Ok(())
    }

    /// Walks one block (`uv(n_ev) uv(n_lv)` + the range-coded stream), appending its events to
    /// evs and its levels to lvs (`CoreEvent::lv0` indexes lvs). With a `limit`, it stops without
    /// error before the first non-checkpoint event whose receive time is past it; the walker's
    /// book is then the state at the limit. Returns the events decoded.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn decode_block(
        &mut self,
        b: &[u8],
        shift: u32,
        flags: u32,
        u: i64,
        ts0: i64,
        limit: Option<i64>,
        evs: &mut Vec<CoreEvent>,
        lvs: &mut Vec<CoreLevel>,
    ) -> WResult<usize> {
        let (ne, k) = uvarint(b).ok_or("corrupt block header")?;
        let (nl, k2) = uvarint(&b[k..]).ok_or("corrupt block header")?;
        let stream = &b[k + k2..];
        let cap = stream.len() as u64 * 64 + 64;
        if ne > cap || nl > cap {
            return Err(format!(
                "corrupt block: header n_ev {} n_lv {} for {} bytes",
                ne,
                nl,
                stream.len()
            ));
        }
        if !(1..=MAX_U).contains(&u) {
            return Err(format!("U {} out of range", u));
        }
        self.reset(flags, u, ts0, limit.unwrap_or(i64::MAX));
        let mut rc = Rc::new(stream, shift);
        let lv_start = lvs.len();
        evs.reserve((ne as usize).min(1 << 16));
        lvs.reserve((nl as usize).min(1 << 16));
        for i in 0..ne {
            match self
                .event(&mut rc, lvs)
                .map_err(|e| format!("event {}: {}", i, e))?
            {
                Step::Event(ev) => evs.push(ev),
                Step::Stop => return Ok(i as usize),
            }
        }
        let got = lvs.len() - lv_start;
        if got as u64 != nl {
            return Err(format!(
                "corrupt block: {} levels decoded, header says {}",
                got, nl
            ));
        }
        Ok(ne as usize)
    }

    /// The levels with size > 0 of one side of the block's book, ascending price, after the last
    /// decode: (price units, size units).
    pub(crate) fn book_side(&self, tok: usize, side: usize) -> Vec<(i64, i64)> {
        let n = (self.u as usize + 1).min(self.book[tok][side].len());
        self.book[tok][side][..n]
            .iter()
            .enumerate()
            .filter(|&(_, &z)| z > 0)
            .map(|(p, &z)| (p as i64, z))
            .collect()
    }
}
