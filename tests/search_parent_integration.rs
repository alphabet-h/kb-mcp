//! End-to-end integration tests for the Parent retriever pipeline (PR-3).
//!
//! These tests exercise the **MCP `search` tool path** through a real
//! `kb-mcp serve --transport http` subprocess, mirroring the pattern in
//! `tests/search_mmr_integration.rs` (PR-2). The Parent retriever is wired
//! at the `apply_parent_retriever` call-site in
//! `src/server.rs::run_search_pipeline`, plus the post-expansion
//! `compute_match_spans` recomputation a few lines below; both paths are
//! covered here end-to-end.
//!
//! All tests are `#[ignore]` because they need:
//! - a built `kb-mcp` binary (`cargo build` first)
//! - the BGE-small model on disk (~130 MB DL on first run, cache hit
//!   afterwards because PR-2 already paid the download)
//! - network access for the initial model fetch
//! - a free TCP port + `curl` on `PATH`
//!
//! Run with:
//! ```text
//! cargo test --test search_parent_integration -- --ignored
//! ```
//!
//! ## What the 2 scenarios cover
//!
//! 1. `test_parent_expanded_from_set_when_enabled` —
//!    With `[search.parent_retriever] enabled = true`, a search hit on the
//!    middle chunk of a 3-chunk document returns `expanded_from = Some(...)`
//!    in the JSON response, and the `content` of the top hit contains text
//!    that lives in *adjacent* chunks (= the merge wire actually fired).
//!    This is the "wire is connected" smoke test.
//!
//! 2. `test_parent_match_spans_recomputed_on_expanded_content` —
//!    `expand_parent` defensively clears `match_spans = None` (`src/parent.rs`
//!    line 139 / 183), and the caller (`run_search_pipeline`) recomputes via
//!    `compute_match_spans` against the post-expansion `content`. We assert
//!    that `match_spans` comes back as `Some([...])` with at least one span
//!    whose offsets land **inside the expanded content** and slice to the
//!    query word — i.e. offsets are valid against the merged content, not
//!    leaked from the pre-expansion chunk.
//!
//! Helpers (kb_mcp_bin / pick_free_port / wait_http_200 / spawn_mcp_server /
//! ServerGuard / mcp_initialize / mcp_search_call / build_index /
//! extract_path_heading_order) are intentionally **copied** from
//! `tests/search_mmr_integration.rs` rather than extracted to `tests/common/`.
//! Reason: keeping each integration test file self-contained matches the
//! existing convention (`tests/http_transport.rs`, `tests/eval_cli.rs` also
//! roll their own helpers) and avoids tying PR-3 to a `tests/common/mcp.rs`
//! refactor that PR-2 deliberately deferred.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

mod common;
use common::temp::TempKbLayout;

// ---------------------------------------------------------------------------
// Helpers — copied verbatim from tests/search_mmr_integration.rs
// ---------------------------------------------------------------------------

fn kb_mcp_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_kb-mcp"))
}

fn pick_free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

fn wait_http_200(url: &str, deadline: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < deadline {
        let out = Command::new("curl")
            .args(["-s", "-o", "/dev/null", "-w", "%{http_code}", url])
            .output();
        if let Ok(out) = out
            && let Ok(code) = String::from_utf8(out.stdout)
            && code.trim() == "200"
        {
            return true;
        }
        thread::sleep(Duration::from_millis(300));
    }
    false
}

fn spawn_mcp_server(kb_path: &Path, config_path: &Path) -> (ServerGuard, String) {
    let port = pick_free_port();
    let bin = kb_mcp_bin();
    assert!(
        bin.exists(),
        "binary not found at {} — run `cargo build` first",
        bin.display()
    );

    let child = Command::new(&bin)
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "serve",
            "--kb-path",
            kb_path.to_str().unwrap(),
            "--transport",
            "http",
            "--port",
            &port.to_string(),
            "--no-watch",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn kb-mcp serve");

    let base = format!("http://127.0.0.1:{port}");
    let guard = ServerGuard { child: Some(child) };

    if !wait_http_200(&format!("{base}/healthz"), Duration::from_secs(60)) {
        panic!("/healthz did not return 200 within 60s — server failed to start");
    }
    (guard, base)
}

