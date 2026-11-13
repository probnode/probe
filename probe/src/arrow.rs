//! Arrow record batches (feature `arrow`): `book-max` rows, events, levels and the markets table;
//! Arrow IPC output; and, with feature `parquet`, parquet output.

use crate::bookmax::{BookMaxRow, VENUE};
use crate::dump::push_hex;
use crate::event::Event;
use crate::file::File;
use arrow_array::builder::{
    BooleanBuilder, Decimal128Builder, Int16Builder, Int32Builder, Int64Builder, Int8Builder,
    StringBuilder, UInt16Builder, UInt32Builder, UInt8Builder,
};
use arrow_array::{ArrayRef, RecordBatch};
use arrow_schema::{ArrowError, DataType, Field, Schema, SchemaRef};
use std::sync::Arc;

/// Rows per record batch written by the converters.
pub const BATCH_ROWS: usize = 1 << 16;

/// Prices and sizes: `decimal(18, 6)`, exact.
pub const DECIMAL: DataType = DataType::Decimal128(18, 6);

fn dec_builder(n: usize) -> Decimal128Builder {
    Decimal128Builder::with_capacity(n).with_data_type(DECIMAL)
}

/// The `book-max` schema: the common columns (`recv_ns`, `venue_ns`, `venue`, `token_id`, `seq`,
/// `qual`), then `op`, `side`, `price`, `size`, `level`, `event_type`.
pub fn book_max_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("recv_ns", DataType::Int64, false),
        Field::new("venue_ns", DataType::Int64, false),
        Field::new("venue", DataType::Utf8, false),
        Field::new("token_id", DataType::Utf8, false),
        Field::new("seq", DataType::Int64, false),
        Field::new("qual", DataType::UInt16, false),
        Field::new("op", DataType::Utf8, false),
        Field::new("side", DataType::Int8, false),
        Field::new("price", DECIMAL, false),
        Field::new("size", DECIMAL, false),
        Field::new("level", DataType::Int16, false),
        Field::new("event_type", DataType::Int8, false),
    ]))
}

/// `book-max` rows as one record batch of [`book_max_schema`].
pub fn book_max_batch(rows: &[BookMaxRow]) -> Result<RecordBatch, ArrowError> {
    let n = rows.len();
    let mut recv = Int64Builder::with_capacity(n);
    let mut venue_ns = Int64Builder::with_capacity(n);
    let mut venue = StringBuilder::with_capacity(n, n * VENUE.len());
    let mut tok = StringBuilder::with_capacity(n, n * 40);
    let mut seq = Int64Builder::with_capacity(n);
    let mut qual = UInt16Builder::with_capacity(n);
    let mut op = StringBuilder::with_capacity(n, n * 4);
    let mut side = Int8Builder::with_capacity(n);
    let mut px = dec_builder(n);
    let mut sz = dec_builder(n);
    let mut lv = Int16Builder::with_capacity(n);
    let mut et = Int8Builder::with_capacity(n);
    for r in rows {
        recv.append_value(r.recv_ns);
        venue_ns.append_value(r.venue_ns);
        venue.append_value(VENUE);
        tok.append_value(r.token_id);
        seq.append_value(r.seq);
        qual.append_value(r.qual);
        op.append_value(r.op.as_str());
        side.append_value(r.side);
        px.append_value(r.price_micro as i128);
        sz.append_value(r.size_micro as i128);
        lv.append_value(r.level);
        et.append_value(r.event_type);
    }
    let cols: Vec<ArrayRef> = vec![
        Arc::new(recv.finish()),
        Arc::new(venue_ns.finish()),
        Arc::new(venue.finish()),
        Arc::new(tok.finish()),
        Arc::new(seq.finish()),
        Arc::new(qual.finish()),
        Arc::new(op.finish()),
        Arc::new(side.finish()),
        Arc::new(px.finish()),
        Arc::new(sz.finish()),
        Arc::new(lv.finish()),
        Arc::new(et.finish()),
    ];
    RecordBatch::try_new(book_max_schema(), cols)
}

