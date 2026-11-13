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
| [`probe-format`](probe) Rust crate | open a file of any version, v1, v2 or v3 (mmap): header, markets, segments, block index; every event and level in arrival order; one market through the block index; a time range; the book of a token at a time; every side stream (trade tx hashes, payloads, best-price exceptions, venue book hashes); Arrow and parquet output; a lossless transcoder to Probe v3 |
| [`probe`](probe-cli) CLI (crate `probe-cli`) | `info`, `cat` (CSV / NDJSON), `convert` (parquet / Arrow IPC), `verify`, `dump`, `book`, `transcode` |
| [`probe-format`](probe-py) Python package | Probe v1, v2 and v3 to pyarrow, pandas and polars, decoded in Rust; transcoding to v3 |
| [spec/PROBE.md](spec/PROBE.md) | the normative specification, with the design rationale and measurements |
| [tests/golden](tests/golden) | conformance vectors and the outputs every reader must reproduce byte for byte |

## Features

- **Small.** 405 MB for an hour of every Polymarket market as Probe v3 (63.6 M events and 163.9 M
  levels, 2.5 bytes per decoded level), 24 % below the same hour as Probe v1.
- **Fast.** Probe v3 decodes that hour, every event and level in arrival order, in 3.36 s on 8
  cores and 13.84 s on one: 3.7 times faster than v1.
- **Everything, not a sample.** Full depth, every change, before any throttle. Books rebuilt from
  Probe matched an independent replay of the raw websocket stream at 100 of 100 random
  (token, time) points.
- **Two clocks on every event.** The receive time at 1 µs, and the venue's own time in ms.
- **One market without the rest.** A per-market block index: one market's hour is read and
  decoded in milliseconds once the file is open, by HTTP range requests if the file is remote.
- **Parallel by construction.** An hour is about 943 k independent blocks, one per market per
  five-minute segment, and v3 decodes its event columns in independent lanes.
