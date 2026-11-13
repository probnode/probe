# Contributing to Probe

Thank you for helping. Bug reports, new readers in other languages, performance work and
corrections to the specification are all welcome.

## Ground rules

- **The specification is the contract.** A change to how a byte decodes is a new format version
  (spec section 2.2), never a change to v1 or v2. A pull request that changes decoding output must
  say which of the two it is: a reader bug fix (the spec already says so), or a spec change.
- **Conformance is byte for byte.** Every reader must reproduce `tests/golden/*.events.csv`,
  `tests/golden/*.books.csv` and `tests/expected/*.core.csv` exactly. Never edit those files to
  make a test pass. New vectors are added, never rewritten.
- **No panics on input.** Corrupt or truncated files must return an error. If you find an input
  that panics, that is a bug worth a test case (`corrupt_input_never_panics`).

## Development

You need a stable Rust toolchain and, for the Python package, Python 3.9+ and
[maturin](https://www.maturin.rs).

```sh
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo doc -p probe-format --all-features --no-deps

pip install maturin pytest pyarrow pandas
maturin develop -m probe-py/Cargo.toml
pytest probe-py/tests
```

CI runs the same on Linux, macOS and Windows.

## Pull requests

- One topic per pull request, with a test that fails without the change.
- Keep public items documented (`#![warn(missing_docs)]`).
- Performance changes: state the machine, the file and the before / after numbers. The core dump
  digest must not change.
- Add a line to `CHANGELOG.md` under `[Unreleased]`.

## Writing another reader

Start from [spec/PROBE.md](spec/PROBE.md) sections 3 and 5, decode `tests/golden/basic_v1.probe`
and compare your core dump (spec 8.2) with `tests/expected/basic_v1.core.csv`. The `vec_*`
vectors each isolate one rule that is easy to get wrong; section 8.1 names the rule. We are happy
to link third-party readers from the README.

## Code of conduct

This project follows the [Contributor Covenant](CODE_OF_CONDUCT.md). By participating you agree
to uphold it.

## License

By contributing you agree that your contributions are licensed under the Apache License 2.0, as
the project is (section 5 of the license).