struct ServerGuard {
    child: Option<Child>,
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

fn mcp_initialize(base: &str) -> String {
    let init_body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"it","version":"0.1"}}}"#;
    let out = Command::new("curl")
        .args([
            "-s",
            "-i",
            "-X",
            "POST",
            "-H",
            "content-type: application/json",
            "-H",
            "accept: application/json, text/event-stream",
            "-d",
            init_body,
            &format!("{base}/mcp"),
        ])
        .output()
        .expect("curl initialize");
    assert!(
        out.status.success(),
        "curl initialize failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lower = stdout.to_ascii_lowercase();
    let h = "mcp-session-id:";
    let idx = lower
        .find(h)
        .unwrap_or_else(|| panic!("no mcp-session-id header in response:\n{stdout}"));
    let after = &stdout[idx + h.len()..];
    let end = after.find('\n').unwrap_or(after.len());
    after[..end].trim().trim_end_matches('\r').to_string()
}

fn mcp_search_call(
    base: &str,
    session_id: &str,
    arguments: serde_json::Value,
) -> serde_json::Value {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "search",
            "arguments": arguments,
        }
    });
    let body_str = serde_json::to_string(&body).unwrap();
    let out = Command::new("curl")
        .args([
            "-s",
            "-X",
            "POST",
            "-H",
            "content-type: application/json",
            "-H",
            "accept: application/json, text/event-stream",
            "-H",
            "MCP-Protocol-Version: 2025-06-18",
            "-H",
            &format!("Mcp-Session-Id: {session_id}"),
            "-d",
            &body_str,
            &format!("{base}/mcp"),
        ])
        .output()
        .expect("curl tools/call");
    assert!(
        out.status.success(),
        "curl tools/call failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let payload = stdout
        .lines()
        .filter_map(|line| {
            line.strip_prefix("data:")
                .or_else(|| line.strip_prefix("data: "))
                .map(|s| s.trim())
        })
        .find(|s| !s.is_empty())
        .unwrap_or_else(|| panic!("no non-empty `data:` line in SSE body:\n{stdout}"));
    let envelope: serde_json::Value = serde_json::from_str(payload)
        .unwrap_or_else(|e| panic!("invalid JSON-RPC envelope ({e}): {payload}"));
    let text = envelope
        .pointer("/result/content/0/text")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("missing result.content[0].text in envelope:\n{envelope}"));
    serde_json::from_str(text)
        .unwrap_or_else(|e| panic!("inner content text is not JSON ({e}): {text}"))
}

/// Run `kb-mcp index` against the given KB so the SQLite + vec index is
/// populated before we spawn the server. Uses BGE-small for speed.
fn build_index(kb_path: &Path) {
    let bin = kb_mcp_bin();
    let st = Command::new(&bin)
        .args([
            "index",
            "--kb-path",
            kb_path.to_str().unwrap(),
            "--model",
            "bge-small-en-v1.5",
        ])
        .status()
        .expect("kb-mcp index");
    assert!(st.success(), "kb-mcp index failed");
}

// ---------------------------------------------------------------------------
// Fixture: a single document with three sections each large enough to
// exceed `whole_doc_threshold_tokens = 100` (~400 byte content) so the
// adjacent-merge path fires (small-chunk path = whole-doc fallback would
// still satisfy assertion #1, but assertion #2 specifically wants
// adjacent merge — so we size sections accordingly).
//
// Marker words "alpha" / "beta" / "gamma" let assertion #1 verify that
// neighbors are present in the expanded content. Search query targets
// "beta" so the middle chunk wins and the merge spans both flanks.
//
// We add a second short doc to widen the candidate pool slightly so the
// search engine has at least 2 docs to choose between (FTS5 likes that).
// ---------------------------------------------------------------------------

