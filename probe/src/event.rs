//! Events and levels with every field of the file: both clocks, prices and sizes in micro units,
//! and the side streams (transaction hashes, payloads, best-price exceptions, book hashes).

use crate::decoder::{floor_div, Block, CoreEvent, CoreLevel};
use crate::error::{format_err, Error, Result};
use crate::file::{BlockCounts, File, Market, Version};
use serde::Deserialize;
use serde_json::value::RawValue;

/// An event type (4.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum EventType {
    /// 0: a venue book snapshot, stored as its difference from the held book.
    Book = 0,
    /// 1: a price_change: its entries, mirrors included.
    PriceChange = 1,
    /// 2: a trade (`last_trade_price`), one level.
    Trade = 2,
    /// 3: a tick_size_change; its JSON is the payload.
    TickSize = 3,
    /// 4: any other market-channel event, in the type-4 block; its JSON is the payload.
    Other = 4,
    /// 5: a checkpoint, a token's full book at its block's start. No clock.
    Checkpoint = 5,
}

impl EventType {
    /// The type from its number.
    pub fn from_u8(v: u8) -> Option<EventType> {
        Some(match v {
            0 => EventType::Book,
            1 => EventType::PriceChange,
            2 => EventType::Trade,
            3 => EventType::TickSize,
            4 => EventType::Other,
            5 => EventType::Checkpoint,
            _ => return None,
        })
    }

    /// The type's number, 0..5.
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    /// A short name: `book`, `price_change`, `trade`, `tick_size`, `other`, `checkpoint`.
    pub fn name(self) -> &'static str {
        match self {
            EventType::Book => "book",
            EventType::PriceChange => "price_change",
            EventType::Trade => "trade",
            EventType::TickSize => "tick_size",
            EventType::Other => "other",
            EventType::Checkpoint => "checkpoint",
        }
    }
}

/// The side of a level.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    /// A bid; for a trade, the venue's `BUY`.
    Bid,
    /// An ask; for a trade, the venue's `SELL`.
    Ask,
}

impl Side {
    /// 0 for a bid, 1 for an ask.
    pub fn as_u8(self) -> u8 {
        match self {
            Side::Bid => 0,
            Side::Ask => 1,
        }
    }

    /// `BUY` or `SELL`.
    pub fn venue_str(self) -> &'static str {
        match self {
            Side::Bid => "BUY",
            Side::Ask => "SELL",
        }
    }
}

/// One level of an event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Level {
    /// Outcome 0 or 1: the token is `tokens[outcome]` of the event's market row.
    pub outcome: u8,
    /// Bid or ask (for a trade: the venue's side).
    pub side: Side,
    /// Price in the row's price units.
    pub price_units: i64,
    /// Size in the row's size units (trade size units for a trade).
    pub size_units: i64,
    /// Price in micro (1e-6).
    pub price_micro: i64,
    /// Size in micro shares. 0 removes the level.
    pub size_micro: i64,
    /// A price_change entry stored as the mirror of the previous explicit entry.
    pub mirror: bool,
    /// The delivered `(best_bid, best_ask)` strings where they differ from the rebuilt book
    /// (a `"b"` row); `None` where the rebuilt book reproduces them (4.6).
    pub best: Option<(String, String)>,
    /// The venue's 20-byte book hash of a price_change entry, when the file has hashes.
    pub book_hash: Option<[u8; 20]>,
}

/// One event, with its levels and side-stream fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    /// Segment index in the file.
    pub segment: usize,
    /// Position in the segment's reference order. `None` for an event read through the block
    /// index alone (a one-market read does not decode the order stream).
    pub index: Option<usize>,
    /// Position in its block, checkpoints counted.
    pub block_index: usize,
    /// Market row, or the segment's `nmkt` for a type-4 event.
    pub row: usize,
    /// The event type.
    pub kind: EventType,
    /// Outcome of types 0, 2, 3 and 5.
    pub outcome: u8,
    /// Type 0: the book event is not the first event object of its websocket frame.
    pub cont: bool,
    /// Receive time in ns since the epoch, at the file's 1 µs resolution. `None` for a checkpoint.
    pub recv_ns: Option<i64>,
    /// The venue's timestamp in ms since the epoch. `None` for a checkpoint. A type-4 event has no
    /// venue clock: its value is the receive time in ms.
    pub venue_ms: Option<i64>,
    /// `fee_rate_bps` of a trade, else 0.
    pub fee_rate_bps: i64,
    /// A checkpoint the segment lists as synthesized from a book with no venue snapshot yet.
    pub unsynced: bool,
    /// A trade's 32-byte transaction hash.
    pub tx_hash: Option<[u8; 32]>,
    /// The JSON payload of a type-3 or type-4 event.
    pub payload: Option<String>,
    /// The venue's 20-byte book hash of a type-0 event, when the file has hashes.
    pub book_hash: Option<[u8; 20]>,
    /// The levels.
    pub levels: Vec<Level>,
}

