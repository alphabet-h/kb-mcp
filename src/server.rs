use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::Result;
use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::schemars;
use rmcp::{tool, tool_router};
use serde::{Deserialize, Serialize};

use crate::db::{Database, SearchHit};
use crate::embedder::{Embedder, ModelChoice, Reranker, RerankerChoice};
use crate::graph::{self, GraphOptions, SeedStrategy};
use crate::{indexer, markdown};

// ---------------------------------------------------------------------------
// Server struct
// ---------------------------------------------------------------------------

pub struct KbServer {
    db: Mutex<Database>,
    embedder: Mutex<Embedder>,
    reranker: Mutex<Option<Reranker>>,
    rerank_by_default: bool,
    kb_path: PathBuf,
    /// `rebuild_index` ツールで markdown パース時に使う除外見出し。
    /// `None` のときは [`markdown::DEFAULT_EXCLUDED_HEADINGS`] を使う。
    exclude_headings: Option<Vec<String>>,
    /// feature 13: 既定の品質フィルタしきい値。`search` / graph で適用。
    /// 0.0 ならフィルタ無効。
    quality_threshold: f32,
    /// feature 16: `get_best_practice` のパス候補テンプレート。
    /// 先頭から順に `{target}` を置換してファイルを探し、最初に存在した
    /// ものを読む。kb-mcp.toml 未指定時は legacy 既定
    /// `["best-practices/{target}/PERFECT.md"]`。
    best_practice_templates: Vec<String>,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

// ---------------------------------------------------------------------------
// Tool parameter types
// ---------------------------------------------------------------------------

#[derive(Deserialize, schemars::JsonSchema, Default)]
struct SearchParams {
    /// The search query text
    query: String,
    /// Maximum number of results to return (default: 5)
    limit: Option<u32>,
    /// Filter by category (e.g. "deep-dive", "ai-news", "tech-watch")
    category: Option<String>,
    /// Filter by topic (e.g. "mcp", "chromadb")
    topic: Option<String>,
    /// Override the server default for reranking. Requires the server to have
    /// been started with `--reranker <model>` (otherwise ignored).
    rerank: Option<bool>,
    /// Override the quality filter threshold for this query (0.0-1.0). If
    /// omitted, the server default (from `kb-mcp.toml` / CLI) is used.
    min_quality: Option<f32>,
    /// If true, disable the quality filter for this query (equivalent to
    /// `min_quality: 0.0`, but more explicit).
    include_low_quality: Option<bool>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
struct GetDocumentParams {
    /// Relative path to the document within knowledge-base/ (e.g. "deep-dive/mcp/overview.md")
    path: String,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
struct GetBestPracticeParams {
    /// Target name (e.g. "claude-code")
    target: String,
    /// Optional: extract only this h2 section (case-insensitive match)
    category: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
struct RebuildIndexParams {
    /// Force full re-index ignoring existing hashes
    force: Option<bool>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
struct GetConnectionGraphParams {
    /// Relative path of the starting document within knowledge-base/
    /// (e.g. "deep-dive/mcp/overview.md"). Must be already indexed.
    path: String,
    /// BFS depth. 1 = direct neighbors only, 2 = neighbors of neighbors (default: 2, max: 3)
    depth: Option<u32>,
    /// Max neighbors fanned out per node at each hop (default: 5, max: 20)
    fan_out: Option<u32>,
    /// Minimum cosine similarity (0.0-1.0) for a neighbor to be included
    /// (default: 0.3). Lower = looser chain.
    min_similarity: Option<f32>,
    /// Seed strategy: "all_chunks" (default, expand from every chunk of
    /// the start doc) or "centroid" (average the start doc's embeddings).
    seed_strategy: Option<String>,
    /// Filter by category (applied to all discovered nodes)
    category: Option<String>,
    /// Filter by topic
    topic: Option<String>,
    /// Paths to exclude from results. The start path itself is always excluded.
    exclude_paths: Option<Vec<String>>,
    /// If true, collapse same-path hits so each document appears at most once.
    /// Default: false (allow multiple chunks from the same doc).
    dedup_by_path: Option<bool>,
}

// ---------------------------------------------------------------------------
// Response types (serialized as JSON text)
// ---------------------------------------------------------------------------
//
// `search` ツールの出力形状は `db::SearchHit` に統一しているので、ここでは
// 個別に定義しない (CLI の `search` サブコマンドと schema 一致)。

#[derive(Serialize)]
struct TopicEntry {
    category: Option<String>,
    topic: Option<String>,
    file_count: u32,
    last_updated: Option<String>,
    titles: Vec<String>,
}

#[derive(Serialize)]
struct DocumentResponse {
    path: String,
    title: Option<String>,
    date: Option<String>,
    topic: Option<String>,
    tags: Vec<String>,
    content: String,
}

#[derive(Serialize)]
struct BestPracticeResponse {
    target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    category: Option<String>,
    content: String,
}

#[derive(Serialize)]
struct IndexStats {
    total_documents: u32,
    updated: u32,
    deleted: u32,
    total_chunks: u32,
    duration_ms: u64,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

// ---------------------------------------------------------------------------
// Tool implementations
// ---------------------------------------------------------------------------

#[tool_router(server_handler)]
impl KbServer {
    #[tool(
        name = "search",
        description = "Hybrid search (vector + FTS5 full-text, merged via Reciprocal Rank Fusion) over the knowledge base. The `score` field is the RRF score (higher = better)."
    )]
    async fn search(
        &self,
        Parameters(params): Parameters<SearchParams>,
    ) -> String {
        let limit = params.limit.unwrap_or(5);

        // Embed the query
        let query_embedding = {
            let mut embedder = self.embedder.lock().unwrap();
            match embedder.embed_single(&params.query) {
                Ok(emb) => emb,
                Err(e) => {
                    return serde_json::to_string_pretty(&ErrorResponse {
                        error: format!("Failed to embed query: {e}"),
                    })
                    .unwrap_or_default();
                }
            }
        };

        // Search the DB (optionally followed by reranking)
        let mut reranker_guard = self.reranker.lock().unwrap();
        let use_rerank =
            params.rerank.unwrap_or(self.rerank_by_default) && reranker_guard.is_some();

        let effective_min_quality = crate::quality::resolve_effective_threshold(
            params.include_low_quality.unwrap_or(false),
            params.min_quality,
            self.quality_threshold,
        );

        let db = self.db.lock().unwrap();
        let search_outcome: anyhow::Result<Vec<crate::db::SearchResult>> = if use_rerank {
            // rerank 入力用に candidates を取得、score は cross-encoder で上書き
            match db.search_hybrid_candidates(
                &params.query,
                &query_embedding,
                limit.saturating_mul(5).max(50),
                params.category.as_deref(),
                params.topic.as_deref(),
                effective_min_quality,
            ) {
                Ok(cands) => {
                    let r = reranker_guard.as_mut().expect("reranker Some checked above");
                    r.rerank_candidates(&params.query, cands, limit)
                }
                Err(e) => Err(e),
            }
        } else {
            db.search_hybrid(
                &params.query,
                &query_embedding,
                limit,
                params.category.as_deref(),
                params.topic.as_deref(),
                effective_min_quality,
            )
        };

        match search_outcome {
            Ok(results) => {
                let hits: Vec<SearchHit> = results.into_iter().map(Into::into).collect();
                serde_json::to_string_pretty(&hits).unwrap_or_default()
            }
            Err(e) => serde_json::to_string_pretty(&ErrorResponse {
                error: format!("Search failed: {e}. Try running rebuild_index first."),
            })
            .unwrap_or_default(),
        }
    }