fn build_test_kb(layout: &TempKbLayout) {
    layout.write(
        "doc1.md",
        concat!(
            "---\n",
            "title: Greek Letter Doc\n",
            "tags: [letters]\n",
            "---\n",
            "\n",
            "## alpha section\n",
            "\n",
            // ~600 byte body, repeated phrase to push token_count above 100
            "alpha discusses the first letter of the greek alphabet. ",
            "It comes before beta and gamma in the standard ordering. ",
            "We use alpha to mean leading or primary in many contexts. ",
            "Alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha. ",
            "alpha discusses the first letter of the greek alphabet. ",
            "It comes before beta and gamma in the standard ordering. ",
            "\n",
            "## beta section\n",
            "\n",
            "beta discusses the second letter of the greek alphabet. ",
            "It sits between alpha and gamma in the standard ordering. ",
            "Beta beta beta beta beta beta beta beta beta beta beta beta. ",
            "We sometimes call a release candidate a beta version. ",
            "beta discusses the second letter of the greek alphabet. ",
            "It sits between alpha and gamma in the standard ordering. ",
            "\n",
            "## gamma section\n",
            "\n",
            "gamma discusses the third letter of the greek alphabet. ",
            "It comes after alpha and beta in the standard ordering. ",
            "Gamma gamma gamma gamma gamma gamma gamma gamma gamma gamma. ",
            "Gamma rays are high energy electromagnetic radiation. ",
            "gamma discusses the third letter of the greek alphabet. ",
            "It comes after alpha and beta in the standard ordering. ",
            "\n",
        ),
    );
    layout.write(
        "doc2.md",
        concat!(
            "---\n",
            "title: Unrelated Side Doc\n",
            "tags: [misc]\n",
            "---\n",
            "\n",
            "## delta\n",
            "\n",
            "delta discusses the fourth letter, kept here only to ",
            "give the index a second document so the candidate pool ",
            "is not pathologically tiny.\n",
        ),
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Scenario 1: with `[search.parent_retriever] enabled = true`, a search hit
/// on the "beta" middle chunk returns `expanded_from = Some(...)` and the
/// expanded `content` includes the alpha/gamma flanking sections.
#[test]
#[ignore = "spawns kb-mcp serve which loads embedding model"]
fn test_parent_expanded_from_set_when_enabled() {
    let layout = TempKbLayout::new("kb-mcp-parent-it-expanded");
    build_test_kb(&layout);
    build_index(layout.kb());

    let cfg_path = layout.root().join("kb-mcp.toml");
    std::fs::write(
        &cfg_path,
        // whole_doc_threshold/max_expanded explicit for clarity even though
        // both match the defaults in src/config.rs.
        concat!(
            "[search.parent_retriever]\n",
            "enabled = true\n",
            "whole_doc_threshold_tokens = 100\n",
            "max_expanded_tokens = 2000\n",
        ),
    )
    .unwrap();

    let (_guard, base) = spawn_mcp_server(layout.kb(), &cfg_path);
    let session = mcp_initialize(&base);

    let resp = mcp_search_call(
        &base,
        &session,
        serde_json::json!({
            "query": "beta letter greek alphabet ordering",
            "limit": 3,
        }),
    );

    let results = resp
        .get("results")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| panic!("response missing `results` array: {resp}"));
    assert!(
        !results.is_empty(),
        "expected at least one result, got empty: {resp}"
    );

    // Find the hit on doc1.md (the multi-section doc). It should not
    // strictly need to be #1 — beta is the most relevant — but doc2.md
    // is deliberately unrelated, so doc1 ought to dominate. We pick the
    // first hit whose path is doc1.md to make this robust to any
    // tie-breaker drift.
    let doc1_hit = results
        .iter()
        .find(|h| {
            h.get("path")
                .and_then(|p| p.as_str())
                .is_some_and(|p| p.ends_with("doc1.md"))
        })
        .unwrap_or_else(|| panic!("no doc1.md hit in results: {results:?}"));

    let expanded = doc1_hit
        .get("expanded_from")
        .unwrap_or_else(|| panic!("doc1 hit missing `expanded_from` key: {doc1_hit}"));
    assert!(
        !expanded.is_null(),
        "expanded_from should be Some(...) when parent_retriever=true, got null: {doc1_hit}"
    );
    // The doc has 3 sections each with token_count ~120; threshold = 100, so
    // we expect Adjacent (not WholeDocument). But assert only that a `kind`
    // field is present (snake_case-tagged enum) — fixture chunking is the
    // markdown chunker's call, and we shouldn't couple this test to its
    // exact splitting.
    assert!(
        expanded.get("kind").and_then(|v| v.as_str()).is_some(),
        "expanded_from should be a tagged enum object with `kind`: {expanded}"
    );

    // Content should have grown to include neighboring sections. We hit the
    // beta chunk; the merge should pull in at least one of alpha / gamma.
    let content = doc1_hit
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("doc1 hit missing `content` string: {doc1_hit}"));
    assert!(
        content.contains("beta"),
        "expanded content should still contain the original `beta` text: {content:?}"
    );
    assert!(
        content.contains("alpha") || content.contains("gamma"),
        "expanded content should contain at least one neighbor (alpha or gamma); \
         got content snippet: {}",
        &content.chars().take(400).collect::<String>()
    );
}

