pub mod config;
pub mod db;
pub mod embedder;
pub mod indexer;
pub mod markdown;
pub mod server;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use config::Config;
use embedder::{ModelChoice, RerankerChoice};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(name = "kb-mcp")]
#[command(
    about = "MCP server for semantic search over a Markdown knowledge base",
    long_about = "MCP server for semantic search over a Markdown knowledge base.\n\
                  \n\
                  Any of the options below can be provided via `kb-mcp.toml` placed\n\
                  in the same directory as the binary. CLI arguments override the file."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the MCP server (stdio transport)
    Serve {
        /// Path to the knowledge-base directory
        #[arg(long)]
        kb_path: Option<PathBuf>,
        /// Embedding model to use (must match the one that built the index)
        #[arg(long, value_enum)]
        model: Option<ModelChoice>,
        /// Optional cross-encoder reranker applied after RRF hybrid search.
        /// Default: none (disabled). Enabling requires a model download.
        #[arg(long, value_enum)]
        reranker: Option<RerankerChoice>,
        /// When reranker is enabled, apply it by default for every `search` call
        /// unless the tool invocation explicitly passes `rerank: false`.
        #[arg(long)]
        rerank_by_default: Option<bool>,
    },
    /// Build or rebuild the search index
    Index {
        /// Path to the knowledge-base directory
        #[arg(long)]
        kb_path: Option<PathBuf>,
        /// Force full re-index. Required when switching `--model`.
        #[arg(long, default_value_t = false)]
        force: bool,
        /// Embedding model to use
        #[arg(long, value_enum)]
        model: Option<ModelChoice>,
    },
    /// Show index status and statistics
    Status {
        /// Path to the knowledge-base directory
        #[arg(long)]
        kb_path: Option<PathBuf>,
    },
    /// One-shot search from the command line (no MCP transport).
    /// Useful for shell scripts / skill bins where invoking the binary
    /// directly is simpler than talking MCP stdio.
    Search {
        /// Search query text (positional)
        query: String,
        /// Path to the knowledge-base directory
        #[arg(long)]
        kb_path: Option<PathBuf>,
        /// Embedding model (must match the index; defaults to config or built-in)
        #[arg(long, value_enum)]
        model: Option<ModelChoice>,
        /// Optional cross-encoder reranker. Adds 300-700ms but improves precision.
        #[arg(long, value_enum)]
        reranker: Option<RerankerChoice>,
        /// Max results to return
        #[arg(long, default_value_t = 5)]
        limit: u32,
        /// Filter by category (e.g. "deep-dive", "ai-news")
        #[arg(long)]
        category: Option<String>,
        /// Filter by topic (e.g. "mcp", "chromadb")
        #[arg(long)]
        topic: Option<String>,
        /// Output format: json (machine-readable) or text (LLM-friendly)
        #[arg(long, value_enum, default_value_t = SearchFormat::Json)]
        format: SearchFormat,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum SearchFormat {
    /// JSON array of hit records (default, machine-readable)
    Json,
    /// Concatenated text blocks (title / path#heading / content, separated by ---)
    Text,
}

/// Resolve the database path from a knowledge-base directory.
///
/// The `.kb-mcp.db` file is placed in the **parent** of `kb_path`
/// (i.e. the repository root when `kb_path` is `knowledge-base/`).
pub fn resolve_db_path(kb_path: &Path) -> PathBuf {
    kb_path
        .parent()
        .unwrap_or(Path::new("."))
        .join(".kb-mcp.db")
}

/// `kb_path` が指定されていなければエラー。(CLI / config どちらからも無い場合)
fn require_kb_path(cli_value: Option<PathBuf>, config_default: Option<PathBuf>) -> Result<PathBuf> {
    cli_value
        .or(config_default)
        .context("--kb-path is required (pass on the command line or set `kb_path` in kb-mcp.toml)")
}

fn main() -> anyhow::Result<()> {
    // 設定ファイルを先に読み、FASTEMBED_CACHE_DIR を embedder 初期化より前に env 反映する。
    let cfg = Config::load_alongside_binary()?;
    cfg.apply_cache_dir_env();

    let cli = Cli::parse();

    match cli.command {
        Commands::Serve {
            kb_path,
            model,
            reranker,
            rerank_by_default,
        } => {
            let kb_path = require_kb_path(kb_path, cfg.kb_path.clone())?;
            let model = model.or(cfg.model).unwrap_or_default();
            let reranker = reranker.or(cfg.reranker).unwrap_or_default();
            // rerank_by_default の CLI 既定値は `true` (reranker 有効時のみ意味を持つ)。
            let rerank_by_default = rerank_by_default
                .or(cfg.rerank_by_default)
                .unwrap_or(true);

            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(async {
                server::run_server(&kb_path, model, reranker, rerank_by_default).await
            })?;
        }
        Commands::Index {
            kb_path,
            force,
            model,
        } => {
            let kb_path = require_kb_path(kb_path, cfg.kb_path.clone())?;
            let model = model.or(cfg.model).unwrap_or_default();

            let db_path = resolve_db_path(&kb_path);
            let db = db::Database::open(&db_path.to_string_lossy())?;
            // モデル DL (BGE-M3 なら ~2.3 GB) の前に meta 整合性を先に確認する。
            // そうしないと不整合時にユーザが不要な DL を待たされる。
            let dim = model.dimension() as u32;
            if !force {
                db.verify_embedding_meta(model.model_id(), dim)?;
            }
            let mut embedder = embedder::Embedder::with_model(model)?;
            if force {
                db.reset_for_model(embedder.model_id(), dim)?;
            }
            eprintln!("Indexing {}...", kb_path.display());
            let result = indexer::rebuild_index(&db, &mut embedder, &kb_path, force)?;
            eprintln!(
                "Done in {}ms: {} docs ({} updated, {} deleted), {} chunks",
                result.duration_ms,
                result.total_documents,
                result.updated,
                result.deleted,
                result.total_chunks
            );
        }
        Commands::Status { kb_path } => {
            let kb_path = require_kb_path(kb_path, cfg.kb_path.clone())?;

            let db_path = resolve_db_path(&kb_path);
            if !db_path.exists() {
                eprintln!(
                    "No index found. Run `kb-mcp index --kb-path {}` first.",
                    kb_path.display()
                );
                return Ok(());
            }
            let db = db::Database::open(&db_path.to_string_lossy())?;
            eprintln!("Documents: {}", db.document_count()?);
            eprintln!("Chunks: {}", db.chunk_count()?);
        }
        Commands::Search {
            query,
            kb_path,
            model,
            reranker,
            limit,
            category,
            topic,
            format,
        } => {
            let kb_path = require_kb_path(kb_path, cfg.kb_path.clone())?;
            let model = model.or(cfg.model).unwrap_or_default();
            let reranker_choice = reranker.or(cfg.reranker).unwrap_or_default();

            let db_path = resolve_db_path(&kb_path);
            let db = db::Database::open(&db_path.to_string_lossy())?;
            let dim = model.dimension() as u32;
            db.verify_embedding_meta(model.model_id(), dim)?;

            let mut embedder = embedder::Embedder::with_model(model)?;
            let query_embedding = embedder.embed_single(&query)?;

            let results = if reranker_choice.is_enabled() {
                let candidates = db.search_hybrid_candidates(
                    &query,
                    &query_embedding,
                    limit.saturating_mul(5).max(50),
                    category.as_deref(),
                    topic.as_deref(),
                )?;
                if let Some(mut r) = embedder::Reranker::try_new(reranker_choice)? {
                    r.rerank_candidates(&query, candidates, limit)?
                } else {
                    // Reranker::try_new returned None even though is_enabled was true;
                    // fall back to hybrid.
                    db.search_hybrid(&query, &query_embedding, limit, category.as_deref(), topic.as_deref())?
                }
            } else {
                db.search_hybrid(&query, &query_embedding, limit, category.as_deref(), topic.as_deref())?
            };

            print_search_results(results, format);
        }
    }

    Ok(())
}

fn print_search_results(results: Vec<db::SearchResult>, format: SearchFormat) {
    // db::SearchHit への移し替えで MCP `search` ツール出力と shape が一致する。
    let hits: Vec<db::SearchHit> = results.into_iter().map(Into::into).collect();
    match format {
        SearchFormat::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&hits).unwrap_or_else(|_| "[]".into())
            );
        }
        SearchFormat::Text => {
            for (i, h) in hits.iter().enumerate() {
                if i > 0 {
                    println!("\n---\n");
                }
                let title = h.title.as_deref().unwrap_or("(no title)");
                let heading = h.heading.as_deref().unwrap_or("");
                println!("# {title}");
                if heading.is_empty() {
                    println!("{}", h.path);
                } else {
                    println!("{}#{heading}", h.path);
                }
                println!("score: {:.4}", h.score);
                println!();
                println!("{}", h.content);
            }
        }
    }
}