    #[tool(
        name = "list_topics",
        description = "List all indexed topics and categories with document counts."
    )]
    async fn list_topics(&self) -> String {
        let db = self.db.lock().unwrap();
        match db.list_topics() {
            Ok(topics) => {
                let entries: Vec<TopicEntry> = topics
                    .into_iter()
                    .map(|t| TopicEntry {
                        category: t.category,
                        topic: t.topic,
                        file_count: t.file_count,
                        last_updated: t.last_updated,
                        titles: t.titles,
                    })
                    .collect();
                serde_json::to_string_pretty(&entries).unwrap_or_default()
            }
            Err(e) => serde_json::to_string_pretty(&ErrorResponse {
                error: format!("Failed to list topics: {e}"),
            })
            .unwrap_or_default(),
        }
    }

    #[tool(
        name = "get_document",
        description = "Get the full content and metadata of a document by its relative path within knowledge-base/."
    )]
    async fn get_document(
        &self,
        Parameters(params): Parameters<GetDocumentParams>,
    ) -> String {
        let file_path = self.kb_path.join(&params.path);

        // Path traversal prevention: ensure resolved path stays inside kb_path
        let canonical = match file_path.canonicalize() {
            Ok(p) => p,
            Err(_) => {
                return serde_json::to_string_pretty(&ErrorResponse {
                    error: format!(
                        "File not found: {}. Path should be relative to knowledge-base/ (e.g. \"deep-dive/mcp/overview.md\").",
                        params.path
                    ),
                })
                .unwrap_or_default();
            }
        };
        if !canonical.starts_with(&self.kb_path) {
            return serde_json::to_string_pretty(&ErrorResponse {
                error: "Access denied: path is outside the knowledge base.".to_string(),
            })
            .unwrap_or_default();
        }

        match std::fs::read_to_string(&canonical) {
            Ok(raw) => {
                let parsed = markdown::parse(&raw);
                let resp = DocumentResponse {
                    path: params.path,
                    title: parsed.frontmatter.title,
                    date: parsed.frontmatter.date,
                    topic: parsed.frontmatter.topic,
                    tags: parsed.frontmatter.tags,
                    content: raw,
                };
                serde_json::to_string_pretty(&resp).unwrap_or_default()
            }
            Err(e) => serde_json::to_string_pretty(&ErrorResponse {
                error: format!("Failed to read file: {e}"),
            })
            .unwrap_or_default(),
        }
    }

