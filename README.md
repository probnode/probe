<p align="center">
  <a href="https://probnode.com"><img src="assets/probnode-logo.png" alt="ProbNode" width="96" height="96"></a>
</p>

<h1 align="center">Probe</h1>

<p align="center">
  <a href="https://github.com/probnode/probe/actions/workflows/ci.yml"><img src="https://github.com/probnode/probe/actions/workflows/ci.yml/badge.svg?branch=main" alt="CI"></a>
  <img src="https://img.shields.io/badge/rust-1.85%2B-orange" alt="Rust 1.85+">
  <img src="https://img.shields.io/badge/python-3.9%2B-blue" alt="Python 3.9+">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-blue" alt="Apache-2.0"></a>
</p>

<p align="center"><b>The lossless file format for prediction-market order books.</b></p>

**Probe** is a lossless file format for prediction-market order books. One file holds one UTC hour
of a venue (today: Polymarket): every change of every price level of every outcome token at full
depth, every trade with its transaction hash, both clocks, and a full-book checkpoint of each
market every five minutes. Nothing is sampled or throttled.

This repository holds the [specification](spec/PROBE.md), the conformance vectors, and the
reference implementations:

| piece | what it gives you |
|---|---|
| [`probe-format`](probe) Rust crate | open a file (mmap), header, versions, markets, segments, block index; every event and level in arrival order; one market through the block index; a time range; the book of a token at a time; every side stream (trade tx hashes, payloads, best-price exceptions, venue book hashes); Arrow and parquet output |
| [`probe`](probe-cli) CLI | `info`, `cat` (CSV / NDJSON), `convert` (parquet / Arrow IPC), `verify`, `dump`, `book` |
| [`probe-format`](probe-py) Python package | Probe to pyarrow, pandas and polars, decoded in Rust |
| [spec/PROBE.md](spec/PROBE.md) | the normative specification, with the design rationale and measurements |
| [tests/golden](tests/golden) | conformance vectors and the outputs every reader must reproduce byte for byte |

## Install

The packages go to crates.io and PyPI with the first release. Until then, from this repository:

```sh
cargo install --path probe-cli    # the probe command
cargo build --release             # the library
```

Once released:

```sh
cargo add probe-format            # the Rust library
cargo install probe-cli           # the probe command
pip install probe-format          # Python
```

## Quick start

### Rust

```rust
use probe_format::File;

fn main() -> probe_format::Result<()> {
    let f = File::open("hour.probe")?;
    println!("{} with {} markets, {} segments", f.version(), f.markets().len(), f.segments().len());

    // One market, through the block index, with its trades' transaction hashes.
    for ev in f.query().market("will-x-happen")?.events() {
        let ev = ev?;
        for l in &ev.levels {
            println!("{:?} {:?} {} {} {}", ev.kind, ev.recv_ns, l.side.venue_str(), l.price_micro, l.size_micro);
        }
    }

    // The book of a token at a time.
    if let Some(book) = f.book_at("7001", f.header().hour_start_ns + 1_800_000_000_000)? {
        println!("best bid {:?}, best ask {:?}", book.bids.last(), book.asks.first());
    }
    Ok(())
}
```

### CLI

```sh
probe info hour.probe --markets
probe cat hour.probe --market will-x-happen --what levels --format ndjson --limit 20
probe cat hour.probe --from 2026-11-12T10:05:00Z --to 2026-11-12T10:06:00Z
probe convert hour.probe -o book.parquet           # the per-level book-max table; .arrow for Arrow IPC
probe verify hour.probe                            # every structural check and every side stream
probe dump hour.probe -o core.csv                  # the conformance core dump and its digest
probe book hour.probe --token 7001 --at 2026-11-12T10:30:00Z
```

### Python

```python
import probe_format as pf

f = pf.open("hour.probe")
book = f.book_max(market="will-x-happen")     # pyarrow.Table
df = book.to_pandas()
events = f.events(start_ns=f.header["hour_start_ns"])   # tx hashes, payloads, book hashes
```

## Why Probe

- **Smallest.** 533 MB for an hour of every Polymarket market: 63.6 M events and 163.9 M levels,
  3.3 bytes per decoded level. That is 2.0x smaller than the same records as fixed 48-byte
  records with zstd 19, 2.9x smaller than with zstd 3, 3.3x smaller than parquet and 14.7x
  smaller than raw records.
- **Everything, not a sample.** Full depth, every change, before any throttle. Books rebuilt from
  Probe matched an independent replay of the raw websocket stream at 100 of 100 random
  (token, time) points.
- **Two clocks on every event.** The receive time at 1 µs, and the venue's own time in ms.
- **One market without the rest.** A per-market block index: one market's hour is read and
  decoded in milliseconds once the file is open, by HTTP range requests if the file is remote.
