//! Probe v3 (header version `"evmin/6"`, specification section 9): a container whose every part
//! names its codec, and the transcoder that writes it.
//!
//! The container keeps v1's 8-byte prefix (magic `EVM4`, header length), so a v1 / v2 reader reads
//! the version and refuses the file. The header is the source's JSON with `version`, `family`
//! (`probe3`), `profile`, `src_version` and, per segment, `parts` as `[name, offset, length,
//! codec]`. A segment's parts: `bidx3` (block index, codec 18), `order` (codec 17), `blocks` (codec
//! 20 with its `tables` and `evcols`, codec 16 with its `tables`, or codec 2), and the side streams
//! `tx`, `payload.zst`, `hash` as stored in the source (codec 0). The markets table is the
//! source's, byte for byte.
//!
//! Reading needs nothing from this module: [`crate::File::open`] detects v3, and every reading
//! API (events, queries, books, side streams, dumps) works on v3 as on v1 and v2. Writing is
//! [`transcode`]: any v1, v2 or v3 file into a v3 file of a [`Profile`], losslessly. The core
//! dump of the result equals the source's byte for byte.
//!
//! ```no_run
//! use probe_format::{v3, File};
//!
//! # fn main() -> probe_format::Result<()> {
//! let src = File::open("hour.probe")?;
//! let out = v3::transcode(&src, v3::Profile::Fast)?;
//! std::fs::write("hour.v3.probe", &out.bytes)?;
//! let back = File::from_bytes(out.bytes)?;
//! v3::verify_transcode(&src, &back)?;
//! # Ok(())
//! # }
//! ```

mod model;
mod order;
mod rans;

use crate::decoder::{Block, SegmentCore};
use crate::error::{format_err, Error, Result};
use crate::file::{BlockCounts, BlockEntry, File, Market, Part, Segment, Version};
use crate::walker::{CoreEvent, CoreLevel, Walker, ET_BOOK, ET_CHECKPOINT, ET_OTHER, ET_PRICE_CHG};
use crate::{put_uvarint, uvarint};
use model::{ColState, State, N_CTX, N_CTX2};
use rans::{count_ops, DecTables, EncTables, Op, Tables};
use serde_json::Value;

/// The v3 header version string.
pub const VERSION: &str = "evmin/6";
/// The v3 header family string.
pub const FAMILY: &str = "probe3";

/// The codec registry (specification 9.2). An id, once registered, means the same bytes forever:
/// a better coder is a new id, never a redefinition. A reader refuses an id it does not implement.
pub mod codec {
    /// Bytes as stored (the side streams, copied from the source).
    pub const RAW: u32 = 0;
    /// `blocks`: Probe v1's adaptive binary range coder (specification 3.10 - 3.12), with the
    /// header's `shift` and `flags`.
    pub const RC1: u32 = 2;
    /// `blocks`: model M1 with static rANS, its tables in the segment's `tables` part.
    pub const M1_RANS: u32 = 16;
    /// `order`: the canonical (receive clock, block order) merge plus deferrals, as columns.
    pub const ORDER_TS: u32 = 17;
    /// Integer columns with static rANS (`bidx3`, and inside codecs 17 and 22).
    pub const COLS_RANS: u32 = 18;
    /// `tables`: the rANS frequency tables of a codec-16 or codec-20 `blocks` part.
    pub const TABLES: u32 = 19;
    /// `blocks`: model M2 (the book-dependent part of each event) with static rANS; with the
    /// segment's `tables` and `evcols`.
    pub const M2_RANS: u32 = 20;
    /// `evcols`: model M2's event columns (type, outcome, cont, clocks) in lanes of whole blocks.
    pub const EVCOLS: u32 = 21;
    /// File part `mkunits`: per market row U and the price, size and trade-size units.
    pub const MKUNITS: u32 = 22;
    /// File part `mkslugs`: the markets' slugs joined by newlines, one zstd frame.
    pub const MKSLUGS: u32 = 23;

    /// The registry name of a codec id; `None` for an id this crate does not implement.
    pub fn name(id: u32) -> Option<&'static str> {
        Some(match id {
            RAW => "raw",
            RC1 => "rc1",
            M1_RANS => "m1-rans",
            ORDER_TS => "order-ts",
            COLS_RANS => "cols-rans",
            TABLES => "rans-tables",
            M2_RANS => "m2-rans",
            EVCOLS => "m2-evcols",
            MKUNITS => "mk-units",
            MKSLUGS => "mk-slugs",
            _ => return None,
        })
    }
}

/// What a v3 file is written for. Every profile decodes to the same events; they differ in the
/// codec of the `blocks` part.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Profile {
    /// Model M2: event columns plus static rANS blocks (codecs 20 and 21). The default: the
    /// smallest and the fastest to decode.
    #[default]
    Fast,
    /// Model M1: static rANS blocks (codec 16), the event fields inside each block.
    Fast1,
    /// v1's range-coded blocks kept byte for byte (codec 2), with v3's order and index. Needs a
    /// v1 or v2 source.
    Rc1,
}

impl Profile {
    /// The profile's name: `fast`, `fast1` or `rc1`.
    pub fn name(self) -> &'static str {
        match self {
            Profile::Fast => "fast",
            Profile::Fast1 => "fast1",
            Profile::Rc1 => "rc1",
        }
    }

    /// Parses a profile name.
    pub fn parse(s: &str) -> Option<Profile> {
        match s {
            "fast" => Some(Profile::Fast),
            "fast1" => Some(Profile::Fast1),
            "rc1" => Some(Profile::Rc1),
            _ => None,
        }
    }

    /// Every profile.
    pub const ALL: [Profile; 3] = [Profile::Fast, Profile::Fast1, Profile::Rc1];
}

impl std::fmt::Display for Profile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// Counts of one v3 block, from `bidx3`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BlockInfo {
    n_ev: usize,
    n_ck: usize,
    n_lv: usize,
}

