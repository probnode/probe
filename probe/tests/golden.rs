//! The conformance vectors of `tests/golden` and the outputs of the Go reference decoder in
//! `tests/expected`.

use probe_format::bookmax::{push_book_max_csv, BookMax, BOOK_MAX_COLUMNS};
use probe_format::dump::write_dump;
use probe_format::{core_dump, Error, EventType, File, Version};

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../tests/");

fn golden(name: &str) -> String {
    format!("{}golden/{}", ROOT, name)
}

fn expected(name: &str) -> String {
    format!("{}expected/{}", ROOT, name)
}

fn read(p: &str) -> Vec<u8> {
    std::fs::read(p).unwrap_or_else(|e| panic!("{}: {}", p, e))
}

/// (file, events.csv, events, levels, core-dump FNV-1a 64 of the Go reference)
const VECTORS: [(&str, &str, u64, u64, u64); 10] = [
    ("basic_v1", "basic", 384, 1927, 0xace341825691e222),
    ("basic_v2", "basic", 384, 1927, 0xace341825691e222),
    ("units_hour_a", "units_hour_a", 61, 237, 0xd85b70621dd973d8),
    ("units_hour_b", "units_hour_b", 61, 285, 0xd39ccc3ba34af669),
    (
        "vec_dts_bucket",
        "vec_dts_bucket",
        812,
        3806,
        0x7ce2bed76353c514,
    ),
    ("vec_flags0", "vec_flags0", 812, 3789, 0xd33d4a230dffd4fb),
    ("vec_shift0", "vec_shift0", 812, 3876, 0x6bb5bd17417fe6b6),
    // shift 0 with probabilities low enough to need two renormalisation steps in one bit
    (
        "vec_renorm_shift0",
        "vec_renorm_shift0",
        9004,
        34157,
        0x7edc63928a7c4cbd,
    ),
    ("vec_shift5", "vec_shift5", 812, 3955, 0x99ed96042c8f5c0c),
    (
        "vec_size_cache",
        "vec_size_cache",
        812,
        3413,
        0x387bd740338b7e8b,
    ),
];

fn first_diff(got: &[u8], want: &[u8]) -> String {
    let g = String::from_utf8_lossy(got);
    let w = String::from_utf8_lossy(want);
    for (i, (a, b)) in g.lines().zip(w.lines()).enumerate() {
        if a != b {
            return format!("line {}:\n got  {}\n want {}", i + 1, a, b);
        }
    }
    format!(
        "lengths differ: {} lines vs {}",
        g.lines().count(),
        w.lines().count()
    )
}

#[test]
fn core_dump_digests_equal_go() {
    for (name, _, ev, lv, fnv) in VECTORS {
        let mut f = File::open(golden(&format!("{}.probe", name))).unwrap();
        for fast in [true, false] {
            f.set_order_fast(fast);
            let s = core_dump(&f, None).unwrap();
            assert_eq!(
                (s.events, s.levels, s.fnv64),
                (ev, lv, fnv),
                "{} order_fast {}",
                name,
                fast
            );
        }
    }
}

#[test]
fn core_dump_bytes_equal_go() {
    for (name, ..) in VECTORS {
        let f = File::open(golden(&format!("{}.probe", name))).unwrap();
        let mut got = Vec::new();
        core_dump(&f, Some(&mut got)).unwrap();
        let want = read(&expected(&format!("{}.core.csv", name)));
        assert!(got == want, "{}: {}", name, first_diff(&got, &want));
    }
}

/// The full dump, every side stream included (tx hashes, payloads, best-price exceptions, book
/// hashes), equals the vectors' events.csv byte for byte.
#[test]
fn full_dump_equals_reference() {
    for (name, ev, ..) in VECTORS {
        let f = File::open(golden(&format!("{}.probe", name))).unwrap();
        let mut got = Vec::new();
        write_dump(&f, &mut got).unwrap();
        let want = read(&golden(&format!("{}.events.csv", ev)));
        assert!(got == want, "{}: {}", name, first_diff(&got, &want));
    }
}

#[test]
fn side_streams_are_present() {
    let f = File::open(golden("basic_v2.probe")).unwrap();
    let evs: Vec<_> = f.events().collect::<Result<_, _>>().unwrap();
    let trades = evs.iter().filter(|e| e.kind == EventType::Trade).count();
    assert!(trades > 0);
    assert!(evs
        .iter()
        .filter(|e| e.kind == EventType::Trade)
        .all(|e| e.tx_hash.is_some()));
    assert!(evs
        .iter()
        .filter(|e| matches!(e.kind, EventType::TickSize | EventType::Other))
        .all(|e| e.payload.is_some()));
    assert!(
        evs.iter().any(|e| e.kind == EventType::Other),
        "type-4 events"
    );
    assert!(
        evs.iter().flat_map(|e| &e.levels).any(|l| l.best.is_some()),
        "best-price exceptions"
    );
    assert!(
        evs.iter()
            .flat_map(|e| &e.levels)
            .any(|l| l.best.is_some() && l.mirror),
        "an exception on a mirror"
    );
    assert!(evs
        .iter()
        .filter(|e| e.kind == EventType::Book)
        .all(|e| e.book_hash.is_some()));
    assert!(evs
        .iter()
        .filter(|e| e.kind == EventType::PriceChange)
        .flat_map(|e| &e.levels)
        .all(|l| l.book_hash.is_some()));
    assert!(evs.iter().any(|e| e.unsynced));
}

