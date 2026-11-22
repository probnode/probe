# Working in this repository

For an agent or a new contributor, before the first change. The short version: this repository
publishes a **file format**, and a file format's promise is that a file written today opens in
ten years. Almost every rule below follows from that one.

## What is here

| | |
|---|---|
| [`spec/PROBE.md`](spec/PROBE.md) | the normative specification. If the code and the spec disagree, one of them is a bug — say which before you fix either |
| [`probe/`](probe) | `probe-format`, the reference reader (Rust). Memory-mapped, reads v1, v2 and v3 |
| [`probe-cli/`](probe-cli) | the `probe` command: `info`, `cat`, `convert`, `verify`, `dump`, `book`, `transcode` |
| [`probe-py/`](probe-py) | `probe-format` on PyPI: the same Rust reader behind pyarrow, pandas and polars, through PyO3 |
| [`tests/golden`](tests/golden) | the conformance vectors of spec section 8 |
| [`tests/expected`](tests/expected) | what an independent Go reader produces from them |
| [`docs/READING.md`](docs/READING.md) | how to read a file, three ways |
| [`CHANGELOG.md`](CHANGELOG.md) | Keep a Changelog; the format's own versions are a section of it |

## Run everything before you believe anything

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --all-features
cargo test -p probe-format --no-default-features --lib --tests   # the reader without the extras

pip install "maturin>=1.5,<2" pytest pyarrow pandas
maturin build --release -m probe-py/Cargo.toml -o dist
pip install --no-index --find-links dist probe-format
python -m pytest -q probe-py/tests
```

CI runs exactly this, on Linux, macOS and Windows, plus an MSRV check (1.85) and `cargo doc`.

## The rules that are not style

1. **Never edit a file in `tests/golden` or `tests/expected` to make a test pass.** They are the
   conformance vectors; a reader that disagrees with them is wrong, and if the vectors themselves
   are wrong that is a specification change, which is a different conversation. This is the single
   rule most likely to be broken by someone in a hurry, including an agent.
2. **A reader proves itself by the core dump, not by eye.** `probe dump` writes the dump of spec
   section 8.2 and prints its FNV-1a 64 digest. Three implementations — the production writer's
   own decoder, a Go reader and this one — produce identical dumps for every vector and for a
   production hour. A fourth reader is correct when it joins them, and not before.
3. **A codec id, once registered, means those bytes forever.** Better coding arrives as a new id,
   never as a redefinition of an old one. v3 names a codec in every part of the file for exactly
   this reason.
4. **A new format version never changes what an older file means.** Readers must accept every
   earlier version. The next container version, should one ever be needed, is reserved as
   `evmin/7`, with the same `EVM4` magic.
5. **A corrupt or truncated file returns an error and never panics.** There is a fuzz target for
   this; if you add a decode path, it goes through the same bounds discipline.
6. **No codec and no claim without measurements.** Sizes and timings in the documentation come
   from a run on a real hour, stated with the machine. "Faster" without a number does not go in.

## The conventions

- **Rust**: `cargo fmt`, clippy at `-D warnings`, MSRV 1.85 — do not reach for a newer language
  feature without moving the MSRV deliberately and saying so in the changelog.
- **No new dependency without a reason in the pull request.** The reader is deliberately small;
  everything it pulls in, a customer pulls in.
- **Comments say why, not what.** The what is in the code and in the spec.
- **Every change that a user could notice gets a `CHANGELOG.md` entry** under `Unreleased`, in the
  Keep a Changelog sections. A change nobody could notice does not need one.
- **The documentation is executed.** `probe-cli/tests/docs.rs` runs the commands printed in
  `README.md` and `docs/READING.md` against a conformance vector. If you change a command in the
  prose, the test tells you; if you add one, add it in a fenced `sh` block and it is picked up.

## Releasing

Tags are `vX.Y.Z`. Three workflows fire from a tag: `release-crates` publishes `probe-format` and
`probe-cli` to crates.io, `release-pypi` builds and publishes the wheels, and `release-binaries`
builds the `probe` command for Linux, macOS and Windows and attaches the archives and
`SHA256SUMS` to the GitHub release. The changelog's `Unreleased` section becomes the version's
section in the same commit as the version bump, not afterwards.

## Who this is for

Probe is the format [ProbNode](https://probnode.com) stores its order books in, and it is
published so that nobody has to take our data on our word: the specification, the vectors and a
reader are all here, under Apache-2.0. Issues and pull requests from outside are welcome — see
[CONTRIBUTING.md](CONTRIBUTING.md), and [SECURITY.md](SECURITY.md) for anything that should not
be filed in public.