impl Event {
    /// The event's time for ordering and filtering: its receive time, or for a checkpoint the
    /// start of its segment.
    pub fn time_ns(&self, f: &File) -> i64 {
        self.recv_ns
            .unwrap_or_else(|| f.segments.get(self.segment).map_or(0, |s| s.t0_ns))
    }
}

static ZERO_MARKET: Market = Market {
    slug: String::new(),
    condition_id: String::new(),
    tokens: [String::new(), String::new()],
    price_unit_micro: 0,
    size_unit_micro: 0,
    trade_size_unit_micro: 0,
    u: 0,
};

impl File {
    pub(crate) fn row_market(&self, si: usize, row: usize) -> &Market {
        match self.segments.get(si) {
            Some(s) if row < s.nmkt => self.markets.get(row).unwrap_or(&ZERO_MARKET),
            _ => &ZERO_MARKET,
        }
    }

    pub(crate) fn build_event(
        &self,
        si: usize,
        index: Option<usize>,
        block_index: usize,
        row: usize,
        e: &CoreEvent,
        lvs: &[CoreLevel],
    ) -> Event {
        let s = &self.segments[si];
        let m = self.row_market(si, row);
        let zu = if e.et == crate::walker::ET_TRADE {
            m.trade_size_unit_micro
        } else {
            m.size_unit_micro
        };
        let unit = self.header.unit_ns;
        let kind = EventType::from_u8(e.et).unwrap_or(EventType::Other);
        let (recv_ns, venue_ms) = if kind == EventType::Checkpoint {
            (None, None)
        } else {
            let tns = e.ts.wrapping_mul(unit);
            (
                Some(floor_div(tns, 1000).wrapping_mul(1000)),
                Some(floor_div(tns, 1_000_000).wrapping_sub(e.lat)),
            )
        };
        let unsynced = kind == EventType::Checkpoint
            && s.unsynced
                .iter()
                .any(|&(r, o)| r == row as i64 && o == e.out as i64);
        Event {
            segment: si,
            index,
            block_index,
            row,
            kind,
            outcome: e.out,
            cont: e.cont != 0,
            recv_ns,
            venue_ms,
            fee_rate_bps: e.fee,
            unsynced,
            tx_hash: None,
            payload: None,
            book_hash: None,
            levels: lvs
                .iter()
                .map(|l| Level {
                    outcome: l.out,
                    side: if l.side == 0 { Side::Bid } else { Side::Ask },
                    price_units: l.p,
                    size_units: l.z,
                    price_micro: l.p.wrapping_mul(m.price_unit_micro),
                    size_micro: l.z.wrapping_mul(zu),
                    mirror: l.mirror,
                    best: None,
                    book_hash: None,
                })
                .collect(),
        }
    }

    /// Segment `si`'s events in the reference order, with every side stream attached (5.1).
    pub fn segment_events(&self, si: usize) -> Result<Vec<Event>> {
        self.segment_events_opts(si, true)
    }

