//! `probe`: inspect, print, convert and verify Probe files.

use clap::{Parser, Subcommand, ValueEnum};
use probe_format::arrow::{book_max_batch, book_max_schema, BatchWriter, BATCH_ROWS};
use probe_format::bookmax::{push_micro, BookMax, BookMaxRow, BOOK_MAX_COLUMNS};
use probe_format::dump::{push_csv, push_hex, write_dump};
use probe_format::{core_dump, Event, EventType, File, Query, Result as PResult};
use serde_json::json;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

mod time;

#[derive(Parser)]
#[command(name = "probe", version, about = "Inspect, print, convert and verify Probe order-book files", long_about = None)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, ValueEnum)]
enum What {
    /// One row per event.
    Events,
    /// One row per level, with its event's columns.
    Levels,
    /// One row per level change, checkpoint level and trade, with the replay's columns.
    BookMax,
}

#[derive(Clone, Copy, ValueEnum)]
enum TextFormat {
    Csv,
    Ndjson,
}

#[derive(Clone, Copy, ValueEnum)]
enum OutFormat {
    Parquet,
    Arrow,
}

#[derive(clap::Args)]
struct Select {
    /// One market: slug, condition id or row number.
    #[arg(long)]
    market: Option<String>,
    /// Real events received at or after this time: ns since the epoch, or RFC 3339
    /// (2026-11-12T10:05:00Z).
    #[arg(long)]
    from: Option<String>,
    /// Real events received before this time.
    #[arg(long)]
    to: Option<String>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Header, version, markets table summary, segments and block indexes.
    Info {
        file: PathBuf,
        /// Print JSON.
        #[arg(long)]
        json: bool,
        /// List every market row.
        #[arg(long)]
        markets: bool,
    },
    /// Print events, levels or book-max rows as CSV or NDJSON.
    Cat {
        file: PathBuf,
        #[arg(long, value_enum, default_value = "events")]
        what: What,
        #[arg(long, value_enum, default_value = "csv")]
        format: TextFormat,
        #[command(flatten)]
        select: Select,
        /// Stop after this many rows.
        #[arg(long)]
        limit: Option<u64>,
        /// Leave out the CSV header.
        #[arg(long)]
        no_header: bool,
        /// Output file (default: stdout).
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Convert to the per-level book-max table, as parquet or an Arrow IPC file.
    Convert {
        file: PathBuf,
        /// Output path; .parquet or .arrow / .feather / .ipc chooses the format.
        #[arg(short, long)]
        output: PathBuf,
        #[arg(long, value_enum)]
        format: Option<OutFormat>,
        #[command(flatten)]
        select: Select,
        /// Parquet zstd level.
        #[arg(long, default_value_t = 3)]
        zstd_level: i32,
    },
    /// Decode everything with every check: structure, counts, and every side stream used exactly.
    Verify {
        file: PathBuf,
        /// Skip the optional venue book-hash part.
        #[arg(long)]
        no_hash: bool,
    },
    /// The conformance dump: the core dump (default) or, with --full, every field and side stream
    /// in the layout of the conformance vectors' events.csv.
    Dump {
        file: PathBuf,
        #[arg(short, long)]
        output: Option<PathBuf>,
        #[arg(long)]
        full: bool,
        /// Decode the order stream with the literal move-to-front list of the specification.
        #[arg(long)]
        literal_order: bool,
    },
    /// A token's book at a time, from one block.
    Book {
        file: PathBuf,
        #[arg(long)]
        token: String,
        /// Receive time: ns since the epoch or RFC 3339.
        #[arg(long)]
        at: String,
        #[arg(long)]
        json: bool,
    },
}

type AnyResult<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("probe: {}", e);
            ExitCode::from(2)
        }
    }
}

fn open(p: &Path) -> AnyResult<File> {
    Ok(File::open(p)?)
}

fn output(p: &Option<PathBuf>) -> AnyResult<Box<dyn Write>> {
    Ok(match p {
        Some(p) => Box::new(BufWriter::with_capacity(1 << 20, std::fs::File::create(p)?)),
        None => Box::new(BufWriter::with_capacity(1 << 20, std::io::stdout().lock())),
    })
}