- **Parallel by construction.** An hour is about 943 k independent blocks, one per market per
  five-minute segment. The Rust reader decodes the whole hour in 10.8 s on 8 cores.
- **Any five-minute boundary is a valid start.** Every block opens with full-book checkpoints.
- **Verifiable.** Conformance vectors and a byte-for-byte core dump keep every reader equal.
  Three independent implementations (the writer's own decoder, a Go reader and this Rust reader)
  agree on every vector and on a production hour's digest.

### Compared with other encodings

The same hour (2026-11-12 10:00 UTC, every market, 163.9 M levels) on the same machine (a cloud
VM, 8 vCPUs), decoded end to end from a warm page cache to records in memory:

| encoding | size | vs Probe | 1 core | 8 cores |
|---|---:|---:|---:|---:|
| **Probe v1, Rust** | **533 MB** | **1.0x** | 49.3 s | 10.8 s |
| 48-byte records + zstd 19 | 1,086 MB | 2.0x | 9.6 s | 2.6 s |
| 48-byte records + zstd 3 | 1,532 MB | 2.9x | 9.4 s | 2.5 s |
| parquet, zstd 3 | 1,758 MB | 3.3x | 22.3 s | 7.3 s |
| 48-byte records, raw | 7,868 MB | 14.7x | 0.7 s | 0.14 s |

The 48-byte record is the size of a DBN trade record, the layout of Databento's format. Probe
today pays decode time for its size: downloaded and read end to end it is level with zstd'd
records at 1 Gbit/s and about 2x faster at 100 Mbit/s. The next version's goal is to lead on
both (see Codec strategy).

Why the gap in size: book updates are predictable from the book itself. A price is near the best
on its side, a size is near the level's previous size, and Polymarket's two outcome books mirror
each other. A general compressor over fixed records cannot see that structure; Probe's context
models do. Section 7 of the [specification](spec/PROBE.md#7-design-rationale) has the ablation,
step by step.

## Layout

    magic "EVM4" | u32 header length | JSON header (version, units, coding parameters, part offsets)
    segments x 12 (5 minutes each)
      order          the arrival order across markets (range-coded)
      bidx           block index: per block, market row and length (v2: + six counts)
      blocks         one per market that changed: checkpoints, then events and levels (range-coded)
      tx             trade transaction hashes, 32 bytes each
      payload.zst    tick-size and other event payloads, best-price exceptions (zstd JSON)
      hash           optional venue book hashes, 20 bytes each
    markets_zst      the markets table (zstd JSON): slugs, tokens, units

## Versions

| Probe | header `version` | status |
|---|---|---|
| v1 | `evmin/4` | frozen |
| v2 | `evmin/5` | current: counts in the block index, side streams in block order, so one market's trades carry their transaction hashes without decoding the rest |
| v3 | - | in design: one container, a registered codec per stream, profiles |

A reader must accept every earlier version, and a new version never changes what an earlier file
means. Upgrading is a lossless transcode, checked by the core dump. `evmin` is the format's
working name, kept in the version strings.

## Codec strategy

Probe separates **the container**, what a file is, from **the codecs**, how each stream inside it
is compressed. The container is the long-term contract; codecs are expected to improve.

- **The container is stable.** Hour files, segments, per-market blocks, the block and time
  indexes, the markets table, both clocks and the checkpoints keep their meaning across versions.
- **Each stream names its codec** (from v3). A better coder is a new codec id, not a new format.
- **Codecs are added, never redefined.** An id once registered means the same bytes forever, so a
  file written today still opens in ten years.
- **Every codec ships conformance vectors** before it is registered; every reader must decode
  them to the identical core dump.
- **Profiles pick codecs by purpose**, all lossless transcodes of each other:
  **FAST** for analysis (static multi-symbol tANS / rANS, interleaved, SIMD), **BALANCED**
  (adaptive multi-symbol coding with the full context models) and **ARCHIVE** for cold storage
  (the strongest models).

v3's target: smaller than records with zstd 19, and faster to decode than records with zstd 3.

## Roadmap

- [x] Specification of v1 and v2, with conformance vectors
- [x] Rust reader: everything in the file, one market, time ranges, books at a time
- [x] `probe` CLI: info, cat, convert (parquet, Arrow), verify, dump, book
- [x] Python bindings (pyarrow, pandas, polars)
- [ ] `.evidx` sidecar reader (one-market side streams for v1 files)
- [ ] Ranged reads over HTTP / object storage from the Rust crate (the format supports it today)
- [ ] C ABI and headers, for C, C++ and other languages
- [ ] v3: registered codecs, the FAST profile, a writer and `probe transcode`
- [ ] Wider SIMD (AVX-512, NEON / SVE) and GPU decode for bulk replays

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Security reports: [SECURITY.md](SECURITY.md).

## License

Apache License 2.0. See [LICENSE](LICENSE). Copyright 2026 ProbNode.
