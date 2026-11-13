//! Probe v3: the conformance vectors of `tests/golden/v3` (every v1 / v2 vector transcoded to
//! every profile) and their core dumps in `tests/expected/v3`; every reading API on v3 against
//! the source; the transcoder's determinism; refusals and corrupt input.
//!
//! `PROBE_WRITE_V3_GOLDEN=1 cargo test --test v3` writes the v3 vectors and their dumps from the
//! current transcoder (for adding a vector; a codec id never changes its bytes).

use probe_format::bookmax::{push_book_max_csv, BookMax, BOOK_MAX_COLUMNS};
use probe_format::dump::write_dump;
use probe_format::v3::{self, codec, Profile};
use probe_format::{core_dump, Error, EventType, File, Version};

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../tests/");

/// (v1 / v2 vector, its events.csv prefix)
const SOURCES: [(&str, &str); 10] = [
    ("basic_v1", "basic"),
    ("basic_v2", "basic"),
    ("units_hour_a", "units_hour_a"),
    ("units_hour_b", "units_hour_b"),
    ("vec_dts_bucket", "vec_dts_bucket"),
    ("vec_flags0", "vec_flags0"),
    ("vec_renorm_shift0", "vec_renorm_shift0"),
    ("vec_shift0", "vec_shift0"),
    ("vec_shift5", "vec_shift5"),
    ("vec_size_cache", "vec_size_cache"),
];

fn read(p: &str) -> Vec<u8> {
    std::fs::read(p).unwrap_or_else(|e| panic!("{}: {}", p, e))
}

fn source(name: &str) -> File {
    File::open(format!("{}golden/{}.probe", ROOT, name)).unwrap()
}

fn v3_path(name: &str, p: Profile) -> String {
    format!("{}golden/v3/{}.{}.probe", ROOT, name, p.name())
}

fn v3_dump_path(name: &str, p: Profile) -> String {
    format!("{}expected/v3/{}.{}.core.csv", ROOT, name, p.name())
}

fn dump(f: &File) -> Vec<u8> {
    let mut out = Vec::new();
    core_dump(f, Some(&mut out)).unwrap();
    out
}

fn full_dump(f: &File) -> Vec<u8> {
    let mut out = Vec::new();
    write_dump(f, &mut out).unwrap();
    out
}

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

fn transcoded(name: &str, p: Profile) -> File {
    File::from_bytes(v3::transcode(&source(name), p).unwrap().bytes).unwrap()
}

/// Every v3 vector: its bytes are what the transcoder writes from its source (the encoder is
/// pinned), its core dump is byte-identical to the source's and to its expected dump.
#[test]
fn v3_vectors_decode_to_their_source() {
    let write = std::env::var_os("PROBE_WRITE_V3_GOLDEN").is_some();
    if write {
        std::fs::create_dir_all(format!("{}golden/v3", ROOT)).unwrap();
        std::fs::create_dir_all(format!("{}expected/v3", ROOT)).unwrap();
    }
    let mut checked = 0;
    for (name, _) in SOURCES {
        let src = source(name);
        let want = dump(&src);
        assert_eq!(
            want,
            read(&format!("{}expected/{}.core.csv", ROOT, name)),
            "{}: source dump",
            name
        );
        for p in Profile::ALL {
            let bytes = v3::transcode(&src, p).unwrap().bytes;
            if write {
                std::fs::write(v3_path(name, p), &bytes).unwrap();
            }
            let committed = read(&v3_path(name, p));
            assert!(
                committed == bytes,
                "{} {}: the transcoder no longer writes the committed vector",
                name,
                p
            );
            let f = File::open(v3_path(name, p)).unwrap();
            assert_eq!(f.version(), Version::V3);
            assert_eq!(f.header().profile.as_deref(), Some(p.name()));
            assert_eq!(f.header().source_version, Some(src.version()));
            let got = dump(&f);
            if write {
                std::fs::write(v3_dump_path(name, p), &got).unwrap();
            }
            assert!(got == want, "{} {}: {}", name, p, first_diff(&got, &want));
            let exp = read(&v3_dump_path(name, p));
            assert!(got == exp, "{} {}: {}", name, p, first_diff(&got, &exp));
            checked += 1;
        }
    }
    assert_eq!(checked, 30);
}