/// A lane of a segment's event columns: its blocks [b0, b1) and its stream in `evcols`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Lane {
    b0: usize,
    b1: usize,
    off: usize,
    len: usize,
}

/// What a v3 segment needs beyond the block index.
#[derive(Clone, Debug)]
pub(crate) struct Seg3 {
    pub(crate) blocks_codec: u32,
    tables: Option<Tables>,
    lanes: Vec<Lane>,
    info: Vec<BlockInfo>,
}

/// The header facts of a segment.
pub(crate) struct SegHead {
    pub(crate) t0_ns: i64,
    pub(crate) nmkt: usize,
    pub(crate) n_ev: usize,
    pub(crate) n_lv: usize,
    pub(crate) n_ck: usize,
    pub(crate) unsynced: Vec<(i64, i64)>,
}

const BIDX_COLS: usize = 9;

/// Events per lane of the event columns the writer aims at (a lane is a run of whole blocks).
const LANE_EV: usize = 16384;

fn unsupported<T>(m: String) -> Result<T> {
    Err(Error::Unsupported(m))
}

fn find_part<'a>(parts: &'a [Part], name: &str) -> Option<&'a Part> {
    parts.iter().find(|p| p.name == name)
}

/// The part `name`, which must exist and have one of `codecs`.
fn need_part<'a>(parts: &'a [Part], si: usize, name: &str, codecs: &[u32]) -> Result<&'a Part> {
    let p = find_part(parts, name)
        .ok_or_else(|| Error::Format(format!("segment {}: no part {:?}", si, name)))?;
    let c = p.codec.unwrap_or(u32::MAX);
    if !codecs.contains(&c) {
        return unsupported(format!(
            "segment {}: part {:?} has codec {}, this reader implements {:?}",
            si, name, c, codecs
        ));
    }
    Ok(p)
}

/// A part's bytes; its range was checked against the body at open.
fn part_of<'a>(body: &'a [u8], p: &Part) -> &'a [u8] {
    &body[p.offset as usize..(p.offset + p.length) as usize]
}

/// Parses one v3 segment of the header: checks every codec, reads `bidx3`, `tables` and the lane
/// directory of `evcols`.
pub(crate) fn parse_segment(
    body: &[u8],
    si: usize,
    h: SegHead,
    parts: Vec<Part>,
    bitmap: Vec<u8>,
    v2_sides: bool,
) -> Result<Segment> {
    let fe = |m: String| Error::Format(format!("segment {}: {}", si, m));
    for side in ["tx", "payload.zst", "hash"] {
        if find_part(&parts, side).is_some() {
            need_part(&parts, si, side, &[codec::RAW])?;
        }
    }
    let bl = need_part(
        &parts,
        si,
        "blocks",
        &[codec::M2_RANS, codec::M1_RANS, codec::RC1],
    )?
    .clone();
    let bx = need_part(&parts, si, "bidx3", &[codec::COLS_RANS])?.clone();
    need_part(&parts, si, "order", &[codec::ORDER_TS])?;
    let blocks_codec = bl.codec.unwrap_or(0);
    let mut seg_bytes = bl.length as usize;
    let tables = if blocks_codec == codec::RC1 {
        None
    } else {
        let tp = need_part(&parts, si, "tables", &[codec::TABLES])?;
        let tb = part_of(body, tp);
        let (t, k) = Tables::read(tb).map_err(fe)?;
        if k != tb.len() {
            return Err(fe("tables: trailing bytes".into()));
        }
        let want = if blocks_codec == codec::M1_RANS {
            N_CTX
        } else {
            N_CTX2
        };
        if t.freq.len() != want {
            return Err(fe(format!(
                "{} table contexts, the model has {}",
                t.freq.len(),
                want
            )));
        }
        Some(t)
    };
    let evcols = if blocks_codec == codec::M2_RANS {
        let p = need_part(&parts, si, "evcols", &[codec::EVCOLS])?;
        seg_bytes += p.length as usize;
        Some(part_of(body, p))
    } else {
        None
    };
    let cols = rans::decode_columns(part_of(body, &bx), BIDX_COLS)
        .map_err(|e| fe(format!("bidx3: {}", e)))?;
    // what a block may claim, against the bytes that code it (a guard for corrupt indexes)
    let ev_cap = (seg_bytes as u64 + 64) * 64;
    let lv_cap = ev_cap * 16;
    let nb = cols[0].len();
    let mut blocks = Vec::with_capacity(nb);
    let mut info = Vec::with_capacity(nb);
    let mut row: u64 = 0;
    let mut first = true;
    let mut off = 0usize;
    let mut start = BlockCounts::default();
    let (mut sum_ev, mut sum_ck, mut sum_lv) = (0u64, 0u64, 0u64);
    for i in 0..nb {
        let d = cols[0][i];
        row = if first {
            d
        } else {
            row.saturating_add(d).saturating_add(1)
        };
        first = false;
        let len = cols[1][i];
        if row > h.nmkt as u64 || len > (bl.length as usize - off) as u64 {
            return Err(fe(format!("bidx3 entry {} out of range", i)));
        }
        let (n_ev, n_ck, n_lv) = (cols[2][i], cols[3][i], cols[4][i]);
        sum_ev += n_ev.min(ev_cap + 1);
        sum_ck += n_ck.min(ev_cap + 1);
        sum_lv += n_lv.min(lv_cap + 1);
        if n_ck > n_ev || sum_ev > ev_cap || sum_lv > lv_cap {
            return Err(fe(format!("bidx3 counts of entry {} out of range", i)));
        }
        let side = [cols[5][i], cols[6][i], cols[7][i], cols[8][i]];
        if side.iter().any(|&c| c > 1 << 40) {
            return Err(fe(format!("bidx3 side count of entry {} out of range", i)));
        }
        let counts = if v2_sides {
            let c = BlockCounts {
                n_ev: n_ev as usize,
                n_lv: n_lv as usize,
                n_trade: side[0] as usize,
                n_pay: side[1] as usize,
                n_hash: side[2] as usize,
                n_bex: side[3] as usize,
                ..start
            };
            start.tx_start += c.n_trade;
            start.pay_start += c.n_pay;
            start.hash_start += c.n_hash;
            start.bex_start += c.n_bex;
            Some(c)
        } else {
            None
        };
        blocks.push(BlockEntry {
            row: row as usize,
            offset: off,
            length: len as usize,
            counts,
        });
        info.push(BlockInfo {
            n_ev: n_ev as usize,
            n_ck: n_ck as usize,
            n_lv: n_lv as usize,
        });
        off += len as usize;
    }
    if off != bl.length as usize {
        return Err(fe(format!(
            "bidx3 covers {} of {} block bytes",
            off, bl.length
        )));
    }
    if sum_ck != h.n_ck as u64 || sum_ev != (h.n_ev as u64).saturating_add(h.n_ck as u64) {
        return Err(fe(format!(
            "bidx3 counts {} checkpoints / {} events against the header's n_ck {} n_ev {}",
            sum_ck, sum_ev, h.n_ck, h.n_ev
        )));
    }
    let lanes = match evcols {
        Some(b) => parse_lanes(b, nb).map_err(fe)?,
        None => Vec::new(),
    };
    Ok(Segment {
        t0_ns: h.t0_ns,
        nmkt: h.nmkt,
        n_ev: h.n_ev,
        n_lv: h.n_lv,
        n_ck: h.n_ck,
        unsynced: h.unsynced,
        parts,
        blocks,
        bitmap,
        v3: Some(Seg3 {
            blocks_codec,
            tables,
            lanes,
            info,
        }),
    })
}

