//! The full dump: every event and level of a file with every side stream, in the layout of the
//! conformance vectors' `events.csv` (specification 8.2).

use crate::decoder::floor_div;
use crate::error::Result;
use crate::event::EventType;
use crate::file::File;
use std::io::Write;

/// Lower-case hex of `b`, appended to `out`.
pub fn push_hex(out: &mut String, b: &[u8]) {
    const H: &[u8; 16] = b"0123456789abcdef";
    for &c in b {
        out.push(H[(c >> 4) as usize] as char);
        out.push(H[(c & 15) as usize] as char);
    }
}

/// Appends a CSV field: quoted, with `"` doubled, only when it holds `,`, `"`, CR or LF.
pub fn push_csv(out: &mut String, s: &str) {
    if !s.contains([',', '"', '\r', '\n']) {
        out.push_str(s);
        return;
    }
    out.push('"');
    out.push_str(&s.replace('"', "\"\""));
    out.push('"');
}

/// Writes the full dump of `f` to `out`:
///
/// ```text
/// E,seg,ev,et,row,out,cont,ts_recv_us,ts_ex_ms,aux,unsynced,extra
/// L,seg,ev,lv,token_id,side,price_micro,size_micro,mirror,best_bid,best_ask,hash
/// ```
///
/// `extra` is the payload (types 3, 4), the transaction hash (type 2, hex) or the book hash
/// (type 0, hex, with hashes). A level's `best_bid` / `best_ask` are the stored exceptions and
/// `hash` its book hash (type 1, with hashes). UTF-8, LF line ends, no header.
pub fn write_dump(f: &File, out: &mut dyn Write) -> Result<()> {
    let unit = f.header.unit_ns;
    let mut line = String::with_capacity(1 << 16);
    for si in 0..f.segments.len() {
        let evs = f.segment_events(si)?;
        for e in &evs {
            let idx = e.index.unwrap_or(0);
            let core_ts = e.recv_ns.map_or(0, |t| t / unit);
            let tns = core_ts.wrapping_mul(unit);
            let ts_ex = match e.venue_ms {
                Some(v) => v,
                None => floor_div(tns, 1_000_000),
            };
            line.push_str(&format!(
                "E,{},{},{},{},{},{},{},{},{},{},",
                si,
                idx,
                e.kind.as_u8(),
                e.row,
                e.outcome,
                e.cont as u8,
                floor_div(tns, 1000),
                ts_ex,
                e.fee_rate_bps,
                e.unsynced as u8
            ));
            if matches!(e.kind, EventType::TickSize | EventType::Other) {
                push_csv(&mut line, e.payload.as_deref().unwrap_or(""));
            } else if let Some(t) = &e.tx_hash {
                push_hex(&mut line, t);
            } else if let Some(h) = &e.book_hash {
                push_hex(&mut line, h);
            }
            line.push('\n');
            for (i, l) in e.levels.iter().enumerate() {
                line.push_str(&format!("L,{},{},{},", si, idx, i));
                push_csv(&mut line, f.token(si, e.row, l.outcome));
                line.push_str(&format!(
                    ",{},{},{},{},",
                    l.side.venue_str(),
                    l.price_micro,
                    l.size_micro,
                    l.mirror as u8
                ));
                match &l.best {
                    Some((bb, ba)) => {
                        push_csv(&mut line, bb);
                        line.push(',');
                        push_csv(&mut line, ba);
                    }
                    None => line.push(','),
                }
                line.push(',');
                if let Some(h) = &l.book_hash {
                    push_hex(&mut line, h);
                }
                line.push('\n');
            }
            if line.len() > 1 << 15 {
                out.write_all(line.as_bytes())?;
                line.clear();
            }
        }
    }
    out.write_all(line.as_bytes())?;
    Ok(())
}
