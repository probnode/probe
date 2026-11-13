//! The decoder core: blocks, the order stream and the reference order of a segment (5.1 steps
//! 1-4), without the side streams. This is the fast path; [`crate::Event`] adds the side streams.
//! A v3 segment decodes through the same API: its blocks into [`Block`]s, its order part into the
//! same arrival order.

use crate::error::{format_err, Error, Result};
use crate::file::File;
use crate::rc;
use crate::uvarint;
pub use crate::walker::{CoreEvent, CoreLevel};
use crate::walker::{Walker, ET_CHECKPOINT, ET_OTHER, ET_TRADE};
use std::io::Write;

/// One decoded block.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Block {
    /// The block's market row, or the segment's `nmkt` for the type-4 block.
    pub row: usize,
    /// Its events in stored order, leading checkpoints first.
    pub events: Vec<CoreEvent>,
    /// Its levels; `CoreEvent::lv0` and `nlv` index this vector.
    pub levels: Vec<CoreLevel>,
    /// Number of leading checkpoints.
    pub n_checkpoints: usize,
}

impl Block {
    /// The levels of event `j`.
    pub fn event_levels(&self, j: usize) -> &[CoreLevel] {
        let e = &self.events[j];
        &self.levels[e.lv0 as usize..(e.lv0 + e.nlv) as usize]
    }
}

/// A decoded segment: its blocks in block-index order, and the reference order as
/// (block index, event index in the block) pairs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SegmentCore {
    /// The blocks, in block-index order.
    pub blocks: Vec<Block>,
    /// The reference order: every block's checkpoints by ascending row, then the real events in
    /// arrival order.
    pub order: Vec<(u32, u32)>,
}

pub(crate) fn floor_div(a: i64, b: i64) -> i64 {
    a.div_euclid(b)
}

impl File {
    /// The `U` a block decodes with: the row's, or 1 for the type-4 block.
    pub(crate) fn block_u(&self, si: usize, row: usize) -> Result<i64> {
        let s = self.seg(si)?;
        if row == s.nmkt {
            return Ok(1);
        }
        let u = self
            .markets
            .get(row)
            .ok_or_else(|| Error::Format(format!("block row {} outside the markets table", row)))?
            .u;
        if !(1..=crate::walker::MAX_U).contains(&u) {
            return format_err(format!("row {}: U {}", row, u));
        }
        Ok(u)
    }

    fn block_bytes(&self, si: usize, bi: usize) -> Result<&[u8]> {
        let s = self.seg(si)?;
        let r = s
            .blocks
            .get(bi)
            .ok_or_else(|| Error::NotFound(format!("segment {}: no block {}", si, bi)))?;
        let bp = self
            .part_bytes(si, "blocks")
            .ok_or_else(|| Error::Format(format!("segment {}: no blocks part", si)))?;
        bp.get(r.offset..r.offset + r.length)
            .ok_or_else(|| Error::Format(format!("segment {}: block {} outside its part", si, bi)))
    }

    pub(crate) fn walk(
        &self,
        w: &mut Walker,
        si: usize,
        bi: usize,
        limit: Option<i64>,
    ) -> Result<Block> {
        let s = self.seg(si)?;
        let row = s
            .blocks
            .get(bi)
            .ok_or_else(|| Error::NotFound(format!("segment {}: no block {}", si, bi)))?
            .row;
        let u = self.block_u(si, row)?;
        let b = self.block_bytes(si, bi)?;
        let mut out = Block {
            row,
            ..Default::default()
        };
        w.decode_block(
            b,
            self.header.shift,
            self.header.flags,
            u,
            s.t0_ns / self.header.unit_ns,
            limit,
            &mut out.events,
            &mut out.levels,
        )
        .map_err(|e| Error::Format(format!("segment {} block row {}: {}", si, row, e)))?;
        while out.n_checkpoints < out.events.len()
            && out.events[out.n_checkpoints].et == ET_CHECKPOINT
        {
            out.n_checkpoints += 1;
        }
        for e in &out.events[out.n_checkpoints..] {
            if e.et == ET_CHECKPOINT {
                return format_err(format!(
                    "segment {} block row {}: checkpoint after the first real event",
                    si, row
                ));
            }
            if (row == s.nmkt) != (e.et == ET_OTHER) {
                return format_err(format!(
                    "segment {} block row {}: event type {} in the wrong block",
                    si, row, e.et
                ));
            }
        }
        Ok(out)
    }

    /// Decodes block `bi` (an index into the segment's block index) alone.
    pub fn decode_block(&self, si: usize, bi: usize) -> Result<Block> {
        if self.is_v3() {
            return self.v3_block(si, bi);
        }
        self.walk(&mut Walker::new(), si, bi, None)
    }

