pub mod db;
pub mod embedder;
pub mod indexer;
pub mod markdown;
pub mod server;

use clap::{Parser, Subcommand};
use embedder::ModelChoice;
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(name = "kb-mcp")]
#[command(about = "MCP server for semantic search over a Markdown knowledge base")]
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
        kb_path: PathBuf,
        /// Embedding model to use (must match the one that built the index)
        #[arg(long, value_enum, default_value_t = ModelChoice::default())]
        model: ModelChoice,
    },
    /// Build or rebuild the search index
    Index {
        /// Path to the knowledge-base directory
        #[arg(long)]
        kb_path: PathBuf,
        /// Force full re-index. Required when switching `--model`.
        #[arg(long, default_value_t = false)]
        force: bool,
        /// Embedding model to use
        #[arg(long, value_enum, default_value_t = ModelChoice::default())]
        model: ModelChoice,
    },
    /// Show index status and statistics
    Status {
        /// Path to the knowledge-base directory
        #[arg(long)]
        kb_path: PathBuf,
    },
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

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Serve { kb_path, model } => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(async {
                server::run_server(&kb_path, model).await
            })?;
        }
        Commands::Index { kb_path, force, model } => {
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
    }

    Ok(())
}
