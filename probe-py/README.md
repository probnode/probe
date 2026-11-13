# probe-format (Python)

Read [Probe](https://github.com/probnode/probe) prediction-market order-book files into
[pyarrow](https://arrow.apache.org/docs/python/) tables, and from there into pandas or polars.
The decoding is the Rust crate [`probe-format`](https://crates.io/crates/probe-format); tables
cross into Python as Arrow IPC streams.

```sh
pip install probe-format
```

```python
import probe_format as pf

f = pf.open("hour.probe")
print(f.version, f.header["markets"], "markets")

book = f.book_max(market="will-x-happen")      # pyarrow.Table, one row per level change
df = book.to_pandas()                          # pandas
pl_df = pf.to_polars(book)                     # polars (pip install polars)

events = f.events(start_ns=f.header["hour_start_ns"],
                  end_ns=f.header["hour_start_ns"] + 600_000_000_000)
levels = f.levels(market="will-x-happen")      # best-price exceptions and book hashes included
f.book_at("7001", 1794477900000000000)          # {"bids": [(price_micro, size_micro), ...], ...}
f.transcode("hour.v3.probe")                   # Probe v3 (profile "fast"), losslessly
```

It reads Probe v1, v2 and v3.

| method | rows |
|---|---|
| `markets()` | the markets table: slug, condition id, tokens, units |
| `events(market, start_ns, end_ns)` | one per event: type, clocks, fee, tx hash, payload, book hash |
| `levels(market, start_ns, end_ns)` | one per level: token, side, price, size, mirror, best-price exception |
| `book_max(market, start_ns, end_ns)` | `book-max`: every level change, checkpoint level and trade with `seq`, `qual`, `op`, `level` |
| `book_at(token, ts_ns)` | a token's book at a receive time |

## Building from source

```sh
pip install maturin
maturin develop -m probe-py/Cargo.toml     # from the repository root
pytest probe-py/tests
```