#[test]
fn v2_decodes_to_the_same_events_as_v1() {
    let a = File::open(golden("basic_v1.probe")).unwrap();
    let b = File::open(golden("basic_v2.probe")).unwrap();
    assert_eq!(a.version(), Version::V1);
    assert_eq!(b.version(), Version::V2);
    let ea: Vec<_> = a.events().collect::<Result<_, _>>().unwrap();
    let eb: Vec<_> = b.events().collect::<Result<_, _>>().unwrap();
    assert_eq!(ea, eb);
}

/// One market through the block index equals the full decode filtered to the market, side
/// streams included: in v2 from the block's runs, in v1 by decoding the segments.
#[test]
fn one_market_equals_full_decode() {
    for name in ["basic_v1", "basic_v2", "vec_shift5", "units_hour_a"] {
        let f = File::open(golden(&format!("{}.probe", name))).unwrap();
        let full: Vec<_> = f.events().collect::<Result<_, _>>().unwrap();
        let mut slugs: Vec<&str> = f.markets().iter().map(|m| m.slug.as_str()).collect();
        slugs.dedup();
        for slug in slugs {
            let rows = f.market_rows(slug).unwrap();
            let want: Vec<_> = full
                .iter()
                .filter(|e| e.kind != EventType::Other && rows.contains(&e.row))
                .cloned()
                .collect();
            let strip = |mut e: probe_format::Event| {
                e.index = None;
                e
            };
            let got: Vec<_> = f
                .query()
                .market(slug)
                .unwrap()
                .events()
                .collect::<Result<_, _>>()
                .unwrap();
            assert_eq!(
                got.into_iter().map(strip).collect::<Vec<_>>(),
                want.clone().into_iter().map(strip).collect::<Vec<_>>(),
                "{} {}",
                name,
                slug
            );
            // without side streams, the blocks alone: the same events and levels, side fields empty
            let bare: Vec<_> = f
                .query()
                .market(slug)
                .unwrap()
                .side_streams(false)
                .events()
                .collect::<Result<_, _>>()
                .unwrap();
            assert_eq!(bare.len(), want.len());
            for (x, y) in bare.iter().zip(&want) {
                assert_eq!(
                    (x.kind, x.recv_ns, x.venue_ms, x.levels.len()),
                    (y.kind, y.recv_ns, y.venue_ms, y.levels.len())
                );
                assert!(x.tx_hash.is_none() && x.payload.is_none());
            }
        }
    }
}

#[test]
fn time_range() {
    let f = File::open(golden("vec_shift5.probe")).unwrap();
    let full: Vec<_> = f.events().collect::<Result<_, _>>().unwrap();
    let t0 = f.segments()[0].t0_ns;
    let (from, to) = (t0 + 120_000_000_000, t0 + 420_000_000_000);
    let q = f.query().from_ns(from).to_ns(to);
    let got: Vec<_> = q.events().collect::<Result<_, _>>().unwrap();
    let segs = q.segments();
    let want: Vec<_> = full
        .iter()
        .filter(|e| segs.contains(&e.segment) && e.recv_ns.is_none_or(|t| t >= from && t < to))
        .cloned()
        .collect();
    assert!(!got.is_empty());
    assert_eq!(got, want);
    assert!(got.iter().all(|e| e.kind == EventType::Checkpoint
        || (e.recv_ns.unwrap() >= from && e.recv_ns.unwrap() < to)));
    // a range after the file reads nothing
    assert_eq!(
        f.query().from_ns(t0 + 7_200_000_000_000).events().count(),
        0
    );
}

#[test]
fn books_equal_reference() {
    for (bin, prefix) in [
        ("basic_v1", "basic"),
        ("basic_v2", "basic"),
        ("vec_dts_bucket", "vec_dts_bucket"),
        ("vec_size_cache", "vec_size_cache"),
        ("vec_flags0", "vec_flags0"),
        ("vec_shift5", "vec_shift5"),
        ("vec_shift0", "vec_shift0"),
        ("vec_renorm_shift0", "vec_renorm_shift0"),
    ] {
        let f = File::open(golden(&format!("{}.probe", bin))).unwrap();
        let qs =
            String::from_utf8(read(&golden(&format!("{}.books_queries.csv", prefix)))).unwrap();
        let mut got = String::new();
        let mut n = 0;
        for l in qs.lines().skip(1) {
            let (tok, ts) = l.split_once(',').unwrap();
            let t: i64 = ts.parse().unwrap();
            match f.book_at(tok, t).unwrap() {
                None => got.push_str(&format!("{},{},NONE,,\n", tok, ts)),
                Some(b) => {
                    for (p, z) in &b.bids {
                        got.push_str(&format!("{},{},BUY,{},{}\n", tok, ts, p, z));
                    }
                    for (p, z) in &b.asks {
                        got.push_str(&format!("{},{},SELL,{},{}\n", tok, ts, p, z));
                    }
                }
            }
            n += 1;
        }
        if prefix == "basic" {
            assert_eq!(n, 12);
        }
        let want = read(&golden(&format!("{}.books.csv", prefix)));
        assert!(
            got.as_bytes() == want.as_slice(),
            "{}: {}",
            bin,
            first_diff(got.as_bytes(), &want)
        );
    }
}

