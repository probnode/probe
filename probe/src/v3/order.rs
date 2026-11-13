//! The reference order of Probe v3 (order codec 17, specification 9.3): the real events of a
//! segment sorted by (receive clock, event index in block order), which both sides compute from
//! the decoded blocks and call the canonical order, plus the few events the venue delivered late,
//! as deferrals.
//!
//! Decoding walks the canonical order with an output position t. A deferral names the canonical
//! position of an event and how many positions later it is emitted (d >= 1): the event waits until
//! output position t + d. At every t a waiting event whose time has come is emitted first,
//! otherwise the next canonical event. The encoder derives the deferrals from the arrival order
//! (d = arrival position - t when the event is reached early); every order has such a form.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

/// The canonical order: gs (ascending event indices) sorted stably by ts.
pub(crate) fn canonical(ts: Vec<i64>, gs: Vec<u32>) -> Vec<u32> {
    let n = gs.len();
    if n == 0 || ts.len() != n {
        return gs;
    }
    let tmin = ts.iter().copied().min().unwrap_or(0);
    let tmax = ts.iter().copied().max().unwrap_or(0);
    let range = tmax.wrapping_sub(tmin) as u64;
    let tbits = 64 - range.leading_zeros();
    let gmax = gs.iter().copied().max().unwrap_or(0) as u64;
    let gbits = 64 - gmax.leading_zeros();
    if tbits + gbits > 64 {
        let mut v: Vec<(i64, u32)> = ts.into_iter().zip(gs).collect();
        v.sort_unstable();
        return v.into_iter().map(|x| x.1).collect();
    }
    // packed (ts - tmin) << gbits | g; a stable LSD radix sort on the ts digits only keeps the
    // initial (ascending g) order among equal clocks
    let mut a: Vec<u64> = ts
        .iter()
        .zip(&gs)
        .map(|(&t, &g)| ((t.wrapping_sub(tmin) as u64) << gbits) | g as u64)
        .collect();
    drop(ts);
    drop(gs);
    let mut b = vec![0u64; n];
    const D: u32 = 11;
    let mut shift = gbits;
    let top = gbits + tbits;
    let mut cnt = vec![0usize; 1 << D];
    while shift < top {
        let mask = (1u64 << D) - 1;
        cnt.iter_mut().for_each(|c| *c = 0);
        for &x in &a {
            cnt[((x >> shift) & mask) as usize] += 1;
        }
        let mut s = 0;
        for c in cnt.iter_mut() {
            let k = *c;
            *c = s;
            s += k;
        }
        for &x in &a {
            let k = ((x >> shift) & mask) as usize;
            b[cnt[k]] = x;
            cnt[k] += 1;
        }
        std::mem::swap(&mut a, &mut b);
        shift += D;
    }
    let gm = if gbits == 0 {
        0
    } else {
        u64::MAX >> (64 - gbits)
    };
    a.into_iter().map(|x| (x & gm) as u32).collect()
}

/// The deferrals turning canon into arrival (arrival[t] = the event index emitted at t), as
/// (run, d - 1) columns: run = canonical positions since the previous deferral.
pub(crate) fn deferrals(
    canon: &[u32],
    arrival: &[u32],
    max_g: usize,
) -> Result<(Vec<u64>, Vec<u64>), String> {
    let n = canon.len();
    if arrival.len() != n {
        return Err("order: arrival and canonical lengths differ".into());
    }
    let mut pos = vec![u32::MAX; max_g + 1];
    for (t, &g) in arrival.iter().enumerate() {
        let slot = pos
            .get_mut(g as usize)
            .ok_or("order: event index out of range")?;
        *slot = t as u32;
    }
    let mut target = vec![false; n];
    let (mut runs, mut ds) = (Vec::new(), Vec::new());
    let mut p = 0usize;
    let mut last: i64 = -1;
    let mut t = 0usize;
    while t < n {
        if target[t] {
            t += 1;
            continue;
        }
        loop {
            if p >= n {
                return Err("order: canonical order exhausted".into());
            }
            let a = *pos.get(canon[p] as usize).unwrap_or(&u32::MAX);
            if a == u32::MAX {
                return Err("order: event missing from the arrival order".into());
            }
            let a = a as usize;
            if a == t {
                p += 1;
                break;
            }
            if a < t || target[a] {
                return Err("order: event behind the output position".into());
            }
            target[a] = true;
            runs.push((p as i64 - last - 1) as u64);
            ds.push((a - t - 1) as u64);
            last = p as i64;
            p += 1;
        }
        t += 1;
    }
    Ok((runs, ds))
}

