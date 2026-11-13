# Conformance vectors

`golden/` holds the conformance vectors of [spec/PROBE.md](../spec/PROBE.md) section 8 and the
outputs every reader must reproduce byte for byte; `expected/` holds outputs of an independent Go
reader that this repository's reader is held to. Never edit these files to make a test pass.

| file | what it is |
|---|---|
| `golden/*.probe` | the vectors (section 8.1 says what each one covers) |
| `golden/basic.events.csv`, `golden/X.events.csv` | the full dump (8.2) of `basic_v1.probe` / `basic_v2.probe` and of `X.probe`: every event and level with every side stream |
| `golden/*.books_queries.csv`, `golden/*.books.csv` | book-at-time queries (`token_id,ts_ns`) and their answers (5.4) |
| `golden/basic_v1.probe.evidx` | the `.evidx` sidecar of `basic_v1.probe` (Appendix A) |
| `expected/X.core.csv` | the core dump (8.2) of `X.probe` |
| `expected/X.SLUG.book-max.csv` | the `book-max` rows of market `SLUG` in `X.probe`, with a header |

The vectors were written by the production writer's code. Its own decoder, the Go reader and the
Rust reader of this repository reproduce every `events.csv`, `books.csv` and core dump.

**Line ends.** Every file here is compared as bytes: `.gitattributes` keeps them out of line-end
conversion. If a test fails on Windows with a diff on every line, check that your checkout did not
convert LF to CRLF.
