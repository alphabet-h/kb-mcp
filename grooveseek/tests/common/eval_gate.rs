//! The machinery two retrieval-quality gates share.
//!
//! `tests/eval_corpus_quality.rs` measures whether prose documents stay findable;
//! `tests/code_eval_quality.rs` measures whether *definitions* do. Everything below is the
//! part of that job neither owns: walking a fixture tree, pinning the configuration a run
//! is measured under, driving `groove index` and `groove eval`, and reading the run back.
//!
//! What is **not** here is how a golden's queries are grouped and what each group's floor
//! is. Those differ between the two gates by design — one averages two populations whose
//! metric ceilings differ, the other one population — and a shared spelling of them would
//! be a single knob two measurements hang off.
//!
//! The split follows this module's own rule (see [`super`]): a second caller is what makes
//! collapsing a copy part of the change, and these functions acquired their second caller
//! when the code gate was added.

use std::path::{Path, PathBuf};
use std::process::Command;

use grooveseek::eval::{AggregateMetrics, ExpectedHit, QueryResult};

/// The `groove` binary cargo built for the test crate that is calling.
pub fn groove_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_groove"))
}

/// `<crate>/tests/fixtures`.
pub fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

// ---------------------------------------------------------------------------
// Corpus
// ---------------------------------------------------------------------------

/// Relative paths of every file under `root`, `/`-separated and sorted.
///
/// The separator is normalised because the indexer stores paths that way
/// (`indexer.rs` replaces `\`), which is what lets one golden file work on both
/// platforms.
pub fn relative_files(root: &Path) -> Vec<String> {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<String>) {
        let entries = std::fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("read fixture directory {}: {e}", dir.display()));
        for entry in entries {
            let entry = entry.unwrap_or_else(|e| panic!("read entry under {}: {e}", dir.display()));
            let path = entry.path();
            if path.is_dir() {
                walk(&path, base, out);
            } else {
                let rel = path
                    .strip_prefix(base)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push(rel);
            }
        }
    }

    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

/// Assert the fixture directory under `root` still holds exactly `manifest`, and return
/// that list.
///
/// `corpus` and `manifest_home` only appear in the failure text: the caller names its own
/// corpus and says where its manifest constant lives, so the message points at the file
/// that has to be edited rather than at this one.
pub fn assert_corpus_matches(
    root: &Path,
    manifest: &[&str],
    corpus: &str,
    manifest_home: &str,
) -> Vec<String> {
    let actual = relative_files(root);
    let mut expected: Vec<String> = manifest.iter().map(|s| (*s).to_string()).collect();
    expected.sort();
    assert_eq!(
        actual,
        expected,
        "the {corpus} fixture corpus drifted from {manifest_home}. Update the \
         manifest in {manifest_home} and add or remove the \
         matching golden query, then re-measure the baseline recorded in this \
         file's module docs. Corpus root: {}",
        root.display()
    );
    actual
}

/// Copy `files` from `root` into `kb`, recreating their subdirectories.
pub fn copy_corpus(root: &Path, files: &[String], kb: &Path) {
    for rel in files {
        let dst = kb.join(rel);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)
                .unwrap_or_else(|e| panic!("create {}: {e}", parent.display()));
        }
        let src = root.join(rel);
        std::fs::copy(&src, &dst)
            .unwrap_or_else(|e| panic!("copy fixture {} -> {}: {e}", src.display(), dst.display()));
    }
}

/// Write the `groove.toml` a run is pinned to, under `root`, and return its path.
///
/// Without a pinned file the run would take whatever config discovery finds from the test
/// process's working directory upwards. That file is user-local and git-ignored, so a
/// developer who has one with, say, `[search.mmr]` enabled would measure a different
/// pipeline than CI and see the gate fail for a reason that has nothing to do with their
/// change.
///
/// `parsers` is the whole of what may be pinned, and an empty slice writes an empty file.
/// A corpus holding anything but Markdown has to name its parsers here, because
/// `[parsers].enabled` defaults to `["md"]` and a file no parser claims is not indexed at
/// all — a permanent miss that every count in a gate would still report as satisfied.
/// **Nothing that configures retrieval belongs in this file**: adding a knob here would
/// recreate, with the pinned file's own hand, the split it exists to prevent.
pub fn pinned_config(root: &Path, parsers: &[&str]) -> PathBuf {
    let path = root.join("groove.toml");
    let body = if parsers.is_empty() {
        String::new()
    } else {
        let list: Vec<String> = parsers.iter().map(|p| format!("\"{p}\"")).collect();
        format!("[parsers]\nenabled = [{}]\n", list.join(", "))
    };
    std::fs::write(&path, body).expect("write pinned groove.toml");
    path
}

// ---------------------------------------------------------------------------
// Running the pipeline
// ---------------------------------------------------------------------------