/// Applies deferrals to the canonical order; returns the arrival order.
pub(crate) fn arrival(canon: &[u32], runs: &[u64], ds: &[u64]) -> Result<Vec<u32>, String> {
    let n = canon.len();
    if runs.len() != ds.len() || runs.len() > n {
        return Err("order: bad deferral list".into());
    }
    let mut out = Vec::with_capacity(n);
    let mut pend: BinaryHeap<Reverse<(u64, u32)>> = BinaryHeap::new();
    let mut p = 0usize;
    let mut di = 0usize;
    let mut next_def = if runs.is_empty() {
        usize::MAX
    } else {
        usize::try_from(runs[0]).unwrap_or(usize::MAX)
    };
    while out.len() < n {
        let t = out.len() as u64;
        if let Some(&Reverse((tg, g))) = pend.peek() {
            if tg == t {
                pend.pop();
                out.push(g);
                continue;
            }
            if tg < t {
                return Err("order: deferral target passed".into());
            }
        }
        if p >= n {
            return Err("order: canonical order exhausted".into());
        }
        if p == next_def {
            pend.push(Reverse((
                t.saturating_add(ds[di]).saturating_add(1),
                canon[p],
            )));
            di += 1;
            next_def = if di < runs.len() {
                p.saturating_add(1)
                    .saturating_add(usize::try_from(runs[di]).unwrap_or(usize::MAX))
            } else {
                usize::MAX
            };
            p += 1;
            continue;
        }
        out.push(canon[p]);
        p += 1;
    }
    if p != n || di != runs.len() || !pend.is_empty() {
        return Err("order: deferrals left over".into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deferrals_roundtrip() {
        let ts: Vec<i64> = vec![5, 9, 9, 12, 3, 9, 20, 1, 9, 9];
        let gs: Vec<u32> = (0..10).collect();
        let canon = canonical(ts.clone(), gs.clone());
        let mut sorted = gs.clone();
        sorted.sort_by_key(|&g| (ts[g as usize], g));
        assert_eq!(canon, sorted);
        let arrivals: [Vec<u32>; 3] = [
            sorted.clone(),
            vec![9, 8, 7, 6, 5, 4, 3, 2, 1, 0],
            vec![4, 7, 0, 1, 2, 3, 6, 5, 8, 9],
        ];
        for arr in arrivals {
            let (runs, ds) = deferrals(&canon, &arr, 9).unwrap();
            assert_eq!(arrival(&canon, &runs, &ds).unwrap(), arr);
        }
        // the radix path with a wide clock range
        let ts: Vec<i64> = (0..3000)
            .map(|i| ((i * 7919) % 1000) as i64 * 1_000_003 - 5)
            .collect();
        let gs: Vec<u32> = (0..3000).collect();
        let canon = canonical(ts.clone(), gs.clone());
        let mut sorted = gs.clone();
        sorted.sort_by_key(|&g| (ts[g as usize], g));
        assert_eq!(canon, sorted);
        // the comparison-sort path (clock range + index bits above 64)
        let ts: Vec<i64> = vec![i64::MAX, i64::MIN, 0, 7];
        let canon = canonical(ts, vec![0, 1, 2, 3]);
        assert_eq!(canon, vec![1, 2, 3, 0]);
    }

    #[test]
    fn bad_deferrals_are_errors() {
        let canon: Vec<u32> = (0..5).collect();
        assert!(arrival(&canon, &[0], &[u64::MAX]).is_err());
        assert!(arrival(&canon, &[u64::MAX], &[0]).is_err());
        assert!(arrival(&canon, &[0, 0, 0, 0, 0, 0], &[0; 6]).is_err());
        assert!(arrival(&canon, &[4], &[3]).is_err());
    }
}
