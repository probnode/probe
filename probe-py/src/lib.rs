//! The native module `probe_format._probe`. The Python package wraps it: every table crosses as
//! an Arrow IPC stream, which `pyarrow` reads without a copy of the column buffers.

use probe_format::arrow::{
    book_max_batch, book_max_schema, events_batch, events_schema, ipc_stream_bytes, levels_batch,
    levels_schema, markets_batch, BATCH_ROWS,
};
use probe_format::bookmax::BookMax;
use probe_format::{Event, File, Query};
use pyo3::exceptions::{PyIOError, PyKeyError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyBytes;

fn err(e: probe_format::Error) -> PyErr {
    match e {
        probe_format::Error::Io(e) => PyIOError::new_err(e.to_string()),
        probe_format::Error::NotFound(m) => PyKeyError::new_err(m),
        other => PyValueError::new_err(other.to_string()),
    }
}

fn arrow_err(e: impl std::fmt::Display) -> PyErr {
    PyValueError::new_err(format!("arrow: {}", e))
}

/// An open Probe file.
#[pyclass(name = "File", module = "probe_format._probe", frozen)]
struct PyFile {
    inner: File,
}

impl PyFile {
    fn query(
        &self,
        market: Option<&str>,
        start_ns: Option<i64>,
        end_ns: Option<i64>,
        side_streams: bool,
    ) -> PyResult<Query<'_>> {
        let mut q = self.inner.query().side_streams(side_streams);
        if let Some(m) = market {
            q = q.market(m).map_err(err)?;
        }
        if let Some(t) = start_ns {
            q = q.from_ns(t);
        }
        if let Some(t) = end_ns {
            q = q.to_ns(t);
        }
        Ok(q)
    }

    fn collect(q: &Query) -> PyResult<Vec<Event>> {
        q.events()
            .collect::<probe_format::Result<Vec<_>>>()
            .map_err(err)
    }
}

#[pymethods]
impl PyFile {
    #[new]
    fn new(path: &str) -> PyResult<Self> {
        Ok(PyFile {
            inner: File::open(path).map_err(err)?,
        })
    }

    /// "v1" or "v2".
    #[getter]
    fn version(&self) -> String {
        self.inner.version().to_string()
    }

    /// The header as a JSON object string.
    fn header_json(&self) -> String {
        let h = self.inner.header();
        format!(
            "{{\"version\":\"{}\",\"header_version\":\"{}\",\"recorder\":{:?},\"unit_ns\":{},\"seg_s\":{},\"shift\":{},\"flags\":{},\"has_hash\":{},\"hour_start_ns\":{},\"bytes\":{},\"markets\":{},\"segments\":{}}}",
            self.inner.version(),
            h.version.header_str(),
            h.recorder,
            h.unit_ns,
            h.seg_s,
            h.shift,
            h.flags,
            h.has_hash,
            h.hour_start_ns,
            self.inner.size(),
            self.inner.markets().len(),
            self.inner.segments().len()
        )
    }

    /// The segments: (t0_ns, nmkt, blocks, n_ev, n_lv, n_ck).
    #[allow(clippy::type_complexity)]
    fn segments(&self) -> Vec<(i64, usize, usize, usize, usize, usize)> {
        self.inner
            .segments()
            .iter()
            .map(|s| (s.t0_ns, s.nmkt, s.blocks.len(), s.n_ev, s.n_lv, s.n_ck))
            .collect()
    }

    /// The markets table as an Arrow IPC stream.
    fn markets_ipc<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        let b = markets_batch(&self.inner).map_err(arrow_err)?;
        let buf = ipc_stream_bytes(&b.schema(), &[b]).map_err(arrow_err)?;
        Ok(PyBytes::new(py, &buf))
    }

    /// The selected events as an Arrow IPC stream.
    #[pyo3(signature = (market=None, start_ns=None, end_ns=None, side_streams=true))]
    fn events_ipc<'py>(
        &self,
        py: Python<'py>,
        market: Option<&str>,
        start_ns: Option<i64>,
        end_ns: Option<i64>,
        side_streams: bool,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let q = self.query(market, start_ns, end_ns, side_streams)?;
        let buf = py.allow_threads(|| -> PyResult<Vec<u8>> {
            let evs = Self::collect(&q)?;
            let mut batches = Vec::new();
            for c in evs.chunks(BATCH_ROWS) {
                batches.push(events_batch(&self.inner, c).map_err(arrow_err)?);
            }
            ipc_stream_bytes(&events_schema(), &batches).map_err(arrow_err)
        })?;
        Ok(PyBytes::new(py, &buf))
    }

    /// The levels of the selected events as an Arrow IPC stream.
    #[pyo3(signature = (market=None, start_ns=None, end_ns=None, side_streams=true))]
    fn levels_ipc<'py>(
        &self,
        py: Python<'py>,
        market: Option<&str>,
        start_ns: Option<i64>,
        end_ns: Option<i64>,
        side_streams: bool,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let q = self.query(market, start_ns, end_ns, side_streams)?;
        let buf = py.allow_threads(|| -> PyResult<Vec<u8>> {
            let evs = Self::collect(&q)?;
            let mut batches = Vec::new();
            for c in evs.chunks(4096) {
                batches.push(levels_batch(&self.inner, c).map_err(arrow_err)?);
            }
            ipc_stream_bytes(&levels_schema(), &batches).map_err(arrow_err)
        })?;
        Ok(PyBytes::new(py, &buf))
    }

    /// The book-max rows of the selected events as an Arrow IPC stream.
    #[pyo3(signature = (market=None, start_ns=None, end_ns=None))]
    fn book_max_ipc<'py>(
        &self,
        py: Python<'py>,
        market: Option<&str>,
        start_ns: Option<i64>,
        end_ns: Option<i64>,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let q = self.query(market, start_ns, end_ns, false)?;
        let buf = py.allow_threads(|| -> PyResult<Vec<u8>> {
            let mut bm = BookMax::new(&self.inner);
            let mut rows = Vec::new();
            let mut batches = Vec::new();
            for ev in q.events() {
                bm.event(&ev.map_err(err)?, &mut rows);
                if rows.len() >= BATCH_ROWS {
                    batches.push(book_max_batch(&rows).map_err(arrow_err)?);
                    rows.clear();
                }
            }
            if !rows.is_empty() {
                batches.push(book_max_batch(&rows).map_err(arrow_err)?);
            }
            ipc_stream_bytes(&book_max_schema(), &batches).map_err(arrow_err)
        })?;
        Ok(PyBytes::new(py, &buf))
    }

    /// The book of a token at a receive time: (segment, row, bids, asks) with levels as
    /// (price_micro, size_micro) ascending, or None when the file holds no book of the token then.
    #[allow(clippy::type_complexity)]
    fn book_at(
        &self,
        token: &str,
        ts_ns: i64,
    ) -> PyResult<Option<(usize, usize, Vec<(i64, i64)>, Vec<(i64, i64)>)>> {
        Ok(self
            .inner
            .book_at(token, ts_ns)
            .map_err(err)?
            .map(|b| (b.segment, b.row, b.bids, b.asks)))
    }

    /// The core dump digest: (events, levels, fnv64 as 16 hex digits).
    fn core_digest(&self) -> PyResult<(u64, u64, String)> {
        let s = probe_format::core_dump(&self.inner, None).map_err(err)?;
        Ok((s.events, s.levels, format!("{:016x}", s.fnv64)))
    }
}

/// The native part of probe_format.
#[pymodule]
fn _probe(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyFile>()?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