    #[tool(
        name = "get_best_practice",
        description = "Get a best-practices document for the given target, optionally extracting a specific h2 section by category name. The file path is resolved via `[best_practice].path_templates` in kb-mcp.toml (default: best-practices/{target}/PERFECT.md)."
    )]
    async fn get_best_practice(
        &self,
        Parameters(params): Parameters<GetBestPracticeParams>,
    ) -> String {
        // feature 16: 設定されたテンプレート列を先頭から試す。
        // `{target}` を params.target に置換、kb_path 相対で canonicalize し、
        // 存在 + kb_path 配下の最初の候補を採用する。
        let mut canonical: Option<PathBuf> = None;
        let mut tried_paths: Vec<String> = Vec::new();
        for tmpl in &self.best_practice_templates {
            let rel = tmpl.replace("{target}", &params.target);
            tried_paths.push(rel.clone());
            let p = self.kb_path.join(&rel);
            let c = match p.canonicalize() {
                Ok(c) => c,
                Err(_) => continue,
            };
            if !c.starts_with(&self.kb_path) {
                // path traversal 防止
                continue;
            }
            canonical = Some(c);
            break;
        }

        let Some(canonical) = canonical else {
            return serde_json::to_string_pretty(&ErrorResponse {
                error: format!(
                    "Best-practices document for target '{}' not found. Tried: [{}]",
                    params.target,
                    tried_paths.join(", ")
                ),
            })
            .unwrap_or_default();
        };

        match std::fs::read_to_string(&canonical) {
            Ok(content) => {
                if let Some(ref cat) = params.category {
                    // Extract a specific h2 section
                    match extract_section(&content, cat) {
                        Some(section) => {
                            let resp = BestPracticeResponse {
                                target: params.target,
                                category: Some(cat.clone()),
                                content: section,
                            };
                            serde_json::to_string_pretty(&resp).unwrap_or_default()
                        }
                        None => {
                            // Return available sections as guidance
                            let sections = list_h2_sections(&content);
                            serde_json::to_string_pretty(&ErrorResponse {
                                error: format!(
                                    "Section '{}' not found. Available sections: {}",
                                    cat,
                                    sections.join(", ")
                                ),
                            })
                            .unwrap_or_default()
                        }
                    }
                } else {
                    // Return TOC + full content
                    let sections = list_h2_sections(&content);
                    let resp = BestPracticeResponse {
                        target: params.target,
                        category: None,
                        content: format!(
                            "## Sections\n{}\n\n---\n\n{}",
                            sections
                                .iter()
                                .map(|s| format!("- {s}"))
                                .collect::<Vec<_>>()
                                .join("\n"),
                            content
                        ),
                    };
                    serde_json::to_string_pretty(&resp).unwrap_or_default()
                }
            }
            Err(e) => serde_json::to_string_pretty(&ErrorResponse {
                error: format!("Failed to read best-practices file: {e}"),
            })
            .unwrap_or_default(),
        }
    }

    #[tool(
        name = "rebuild_index",
        description = "Rebuild the search index by scanning all Markdown files in the knowledge base."
    )]
    async fn rebuild_index(
        &self,
        Parameters(params): Parameters<RebuildIndexParams>,
    ) -> String {
        let force = params.force.unwrap_or(false);

        // Lock order: embedder first, then db (consistent with search)
        let mut embedder = self.embedder.lock().unwrap();
        let db = self.db.lock().unwrap();

        match indexer::rebuild_index(
            &db,
            &mut embedder,
            &self.kb_path,
            force,
            self.exclude_headings.as_deref(),
        ) {
            Ok(result) => {
                let stats = IndexStats {
                    total_documents: result.total_documents,
                    updated: result.updated,
                    deleted: result.deleted,
                    total_chunks: result.total_chunks,
                    duration_ms: result.duration_ms,
                };
                serde_json::to_string_pretty(&stats).unwrap_or_default()
            }
            Err(e) => serde_json::to_string_pretty(&ErrorResponse {
                error: format!("Rebuild failed: {e}"),
            })
            .unwrap_or_default(),
        }
    }

    #[tool(
        name = "get_connection_graph",
        description = "BFS-expand semantically related chunks starting from a \
                       document path. Returns a flat list of nodes with \
                       parent_id / depth / score, useful for chained context \
                       discovery by an LLM agent."
    )]
    async fn get_connection_graph(
        &self,
        Parameters(params): Parameters<GetConnectionGraphParams>,
    ) -> String {
        // パラメータ検証 + 上限クランプ
        let depth = params
            .depth
            .unwrap_or(graph::DEFAULT_DEPTH)
            .min(graph::MAX_DEPTH);
        let fan_out = params
            .fan_out
            .unwrap_or(graph::DEFAULT_FAN_OUT)
            .min(graph::MAX_FAN_OUT);
        let min_similarity = params
            .min_similarity
            .unwrap_or(graph::DEFAULT_MIN_SIMILARITY)
            .clamp(0.0, 1.0);
        let seed_strategy = match params.seed_strategy.as_deref() {
            Some("centroid") => SeedStrategy::Centroid,
            Some("all_chunks") | None => SeedStrategy::AllChunks,
            Some(other) => {
                return serde_json::to_string_pretty(&ErrorResponse {
                    error: format!(
                        "unknown seed_strategy '{other}' (expected 'all_chunks' or 'centroid')"
                    ),
                })
                .unwrap_or_default();
            }
        };

        let opts = GraphOptions {
            depth,
            fan_out,
            min_similarity,
            seed_strategy,
            category: params.category,
            topic: params.topic,
            exclude_paths: params.exclude_paths.unwrap_or_default(),
            dedup_by_path: params.dedup_by_path.unwrap_or(false),
            min_quality: self.quality_threshold,
        };

        let db = self.db.lock().unwrap();
        match graph::build_connection_graph(&db, &params.path, &opts) {
            Ok(g) => serde_json::to_string_pretty(&g).unwrap_or_default(),
            Err(e) => serde_json::to_string_pretty(&ErrorResponse {
                error: format!("get_connection_graph failed: {e}"),
            })
            .unwrap_or_default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract the h2 section whose heading contains `category_lower` (case-insensitive).
/// Returns all text from that heading until the next h2 heading.
fn extract_section(content: &str, category: &str) -> Option<String> {
    let cat_lower = category.to_lowercase();
    let mut lines = content.lines();
    let mut found = false;
    let mut section_lines: Vec<&str> = Vec::new();

    for line in &mut lines {
        if line.starts_with("## ") {
            if found {
                // We've hit the next h2 — stop collecting
                break;
            }
            let heading_text = line.trim_start_matches("## ").trim();
            if heading_text.to_lowercase().contains(&cat_lower) {
                found = true;
                section_lines.push(line);
                continue;
            }
        }
        if found {
            section_lines.push(line);
        }
    }

    if found {
        Some(section_lines.join("\n").trim().to_string())
    } else {
        None
    }
}

/// List all h2 headings in the content.
fn list_h2_sections(content: &str) -> Vec<String> {
    content
        .lines()
        .filter(|line| line.starts_with("## "))
        .map(|line| line.trim_start_matches("## ").trim().to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// Server bootstrap
// ---------------------------------------------------------------------------

/// Run the MCP server on stdio transport.
pub async fn run_server(
    kb_path: &std::path::Path,
    model: ModelChoice,
    reranker_choice: RerankerChoice,
    rerank_by_default: bool,
    exclude_headings: Option<Vec<String>>,
    quality_threshold: f32,
    best_practice_templates: Vec<String>,
) -> Result<()> {
    let db_path = crate::resolve_db_path(kb_path);
    let db = Database::open(&db_path.to_string_lossy())?;

    // モデル DL の前に meta 整合性を確認。不整合ならここで止めて DL を回避。
    db.verify_embedding_meta(model.model_id(), model.dimension() as u32)?;
    let embedder = Embedder::with_model(model)?;
    let reranker = Reranker::try_new(reranker_choice)?;

    let kb_path = kb_path.canonicalize().unwrap_or_else(|_| kb_path.to_path_buf());

    let server = KbServer {
        db: Mutex::new(db),
        embedder: Mutex::new(embedder),
        reranker: Mutex::new(reranker),
        rerank_by_default,
        kb_path,
        exclude_headings,
        quality_threshold,
        best_practice_templates,
        tool_router: KbServer::tool_router(),
    };

    eprintln!("kb-mcp server ready (stdio transport)");

    let transport = rmcp::transport::io::stdio();
    let service = rmcp::serve_server(server, transport).await?;

    // Wait for the service to finish (client disconnects)
    service.waiting().await?;

    Ok(())
}
