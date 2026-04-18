pub mod config;
pub mod db;
pub mod embedder;
pub mod graph;
pub mod indexer;
pub mod markdown;
pub mod quality;
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
        ///
        /// Default: `true` (when `--reranker` is set). Has no effect while
        /// `--reranker none`. Omit this flag to take the default or the
        /// `rerank_by_default` value from `kb-mcp.toml`.
        #[arg(long, value_parser = clap::value_parser!(bool))]
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
    /// Expand a connection graph starting from a document path.
    /// Useful for chained context discovery from the CLI.
    Graph {
        /// Start document path (relative to kb-path)
        #[arg(long)]
        start: String,
        /// Path to the knowledge-base directory
        #[arg(long)]
        kb_path: Option<PathBuf>,
        /// Embedding model (must match the index; defaults to config or built-in)
        #[arg(long, value_enum)]
        model: Option<ModelChoice>,
        /// BFS depth (default 2, clamped to max 3)
        #[arg(long, default_value_t = graph::DEFAULT_DEPTH)]
        depth: u32,
        /// Max fan-out per node (default 5, clamped to max 20)
        #[arg(long = "fan-out", default_value_t = graph::DEFAULT_FAN_OUT)]
        fan_out: u32,
        /// Minimum cosine similarity 0.0-1.0 (default 0.3)
        #[arg(long = "min-similarity", default_value_t = graph::DEFAULT_MIN_SIMILARITY)]
        min_similarity: f32,
        /// Seed strategy (all_chunks | centroid)
        #[arg(long = "seed-strategy", value_enum, default_value_t = CliSeedStrategy::AllChunks)]
        seed_strategy: CliSeedStrategy,
        /// Filter by category
        #[arg(long)]
        category: Option<String>,
        /// Filter by topic
        #[arg(long)]
        topic: Option<String>,
        /// Comma-separated paths to exclude from the graph (in addition to
        /// the start path which is always excluded).
        #[arg(long, value_delimiter = ',')]
        exclude: Vec<String>,
        /// Collapse same-path hits so each document appears at most once.
        #[arg(long = "dedup-by-path", default_value_t = false)]
        dedup_by_path: bool,
        /// Output format
        #[arg(long, value_enum, default_value_t = SearchFormat::Json)]
        format: SearchFormat,
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
        /// Override quality filter threshold (0.0-1.0). Defaults to the
        /// `[quality_filter].threshold` in kb-mcp.toml (0.3 if unset).
        #[arg(long = "min-quality")]
        min_quality: Option<f32>,
        /// Disable the quality filter for this query (shorthand for
        /// `--min-quality 0.0`).
        #[arg(long = "include-low-quality", default_value_t = false)]
        include_low_quality: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum SearchFormat {
    /// JSON array of hit records (default, machine-readable)
    Json,
    /// Concatenated text blocks (title / path#heading / content, separated by ---)
    Text,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum CliSeedStrategy {
    AllChunks,
    Centroid,
}

impl From<CliSeedStrategy> for graph::SeedStrategy {
    fn from(c: CliSeedStrategy) -> Self {
        match c {
            CliSeedStrategy::AllChunks => graph::SeedStrategy::AllChunks,
            CliSeedStrategy::Centroid => graph::SeedStrategy::Centroid,
        }
    }
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

            let exclude_headings = cfg.exclude_headings.clone();
            let quality_threshold = cfg
                .quality_filter
                .clone()
                .unwrap_or_default()
                .effective_threshold();
            let best_practice_templates = cfg
                .best_practice
                .clone()
                .unwrap_or_default()
                .path_templates;
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(async {
                server::run_server(
                    &kb_path,
                    model,
                    reranker,
                    rerank_by_default,
                    exclude_headings,
                    quality_threshold,
                    best_practice_templates,
                )
                .await
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
            let result = indexer::rebuild_index(
                &db,
                &mut embedder,
                &kb_path,
                force,
                cfg.exclude_headings.as_deref(),
            )?;
            eprintln!(
                "Done in {}ms: {} docs ({} updated, {} renamed, {} deleted), {} chunks",
                result.duration_ms,
                result.total_documents,
                result.updated,
                result.renamed,
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
            let total_docs = db.document_count()?;
            let total_chunks = db.chunk_count()?;
            eprintln!("Documents: {total_docs}");
            eprintln!("Chunks: {total_chunks}");
            // feature 13: 設定済みの threshold で filter される件数を表示
            let qf = cfg.quality_filter.clone().unwrap_or_default();
            let threshold = qf.effective_threshold();
            if threshold > 0.0 {
                let (above, below) = db.chunk_count_by_quality(threshold)?;
                eprintln!(
                    "Quality filter (threshold={threshold}): {above} passing, {below} below threshold"
                );
            }
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
            min_quality,
            include_low_quality,
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

            let server_default = cfg
                .quality_filter
                .clone()
                .unwrap_or_default()
                .effective_threshold();
            let effective_min_quality = quality::resolve_effective_threshold(
                include_low_quality,
                min_quality,
                server_default,
            );

            let results = if reranker_choice.is_enabled() {
                let candidates = db.search_hybrid_candidates(
                    &query,
                    &query_embedding,
                    limit.saturating_mul(5).max(50),
                    category.as_deref(),
                    topic.as_deref(),
                    effective_min_quality,
                )?;
                if let Some(mut r) = embedder::Reranker::try_new(reranker_choice)? {
                    r.rerank_candidates(&query, candidates, limit)?
                } else {
                    db.search_hybrid(&query, &query_embedding, limit, category.as_deref(), topic.as_deref(), effective_min_quality)?
                }
            } else {
                db.search_hybrid(&query, &query_embedding, limit, category.as_deref(), topic.as_deref(), effective_min_quality)?
            };

            print_search_results(results, format);
        }
        Commands::Graph {
            start,
            kb_path,
            model,
            depth,
            fan_out,
            min_similarity,
            seed_strategy,
            category,
            topic,
            exclude,
            dedup_by_path,
            format,
        } => {
            let kb_path = require_kb_path(kb_path, cfg.kb_path.clone())?;
            let model = model.or(cfg.model).unwrap_or_default();

            let db_path = resolve_db_path(&kb_path);
            // Status と同じく、DB がまだ作られていない状態を親切なエラーで弾く。
            if !db_path.exists() {
                anyhow::bail!(
                    "No index found at {}. Run `kb-mcp index --kb-path {}` first.",
                    db_path.display(),
                    kb_path.display()
                );
            }
            let db = db::Database::open(&db_path.to_string_lossy())?;
            db.verify_embedding_meta(model.model_id(), model.dimension() as u32)?;

            let opts = graph::GraphOptions {
                depth: depth.min(graph::MAX_DEPTH),
                fan_out: fan_out.min(graph::MAX_FAN_OUT),
                min_similarity: min_similarity.clamp(0.0, 1.0),
                seed_strategy: seed_strategy.into(),
                category,
                topic,
                exclude_paths: exclude,
                dedup_by_path,
                min_quality: cfg
                    .quality_filter
                    .clone()
                    .unwrap_or_default()
                    .effective_threshold(),
            };
            let g = graph::build_connection_graph(&db, &start, &opts)?;
            print_graph(g, format);
        }
    }

    Ok(())
}

fn print_graph(g: graph::ConnectionGraph, format: SearchFormat) {
    match format {
        SearchFormat::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&g).unwrap_or_else(|_| "{}".into())
            );
        }
        SearchFormat::Text => {
            println!("# Connection graph from: {}", g.start_path);
            println!(
                "nodes={} max_depth={} knn_queries={} duration_ms={}",
                g.stats.total_nodes,
                g.stats.max_depth_reached,
                g.stats.knn_queries,
                g.stats.duration_ms
            );
            for n in &g.nodes {
                println!();
                let parent = n
                    .parent_id
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "-".into());
                let heading = n.heading.as_deref().unwrap_or("");
                println!(
                    "[{:>3}] depth={} parent={} score={:.3}  {}#{}",
                    n.node_id, n.depth, parent, n.score, n.path, heading
                );
                if let Some(t) = &n.title {
                    println!("     title: {t}");
                }
                println!("     {}", n.snippet);
            }
        }
    }
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