    pub(crate) fn segment_events_opts(&self, si: usize, hashes: bool) -> Result<Vec<Event>> {
        #[cfg(feature = "parallel")]
        let core = self.segment_core_par(si)?;
        #[cfg(not(feature = "parallel"))]
        let core = self.segment_core(si)?;
        let mut evs = Vec::with_capacity(core.order.len());
        let mut pos: Vec<Vec<usize>> = core
            .blocks
            .iter()
            .map(|b| vec![usize::MAX; b.events.len()])
            .collect();
        for (idx, &(bi, j)) in core.order.iter().enumerate() {
            let b = &core.blocks[bi as usize];
            evs.push(self.build_event(
                si,
                Some(idx),
                j as usize,
                b.row,
                &b.events[j as usize],
                b.event_levels(j as usize),
            ));
            pos[bi as usize][j as usize] = idx;
        }
        let side = SideStreams::read(self, si, hashes)?;
        match self.header.side_layout() {
            Version::V1 | Version::V3 => {
                let nck = self.segments[si].n_ck.min(evs.len());
                attach_v1(&mut evs, nck, &side)?;
            }
            Version::V2 => {
                let s = &self.segments[si];
                let (mut np, mut nb) = (0usize, 0usize);
                for (bi, entry) in s.blocks.iter().enumerate() {
                    let c = entry
                        .counts
                        .ok_or_else(|| Error::Format("v2 block without counts".into()))?;
                    attach_block(&mut evs, &pos[bi], &c, &side, self.header.has_hash).map_err(
                        |e| Error::Format(format!("segment {} block row {}: {}", si, entry.row, e)),
                    )?;
                    np += c.n_pay;
                    nb += c.n_bex;
                }
                if np != side.payloads.len() || nb != side.best.len() {
                    return format_err(format!(
                        "segment {}: payload.zst: bidx counts {} payloads, {} best rows; the part has {}, {}",
                        si,
                        np,
                        nb,
                        side.payloads.len(),
                        side.best.len()
                    ));
                }
            }
        }
        Ok(evs)
    }

    /// The events of one block read alone through the block index (no order stream), with the
    /// side streams of a v2 file attached from the block's runs; `index` is `None`. In a v1 file
    /// the side-stream fields stay empty (v1 orders them by the whole segment).
    pub(crate) fn block_events(
        &self,
        si: usize,
        bi: usize,
        side: Option<&SideStreams>,
    ) -> Result<Vec<Event>> {
        let b: Block = self.decode_block(si, bi)?;
        let mut evs: Vec<Event> = (0..b.events.len())
            .map(|j| self.build_event(si, None, j, b.row, &b.events[j], b.event_levels(j)))
            .collect();
        if let (Some(sd), Version::V2) = (side, self.header.side_layout()) {
            let entry = &self.segments[si].blocks[bi];
            let c = entry
                .counts
                .ok_or_else(|| Error::Format("v2 block without counts".into()))?;
            let pos: Vec<usize> = (0..evs.len()).collect();
            attach_block(&mut evs, &pos, &c, sd, self.header.has_hash).map_err(|e| {
                Error::Format(format!("segment {} block row {}: {}", si, entry.row, e))
            })?;
        }
        Ok(evs)
    }
}

#[derive(Deserialize, Default)]
struct PayloadJson<'a> {
    #[serde(default)]
    p: Vec<String>,
    #[serde(default, borrow)]
    b: Vec<Vec<&'a RawValue>>,
}

/// One best-price exception row: (event index, entry index, best_bid, best_ask).
pub(crate) type BestRow = (i64, i64, String, String);

/// A segment's side streams, read and parsed.
pub(crate) struct SideStreams<'f> {
    pub(crate) tx: &'f [u8],
    pub(crate) hash: Option<&'f [u8]>,
    pub(crate) payloads: Vec<String>,
    pub(crate) best: Vec<BestRow>,
}