/// `evcols` layout: uv(n_lanes), per lane uv(first block - previous first block), per lane
/// uv(stream bytes), then the lane streams in order. Lane j covers blocks [first_j, first_j+1).
fn parse_lanes(b: &[u8], nb: usize) -> std::result::Result<Vec<Lane>, String> {
    let mut p = 0usize;
    let mut next = || -> std::result::Result<u64, String> {
        let (v, k) = uvarint(&b[p..]).ok_or("evcols: bad varint")?;
        p += k;
        Ok(v)
    };
    let n = next()?;
    if n > nb.max(1) as u64 {
        return Err("evcols: too many lanes".into());
    }
    let n = n as usize;
    let mut firsts = Vec::with_capacity(n);
    let mut f = 0u64;
    for j in 0..n {
        let d = next()?;
        if (j == 0 && d != 0) || (j > 0 && d == 0) {
            return Err("evcols: bad lane start".into());
        }
        f = f.saturating_add(d);
        if f >= nb as u64 {
            return Err("evcols: lane start past the blocks".into());
        }
        firsts.push(f as usize);
    }
    let mut lens = Vec::with_capacity(n);
    for _ in 0..n {
        lens.push(next()?);
    }
    let mut lanes = Vec::with_capacity(n);
    let mut off = p;
    for j in 0..n {
        let b1 = if j + 1 < n { firsts[j + 1] } else { nb };
        if lens[j] > (b.len() - off) as u64 {
            return Err("evcols: lane out of range".into());
        }
        let len = lens[j] as usize;
        lanes.push(Lane {
            b0: firsts[j],
            b1,
            off,
            len,
        });
        off += len;
    }
    if off != b.len() || (n == 0 && nb > 0) {
        return Err("evcols: lanes do not cover the part".into());
    }
    Ok(lanes)
}

/// The file parts `mkunits` (codec 22) and `mkslugs` (codec 23) must agree with the markets
/// table they summarise.
pub(crate) fn check_market_parts(
    body: &[u8],
    units: Option<(usize, usize)>,
    slugs: Option<(usize, usize)>,
    markets: &[Market],
) -> Result<()> {
    if let Some((o, n)) = units {
        let cols = rans::decode_columns(&body[o..o + n], 4)
            .map_err(|e| Error::Format(format!("mkunits: {}", e)))?;
        let same = cols[0].len() == markets.len()
            && markets.iter().enumerate().all(|(i, m)| {
                [
                    m.u,
                    m.price_unit_micro,
                    m.size_unit_micro,
                    m.trade_size_unit_micro,
                ]
                .iter()
                .zip(&cols)
                .all(|(&v, c)| v >= 0 && c[i] == v as u64)
            });
        if !same {
            return format_err("mkunits disagrees with the markets table");
        }
    }
    if let Some((o, n)) = slugs {
        let raw = zstd::stream::decode_all(&body[o..o + n])
            .map_err(|e| Error::Format(format!("mkslugs: {}", e)))?;
        let same = if markets.is_empty() {
            raw.is_empty()
        } else {
            let mut it = raw.split(|&c| c == b'\n');
            markets.iter().all(|m| it.next() == Some(m.slug.as_bytes())) && it.next().is_none()
        };
        if !same {
            return format_err("mkslugs disagrees with the markets table");
        }
    }
    Ok(())
}

/// Runs f over 0..n with a per-worker state, in parallel when asked and the `parallel` feature
/// is on; the results keep index order.
fn par_map<S, T, I, F>(n: usize, parallel: bool, init: I, f: F) -> Vec<T>
where
    T: Send,
    I: Fn() -> S + Sync + Send,
    F: Fn(&mut S, usize) -> T + Sync + Send,
{
    #[cfg(feature = "parallel")]
    if parallel {
        use rayon::prelude::*;
        return (0..n)
            .into_par_iter()
            .with_min_len(8)
            .map_init(&init, |s, i| f(s, i))
            .collect();
    }
    let _ = parallel;
    let mut s = init();
    (0..n).map(|i| f(&mut s, i)).collect()
}

fn collect<T>(v: Vec<Result<T>>) -> Result<Vec<T>> {
    v.into_iter().collect()
}

// ------------------------------------------------------------------------------------------------
// Reading

impl File {
    fn seg3(&self, si: usize) -> Result<&Seg3> {
        self.seg(si)?
            .v3
            .as_ref()
            .ok_or_else(|| Error::Format(format!("segment {}: not a v3 segment", si)))
    }