- **Any five-minute boundary is a valid start.** Every block opens with full-book checkpoints.
- **Verifiable.** Conformance vectors and a byte-for-byte core dump keep every reader equal.
  Three independent implementations (the writer's own decoder, a Go reader and this Rust reader)
  agree on every vector and on a production hour's digest, and every v3 file decodes to its
  source's dump byte for byte.
- **Built to last.** In v3 every part of a file names its codec, and a codec id, once registered,
  means the same bytes forever. Better coders arrive as new ids; old files keep opening.
- **Safe on any input.** Corrupt or truncated files return an error, never a panic.

Why it is small: book updates are predictable from the book itself. A price is near the best
on its side, a size is near the level's previous size, and Polymarket's two outcome books mirror
each other. Probe's context models use that structure. Section 7 of the [specification](spec/PROBE.md#7-design-rationale) has the ablation,
step by step. Probe v3 keeps those models, codes them with static per-segment rANS tables, and
stores the arrival order as the clock order plus the 1 % of events that arrived late
([section 9](spec/PROBE.md#9-probe-v3-evmin6)).

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

    // Probe v3, losslessly: the same events, levels and side streams, in fewer bytes.
    let v3 = probe_format::v3::transcode(&f, probe_format::v3::Profile::Fast)?;
    std::fs::write("hour.v3.probe", &v3.bytes)?;
    Ok(())
}
```

### CLI

```sh
probe transcode hour.probe hour.v3.probe           # Probe v3, profile fast (the default)
probe transcode hour.probe hour.v3.probe --profile fast1 --verify
probe info hour.v3.probe                           # every part with its codec
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
f.transcode("hour.v3.probe")                  # Probe v3, profile fast
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

Probe v3 keeps that container and names a codec on every part (`[name, offset, length, codec]`):

    segments x 12
      tables         codec 19   the segment's rANS frequency tables
      evcols         codec 21   event type, outcome and both clocks of every event, in lanes
      blocks         codec 20   one per market: the book-dependent part of its events (static rANS)
      bidx3          codec 18   block index: row, length and counts per block
      order          codec 17   the events that arrived out of receive-clock order (deferrals)
      tx, payload.zst, hash      codec 0    the source's side streams, byte for byte
    mkunits, mkslugs codecs 22, 23   units and slugs of the markets table, without its JSON
    markets_zst      the source's markets table, byte for byte

### Probe v3 profiles, measured

One production hour (63.6 M events, 163.9 M levels) in every profile and in v1, on one GCP
e2-standard-8 VM (AMD EPYC 7B12, 8 vCPU), with the reference implementation of the v3 codecs.
Decode = from the file to every event and level in memory, in arrival order; one market = open the
file and decode one market. Every encoding has the same core-dump digest.

| encoding | size | decode, 1 core | decode, 8 cores | one market |
|---|---:|---:|---:|---:|
| **v3 fast** | **405,429,923 B** | **13.84 s** | **3.36 s** | **0.19 s** |
| v3 fast1 | 406,362,525 B | 17.53 s | 6.07 s | 0.22 s |
| v3 rc1 | 451,485,094 B | 40.72 s | 10.00 s | 0.20 s |
| v1 | 533,465,796 B | 51.06 s | 12.47 s | 0.37 s |

Transcoding that hour from v1 to v3 fast took 43.7 s on 8 threads and 111.8 s on 1, at 1.3 GB peak
memory. The profiles: **fast** (model M2: event columns + static rANS blocks, the default),
**fast1** (model M1: one static-rANS stream per block), **rc1** (v1's blocks kept as they are,
with v3's order and index). On tiny files, such as the conformance vectors, the per-segment
tables cost more than they save; v3 is built for real hours.

### Codec strategy

Probe separates **the container**, what a file is, from **the codecs**, how each stream inside it
is compressed. The container is the long-term contract; codecs are expected to improve.

- **The container is stable.** Hour files, segments, per-market blocks, the block and time
  indexes, the markets table, both clocks and the checkpoints keep their meaning across versions.
- **Each part names its codec** (v3). A better coder is a new codec id, not a new format. The
  registry today: 0 raw, 2 rc1 (v1's range coder), 16 model M1, 17 the clock-merge order, 18
  integer columns, 19 rANS tables, 20 model M2, 21 event columns, 22 / 23 markets units and slugs.
- **Codecs are added, never redefined.** An id once registered means the same bytes forever, so a
  file written today still opens in ten years.
- **Readers refuse what they do not implement.** An unknown codec id is refused, never guessed.
- **Every codec ships conformance vectors** before it is registered; every reader must decode
  them to the identical core dump. `tests/golden/v3` holds every vector in every profile.
- **Profiles pick codecs by purpose**, all lossless transcodes of each other and of v1 / v2:
  **fast** and **fast1** today; an adaptive profile and an archive profile (checkpoints predicted
  from the previous segment) are measured candidates for later codec ids.


### Roadmap

- [x] Specification of v1 and v2, with conformance vectors
- [x] Rust reader: everything in the file, one market, time ranges, books at a time
- [x] `probe` CLI: info, cat, convert (parquet, Arrow), verify, dump, book
- [x] Python bindings (pyarrow, pandas, polars)
- [ ] `.evidx` sidecar reader (one-market side streams for v1 files)
- [ ] Ranged reads over HTTP / object storage from the Rust crate (the format supports it today)
- [ ] C ABI and headers, for C, C++ and other languages
- [x] v3: registered codecs, the fast profiles, a lossless transcoder and `probe transcode`
- [ ] Wider SIMD (AVX-512, NEON / SVE) and GPU decode for bulk replays

## Versioning and compatibility

The format and the implementations are versioned separately.

| Probe | header `version` | status |
|---|---|---|
| v1 | `evmin/4` | frozen |
| v2 | `evmin/5` | frozen; what writers record: counts in the block index, side streams in block order, so one market's trades carry their transaction hashes without decoding the rest |
| v3 | `evmin/6` | current: one container, a registered codec per part, profiles fast, fast1 and rc1; made from v1 / v2 by a lossless transcode |

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