/// The events schema: one row per event.
pub fn events_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("segment", DataType::UInt32, false),
        Field::new("index", DataType::Int64, true),
        Field::new("event_type", DataType::Int8, false),
        Field::new("row", DataType::UInt32, false),
        Field::new("slug", DataType::Utf8, true),
        Field::new("token_id", DataType::Utf8, true),
        Field::new("outcome", DataType::UInt8, false),
        Field::new("cont", DataType::Boolean, false),
        Field::new("recv_ns", DataType::Int64, true),
        Field::new("venue_ns", DataType::Int64, true),
        Field::new("fee_rate_bps", DataType::Int32, true),
        Field::new("unsynced", DataType::Boolean, false),
        Field::new("n_levels", DataType::UInt32, false),
        Field::new("tx_hash", DataType::Utf8, true),
        Field::new("payload", DataType::Utf8, true),
        Field::new("book_hash", DataType::Utf8, true),
    ]))
}

fn hex0x(b: &[u8]) -> String {
    let mut s = String::with_capacity(2 + 2 * b.len());
    s.push_str("0x");
    push_hex(&mut s, b);
    s
}

fn hexs(b: &[u8]) -> String {
    let mut s = String::with_capacity(2 * b.len());
    push_hex(&mut s, b);
    s
}

/// Events as one record batch of [`events_schema`]. `token_id` is the event's outcome token
/// (types 0, 2, 3, 5); `tx_hash` is `0x` + hex.
pub fn events_batch(f: &File, evs: &[Event]) -> Result<RecordBatch, ArrowError> {
    let n = evs.len();
    let mut seg = UInt32Builder::with_capacity(n);
    let mut idx = Int64Builder::with_capacity(n);
    let mut et = Int8Builder::with_capacity(n);
    let mut row = UInt32Builder::with_capacity(n);
    let mut slug = StringBuilder::new();
    let mut tok = StringBuilder::new();
    let mut out = UInt8Builder::with_capacity(n);
    let mut cont = BooleanBuilder::with_capacity(n);
    let mut recv = Int64Builder::with_capacity(n);
    let mut venue = Int64Builder::with_capacity(n);
    let mut fee = Int32Builder::with_capacity(n);
    let mut uns = BooleanBuilder::with_capacity(n);
    let mut nlv = UInt32Builder::with_capacity(n);
    let mut tx = StringBuilder::new();
    let mut pay = StringBuilder::new();
    let mut bh = StringBuilder::new();
    for e in evs {
        seg.append_value(e.segment as u32);
        idx.append_option(e.index.map(|i| i as i64));
        et.append_value(e.kind.as_u8() as i8);
        row.append_value(e.row as u32);
        let m = f.row_market(e.segment, e.row);
        if m.slug.is_empty() {
            slug.append_null();
        } else {
            slug.append_value(&m.slug);
        }
        let has_out = matches!(e.kind.as_u8(), 0 | 2 | 3 | 5);
        let t = f.token(e.segment, e.row, e.outcome);
        if has_out && !t.is_empty() {
            tok.append_value(t);
        } else {
            tok.append_null();
        }
        out.append_value(e.outcome);
        cont.append_value(e.cont);
        recv.append_option(e.recv_ns);
        venue.append_option(e.venue_ms.map(|v| v.wrapping_mul(1_000_000)));
        if e.kind == crate::EventType::Trade {
            fee.append_value(e.fee_rate_bps as i32);
        } else {
            fee.append_null();
        }
        uns.append_value(e.unsynced);
        nlv.append_value(e.levels.len() as u32);
        tx.append_option(e.tx_hash.as_ref().map(|h| hex0x(h)));
        pay.append_option(e.payload.as_deref());
        bh.append_option(e.book_hash.as_ref().map(|h| hexs(h)));
    }
    let cols: Vec<ArrayRef> = vec![
        Arc::new(seg.finish()),
        Arc::new(idx.finish()),
        Arc::new(et.finish()),
        Arc::new(row.finish()),
        Arc::new(slug.finish()),
        Arc::new(tok.finish()),
        Arc::new(out.finish()),
        Arc::new(cont.finish()),
        Arc::new(recv.finish()),
        Arc::new(venue.finish()),
        Arc::new(fee.finish()),
        Arc::new(uns.finish()),
        Arc::new(nlv.finish()),
        Arc::new(tx.finish()),
        Arc::new(pay.finish()),
        Arc::new(bh.finish()),
    ];
    RecordBatch::try_new(events_schema(), cols)
}

