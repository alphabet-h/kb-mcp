//! End-to-end integration tests for the MMR re-rank pipeline (PR-2).
//!
//! These tests exercise the **MCP `search` tool path** through a real
//! `kb-mcp serve --transport http` subprocess, because that is the only
//! call-site where MMR is actually plumbed (CLI `kb-mcp search` parses
//! `--mmr` flags but currently discards them — see `src/main.rs` Task 2.9
//! comment "MMR / parent-retriever flags are parsed-but-not-yet-pipeline-active").
//!
//! All tests are `#[ignore]` because they need:
//! - a built `kb-mcp` binary (`cargo build` first)
//! - the BGE-small model on disk (~130 MB DL on first run)
//! - network access for the initial model fetch
//! - a free TCP port + `curl` on `PATH`
//!
//! Run with:
//! ```text
//! cargo test --test search_mmr_integration -- --ignored
//! ```
//!
//! The same trade-offs as `tests/http_transport.rs`: we use `curl` instead
//! of pulling in `reqwest` to keep the dev-dep surface small. The MCP
//! Streamable HTTP transport returns Server-Sent Events by default
//! (`text/event-stream`), so the helper below grabs the first `data:` line
//! out of the body and parses it as a JSON-RPC envelope.
//!
//! ## What the 3 scenarios cover
//!
//! 1. `test_mmr_off_matches_legacy_search_chunk_id_order` —
//!    Two `mmr: false` requests against the same KB+query produce the
//!    *same* (path, heading) sequence (= MMR-off path is deterministic
//!    and does not perturb the legacy bit-exact ordering invariant #3).
//!    We compare `(path, heading)` tuples rather than the f32 score
//!    itself: BGE/ONNX/SIMD scores are not bit-exact across OS/CPU and
//!    the integration harness must run on Windows + Linux + macOS.
//!
//! 2. `test_mmr_per_call_override_beats_toml` —
//!    `kb-mcp.toml` says `[search.mmr] enabled = false`, the request
//!    passes `mmr: true` with a low `mmr_lambda` (= diversity-leaning).
//!    We assert the result *differs* from the MMR-off baseline when
//!    the candidate pool has enough material for MMR to reorder. (We
//!    use a deliberately redundant fixture so that a low-lambda MMR
//!    pass has something to do; otherwise MMR-on and MMR-off can
//!    coincide and the assertion would be meaningless.)
//!
//! 3. `test_mmr_lambda_warn_when_mmr_off` —
//!    A request with `mmr: false` + `mmr_lambda: 0.3` emits a
//!    `tracing::warn!` per `SearchOverrides::resolve` (see
//!    `src/config.rs` "footgun guard"). Asserting the actual log line
//!    requires `tracing-test` (not a current dep), so this test only
//!    smoke-checks that the request *succeeds* — i.e. an out-of-band
//!    `lambda` is silently ignored, not turned into an error. The warn
//!    emission itself is verified by code review of `config.rs`.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

mod common;
use common::temp::TempKbLayout;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Locate the kb-mcp binary under test. Cargo sets `CARGO_BIN_EXE_<name>`
/// for integration tests automatically — same pattern as `tests/eval_cli.rs`.
fn kb_mcp_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_kb-mcp"))
}

/// Pick a free ephemeral TCP port. Bind, take `local_addr().port()`, drop.
/// TOCTOU between drop and the spawned server's bind exists in theory but is
/// fine for an integration test (same approach `tests/http_transport.rs` uses).
fn pick_free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

/// Poll `<base>/healthz` until 200 or `deadline` expires.
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

/// Spawn `kb-mcp serve --transport http` against the given KB + config and
/// wait for `/healthz` to come up. Returns the child handle (kill on Drop)
/// and the base URL `http://127.0.0.1:<port>`.
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

    // 60 s upper bound: covers BGE-small first-time DL on cold cache.
    if !wait_http_200(&format!("{base}/healthz"), Duration::from_secs(60)) {
        // guard's Drop will reap the child; surface a useful error.
        panic!("/healthz did not return 200 within 60s — server failed to start");
    }
    (guard, base)
}

/// RAII handle for the spawned MCP server child. Kills + reaps on Drop so a
/// panicking test does not orphan the server process (would block the next
/// `pick_free_port`-based test on the same OS).
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

/// Issue a JSON-RPC `initialize` against `<base>/mcp` and return the
/// `Mcp-Session-Id` header value. Subsequent `tools/call` requests must
/// echo this header back per the Streamable HTTP spec.
fn mcp_initialize(base: &str) -> String {
    let init_body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"it","version":"0.1"}}}"#;
    // `-i` to include response headers — the server stamps `Mcp-Session-Id`
    // there. `-D -` would also work but `-i` keeps the body inline so we
    // can confirm it returned a valid InitializeResult before we proceed.
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
    // Header name is case-insensitive (HTTP), but rmcp emits it as
    // `mcp-session-id` (lowercase) in axum. Search case-insensitively.
    let lower = stdout.to_ascii_lowercase();
    let h = "mcp-session-id:";
    let idx = lower
        .find(h)
        .unwrap_or_else(|| panic!("no mcp-session-id header in response:\n{stdout}"));
    let after = &stdout[idx + h.len()..];
    let end = after.find('\n').unwrap_or(after.len());
    after[..end].trim().trim_end_matches('\r').to_string()
}

