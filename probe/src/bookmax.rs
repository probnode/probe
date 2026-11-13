//! `book-max` rows: every level change, checkpoint level and trade as one row, with the replay's
//! columns (`seq`, `qual`, `op`, `level`). This is the per-level table of the specification's
//! companion schema, the unit of the parquet / Arrow conversion.

use crate::decoder::floor_div;
use crate::event::{Event, EventType, Side};
use crate::file::File;
use std::collections::HashMap;

/// `qual` bit 0: the last row of one event (one venue message).
pub const QUAL_END: u16 = 1 << 0;
/// `qual` bit 1: a row of a full-book checkpoint.
pub const QUAL_CKPT: u16 = 1 << 1;
/// `qual` bit 2: a mirrored price_change entry.
pub const QUAL_MIRROR: u16 = 1 << 2;
/// `qual` bit 3: the token's book is not synced yet (an unsynced checkpoint, until the token's
/// next venue book).
pub const QUAL_UNSYNCED: u16 = 1 << 3;

/// The `op` of a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    /// A checkpoint level: the rows of one checkpoint replace the token's book.
    Snap,
    /// A level now has this size; size 0 removes it.
    Set,
    /// A trade. It does not change the book.
    Trade,
}

impl Op {
    /// `snap`, `set` or `trade`.
    pub fn as_str(self) -> &'static str {
        match self {
            Op::Snap => "snap",
            Op::Set => "set",
            Op::Trade => "trade",
        }
    }
}

/// One `book-max` row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BookMaxRow<'a> {
    /// Receive time, ns since the epoch (1 µs resolution). A checkpoint row has its segment's
    /// start.
    pub recv_ns: i64,
    /// The venue's time, ns since the epoch (ms resolution). A checkpoint row has its segment's
    /// start.
    pub venue_ns: i64,
    /// The outcome token.
    pub token_id: &'a str,
    /// The row's position in the output, from 0: apply rows in `seq` order.
    pub seq: i64,
    /// Quality bits (`QUAL_*`).
    pub qual: u16,
    /// What the row does.
    pub op: Op,
    /// 0 bid, 1 ask (for a trade: the aggressor's side, the venue's).
    pub side: i8,
    /// Price in micro.
    pub price_micro: i64,
    /// Size in micro shares.
    pub size_micro: i64,
    /// The level's position on its side after the change, 0 = best. For a trade, the position of
    /// its price on the side it trades against.
    pub level: i16,
    /// The event type: 0 book diff, 1 price_change, 2 trade, 5 checkpoint.
    pub event_type: i8,
}

/// The column names of `book-max`, in order.
pub const BOOK_MAX_COLUMNS: [&str; 12] = [
    "recv_ns",
    "venue_ns",
    "venue",
    "token_id",
    "seq",
    "qual",
    "op",
    "side",
    "price",
    "size",
    "level",
    "event_type",
];

/// The `venue` column's value.
pub const VENUE: &str = "polymarket";

#[derive(Default)]
struct SideBook {
    px: Vec<i64>,
    sz: Vec<i64>,
    ask: bool,
}

impl SideBook {
    /// The levels strictly better than p, and whether the next one is p.
    fn find(&self, p: i64) -> (usize, bool) {
        let i = if self.ask {
            self.px.partition_point(|&x| x < p)
        } else {
            self.px.partition_point(|&x| x > p)
        };
        (i, i < self.px.len() && self.px[i] == p)
    }

    fn set(&mut self, p: i64, z: i64) -> usize {
        let (i, ok) = self.find(p);
        if ok && z == 0 {
            self.px.remove(i);
            self.sz.remove(i);
        } else if ok {
            self.sz[i] = z;
        } else if z != 0 {
            self.px.insert(i, p);
            self.sz.insert(i, z);
        }
        i
    }

    fn reset(&mut self) {
        self.px.clear();
        self.sz.clear();
    }
}

fn new_book() -> [SideBook; 2] {
    [
        SideBook::default(),
        SideBook {
            ask: true,
            ..Default::default()
        },
    ]
}

fn clamp_level(i: usize) -> i16 {
    i.min(i16::MAX as usize) as i16
}

/// Replays events into `book-max` rows. Feed it events in stored order, one market or a whole
/// file; it keeps every token's book.
pub struct BookMax<'a> {
    file: &'a File,
    books: HashMap<&'a str, [SideBook; 2]>,
    unsynced: HashMap<&'a str, bool>,
    seq: i64,
}

impl<'a> BookMax<'a> {
    /// A replay over events of `file`.
    pub fn new(file: &'a File) -> Self {
        BookMax {
            file,
            books: HashMap::new(),
            unsynced: HashMap::new(),
            seq: 0,
        }
    }

    /// Rows emitted so far.
    pub fn rows(&self) -> i64 {
        self.seq
    }