    /// Block bi's checks against its `bidx3` entry: counts, leading checkpoints, type-4 events
    /// only in the market-less block.
    fn check_block3(&self, si: usize, bi: usize, b: &Block) -> Result<()> {
        let s = self.seg(si)?;
        let inf = self.seg3(si)?.info[bi];
        let fail = |m: &str| format_err(format!("segment {} block row {}: {}", si, b.row, m));
        if b.events.len() != inf.n_ev || b.levels.len() != inf.n_lv {
            return fail("event or level count differs from the block index");
        }
        let mut nck = 0;
        while nck < b.events.len() && b.events[nck].et == ET_CHECKPOINT {
            nck += 1;
        }
        if nck != inf.n_ck || nck != b.n_checkpoints {
            return fail("checkpoints not leading or their count wrong");
        }
        for e in &b.events[nck..] {
            if e.et == ET_CHECKPOINT || (b.row == s.nmkt) != (e.et == ET_OTHER) {
                return fail("event type out of place");
            }
        }
        Ok(())
    }

    /// A model-M1 block.
    fn m1_block(&self, si: usize, bi: usize, dt: &DecTables, st: &mut State) -> Result<Block> {
        let s = self.seg(si)?;
        let inf = self.seg3(si)?.info[bi];
        let row = s.blocks[bi].row;
        let u = self.block_u(si, row)?;
        let bytes = self.v3_block_bytes(si, bi)?;
        st.reset(u, s.t0_ns / self.header.unit_ns, inf.n_lv);
        let mut d = rans::Dec::new(bytes, dt);
        let mut b = Block {
            row,
            events: Vec::with_capacity(inf.n_ev.min(1 << 16)),
            levels: Vec::with_capacity(inf.n_lv.min(1 << 16)),
            n_checkpoints: inf.n_ck,
        };
        for k in 0..inf.n_ev {
            let ev = st.dec_event(&mut d, &mut b.levels).ok_or_else(|| {
                Error::Format(format!(
                    "segment {} block row {}: corrupt event {}",
                    si, row, k
                ))
            })?;
            b.events.push(ev);
        }
        if !d.finish() {
            return format_err(format!(
                "segment {} block row {}: stream not consumed exactly",
                si, row
            ));
        }
        self.check_block3(si, bi, &b)?;
        Ok(b)
    }

    /// The event-column fields of the blocks [lane.b0, upto) of a lane (model M2).
    fn lane_columns(
        &self,
        si: usize,
        lane: &Lane,
        upto: usize,
        dt: &DecTables,
    ) -> Result<Vec<Vec<CoreEvent>>> {
        let s = self.seg(si)?;
        let s3 = self.seg3(si)?;
        let all = self
            .part_bytes(si, "evcols")
            .ok_or_else(|| Error::Format(format!("segment {}: no evcols", si)))?;
        let b = &all[lane.off..lane.off + lane.len];
        let ts0 = s.t0_ns / self.header.unit_ns;
        let mut d = rans::Dec::new(b, dt);
        let mut cs = ColState::default();
        let end = upto.min(lane.b1);
        let mut out = Vec::with_capacity(end - lane.b0);
        for bi in lane.b0..end {
            let n = s3.info[bi].n_ev;
            cs.reset(ts0);
            let mut evs = Vec::with_capacity(n.min(1 << 16));
            for _ in 0..n {
                evs.push(cs.dec(&mut d).ok_or_else(|| {
                    Error::Format(format!(
                        "segment {} block row {}: corrupt event columns",
                        si, s.blocks[bi].row
                    ))
                })?);
            }
            out.push(evs);
        }
        if end == lane.b1 && !d.finish() {
            return format_err(format!(
                "segment {}: event column lane not consumed exactly",
                si
            ));
        }
        Ok(out)
    }

    /// A model-M2 block from its event columns.
    fn m2_block(
        &self,
        si: usize,
        bi: usize,
        mut events: Vec<CoreEvent>,
        dt: &DecTables,
        st: &mut State,
    ) -> Result<Block> {
        let s = self.seg(si)?;
        let inf = self.seg3(si)?.info[bi];
        let row = s.blocks[bi].row;
        let u = self.block_u(si, row)?;
        let bytes = self.v3_block_bytes(si, bi)?;
        st.reset_m2(u, inf.n_lv);
        let mut d = rans::Dec::new(bytes, dt);
        let mut levels = Vec::with_capacity(inf.n_lv.min(1 << 16));
        for ev in events.iter_mut() {
            st.dec_body(&mut d, ev, &mut levels).ok_or_else(|| {
                Error::Format(format!("segment {} block row {}: corrupt block", si, row))
            })?;
        }
        if !d.finish() {
            return format_err(format!(
                "segment {} block row {}: stream not consumed exactly",
                si, row
            ));
        }
        let b = Block {
            row,
            events,
            levels,
            n_checkpoints: inf.n_ck,
        };
        self.check_block3(si, bi, &b)?;
        Ok(b)
    }

    fn v3_block_bytes(&self, si: usize, bi: usize) -> Result<&[u8]> {
        let r = self.seg(si)?.blocks[bi];
        let bp = self
            .part_bytes(si, "blocks")
            .ok_or_else(|| Error::Format(format!("segment {}: no blocks part", si)))?;
        bp.get(r.offset..r.offset + r.length)
            .ok_or_else(|| Error::Format(format!("segment {}: block {} outside its part", si, bi)))
    }