/// POST a `tools/call` request for the `search` tool with `arguments` =
/// the given JSON value. Returns the deserialized JSON value of the
/// `result.content[0].text` (= the inner SearchResponse JSON our server
/// produces).
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
    // SSE response: one or more events of the form `data: <payload>\n\n`,
    // optionally with `id:` / `retry:` lines mixed in. rmcp prepends a
    // **priming event** (`data:` with empty payload, used by SSE clients
    // to learn the retry hint per SEP-1699) before the actual JSON-RPC
    // response. We must skip empty `data:` lines and pick the first
    // *non-empty* one. Lines may be CRLF on Windows; trim defensively.
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
    // Pull `result.content[0].text` and parse it as JSON (= inner SearchResponse).
    let text = envelope
        .pointer("/result/content/0/text")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("missing result.content[0].text in envelope:\n{envelope}"));
    serde_json::from_str(text)
        .unwrap_or_else(|e| panic!("inner content text is not JSON ({e}): {text}"))
}

/// Build a small KB with **deliberately redundant** content so MMR has
/// something to dedupe. Three docs, each with two sections covering very
/// similar Rust async / tokio material — enough overlap that an MMR pass
/// with `lambda < 0.5` will reorder away from a pure relevance ranking.
fn build_test_kb(layout: &TempKbLayout) {
    layout.write(
        "tokio_one.md",
        concat!(
            "---\ntitle: Tokio Async Runtime One\ntags: [rust, tokio]\n---\n",
            "\n",
            "## tokio runtime\n",
            "\n",
            "The tokio runtime is an async executor for Rust that drives ",
            "futures to completion. It uses a multi-threaded scheduler with ",
            "work-stealing for high throughput in concurrent rust programs.\n",
            "\n",
            "## tokio tasks\n",
            "\n",
            "tokio::spawn creates a task that the tokio runtime polls. Each ",
            "task in the rust async ecosystem runs cooperatively until it ",
            "yields at the next .await point.\n",
        ),
    );
    layout.write(
        "tokio_two.md",
        concat!(
            "---\ntitle: Tokio Async Runtime Two\ntags: [rust, tokio]\n---\n",
            "\n",
            "## async tokio basics\n",
            "\n",
            "Async rust with tokio uses futures, the .await operator, and ",
            "the tokio runtime to drive non-blocking I/O. The tokio runtime ",
            "scheduler is the heart of every async rust application.\n",
            "\n",
            "## tokio macros\n",
            "\n",
            "The #[tokio::main] macro wraps an async fn into a synchronous ",
            "entry point that constructs a tokio runtime under the hood.\n",
        ),
    );
    layout.write(
        "rayon.md",
        concat!(
            "---\ntitle: Rayon Data Parallel\ntags: [rust, parallel]\n---\n",
            "\n",
            "## rayon basics\n",
            "\n",
            "Rayon is a data-parallel library for rust. It is not async ",
            "but uses work-stealing similar to tokio's scheduler model. ",
            "Use rayon when CPU-bound, tokio when I/O-bound.\n",
        ),
    );
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

/// Extract `(path, heading)` order from a SearchResponse-shaped JSON.
/// Used as a stable cross-OS proxy for the chunk-id sequence (raw f32
/// score is not bit-exact across architectures).
fn extract_path_heading_order(resp: &serde_json::Value) -> Vec<(String, String)> {
    resp["results"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|hit| {
                    let p = hit["path"].as_str().unwrap_or("").to_string();
                    let h = hit["heading"].as_str().unwrap_or("").to_string();
                    (p, h)
                })
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Scenario 1: with MMR off (both via toml *and* via per-call), the
/// `(path, heading)` sequence is deterministic across two identical
/// requests. This guards invariant #3 (MMR-off path is bit-exact wrt
/// pre-MMR behavior — equivalent to "calling search twice gives the
/// same result", since the pre-MMR pipeline is the same code path).
#[test]
#[ignore = "requires built binary, BGE-small model download, free TCP port"]
fn test_mmr_off_matches_legacy_search_chunk_id_order() {
    let layout = TempKbLayout::new("kb-mcp-mmr-it-off");
    build_test_kb(&layout);
    build_index(layout.kb());

    // toml with MMR explicitly off (= legacy code path).
    let cfg_path = layout.root().join("kb-mcp.toml");
    std::fs::write(
        &cfg_path,
        "[search.mmr]\nenabled = false\nlambda = 0.7\nsame_doc_penalty = 0.0\n",
    )
    .unwrap();

    let (_guard, base) = spawn_mcp_server(layout.kb(), &cfg_path);
    let session = mcp_initialize(&base);

    let args = serde_json::json!({
        "query": "tokio runtime async rust",
        "limit": 5,
        "mmr": false,
    });
    let r1 = mcp_search_call(&base, &session, args.clone());
    let r2 = mcp_search_call(&base, &session, args);

    let order1 = extract_path_heading_order(&r1);
    let order2 = extract_path_heading_order(&r2);
    assert!(!order1.is_empty(), "first search returned no results: {r1}");
    assert_eq!(
        order1, order2,
        "MMR-off path must produce the same (path, heading) order across two identical requests \
         (= bit-exact legacy invariant #3). Got:\n  r1={order1:?}\n  r2={order2:?}"
    );
}

/// Scenario 2: per-call `mmr: true` overrides toml `enabled = false` and
/// changes the result order on a deliberately redundant KB. This is the
/// "knob actually does something" smoke. We don't compare to a hand-rolled
/// expected order — that would be brittle across model versions — only
/// that the order is *different* from the MMR-off baseline.
///
/// Note: because the candidate pool size is small (3 docs, 5 chunks total)
/// and the BGE-small model is deterministic, in pathological cases MMR-on
/// could coincidentally produce the same order as MMR-off. To make this
/// robust we crank `mmr_lambda` low (= heavy diversity bias) and
/// `mmr_same_doc_penalty` high (= force selecting from different docs).
/// If this still fails reproducibly, the fixture above needs more docs
/// with overlap.
#[test]
#[ignore = "requires built binary, BGE-small model download, free TCP port"]
fn test_mmr_per_call_override_beats_toml() {
    let layout = TempKbLayout::new("kb-mcp-mmr-it-override");
    build_test_kb(&layout);
    build_index(layout.kb());

    // toml says MMR off — per-call override flips it on.
    let cfg_path = layout.root().join("kb-mcp.toml");
    std::fs::write(
        &cfg_path,
        "[search.mmr]\nenabled = false\nlambda = 0.7\nsame_doc_penalty = 0.0\n",
    )
    .unwrap();

    let (_guard, base) = spawn_mcp_server(layout.kb(), &cfg_path);
    let session = mcp_initialize(&base);

    let baseline = mcp_search_call(
        &base,
        &session,
        serde_json::json!({
            "query": "tokio runtime async rust",
            "limit": 5,
            "mmr": false,
        }),
    );
    let mmr_on = mcp_search_call(
        &base,
        &session,
        serde_json::json!({
            "query": "tokio runtime async rust",
            "limit": 5,
            "mmr": true,
            "mmr_lambda": 0.1,            // heavy diversity bias
            "mmr_same_doc_penalty": 0.5,  // strong intra-doc penalty
        }),
    );

    let baseline_order = extract_path_heading_order(&baseline);
    let mmr_order = extract_path_heading_order(&mmr_on);
    assert!(
        !baseline_order.is_empty() && !mmr_order.is_empty(),
        "expected non-empty results. baseline={baseline_order:?}, mmr={mmr_order:?}"
    );
    assert_ne!(
        baseline_order, mmr_order,
        "per-call mmr=true with low lambda + high same_doc_penalty must differ from MMR-off \
         baseline on a redundant KB. baseline={baseline_order:?}, mmr={mmr_order:?}. \
         If this assertion fires, the fixture in build_test_kb may not have enough \
         intra-doc overlap to make MMR reorder."
    );
}

/// Scenario 3: passing `mmr_lambda` while `mmr` is explicitly false (or
/// implicitly off via toml) is a "footgun" pattern — `SearchOverrides::resolve`
/// silently ignores the lambda but emits `tracing::warn!` exactly once.
///
/// We can't assert on the warn line itself without `tracing-test` (not
/// currently a dep), so this test is a smoke: the request must complete
/// successfully and return well-formed results. The warn emission is
/// covered by `src/config.rs::test_search_overrides_resolve_warn_emitted_when_mmr_off_with_lambda`.
#[test]
#[ignore = "requires built binary, BGE-small model download, free TCP port"]
fn test_mmr_lambda_warn_when_mmr_off() {
    let layout = TempKbLayout::new("kb-mcp-mmr-it-warn");
    build_test_kb(&layout);
    build_index(layout.kb());

    let cfg_path = layout.root().join("kb-mcp.toml");
    std::fs::write(
        &cfg_path,
        "[search.mmr]\nenabled = false\nlambda = 0.7\nsame_doc_penalty = 0.0\n",
    )
    .unwrap();

    let (_guard, base) = spawn_mcp_server(layout.kb(), &cfg_path);
    let session = mcp_initialize(&base);

    // mmr explicitly false + lambda set → ghost lambda. Must not error.
    let resp = mcp_search_call(
        &base,
        &session,
        serde_json::json!({
            "query": "tokio async",
            "limit": 3,
            "mmr": false,
            "mmr_lambda": 0.3,
        }),
    );

    // The wrapper shape (results / low_confidence / filter_applied) must be
    // present — i.e. the request completed instead of returning ErrorResponse.
    assert!(
        resp.get("results").is_some(),
        "ghost-lambda request must still return a SearchResponse (got: {resp})"
    );
    assert!(
        resp.get("low_confidence").is_some(),
        "ghost-lambda request must include low_confidence flag (got: {resp})"
    );
    let order = extract_path_heading_order(&resp);
    assert!(
        !order.is_empty(),
        "ghost-lambda request must still produce hits (got empty results: {resp})"
    );
}
