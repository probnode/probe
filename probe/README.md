# probe-format

The Rust reader of [Probe](https://github.com/probnode/probe), a lossless file format for
prediction-market order books: every change of every price level of every outcome token, every
trade, two clocks, and five-minute full-book checkpoints, at about 2.5 bytes per level as Probe v3.

```rust
use probe_format::File;

let f = File::open("hour.probe")?;
for ev in f.query().market("will-x-happen")?.events() {
    let ev = ev?;
    for l in &ev.levels {
        println!("{:?} {:?} {} {} {}", ev.kind, ev.recv_ns, l.side.venue_str(), l.price_micro, l.size_micro);
    }
}
```

It reads Probe v1, v2 and v3 completely: header, markets table, segments and block indexes, every
event and level in the reference order, one market through the block index, a time range, the
book of a token at a time, and every side stream (trade transaction hashes, JSON payloads,
best-price exceptions, venue book hashes). Corrupt input gives an error, never a panic.

`probe_format::v3::transcode` writes Probe v3 from any Probe file, losslessly, in the profiles
`fast` (the default), `fast1` and `rc1`.

Features: `parallel` (default, rayon), `arrow` (record batches, Arrow IPC), `parquet`.

The specification is [spec/PROBE.md](https://github.com/probnode/probe/blob/main/spec/PROBE.md).
License: Apache-2.0.