/// The levels schema: one row per level, with its event's columns.
pub fn levels_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("segment", DataType::UInt32, false),
        Field::new("index", DataType::Int64, true),
        Field::new("level_index", DataType::UInt32, false),
        Field::new("event_type", DataType::Int8, false),
        Field::new("row", DataType::UInt32, false),
        Field::new("token_id", DataType::Utf8, false),
        Field::new("side", DataType::Int8, false),
        Field::new("price", DECIMAL, false),
        Field::new("size", DECIMAL, false),
        Field::new("mirror", DataType::Boolean, false),
        Field::new("recv_ns", DataType::Int64, true),
        Field::new("venue_ns", DataType::Int64, true),
        Field::new("best_bid", DataType::Utf8, true),
        Field::new("best_ask", DataType::Utf8, true),
        Field::new("book_hash", DataType::Utf8, true),
    ]))
}

/// The levels of events as one record batch of [`levels_schema`].
pub fn levels_batch(f: &File, evs: &[Event]) -> Result<RecordBatch, ArrowError> {
    let n: usize = evs.iter().map(|e| e.levels.len()).sum();
    let mut seg = UInt32Builder::with_capacity(n);
    let mut idx = Int64Builder::with_capacity(n);
    let mut li = UInt32Builder::with_capacity(n);
    let mut et = Int8Builder::with_capacity(n);
    let mut row = UInt32Builder::with_capacity(n);
    let mut tok = StringBuilder::with_capacity(n, n * 40);
    let mut side = Int8Builder::with_capacity(n);
    let mut px = dec_builder(n);
    let mut sz = dec_builder(n);
    let mut mir = BooleanBuilder::with_capacity(n);
    let mut recv = Int64Builder::with_capacity(n);
    let mut venue = Int64Builder::with_capacity(n);
    let mut bb = StringBuilder::new();
    let mut ba = StringBuilder::new();
    let mut bh = StringBuilder::new();
    for e in evs {
        for (k, l) in e.levels.iter().enumerate() {
            seg.append_value(e.segment as u32);
            idx.append_option(e.index.map(|i| i as i64));
            li.append_value(k as u32);
            et.append_value(e.kind.as_u8() as i8);
            row.append_value(e.row as u32);
            tok.append_value(f.token(e.segment, e.row, l.outcome));
            side.append_value(l.side.as_u8() as i8);
            px.append_value(l.price_micro as i128);
            sz.append_value(l.size_micro as i128);
            mir.append_value(l.mirror);
            recv.append_option(e.recv_ns);
            venue.append_option(e.venue_ms.map(|v| v.wrapping_mul(1_000_000)));
            match &l.best {
                Some((b, a)) => {
                    bb.append_value(b);
                    ba.append_value(a);
                }
                None => {
                    bb.append_null();
                    ba.append_null();
                }
            }
            bh.append_option(l.book_hash.as_ref().map(|h| hexs(h)));
        }
    }
    let cols: Vec<ArrayRef> = vec![
        Arc::new(seg.finish()),
        Arc::new(idx.finish()),
        Arc::new(li.finish()),
        Arc::new(et.finish()),
        Arc::new(row.finish()),
        Arc::new(tok.finish()),
        Arc::new(side.finish()),
        Arc::new(px.finish()),
        Arc::new(sz.finish()),
        Arc::new(mir.finish()),
        Arc::new(recv.finish()),
        Arc::new(venue.finish()),
        Arc::new(bb.finish()),
        Arc::new(ba.finish()),
        Arc::new(bh.finish()),
    ];
    RecordBatch::try_new(levels_schema(), cols)
}

