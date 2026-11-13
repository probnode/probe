//! A reader for **Probe**, a lossless file format for prediction-market order books: one UTC hour
//! of every change of every price level of every outcome token, every trade, two clocks, and a
//! full-book checkpoint of each market every five minutes.
//!
//! The format is specified in `spec/PROBE.md` of the repository. This crate reads Probe v1
//! (header version `evmin/4`), v2 (`evmin/5`) and v3 (`evmin/6`) completely: the header, the
//! markets table, the segments and block indexes, every event and level in the reference order,
//! one market through the block index, and every side stream (trade transaction hashes, JSON
//! payloads, best-price exceptions, venue book hashes). [`File::open`] detects the version, and
//! every API below works the same on all three.
//!
//! Probe v3 is a container in which every part names its codec. [`v3::transcode`] turns any
//! v1, v2 or v3 file into a v3 file of a [`v3::Profile`] (`fast`, `fast1`, `rc1`), losslessly:
//! the result decodes to the same events, levels and side streams, byte for byte.
//!
//! ```no_run
//! use probe_format::File;
//!
//! # fn main() -> probe_format::Result<()> {
//! let f = File::open("hour.probe")?;
//! println!("{} with {} markets and {} segments", f.version(), f.markets().len(), f.segments().len());
//!
//! // Every event of the hour, in arrival order.
//! for ev in f.events() {
//!     let ev = ev?;
//!     for l in &ev.levels {
//!         let _ = (ev.kind, ev.recv_ns, l.side, l.price_micro, l.size_micro);
//!     }
//! }
//!
//! // One market, ten minutes, through the block index.
//! let from = f.header().hour_start_ns;
//! for ev in f.query().market("will-x-happen")?.from_ns(from).to_ns(from + 600_000_000_000).events() {
//!     let ev = ev?;
//!     if let Some(tx) = ev.tx_hash {
//!         let _ = tx;
//!     }
//! }
//!
//! // A token's book at a time.
//! if let Some(book) = f.book_at("7001", from + 1_000_000_000)? {
//!     println!("{} bids, {} asks", book.bids.len(), book.asks.len());
//! }
//! # Ok(())
//! # }
//! ```
//!
//! Errors, never panics, on corrupt or truncated input: every range, count and side-stream use is
//! checked.
//!
//! # Features
//!
//! - `parallel` (default): blocks of a segment decode on all cores (rayon).
//! - `arrow`: [`arrow`] record batches of events, levels, markets and `book-max` rows, and Arrow
//!   IPC output.
//! - `parquet`: parquet output of `book-max` rows.
//!
//! Transcoding to v3 is part of the default build.

#![warn(missing_docs)]

pub mod bookmax;
pub mod decoder;
pub mod dump;
mod error;
mod event;
mod file;
mod query;
mod rc;
pub mod v3;
mod walker;

#[cfg(feature = "arrow")]
pub mod arrow;

pub use crate::decoder::{core_dump, CoreDumpSummary};
pub use error::{Error, Result};
pub use event::{Event, EventType, Level, Side};
pub use file::{
    read_header, BlockCounts, BlockEntry, File, Header, Market, Part, Segment, Version, MAGIC,
    SUPPORTED_FLAGS,
};
pub use query::{Book, Events, Query};

/// Appends v as an unsigned LEB128 varint.
#[inline]
pub(crate) fn put_uvarint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// Reads an unsigned LEB128 varint: (value, bytes read). `None` when cut short or longer than 10
/// bytes.
#[inline]
pub fn uvarint(b: &[u8]) -> Option<(u64, usize)> {
    let mut x: u64 = 0;
    let mut s = 0u32;
    for (i, &c) in b.iter().enumerate() {
        if i == 10 {
            return None;
        }
        if c < 0x80 {
            if i == 9 && c > 1 {
                return None;
            }
            return Some((x | ((c as u64) << s), i + 1));
        }
        x |= ((c & 0x7f) as u64) << s;
        s += 7;
    }
    None
}