    /// Decodes block `bi` up to receive time `limit` (in `unit_ns`): it stops before the first
    /// non-checkpoint event past it. Returns the block and the book of both outcomes at that
    /// point, `[outcome][side]` as (price units, size units) ascending.
    #[allow(clippy::type_complexity)]
    pub(crate) fn decode_block_until(
        &self,
        si: usize,
        bi: usize,
        limit: i64,
    ) -> Result<(Block, [[Vec<(i64, i64)>; 2]; 2])> {
        if self.is_v3() {
            return self.v3_block_until(si, bi, limit);
        }
        let mut w = Walker::new();
        let b = self.walk(&mut w, si, bi, Some(limit))?;
        let book = [
            [w.book_side(0, 0), w.book_side(0, 1)],
            [w.book_side(1, 0), w.book_side(1, 1)],
        ];
        Ok((b, book))
    }

    /// Decodes segment `si`'s order stream: the block id of every real event in arrival order.
    /// In v3 the order part needs the blocks' clocks, so this decodes the segment's blocks.
    pub fn decode_order(&self, si: usize) -> Result<Vec<u32>> {
        if self.is_v3() {
            let blocks = self.v3_blocks(si, cfg!(feature = "parallel"))?;
            return self.v3_ids(si, &blocks);
        }
        let s = self.seg(si)?;
        let b = self
            .part_bytes(si, "order")
            .ok_or_else(|| Error::Format(format!("segment {}: no order part", si)))?;
        let r = if self.order_fast {
            decode_order_fast(b, s.nmkt, self.header.shift, s.n_ev)
        } else {
            decode_order(b, s.nmkt, self.header.shift, s.n_ev)
        };
        r.map_err(|e| Error::Format(format!("segment {}: {}", si, e)))
    }

    /// Decodes segment `si` into its blocks and the reference order, on one thread.
    pub fn segment_core(&self, si: usize) -> Result<SegmentCore> {
        if self.is_v3() {
            let blocks = self.v3_blocks(si, false)?;
            let ids = self.v3_ids(si, &blocks)?;
            return self.assemble(si, blocks, ids);
        }
        let n = self.seg(si)?.blocks.len();
        let mut w = Walker::new();
        let mut blocks = Vec::with_capacity(n);
        for bi in 0..n {
            blocks.push(self.walk(&mut w, si, bi, None)?);
        }
        let ids = self.decode_order(si)?;
        self.assemble(si, blocks, ids)
    }

    /// Decodes segment `si` with its blocks in parallel (rayon), beside the order stream.
    #[cfg(feature = "parallel")]
    pub fn segment_core_par(&self, si: usize) -> Result<SegmentCore> {
        use rayon::prelude::*;
        if self.is_v3() {
            let blocks = self.v3_blocks(si, true)?;
            let ids = self.v3_ids(si, &blocks)?;
            return self.assemble(si, blocks, ids);
        }
        let n = self.seg(si)?.blocks.len();
        let chunk = 64usize;
        let (ids, blocks) = rayon::join(
            || self.decode_order(si),
            || -> Result<Vec<Block>> {
                let parts: Vec<Result<Vec<Block>>> = (0..n.div_ceil(chunk))
                    .into_par_iter()
                    .map_init(Walker::new, |w, c| {
                        (c * chunk..n.min((c + 1) * chunk))
                            .map(|bi| self.walk(w, si, bi, None))
                            .collect::<Result<Vec<Block>>>()
                    })
                    .collect();
                let mut all = Vec::with_capacity(n);
                for p in parts {
                    all.extend(p?);
                }
                Ok(all)
            },
        );
        self.assemble(si, blocks?, ids?)
    }

    /// Puts a segment's blocks into the reference order (5.1 step 3) and checks the counts.
    fn assemble(&self, si: usize, blocks: Vec<Block>, ids: Vec<u32>) -> Result<SegmentCore> {
        let s = self.seg(si)?;
        let mut by_row = vec![u32::MAX; s.nmkt + 1];
        for (bi, b) in blocks.iter().enumerate() {
            if (b.row < s.nmkt) != s.has_row(b.row) {
                return format_err(format!(
                    "segment {} block row {}: ckpt bitmap disagrees with the block index",
                    si, b.row
                ));
            }
            by_row[b.row] = bi as u32;
        }
        let mut order = Vec::with_capacity(ids.len() + s.n_ck.min(1 << 20));
        for (bi, b) in blocks.iter().enumerate() {
            for j in 0..b.n_checkpoints {
                order.push((bi as u32, j as u32));
            }
        }
        let nck = order.len();
        let mut next: Vec<u32> = blocks.iter().map(|b| b.n_checkpoints as u32).collect();
        let mut nlv = 0usize;
        for &id in &ids {
            let bi = by_row.get(id as usize).copied().unwrap_or(u32::MAX);
            if bi == u32::MAX || next[bi as usize] as usize >= blocks[bi as usize].events.len() {
                return format_err(format!(
                    "segment {}: order: event of block {} past its end",
                    si, id
                ));
            }
            let j = next[bi as usize];
            next[bi as usize] += 1;
            nlv += blocks[bi as usize].events[j as usize].nlv as usize;
            order.push((bi, j));
        }
        for (bi, b) in blocks.iter().enumerate() {
            if next[bi] as usize != b.events.len() {
                return format_err(format!(
                    "segment {} block row {}: events not in the order stream",
                    si, b.row
                ));
            }
        }
        if nck != s.n_ck || ids.len() != s.n_ev || nlv != s.n_lv {
            return format_err(format!(
                "segment {}: counts n_ck {} n_ev {} n_lv {} decoded, header says {} {} {}",
                si,
                nck,
                ids.len(),
                nlv,
                s.n_ck,
                s.n_ev,
                s.n_lv
            ));
        }
        Ok(SegmentCore { blocks, order })
    }
}

