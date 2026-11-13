//! The `probe` binary on the conformance vectors.

use std::path::PathBuf;
use std::process::Command;

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../tests/");

fn golden(name: &str) -> String {
    format!("{}golden/{}", ROOT, name)
}

fn probe(args: &[&str]) -> (i32, Vec<u8>, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_probe"))
        .args(args)
        .output()
        .expect("run probe");
    (
        out.status.code().unwrap_or(-1),
        out.stdout,
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("probe-cli-test-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d.join(name)
}

#[test]
fn dump_equals_the_go_reference() {
    for v in ["basic_v1", "basic_v2", "vec_renorm_shift0"] {
        let (code, out, err) = probe(&["dump", &golden(&format!("{}.probe", v))]);
        assert_eq!(code, 0, "{}", err);
        let want = std::fs::read(format!("{}expected/{}.core.csv", ROOT, v)).unwrap();
        assert!(out == want, "{}: core dump differs", v);
        assert!(err.contains("\"fnv64\""));
    }
    let (code, out, err) = probe(&["dump", "--full", &golden("basic_v2.probe")]);
    assert_eq!(code, 0, "{}", err);
    assert!(
        out == std::fs::read(golden("basic.events.csv")).unwrap(),
        "full dump differs from events.csv"
    );
}

#[test]
fn info_json() {
    let (code, out, err) = probe(&["info", "--json", "--markets", &golden("basic_v2.probe")]);
    assert_eq!(code, 0, "{}", err);
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["version"], "v2");
    assert_eq!(v["header_version"], "evmin/5");
    assert_eq!(v["markets"], 5);
    assert_eq!(v["segments"].as_array().unwrap().len(), 3);
    assert_eq!(v["market_rows"][2]["slug"], "mkt-2");
    let (code, out, _) = probe(&["info", &golden("basic_v1.probe")]);
    assert_eq!(code, 0);
    assert!(String::from_utf8(out).unwrap().contains("v1 (evmin/4)"));
}

#[test]
fn cat_filters() {
    let f = golden("basic_v2.probe");
    let (code, out, err) = probe(&["cat", &f, "--format", "ndjson", "--limit", "5"]);
    assert_eq!(code, 0, "{}", err);
    let s = String::from_utf8(out).unwrap();
    assert_eq!(s.lines().count(), 5);
    for l in s.lines() {
        let v: serde_json::Value = serde_json::from_str(l).unwrap();
        assert_eq!(v["event"], "checkpoint");
    }
    let (code, out, _) = probe(&[
        "cat",
        &f,
        "--what",
        "levels",
        "--market",
        "mkt-1",
        "--no-header",
    ]);
    assert_eq!(code, 0);
    let s = String::from_utf8(out).unwrap();
    assert!(
        !s.is_empty()
            && s.lines()
                .all(|l| l.contains(",70011,") || l.contains(",70012,")),
        "only mkt-1's tokens"
    );
    let (code, out, _) = probe(&["cat", &f, "--what", "book-max", "--market", "mkt-2"]);
    assert_eq!(code, 0);
    assert_eq!(
        out,
        std::fs::read(format!("{}expected/basic_v2.mkt-2.book-max.csv", ROOT)).unwrap()
    );
    let (code, out, _) = probe(&[
        "cat",
        &f,
        "--from",
        "2026-11-11T21:05:00Z",
        "--to",
        "2026-11-11T21:06:00Z",
        "--format",
        "ndjson",
    ]);
    assert_eq!(code, 0);
    for l in String::from_utf8(out).unwrap().lines() {
        let v: serde_json::Value = serde_json::from_str(l).unwrap();
        if v["event"] != "checkpoint" {
            let t = v["recv_ns"].as_i64().unwrap();
            assert!(
                (1_794_431_100_000_000_000..1_794_431_160_000_000_000).contains(&t),
                "{}",
                t
            );
        }
    }
    let (code, _, err) = probe(&["cat", &f, "--market", "no-such-market"]);
    assert_ne!(code, 0);
    assert!(err.contains("no market"));
}

#[test]
fn convert_parquet_and_arrow() {
    let f = golden("basic_v1.probe");
    let pq = tmp("basic.parquet");
    let (code, _, err) = probe(&["convert", &f, "-o", pq.to_str().unwrap()]);
    assert_eq!(code, 0, "{}", err);
    let b = std::fs::read(&pq).unwrap();
    assert!(b.len() > 100 && &b[..4] == b"PAR1" && &b[b.len() - 4..] == b"PAR1");
    let ar = tmp("basic.arrow");
    let (code, _, err) = probe(&[
        "convert",
        &f,
        "-o",
        ar.to_str().unwrap(),
        "--market",
        "mkt-0",
    ]);
    assert_eq!(code, 0, "{}", err);
    let b = std::fs::read(&ar).unwrap();
    assert!(&b[..6] == b"ARROW1");
    let (code, _, _) = probe(&["convert", &f, "-o", tmp("x.unknown").to_str().unwrap()]);
    assert_ne!(code, 0);
}

#[test]
fn verify_and_book() {
    for v in ["basic_v1", "basic_v2", "units_hour_a", "vec_size_cache"] {
        let (code, out, err) = probe(&["verify", &golden(&format!("{}.probe", v))]);
        assert_eq!(code, 0, "{} {} {}", v, String::from_utf8_lossy(&out), err);
        assert!(String::from_utf8(out).unwrap().starts_with("OK "));
    }
    let bad = tmp("bad.probe");
    let mut b = std::fs::read(golden("basic_v2.probe")).unwrap();
    let n = b.len();
    b.truncate(n - 100);
    std::fs::write(&bad, b).unwrap();
    let (code, out, _) = probe(&["verify", bad.to_str().unwrap()]);
    assert_eq!(code, 1);
    assert!(String::from_utf8(out).unwrap().starts_with("FAIL "));
    let qs = std::fs::read_to_string(golden("basic.books_queries.csv")).unwrap();
    let want = std::fs::read_to_string(golden("basic.books.csv")).unwrap();
    let mut got = String::new();
    for l in qs.lines().skip(1) {
        let (tok, ts) = l.split_once(',').unwrap();
        let (code, out, err) = probe(&[
            "book",
            &golden("basic_v1.probe"),
            "--token",
            tok,
            "--at",
            ts,
        ]);
        assert_eq!(code, 0, "{}", err);
        got.push_str(&String::from_utf8(out).unwrap());
    }
    assert_eq!(got, want);
}
