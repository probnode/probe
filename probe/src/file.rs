//! The container (specification section 3): magic, header JSON, markets table, segments, parts and
//! the block index.

use crate::error::{format_err, Error, Result};
use crate::uvarint;
use serde::Deserialize;
use std::path::Path;

/// The magic of every Probe file, v1 and v2.
pub const MAGIC: &[u8; 4] = b"EVM4";

/// The flag bits this reader decodes (3.11): 16 (`lat` as a delta) and 64 (price from the best).
/// A file that sets any other bit is refused.
pub const SUPPORTED_FLAGS: u32 = 16 | 64;

/// A Probe format version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Version {
    /// Probe v1, header version `"evmin/4"`: side streams in the segment's arrival order.
    V1,
    /// Probe v2, header version `"evmin/5"`: per-block counts in the block index, side streams in
    /// block order.
    V2,
}

impl Version {
    /// The header's `version` string: `"evmin/4"` or `"evmin/5"`.
    pub fn header_str(self) -> &'static str {
        match self {
            Version::V1 => "evmin/4",
            Version::V2 => "evmin/5",
        }
    }

    /// The version number: 1 or 2.
    pub fn number(self) -> u32 {
        match self {
            Version::V1 => 1,
            Version::V2 => 2,
        }
    }

    /// Parses a header `version` string; `None` for any string this reader does not implement.
    pub fn from_header_str(s: &str) -> Option<Version> {
        match s {
            "evmin/4" => Some(Version::V1),
            "evmin/5" => Some(Version::V2),
            _ => None,
        }
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "v{}", self.number())
    }
}

/// The file header (3.2), checked against what this reader implements.
#[derive(Clone, Debug)]
pub struct Header {
    /// The format version.
    pub version: Version,
    /// Identifies the writer. Informative only.
    pub recorder: String,
    /// The receive clock's unit in ns. Always 1000.
    pub unit_ns: i64,
    /// Segment length in seconds. Always 300.
    pub seg_s: i64,
    /// The range coder's adaptation shift (3.10.1); 0 selects the two-rate mix in blocks.
    pub shift: u32,
    /// Coding-variant bits (3.11).
    pub flags: u32,
    /// Whether segments carry the optional `hash` part (venue book hashes).
    pub has_hash: bool,
    /// The file's UTC hour start, ns since the epoch.
    pub hour_start_ns: i64,
    /// `[offset, length]` of the zstd markets table in the body.
    pub markets_zst: (u64, u64),
}

/// One row of the markets table (3.4). A market whose units become finer during the hour gets a
/// new row; within one segment a market has at most one row.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct Market {
    /// The venue's market slug.
    #[serde(default)]
    pub slug: String,
    /// The venue's condition id.
    #[serde(default)]
    pub condition_id: String,
    /// The two outcome token ids: outcome 0 is `tokens[0]`, outcome 1 is `tokens[1]`.
    #[serde(default)]
    pub tokens: [String; 2],
    /// One price unit in micro (1e-6).
    #[serde(default)]
    pub price_unit_micro: i64,
    /// One book-size unit in micro shares (event types 0, 1 and 5).
    #[serde(default)]
    pub size_unit_micro: i64,
    /// One trade-size unit in micro shares (event type 2).
    #[serde(default)]
    pub trade_size_unit_micro: i64,
    /// Price units in 1.0: `U = 1e6 / price_unit_micro`. The mirror price of `p` is `U - p`.
    #[serde(rename = "U", default)]
    pub u: i64,
}

/// A named byte range of a segment, offset into the body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    /// The part name: `order`, `bidx`, `blocks`, `tx`, `payload.zst`, `hash`, or one a later
    /// writer added (readers ignore names they do not know).
    pub name: String,
    /// Offset into the body.
    pub offset: u64,
    /// Length in bytes.
    pub length: u64,
}

/// A v2 block's side-stream counts (3.7) and their prefix sums over the blocks before it, which
/// are the block's start in each side stream.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BlockCounts {
    /// Events of the block, checkpoints included.
    pub n_ev: usize,
    /// Levels of the block, checkpoint levels included.
    pub n_lv: usize,
    /// Type-2 trades: 32-byte items in `tx`.
    pub n_trade: usize,
    /// Type-3 and type-4 events: strings in `payload.zst` `"p"`.
    pub n_pay: usize,
    /// 20-byte items in `hash` (0 without `has_hash`).
    pub n_hash: usize,
    /// Rows in `payload.zst` `"b"` (best-price exceptions).
    pub n_bex: usize,
    /// The block's first item in `tx` (32-byte items).
    pub tx_start: usize,
    /// The block's first string in `"p"`.
    pub pay_start: usize,
    /// The block's first item in `hash` (20-byte items).
    pub hash_start: usize,
    /// The block's first row in `"b"`.
    pub bex_start: usize,
}