/// Scenario 2: after `expand_parent` clears `match_spans = None` defensively,
/// the caller (`run_search_pipeline`) recomputes spans against the
/// post-expansion `content`. This test asserts that `match_spans` comes back
/// populated and that each span's `[start, end)` slice yields the (case-
/// folded) query word — i.e. the recomputation actually ran on the merged
/// content, not on the pre-expansion chunk.
///
/// We deliberately query a word that occurs in **multiple** sections so that
/// after the merge there is more than one match position, exercising the
/// multi-span path in `compute_match_spans`.
#[test]
#[ignore = "spawns kb-mcp serve which loads embedding model"]
fn test_parent_match_spans_recomputed_on_expanded_content() {
    let layout = TempKbLayout::new("kb-mcp-parent-it-spans");
    build_test_kb(&layout);
    build_index(layout.kb());

    let cfg_path = layout.root().join("kb-mcp.toml");
    std::fs::write(
        &cfg_path,
        concat!(
            "[search.parent_retriever]\n",
            "enabled = true\n",
            "whole_doc_threshold_tokens = 100\n",
            "max_expanded_tokens = 2000\n",
        ),
    )
    .unwrap();

    let (_guard, base) = spawn_mcp_server(layout.kb(), &cfg_path);
    let session = mcp_initialize(&base);

    // "alphabet" appears once in every greek-letter section (alpha / beta /
    // gamma). After parent expansion of the beta chunk, the merged content
    // should contain "alphabet" at least twice.
    let resp = mcp_search_call(
        &base,
        &session,
        serde_json::json!({
            "query": "alphabet",
            "limit": 3,
        }),
    );

    let results = resp
        .get("results")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| panic!("response missing `results`: {resp}"));
    assert!(
        !results.is_empty(),
        "expected at least one result, got empty: {resp}"
    );

    let doc1_hit = results
        .iter()
        .find(|h| {
            h.get("path")
                .and_then(|p| p.as_str())
                .is_some_and(|p| p.ends_with("doc1.md"))
        })
        .unwrap_or_else(|| panic!("no doc1.md hit in results: {results:?}"));

    // expanded_from must be set (parent retriever ran).
    let expanded = doc1_hit
        .get("expanded_from")
        .unwrap_or_else(|| panic!("doc1 hit missing expanded_from: {doc1_hit}"));
    assert!(
        !expanded.is_null(),
        "expanded_from should be Some(...) on doc1 hit: {doc1_hit}"
    );

    // match_spans must be present (= recomputed against expanded content,
    // NOT the defensive `None` left by expand_parent).
    let content = doc1_hit
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("doc1 hit missing content: {doc1_hit}"));
    let spans = doc1_hit
        .get("match_spans")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| {
            panic!(
                "match_spans should be present (and an array) on the expanded hit. \
                 If this fires it usually means run_search_pipeline did not recompute \
                 spans after apply_parent_retriever — see src/server.rs around the \
                 `for h in &mut hits {{ h.match_spans = compute_match_spans(...) }}` loop. \
                 doc1_hit = {doc1_hit}"
            )
        });
    assert!(
        !spans.is_empty(),
        "match_spans should contain at least one match for `alphabet` in expanded content. \
         content len = {}, spans = {spans:?}",
        content.len()
    );

    // Each span must slice to the (case-folded) query word *within* the
    // expanded content boundary. If recomputation had been skipped we would
    // see either None / [] (defensive clear left as-is) or stale offsets
    // pointing past the original chunk boundary.
    for (i, span) in spans.iter().enumerate() {
        let start = span
            .get("start")
            .and_then(|v| v.as_u64())
            .unwrap_or_else(|| panic!("span[{i}] missing `start`: {span}"))
            as usize;
        let end =
            span.get("end")
                .and_then(|v| v.as_u64())
                .unwrap_or_else(|| panic!("span[{i}] missing `end`: {span}")) as usize;
        assert!(
            end <= content.len(),
            "span[{i}] end ({end}) must be within expanded content (len {}); \
             possible stale offset from pre-expansion chunk. spans={spans:?}",
            content.len()
        );
        assert!(
            start < end,
            "span[{i}] start ({start}) must be < end ({end}): {span}"
        );
        let slice = content
            .get(start..end)
            .unwrap_or_else(|| panic!("span[{i}] {start}..{end} not on a char boundary"));
        assert_eq!(
            slice.to_ascii_lowercase(),
            "alphabet",
            "span[{i}] should slice to the query word (case-insensitive); got {slice:?}"
        );
    }
}