/// book-max of every market of every vector equals the Go reference's book-max for that market.
#[test]
fn book_max_equals_go() {
    let mut checked = 0;
    for (name, ..) in VECTORS {
        let f = File::open(golden(&format!("{}.probe", name))).unwrap();
        let mut slugs: Vec<String> = f.markets().iter().map(|m| m.slug.clone()).collect();
        slugs.sort();
        slugs.dedup();
        for slug in slugs {
            let want = read(&expected(&format!("{}.{}.book-max.csv", name, slug)));
            let mut got = BOOK_MAX_COLUMNS.join(",");
            got.push('\n');
            let mut bm = BookMax::new(&f);
            let mut rows = Vec::new();
            for ev in f
                .query()
                .market(&slug)
                .unwrap()
                .side_streams(false)
                .events()
            {
                rows.clear();
                bm.event(&ev.unwrap(), &mut rows);
                for r in &rows {
                    push_book_max_csv(&mut got, r);
                    got.push('\n');
                }
            }
            assert!(
                got.as_bytes() == want.as_slice(),
                "{} {}: {}",
                name,
                slug,
                first_diff(got.as_bytes(), &want)
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 29);
}

fn patched(name: &str, from: &str, to: &str) -> Vec<u8> {
    let mut b = read(&golden(name));
    let hl = u32::from_le_bytes([b[4], b[5], b[6], b[7]]) as usize;
    let h = String::from_utf8(b[8..8 + hl].to_vec()).unwrap();
    assert!(h.contains(from));
    let h2 = h.replacen(from, to, 1);
    assert_eq!(h2.len(), h.len(), "same-length patch only");
    b[8..8 + hl].copy_from_slice(h2.as_bytes());
    b
}

#[test]
fn refuses_unknown_versions_and_variants() {
    for (from, to) in [
        ("\"evmin/5\"", "\"evmin/7\""),
        ("\"layout\":\"mkt\"", "\"layout\":\"tim\""),
        ("\"family\":\"rc\"", "\"family\":\"xx\""),
    ] {
        match File::from_bytes(patched("basic_v2.probe", from, to)) {
            Err(Error::Unsupported(_)) => {}
            other => panic!("{} -> {}: {:?}", from, to, other.map(|_| ())),
        }
    }
    // a flag bit this reader does not decode (80 = 16 + 64; 81 adds bit 1)
    match File::from_bytes(patched("basic_v1.probe", "\"flags\":80", "\"flags\":81")) {
        Err(Error::Unsupported(_)) => {}
        other => panic!("flags 81: {:?}", other.map(|_| ())),
    }
    assert!(File::from_bytes(b"EVM5\x00\x00\x00\x00".to_vec()).is_err());
    assert!(File::from_bytes(Vec::new()).is_err());
}

/// Corrupted and truncated files give errors or garbage, never a panic.
#[test]
fn corrupt_input_never_panics() {
    let mut seed: u64 = 0x9e3779b97f4a7c15;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for name in ["basic_v1.probe", "basic_v2.probe", "vec_shift0.probe"] {
        let base = read(&golden(name));
        for i in 0..200 {
            let mut b = base.clone();
            match i % 4 {
                0 => b.truncate((next() as usize) % b.len()),
                1 => {
                    for _ in 0..1 + next() % 8 {
                        let k = (next() as usize) % b.len();
                        b[k] ^= 1 << (next() % 8);
                    }
                }
                2 => {
                    // corrupt the body only, keeping the header parseable
                    let hl = u32::from_le_bytes([b[4], b[5], b[6], b[7]]) as usize + 8;
                    for _ in 0..1 + next() % 16 {
                        let k = hl + (next() as usize) % (b.len() - hl);
                        b[k] = next() as u8;
                    }
                }
                _ => {
                    let k = (next() as usize) % b.len();
                    b.drain(k..(k + 1 + (next() as usize) % 64).min(b.len()));
                }
            }
            if let Ok(f) = File::from_bytes(b) {
                let _ = core_dump(&f, None);
                let _ = f.events().count();
                let mut sink = Vec::new();
                let _ = write_dump(&f, &mut sink);
                for m in f.markets().iter().take(3) {
                    if let Ok(q) = f.query().market(&m.slug) {
                        let _ = q.events().count();
                    }
                    let _ = f.book_at(&m.tokens[0], i64::MAX);
                }
            }
        }
    }
}