/// The markets table as one record batch: `row`, `slug`, `condition_id`, `token_0`, `token_1`,
/// `price_unit_micro`, `size_unit_micro`, `trade_size_unit_micro`, `U`.
pub fn markets_batch(f: &File) -> Result<RecordBatch, ArrowError> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("row", DataType::UInt32, false),
        Field::new("slug", DataType::Utf8, false),
        Field::new("condition_id", DataType::Utf8, false),
        Field::new("token_0", DataType::Utf8, false),
        Field::new("token_1", DataType::Utf8, false),
        Field::new("price_unit_micro", DataType::Int64, false),
        Field::new("size_unit_micro", DataType::Int64, false),
        Field::new("trade_size_unit_micro", DataType::Int64, false),
        Field::new("U", DataType::Int64, false),
    ]));
    let ms = f.markets();
    let mut row = UInt32Builder::new();
    let mut slug = StringBuilder::new();
    let mut cid = StringBuilder::new();
    let mut t0 = StringBuilder::new();
    let mut t1 = StringBuilder::new();
    let mut pu = Int64Builder::new();
    let mut su = Int64Builder::new();
    let mut tu = Int64Builder::new();
    let mut u = Int64Builder::new();
    for (i, m) in ms.iter().enumerate() {
        row.append_value(i as u32);
        slug.append_value(&m.slug);
        cid.append_value(&m.condition_id);
        t0.append_value(&m.tokens[0]);
        t1.append_value(&m.tokens[1]);
        pu.append_value(m.price_unit_micro);
        su.append_value(m.size_unit_micro);
        tu.append_value(m.trade_size_unit_micro);
        u.append_value(m.u);
    }
    let cols: Vec<ArrayRef> = vec![
        Arc::new(row.finish()),
        Arc::new(slug.finish()),
        Arc::new(cid.finish()),
        Arc::new(t0.finish()),
        Arc::new(t1.finish()),
        Arc::new(pu.finish()),
        Arc::new(su.finish()),
        Arc::new(tu.finish()),
        Arc::new(u.finish()),
    ];
    RecordBatch::try_new(schema, cols)
}

/// Batches as an Arrow IPC stream (what `pyarrow.ipc.open_stream` reads).
pub fn ipc_stream_bytes(
    schema: &SchemaRef,
    batches: &[RecordBatch],
) -> Result<Vec<u8>, ArrowError> {
    let mut buf = Vec::new();
    {
        let mut w = arrow_ipc::writer::StreamWriter::try_new(&mut buf, schema)?;
        for b in batches {
            w.write(b)?;
        }
        w.finish()?;
    }
    Ok(buf)
}

/// A sink for record batches of one schema: an Arrow IPC file or a parquet file.
#[allow(clippy::large_enum_variant)]
pub enum BatchWriter<W: std::io::Write + Send> {
    /// An Arrow IPC file (`.arrow`, Feather v2).
    Ipc(arrow_ipc::writer::FileWriter<W>),
    /// A parquet file.
    #[cfg(feature = "parquet")]
    Parquet(parquet::arrow::ArrowWriter<W>),
}

impl<W: std::io::Write + Send> BatchWriter<W> {
    /// An Arrow IPC file writer.
    pub fn ipc(w: W, schema: &SchemaRef) -> Result<Self, ArrowError> {
        Ok(BatchWriter::Ipc(arrow_ipc::writer::FileWriter::try_new(
            w, schema,
        )?))
    }

    /// A parquet writer, zstd at `zstd_level` (1..22).
    #[cfg(feature = "parquet")]
    pub fn parquet(w: W, schema: &SchemaRef, zstd_level: i32) -> Result<Self, ArrowError> {
        use parquet::basic::{Compression, ZstdLevel};
        use parquet::file::properties::WriterProperties;
        let lvl = ZstdLevel::try_new(zstd_level)
            .map_err(|e| ArrowError::InvalidArgumentError(e.to_string()))?;
        let props = WriterProperties::builder()
            .set_compression(Compression::ZSTD(lvl))
            .set_max_row_group_size(BATCH_ROWS * 16)
            .build();
        let pw = parquet::arrow::ArrowWriter::try_new(w, schema.clone(), Some(props))
            .map_err(|e| ArrowError::ExternalError(Box::new(e)))?;
        Ok(BatchWriter::Parquet(pw))
    }

    /// Writes one batch.
    pub fn write(&mut self, b: &RecordBatch) -> Result<(), ArrowError> {
        match self {
            BatchWriter::Ipc(w) => w.write(b),
            #[cfg(feature = "parquet")]
            BatchWriter::Parquet(w) => w
                .write(b)
                .map_err(|e| ArrowError::ExternalError(Box::new(e))),
        }
    }

    /// Finishes the file (footer, metadata).
    pub fn finish(self) -> Result<(), ArrowError> {
        match self {
            BatchWriter::Ipc(mut w) => w.finish(),
            #[cfg(feature = "parquet")]
            BatchWriter::Parquet(w) => w
                .close()
                .map(|_| ())
                .map_err(|e| ArrowError::ExternalError(Box::new(e))),
        }
    }
}