    /// One v3 block decoded alone (a model-M2 block decodes its lane's columns up to it).
    pub(crate) fn v3_block(&self, si: usize, bi: usize) -> Result<Block> {
        let s3 = self.seg3(si)?;
        if bi >= s3.info.len() {
            return Err(Error::NotFound(format!("segment {}: no block {}", si, bi)));
        }
        match s3.blocks_codec {
            codec::RC1 => {
                let b = self.walk(&mut Walker::new(), si, bi, None)?;
                self.check_block3(si, bi, &b)?;
                Ok(b)
            }
            codec::M1_RANS => {
                let dt = DecTables::new(s3.tables.as_ref().expect("checked at open"));
                self.m1_block(si, bi, &dt, &mut State::new())
            }
            _ => {
                let dt = DecTables::new(s3.tables.as_ref().expect("checked at open"));
                let j = s3.lanes.partition_point(|l| l.b1 <= bi);
                let lane = *s3.lanes.get(j).ok_or_else(|| {
                    Error::Format(format!("segment {}: block outside the lanes", si))
                })?;
                let mut cols = self.lane_columns(si, &lane, bi + 1, &dt)?;
                let evs = cols.pop().unwrap_or_default();
                self.m2_block(si, bi, evs, &dt, &mut State::new())
            }
        }
    }

    /// Every block of a v3 segment.
    pub(crate) fn v3_blocks(&self, si: usize, parallel: bool) -> Result<Vec<Block>> {
        let s3 = self.seg3(si)?;
        let n = s3.info.len();
        match s3.blocks_codec {
            codec::RC1 => collect(par_map(n, parallel, Walker::new, |w, bi| {
                let b = self.walk(w, si, bi, None)?;
                self.check_block3(si, bi, &b)?;
                Ok(b)
            })),
            codec::M1_RANS => {
                let dt = DecTables::new(s3.tables.as_ref().expect("checked at open"));
                collect(par_map(n, parallel, State::new, |st, bi| {
                    self.m1_block(si, bi, &dt, st)
                }))
            }
            _ => {
                let dt = DecTables::new(s3.tables.as_ref().expect("checked at open"));
                let lanes = collect(par_map(
                    s3.lanes.len(),
                    parallel,
                    || (),
                    |_, j| {
                        let l = s3.lanes[j];
                        self.lane_columns(si, &l, l.b1, &dt)
                    },
                ))?;
                let cols: Vec<Vec<CoreEvent>> = lanes.into_iter().flatten().collect();
                if cols.len() != n {
                    return format_err(format!(
                        "segment {}: the lanes do not cover every block",
                        si
                    ));
                }
                collect(par_map(n, parallel, State::new, |st, bi| {
                    self.m2_block(si, bi, cols[bi].clone(), &dt, st)
                }))
            }
        }
    }

    /// The block id of every real event of a v3 segment in arrival order (the order stream of
    /// v1), from its decoded blocks and the `order` part.
    pub(crate) fn v3_ids(&self, si: usize, blocks: &[Block]) -> Result<Vec<u32>> {
        let s = self.seg(si)?;
        let fe = |m: String| Error::Format(format!("segment {}: {}", si, m));
        let mut ev0 = Vec::with_capacity(blocks.len() + 1);
        let mut total = 0usize;
        for b in blocks {
            ev0.push(total);
            total += b.events.len();
        }
        if total > u32::MAX as usize {
            return Err(fe("too many events".into()));
        }
        let n_real = total - blocks.iter().map(|b| b.n_checkpoints).sum::<usize>();
        let mut gs = Vec::with_capacity(n_real);
        let mut ts = Vec::with_capacity(n_real);
        let mut blk_of = vec![0u32; total];
        for (bi, b) in blocks.iter().enumerate() {
            for (j, e) in b.events.iter().enumerate() {
                blk_of[ev0[bi] + j] = bi as u32;
                if j >= b.n_checkpoints {
                    gs.push((ev0[bi] + j) as u32);
                    ts.push(e.ts);
                }
            }
        }
        let canon = order::canonical(ts, gs);
        let op = self
            .part_bytes(si, "order")
            .ok_or_else(|| fe("no order part".into()))?;
        let cols = rans::decode_columns(op, 2).map_err(|e| fe(format!("order: {}", e)))?;
        let arr = order::arrival(&canon, &cols[0], &cols[1]).map_err(fe)?;
        let mut next: Vec<usize> = blocks.iter().map(|b| b.n_checkpoints).collect();
        let mut ids = Vec::with_capacity(arr.len());
        for g in arr {
            let bi = blk_of[g as usize] as usize;
            if g as usize != ev0[bi] + next[bi] {
                return Err(fe("order: a block's events out of block order".into()));
            }
            next[bi] += 1;
            ids.push(blocks[bi].row as u32);
        }
        if ids.len() != s.n_ev {
            return Err(fe(format!(
                "order: {} events, the header says {}",
                ids.len(),
                s.n_ev
            )));
        }
        Ok(ids)
    }

    /// A v3 block up to receive time `limit`, and the book of both outcomes there (see
    /// `decode_block_until`).
    #[allow(clippy::type_complexity)]
    pub(crate) fn v3_block_until(
        &self,
        si: usize,
        bi: usize,
        limit: i64,
    ) -> Result<(Block, [[Vec<(i64, i64)>; 2]; 2])> {
        let mut b = self.v3_block(si, bi)?;
        let keep = b
            .events
            .iter()
            .position(|e| e.et != ET_CHECKPOINT && e.ts > limit)
            .unwrap_or(b.events.len());
        let nl = b
            .events
            .get(keep)
            .map_or(b.levels.len(), |e| e.lv0 as usize);
        b.events.truncate(keep);
        b.levels.truncate(nl);
        let mut book: [[std::collections::BTreeMap<i64, i64>; 2]; 2] = Default::default();
        for e in &b.events {
            if e.et == ET_BOOK || e.et == ET_PRICE_CHG || e.et == ET_CHECKPOINT {
                for l in b.event_levels_of(e) {
                    book[(l.out & 1) as usize][(l.side & 1) as usize].insert(l.p, l.z);
                }
            }
        }
        let side = |m: &std::collections::BTreeMap<i64, i64>| -> Vec<(i64, i64)> {
            m.iter()
                .filter(|&(_, &z)| z > 0)
                .map(|(&p, &z)| (p, z))
                .collect()
        };
        let out = [
            [side(&book[0][0]), side(&book[0][1])],
            [side(&book[1][0]), side(&book[1][1])],
        ];
        Ok((b, out))
    }
}

