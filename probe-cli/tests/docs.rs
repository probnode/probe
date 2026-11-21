//! The documentation, executed.
//!
//! Every `probe …` line printed in `README.md` and `docs/READING.md` is read out of the markdown
//! and held to the binary: the subcommand must exist, and every option on the line must appear in
//! that subcommand's help. Then a fixed list of file-only invocations is actually run against a
//! conformance vector.
//!
//! Why the first half is not simply "run every documented line": the documented lines carry a
//! market slug, a token and timestamps from a real production hour, and inventing substitutions
//! for them would test the substitutions. What rots in documentation is not the example data, it
//! is a flag that was renamed and a command that was removed - and that is exactly what a help
//! text can be held to, with no guessing.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../");

fn run(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_probe"))
        .args(args)
        .output()
        .expect("run probe");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// `--help` prints to stdout on success and to stderr on a bad command; take both.
fn help(args: &[&str]) -> String {
    let (_, out, err) = run(args);
    format!("{out}{err}")
}

fn tmp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("probe-docs-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

/// Every `probe …` command line inside a fenced `sh` block of a markdown file.
fn documented_commands(markdown: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut inside = false;
    for line in markdown.lines() {
        let t = line.trim();
        if t.starts_with("```") {
            // ```sh opens, ``` closes. Any other language is not a shell block.
            inside = t == "```sh" || t == "```bash";
            continue;
        }
        if !inside {
            continue;
        }
        // Strip a trailing `# comment`, which the pages use to say what a line does.
        let cmd = match t.split_once(" # ") {
            Some((before, _)) => before.trim(),
            None => t,
        };
        if cmd.starts_with("probe ") || cmd == "probe" {
            found.push(cmd.to_string());
        }
    }
    found
}

fn page(name: &str) -> String {
    std::fs::read_to_string(format!("{ROOT}{name}")).unwrap_or_else(|e| panic!("{name}: {e}"))
}

#[test]
fn every_documented_command_and_option_exists() {
    let mut lines = documented_commands(&page("README.md"));
    lines.extend(documented_commands(&page("docs/READING.md")));
    assert!(
        lines.len() >= 10,
        "only {} probe commands found in the pages - has a fenced block lost its `sh`?",
        lines.len()
    );

    let root_help = help(&["--help"]);
    let mut checked: BTreeSet<String> = BTreeSet::new();

    for line in &lines {
        let mut words = line.split_whitespace();
        let _probe = words.next();
        let Some(sub) = words.next() else { continue };
        if sub.starts_with('-') {
            // `probe --version`: an option on the binary itself.
            assert!(
                root_help.contains(sub),
                "`{line}`: the binary has no {sub}\n--- probe --help ---\n{root_help}"
            );
            continue;
        }
        assert!(
            root_help.contains(sub),
            "`{line}`: there is no `probe {sub}` subcommand\n--- probe --help ---\n{root_help}"
        );
        let sub_help = help(&[sub, "--help"]);
        for word in line.split_whitespace().filter(|w| w.starts_with("--")) {
            let flag = word.split('=').next().unwrap();
            assert!(
                sub_help.contains(flag),
                "`{line}`: `probe {sub}` has no {flag}\n--- probe {sub} --help ---\n{sub_help}"
            );
        }
        checked.insert(sub.to_string());
    }

    // The pages are meant to cover the tool; if a subcommand stops appearing in them, that is a
    // documentation hole and this is where it shows up.
    for sub in ["info", "cat", "convert", "verify", "dump", "book", "transcode"] {
        assert!(
            checked.contains(sub),
            "`probe {sub}` is in the binary and in no page"
        );
    }
}

#[test]
fn the_documented_invocations_run() {
    let vector_owned = format!("{ROOT}tests/golden/basic_v2.probe");
    let v3 = tmp("hour.v3.probe");
    let parquet = tmp("book.parquet");
    let core = tmp("core.csv");
    let v3_owned = v3.to_string_lossy().into_owned();
    let parquet_owned = parquet.to_string_lossy().into_owned();
    let core_owned = core.to_string_lossy().into_owned();
    // Bound as &str so the table below is plainly Vec<Vec<&str>> and no coercion is in play.
    let vector: &str = &vector_owned;
    let v3s: &str = &v3_owned;
    let parquets: &str = &parquet_owned;
    let cores: &str = &core_owned;

    // The file-only shape of each documented command: the same subcommand and the same options,
    // with this repository's vector in place of the production hour the pages name.
    let cases: Vec<Vec<&str>> = vec![
        vec!["--version"],
        vec!["info", vector],
        vec!["info", vector, "--markets"],
        vec!["cat", vector, "--what", "levels", "--format", "ndjson", "--limit", "20"],
        vec!["cat", vector, "--what", "events", "--format", "csv", "--limit", "5"],
        vec!["convert", vector, "-o", parquets],
        vec!["verify", vector],
        vec!["dump", vector, "-o", cores],
        vec!["transcode", vector, v3s, "--profile", "fast", "--verify"],
        vec!["info", v3s],
    ];
    for args in cases {
        let (code, _, err) = run(&args);
        assert_eq!(code, 0, "probe {} failed: {err}", args.join(" "));
    }
    assert!(parquet.exists() && core.exists() && v3.exists());
    let _ = std::fs::remove_dir_all(parquet.parent().unwrap());
}