/// The blocks codec of each profile, and every part's codec registered.
#[test]
fn profiles_use_their_codecs() {
    for (p, want) in [
        (Profile::Fast, codec::M2_RANS),
        (Profile::Fast1, codec::M1_RANS),
        (Profile::Rc1, codec::RC1),
    ] {
        let f = File::open(v3_path("basic_v2", p)).unwrap();
        for s in f.segments() {
            assert_eq!(s.blocks_codec(), Some(want));
            for part in &s.parts {
                let c = part.codec.expect("a v3 part names its codec");
                assert!(codec::name(c).is_some(), "{} codec {}", part.name, c);
            }
            assert_eq!(s.part("bidx3").unwrap().codec, Some(codec::COLS_RANS));
            assert_eq!(s.part("order").unwrap().codec, Some(codec::ORDER_TS));
            assert_eq!(s.part("tx").unwrap().codec, Some(codec::RAW));
            assert_eq!(s.part("evcols").is_some(), p == Profile::Fast);
            assert_eq!(s.part("tables").is_some(), p != Profile::Rc1);
        }
    }
}

/// The transcoder is deterministic, v3 transcodes to v3, and verify_transcode passes.
#[test]
fn transcoder_is_deterministic_and_lossless() {
    for (name, _) in SOURCES {
        let src = source(name);
        let want = dump(&src);
        for p in Profile::ALL {
            let a = v3::transcode(&src, p).unwrap();
            let b = v3::transcode(&src, p).unwrap();
            assert_eq!(a.bytes, b.bytes, "{} {}: not deterministic", name, p);
            let total: u64 = a.part_bytes.iter().map(|x| x.1).sum();
            assert_eq!(total, a.bytes.len() as u64, "{} {}: part sizes", name, p);
            let f = File::from_bytes(a.bytes).unwrap();
            v3::verify_transcode(&src, &f).unwrap();
            // v3 -> v3
            for q in [Profile::Fast, Profile::Fast1] {
                let g = File::from_bytes(v3::transcode(&f, q).unwrap().bytes).unwrap();
                assert_eq!(dump(&g), want, "{} {} -> {}", name, p, q);
                assert_eq!(g.header().source_version, Some(src.version()));
            }
            // rc1 keeps v1 blocks: it needs a v1 / v2 source
            assert!(matches!(
                v3::transcode(&f, Profile::Rc1),
                Err(Error::Unsupported(_))
            ));
        }
    }
}

/// Side streams: the full dump (every tx hash, payload, best-price exception, book hash) of a v3
/// file equals the source vector's events.csv.
#[test]
fn v3_full_dump_equals_reference() {
    for (name, ev) in SOURCES {
        let want = read(&format!("{}golden/{}.events.csv", ROOT, ev));
        for p in Profile::ALL {
            let f = File::open(v3_path(name, p)).unwrap();
            let got = full_dump(&f);
            assert!(got == want, "{} {}: {}", name, p, first_diff(&got, &want));
        }
    }
}

