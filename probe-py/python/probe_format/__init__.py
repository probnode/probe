"""Read Probe order-book files into Arrow, pandas and polars.

    import probe_format as pf

    f = pf.open("hour.probe")
    f.version                      # "v1", "v2" or "v3"
    f.markets()                    # pyarrow.Table of the markets table
    t = f.book_max(market="will-x-happen")   # every level change, checkpoint level and trade
    df = t.to_pandas()             # or pf.to_polars(t)
    f.events(start_ns=..., end_ns=...)        # one row per event, side streams included
    f.levels(market="will-x-happen")          # one row per level
    f.book_at("7001", ts_ns)       # a token's book at a time

Times are ns since the epoch on the receive clock. Prices and sizes are decimal(18, 6).
"""

from __future__ import annotations

import json
from typing import Any, Optional

import pyarrow as pa
import pyarrow.ipc

from . import _probe

__all__ = ["File", "open", "to_polars", "__version__"]

__version__: str = _probe.__version__


def _table(buf: bytes) -> pa.Table:
    return pa.ipc.open_stream(pa.py_buffer(buf)).read_all()


class File:
    """An open Probe file (v1, v2 or v3). The file is memory-mapped; tables are decoded on request."""

    def __init__(self, path: str) -> None:
        self._f = _probe.File(str(path))
        self.path = str(path)

    @property
    def version(self) -> str:
        """``"v1"`` (header ``evmin/4``), ``"v2"`` (``evmin/5``) or ``"v3"`` (``evmin/6``)."""
        return self._f.version

    def transcode(self, path: str, profile: str = "fast") -> int:
        """Write this file as Probe v3 to ``path``, losslessly; ``profile`` is ``"fast"`` (the
        default), ``"fast1"`` or ``"rc1"``. Returns the bytes written."""
        return self._f.transcode(str(path), profile)

    @property
    def header(self) -> dict[str, Any]:
        """The header: version, coding parameters, hour start, counts."""
        return json.loads(self._f.header_json())

    def segments(self) -> list[dict[str, int]]:
        """The 5-minute segments with their counts."""
        keys = ("t0_ns", "nmkt", "blocks", "n_ev", "n_lv", "n_ck")
        return [dict(zip(keys, s)) for s in self._f.segments()]

    def markets(self) -> pa.Table:
        """The markets table: one row per market row, with its tokens and units."""
        return _table(self._f.markets_ipc())

    def events(self, market: Optional[str] = None, start_ns: Optional[int] = None,
               end_ns: Optional[int] = None, side_streams: bool = True) -> pa.Table:
        """One row per event in stored order, with tx hashes, payloads and book hashes.

        ``market`` is a slug, condition id or row number; ``start_ns`` / ``end_ns`` keep real
        events received in ``[start_ns, end_ns)`` (checkpoints of the segments read are kept).
        """
        return _table(self._f.events_ipc(market, start_ns, end_ns, side_streams))

    def levels(self, market: Optional[str] = None, start_ns: Optional[int] = None,
               end_ns: Optional[int] = None, side_streams: bool = True) -> pa.Table:
        """One row per level, with its event's columns and best-price exceptions."""
        return _table(self._f.levels_ipc(market, start_ns, end_ns, side_streams))

    def book_max(self, market: Optional[str] = None, start_ns: Optional[int] = None,
                 end_ns: Optional[int] = None) -> pa.Table:
        """The ``book-max`` table: every level change, checkpoint level and trade, with
        ``seq``, ``qual``, ``op`` and the level's position ``level``."""
        return _table(self._f.book_max_ipc(market, start_ns, end_ns))

    def book_at(self, token: str, ts_ns: int) -> Optional[dict[str, Any]]:
        """The token's book at receive time ``ts_ns``: bids and asks as (price, size) in micro
        units, ascending price; ``None`` when the file holds no book of the token then."""
        r = self._f.book_at(str(token), int(ts_ns))
        if r is None:
            return None
        seg, row, bids, asks = r
        return {"token": str(token), "segment": seg, "row": row, "bids": bids, "asks": asks}

    def core_digest(self) -> tuple[int, int, str]:
        """(events, levels, FNV-1a 64 hex) of the conformance core dump."""
        return self._f.core_digest()

    def __repr__(self) -> str:
        h = self.header
        return f"probe_format.File({self.path!r}, {h['version']}, {h['markets']} markets, {h['segments']} segments)"


def open(path: str) -> File:  # noqa: A001 - mirrors the builtin on purpose
    """Opens a Probe file."""
    return File(path)


def to_polars(table: pa.Table):
    """A polars DataFrame of an Arrow table (needs ``polars``)."""
    import polars as pl

    return pl.from_arrow(table)
