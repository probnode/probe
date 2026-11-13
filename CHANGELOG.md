# Changelog

All notable changes to the implementations in this repository (the `probe-format` crate, the
`probe-cli` crate and the `probe-format` Python package) are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the packages follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html). The format's own versions are
specified in [spec/PROBE.md](spec/PROBE.md) and summarised under [Format versions](#format-versions).

## [Unreleased]

## [0.2.0] - 2026-11-13

Probe v3 (`evmin/6`): read it everywhere, and write it from any Probe file.

### Added

- Probe v3 reading in `probe-format`: `File::open` detects v3, and every API (events, one
  market, time ranges, books at a time, side streams, core and full dumps, `book-max`, Arrow and
  parquet) works on v3 exactly as on v1 and v2. Codecs 0, 2 and 16 - 23 of the registry.
- `probe_format::v3`: `transcode` (v1, v2 or v3 to v3; lossless and deterministic) with the
  profiles `fast` (the default: model M2, event columns and static rANS blocks), `fast1` (model
  M1) and `rc1` (v1's blocks kept, with v3's order and index); `verify_transcode`; the codec
  registry `v3::codec`.
- `probe transcode IN OUT [--profile fast|fast1|rc1] [--verify]`. `probe info` shows each part's
  codec, the profile and the source version; `cat`, `convert`, `verify`, `dump` and `book` read v3.
- Python: `probe_format` reads v3; `File.transcode(path, profile="fast")`; the header has
  `profile` and `source_version`.
- `Header::profile`, `Header::source_version`, `Header::side_layout`, `Part::codec`,
  `Segment::blocks_codec`.
- Specification section 9: the v3 container, the codec registry and its rules, static rANS,
  the block index, the clock-merge order, models M1 and M2, the markets parts, the profiles, the
  design rationale and the measurements.
- Conformance: `tests/golden/v3` (every v1 / v2 vector in every profile, 30 files) and their core
  dumps in `tests/expected/v3`. Every v3 dump is byte-identical to its source's, every full dump
  equals the source's `events.csv`, books and `book-max` equal the reference, and the transcoder
  writes the committed bytes. The corrupt-input test covers v3.

### Changed

- `Version` has a third variant, `V3`; `Header` and `Part` have new fields. Code that matches
  `Version` exhaustively or builds these structs needs an update.
- `Block` and `SegmentCore` implement `PartialEq` and `Eq`.

## [0.1.0] - 2026-11-13

First release of the reference implementations. `probe-format` and `probe-cli` are on crates.io;
the `probe-format` Python package follows on PyPI.

### Added

- `probe-format` crate: header, versions, markets, segments and block indexes; every event and
  level in the reference order (serial or parallel); one market through the block index; a
  receive-time range; the book of a token at a time; every side stream (trade transaction
  hashes, payloads, best-price exceptions, venue book hashes); the core and full conformance
  dumps; `book-max` rows; Arrow record batches, Arrow IPC and parquet output (features `arrow`,
  `parquet`).
- `probe` CLI (crate `probe-cli`): `info`, `cat`, `convert`, `verify`, `dump`, `book`.
- `probe-format` Python package: events, levels, markets and `book-max` as pyarrow tables;
  pandas and polars through Arrow; books at a time. One abi3 wheel per platform, Python 3.9+.
- Conformance: every vector's full dump equals its `events.csv`, every core dump and every
  market's `book-max` equals the Go reader's, every book query equals `books.csv`.
- The range decoder renormalises in a loop, as the encoder does (vector `vec_renorm_shift0`).
- Files with flag bits other than 16 and 64 are refused rather than decoded.

## Format versions

### v3 (`evmin/6`) - current, 2026-11-13

- The v1 container with a registered codec id on every part; codecs are added, never redefined,
  and a reader refuses an id it does not implement.
- A new block index (`bidx3`), the arrival order as the receive-clock order plus deferrals, and
  static-rANS blocks (models M1 and M2); side streams and the markets table kept byte for byte.
- Profiles `fast`, `fast1` and `rc1`, all lossless transcodes of v1 / v2.

### v2 (`evmin/5`) - frozen; what writers record

- The block index has 8 uvarints per entry: offset and length plus the block's event, level,
  trade, payload, hash and best-exception counts.
- Side streams are in block order, and best-price exceptions are block-relative, so one market's
  trade hashes and payloads need only its own blocks.

### v1 (`evmin/4`) - frozen 2026-11-12

- The first full-book hour format.
- An optional `.evidx` sidecar gives v1 files the v2 per-block positions.

[Unreleased]: https://github.com/probnode/probe/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/probnode/probe/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/probnode/probe/releases/tag/v0.1.0