/// Events, one market, a time range, the decoder core and the order: v3 equals its source.
#[test]
fn v3_reads_like_its_source() {
    for name in ["basic_v1", "basic_v2", "vec_shift5", "units_hour_a"] {
        let src = source(name);
        let all: Vec<_> = src.events().collect::<Result<_, _>>().unwrap();
        let mut slugs: Vec<String> = src.markets().iter().map(|m| m.slug.clone()).collect();
        slugs.dedup();
        let t0 = src.segments()[0].t0_ns;
        for p in Profile::ALL {
            let f = transcoded(name, p);
            let got: Vec<_> = f.events().collect::<Result<_, _>>().unwrap();
            assert_eq!(got, all, "{} {}: events", name, p);
            for si in 0..f.segments().len() {
                assert_eq!(f.segment_core(si).unwrap(), src.segment_core(si).unwrap());
                #[cfg(feature = "parallel")]
                assert_eq!(
                    f.segment_core_par(si).unwrap(),
                    src.segment_core(si).unwrap()
                );
                assert_eq!(f.decode_order(si).unwrap(), src.decode_order(si).unwrap());
                for bi in 0..f.segments()[si].blocks.len() {
                    assert_eq!(
                        f.decode_block(si, bi).unwrap(),
                        src.decode_block(si, bi).unwrap(),
                        "{} {} segment {} block {}",
                        name,
                        p,
                        si,
                        bi
                    );
                }
            }
            for slug in &slugs {
                for side in [true, false] {
                    let a: Vec<_> = src
                        .query()
                        .market(slug)
                        .unwrap()
                        .side_streams(side)
                        .events()
                        .collect::<Result<_, _>>()
                        .unwrap();
                    let b: Vec<_> = f
                        .query()
                        .market(slug)
                        .unwrap()
                        .side_streams(side)
                        .events()
                        .collect::<Result<_, _>>()
                        .unwrap();
                    assert_eq!(a, b, "{} {} {}", name, p, slug);
                }
            }
            let (from, to) = (t0 + 120_000_000_000, t0 + 420_000_000_000);
            let a: Vec<_> = src
                .query()
                .from_ns(from)
                .to_ns(to)
                .events()
                .collect::<Result<_, _>>()
                .unwrap();
            let b: Vec<_> = f
                .query()
                .from_ns(from)
                .to_ns(to)
                .events()
                .collect::<Result<_, _>>()
                .unwrap();
            assert_eq!(a, b, "{} {}: time range", name, p);
        }
    }
}

/// book_at on v3 equals the reference books of the source vectors.
#[test]
fn v3_books_equal_reference() {
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
        let qs = String::from_utf8(read(&format!(
            "{}golden/{}.books_queries.csv",
            ROOT, prefix
        )))
        .unwrap();
        let want = read(&format!("{}golden/{}.books.csv", ROOT, prefix));
        for p in Profile::ALL {
            let f = File::open(v3_path(bin, p)).unwrap();
            let mut got = String::new();
            for l in qs.lines().skip(1) {
                let (tok, ts) = l.split_once(',').unwrap();
                match f.book_at(tok, ts.parse().unwrap()).unwrap() {
                    None => got.push_str(&format!("{},{},NONE,,\n", tok, ts)),
                    Some(b) => {
                        for (px, z) in &b.bids {
                            got.push_str(&format!("{},{},BUY,{},{}\n", tok, ts, px, z));
                        }
                        for (px, z) in &b.asks {
                            got.push_str(&format!("{},{},SELL,{},{}\n", tok, ts, px, z));
                        }
                    }
                }
            }
            assert!(
                got.as_bytes() == want.as_slice(),
                "{} {}: {}",
                bin,
                p,
                first_diff(got.as_bytes(), &want)
            );
        }
    }
}

/// book-max of every market of every v3 vector equals the reference's.
#[test]
fn v3_book_max_equals_reference() {
    let mut checked = 0;
    for (name, _) in SOURCES {
        for p in Profile::ALL {
            let f = File::open(v3_path(name, p)).unwrap();
            let mut slugs: Vec<String> = f.markets().iter().map(|m| m.slug.clone()).collect();
            slugs.sort();
            slugs.dedup();
            for slug in slugs {
                let want = read(&format!("{}expected/{}.{}.book-max.csv", ROOT, name, slug));
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
                    "{} {} {}: {}",
                    name,
                    p,
                    slug,
                    first_diff(got.as_bytes(), &want)
                );
                checked += 1;
            }
        }
    }
    assert_eq!(checked, 87);
}

fn patched(b: &[u8], from: &str, to: &str) -> Vec<u8> {
    let mut b = b.to_vec();
    let hl = u32::from_le_bytes([b[4], b[5], b[6], b[7]]) as usize;
    let h = String::from_utf8(b[8..8 + hl].to_vec()).unwrap();
    assert!(h.contains(from), "{}", from);
    let h2 = h.replace(from, to);
    assert_eq!(h2.len(), h.len(), "same-length patch only");
    b[8..8 + hl].copy_from_slice(h2.as_bytes());
    b
}