pub fn index_corpus(kb: &Path, config: &Path, model: &str) {
    let out = Command::new(groove_bin())
        .arg("index")
        .arg("--kb-path")
        .arg(kb)
        .arg("--config")
        .arg(config)
        .arg("--model")
        .arg(model)
        .output()
        .expect("spawn groove index");
    assert!(
        out.status.success(),
        "groove index failed for {model}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Run `groove eval` over `golden` and return its JSON.
///
/// `--no-history` keeps the run stateless: no `.groove-eval-history.json` is written or
/// read, so a gate measures this build only and never compares against a stale
/// neighbouring run.
pub fn run_eval(kb: &Path, config: &Path, golden: &Path, model: &str) -> serde_json::Value {
    let out = Command::new(groove_bin())
        .arg("eval")
        .arg("--kb-path")
        .arg(kb)
        .arg("--config")
        .arg(config)
        .arg("--golden")
        .arg(golden)
        .arg("--model")
        .arg(model)
        .arg("--format")
        .arg("json")
        .arg("--no-history")
        .output()
        .expect("spawn groove eval");
    assert!(
        out.status.success(),
        "groove eval failed for {model}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "groove eval did not print JSON for {model}: {e}\nstdout was:\n{}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

// ---------------------------------------------------------------------------
// Reading a run back
// ---------------------------------------------------------------------------

pub fn metric(run: &serde_json::Value, pointer: &str) -> f64 {
    run.pointer(pointer)
        .and_then(|v| v.as_f64())
        .unwrap_or_else(|| panic!("no numeric metric at {pointer} in eval JSON:\n{run}"))
}

/// The run's per-query results, as the types `groove eval` serialised them from. Parsed
/// once and handed to everything else, so nothing digs through the JSON a second time with
/// its own idea of the shape.
pub fn per_query(run: &serde_json::Value) -> Vec<QueryResult> {
    serde_json::from_value(run["per_query"].clone())
        .unwrap_or_else(|e| panic!("eval JSON `per_query` did not parse: {e}\n{run}"))
}

/// The `k` values this run scored, taken from its own fingerprint rather than written out
/// by a caller — a second list would be the same metric computed over a different set of
/// `k` without anything saying so.
pub fn k_values(run: &serde_json::Value) -> Vec<usize> {
    serde_json::from_value(run["fingerprint"]["k_values"].clone())
        .unwrap_or_else(|e| panic!("eval JSON `fingerprint.k_values` did not parse: {e}\n{run}"))
}

/// One mean out of an [`AggregateMetrics`], by `k`.
///
/// Panics rather than defaulting: a `k` the run did not score would otherwise read as 0.0
/// and fail a gate for a reason that has nothing to do with retrieval.
pub fn at_k(metrics: &AggregateMetrics, k: usize, what: &str) -> f64 {
    *metrics.recall_at_k.get(&k).unwrap_or_else(|| {
        panic!(
            "the {what} group has no recall@{k}; the run scored {:?}",
            metrics.recall_at_k.keys().collect::<Vec<_>>()
        )
    })
}

/// How an expectation is named in a failure report.
///
/// One spelling, because a report that prints the path alone cannot explain a miss whose
/// cause is the heading: the file came back, the definition did not.
pub fn describe_expected(hit: &ExpectedHit) -> String {
    match &hit.heading {
        Some(h) => format!("{} ({h})", hit.path),
        None => hit.path.clone(),
    }
}

/// Human-readable list of the queries that did not rank an expected document first.
///
/// Included in every failure message: a nightly failure has to be diagnosable from the log
/// alone, without re-running a 2.3 GB model.
pub fn ranking_report(all: &[QueryResult]) -> String {
    let mut report = String::new();
    for q in all {
        let rr = q.metrics.reciprocal_rank;
        if rr >= 1.0 {
            continue;
        }
        // Every expected path, not just the first: `reciprocal_rank` is the rank of the
        // *earliest* expected hit, so naming one of several would leave the reader
        // guessing which one the rank refers to.
        let expected: Vec<String> = q.expected.iter().map(describe_expected).collect();
        let expected = if expected.is_empty() {
            "<none>".to_string()
        } else {
            expected.join(", ")
        };
        // The path alone, not the winning chunk's heading. A gate whose corpus makes the
        // heading part of the answer prints that separately (`code_eval_quality`'s
        // `winning_heading_report`), so that this line reads the same in both.
        let top1 = q
            .top_k
            .first()
            .map(|h| h.path.as_str())
            .unwrap_or("<nothing returned>");
        // "the earliest of them", because a multi-answer query names two and one rank
        // cannot belong to both. Writing "at rank 2" after a list of two reads as though
        // both sat there.
        let position = if rr > 0.0 {
            // `reciprocal_rank` is 1/rank of the first expected hit inside the retrieved
            // window; 0.0 means no expected hit was retrieved at all.
            format!("earliest of them at rank {}", (1.0 / rr).round() as i64)
        } else {
            "none of them inside the retrieved window".to_string()
        };
        report.push_str(&format!(
            "  {}: expected {expected}; {position}; top-1 was {top1}\n",
            q.id
        ));
    }
    if report.is_empty() {
        report.push_str("  (every query ranked its expected document first)\n");
    }
    report
}
