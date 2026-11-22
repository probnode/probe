# Reading a Probe file

One UTC hour of a venue, every change of every price level of every outcome token at full depth,
every trade, both clocks. This page is the whole path, three ways: the `probe` command, the Rust
crate, and the Python package. The bytes themselves are in [the specification](../spec/PROBE.md);
nothing here restates them.

Every command below is executed by `probe-cli/tests/docs.rs` against the conformance vector
`tests/golden/basic_v2.probe` on every push, so the page cannot drift from the tool.

## Install

The command, with no toolchain:

```sh
# Linux, macOS, Windows: the archive for your platform, from the release page
curl -LO https://github.com/probnode/probe/releases/latest/download/probe-x86_64-unknown-linux-musl.tar.gz
curl -LO https://github.com/probnode/probe/releases/latest/download/SHA256SUMS
sha256sum --check --ignore-missing SHA256SUMS
tar xzf probe-x86_64-unknown-linux-musl.tar.gz
./probe --version
```

The archives are built by CI from the tag and the checksums are published beside them; verify
before you run, as you would with any binary from the internet.

With a toolchain, or as a library:

```sh
cargo install probe-cli          # the `probe` command
cargo add probe-format           # the Rust library
pip install probe-format         # the Python package (pyarrow in, pandas or polars out)
```

## What is in the file

```sh
probe info hour.probe
probe info hour.probe --markets
```

`info` prints the header: the Probe version, the hour, the units and scales, the number of
markets and segments, the block index and, with `--markets`, every market and token in the hour.
Nothing is decoded beyond the header, so it is instant on a 500 MB file.

## One market, without the rest

A per-market block index means a market's hour is a seek rather than a scan — milliseconds once
the file is open, and over HTTP range requests if the file is remote.

```sh
probe cat hour.probe --market will-x-happen --what levels --format ndjson --limit 20
probe cat hour.probe --market will-x-happen --what events --format csv
```

`--what events` gives one row per event — its type, both clocks, the fee, the transaction hash,
the payload and the book hash. `--what levels` gives one row per level change: token, side,
price, size, whether it mirrors the other outcome, and the best-price exception if there was one.

## A window of time

```sh
probe cat hour.probe --from 2026-11-12T10:05:00Z --to 2026-11-12T10:06:00Z
```

Any five-minute boundary is a valid start: every block opens with full-book checkpoints, so a
window does not have to be read from the top of the hour.

## The book at an instant

```sh
probe book hour.probe --token 7001 --at 2026-11-12T10:30:00Z
```

Both sides, every level, as of that receive time — rebuilt from the checkpoint that opens the
block and the changes since.

## Into the Arrow world

```sh
probe convert hour.probe -o book.parquet     # or book.arrow
```

`book-max`: every level change, checkpoint level and trade with its sequence, quality flag,
operation and level. That is the same table ProbNode sells as the `book-max` schema, so a parquet
you make here and a parquet you buy carry the same rows.

## Python

```python
import probe_format as pf

f = pf.open("hour.probe")
print(f.version, len(f.markets()), "markets")

book = f.book_max(market="will-x-happen")    # pyarrow.Table
df = book.to_pandas()
pl_df = pf.to_polars(book)

events = f.events(market="will-x-happen")
levels = f.levels(market="will-x-happen")
snapshot = f.book_at("7001", 1794477900000000000)
```

An hour of the full book is hundreds of millions of levels, so read it a market at a time rather
than whole:

```python
for slug in f.markets()["slug"].to_pylist():
    table = f.book_max(market=slug)
    ...
```

## Rust

```rust
use probe_format::File;

let f = File::open("hour.probe")?;
println!("{} with {} markets", f.version(), f.markets().len());

// Every event of the hour, in arrival order.
for ev in f.events() {
    let ev = ev?;
    for l in &ev.levels {
        let _ = (ev.kind, ev.recv_ns, l.side, l.price_micro, l.size_micro);
    }
}
```

The crate is `probe-format`; `File::open` memory-maps the file and detects v1, v2 or v3 from the
header, and every API behaves the same on all three. The full surface — `events`, `query`,
`core_dump`, `v3::transcode` — is on [docs.rs](https://docs.rs/probe-format), and the crate's own
front page carries this example.

## Upgrading a file to v3

v3 is smaller and faster to decode, and the transcode is lossless and deterministic: the core
dump of the result is byte-identical to the core dump of the source, which is what `--verify`
checks.

```sh
probe transcode hour.probe hour.v3.probe --profile fast --verify
probe info hour.v3.probe
```

Profiles: `fast` (the default), `fast1`, `rc1`. A reader of any version opens all three.

## Checking a file, and checking a reader

```sh
probe verify hour.probe          # every structural check, every side stream
probe dump hour.probe -o core.csv
```

`dump` writes the conformance core dump and prints its FNV-1a 64 digest. That digest is how a new
reader proves itself: decode the vectors in [`tests/golden`](../tests/golden), write the core
dump, and compare. Three independent implementations agree on every vector and on a production
hour — see [`tests/README.md`](../tests/README.md).

## When something is wrong

A corrupt or truncated file returns an error and never a panic. `probe verify` is the first thing
to run; it names the structure that failed rather than the byte offset where decoding stopped. If
a file was transferred as text rather than binary, the magic check fails immediately: the first
four bytes of every Probe file are `EVM4`, whatever the version.

Files written by ProbNode are named `.evmin`, which is the format's own version string
(`evmin/4`, `/5`, `/6` for v1, v2 and v3). The extension is not checked; the magic is.