/// A codec id this reader does not implement, an unknown version or family, a source version
/// that is not v1 / v2: refused as unsupported, never guessed.
#[test]
fn v3_refuses_what_it_does_not_implement() {
    let fast = read(&v3_path("basic_v2", Profile::Fast));
    for (from, to) in [
        ("\"evmin/6\"", "\"evmin/7\""),
        ("\"family\":\"probe3\"", "\"family\":\"probe4\""),
        ("\"src_version\":\"evmin/5\"", "\"src_version\":\"evmin/6\""),
        ("\"evcols\"", "\"evcolz\""),
        (",20]", ",24]"),
        (",21]", ",25]"),
        (",17]", ",27]"),
        (",18]", ",28]"),
        (",19]", ",29]"),
    ] {
        let b = patched(&fast, from, to);
        match File::from_bytes(b) {
            Err(Error::Unsupported(_)) | Err(Error::Format(_)) => {}
            other => panic!("{} -> {}: {:?}", from, to, other.map(|_| ())),
        }
    }
    // codec ids on the blocks, order and index parts are refused as unsupported
    for (from, to) in [(",20]", ",24]"), (",17]", ",27]"), (",18]", ",28]")] {
        assert!(matches!(
            File::from_bytes(patched(&fast, from, to)),
            Err(Error::Unsupported(_))
        ));
    }
    // an unknown part name is ignored
    let v1 = read(&v3_path("basic_v1", Profile::Fast));
    let renamed = patched(&v1, "\"tx\"", "\"tq\"");
    assert!(File::from_bytes(renamed).is_ok());
}

/// Corrupted and truncated v3 files give errors or garbage, never a panic.
#[test]
fn v3_corrupt_input_never_panics() {
    let mut seed: u64 = 0x2545f4914f6cdd1d;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for name in ["basic_v1", "basic_v2", "vec_shift0"] {
        for p in Profile::ALL {
            let base = read(&v3_path(name, p));
            let hl = u32::from_le_bytes([base[4], base[5], base[6], base[7]]) as usize + 8;
            for i in 0..160 {
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
                        for _ in 0..1 + next() % 16 {
                            let k = hl + (next() as usize) % (b.len() - hl);
                            b[k] = next() as u8;
                        }
                    }
                    _ => {
                        let k = hl + (next() as usize) % (b.len() - hl);
                        b[k] ^= 1 << (next() % 8);
                    }
                }
                if let Ok(f) = File::from_bytes(b) {
                    let _ = core_dump(&f, None);
                    let _ = f.events().count();
                    let mut sink = Vec::new();
                    let _ = write_dump(&f, &mut sink);
                    for si in 0..f.segments().len() {
                        for bi in 0..f.segments()[si].blocks.len() {
                            let _ = f.decode_block(si, bi);
                        }
                    }
                    for m in f.markets().iter().take(3) {
                        if let Ok(q) = f.query().market(&m.slug) {
                            let _ = q.events().count();
                        }
                        let _ = f.book_at(&m.tokens[0], i64::MAX);
                    }
                    let _ = v3::transcode(&f, Profile::Fast);
                }
            }
        }
    }
}

/// The side streams of a v3 file are the source's bytes.
#[test]
fn v3_keeps_side_streams_and_markets_byte_for_byte() {
    for (name, _) in SOURCES {
        let src = source(name);
        for p in Profile::ALL {
            let f = File::open(v3_path(name, p)).unwrap();
            assert_eq!(f.markets(), src.markets());
            for si in 0..src.segments().len() {
                for part in ["tx", "payload.zst", "hash"] {
                    assert_eq!(
                        f.part_bytes(si, part),
                        src.part_bytes(si, part),
                        "{} {} {}",
                        name,
                        p,
                        part
                    );
                }
            }
            let trades = f
                .events()
                .filter(|e| e.as_ref().unwrap().kind == EventType::Trade)
                .count();
            let src_trades = src
                .events()
                .filter(|e| e.as_ref().unwrap().kind == EventType::Trade)
                .count();
            assert_eq!(trades, src_trades);
        }
    }
}
