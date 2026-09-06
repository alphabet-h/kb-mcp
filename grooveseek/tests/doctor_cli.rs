//! `groove doctor` as a command: where it writes, and what it exits with.
//!
//! The findings themselves are unit-tested in `src/doctor.rs`. What only a
//! subprocess can check is the contract CI depends on — **stdout** carries the
//! report (CLAUDE.md's CLI output convention puts a command's result there;
//! `index` and `status` are the stderr-only ones), and the exit code
//! distinguishes "clean" from "found something" from "could not look".
//!
//! No embedding model is involved: the fixtures are built by writing rows
//! through the library, so these run under a plain `cargo test`.

use std::path::Path;
use std::process::Command;

mod common;
use common::mcp::grooveseek_bin;
use common::temp::TempKbLayout;

/// Build an index directly, without going through `groove index` — that would
/// need the embedding model, and none of what `doctor` looks at depends on the
/// embedding being real.
fn seed_index(kb: &Path) {
    let db_path = grooveseek::resolve_db_path(kb);
    let db = grooveseek::db::Database::open(&db_path.to_string_lossy()).expect("open db");
    db.verify_embedding_meta("bge-small-en-v1.5", 384)
        .expect("meta");
    let doc = db
        .upsert_document(
            "notes/a.md",
            Some("A"),
            None,
            None,
            None,
            &[],
            None,
            "h",
            42,
        )
        .expect("upsert");
    db.insert_chunk(doc, 0, Some("H"), None, "body", None, &vec![0.1; 384], 1.0)
        .expect("chunk");
}

fn run_doctor(kb: &Path, json: bool) -> (i32, String, String) {
    let mut args = vec![
        "doctor".to_string(),
        "--kb-path".to_string(),
        kb.display().to_string(),
    ];
    if json {
        args.push("--format".to_string());
        args.push("json".to_string());
    }
    let out = Command::new(grooveseek_bin())
        .args(&args)
        .output()
        .expect("groove doctor");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn a_healthy_index_exits_zero_and_says_so_on_stdout() {
    let layout = TempKbLayout::new("groove-doctor-clean");
    layout.write("notes/a.md", "# A\n\nbody\n");
    seed_index(layout.kb());

    let (code, stdout, _) = run_doctor(layout.kb(), false);
    assert_eq!(code, 0, "a healthy index must not fail a CI gate");
    assert!(
        stdout.contains("No issues found"),
        "the report belongs on stdout, got: {stdout}"
    );
}

#[test]
fn a_broken_index_exits_one_and_names_the_check() {
    let layout = TempKbLayout::new("groove-doctor-broken");
    layout.write("notes/a.md", "# A\n\nbody\n");
    seed_index(layout.kb());
    {
        let db_path = grooveseek::resolve_db_path(layout.kb());
        let conn = rusqlite::Connection::open(&db_path).expect("open");
        conn.execute_batch("DELETE FROM fts_chunks").expect("break");
    }

    let (code, stdout, _) = run_doctor(layout.kb(), true);
    assert_eq!(code, 1, "findings must be distinguishable from a clean run");

    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("--format json must emit JSON on stdout ({e}): {stdout}"));
    let checks: Vec<&str> = parsed["findings"]
        .as_array()
        .expect("findings array")
        .iter()
        .filter_map(|f| f["check"].as_str())
        .collect();
    assert!(
        checks.contains(&"missing-fts-row"),
        "expected the missing FTS row to be named, got {checks:?}"
    );
}

