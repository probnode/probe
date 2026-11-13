<p align="center">
  <a href="https://probnode.com"><img src="https://raw.githubusercontent.com/probnode/probe/main/assets/probnode-logo.png" alt="ProbNode" width="96" height="96"></a>
</p>

<h1 align="center">Probe</h1>

<p align="center"><b>The lossless file format for prediction-market order books.</b></p>

<p align="center">
  <a href="https://github.com/probnode/probe/actions/workflows/ci.yml"><img src="https://github.com/probnode/probe/actions/workflows/ci.yml/badge.svg?branch=main" alt="CI"></a>
  <a href="https://crates.io/crates/probe-format"><img src="https://img.shields.io/crates/v/probe-format.svg?label=probe-format" alt="probe-format on crates.io"></a>
  <a href="https://crates.io/crates/probe-cli"><img src="https://img.shields.io/crates/v/probe-cli.svg?label=probe-cli" alt="probe-cli on crates.io"></a>
  <a href="https://docs.rs/probe-format"><img src="https://img.shields.io/docsrs/probe-format" alt="docs.rs"></a>
  <a href="https://github.com/probnode/probe/blob/main/Cargo.toml"><img src="https://img.shields.io/badge/MSRV-1.85-blue.svg" alt="MSRV 1.85"></a>
  <img src="https://img.shields.io/badge/python-3.9%2B-blue.svg" alt="Python 3.9+">
  <a href="https://github.com/probnode/probe/blob/main/LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-blue.svg" alt="License: Apache-2.0"></a>
  <a href="https://pypi.org/project/probe-format/"><img src="https://img.shields.io/pypi/v/probe-format.svg?label=PyPI" alt="probe-format on PyPI"></a>
</p>

## Overview

