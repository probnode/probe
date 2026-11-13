//! Selecting what to read: one market through the block index, a time range, with or without the
//! side streams; and the book of a token at a time (5.4).

use crate::decoder::floor_div;
use crate::error::{Error, Result};
use crate::event::{Event, EventType, SideStreams};
use crate::file::{File, Version};

/// A selection of events: all of the file, or one market, optionally limited to a receive-time
/// range. Build one with [`File::query`].
///
/// **Time range.** `[from, to)` in ns since the epoch, on the receive clock. A segment is read
/// when its window `[t0, t0 + 300 s)` starts before `to` and ends after `from`. Within a read
/// segment, every checkpoint is kept (it is the book at the segment start, so the output replays
/// alone), and a real event is kept when its receive time is in the range. Events are never
/// re-sorted: a late event stored in a later segment (specification 5.6) is seen with that
/// segment.
///
/// **One market.** The market's blocks are found through the block index and decoded alone. In
/// a v2 file their side streams come from the block's runs. A v1 file orders its side streams by
/// the whole segment, so with side streams on, each segment holding the market is decoded whole;
/// turn them off ([`Query::side_streams`]) to read the blocks alone.
#[derive(Clone, Debug)]
pub struct Query<'a> {
    file: &'a File,
    rows: Option<Vec<usize>>,
    from_ns: Option<i64>,
    to_ns: Option<i64>,
    side: bool,
    hashes: bool,
}

impl File {
    /// A query over the whole file, side streams on.
    pub fn query(&self) -> Query<'_> {
        Query {
            file: self,
            rows: None,
            from_ns: None,
            to_ns: None,
            side: true,
            hashes: true,
        }
    }

    /// Every event of the file in the reference order, segment by segment, with side streams.
    pub fn events(&self) -> Events<'_> {
        self.query().events()
    }

    /// The book of `token` at receive time `t_ns` (5.4): the latest segment with
    /// `t0_ns <= t_ns` whose checkpoint bitmap holds a row of the token; that one block is
    /// decoded, its checkpoints applied, then its events in stored order while their receive
    /// time is `<= t_ns`. `None` when no segment qualifies: the token has no book in this file.
    pub fn book_at(&self, token: &str, t_ns: i64) -> Result<Option<Book>> {
        let rows: Vec<(usize, u8)> = self
            .markets
            .iter()
            .enumerate()
            .filter_map(|(i, m)| {
                if m.tokens[0] == token {
                    Some((i, 0u8))
                } else if m.tokens[1] == token {
                    Some((i, 1u8))
                } else {
                    None
                }
            })
            .collect();
        for si in (0..self.segments.len()).rev() {
            let s = &self.segments[si];
            if s.t0_ns > t_ns {
                continue;
            }
            let Some(&(row, out)) = rows.iter().find(|&&(r, _)| s.has_row(r)) else {
                continue;
            };
            let bi = s.find_block(row).ok_or_else(|| {
                Error::Format(format!(
                    "segment {}: ckpt bit of row {} set but no block",
                    si, row
                ))
            })?;
            let (_, book) =
                self.decode_block_until(si, bi, floor_div(t_ns, self.header.unit_ns))?;
            let m = &self.markets[row];
            let conv = |v: &[(i64, i64)]| -> Vec<(i64, i64)> {
                v.iter()
                    .map(|&(p, z)| {
                        (
                            p.wrapping_mul(m.price_unit_micro),
                            z.wrapping_mul(m.size_unit_micro),
                        )
                    })
                    .collect()
            };
            return Ok(Some(Book {
                token: token.to_string(),
                segment: si,
                row,
                bids: conv(&book[out as usize][0]),
                asks: conv(&book[out as usize][1]),
            }));
        }
        Ok(None)
    }
}

/// A token's book at a time, from [`File::book_at`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Book {
    /// The token id.
    pub token: String,
    /// The segment whose block gave the book.
    pub segment: usize,
    /// The market row of that block.
    pub row: usize,
    /// Bid levels, (price micro, size micro), ascending price.
    pub bids: Vec<(i64, i64)>,
    /// Ask levels, (price micro, size micro), ascending price.
    pub asks: Vec<(i64, i64)>,
}

impl<'a> Query<'a> {
    /// Reads one market, named by slug, condition id or row number (all its rows).
    pub fn market(mut self, key: &str) -> Result<Self> {
        self.rows = Some(self.file.market_rows(key)?);
        Ok(self)
    }

    /// Reads one market row.
    pub fn row(mut self, row: usize) -> Result<Self> {
        if row >= self.file.markets.len() {
            return Err(Error::NotFound(format!(
                "no market row {} ({} rows)",
                row,
                self.file.markets.len()
            )));
        }
        self.rows = Some(vec![row]);
        Ok(self)
    }