/// The order part (3.6): `uv(n)` + the range-coded stream; a move-to-front list over `nmkt + 1`
/// ids, a same-as-previous bit with context = the previous same bit, else `gamma(rank - 1)`.
/// This is the literal list of the specification, O(nmkt) per event.
pub fn decode_order(
    b: &[u8],
    nmkt: usize,
    shift: u32,
    want: usize,
) -> std::result::Result<Vec<u32>, String> {
    let (n, k) = uvarint(b).ok_or("order: bad count")?;
    if n != want as u64 {
        return Err(format!(
            "order: count {}, the segment's n_ev is {}",
            n, want
        ));
    }
    if n > (b.len() as u64 * 8 + 64) * 2048 {
        return Err(format!("order: count {} for {} bytes", n, b.len()));
    }
    // single-rate always: the header shift, or 5 when it says 0
    let shift = if shift == 0 { 5 } else { shift };
    let mut rc = rc::Rc::new(&b[k..], shift);
    let mut ar = Vec::new();
    let mut g = rc::Gamma::new();
    let mut same = [rc::PROB_INIT; 2];
    let mut prev = 0usize;
    let mut list: Vec<u32> = (0..=nmkt as u32).collect();
    let mut ids = Vec::with_capacity((n as usize).min(1 << 20));
    for _ in 0..n {
        let s = rc.bit(&mut same[prev]) as usize;
        prev = s;
        let mut r = 0usize;
        if s == 0 {
            let u = rc::gamma(&mut rc, &mut g, &mut ar);
            if u + 1 >= list.len() as u64 {
                return Err(format!("order: rank {} of {} ids", u + 1, list.len()));
            }
            r = u as usize + 1;
        }
        let id = list[r];
        list.copy_within(0..r, 1);
        list[0] = id;
        ids.push(id);
        if rc.overrun() {
            return Err("order: stream overrun".into());
        }
    }
    Ok(ids)
}

/// [`decode_order`] with an O(log n) move-to-front (the same ids): id i starts in recency slot
/// `nmkt - i`, a move to the front puts the id in a new, higher slot, and rank r is the (r+1)-th
/// occupied slot from the top, found by a Fenwick-tree descent.
pub fn decode_order_fast(
    b: &[u8],
    nmkt: usize,
    shift: u32,
    want: usize,
) -> std::result::Result<Vec<u32>, String> {
    let (n, k) = uvarint(b).ok_or("order: bad count")?;
    if n != want as u64 {
        return Err(format!(
            "order: count {}, the segment's n_ev is {}",
            n, want
        ));
    }
    if n > (b.len() as u64 * 8 + 64) * 2048 {
        return Err(format!("order: count {} for {} bytes", n, b.len()));
    }
    // The tree below takes nmkt + n slots up front. A count far beyond what real streams pack
    // per bit goes to the literal decoder, which grows as it decodes and stops at an overrun.
    if n > (b.len() as u64 * 8 + 1024) * 64 {
        return decode_order(b, nmkt, shift, want);
    }
    let shift = if shift == 0 { 5 } else { shift };
    let mut rc = rc::Rc::new(&b[k..], shift);
    let mut ar = Vec::new();
    let mut g = rc::Gamma::new();
    let mut same = [rc::PROB_INIT; 2];
    let mut prev = 0usize;
    let m = nmkt + 1;
    let size = m + n as usize;
    let mut tree = vec![0i32; size + 1];
    let mut slot_id = vec![0u32; size];
    for s in 0..m {
        slot_id[s] = (nmkt - s) as u32;
        tree[s + 1] = 1;
    }
    for i in 1..=size {
        let p = i + (i & i.wrapping_neg());
        if p <= size {
            tree[p] += tree[i];
        }
    }
    let mut log = 1;
    while (1usize << log) <= size {
        log += 1;
    }
    let mut top = m;
    let mut front = 0u32;
    let mut ids = Vec::with_capacity((n as usize).min(1 << 20));
    for _ in 0..n {
        let s = rc.bit(&mut same[prev]) as usize;
        prev = s;
        if s == 0 {
            let u = rc::gamma(&mut rc, &mut g, &mut ar);
            if u + 1 >= m as u64 {
                return Err(format!("order: rank {} of {} ids", u + 1, m));
            }
            // the (m - r)-th occupied slot from the bottom
            let mut kk = m as i64 - (u as i64 + 1);
            let mut pos = 0usize;
            let mut step = 1usize << log;
            while step > 0 {
                let p = pos + step;
                if p <= size && (tree[p] as i64) < kk {
                    pos = p;
                    kk -= tree[p] as i64;
                }
                step >>= 1;
            }
            let id = slot_id[pos];
            let mut i = pos + 1;
            while i <= size {
                tree[i] -= 1;
                i += i & i.wrapping_neg();
            }
            slot_id[top] = id;
            let mut i = top + 1;
            while i <= size {
                tree[i] += 1;
                i += i & i.wrapping_neg();
            }
            top += 1;
            front = id;
        }
        ids.push(front);
        if rc.overrun() {
            return Err("order: stream overrun".into());
        }
    }
    Ok(ids)
}