fn query<'a>(f: &'a File, s: &Select) -> AnyResult<Query<'a>> {
    let mut q = f.query();
    if let Some(m) = &s.market {
        q = q.market(m)?;
    }
    if let Some(t) = &s.from {
        q = q.from_ns(time::parse(t)?);
    }
    if let Some(t) = &s.to {
        q = q.to_ns(time::parse(t)?);
    }
    Ok(q)
}

fn run(cli: Cli) -> AnyResult<ExitCode> {
    match cli.cmd {
        Cmd::Info {
            file,
            json,
            markets,
        } => info(&file, json, markets)?,
        Cmd::Cat {
            file,
            what,
            format,
            select,
            limit,
            no_header,
            output: o,
        } => {
            let f = open(&file)?;
            let q = query(&f, &select)?;
            let mut w = output(&o)?;
            cat(&f, &q, what, format, limit, !no_header, &mut *w)?;
            w.flush()?;
        }
        Cmd::Convert {
            file,
            output: o,
            format,
            select,
            zstd_level,
        } => {
            let f = open(&file)?;
            let q = query(&f, &select)?;
            let fmt = match format {
                Some(x) => x,
                None => match o.extension().and_then(|e| e.to_str()).unwrap_or("") {
                    "parquet" | "pq" => OutFormat::Parquet,
                    "arrow" | "feather" | "ipc" => OutFormat::Arrow,
                    e => {
                        return Err(format!(
                            "cannot tell the format from extension {:?}: use --format",
                            e
                        )
                        .into())
                    }
                },
            };
            let n = convert(&f, &q, &o, fmt, zstd_level)?;
            eprintln!("{} book-max rows -> {}", n, o.display());
        }
        Cmd::Verify { file, no_hash } => return verify(&file, no_hash),
        Cmd::Dump {
            file,
            output: o,
            full,
            literal_order,
        } => {
            let mut f = open(&file)?;
            f.set_order_fast(!literal_order);
            let t = std::time::Instant::now();
            let mut w = output(&o)?;
            if full {
                write_dump(&f, &mut *w)?;
                w.flush()?;
            } else {
                let s = core_dump(&f, Some(&mut *w))?;
                w.flush()?;
                let summary = json!({"events": s.events, "levels": s.levels, "fnv64": format!("{:016x}", s.fnv64), "file": file.display().to_string(), "seconds": t.elapsed().as_secs_f64()});
                if o.is_some() {
                    println!("{}", summary);
                } else {
                    eprintln!("{}", summary);
                }
            }
        }
        Cmd::Book {
            file,
            token,
            at,
            json,
        } => {
            let f = open(&file)?;
            let t = time::parse(&at)?;
            match f.book_at(&token, t)? {
                None => {
                    if json {
                        println!(
                            "{}",
                            json!({"token": token, "ts_ns": t, "available": false})
                        );
                    } else {
                        println!("{},{},NONE,,", token, t);
                    }
                }
                Some(b) => {
                    if json {
                        println!(
                            "{}",
                            json!({"token": token, "ts_ns": t, "available": true, "segment": b.segment, "row": b.row, "bids": b.bids, "asks": b.asks})
                        );
                    } else {
                        for (p, z) in &b.bids {
                            println!("{},{},BUY,{},{}", token, t, p, z);
                        }
                        for (p, z) in &b.asks {
                            println!("{},{},SELL,{},{}", token, t, p, z);
                        }
                    }
                }
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn info(path: &Path, as_json: bool, list_markets: bool) -> AnyResult<()> {
    let f = open(path)?;
    let h = f.header();
    let segs: Vec<_> = f
        .segments()
        .iter()
        .map(|s| {
            json!({
                "t0_ns": s.t0_ns, "t0": time::format(s.t0_ns), "nmkt": s.nmkt, "blocks": s.blocks.len(), "n_ev": s.n_ev, "n_lv": s.n_lv,
                "n_ck": s.n_ck, "unsynced": s.unsynced.len(), "has_other_block": s.has_other_block(),
                "parts": s.parts.iter().map(|p| json!([p.name, p.offset, p.length])).collect::<Vec<_>>(),
            })
        })
        .collect();
    let markets: Vec<_> = f
        .markets()
        .iter()
        .enumerate()
        .map(|(i, m)| {
            json!({"row": i, "slug": m.slug, "condition_id": m.condition_id, "tokens": m.tokens, "price_unit_micro": m.price_unit_micro,
                   "size_unit_micro": m.size_unit_micro, "trade_size_unit_micro": m.trade_size_unit_micro, "U": m.u})
        })
        .collect();
    if as_json {
        let mut v = json!({
            "file": path.display().to_string(), "bytes": f.size(), "version": format!("{}", f.version()), "header_version": h.version.header_str(),
            "recorder": h.recorder, "unit_ns": h.unit_ns, "seg_s": h.seg_s, "shift": h.shift, "flags": h.flags, "has_hash": h.has_hash,
            "hour_start_ns": h.hour_start_ns, "hour_start": time::format(h.hour_start_ns), "markets": f.markets().len(), "segments": segs,
        });
        if list_markets {
            v["market_rows"] = json!(markets);
        }
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    println!("{}", path.display());
    println!("  bytes      {}", f.size());
    println!(
        "  version    {} ({}), recorder {:?}",
        f.version(),
        h.version.header_str(),
        h.recorder
    );
    println!(
        "  coding     family rc, layout mkt, shift {}, flags {}, has_hash {}",
        h.shift, h.flags, h.has_hash
    );
    println!(
        "  hour       {} (unit_ns {}, seg_s {})",
        time::format(h.hour_start_ns),
        h.unit_ns,
        h.seg_s
    );
    println!("  markets    {} rows", f.markets().len());
    if list_markets {
        for (i, m) in f.markets().iter().enumerate() {
            println!(
                "    {:>6} {:<32} {} tokens {} / {}  price_unit {} size_unit {} trade_size_unit {} U {}",
                i, m.slug, m.condition_id, m.tokens[0], m.tokens[1], m.price_unit_micro, m.size_unit_micro, m.trade_size_unit_micro, m.u
            );
        }
    }
    println!("  segments   {}", f.segments().len());
    for (i, s) in f.segments().iter().enumerate() {
        let parts: Vec<String> = s
            .parts
            .iter()
            .map(|p| format!("{} {}", p.name, p.length))
            .collect();
        println!(
            "    {:>2} {}  nmkt {}  blocks {}  n_ev {}  n_lv {}  n_ck {}  unsynced {}  [{}]",
            i,
            time::format(s.t0_ns),
            s.nmkt,
            s.blocks.len(),
            s.n_ev,
            s.n_lv,
            s.n_ck,
            s.unsynced.len(),
            parts.join(", ")
        );
    }
    Ok(())
}

fn opt<T: std::fmt::Display>(v: Option<T>) -> String {
    v.map(|x| x.to_string()).unwrap_or_default()
}

fn hex(b: &[u8], prefix: bool) -> String {
    let mut s = String::new();
    if prefix {
        s.push_str("0x");
    }
    push_hex(&mut s, b);
    s
}

fn micro(v: i64) -> String {
    let mut s = String::new();
    push_micro(&mut s, v);
    s
}

const EVENT_COLS: [&str; 17] = [
    "segment",
    "index",
    "event_type",
    "event",
    "row",
    "slug",
    "token_id",
    "outcome",
    "cont",
    "recv_ns",
    "venue_ns",
    "fee_rate_bps",
    "unsynced",
    "n_levels",
    "tx_hash",
    "payload",
    "book_hash",
];

const LEVEL_COLS: [&str; 16] = [
    "segment",
    "index",
    "level_index",
    "event_type",
    "row",
    "token_id",
    "side",
    "price",
    "size",
    "mirror",
    "recv_ns",
    "venue_ns",
    "best_bid",
    "best_ask",
    "book_hash",
    "outcome",
];

fn event_fields(f: &File, e: &Event) -> Vec<(&'static str, serde_json::Value, String)> {
    let m = f.market(e.row).filter(|_| e.kind != EventType::Other);
    let has_out = matches!(e.kind.as_u8(), 0 | 2 | 3 | 5);
    let tok = if has_out {
        f.token(e.segment, e.row, e.outcome)
    } else {
        ""
    };
    let venue_ns = e.venue_ms.map(|v| v.wrapping_mul(1_000_000));
    let fee = if e.kind == EventType::Trade {
        Some(e.fee_rate_bps)
    } else {
        None
    };
    let tx = e.tx_hash.map(|h| hex(&h, true));
    let bh = e.book_hash.map(|h| hex(&h, false));
    vec![
        ("segment", json!(e.segment), e.segment.to_string()),
        ("index", json!(e.index), opt(e.index)),
        (
            "event_type",
            json!(e.kind.as_u8()),
            e.kind.as_u8().to_string(),
        ),
        ("event", json!(e.kind.name()), e.kind.name().to_string()),
        ("row", json!(e.row), e.row.to_string()),
        (
            "slug",
            json!(m.map(|m| m.slug.as_str())),
            m.map(|m| m.slug.clone()).unwrap_or_default(),
        ),
        (
            "token_id",
            if tok.is_empty() {
                json!(null)
            } else {
                json!(tok)
            },
            tok.to_string(),
        ),
        ("outcome", json!(e.outcome), e.outcome.to_string()),
        ("cont", json!(e.cont), (e.cont as u8).to_string()),
        ("recv_ns", json!(e.recv_ns), opt(e.recv_ns)),
        ("venue_ns", json!(venue_ns), opt(venue_ns)),
        ("fee_rate_bps", json!(fee), opt(fee)),
        (
            "unsynced",
            json!(e.unsynced),
            (e.unsynced as u8).to_string(),
        ),
        (
            "n_levels",
            json!(e.levels.len()),
            e.levels.len().to_string(),
        ),
        ("tx_hash", json!(tx), tx.clone().unwrap_or_default()),
        (
            "payload",
            json!(e.payload),
            e.payload.clone().unwrap_or_default(),
        ),
        ("book_hash", json!(bh), bh.clone().unwrap_or_default()),
    ]
}

fn level_fields(f: &File, e: &Event, k: usize) -> Vec<(&'static str, serde_json::Value, String)> {
    let l = &e.levels[k];
    let tok = f.token(e.segment, e.row, l.outcome);
    let venue_ns = e.venue_ms.map(|v| v.wrapping_mul(1_000_000));
    let (bb, ba) = match &l.best {
        Some((b, a)) => (Some(b.clone()), Some(a.clone())),
        None => (None, None),
    };
    let bh = l.book_hash.map(|h| hex(&h, false));
    vec![
        ("segment", json!(e.segment), e.segment.to_string()),
        ("index", json!(e.index), opt(e.index)),
        ("level_index", json!(k), k.to_string()),
        (
            "event_type",
            json!(e.kind.as_u8()),
            e.kind.as_u8().to_string(),
        ),
        ("row", json!(e.row), e.row.to_string()),
        ("token_id", json!(tok), tok.to_string()),
        ("side", json!(l.side.as_u8()), l.side.as_u8().to_string()),
        ("price", json!(micro(l.price_micro)), micro(l.price_micro)),
        ("size", json!(micro(l.size_micro)), micro(l.size_micro)),
        ("mirror", json!(l.mirror), (l.mirror as u8).to_string()),
        ("recv_ns", json!(e.recv_ns), opt(e.recv_ns)),
        ("venue_ns", json!(venue_ns), opt(venue_ns)),
        ("best_bid", json!(bb), bb.clone().unwrap_or_default()),
        ("best_ask", json!(ba), ba.clone().unwrap_or_default()),
        ("book_hash", json!(bh), bh.clone().unwrap_or_default()),
        ("outcome", json!(l.outcome), l.outcome.to_string()),
    ]
}

fn write_row(
    w: &mut dyn Write,
    fmt: TextFormat,
    fields: Vec<(&'static str, serde_json::Value, String)>,
) -> AnyResult<()> {
    match fmt {
        TextFormat::Csv => {
            let mut line = String::new();
            for (i, (_, _, s)) in fields.iter().enumerate() {
                if i > 0 {
                    line.push(',');
                }
                push_csv(&mut line, s);
            }
            line.push('\n');
            w.write_all(line.as_bytes())?;
        }
        TextFormat::Ndjson => {
            let mut m = serde_json::Map::new();
            for (k, v, _) in fields {
                m.insert(k.to_string(), v);
            }
            serde_json::to_writer(&mut *w, &serde_json::Value::Object(m))?;
            w.write_all(b"\n")?;
        }
    }
    Ok(())
}

fn book_max_fields(r: &BookMaxRow) -> Vec<(&'static str, serde_json::Value, String)> {
    let venue = probe_format::bookmax::VENUE;
    vec![
        ("recv_ns", json!(r.recv_ns), r.recv_ns.to_string()),
        ("venue_ns", json!(r.venue_ns), r.venue_ns.to_string()),
        ("venue", json!(venue), venue.to_string()),
        ("token_id", json!(r.token_id), r.token_id.to_string()),
        ("seq", json!(r.seq), r.seq.to_string()),
        ("qual", json!(r.qual), r.qual.to_string()),
        ("op", json!(r.op.as_str()), r.op.as_str().to_string()),
        ("side", json!(r.side), r.side.to_string()),
        ("price", json!(micro(r.price_micro)), micro(r.price_micro)),
        ("size", json!(micro(r.size_micro)), micro(r.size_micro)),
        ("level", json!(r.level), r.level.to_string()),
        ("event_type", json!(r.event_type), r.event_type.to_string()),
    ]
}

fn cat(
    f: &File,
    q: &Query,
    what: What,
    fmt: TextFormat,
    limit: Option<u64>,
    header: bool,
    w: &mut dyn Write,
) -> AnyResult<()> {
    if header && matches!(fmt, TextFormat::Csv) {
        let cols: Vec<&str> = match what {
            What::Events => EVENT_COLS.to_vec(),
            What::Levels => LEVEL_COLS.to_vec(),
            What::BookMax => BOOK_MAX_COLUMNS.to_vec(),
        };
        writeln!(w, "{}", cols.join(","))?;
    }
    let mut n = 0u64;
    let full = |n: u64| limit.is_some_and(|l| n >= l);
    let mut bm = BookMax::new(f);
    let mut rows = Vec::new();
    for ev in q.events() {
        if full(n) {
            break;
        }
        let ev = ev?;
        match what {
            What::Events => {
                write_row(w, fmt, event_fields(f, &ev))?;
                n += 1;
            }
            What::Levels => {
                for k in 0..ev.levels.len() {
                    if full(n) {
                        break;
                    }
                    write_row(w, fmt, level_fields(f, &ev, k))?;
                    n += 1;
                }
            }
            What::BookMax => {
                rows.clear();
                bm.event(&ev, &mut rows);
                for r in &rows {
                    if full(n) {
                        break;
                    }
                    write_row(w, fmt, book_max_fields(r))?;
                    n += 1;
                }
            }
        }
    }
    Ok(())
}

fn convert(f: &File, q: &Query, out: &Path, fmt: OutFormat, zstd_level: i32) -> AnyResult<u64> {
    let schema = book_max_schema();
    let fh = BufWriter::with_capacity(1 << 20, std::fs::File::create(out)?);
    let mut w = match fmt {
        OutFormat::Parquet => BatchWriter::parquet(fh, &schema, zstd_level)?,
        OutFormat::Arrow => BatchWriter::ipc(fh, &schema)?,
    };
    let mut bm = BookMax::new(f);
    let mut rows: Vec<BookMaxRow> = Vec::with_capacity(BATCH_ROWS + 1024);
    for ev in q.events() {
        let ev = ev?;
        bm.event(&ev, &mut rows);
        if rows.len() >= BATCH_ROWS {
            w.write(&book_max_batch(&rows)?)?;
            rows.clear();
        }
    }
    if !rows.is_empty() || bm.rows() == 0 {
        w.write(&book_max_batch(&rows)?)?;
    }
    w.finish()?;
    Ok(bm.rows() as u64)
}

fn verify(path: &Path, no_hash: bool) -> AnyResult<ExitCode> {
    let t = std::time::Instant::now();
    let f = match File::open(path) {
        Ok(f) => f,
        Err(e) => {
            println!("FAIL {}: {}", path.display(), e);
            return Ok(ExitCode::from(1));
        }
    };
    match verify_all(&f, no_hash) {
        Ok(v) => {
            println!(
                "OK {} {} {:.3}s",
                path.display(),
                v,
                t.elapsed().as_secs_f64()
            );
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            println!("FAIL {}: {}", path.display(), e);
            Ok(ExitCode::from(1))
        }
    }
}

/// Every segment decoded with every side stream, plus the structural invariants a file alone can
/// show: segment starts on the grid, ascending, inside the hour; one market block per bitmap
/// bit; every market block opening with one checkpoint per outcome, all checkpoint sizes > 0.
fn verify_all(f: &File, no_hash: bool) -> PResult<serde_json::Value> {
    {
        let h = f.header();
        let seg_ns = h.seg_s * 1_000_000_000;
        let (mut ne, mut nl, mut nt, mut np, mut nh, mut nb) = (0u64, 0u64, 0u64, 0u64, 0u64, 0u64);
        let mut prev_t0 = i64::MIN;
        for (si, s) in f.segments().iter().enumerate() {
            let fail = |m: String| {
                Err(probe_format::Error::Format(format!(
                    "segment {}: {}",
                    si, m
                )))
            };
            if s.t0_ns % seg_ns != 0 {
                return fail(format!(
                    "t0_ns {} is not a multiple of {} s",
                    s.t0_ns, h.seg_s
                ));
            }
            if s.t0_ns <= prev_t0 {
                return fail("segments do not ascend".into());
            }
            if h.hour_start_ns != 0
                && (s.t0_ns < h.hour_start_ns || s.t0_ns >= h.hour_start_ns + 3_600_000_000_000)
            {
                return fail(format!("t0_ns {} outside the file's hour", s.t0_ns));
            }
            prev_t0 = s.t0_ns;
            let market_blocks = s.blocks.iter().filter(|b| b.row < s.nmkt).count();
            if market_blocks != s.bitmap_rows() {
                return fail(format!(
                    "{} market blocks, {} checkpoint bitmap rows",
                    market_blocks,
                    s.bitmap_rows()
                ));
            }
            if h.has_hash && !no_hash && s.part("hash").is_none() {
                return fail("has_hash but no hash part".into());
            }
            let evs = f.query().book_hashes(!no_hash).segment(si)?;
            for e in &evs {
                ne += 1;
                nl += e.levels.len() as u64;
                nt += e.tx_hash.is_some() as u64;
                np += e.payload.is_some() as u64;
                nh += e.book_hash.is_some() as u64
                    + e.levels.iter().filter(|l| l.book_hash.is_some()).count() as u64;
                nb += e.levels.iter().filter(|l| l.best.is_some()).count() as u64;
                if e.kind == EventType::Checkpoint && e.levels.iter().any(|l| l.size_micro <= 0) {
                    return fail("a checkpoint level with size 0".into());
                }
            }
            let mut ck: std::collections::HashMap<usize, [u8; 2]> =
                std::collections::HashMap::new();
            for e in evs.iter().filter(|e| e.kind == EventType::Checkpoint) {
                ck.entry(e.row).or_insert([0, 0])[(e.outcome & 1) as usize] += 1;
            }
            if let Some((row, c)) = ck.iter().find(|(_, c)| **c != [1, 1]) {
                return fail(format!(
                    "market row {}: checkpoints per outcome {:?}, want one each",
                    row, c
                ));
            }
        }
        Ok(
            json!({"segments": f.segments().len(), "events": ne, "levels": nl, "trades": nt, "payloads": np, "hash_items": nh, "best_exceptions": nb}),
        )
    }
}
