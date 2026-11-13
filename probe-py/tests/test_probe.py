"""The Python package against the conformance vectors (run from the repository root or anywhere)."""

import csv
import os

import pyarrow as pa
import pytest

import probe_format as pf

GOLDEN = os.path.join(os.path.dirname(__file__), "..", "..", "tests", "golden")
EXPECTED = os.path.join(os.path.dirname(__file__), "..", "..", "tests", "expected")


def golden(name):
    return os.path.join(GOLDEN, name)


@pytest.mark.parametrize("name", ["basic_v1.probe", "basic_v2.probe"])
def test_open_and_counts(name):
    f = pf.open(golden(name))
    assert f.version == ("v1" if "v1" in name else "v2")
    assert f.header["markets"] == 5
    assert len(f.segments()) == 3
    assert f.core_digest() == (384, 1927, "ace341825691e222")
    ev = f.events()
    assert ev.num_rows == 384
    lv = f.levels()
    assert lv.num_rows == 1927
    m = f.markets()
    assert m.num_rows == 5 and m.column("slug").to_pylist()[0] == "mkt-0"


def test_side_streams_v2_one_market_equal_full_read():
    f = pf.open(golden("basic_v2.probe"))
    full = f.events().to_pylist()
    one = f.events(market="mkt-2").to_pylist()
    want = [e for e in full if e["slug"] == "mkt-2"]
    drop = lambda e: {k: v for k, v in e.items() if k != "index"}  # noqa: E731 - no order stream in a one-market read
    assert [drop(e) for e in one] == [drop(e) for e in want]
    assert any(e["tx_hash"] for e in one), "the market's trades carry their tx hashes"


@pytest.mark.parametrize("vec,slug", [("basic_v2", "mkt-2"), ("basic_v1", "mkt-0"), ("vec_shift0", "mkt-1")])
def test_book_max_equals_reference(vec, slug):
    f = pf.open(golden(vec + ".probe"))
    t = f.book_max(market=slug)
    with open(os.path.join(EXPECTED, f"{vec}.{slug}.book-max.csv"), newline="") as fh:
        rows = list(csv.DictReader(fh))
    assert t.num_rows == len(rows)
    assert t.schema.field("price").type == pa.decimal128(18, 6)
    got = t.to_pylist()
    for g, w in zip(got, rows):
        assert g["recv_ns"] == int(w["recv_ns"])
        assert g["seq"] == int(w["seq"]) and g["qual"] == int(w["qual"]) and g["op"] == w["op"]
        assert f"{g['price']:.6f}" == w["price"] and f"{g['size']:.6f}" == w["size"]
        assert g["level"] == int(w["level"])


def test_book_at_and_time_range():
    f = pf.open(golden("basic_v2.probe"))
    with open(golden("basic.books_queries.csv"), newline="") as fh:
        q = next(csv.DictReader(fh))
    b = f.book_at(q["token_id"], int(q["ts_ns"]))
    assert b is None or isinstance(b["bids"], list)
    t0 = f.segments()[1]["t0_ns"]
    ev = f.events(start_ns=t0, end_ns=t0 + 60_000_000_000).to_pylist()
    real = [e for e in ev if e["event_type"] != 5]
    assert real and all(t0 <= e["recv_ns"] < t0 + 60_000_000_000 for e in real)


def test_pandas_round_trip():
    pd = pytest.importorskip("pandas")
    df = pf.open(golden("basic_v1.probe")).book_max().to_pandas()
    assert isinstance(df, pd.DataFrame) and len(df) > 0


def test_unknown_market_and_bad_file(tmp_path):
    f = pf.open(golden("basic_v1.probe"))
    with pytest.raises(KeyError):
        f.events(market="no-such-market")
    bad = tmp_path / "bad.probe"
    bad.write_bytes(b"EVM4\x05\x00\x00\x00{oops")
    with pytest.raises(ValueError):
        pf.open(str(bad))