impl<'f> SideStreams<'f> {
    pub(crate) fn read(f: &'f File, si: usize, hashes: bool) -> Result<SideStreams<'f>> {
        let tx = f.part_bytes(si, "tx").unwrap_or(&[]);
        if tx.len() % 32 != 0 {
            return format_err(format!("segment {}: tx part of {} bytes", si, tx.len()));
        }
        let hash = if f.header.has_hash && hashes {
            let h = f.part_bytes(si, "hash").ok_or_else(|| {
                Error::Format(format!("segment {}: has_hash but no hash part", si))
            })?;
            if h.len() % 20 != 0 {
                return format_err(format!("segment {}: hash part of {} bytes", si, h.len()));
            }
            Some(h)
        } else {
            None
        };
        let (payloads, best) = match f.part_bytes(si, "payload.zst") {
            None => (Vec::new(), Vec::new()),
            Some(pz) => {
                let j = zstd::stream::decode_all(pz)
                    .map_err(|e| Error::Format(format!("segment {}: payload.zst: {}", si, e)))?;
                let pl: PayloadJson = serde_json::from_slice(&j)
                    .map_err(|e| Error::Format(format!("segment {}: payload.zst: {}", si, e)))?;
                let mut best = Vec::with_capacity(pl.b.len());
                for row in &pl.b {
                    best.push(
                        best_row(row)
                            .map_err(|e| Error::Format(format!("segment {}: {}", si, e)))?,
                    );
                }
                (pl.p, best)
            }
        };
        Ok(SideStreams {
            tx,
            hash,
            payloads,
            best,
        })
    }
}

fn best_row(b: &[&RawValue]) -> std::result::Result<BestRow, String> {
    if b.len() != 4 {
        return Err(format!(
            "payload.zst: best exception with {} fields",
            b.len()
        ));
    }
    let ei: i64 = serde_json::from_str(b[0].get())
        .map_err(|_| format!("payload.zst: best exception index {}", b[0].get()))?;
    let li: i64 = serde_json::from_str(b[1].get())
        .map_err(|_| format!("payload.zst: best exception index {}", b[1].get()))?;
    Ok((ei, li, best_string(b[2])?, best_string(b[3])?))
}

/// A delivered best_bid / best_ask: a JSON string's value; null as empty; anything else as its
/// JSON text.
fn best_string(v: &RawValue) -> std::result::Result<String, String> {
    let t = v.get();
    if t.starts_with('"') {
        return serde_json::from_str::<String>(t).map_err(|e| format!("payload.zst: {}", e));
    }
    if t == "null" {
        return Ok(String::new());
    }
    Ok(t.to_string())
}

fn hash20(b: &[u8]) -> [u8; 20] {
    let mut h = [0u8; 20];
    h.copy_from_slice(b);
    h
}

fn hash32(b: &[u8]) -> [u8; 32] {
    let mut h = [0u8; 32];
    h.copy_from_slice(b);
    h
}

/// v1: the side streams follow the real events in reference order (5.1 step 5). `evs[nck..]` are
/// the real events; `"b"` rows name the index in the decoded segment.
fn attach_v1(evs: &mut [Event], nck: usize, sd: &SideStreams) -> Result<()> {
    let (mut np, mut nt, mut nh) = (0usize, 0usize, 0usize);
    let hash = sd.hash;
    let take_hash = |nh: &mut usize| -> Result<Option<[u8; 20]>> {
        match hash {
            None => Ok(None),
            Some(h) => {
                let item = h.get(*nh..*nh + 20).ok_or_else(|| {
                    Error::Format(format!("hash part: {} bytes, need more", h.len()))
                })?;
                *nh += 20;
                Ok(Some(hash20(item)))
            }
        }
    };
    for e in evs[nck..].iter_mut() {
        match e.kind {
            EventType::TickSize | EventType::Other => {
                let p = sd.payloads.get(np).ok_or_else(|| {
                    Error::Format(format!(
                        "payload.zst: {} payloads, need more",
                        sd.payloads.len()
                    ))
                })?;
                e.payload = Some(p.clone());
                np += 1;
            }
            EventType::Trade => {
                let t = sd.tx.get(nt..nt + 32).ok_or_else(|| {
                    Error::Format(format!("tx part: {} bytes, need more", sd.tx.len()))
                })?;
                e.tx_hash = Some(hash32(t));
                nt += 32;
            }
            EventType::Book => e.book_hash = take_hash(&mut nh)?,
            EventType::PriceChange => {
                for l in e.levels.iter_mut() {
                    l.book_hash = take_hash(&mut nh)?;
                }
            }
            EventType::Checkpoint => {}
        }
    }
    let hl = hash.map_or(0, |h| h.len());
    if np != sd.payloads.len() || nt != sd.tx.len() || nh != hl {
        return format_err(format!(
            "side streams: used {}/{} payloads, {}/{} tx bytes, {}/{} hash bytes",
            np,
            sd.payloads.len(),
            nt,
            sd.tx.len(),
            nh,
            hl
        ));
    }
    if evs.len() == nck {
        if !sd.best.is_empty() {
            return format_err("payload.zst: best exceptions without events");
        }
        return Ok(());
    }
    for (ei, li, bb, ba) in &sd.best {
        let (ei, li) = (*ei, *li);
        if ei < nck as i64 || ei >= evs.len() as i64 || li < 0 {
            return format_err(format!(
                "payload.zst: best exception [{}, {}] outside the segment",
                ei, li
            ));
        }
        let lv = evs[ei as usize]
            .levels
            .get_mut(li as usize)
            .ok_or_else(|| {
                Error::Format(format!(
                    "payload.zst: best exception [{}, {}] outside the segment",
                    ei, li
                ))
            })?;
        lv.best = Some((bb.clone(), ba.clone()));
    }
    Ok(())
}

/// v2: one block's side streams from its runs (3.7). `pos[j]` is the index in `evs` of the
/// block's event j (block order, checkpoints first). The block's counts must equal its entry's.
fn attach_block(
    evs: &mut [Event],
    pos: &[usize],
    c: &BlockCounts,
    sd: &SideStreams,
    file_has_hash: bool,
) -> std::result::Result<(), String> {
    if pos.len() != c.n_ev {
        return Err(format!(
            "{} events decoded, bidx says {}",
            pos.len(),
            c.n_ev
        ));
    }
    let tx = sd
        .tx
        .get(32 * c.tx_start..32 * (c.tx_start + c.n_trade))
        .ok_or_else(|| {
            format!(
                "tx run [{}, +{}) outside the {}-byte part",
                32 * c.tx_start,
                32 * c.n_trade,
                sd.tx.len()
            )
        })?;
    let hash: Option<&[u8]> = match sd.hash {
        Some(h) => Some(
            h.get(20 * c.hash_start..20 * (c.hash_start + c.n_hash))
                .ok_or_else(|| {
                    format!(
                        "hash run [{}, +{}) outside the {}-byte part",
                        20 * c.hash_start,
                        20 * c.n_hash,
                        h.len()
                    )
                })?,
        ),
        _ => None,
    };
    let pay = sd
        .payloads
        .get(c.pay_start..c.pay_start + c.n_pay)
        .ok_or_else(|| {
            format!(
                "payload run [{}, +{}) outside {} payloads",
                c.pay_start,
                c.n_pay,
                sd.payloads.len()
            )
        })?;
    let bex = sd
        .best
        .get(c.bex_start..c.bex_start + c.n_bex)
        .ok_or_else(|| {
            format!(
                "best rows [{}, +{}) outside {} rows",
                c.bex_start,
                c.n_bex,
                sd.best.len()
            )
        })?;
    let (mut nlv, mut nt, mut np, mut nh) = (0usize, 0usize, 0usize, 0usize);
    let take_hash = |nh: &mut usize| -> std::result::Result<Option<[u8; 20]>, String> {
        match hash {
            None => Ok(None),
            Some(h) => {
                let item = h
                    .get(*nh..*nh + 20)
                    .ok_or_else(|| format!("hash run: {} items, need more", h.len() / 20))?;
                *nh += 20;
                Ok(Some(hash20(item)))
            }
        }
    };
    for &i in pos {
        let e = evs.get_mut(i).ok_or("event position outside the segment")?;
        nlv += e.levels.len();
        match e.kind {
            EventType::TickSize | EventType::Other => {
                let p = pay
                    .get(np)
                    .ok_or_else(|| format!("payload run: {} payloads, need more", pay.len()))?;
                e.payload = Some(p.clone());
                np += 1;
            }
            EventType::Trade => {
                let t = tx
                    .get(nt..nt + 32)
                    .ok_or_else(|| format!("tx run: {} trades, need more", tx.len() / 32))?;
                e.tx_hash = Some(hash32(t));
                nt += 32;
            }
            EventType::Book => e.book_hash = take_hash(&mut nh)?,
            EventType::PriceChange => {
                for l in e.levels.iter_mut() {
                    l.book_hash = take_hash(&mut nh)?;
                }
            }
            EventType::Checkpoint => {}
        }
    }
    let hl = hash.map_or(0, |h| h.len());
    if nlv != c.n_lv || nt != tx.len() || np != pay.len() || nh != hl {
        return Err(format!(
            "side counts: {} levels, {} trades, {} payloads, {} hash items decoded; bidx says {} {} {} {}",
            nlv,
            nt / 32,
            np,
            nh / 20,
            c.n_lv,
            c.n_trade,
            c.n_pay,
            c.n_hash
        ));
    }
    if !file_has_hash && c.n_hash != 0 {
        return Err(format!("bidx: {} hash items without has_hash", c.n_hash));
    }
    for (ei, li, bb, ba) in bex {
        let (ei, li) = (*ei, *li);
        if ei < 0 || ei as usize >= pos.len() || li < 0 {
            return Err(format!(
                "payload.zst: best exception [{}, {}] outside the block",
                ei, li
            ));
        }
        let e = evs
            .get_mut(pos[ei as usize])
            .ok_or("event position outside the segment")?;
        let lv = e.levels.get_mut(li as usize).ok_or_else(|| {
            format!(
                "payload.zst: best exception [{}, {}] outside the block",
                ei, li
            )
        })?;
        lv.best = Some((bb.clone(), ba.clone()));
    }
    Ok(())
}