**Probe** is a lossless file format for prediction-market order books. One file holds one UTC hour
of a venue (today: Polymarket): every change of every price level of every outcome token at full
depth, every trade with its transaction hash, both clocks, and a full-book checkpoint of each
market every five minutes. Nothing is sampled or throttled. [ProbNode](https://probnode.com)
stores its order books in Probe.

This repository holds the [specification](spec/PROBE.md), the conformance vectors, and the
reference implementations:

| piece | what it gives you |
|---|---|
| [`probe-format`](probe) Rust crate | open a file (mmap), header, versions, markets, segments, block index; every event and level in arrival order; one market through the block index; a time range; the book of a token at a time; every side stream (trade tx hashes, payloads, best-price exceptions, venue book hashes); Arrow and parquet output |
| [`probe`](probe-cli) CLI (crate `probe-cli`) | `info`, `cat` (CSV / NDJSON), `convert` (parquet / Arrow IPC), `verify`, `dump`, `book` |
| [`probe-format`](probe-py) Python package | Probe to pyarrow, pandas and polars, decoded in Rust |
| [spec/PROBE.md](spec/PROBE.md) | the normative specification, with the design rationale and measurements |
| [tests/golden](tests/golden) | conformance vectors and the outputs every reader must reproduce byte for byte |

## Features

- **Smallest.** 533 MB for an hour of every Polymarket market: 63.6 M events and 163.9 M levels,
  3.3 bytes per decoded level.
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
- **Safe on any input.** Corrupt or truncated files return an error, never a panic.

Why it is small: book updates are predictable from the book itself. A price is near the best
on its side, a size is near the level's previous size, and Polymarket's two outcome books mirror
each other. Probe's context models use that structure. Section 7 of the [specification](spec/PROBE.md#7-design-rationale) has the ablation,
step by step.

## Installation

Rust library and command line, from crates.io:

```sh
cargo add probe-format                         # the library
cargo add probe-format --features parquet      # with Arrow and parquet output
cargo install probe-cli                        # the probe command
```

Python (prebuilt wheels for Linux, macOS and Windows, Python 3.9+):

```sh
pip install probe-format                       # import probe_format
```

Library features: `parallel` (default, rayon), `arrow` (record batches, Arrow IPC), `parquet`.

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

ProbNode's `book_full` dataset serves Probe hours; the [Rust client](https://github.com/probnode/probnode-rs)
opens them directly with its `probe` feature.

## Documentation

- The specification: [spec/PROBE.md](spec/PROBE.md), normative, with the design rationale and
  the measurements above.
- Rust API: [docs.rs/probe-format](https://docs.rs/probe-format).
- The conformance suite: [tests/README.md](tests/README.md).
- ProbNode's data and schemas: [probnode.com/docs](https://probnode.com/docs).

### File layout

    magic "EVM4" | u32 header length | JSON header (version, units, coding parameters, part offsets)
    segments x 12 (5 minutes each)
      order          the arrival order across markets (range-coded)
      bidx           block index: per block, market row and length (v2: + six counts)
      blocks         one per market that changed: checkpoints, then events and levels (range-coded)
      tx             trade transaction hashes, 32 bytes each
      payload.zst    tick-size and other event payloads, best-price exceptions (zstd JSON)
      hash           optional venue book hashes, 20 bytes each
    markets_zst      the markets table (zstd JSON): slugs, tokens, units

### Codec strategy

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


### Roadmap

- [x] Specification of v1 and v2, with conformance vectors
- [x] Rust reader: everything in the file, one market, time ranges, books at a time
- [x] `probe` CLI: info, cat, convert (parquet, Arrow), verify, dump, book
- [x] Python bindings (pyarrow, pandas, polars)
- [ ] `.evidx` sidecar reader (one-market side streams for v1 files)
- [ ] Ranged reads over HTTP / object storage from the Rust crate (the format supports it today)
- [ ] C ABI and headers, for C, C++ and other languages
- [ ] v3: registered codecs, the FAST profile, a writer and `probe transcode`
- [ ] Wider SIMD (AVX-512, NEON / SVE) and GPU decode for bulk replays

## Versioning and compatibility

The format and the implementations are versioned separately.

| Probe | header `version` | status |
|---|---|---|
| v1 | `evmin/4` | frozen |
| v2 | `evmin/5` | current: counts in the block index, side streams in block order, so one market's trades carry their transaction hashes without decoding the rest |
| v3 | - | in design: one container, a registered codec per stream, profiles |

- A reader must accept every earlier version, and a new version never changes what an earlier
  file means. Upgrading is a lossless transcode, checked by the core dump. `evmin` is the
  format's working name, kept in the version strings.
- The crates and the Python package follow [Semantic Versioning](https://semver.org). Until 1.0, a
  minor version may change the API; every change is in the [CHANGELOG](CHANGELOG.md).
- Minimum supported Rust version: **1.85**; Python **3.9+** (one abi3 wheel per platform).
- Tested on Linux, macOS and Windows.

## Support

- Questions about the format or ProbNode's data: [hello@probnode.com](mailto:hello@probnode.com).
- Bugs, readers in other languages and feature requests: [open an issue](https://github.com/probnode/probe/issues/new/choose).
- Security reports: privately, as the [security policy](SECURITY.md) says.

## Contributing

Bug reports, new readers in other languages, performance work and corrections to the
specification are all welcome; see [CONTRIBUTING.md](CONTRIBUTING.md). This project follows the
[Contributor Covenant](CODE_OF_CONDUCT.md).

## Other ProbNode libraries

| Repository | Language | What it is | Package |
|---|---|---|---|
| [probnode-sdk](https://github.com/probnode/probnode-sdk) | Python | The official Python client, the `probnode` command line and the MCP server | [`probnode-sdk`](https://pypi.org/project/probnode-sdk/), [`probnode-mcp`](https://pypi.org/project/probnode-mcp/) on PyPI |
| [probnode-rs](https://github.com/probnode/probnode-rs) | Rust | The official Rust client: async and blocking, typed models | `probnode` (crates.io release on the way) |
| [probnode-cpp](https://github.com/probnode/probnode-cpp) | C++17 | The official C++ client, built with CMake | CMake `find_package` or FetchContent |

## License

Apache License 2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE). Copyright 2026 ProbNode.