    /// Applies one event and appends its rows to `out` (none for types 3 and 4).
    pub fn event(&mut self, ev: &Event, out: &mut Vec<BookMaxRow<'a>>) {
        let f = self.file;
        let si = ev.segment;
        let t0 = f.segments.get(si).map_or(0, |s| s.t0_ns);
        let (recv_ns, venue_ns) = match (ev.recv_ns, ev.venue_ms) {
            (Some(r), Some(v)) => (r, v.wrapping_mul(1_000_000)),
            _ => (
                floor_div(t0, 1000).wrapping_mul(1000),
                floor_div(t0, 1_000_000).wrapping_mul(1_000_000),
            ),
        };
        let et = ev.kind.as_u8() as i8;
        let emit = |seq: &mut i64,
                    out: &mut Vec<BookMaxRow<'a>>,
                    tok: &'a str,
                    op: Op,
                    side: Side,
                    p: i64,
                    z: i64,
                    lv: usize,
                    qual: u16| {
            out.push(BookMaxRow {
                recv_ns,
                venue_ns,
                token_id: tok,
                seq: *seq,
                qual,
                op,
                side: side.as_u8() as i8,
                price_micro: p,
                size_micro: z,
                level: clamp_level(lv),
                event_type: et,
            });
            *seq += 1;
        };
        let n = ev.levels.len();
        match ev.kind {
            EventType::Checkpoint => {
                let tok = f.token(si, ev.row, ev.outcome);
                let b = self.books.entry(tok).or_insert_with(new_book);
                b[0].reset();
                b[1].reset();
                for l in &ev.levels {
                    if l.size_micro != 0 {
                        b[l.side.as_u8() as usize].set(l.price_micro, l.size_micro);
                    }
                }
                self.unsynced.insert(tok, ev.unsynced);
                let q = QUAL_CKPT | if ev.unsynced { QUAL_UNSYNCED } else { 0 };
                if n == 0 {
                    // An empty checkpoint is one row (bid, price 0, size 0), so that every
                    // checkpoint reaches the reader and replaces the book it holds.
                    let lv = b[0].find(0).0;
                    emit(
                        &mut self.seq,
                        out,
                        tok,
                        Op::Snap,
                        Side::Bid,
                        0,
                        0,
                        lv,
                        q | QUAL_END,
                    );
                }
                for (i, l) in ev.levels.iter().enumerate() {
                    let tok_l = f.token(si, ev.row, l.outcome);
                    let lv = b[l.side.as_u8() as usize].find(l.price_micro).0;
                    emit(
                        &mut self.seq,
                        out,
                        tok_l,
                        Op::Snap,
                        l.side,
                        l.price_micro,
                        l.size_micro,
                        lv,
                        q | if i + 1 == n { QUAL_END } else { 0 },
                    );
                }
            }
            EventType::Book | EventType::PriceChange => {
                for (i, l) in ev.levels.iter().enumerate() {
                    let tok = f.token(si, ev.row, l.outcome);
                    let lv = self.books.entry(tok).or_insert_with(new_book)
                        [l.side.as_u8() as usize]
                        .set(l.price_micro, l.size_micro);
                    let mut q = if l.mirror { QUAL_MIRROR } else { 0 };
                    // A venue book (type 0) is the token's full snapshot: it syncs the book, its own
                    // rows included.
                    if ev.kind == EventType::PriceChange
                        && self.unsynced.get(tok).copied().unwrap_or(false)
                    {
                        q |= QUAL_UNSYNCED;
                    }
                    if i + 1 == n {
                        q |= QUAL_END;
                    }
                    emit(
                        &mut self.seq,
                        out,
                        tok,
                        Op::Set,
                        l.side,
                        l.price_micro,
                        l.size_micro,
                        lv,
                        q,
                    );
                }
                if ev.kind == EventType::Book {
                    for l in &ev.levels {
                        self.unsynced.remove(f.token(si, ev.row, l.outcome));
                    }
                }
            }
            EventType::Trade => {
                for (i, l) in ev.levels.iter().enumerate() {
                    let tok = f.token(si, ev.row, l.outcome);
                    let b = self.books.entry(tok).or_insert_with(new_book);
                    // The aggressor buys from the asks (side 0) or sells into the bids (side 1).
                    let lv = b[1 - l.side.as_u8() as usize].find(l.price_micro).0;
                    let mut q = if self.unsynced.get(tok).copied().unwrap_or(false) {
                        QUAL_UNSYNCED
                    } else {
                        0
                    };
                    if i + 1 == n {
                        q |= QUAL_END;
                    }
                    emit(
                        &mut self.seq,
                        out,
                        tok,
                        Op::Trade,
                        l.side,
                        l.price_micro,
                        l.size_micro,
                        lv,
                        q,
                    );
                }
            }
            EventType::TickSize | EventType::Other => {}
        }
    }
}

/// Appends `v / 1e6` with exactly six decimals.
pub fn push_micro(out: &mut String, v: i64) {
    let mut u = v as i128;
    if u < 0 {
        out.push('-');
        u = -u;
    }
    out.push_str(&format!("{}.{:06}", u / 1_000_000, u % 1_000_000));
}

/// One `book-max` row as CSV (the columns of [`BOOK_MAX_COLUMNS`]), without a line end.
pub fn push_book_max_csv(out: &mut String, r: &BookMaxRow) {
    out.push_str(&format!("{},{},{},", r.recv_ns, r.venue_ns, VENUE));
    crate::dump::push_csv(out, r.token_id);
    out.push_str(&format!(
        ",{},{},{},{},",
        r.seq,
        r.qual,
        r.op.as_str(),
        r.side
    ));
    push_micro(out, r.price_micro);
    out.push(',');
    push_micro(out, r.size_micro);
    out.push_str(&format!(",{},{}", r.level, r.event_type));
}
