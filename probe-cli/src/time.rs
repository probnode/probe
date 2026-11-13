//! Times on the command line: ns since the epoch, or RFC 3339 in UTC.

/// Days from 1970-01-01 to a civil date (proleptic Gregorian).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// ns since the epoch as `YYYY-MM-DDTHH:MM:SS[.fffffffff]Z`.
pub fn format(ns: i64) -> String {
    let secs = ns.div_euclid(1_000_000_000);
    let frac = ns.rem_euclid(1_000_000_000);
    let (y, m, d) = civil_from_days(secs.div_euclid(86400));
    let s = secs.rem_euclid(86400);
    let mut out = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        y,
        m,
        d,
        s / 3600,
        s / 60 % 60,
        s % 60
    );
    if frac != 0 {
        let f = format!("{:09}", frac);
        out.push('.');
        out.push_str(f.trim_end_matches('0'));
    }
    out.push('Z');
    out
}

fn num(s: &str, what: &str) -> Result<i64, String> {
    if s.is_empty() || !s.bytes().all(|c| c.is_ascii_digit()) {
        return Err(format!("bad {} {:?}", what, s));
    }
    s.parse::<i64>()
        .map_err(|_| format!("bad {} {:?}", what, s))
}

/// Parses ns since the epoch (an integer), or `YYYY-MM-DD[THH:MM[:SS[.f]]](Z|+00:00)`.
pub fn parse(s: &str) -> Result<i64, String> {
    let s = s.trim();
    if let Ok(v) = s.parse::<i64>() {
        return Ok(v);
    }
    let err = || {
        format!(
            "time {:?}: want ns since the epoch or RFC 3339 in UTC (2026-11-12T10:05:00Z)",
            s
        )
    };
    let body = s
        .strip_suffix('Z')
        .or_else(|| s.strip_suffix("+00:00"))
        .unwrap_or(s);
    let (date, clock) = match body.split_once(['T', ' ']) {
        Some((d, c)) => (d, Some(c)),
        None => (body, None),
    };
    let dp: Vec<&str> = date.split('-').collect();
    if dp.len() != 3 {
        return Err(err());
    }
    let (y, mo, d) = (
        num(dp[0], "year")?,
        num(dp[1], "month")?,
        num(dp[2], "day")?,
    );
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return Err(err());
    }
    let mut ns = days_from_civil(y, mo, d) * 86_400 * 1_000_000_000;
    if let Some(c) = clock {
        let (hms, frac) = match c.split_once('.') {
            Some((a, b)) => (a, Some(b)),
            None => (c, None),
        };
        let tp: Vec<&str> = hms.split(':').collect();
        if tp.len() < 2 || tp.len() > 3 {
            return Err(err());
        }
        let h = num(tp[0], "hour")?;
        let mi = num(tp[1], "minute")?;
        let se = if tp.len() == 3 {
            num(tp[2], "second")?
        } else {
            0
        };
        if h > 23 || mi > 59 || se > 60 {
            return Err(err());
        }
        ns += (h * 3600 + mi * 60 + se) * 1_000_000_000;
        if let Some(f) = frac {
            if f.is_empty() || f.len() > 9 || !f.bytes().all(|c| c.is_ascii_digit()) {
                return Err(err());
            }
            ns += num(f, "fraction")? * 10i64.pow(9 - f.len() as u32);
        }
    }
    Ok(ns)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let t = 1_794_477_600_000_000_000; // 2026-11-12T10:00:00Z
        assert_eq!(format(t), "2026-11-12T10:00:00Z");
        assert_eq!(parse("2026-11-12T10:00:00Z").unwrap(), t);
        assert_eq!(parse("2026-11-12T10:00Z").unwrap(), t);
        assert_eq!(parse("2026-11-12T10:00:00.5Z").unwrap(), t + 500_000_000);
        assert_eq!(parse("2026-11-12").unwrap(), t - 10 * 3600 * 1_000_000_000);
        assert_eq!(parse(&t.to_string()).unwrap(), t);
        assert_eq!(format(t + 1_000), "2026-11-12T10:00:00.000001Z");
        assert!(parse("2026-13-01").is_err());
        assert!(parse("yesterday").is_err());
    }
}