/// One entry of a segment's block index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockEntry {
    /// The market row, or the segment's `nmkt` for the block of type-4 events.
    pub row: usize,
    /// Offset into the segment's `blocks` part.
    pub offset: usize,
    /// Length in bytes.
    pub length: usize,
    /// The v2 counts; `None` in a v1 file.
    pub counts: Option<BlockCounts>,
}

/// One 5-minute segment.
#[derive(Clone, Debug)]
pub struct Segment {
    /// Segment start, ns since the epoch.
    pub t0_ns: i64,
    /// Market rows in use: block ids `0..nmkt` are rows, `nmkt` is the type-4 block.
    pub nmkt: usize,
    /// Real events (checkpoints excluded).
    pub n_ev: usize,
    /// Levels of the real events.
    pub n_lv: usize,
    /// Checkpoint events.
    pub n_ck: usize,
    /// Checkpoints synthesized from a book that had no venue snapshot yet: (row, outcome).
    pub unsynced: Vec<(i64, i64)>,
    /// The segment's parts.
    pub parts: Vec<Part>,
    /// The block index, ascending row.
    pub blocks: Vec<BlockEntry>,
    bitmap: Vec<u8>,
}

impl Segment {
    /// The checkpoint bitmap bit of a row: the row has a block in this segment, which opens with
    /// checkpoints.
    pub fn has_row(&self, row: usize) -> bool {
        row < self.nmkt
            && (row >> 3) < self.bitmap.len()
            && (self.bitmap[row >> 3] >> (row & 7)) & 1 == 1
    }

    /// A part by name.
    pub fn part(&self, name: &str) -> Option<&Part> {
        self.parts.iter().find(|p| p.name == name)
    }

    /// The index in `blocks` of a row's block (a market row, or `nmkt` for the type-4 block).
    pub fn find_block(&self, row: usize) -> Option<usize> {
        if row < self.nmkt && !self.has_row(row) {
            return None;
        }
        self.blocks.binary_search_by_key(&row, |b| b.row).ok()
    }

    /// Whether the segment has a type-4 block.
    pub fn has_other_block(&self) -> bool {
        self.blocks.last().is_some_and(|b| b.row == self.nmkt)
    }

    /// Number of rows whose bitmap bit is set.
    pub fn bitmap_rows(&self) -> usize {
        (0..self.nmkt).filter(|&r| self.has_row(r)).count()
    }
}

pub(crate) enum Bytes {
    Map(memmap2::Mmap),
    Vec(Vec<u8>),
}

impl std::ops::Deref for Bytes {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Bytes::Map(m) => &m[..],
            Bytes::Vec(v) => &v[..],
        }
    }
}

#[derive(Deserialize)]
struct HeaderJson {
    #[serde(default)]
    magic: String,
    version: serde_json::Value,
    #[serde(default)]
    recorder: String,
    family: String,
    layout: String,
    unit_ns: i64,
    #[serde(default = "default_seg_s")]
    seg_s: i64,
    shift: i64,
    flags: i64,
    #[serde(default)]
    has_hash: bool,
    #[serde(default)]
    hour_start_ns: i64,
    markets_zst: (i64, i64),
    segments: Vec<SegmentJson>,
}

fn default_seg_s() -> i64 {
    300
}

#[derive(Deserialize)]
struct SegmentJson {
    t0_ns: i64,
    parts: Vec<(String, i64, i64)>,
    nmkt: usize,
    n_ev: usize,
    n_lv: usize,
    n_ck: usize,
    #[serde(default)]
    ckpt: String,
    #[serde(default)]
    unsynced: Option<Vec<(i64, i64)>>,
}

/// An open Probe file: the header, the markets table and every segment's block index are read
/// at open; blocks and side streams are decoded on request.
pub struct File {
    pub(crate) data: Bytes,
    pub(crate) body_off: usize,
    pub(crate) header: Header,
    pub(crate) markets: Vec<Market>,
    pub(crate) segments: Vec<Segment>,
    pub(crate) order_fast: bool,
}