impl Block {
    fn event_levels_of(&self, e: &CoreEvent) -> &[CoreLevel] {
        let a = (e.lv0 as usize).min(self.levels.len());
        let z = (a + e.nlv as usize).min(self.levels.len());
        &self.levels[a..z]
    }
}

// ------------------------------------------------------------------------------------------------
// Writing

/// A transcoded v3 file.
#[derive(Clone, Debug)]
pub struct Transcoded {
    /// The file's bytes.
    pub bytes: Vec<u8>,
    /// Bytes per part name, summed over segments, in order of first appearance; the header and
    /// the file parts (`mkunits`, `mkslugs`, `markets_zst`) included.
    pub part_bytes: Vec<(String, u64)>,
}

fn add_stat(st: &mut Vec<(String, u64)>, name: &str, n: usize) {
    match st.iter_mut().find(|p| p.0 == name) {
        Some(p) => p.1 += n as u64,
        None => st.push((name.to_string(), n as u64)),
    }
}

/// The U of a block row (1 for the market-less block), checked as the decoder checks it.
fn u_of(f: &File, nmkt: usize, row: usize) -> std::result::Result<i64, String> {
    if row == nmkt {
        return Ok(1);
    }
    let u = f
        .markets
        .get(row)
        .ok_or(format!("row {} outside the markets table", row))?
        .u;
    if !(1..=crate::walker::MAX_U).contains(&u) {
        return Err(format!("row {}: U {}", row, u));
    }
    Ok(u)
}

fn wfail<T>(si: usize, row: usize, e: String) -> Result<T> {
    Err(Error::Format(format!(
        "transcode: segment {} block row {}: {}",
        si, row, e
    )))
}

/// Model M1: the `tables` part and one stream per block.
fn encode_m1(f: &File, si: usize, core: &SegmentCore) -> Result<(Vec<u8>, Vec<Vec<u8>>)> {
    let s = f.seg(si)?;
    let ts0 = s.t0_ns / f.header.unit_ns;
    let nb = core.blocks.len();
    let ops = collect(par_map(nb, true, State::new, |st, bi| {
        let b = &core.blocks[bi];
        let u = u_of(f, s.nmkt, b.row).or_else(|e| wfail(si, b.row, e))?;
        st.reset(u, ts0, usize::MAX);
        let mut ops: Vec<Op> = Vec::new();
        for e in &b.events {
            st.enc_event(&mut ops, e, b.event_levels_of(e))
                .or_else(|e| wfail(si, b.row, e))?;
        }
        Ok(ops)
    }))?;
    let mut counts = vec![[0u64; 256]; N_CTX];
    for o in &ops {
        count_ops(&mut counts, o);
    }
    let t = Tables::from_counts(&counts);
    let et = EncTables::new(&t);
    let mut tb = Vec::new();
    t.write(&mut tb);
    let streams = collect(par_map(
        nb,
        true,
        || (),
        |_, bi| {
            let mut out = Vec::with_capacity(ops[bi].len());
            et.encode(&ops[bi], &mut out)
                .or_else(|e| wfail(si, core.blocks[bi].row, e))?;
            Ok(out)
        },
    ))?;
    Ok((tb, streams))
}

/// The lanes of a segment's event columns: runs of whole blocks of about LANE_EV events.
fn split_lanes(n_ev: &[usize]) -> Vec<usize> {
    let mut firsts = Vec::new();
    let mut acc = 0usize;
    for (bi, &n) in n_ev.iter().enumerate() {
        if bi == 0 || acc >= LANE_EV {
            firsts.push(bi);
            acc = 0;
        }
        acc += n;
    }
    firsts
}

/// Model M2: the `tables`, `evcols` and `blocks` parts and the block lengths.
/// The parts model M2 writes for a segment: `tables`, `evcols`, and one stream per block.
type M2Parts = (Vec<u8>, Vec<u8>, Vec<Vec<u8>>);

fn encode_m2(f: &File, si: usize, core: &SegmentCore) -> Result<M2Parts> {
    let s = f.seg(si)?;
    let ts0 = s.t0_ns / f.header.unit_ns;
    let nb = core.blocks.len();
    let ops = collect(par_map(
        nb,
        true,
        || (State::new(), ColState::default()),
        |(st, cs), bi| {
            let b = &core.blocks[bi];
            let u = u_of(f, s.nmkt, b.row).or_else(|e| wfail(si, b.row, e))?;
            cs.reset(ts0);
            st.reset_m2(u, usize::MAX);
            let (mut cops, mut bops): (Vec<Op>, Vec<Op>) = (Vec::new(), Vec::new());
            for e in &b.events {
                cs.enc(&mut cops, e).or_else(|e| wfail(si, b.row, e))?;
                st.enc_body(&mut bops, e, b.event_levels_of(e))
                    .or_else(|e| wfail(si, b.row, e))?;
            }
            Ok((cops, bops))
        },
    ))?;
    let mut counts = vec![[0u64; 256]; N_CTX2];
    for (c, b) in &ops {
        count_ops(&mut counts, c);
        count_ops(&mut counts, b);
    }
    let t = Tables::from_counts(&counts);
    let et = EncTables::new(&t);
    let mut tb = Vec::new();
    t.write(&mut tb);
    let streams = collect(par_map(
        nb,
        true,
        || (),
        |_, bi| {
            let mut out = Vec::with_capacity(ops[bi].1.len());
            et.encode(&ops[bi].1, &mut out)
                .or_else(|e| wfail(si, core.blocks[bi].row, e))?;
            Ok(out)
        },
    ))?;
    let n_ev: Vec<usize> = core.blocks.iter().map(|b| b.events.len()).collect();
    let firsts = split_lanes(&n_ev);
    let lanes = collect(par_map(
        firsts.len(),
        true,
        || (),
        |_, j| {
            let b1 = if j + 1 < firsts.len() {
                firsts[j + 1]
            } else {
                nb
            };
            let mut lane_ops = Vec::new();
            for o in &ops[firsts[j]..b1] {
                lane_ops.extend_from_slice(&o.0);
            }
            let mut out = Vec::new();
            et.encode(&lane_ops, &mut out).map_err(|e| {
                Error::Format(format!("transcode: segment {}: event columns: {}", si, e))
            })?;
            Ok(out)
        },
    ))?;
    let mut evcols = Vec::new();
    put_uvarint(&mut evcols, firsts.len() as u64);
    let mut prev = 0usize;
    for &x in &firsts {
        put_uvarint(&mut evcols, (x - prev) as u64);
        prev = x;
    }
    for l in &lanes {
        put_uvarint(&mut evcols, l.len() as u64);
    }
    for l in lanes {
        evcols.extend_from_slice(&l);
    }
    Ok((tb, evcols, streams))
}