    /// Keeps real events received at or after `from_ns`.
    pub fn from_ns(mut self, from_ns: i64) -> Self {
        self.from_ns = Some(from_ns);
        self
    }

    /// Keeps real events received before `to_ns`.
    pub fn to_ns(mut self, to_ns: i64) -> Self {
        self.to_ns = Some(to_ns);
        self
    }

    /// Attaches the side streams (default on): trade tx hashes, payloads, best-price exceptions
    /// and book hashes.
    pub fn side_streams(mut self, on: bool) -> Self {
        self.side = on;
        self
    }

    /// Attaches the venue book hashes when the file has them (default on). Off skips the `hash`
    /// part, the largest side stream.
    pub fn book_hashes(mut self, on: bool) -> Self {
        self.hashes = on;
        self
    }

    /// The market rows selected, if any.
    pub fn rows(&self) -> Option<&[usize]> {
        self.rows.as_deref()
    }

    /// The segments the query reads.
    pub fn segments(&self) -> Vec<usize> {
        let seg_ns = self.file.header.seg_s.saturating_mul(1_000_000_000);
        (0..self.file.segments.len())
            .filter(|&si| {
                let t0 = self.file.segments[si].t0_ns;
                self.to_ns.is_none_or(|to| t0 < to)
                    && self
                        .from_ns
                        .is_none_or(|from| t0.saturating_add(seg_ns) > from)
            })
            .collect()
    }

    fn keep(&self, e: &Event) -> bool {
        match e.recv_ns {
            None => true,
            Some(t) => self.from_ns.is_none_or(|f| t >= f) && self.to_ns.is_none_or(|to| t < to),
        }
    }

    /// The selected events of segment `si`, in stored order.
    pub fn segment(&self, si: usize) -> Result<Vec<Event>> {
        let f = self.file;
        let s = f.seg(si)?;
        let mut evs = match &self.rows {
            None => {
                if self.side {
                    f.segment_events_opts(si, self.hashes)?
                } else {
                    let core = f.segment_core(si)?;
                    core.order
                        .iter()
                        .enumerate()
                        .map(|(idx, &(bi, j))| {
                            let b = &core.blocks[bi as usize];
                            f.build_event(
                                si,
                                Some(idx),
                                j as usize,
                                b.row,
                                &b.events[j as usize],
                                b.event_levels(j as usize),
                            )
                        })
                        .collect()
                }
            }
            Some(rows) => {
                let held: Vec<usize> = rows.iter().copied().filter(|&r| s.has_row(r)).collect();
                if held.is_empty() {
                    Vec::new()
                } else if self.side && f.header.version == Version::V1 {
                    let all = f.segment_events_opts(si, self.hashes)?;
                    all.into_iter()
                        .filter(|e| e.kind != EventType::Other && held.contains(&e.row))
                        .collect()
                } else {
                    let sd = if self.side {
                        Some(SideStreams::read(f, si, self.hashes)?)
                    } else {
                        None
                    };
                    let mut out = Vec::new();
                    for r in held {
                        let bi = s.find_block(r).ok_or_else(|| {
                            Error::Format(format!(
                                "segment {}: ckpt bit of row {} set but no block",
                                si, r
                            ))
                        })?;
                        out.extend(f.block_events(si, bi, sd.as_ref())?);
                    }
                    out
                }
            }
        };
        if self.from_ns.is_some() || self.to_ns.is_some() {
            evs.retain(|e| self.keep(e));
        }
        Ok(evs)
    }

    /// Iterates the selected events, segment by segment.
    pub fn events(&self) -> Events<'a> {
        Events {
            q: self.clone(),
            segs: self.segments().into_iter(),
            buf: Vec::new().into_iter(),
            failed: false,
        }
    }
}

/// An iterator over a query's events. Each segment is decoded when the iterator reaches it; an
/// error ends the iteration.
pub struct Events<'a> {
    q: Query<'a>,
    segs: std::vec::IntoIter<usize>,
    buf: std::vec::IntoIter<Event>,
    failed: bool,
}

impl Iterator for Events<'_> {
    type Item = Result<Event>;

    fn next(&mut self) -> Option<Result<Event>> {
        loop {
            if let Some(e) = self.buf.next() {
                return Some(Ok(e));
            }
            if self.failed {
                return None;
            }
            let si = self.segs.next()?;
            match self.q.segment(si) {
                Ok(v) => self.buf = v.into_iter(),
                Err(e) => {
                    self.failed = true;
                    return Some(Err(e));
                }
            }
        }
    }
}
