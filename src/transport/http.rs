//! [feature 18] Streamable HTTP transport runner.
//!
//! rmcp 1.x の `StreamableHttpService` を axum でマウントし、複数クライアント
//! 同時接続可能な MCP サーバを提供する。mount path は `/mcp` 固定 (MVP)。
//! `/healthz` は 200 "ok" を返すだけの health check。
//!
//! rmcp の service factory は session 毎に新しい Handler を要求するが、
//! 重いリソース (embedder / reranker / DB) は `KbServerShared` を Arc で
//! 共有するので重複ロードは起きない。

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::{Router, routing::get};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService,
    session::local::LocalSessionManager,
};

use crate::server::{KbServer, KbServerShared};

/// Start an axum-based HTTP server that exposes the MCP service at `/mcp`.
/// Blocks until SIGINT or a bind error. On bind failure, returns with a
/// helpful context message.
pub async fn run_http(addr: SocketAddr, shared: KbServerShared) -> Result<()> {
    // Session manager: LocalSessionManager keeps per-session state in memory.
    // Suitable for a single-process server (our deployment model).
    let session_manager = Arc::new(LocalSessionManager::default());

    // Service factory: invoked per new MCP session. Builds a fresh `KbServer`
    // handle that clones the Arc-shared heavy resources. The factory must
    // return `Result<_, std::io::Error>` per rmcp's trait. `shared` は以降
    // 使わないので clone せず move する (evaluator Med #4)。
    let factory_shared = shared;
    let factory = move || -> Result<KbServer, std::io::Error> {
        Ok(KbServer::from_shared(&factory_shared))
    };

    let mcp_service = StreamableHttpService::new(
        factory,
        session_manager,
        StreamableHttpServerConfig::default(),
    );

    let app = Router::new()
        .route("/healthz", get(healthz))
        .nest_service("/mcp", mcp_service);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| {
            format!(
                "failed to bind {addr}: is another kb-mcp instance running, or the \
                 port occupied?"
            )
        })?;
    eprintln!(
        "kb-mcp server ready (http transport, listening on {})",
        listener.local_addr().unwrap_or(addr)
    );

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            // Ctrl-C でグレースフルシャットダウン。Windows / Linux 両対応。
            let _ = tokio::signal::ctrl_c().await;
            eprintln!("kb-mcp: shutdown signal received");
        })
        .await
        .context("axum::serve failed")?;
    Ok(())
}

/// Health check endpoint. Always returns 200 with body "ok".
async fn healthz() -> &'static str {
    "ok"
}