impl std::fmt::Debug for File {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("File")
            .field("size", &self.data.len())
            .field("header", &self.header)
            .field("markets", &self.markets.len())
            .field("segments", &self.segments.len())
            .finish()
    }
}

/// Reads only the header of a file held in `b` (its first `8 + header_len` bytes suffice).
pub fn read_header(b: &[u8]) -> Result<Header> {
    let (h, _) = parse_header_json(b)?;
    check_header(&h)
}

fn parse_header_json(b: &[u8]) -> Result<(HeaderJson, usize)> {
    if b.len() < 8 || &b[..4] != MAGIC {
        return format_err("not a Probe file (magic EVM4)");
    }
    let hl = u32::from_le_bytes([b[4], b[5], b[6], b[7]]) as usize;
    if hl > b.len() - 8 {
        return format_err(format!(
            "header length {} past the end of the {}-byte file",
            hl,
            b.len()
        ));
    }
    let h: HeaderJson = serde_json::from_slice(&b[8..8 + hl])
        .map_err(|e| Error::Format(format!("header: {}", e)))?;
    Ok((h, 8 + hl))
}

fn check_header(h: &HeaderJson) -> Result<Header> {
    let version = match h.version.as_str().and_then(Version::from_header_str) {
        Some(v) => v,
        None => {
            return Err(Error::Unsupported(format!(
                "version {}, want \"evmin/4\" or \"evmin/5\"",
                h.version
            )))
        }
    };
    if !h.magic.is_empty() && h.magic != "EVM4" {
        return format_err(format!("header magic {:?}", h.magic));
    }
    if h.family != "rc" || h.layout != "mkt" {
        return Err(Error::Unsupported(format!(
            "family {:?} layout {:?}, want rc / mkt",
            h.family, h.layout
        )));
    }
    if h.unit_ns != 1000 {
        return Err(Error::Unsupported(format!(
            "unit_ns {}, want 1000",
            h.unit_ns
        )));
    }
    if h.seg_s != 300 {
        return Err(Error::Unsupported(format!("seg_s {}, want 300", h.seg_s)));
    }
    if !(0..=15).contains(&h.shift) {
        return Err(Error::Unsupported(format!("shift {}", h.shift)));
    }
    if h.flags < 0 || h.flags > u32::MAX as i64 || (h.flags as u32) & !SUPPORTED_FLAGS != 0 {
        return Err(Error::Unsupported(format!(
            "flags {}: this reader decodes bits 16 and 64 only",
            h.flags
        )));
    }
    if h.markets_zst.0 < 0 || h.markets_zst.1 < 0 {
        return format_err("markets_zst range");
    }
    Ok(Header {
        version,
        recorder: h.recorder.clone(),
        unit_ns: h.unit_ns,
        seg_s: h.seg_s,
        shift: h.shift as u32,
        flags: h.flags as u32,
        has_hash: h.has_hash,
        hour_start_ns: h.hour_start_ns,
        markets_zst: (h.markets_zst.0 as u64, h.markets_zst.1 as u64),
    })
}

/// Lenient base64 (standard alphabet, padding optional).
fn b64decode(s: &str) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut acc = 0u32;
    let mut nb = 0;
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => return format_err(format!("ckpt: bad base64 byte {}", c)),
        } as u32;
        acc = ((acc << 6) | v) & 0xFFFFFF;
        nb += 6;
        if nb >= 8 {
            nb -= 8;
            out.push((acc >> nb) as u8);
        }
    }
    Ok(out)
}

