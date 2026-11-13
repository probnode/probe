# probe-cli

The `probe` command for [Probe](https://github.com/probnode/probe) order-book files.

```sh
cargo install probe-cli

probe transcode hour.probe hour.v3.probe     # Probe v3, profile fast; --profile fast1 | rc1, --verify
probe info hour.probe --markets              # header, versions, markets, segments, part codecs
probe cat hour.probe --market will-x-happen --what levels --format ndjson --limit 20
probe cat hour.probe --from 2026-11-12T10:05:00Z --to 2026-11-12T10:06:00Z
probe convert hour.probe -o book.parquet     # the per-level book-max table (or .arrow)
probe verify hour.probe                      # every check, every side stream
probe dump hour.probe -o core.csv            # the conformance core dump and its FNV-1a 64
probe book hour.probe --token 7001 --at 2026-11-12T10:30:00Z
```

License: Apache-2.0.
