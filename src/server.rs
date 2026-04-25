use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::schemars;
use rmcp::{tool, tool_router};
use serde::{Deserialize, Serialize};

use crate::db::{Database, SearchHit};
use crate::embedder::{Embedder, ModelChoice, Reranker, RerankerChoice};
use crate::graph::{self, GraphOptions, SeedStrategy};
use crate::parser::Registry;
use crate::{indexer, markdown};

// ---------------------------------------------------------------------------
// Server struct
// ---------------------------------------------------------------------------

pub struct KbServer {
    /// watcher と共有するため `Arc<Mutex<_>>` で保持。
    db: Arc<Mutex<Database>>,
    embedder: Arc<Mutex<Embedder>>,
    /// HTTP トランスポートの service factory でセッションごとに
    /// `KbServer` を clone するため Arc 化。Option なのは reranker 無効のケース。
    reranker: Arc<Mutex<Option<Reranker>>>,
    rerank_by_default: bool,
    kb_path: PathBuf,
    /// `rebuild_index` ツールで markdown パース時に使う除外見出し。
    /// `None` のときは [`markdown::DEFAULT_EXCLUDED_HEADINGS`] を使う。
    exclude_headings: Option<Vec<String>>,
    /// `rebuild_index` ツールで walkdir 時にスキップするディレクトリ basename。
    exclude_dirs: Vec<String>,
    /// Quality filter: 既定の品質フィルタしきい値。`search` / graph で適用。
    /// 0.0 ならフィルタ無効。
    quality_threshold: f32,
    /// Best-practice resolver: `get_best_practice` のパス候補テンプレート。
    /// 先頭から順に `{target}` を置換してファイルを探し、最初に存在した
    /// ものを読む。kb-mcp.toml 未指定時は legacy 既定
    /// `["best-practices/{target}/PERFECT.md"]`。
    best_practice_templates: Vec<String>,
    /// Parser registry: index 対象の拡張子レジストリ。`rebuild_index` MCP ツール
    /// から `indexer::rebuild_index` に渡す。`kb-mcp.toml` の
    /// `[parsers].enabled` が無ければ `Registry::defaults()` = `["md"]` のみ。
    /// watcher とも共有するため Arc。
    parser_registry: Arc<Registry>,
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
    /// Filter by category (legacy, single value; e.g. "deep-dive",
    /// "ai-news", "tech-watch"). Prefer `path_globs` / `tags_any` /
    /// `tags_all` for new clients.
    category: Option<String>,
    /// Filter by topic (legacy, single value; e.g. "mcp", "chromadb").
    /// Prefer `path_globs` / `tags_any` / `tags_all` for new clients.
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

    // ----- structured filter set (path / tags / date) -----
    /// Path glob patterns. `!` prefix marks an exclude pattern,
    /// e.g. `["docs/**", "!docs/draft/**"]`. An empty array `[]`
    /// is rejected — pass `null` (omit the field) to disable, or
    /// `["**", "!a/**"]` to express exclude-only intent.
    path_globs: Option<Vec<String>>,
    /// Hit passes if it carries any of these tags (OR semantics).
    tags_any: Option<Vec<String>>,
    /// Hit passes only if it carries every one of these tags (AND).
    tags_all: Option<Vec<String>>,
    /// Inclusive lower bound on `frontmatter.date` (lexicographic, ISO-8601 friendly).
    date_from: Option<String>,
    /// Inclusive upper bound on `frontmatter.date` (lexicographic, ISO-8601 friendly).
    date_to: Option<String>,