/// The parts of one v3 segment: (name, bytes, codec), in file order.
fn encode_segment(f: &File, si: usize, profile: Profile) -> Result<Vec<(String, Vec<u8>, u32)>> {
    let s = f.seg(si)?;
    #[cfg(feature = "parallel")]
    let core = f.segment_core_par(si)?;
    #[cfg(not(feature = "parallel"))]
    let core = f.segment_core(si)?;
    let nb = core.blocks.len();
    let mut parts: Vec<(String, Vec<u8>, u32)> = Vec::new();
    let lens: Vec<usize> = match profile {
        Profile::Fast => {
            let (tb, ec, bl) = encode_m2(f, si, &core)?;
            let lens = bl.iter().map(|b| b.len()).collect();
            parts.push(("tables".into(), tb, codec::TABLES));
            parts.push(("evcols".into(), ec, codec::EVCOLS));
            parts.push(("blocks".into(), bl.concat(), codec::M2_RANS));
            lens
        }
        Profile::Fast1 => {
            let (tb, bl) = encode_m1(f, si, &core)?;
            let lens = bl.iter().map(|b| b.len()).collect();
            parts.push(("tables".into(), tb, codec::TABLES));
            parts.push(("blocks".into(), bl.concat(), codec::M1_RANS));
            lens
        }
        Profile::Rc1 => {
            if f.header.version == Version::V3 {
                return Err(Error::Unsupported(
                    "profile rc1 keeps v1 blocks: the source must be v1 or v2".into(),
                ));
            }
            let bp = f.part_bytes(si, "blocks").unwrap_or(&[]).to_vec();
            parts.push(("blocks".into(), bp, codec::RC1));
            s.blocks.iter().map(|b| b.length).collect()
        }
    };
    if s.blocks.len() != nb || lens.len() != nb {
        return format_err(format!("transcode: segment {}: block counts differ", si));
    }
    // the block index
    let mut cols: Vec<Vec<u64>> = (0..BIDX_COLS).map(|_| Vec::with_capacity(nb)).collect();
    let mut prev: i64 = -1;
    for (bi, b) in core.blocks.iter().enumerate() {
        let row = b.row as i64;
        if row <= prev {
            return format_err(format!(
                "transcode: segment {}: block rows not ascending",
                si
            ));
        }
        cols[0].push((row - prev - 1) as u64);
        prev = row;
        cols[1].push(lens[bi] as u64);
        cols[2].push(b.events.len() as u64);
        cols[3].push(b.n_checkpoints as u64);
        cols[4].push(b.levels.len() as u64);
        let side = s.blocks[bi]
            .counts
            .map_or([0; 4], |c| [c.n_trade, c.n_pay, c.n_hash, c.n_bex]);
        for k in 0..4 {
            cols[5 + k].push(side[k] as u64);
        }
    }
    parts.push((
        "bidx3".into(),
        rans::encode_columns(&cols).map_err(Error::Format)?,
        codec::COLS_RANS,
    ));
    // the order: canonical from the clocks, deferrals from the arrival order
    let mut ev0 = Vec::with_capacity(nb);
    let mut total = 0u32;
    for b in &core.blocks {
        ev0.push(total);
        total += b.events.len() as u32;
    }
    let mut gs = Vec::with_capacity(s.n_ev);
    let mut ts = Vec::with_capacity(s.n_ev);
    for (bi, b) in core.blocks.iter().enumerate() {
        for (j, e) in b.events.iter().enumerate().skip(b.n_checkpoints) {
            gs.push(ev0[bi] + j as u32);
            ts.push(e.ts);
        }
    }
    let canon = order::canonical(ts, gs);
    let n_ck: usize = core.blocks.iter().map(|b| b.n_checkpoints).sum();
    let arr: Vec<u32> = core.order[n_ck..]
        .iter()
        .map(|&(bi, j)| ev0[bi as usize] + j)
        .collect();
    let (runs, ds) = order::deferrals(&canon, &arr, total as usize)
        .map_err(|e| Error::Format(format!("transcode: segment {}: {}", si, e)))?;
    parts.push((
        "order".into(),
        rans::encode_columns(&[runs, ds]).map_err(Error::Format)?,
        codec::ORDER_TS,
    ));
    // the side streams, as stored
    for p in &s.parts {
        if ["blocks", "order", "bidx", "bidx3", "tables", "evcols"].contains(&p.name.as_str()) {
            continue;
        }
        let b = f.part_bytes(si, &p.name).unwrap_or(&[]).to_vec();
        parts.push((p.name.clone(), b, codec::RAW));
    }
    Ok(parts)
}

/// `mkunits` (codec 22) and `mkslugs` (codec 23) of a markets table; `None` when a unit is
/// negative or a slug holds a newline (readers then use the markets table alone).
fn market_parts(ms: &[Market]) -> Option<(Vec<u8>, Vec<u8>)> {
    let mut cols: Vec<Vec<u64>> = (0..4).map(|_| Vec::with_capacity(ms.len())).collect();
    let mut raw = Vec::new();
    for (i, m) in ms.iter().enumerate() {
        let u = [
            m.u,
            m.price_unit_micro,
            m.size_unit_micro,
            m.trade_size_unit_micro,
        ];
        for k in 0..4 {
            if u[k] < 0 {
                return None;
            }
            cols[k].push(u[k] as u64);
        }
        if m.slug.contains('\n') {
            return None;
        }
        if i > 0 {
            raw.push(b'\n');
        }
        raw.extend_from_slice(m.slug.as_bytes());
    }
    let ub = rans::encode_columns(&cols).ok()?;
    let sb = zstd::bulk::compress(&raw, 19).ok()?;
    Some((ub, sb))
}