/// bidx entries: `uv(row - prev_row - 1) uv(length)`, and in v2 six counts (3.7).
fn scan_bidx(b: &[u8], nmkt: usize, blocks_len: usize, v2: bool) -> Result<Vec<BlockEntry>> {
    let nf = if v2 { 8 } else { 2 };
    let mut out = Vec::new();
    let mut p = 0;
    let mut row: i64 = -1;
    let mut off = 0usize;
    let mut v = [0u64; 8];
    let mut start = BlockCounts::default();
    while p < b.len() {
        for slot in v.iter_mut().take(nf) {
            let (x, n) =
                uvarint(&b[p..]).ok_or_else(|| Error::Format("bidx: bad varint".into()))?;
            *slot = x;
            p += n;
        }
        let (d, l) = (v[0], v[1]);
        if d > nmkt as u64 || l > (blocks_len - off) as u64 {
            return format_err("bidx: entry out of range");
        }
        for &c in &v[2..nf] {
            if c > 1 << 40 {
                return format_err(format!("bidx: count {} out of range", c));
            }
        }
        row += d as i64 + 1;
        if row as usize > nmkt {
            return format_err(format!("bidx: row {} > nmkt {}", row, nmkt));
        }
        let counts = if v2 {
            let c = BlockCounts {
                n_ev: v[2] as usize,
                n_lv: v[3] as usize,
                n_trade: v[4] as usize,
                n_pay: v[5] as usize,
                n_hash: v[6] as usize,
                n_bex: v[7] as usize,
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
        out.push(BlockEntry {
            row: row as usize,
            offset: off,
            length: l as usize,
            counts,
        });
        off += l as usize;
    }
    if off != blocks_len {
        return format_err(format!("bidx covers {} of {} block bytes", off, blocks_len));
    }
    Ok(out)
}

impl File {
    /// Opens a file by memory map and reads its header, markets table and block indexes.
    ///
    /// The file must not be modified while it is open: the map would see the change.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<File> {
        let path = path.as_ref();
        let fh = std::fs::File::open(path).map_err(|e| {
            Error::Io(std::io::Error::new(
                e.kind(),
                format!("{}: {}", path.display(), e),
            ))
        })?;
        let len = fh.metadata()?.len();
        if len == 0 {
            return File::from_bytes(Vec::new());
        }
        // SAFETY: the map is read only; the documented contract is that the file is not modified
        // while open. A change would alter the bytes read, never memory safety of this crate's
        // own data structures, which hold no pointers into the map.
        let m = unsafe { memmap2::Mmap::map(&fh) }?;
        File::parse(Bytes::Map(m))
    }

    /// Parses a file held in memory.
    pub fn from_bytes(data: Vec<u8>) -> Result<File> {
        File::parse(Bytes::Vec(data))
    }

    fn parse(data: Bytes) -> Result<File> {
        let b: &[u8] = &data;
        let (hj, body_off) = parse_header_json(b)?;
        let header = check_header(&hj)?;
        let v2 = header.version == Version::V2;
        let body_len = b.len() - body_off;
        let rng = |off: i64, n: i64| -> Result<(usize, usize)> {
            if off < 0
                || n < 0
                || off as u64 > body_len as u64
                || n as u64 > body_len as u64 - off as u64
            {
                return format_err(format!(
                    "range [{}, +{}) outside the {}-byte body",
                    off, n, body_len
                ));
            }
            Ok((off as usize, n as usize))
        };
        let (mo, ml) = rng(hj.markets_zst.0, hj.markets_zst.1)?;
        let mj = zstd::stream::decode_all(&b[body_off + mo..body_off + mo + ml])
            .map_err(|e| Error::Format(format!("markets_zst: {}", e)))?;
        let markets: Vec<Market> =
            serde_json::from_slice(&mj).map_err(|e| Error::Format(format!("markets: {}", e)))?;
        let mut segments = Vec::with_capacity(hj.segments.len());
        for (si, s) in hj.segments.iter().enumerate() {
            if s.nmkt > markets.len() {
                return format_err(format!(
                    "segment {}: nmkt {} > {} market rows",
                    si,
                    s.nmkt,
                    markets.len()
                ));
            }
            let mut parts = Vec::with_capacity(s.parts.len());
            for p in &s.parts {
                let (o, n) = rng(p.1, p.2)
                    .map_err(|e| Error::Format(format!("segment {} part {}: {}", si, p.0, e)))?;
                parts.push(Part {
                    name: p.0.clone(),
                    offset: o as u64,
                    length: n as u64,
                });
            }
            let get = |name: &str| -> Result<(usize, usize)> {
                parts
                    .iter()
                    .find(|p| p.name == name)
                    .map(|p| (p.offset as usize, p.length as usize))
                    .ok_or_else(|| Error::Format(format!("segment {}: no part {:?}", si, name)))
            };
            let bp = get("blocks")?;
            get("order")?;
            let bi = get("bidx")?;
            let bidx = &b[body_off + bi.0..body_off + bi.0 + bi.1];
            let blocks = scan_bidx(bidx, s.nmkt, bp.1, v2)
                .map_err(|e| Error::Format(format!("segment {}: {}", si, e)))?;
            let seg = Segment {
                t0_ns: s.t0_ns,
                nmkt: s.nmkt,
                n_ev: s.n_ev,
                n_lv: s.n_lv,
                n_ck: s.n_ck,
                unsynced: s.unsynced.clone().unwrap_or_default(),
                parts,
                blocks,
                bitmap: b64decode(&s.ckpt)
                    .map_err(|e| Error::Format(format!("segment {}: {}", si, e)))?,
            };
            if v2 {
                check_side_totals(&seg, header.has_hash)
                    .map_err(|e| Error::Format(format!("segment {}: {}", si, e)))?;
            }
            segments.push(seg);
        }
        Ok(File {
            data,
            body_off,
            header,
            markets,
            segments,
            order_fast: true,
        })
    }

    /// The header.
    pub fn header(&self) -> &Header {
        &self.header
    }

    /// The format version.
    pub fn version(&self) -> Version {
        self.header.version
    }

    /// The markets table.
    pub fn markets(&self) -> &[Market] {
        &self.markets
    }

    /// One market row.
    pub fn market(&self, row: usize) -> Option<&Market> {
        self.markets.get(row)
    }

    /// The segments, in ascending `t0_ns`.
    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// The file's size in bytes.
    pub fn size(&self) -> u64 {
        self.data.len() as u64
    }

    /// The token id of outcome `out` of a block's row; `""` for the type-4 block.
    pub fn token(&self, seg: usize, row: usize, out: u8) -> &str {
        match self.segments.get(seg) {
            Some(s) if row < s.nmkt => self
                .markets
                .get(row)
                .and_then(|m| m.tokens.get(out as usize))
                .map(|t| t.as_str())
                .unwrap_or(""),
            _ => "",
        }
    }

    /// The market rows of a market, named by slug, condition id, or row number. A unit change
    /// gives one market several rows; they are returned in ascending order.
    pub fn market_rows(&self, key: &str) -> Result<Vec<usize>> {
        let rows: Vec<usize> = (0..self.markets.len())
            .filter(|&i| self.markets[i].slug == key || self.markets[i].condition_id == key)
            .collect();
        if !rows.is_empty() {
            return Ok(rows);
        }
        if let Ok(r) = key.parse::<usize>() {
            if r < self.markets.len() {
                return Ok(vec![r]);
            }
        }
        Err(Error::NotFound(format!(
            "no market {:?} (slug, condition id or row)",
            key
        )))
    }

    /// The bytes of a segment's part, `None` when the segment has no such part.
    pub fn part_bytes(&self, seg: usize, name: &str) -> Option<&[u8]> {
        let p = self.segments.get(seg)?.part(name)?;
        let a = self.body_off + p.offset as usize;
        self.data.get(a..a + p.length as usize)
    }

    /// Chooses the order-stream decoder: `true` (the default) for the O(log n) one, `false` for
    /// the literal move-to-front list of the specification. Both give the same ids.
    pub fn set_order_fast(&mut self, fast: bool) {
        self.order_fast = fast;
    }

    pub(crate) fn seg(&self, si: usize) -> Result<&Segment> {
        self.segments.get(si).ok_or_else(|| {
            Error::NotFound(format!(
                "no segment {} ({} segments)",
                si,
                self.segments.len()
            ))
        })
    }
}

/// v2: the bidx count totals against the `tx` and `hash` part sizes.
fn check_side_totals(s: &Segment, has_hash: bool) -> Result<()> {
    let (mut tx, mut hash) = (0usize, 0usize);
    for b in &s.blocks {
        if let Some(c) = b.counts {
            tx += c.n_trade;
            hash += c.n_hash;
        }
    }
    let tx_len = s.part("tx").map_or(0, |p| p.length);
    if tx as u64 * 32 != tx_len {
        return format_err(format!("bidx: {} trades, tx part {} bytes", tx, tx_len));
    }
    let hash_len = if has_hash {
        s.part("hash").map_or(0, |p| p.length)
    } else {
        0
    };
    if hash as u64 * 20 != hash_len {
        return format_err(format!(
            "bidx: {} hash items, hash part {} bytes",
            hash, hash_len
        ));
    }
    Ok(())
}