    // ----- low-confidence cutoff -----
    /// Rank-based ratio threshold for trimming low-confidence tail results.
    /// `null` falls back to the server default (`kb-mcp.toml` / CLI);
    /// `0.0` disables the cutoff for this query.
    min_confidence_ratio: Option<f32>,
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
    /// File-rename を検出して path だけ UPDATE した件数。
    #[serde(default)]
    renamed: u32,
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
    async fn search(&self, Parameters(params): Parameters<SearchParams>) -> String {
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
        let filters = crate::db::SearchFilters {
            category: params.category.as_deref(),
            topic: params.topic.as_deref(),
            min_quality: effective_min_quality,
            ..Default::default()
        };
        let search_outcome: anyhow::Result<Vec<crate::db::SearchResult>> = if use_rerank {
            // rerank 入力用に candidates を取得、score は cross-encoder で上書き
            match db.search_hybrid_candidates(
                &params.query,
                &query_embedding,
                limit.saturating_mul(5).max(50),
                &filters,
            ) {
                Ok(cands) => {
                    let r = reranker_guard
                        .as_mut()
                        .expect("reranker Some checked above");
                    r.rerank_candidates(&params.query, cands, limit)
                }
                Err(e) => Err(e),
            }
        } else {
            db.search_hybrid(&params.query, &query_embedding, limit, &filters)
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
    async fn get_document(&self, Parameters(params): Parameters<GetDocumentParams>) -> String {
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
                let ext = canonical.extension().and_then(|e| e.to_str()).unwrap_or("");
                let resp = build_document_response(&self.parser_registry, &params.path, ext, raw);
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
        description = "Get a best-practices document for the given target, optionally extracting a specific h2 section by category name. Opt-in: requires `[best_practice].path_templates` to be configured in kb-mcp.toml (e.g. `path_templates = [\"best-practices/{target}/PERFECT.md\"]`); returns a 'not configured' error otherwise."
    )]
    async fn get_best_practice(
        &self,
        Parameters(params): Parameters<GetBestPracticeParams>,
    ) -> String {
        if self.best_practice_templates.is_empty() {
            return serde_json::to_string_pretty(&ErrorResponse {
                error: "get_best_practice is not configured. Add `[best_practice].path_templates` to kb-mcp.toml (for example: `path_templates = [\"best-practices/{target}/PERFECT.md\"]`) to enable this tool.".to_string(),
            })
            .unwrap_or_default();
        }
        let canonical = match resolve_best_practice_path(
            &self.kb_path,
            &self.best_practice_templates,
            &params.target,
        ) {
            ResolveOutcome::Found(p) => p,
            ResolveOutcome::NotFound(tried) => {
                return serde_json::to_string_pretty(&ErrorResponse {
                    error: format!(
                        "Best-practices document for target '{}' not found. Tried: [{}]",
                        params.target,
                        tried.join(", ")
                    ),
                })
                .unwrap_or_default();
            }
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
        description = "Rebuild the search index by scanning all source files in the knowledge base (Markdown plus any other extensions enabled via `[parsers].enabled` in kb-mcp.toml)."
    )]
    async fn rebuild_index(&self, Parameters(params): Parameters<RebuildIndexParams>) -> String {
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
            &self.exclude_dirs,
            &self.parser_registry,
        ) {
            Ok(result) => {
                let stats = IndexStats {
                    total_documents: result.total_documents,
                    updated: result.updated,
                    renamed: result.renamed,
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

/// Convert the user-facing `path_globs` input
/// (e.g. `["docs/**", "!docs/draft/**"]`) into a [`crate::db::CompiledPathGlobs`].
///
/// Patterns prefixed with `!` are routed into the exclude `GlobSet`; the rest
/// build the include set. An empty input array is an explicit error — callers
/// should pass `None` to disable filtering, or `["**", "!a/**"]` to express
/// exclude-only intent. Inputs consisting entirely of `!`-prefixed patterns
/// are accepted: `include` stays `None` (interpreted as "match everything")
/// and the excludes apply on top.
///
/// Visible to the crate so the CLI (`src/main.rs`) can reuse the same
/// validation path.
pub(crate) fn compile_path_globs(
    patterns: &[String],
) -> anyhow::Result<crate::db::CompiledPathGlobs> {
    use anyhow::Context;
    if patterns.is_empty() {
        anyhow::bail!(
            "path_globs cannot be empty. Use null to disable, or [\"**\", \"!a/**\"] for exclude-only."
        );
    }
    let mut include_b = globset::GlobSetBuilder::new();
    let mut exclude_b = globset::GlobSetBuilder::new();
    let mut has_include = false;
    let mut has_exclude = false;
    for raw in patterns {
        let (target, pat, is_exclude) = if let Some(rest) = raw.strip_prefix('!') {
            (&mut exclude_b, rest, true)
        } else {
            (&mut include_b, raw.as_str(), false)
        };
        let glob = globset::Glob::new(pat)
            .with_context(|| format!("invalid path_glob pattern: {raw:?}"))?;
        target.add(glob);
        if is_exclude {
            has_exclude = true;
        } else {
            has_include = true;
        }
    }
    let include = if has_include {
        Some(include_b.build()?)
    } else {
        None
    };
    let exclude = if has_exclude {
        Some(exclude_b.build()?)
    } else {
        None
    };
    Ok(crate::db::CompiledPathGlobs { include, exclude })
}

/// `get_document` ツール用に、拡張子に対応する Parser で
/// frontmatter (title/date/topic/tags) を抽出し DocumentResponse を組む。
/// 純粋関数化してテスト可能にしている。
///
/// 登録されていない拡張子はフォールバックで Markdown parser を使う (pre-
/// feature-20 と同じ挙動)。`.txt` はファイル名から title を derive するため
/// `path_hint` を必ず渡す。
fn build_document_response(
    registry: &Registry,
    path_hint: &str,
    ext: &str,
    raw: String,
) -> DocumentResponse {
    let parsed = match registry.by_extension(ext) {
        Some(p) => p.parse(&raw, path_hint, &[]),
        None => markdown::parse(&raw),
    };
    DocumentResponse {
        path: path_hint.to_string(),
        title: parsed.frontmatter.title,
        date: parsed.frontmatter.date,
        topic: parsed.frontmatter.topic,
        tags: parsed.frontmatter.tags,
        content: raw,
    }
}

/// `get_best_practice` のパス解決結果。
#[derive(Debug, PartialEq)]
enum ResolveOutcome {
    /// `canonicalize` 済みのファイル絶対パス。
    Found(PathBuf),
    /// どのテンプレートにもマッチしなかった。試行した相対パス列。
    NotFound(Vec<String>),
}

/// Best-practice resolver: テンプレート列に `{target}` を置換してファイルを探す。
/// 先頭から順に試し、`canonicalize` 成功 & `kb_path` 配下に収まる最初の
/// 候補を返す。`kb_path` は呼び出し側で既に canonicalize されている前提
/// (`run_server` / tests で事前処理)。
fn resolve_best_practice_path(
    kb_path: &std::path::Path,
    templates: &[String],
    target: &str,
) -> ResolveOutcome {
    let mut tried: Vec<String> = Vec::new();
    for tmpl in templates {
        let rel = tmpl.replace("{target}", target);
        tried.push(rel.clone());
        let candidate = kb_path.join(&rel);
        let canon = match candidate.canonicalize() {
            Ok(c) => c,
            Err(_) => continue,
        };
        if !canon.starts_with(kb_path) {
            // path traversal reject
            continue;
        }
        return ResolveOutcome::Found(canon);
    }
    ResolveOutcome::NotFound(tried)
}

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

/// `KbServer` を構成する共有リソース。HTTP トランスポートの
/// service factory が session ごとに `KbServer` を生成するため、重いリソース
/// (DB / embedder / reranker / registry) を 1 回だけロードして Arc で共有する。
#[derive(Clone)]
pub struct KbServerShared {
    pub db: Arc<Mutex<Database>>,
    pub embedder: Arc<Mutex<Embedder>>,
    pub reranker: Arc<Mutex<Option<Reranker>>>,
    pub rerank_by_default: bool,
    pub kb_path: PathBuf,
    pub exclude_headings: Option<Vec<String>>,
    pub exclude_dirs: Vec<String>,
    pub quality_threshold: f32,
    pub best_practice_templates: Vec<String>,
    pub parser_registry: Arc<Registry>,
}

impl KbServer {
    /// Shared state から新しい `KbServer` を組み立てる。
    /// Arc::clone で軽量、embedder / reranker モデルの重複ロードは起きない。
    pub fn from_shared(shared: &KbServerShared) -> Self {
        Self {
            db: Arc::clone(&shared.db),
            embedder: Arc::clone(&shared.embedder),
            reranker: Arc::clone(&shared.reranker),
            rerank_by_default: shared.rerank_by_default,
            kb_path: shared.kb_path.clone(),
            exclude_headings: shared.exclude_headings.clone(),
            exclude_dirs: shared.exclude_dirs.clone(),
            quality_threshold: shared.quality_threshold,
            best_practice_templates: shared.best_practice_templates.clone(),
            parser_registry: Arc::clone(&shared.parser_registry),
            tool_router: KbServer::tool_router(),
        }
    }
}

/// Run the MCP server on the selected transport.
#[allow(clippy::too_many_arguments)]
pub async fn run_server(
    kb_path: &std::path::Path,
    model: ModelChoice,
    reranker_choice: RerankerChoice,
    rerank_by_default: bool,
    exclude_headings: Option<Vec<String>>,
    exclude_dirs: Vec<String>,
    quality_threshold: f32,
    best_practice_templates: Vec<String>,
    parser_registry: Registry,
    watch_config: crate::watcher::WatchConfig,
    transport: crate::transport::Transport,
) -> Result<()> {
    let db_path = crate::resolve_db_path(kb_path);
    let db = Database::open(&db_path.to_string_lossy())?;

    // モデル DL の前に meta 整合性を確認。不整合ならここで止めて DL を回避。
    db.verify_embedding_meta(model.model_id(), model.dimension() as u32)?;
    let embedder = Embedder::with_model(model)?;
    let reranker = Reranker::try_new(reranker_choice)?;

    let kb_path = kb_path
        .canonicalize()
        .unwrap_or_else(|_| kb_path.to_path_buf());

    // watcher と共有するため Arc 化。
    // HTTP service factory でも共有するため KbServerShared にまとめる。
    let shared = KbServerShared {
        db: Arc::new(Mutex::new(db)),
        embedder: Arc::new(Mutex::new(embedder)),
        reranker: Arc::new(Mutex::new(reranker)),
        rerank_by_default,
        kb_path: kb_path.clone(),
        exclude_headings,
        exclude_dirs,
        quality_threshold,
        best_practice_templates,
        parser_registry: Arc::new(parser_registry),
    };

    // watcher をバックグラウンドで並走。
    let watcher_state = crate::watcher::WatcherState {
        kb_path: kb_path.clone(),
        db: Arc::clone(&shared.db),
        embedder: Arc::clone(&shared.embedder),
        registry: Arc::clone(&shared.parser_registry),
        exclude_headings: shared.exclude_headings.clone(),
        exclude_dirs: shared.exclude_dirs.clone(),
        config: watch_config,
    };
    let watcher_handle = tokio::spawn(async move {
        if let Err(e) = crate::watcher::run_watch_loop(watcher_state).await {
            eprintln!("watcher exited with error: {e}");
        }
    });

    let result = match transport {
        crate::transport::Transport::Stdio => crate::transport::stdio::run_stdio(&shared).await,
        crate::transport::Transport::Http { addr } => {
            // move shared to http runner (no clone needed — stdio branch
            // consumes it only by reference and is mutually exclusive).
            crate::transport::http::run_http(addr, shared).await
        }
    };
    watcher_handle.abort();
    result
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// 一意な tempdir を作って kb_path として返す。Drop で削除。
    struct TempKb {
        path: PathBuf,
    }
    impl TempKb {
        fn new(prefix: &str) -> Self {
            let pid = std::process::id();
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let path = std::env::temp_dir().join(format!("kb-mcp-srvtest-{prefix}-{pid}-{nonce}"));
            fs::create_dir_all(&path).unwrap();
            let canon = path.canonicalize().unwrap();
            Self { path: canon }
        }
        fn write(&self, rel: &str, content: &str) -> PathBuf {
            let full = self.path.join(rel);
            if let Some(parent) = full.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&full, content).unwrap();
            full
        }
    }
    impl Drop for TempKb {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn test_resolve_best_practice_first_template_hit() {
        let kb = TempKb::new("bp1");
        kb.write("best-practices/claude-code/PERFECT.md", "# CC\n");
        let templates = vec!["best-practices/{target}/PERFECT.md".to_string()];
        let r = resolve_best_practice_path(&kb.path, &templates, "claude-code");
        match r {
            ResolveOutcome::Found(p) => {
                assert!(
                    p.ends_with("best-practices/claude-code/PERFECT.md")
                        || p.ends_with("best-practices\\claude-code\\PERFECT.md")
                );
            }
            other => panic!("expected Found, got {other:?}"),
        }
    }

    #[test]
    fn test_resolve_best_practice_falls_through_to_second_template() {
        let kb = TempKb::new("bp2");
        kb.write("docs/cursor.md", "# cursor\n");
        let templates = vec![
            "best-practices/{target}/PERFECT.md".to_string(), // 不存在
            "docs/{target}.md".to_string(),                   // ヒット
        ];
        let r = resolve_best_practice_path(&kb.path, &templates, "cursor");
        match r {
            ResolveOutcome::Found(p) => {
                assert!(p.ends_with("docs/cursor.md") || p.ends_with("docs\\cursor.md"))
            }
            other => panic!("expected Found, got {other:?}"),
        }
    }

    #[test]
    fn test_resolve_best_practice_traversal_rejected() {
        let kb = TempKb::new("bp3");
        // kb_path の外側にファイルを作る (親ディレクトリに)
        let outside = kb.path.parent().unwrap().join(format!(
            "kb-mcp-srvtest-outside-{}-{}.md",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        fs::write(&outside, "secret").unwrap();

        // `{target}` に `../<ファイル名>` を入れて kb 外を指す
        let target_rel = format!("../{}", outside.file_name().unwrap().to_string_lossy());
        let templates = vec!["{target}".to_string()];
        let r = resolve_best_practice_path(&kb.path, &templates, &target_rel);
        // 実ファイルは存在するが kb_path 配下ではないので拒否される
        match r {
            ResolveOutcome::NotFound(tried) => {
                assert_eq!(tried.len(), 1);
            }
            ResolveOutcome::Found(p) => panic!("traversal was not rejected: {p:?}"),
        }
        let _ = fs::remove_file(&outside);
    }

    #[test]
    fn test_resolve_best_practice_all_missing_returns_tried_list() {
        let kb = TempKb::new("bp4");
        let templates = vec!["a/{target}.md".to_string(), "b/{target}.md".to_string()];
        let r = resolve_best_practice_path(&kb.path, &templates, "nope");
        match r {
            ResolveOutcome::NotFound(tried) => {
                assert_eq!(
                    tried,
                    vec!["a/nope.md".to_string(), "b/nope.md".to_string()]
                );
            }
            ResolveOutcome::Found(p) => panic!("expected NotFound, got {p:?}"),
        }
    }

    #[test]
    fn test_resolve_best_practice_empty_templates_returns_empty_tried() {
        let kb = TempKb::new("bp5");
        let r = resolve_best_practice_path(&kb.path, &[], "any");
        match r {
            ResolveOutcome::NotFound(tried) => assert!(tried.is_empty()),
            ResolveOutcome::Found(_) => panic!("expected NotFound"),
        }
    }

    // -----------------------------------------------------------------------
    // build_document_response の拡張子認識
    // evaluator 指摘 High #1: .txt で title が落ちる不整合を防ぐ回帰テスト。
    // -----------------------------------------------------------------------

    #[test]
    fn test_build_document_response_md_with_frontmatter() {
        let reg = Registry::from_enabled(&["md".into(), "txt".into()]).unwrap();
        let md = "---\ntitle: Hello\ntags: [a, b]\n---\n\n# body";
        let resp = build_document_response(&reg, "notes/hello.md", "md", md.to_string());
        assert_eq!(resp.title.as_deref(), Some("Hello"));
        assert_eq!(resp.tags, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(resp.path, "notes/hello.md");
        assert!(resp.content.contains("# body"));
    }

    #[test]
    fn test_build_document_response_txt_derives_title_from_filename() {
        let reg = Registry::from_enabled(&["md".into(), "txt".into()]).unwrap();
        let raw = "forest ecosystem notes body.";
        let resp = build_document_response(
            &reg,
            "nature/forest-ecosystem-notes.txt",
            "txt",
            raw.to_string(),
        );
        // .txt has no frontmatter — title must come from the filename
        assert_eq!(
            resp.title.as_deref(),
            Some("forest ecosystem notes"),
            "search and get_document must return the same derived title"
        );
        assert!(resp.date.is_none());
        assert!(resp.tags.is_empty());
        assert_eq!(resp.content, raw);
    }

    #[test]
    fn test_build_document_response_unknown_ext_falls_back_to_markdown() {
        // 登録外の拡張子は markdown::parse にフォールバック (legacy 相当)。
        // 通常は collect_source_files が registry の extensions しか拾わないため
        // 到達しないが、外部からの直接 path 指定でも落ちないように。
        let reg = Registry::defaults(); // md only
        let raw = "---\ntitle: x\n---\n\nbody";
        let resp = build_document_response(&reg, "a.unknown", "unknown", raw.to_string());
        // markdown::parse が frontmatter を拾う
        assert_eq!(resp.title.as_deref(), Some("x"));
    }

    // -----------------------------------------------------------------------
    // compile_path_globs: SearchParams.path_globs -> CompiledPathGlobs
    // -----------------------------------------------------------------------

    #[test]
    fn test_compile_path_globs_include_only() {
        let cpg = compile_path_globs(&["docs/**".into()]).unwrap();
        assert!(cpg.matches("docs/a.md"));
        assert!(!cpg.matches("notes/a.md"));
    }

    #[test]
    fn test_compile_path_globs_with_exclude() {
        let cpg = compile_path_globs(&["docs/**".into(), "!docs/draft/**".into()]).unwrap();
        assert!(cpg.matches("docs/a.md"));
        assert!(!cpg.matches("docs/draft/b.md"));
        assert!(!cpg.matches("notes/c.md"));
    }

    #[test]
    fn test_compile_path_globs_empty_array_is_error() {
        let err = compile_path_globs(&[]).unwrap_err();
        assert!(err.to_string().contains("path_globs cannot be empty"));
    }

    #[test]
    fn test_compile_path_globs_only_excludes_warns() {
        // include なし (全部 `!` prefix) は実装としてはエラーにしない、
        // 「全件 include + これらを exclude」と解釈する。
        let cpg = compile_path_globs(&["!docs/draft/**".into()]).unwrap();
        assert!(cpg.matches("docs/a.md"));        // include 無 = 全 include
        assert!(!cpg.matches("docs/draft/b.md")); // exclude 効く
    }
}