/// FNV-1a 64 over a byte stream.
#[derive(Clone, Copy, Debug)]
pub struct Fnv64(pub u64);

impl Default for Fnv64 {
    fn default() -> Self {
        Fnv64(0xcbf29ce484222325)
    }
}

impl Fnv64 {
    /// Hashes more bytes.
    pub fn write(&mut self, b: &[u8]) {
        for &c in b {
            self.0 ^= c as u64;
            self.0 = self.0.wrapping_mul(0x100000001b3);
        }
    }
}

/// The result of [`core_dump`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoreDumpSummary {
    /// Events dumped, checkpoints included.
    pub events: u64,
    /// Levels dumped.
    pub levels: u64,
    /// FNV-1a 64 of the dump text.
    pub fnv64: u64,
}

/// The core dump, the conformance text every reader of the core must reproduce byte for byte:
/// per event, in the reference order,
///
/// ```text
/// E,seg,idx,et,row,outcome,cont,ts_recv_us,ts_ex_ms,fee_rate_bps
/// L,level,outcome,side,price_micro,size_micro,mirror      (one per level)
/// ```
///
/// written to `out` (if any) and hashed.
pub fn core_dump(f: &File, mut out: Option<&mut dyn Write>) -> Result<CoreDumpSummary> {
    use std::fmt::Write as _;
    let mut h = Fnv64::default();
    let (mut ne, mut nl) = (0u64, 0u64);
    let mut line = String::with_capacity(1 << 16);
    let zero = crate::Market::default();
    let unit = f.header.unit_ns;
    for si in 0..f.segments.len() {
        #[cfg(feature = "parallel")]
        let so = f.segment_core_par(si)?;
        #[cfg(not(feature = "parallel"))]
        let so = f.segment_core(si)?;
        let s = &f.segments[si];
        for (idx, &(bi, j)) in so.order.iter().enumerate() {
            let b = &so.blocks[bi as usize];
            let e = &b.events[j as usize];
            let m = if b.row < s.nmkt {
                f.markets.get(b.row).unwrap_or(&zero)
            } else {
                &zero
            };
            let zu = if e.et == ET_TRADE {
                m.trade_size_unit_micro
            } else {
                m.size_unit_micro
            };
            let tns = e.ts.wrapping_mul(unit);
            let _ = writeln!(
                line,
                "E,{},{},{},{},{},{},{},{},{}",
                si,
                idx,
                e.et,
                b.row,
                e.out,
                e.cont,
                floor_div(tns, 1000),
                floor_div(tns, 1_000_000).wrapping_sub(e.lat),
                e.fee
            );
            for (k, l) in b.event_levels(j as usize).iter().enumerate() {
                let _ = writeln!(
                    line,
                    "L,{},{},{},{},{},{}",
                    k,
                    l.out,
                    l.side,
                    l.p.wrapping_mul(m.price_unit_micro),
                    l.z.wrapping_mul(zu),
                    l.mirror as u8
                );
            }
            ne += 1;
            nl += e.nlv as u64;
            if line.len() > 1 << 15 {
                h.write(line.as_bytes());
                if let Some(o) = out.as_mut() {
                    o.write_all(line.as_bytes())?;
                }
                line.clear();
            }
        }
    }
    h.write(line.as_bytes());
    if let Some(o) = out.as_mut() {
        o.write_all(line.as_bytes())?;
    }
    Ok(CoreDumpSummary {
        events: ne,
        levels: nl,
        fnv64: h.0,
    })
}