/// codex P2 round 2: `require_kb_path` used to run before the exit-code
/// mapping, so the most ordinary setup mistake — no `--kb-path` anywhere —
/// exited 1, the code reserved for "inspected it, found something".
#[test]
fn no_kb_path_at_all_exits_two_rather_than_looking_like_a_finding() {
    let layout = TempKbLayout::new("groove-doctor-nokbpath");
    // An explicit config with no `kb_path`, so the result does not depend on
    // whether the machine running the test happens to have one discoverable.
    let cfg = layout.root().join("groove.toml");
    std::fs::write(&cfg, "model = \"bge-small-en-v1.5\"\n").expect("write config");

    let out = Command::new(grooveseek_bin())
        .args(["--config", &cfg.to_string_lossy(), "doctor"])
        .output()
        .expect("groove doctor");
    assert_eq!(
        out.status.code().unwrap_or(-1),
        2,
        "a missing --kb-path is a failure to run, not a finding"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("--kb-path is required"),
        "the reason belongs on stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// codex P2 round 3: config discovery runs before the subcommand arm, so a
/// configuration that will not parse used to exit 1 — again the code that
/// means "inspected it, found something", for a run that never inspected
/// anything.
#[test]
fn a_configuration_that_will_not_load_exits_two() {
    let layout = TempKbLayout::new("groove-doctor-badcfg");
    let cfg = layout.root().join("groove.toml");
    std::fs::write(&cfg, "model = \"bge-small-en-v1.5\"\nthis is not toml\n")
        .expect("write config");

    let out = Command::new(grooveseek_bin())
        .args([
            "--config",
            &cfg.to_string_lossy(),
            "doctor",
            "--kb-path",
            &layout.kb().display().to_string(),
        ])
        .output()
        .expect("groove doctor");
    assert_eq!(
        out.status.code().unwrap_or(-1),
        2,
        "a config that will not parse is a failure to run"
    );
}

#[test]
fn a_missing_index_exits_two_rather_than_reporting_a_clean_bill() {
    // "I could not look" and "I looked and found nothing" are different
    // answers, and a CI gate that conflates them passes on a machine where the
    // index was never built.
    let layout = TempKbLayout::new("groove-doctor-noindex");
    layout.write("notes/a.md", "# A\n\nbody\n");

    let (code, stdout, stderr) = run_doctor(layout.kb(), false);
    assert_eq!(code, 2, "no index is a failure to run, not a finding");
    assert!(
        stdout.is_empty(),
        "there is no report to print, so stdout stays empty: {stdout}"
    );
    assert!(
        stderr.contains("No index found"),
        "the reason belongs on stderr: {stderr}"
    );
}

/// `groove serve` starts its watcher without an index, which is why the chunking policy
/// cannot be recorded by `groove index` alone.
///
/// `groove status`, `groove graph` and `groove doctor` all refuse a knowledge base with no
/// index; `groove serve` does not, and its watcher reaches
/// [`grooveseek::indexer::reindex_single_file`] without ever going through
/// [`grooveseek::indexer::rebuild_index`]. So the first source file a knowledge base ever
/// gets can arrive there, and the policy has to be recorded on that path too (codex P2,
/// round 3). This pins the premise: if `groove serve` ever starts refusing, that call
/// becomes dead weight rather than a silent gap, and someone should be told which it is.
#[test]
fn serve_starts_its_watcher_without_an_index_so_the_watcher_can_seed_one() {
    let layout = TempKbLayout::new("groove-serve-noindex");
    layout.write("notes/a.md", "# A\n\nbody\n");

    let out = Command::new(grooveseek_bin())
        .args(["serve", "--kb-path", &layout.kb().display().to_string()])
        .output()
        .expect("groove serve");

    // It exits non-zero because nothing is speaking MCP on its stdin, not because it declined
    // to look at the knowledge base. What says it got that far is the database: `serve` opens
    // one, and opening one creates it.
    //
    // Asserted on the file rather than on a log line. The first version of this looked for
    // the word "watching" and passed here while failing on the macOS runner, which printed
    // "watcher started" instead: the watcher announces itself twice from two lines of
    // `watcher.rs`, and which of them a run shows is not this test's business. The property
    // is about the index.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("No index found"),
        "serve is not supposed to require an index: {stderr}"
    );
    assert!(
        grooveseek::resolve_db_path(layout.kb()).exists(),
        "serve created no index, so nothing the watcher adds could land in one: {stderr}"
    );
}

/// Every way a document enters an index goes through one function, which is where the
/// chunking policy is resolved.
///
/// Resolving it at the callers instead is what left the rename branch uncovered: it reaches
/// [`grooveseek::indexer`]'s shared entry directly rather than through
/// [`grooveseek::indexer::reindex_single_file`] (codex P2, round 5). This reads the source
/// because the function is private, and the point is the shape of the call graph rather than
/// any one behaviour: if a fourth way in appears, it inherits the policy by arriving here.
#[test]
fn the_chunk_policy_is_resolved_where_the_insertion_paths_meet() {
    let src = include_str!("../src/indexer.rs");
    let entry = src
        .split("fn index_single_disk_entry(")
        .nth(1)
        .expect("the shared insertion path still exists");
    let body = entry.split("\nfn ").next().unwrap_or(entry);
    assert!(
        body.contains("resolve_code_chunk_policy(db, false)"),
        "the shared insertion path no longer resolves the chunking policy, so a caller that \
         does not do it itself would leave the index unable to say what chunked it"
    );
    // And not at the callers, where it would be one path away from being missed again.
    let reindex = src
        .split("pub fn reindex_single_file(")
        .nth(1)
        .expect("reindex_single_file still exists");
    let reindex_body = reindex.split("\npub fn ").next().unwrap_or(reindex);
    assert!(
        !reindex_body.contains("resolve_code_chunk_policy"),
        "the policy is resolved twice; the shared path already covers this caller"
    );
}