/// Transcodes any Probe file (v1, v2 or v3) into a v3 file of the given profile.
///
/// Lossless: the result's core dump and full dump equal the source's byte for byte, and its
/// side streams and markets table are the source's bytes. Deterministic: the same source and
/// profile give the same bytes on every machine and thread count. Profile [`Profile::Rc1`]
/// keeps v1 blocks and needs a v1 or v2 source.
///
/// The transcoder decodes one segment at a time; memory is about one decoded segment plus the
/// output.
pub fn transcode(src: &File, profile: Profile) -> Result<Transcoded> {
    let hl = u32::from_le_bytes([src.data[4], src.data[5], src.data[6], src.data[7]]) as usize;
    let mut hv: Value = serde_json::from_slice(&src.data[8..8 + hl])
        .map_err(|e| Error::Format(format!("source header: {}", e)))?;
    let (mo, ml) = src.header.markets_zst;
    let mzst = &src.data[src.body_off + mo as usize..src.body_off + (mo + ml) as usize];
    let mut body: Vec<u8> = Vec::new();
    let mut st: Vec<(String, u64)> = Vec::new();
    let mut seg_parts: Vec<Value> = Vec::with_capacity(src.segments.len());
    for si in 0..src.segments.len() {
        let parts = encode_segment(src, si, profile)?;
        let mut pj = Vec::with_capacity(parts.len());
        for (n, b, c) in parts {
            add_stat(&mut st, &n, b.len());
            pj.push(serde_json::json!([n, body.len(), b.len(), c]));
            body.extend_from_slice(&b);
        }
        seg_parts.push(Value::Array(pj));
    }
    let mk = market_parts(&src.markets).map(|(u, sl)| {
        let m = [body.len(), u.len(), body.len() + u.len(), sl.len()];
        add_stat(&mut st, "mkunits", u.len());
        add_stat(&mut st, "mkslugs", sl.len());
        body.extend_from_slice(&u);
        body.extend_from_slice(&sl);
        m
    });
    let o = hv
        .as_object_mut()
        .ok_or_else(|| Error::Format("source header is not an object".into()))?;
    o.insert("version".into(), VERSION.into());
    o.insert("family".into(), FAMILY.into());
    o.insert("profile".into(), profile.name().into());
    o.insert(
        "src_version".into(),
        src.header.side_layout().header_str().into(),
    );
    o.insert(
        "markets_zst".into(),
        serde_json::json!([body.len(), mzst.len()]),
    );
    o.remove("mkunits");
    o.remove("mkslugs");
    if let Some([uo, ul, so, sl]) = mk {
        o.insert("mkunits".into(), serde_json::json!([uo, ul]));
        o.insert("mkslugs".into(), serde_json::json!([so, sl]));
    }
    let segs = o
        .get_mut("segments")
        .and_then(|s| s.as_array_mut())
        .ok_or_else(|| Error::Format("source header has no segments".into()))?;
    if segs.len() != seg_parts.len() {
        return format_err("source header: segment count differs");
    }
    for (s, p) in segs.iter_mut().zip(seg_parts) {
        s.as_object_mut()
            .ok_or_else(|| Error::Format("source header: a segment is not an object".into()))?
            .insert("parts".into(), p);
    }
    let hj = serde_json::to_vec(&hv).map_err(|e| Error::Format(e.to_string()))?;
    let hj_len = u32::try_from(hj.len()).map_err(|_| Error::Format("header too large".into()))?;
    let mut out = Vec::with_capacity(8 + hj.len() + body.len() + mzst.len());
    out.extend_from_slice(crate::MAGIC);
    out.extend_from_slice(&hj_len.to_le_bytes());
    out.extend_from_slice(&hj);
    out.extend_from_slice(&body);
    out.extend_from_slice(mzst);
    add_stat(&mut st, "markets_zst", mzst.len());
    st.insert(0, ("header".into(), 8 + hj.len() as u64));
    Ok(Transcoded {
        bytes: out,
        part_bytes: st,
    })
}

/// Checks that `dst` decodes to exactly what `src` decodes to: segment by segment, every block's
/// events and levels and the reference order. A transcode is lossless when this passes.
pub fn verify_transcode(src: &File, dst: &File) -> Result<()> {
    if src.segments.len() != dst.segments.len() || src.markets != dst.markets {
        return format_err("verify: segments or markets table differ");
    }
    for si in 0..src.segments.len() {
        #[cfg(feature = "parallel")]
        let (a, b) = (src.segment_core_par(si)?, dst.segment_core_par(si)?);
        #[cfg(not(feature = "parallel"))]
        let (a, b) = (src.segment_core(si)?, dst.segment_core(si)?);
        if a != b {
            return format_err(format!("verify: segment {} decodes differently", si));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lanes_split_on_whole_blocks() {
        assert_eq!(split_lanes(&[]), Vec::<usize>::new());
        assert_eq!(split_lanes(&[5, 5]), vec![0]);
        assert_eq!(split_lanes(&[LANE_EV, 1, LANE_EV + 3, 2]), vec![0, 1, 3]);
    }

    #[test]
    fn profiles_and_codecs_have_names() {
        for p in Profile::ALL {
            assert_eq!(Profile::parse(p.name()), Some(p));
        }
        assert_eq!(Profile::default(), Profile::Fast);
        for id in [0, 2, 16, 17, 18, 19, 20, 21, 22, 23] {
            assert!(codec::name(id).is_some(), "{}", id);
        }
        assert!(codec::name(1).is_none());
        assert!(codec::name(24).is_none());
    }
}
