# Changelog

This file covers the format and the implementations in this repository. Format versions are
specified in [spec/PROBE.md](spec/PROBE.md). The crates and the Python package follow semantic
versioning.

## Format

### v3 - in design
- One container with a registered codec id per stream, and profiles FAST, BALANCED and ARCHIVE,
  transcoded losslessly from v1 and v2.
- Target: smaller than fixed records with zstd 19, and faster to decode than fixed records with
  zstd 3, on the same hour.

### v2 (`evmin/5`) - current
- The block index has 8 uvarints per entry: offset and length plus the block's event, level,
  trade, payload, hash and best-exception counts.
- Side streams are in block order, and best-price exceptions are block-relative, so one market's
  trade hashes and payloads need only its own blocks.

### v1 (`evmin/4`) - frozen 2026-11-12
- The first full-book hour format.
- An optional `.evidx` sidecar gives v1 files the v2 per-block positions.

## Implementations

### 0.1.0 - unreleased
- `probe-format` crate: header, versions, markets, segments and block indexes; every event and
  level in the reference order (serial or parallel); one market through the block index; a
  receive-time range; the book of a token at a time; every side stream (trade transaction
  hashes, payloads, best-price exceptions, venue book hashes); the core and full conformance
  dumps; `book-max` rows; Arrow record batches, Arrow IPC and parquet output (features `arrow`,
  `parquet`).
- `probe` CLI: `info`, `cat`, `convert`, `verify`, `dump`, `book`.
- `probe-format` Python package: events, levels, markets and `book-max` as pyarrow tables;
  pandas and polars through Arrow; books at a time.
- Conformance: every vector's full dump equals its `events.csv`, every core dump and every
  market's `book-max` equals the Go reader's, every book query equals `books.csv`.
- The range decoder renormalises in a loop, as the encoder does (vector `vec_renorm_shift0`).
- Files with flag bits other than 16 and 64 are refused rather than decoded.
