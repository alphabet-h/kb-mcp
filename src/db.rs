use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::sync::Once;

/// RRF の定数項。原論文および多くの実装で慣例 60。
const RRF_K: f32 = 60.0;

/// filter (category / topic) を Rust 側で適用する際の KNN / FTS の over-fetch 倍率。
/// filter が選択的な場合に target `limit` 件に届くよう多めに候補を取る。
const FILTER_OVERFETCH_FACTOR: u32 = 10;
const FILTER_OVERFETCH_CAP: u32 = 10_000;

// ---------------------------------------------------------------------------
// Public result types
// ---------------------------------------------------------------------------

/// A single vector-search hit returned by [`Database::search_similar`].
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub score: f32,
    pub content: String,
    pub heading: Option<String>,
    pub path: String,
    pub title: Option<String>,
    pub topic: Option<String>,
    pub date: Option<String>,
}

/// Topic/category grouping returned by [`Database::list_topics`].
#[derive(Debug, Clone)]
pub struct TopicInfo {
    pub category: Option<String>,
    pub topic: Option<String>,
    pub file_count: u32,
    pub last_updated: Option<String>,
    pub titles: Vec<String>,
}

/// FTS5 クエリ用にユーザ入力をサニタイズする。
///
/// - trim 後に空、または 3 文字未満 (trigram の下限未満) なら `None` を返し
///   呼び出し側で vector-only にフォールバックさせる
/// - ダブルクォートを 2 連化してフレーズ全体をクォートで囲み、`AND` / `OR` /
///   `NOT` / `NEAR` / `*` / `:` 等の予約構文を中立化する
fn sanitize_fts_query(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.chars().count() < 3 {
        return None;
    }
    let escaped = trimmed.replace('"', "\"\"");
    Some(format!("\"{escaped}\""))
}

/// `CREATE VIRTUAL TABLE ... USING vec0(... embedding float[384] ...)` 形式の
/// SQL から次元数を抽出する。失敗時は `None`。
fn parse_dim_from_create_sql(sql: &str) -> Option<u32> {
    let start = sql.find("float[")? + "float[".len();
    let rest = &sql[start..];
    let end = rest.find(']')?;
    rest[..end].trim().parse().ok()
}

// ---------------------------------------------------------------------------
// Extension loading (once per process)
// ---------------------------------------------------------------------------

static INIT_VEC: Once = Once::new();

fn ensure_vec_extension() {
    INIT_VEC.call_once(|| unsafe {
        rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
            *const (),
            unsafe extern "C" fn(
                *mut rusqlite::ffi::sqlite3,
                *mut *mut i8,
                *const rusqlite::ffi::sqlite3_api_routines,
            ) -> i32,
        >(
            sqlite_vec::sqlite3_vec_init as *const (),
        )));
    });
}

// ---------------------------------------------------------------------------
// Database
// ---------------------------------------------------------------------------

/// Thin wrapper around a rusqlite [`Connection`] that owns the SQLite schema
/// (documents, chunks, vec_chunks, index_meta) and exposes CRUD + vector-search
/// helpers.
pub struct Database {
    conn: Connection,
}

impl Database {
    /// Open (or create) a file-backed database at `path`.
    pub fn open(path: &str) -> Result<Self> {
        ensure_vec_extension();
        let conn = Connection::open(path)
            .with_context(|| format!("failed to open database at {path}"))?;
        let db = Self { conn };
        db.init()?;
        Ok(db)
    }

    /// Open an in-memory database (useful for tests).
    pub fn open_in_memory() -> Result<Self> {
        ensure_vec_extension();
        let conn =
            Connection::open_in_memory().context("failed to open in-memory database")?;
        let db = Self { conn };
        db.init()?;
        Ok(db)
    }

    // -- private init --------------------------------------------------------

    fn init(&self) -> Result<()> {
        // WAL mode + foreign keys
        self.conn.execute_batch("PRAGMA journal_mode = WAL;")?;
        self.conn.execute_batch("PRAGMA foreign_keys = ON;")?;

        // vec_chunks は dim が未知の段階では作れないので遅延生成にする。
        // meta に dim が記録されていれば init 時に作るが、無ければ
        // `verify_embedding_meta` が実行時に決定した dim で作る。
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS index_meta (
                key   TEXT PRIMARY KEY,
                value TEXT
            );

            CREATE TABLE IF NOT EXISTS documents (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                path         TEXT UNIQUE NOT NULL,
                title        TEXT,
                topic        TEXT,
                category     TEXT,
                depth        TEXT,
                tags         TEXT,
                date         TEXT,
                content_hash TEXT NOT NULL,
                last_indexed TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS chunks (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                document_id  INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
                chunk_index  INTEGER NOT NULL,
                heading      TEXT,
                content      TEXT NOT NULL,
                token_count  INTEGER
            );
            ",
        )?;

        // FTS5 仮想テーブル: contentless + trigram tokenizer。
        // - contentless (content=''): chunks 側で本文を保持するのでメタ同期で十分
        // - contentless_delete=1: rowid 指定の DELETE を許可 (SQLite 3.43+)
        // - trigram: 日本語を含む任意言語で 3-gram ヒットが効く (SQLite 3.34+)
        // - rowid = chunks.id で統一 (INSERT 時に明示)
        self.conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS fts_chunks USING fts5(
                heading,
                content,
                content='',
                contentless_delete=1,
                tokenize = \"trigram remove_diacritics 1 case_sensitive 0\"
            );",
        )?;

        // meta に dim が記録されていれば vec_chunks を復元
        if let Some((_, dim)) = self.read_embedding_meta()? {
            self.ensure_vec_chunks_table(dim)?;
        }
        Ok(())
    }

    /// 現存する `vec_chunks` の宣言済み次元を返す。テーブルが無い or
    /// `CREATE` 文から次元を抜き出せない場合は `None`。
    fn current_vec_dim(&self) -> Result<Option<u32>> {
        use rusqlite::OptionalExtension;
        let sql: Option<String> = self
            .conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name='vec_chunks'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        Ok(sql.as_deref().and_then(parse_dim_from_create_sql))
    }

    /// 指定 `dim` の `vec_chunks` が存在することを保証する。
    /// 既存テーブルが別次元なら error (再構築は `recreate_vec_chunks` 経由)。
    fn ensure_vec_chunks_table(&self, dim: u32) -> Result<()> {
        if let Some(existing) = self.current_vec_dim()? {
            if existing == dim {
                return Ok(());
            }
            anyhow::bail!(
                "vec_chunks declared float[{existing}] but runtime dim is {dim}. \
                 Run index with --force to rebuild."
            );
        }
        let sql = format!(
            "CREATE VIRTUAL TABLE vec_chunks USING vec0(
                 chunk_id INTEGER PRIMARY KEY,
                 embedding float[{dim}]
             )"
        );
        self.conn.execute_batch(&sql)?;
        Ok(())
    }

    // -- public API ----------------------------------------------------------

    /// Insert or update a document row. On update the old chunks (and their
    /// vec_chunks entries) are deleted so the caller can re-insert fresh ones.
    ///
    /// Returns the document `id`.
    #[allow(clippy::too_many_arguments)]
    pub fn upsert_document(
        &self,
        path: &str,
        title: Option<&str>,
        topic: Option<&str>,
        category: Option<&str>,
        depth: Option<&str>,
        tags: &[String],
        date: Option<&str>,
        content_hash: &str,
    ) -> Result<i64> {
        let now = chrono::Utc::now().to_rfc3339();
        let tags_json = serde_json::to_string(tags)?;

        // Check if document already exists
        use rusqlite::OptionalExtension;
        let existing_id: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM documents WHERE path = ?1",
                params![path],
                |row| row.get(0),
            )
            .optional()?;

        if let Some(doc_id) = existing_id {
            // Delete old vector / FTS entries for chunks that belong to this document
            self.conn.execute(
                "DELETE FROM vec_chunks WHERE chunk_id IN \
                 (SELECT id FROM chunks WHERE document_id = ?1)",
                params![doc_id],
            )?;
            self.conn.execute(
                "DELETE FROM fts_chunks WHERE rowid IN \
                 (SELECT id FROM chunks WHERE document_id = ?1)",
                params![doc_id],
            )?;
            // Cascade will handle chunks when we update the document,
            // but we delete explicitly to be safe before the UPDATE
            self.conn.execute(
                "DELETE FROM chunks WHERE document_id = ?1",
                params![doc_id],
            )?;
            // Update the document row
            self.conn.execute(
                "UPDATE documents SET title = ?1, topic = ?2, category = ?3,
                 depth = ?4, tags = ?5, date = ?6, content_hash = ?7,
                 last_indexed = ?8 WHERE id = ?9",
                params![title, topic, category, depth, tags_json, date, content_hash, now, doc_id],
            )?;
            Ok(doc_id)
        } else {
            self.conn.execute(
                "INSERT INTO documents (path, title, topic, category, depth, tags, date, content_hash, last_indexed)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![path, title, topic, category, depth, tags_json, date, content_hash, now],
            )?;
            Ok(self.conn.last_insert_rowid())
        }
    }

    /// Insert a chunk row **and** its corresponding vec_chunks embedding + FTS row.
    ///
    /// `embedding` の長さは現在の `vec_chunks` の宣言次元 (`ModelChoice` に連動、
    /// BGE-small-en-v1.5 で 384 / BGE-M3 で 1024) と一致する必要がある。
    /// Returns the chunk `id`.
    pub fn insert_chunk(
        &self,
        document_id: i64,
        chunk_index: i32,
        heading: Option<&str>,
        content: &str,
        embedding: &[f32],
    ) -> Result<i64> {
        // Rough token estimate: 1 token ~= 4 chars (English average)
        let token_count = (content.len() / 4) as i32;

        self.conn.execute(
            "INSERT INTO chunks (document_id, chunk_index, heading, content, token_count)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![document_id, chunk_index, heading, content, token_count],
        )?;
        let chunk_id = self.conn.last_insert_rowid();

        // sqlite-vec accepts embeddings as a JSON array string
        let embedding_json = serde_json::to_string(embedding)?;
        self.conn.execute(
            "INSERT INTO vec_chunks (chunk_id, embedding) VALUES (?1, ?2)",
            params![chunk_id, embedding_json],
        )?;

        // FTS5 contentless: rowid を chunks.id に合わせる必要あり
        self.conn.execute(
            "INSERT INTO fts_chunks (rowid, heading, content) VALUES (?1, ?2, ?3)",
            params![chunk_id, heading, content],
        )?;

        Ok(chunk_id)
    }

    /// Return the stored `content_hash` for a document path, or `None` if the
    /// path is not indexed yet.
    pub fn get_document_hash(&self, path: &str) -> Result<Option<String>> {
        use rusqlite::OptionalExtension;
        let result = self
            .conn
            .query_row(
                "SELECT content_hash FROM documents WHERE path = ?1",
                params![path],
                |row| row.get(0),
            )
            .optional()?;
        Ok(result)
    }

    /// Delete a document and all associated chunks / vectors / FTS rows.
    pub fn delete_document(&self, path: &str) -> Result<()> {
        // Delete vector entries first (no FK from virtual table)
        self.conn.execute(
            "DELETE FROM vec_chunks WHERE chunk_id IN \
             (SELECT c.id FROM chunks c JOIN documents d ON c.document_id = d.id WHERE d.path = ?1)",
            params![path],
        )?;
        // FTS5 contentless: rowid ベースで削除
        self.conn.execute(
            "DELETE FROM fts_chunks WHERE rowid IN \
             (SELECT c.id FROM chunks c JOIN documents d ON c.document_id = d.id WHERE d.path = ?1)",
            params![path],
        )?;
        // Delete chunks (cascade would handle this, but be explicit)
        self.conn.execute(
            "DELETE FROM chunks WHERE document_id IN \
             (SELECT id FROM documents WHERE path = ?1)",
            params![path],
        )?;
        // Delete the document row
        self.conn
            .execute("DELETE FROM documents WHERE path = ?1", params![path])?;
        Ok(())
    }

    /// ベクトル単体類似検索。最大 `limit` 件を距離昇順 (小さい = より類似) で返す。
    ///
    /// `search_hybrid` とのロジック統一のため、内部では [`Self::search_vec_candidates`]
    /// に委譲し、`chunk_id` を剥いだ `SearchResult` のみを返す。
    /// 主に単体ベクトル検索のテスト / ツール用途で残している。
    pub fn search_similar(
        &self,
        query_embedding: &[f32],
        limit: u32,
        category: Option<&str>,
        topic: Option<&str>,
    ) -> Result<Vec<SearchResult>> {
        let hits = self.search_vec_candidates(query_embedding, limit, category, topic)?;
        Ok(hits.into_iter().map(|(_, r)| r).collect())
    }

    /// FTS5 側の候補検索。最大 `limit` 件を bm25 昇順 (小さい = 関連度高) で返す。
    /// 返値は `(chunk_id, SearchResult の雛形)` のタプル列 (`score` は bm25)。
    ///
    /// `search_vec_candidates` と同様に、category / topic フィルタが指定されて
    /// いる場合は `FILTER_OVERFETCH_FACTOR` 倍を取りに行き、Rust 側で絞り込む。
    fn search_fts_candidates(
        &self,
        query_text: &str,
        limit: u32,
        category: Option<&str>,
        topic: Option<&str>,
    ) -> Result<Vec<(i64, SearchResult)>> {
        let Some(fts_query) = sanitize_fts_query(query_text) else {
            return Ok(Vec::new());
        };

        let has_filters = category.is_some() || topic.is_some();
        let fetch_limit = if has_filters {
            limit
                .saturating_mul(FILTER_OVERFETCH_FACTOR)
                .min(FILTER_OVERFETCH_CAP)
        } else {
            limit
        };

        let sql = "
            SELECT c.id, bm25(fts_chunks) AS score,
                   c.content, c.heading,
                   d.path, d.title, d.topic, d.date, d.category
            FROM fts_chunks f
            JOIN chunks c ON c.id = f.rowid
            JOIN documents d ON d.id = c.document_id
            WHERE fts_chunks MATCH ?1
            ORDER BY bm25(fts_chunks)
            LIMIT ?2
        ";
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params![fts_query, fetch_limit], |row| {
            let chunk_id: i64 = row.get(0)?;
            let score: f32 = row.get(1)?;
            Ok((
                chunk_id,
                score,
                row.get::<_, String>(2)?,           // content
                row.get::<_, Option<String>>(3)?,   // heading
                row.get::<_, String>(4)?,           // path
                row.get::<_, Option<String>>(5)?,   // title
                row.get::<_, Option<String>>(6)?,   // topic
                row.get::<_, Option<String>>(7)?,   // date
                row.get::<_, Option<String>>(8)?,   // category
            ))
        })?;

        let mut results = Vec::new();
        for row in rows {
            let (chunk_id, score, content, heading, path, title, r_topic, date, r_category) =
                row?;
            if let Some(cat) = category
                && r_category.as_deref() != Some(cat)
            {
                continue;
            }
            if let Some(t) = topic
                && r_topic.as_deref() != Some(t)
            {
                continue;
            }
            results.push((
                chunk_id,
                SearchResult {
                    score, // 一旦 bm25 を入れておく (呼び出し側で RRF に上書き)
                    content,
                    heading,
                    path,
                    title,
                    topic: r_topic,
                    date,
                },
            ));
            if results.len() >= limit as usize {
                break;
            }
        }
        Ok(results)
    }

    /// ベクトル検索 + FTS5 を Reciprocal Rank Fusion (RRF, k=60) で統合する
    /// ハイブリッド検索。各側の順位だけを使うため、距離や bm25 の正規化は不要。
    ///
    /// FTS 側でヒットが 0 件 (trigram 下限以下のクエリや予約語のみ等) の場合は
    /// vec-only の順位で結果を返す (スコアは RRF 公式で計算)。
    pub fn search_hybrid(
        &self,
        query_text: &str,
        query_embedding: &[f32],
        limit: u32,
        category: Option<&str>,
        topic: Option<&str>,
    ) -> Result<Vec<SearchResult>> {
        let candidates = limit.saturating_mul(5).max(50);

        // vec 側: 既存の search_similar は SearchResult を返すだけで chunk_id を
        // 外に出さないので、RRF 用に chunk_id を拾う最小 SELECT を別途組む。
        let vec_hits = self.search_vec_candidates(
            query_embedding,
            candidates,
            category,
            topic,
        )?;
        let fts_hits = self.search_fts_candidates(query_text, candidates, category, topic)?;

        // RRF: chunk_id ごとに 1/(K + rank + 1) を加算
        let mut scores: HashMap<i64, f32> = HashMap::new();
        let mut rows: HashMap<i64, SearchResult> = HashMap::new();
        for (rank, (chunk_id, row)) in vec_hits.into_iter().enumerate() {
            *scores.entry(chunk_id).or_insert(0.0) += 1.0 / (RRF_K + (rank as f32) + 1.0);
            rows.entry(chunk_id).or_insert(row);
        }
        for (rank, (chunk_id, row)) in fts_hits.into_iter().enumerate() {
            *scores.entry(chunk_id).or_insert(0.0) += 1.0 / (RRF_K + (rank as f32) + 1.0);
            rows.entry(chunk_id).or_insert(row);
        }

        let mut merged: Vec<(i64, f32)> = scores.into_iter().collect();
        merged.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        merged.truncate(limit as usize);

        let results = merged
            .into_iter()
            .filter_map(|(id, rrf)| {
                rows.remove(&id).map(|mut r| {
                    r.score = rrf;
                    r
                })
            })
            .collect();
        Ok(results)
    }

    /// RRF 用: ベクトル検索の候補を `(chunk_id, SearchResult)` で返す。
    /// 既存の `search_similar` とロジックは同じだが chunk_id を外に出す。
    /// ベクトル検索で最大 `limit` 件の候補を `(chunk_id, SearchResult)` で返す。
    /// `score` フィールドには距離 (小さいほど類似) が入る。
    ///
    /// category / topic フィルタが指定されている場合は、Rust 側でフィルタが
    /// 適用されて候補が減る分を補うため `FILTER_OVERFETCH_FACTOR` 倍の
    /// KNN を SQLite へ投げる ([`FILTER_OVERFETCH_CAP`] 上限)。
    fn search_vec_candidates(
        &self,
        query_embedding: &[f32],
        limit: u32,
        category: Option<&str>,
        topic: Option<&str>,
    ) -> Result<Vec<(i64, SearchResult)>> {
        let has_filters = category.is_some() || topic.is_some();
        let fetch_k = if has_filters {
            limit
                .saturating_mul(FILTER_OVERFETCH_FACTOR)
                .min(FILTER_OVERFETCH_CAP)
        } else {
            limit
        };
        let embedding_json = serde_json::to_string(query_embedding)?;
        let sql = "
            SELECT v.chunk_id, v.distance,
                   c.content, c.heading,
                   d.path, d.title, d.topic, d.date, d.category
            FROM vec_chunks v
            JOIN chunks c ON c.id = v.chunk_id
            JOIN documents d ON d.id = c.document_id
            WHERE v.embedding MATCH ?1 AND k = ?2
            ORDER BY v.distance
        ";
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params![embedding_json, fetch_k], |row| {
            let chunk_id: i64 = row.get(0)?;
            let distance: f32 = row.get(1)?;
            Ok((
                chunk_id,
                distance,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
            ))
        })?;

        let mut out = Vec::with_capacity(limit as usize);
        for row in rows {
            let (chunk_id, distance, content, heading, path, title, r_topic, date, r_category) =
                row?;
            if let Some(cat) = category
                && r_category.as_deref() != Some(cat)
            {
                continue;
            }
            if let Some(t) = topic
                && r_topic.as_deref() != Some(t)
            {
                continue;
            }
            out.push((
                chunk_id,
                SearchResult {
                    score: distance,
                    content,
                    heading,
                    path,
                    title,
                    topic: r_topic,
                    date,
                },
            ));
            if out.len() >= limit as usize {
                break;
            }
        }
        Ok(out)
    }

    /// List all indexed topics grouped by (category, topic).
    pub fn list_topics(&self) -> Result<Vec<TopicInfo>> {
        let sql = "
            SELECT category, topic,
                   COUNT(*) AS file_count,
                   MAX(last_indexed) AS last_updated,
                   GROUP_CONCAT(title, '||') AS titles
            FROM documents
            GROUP BY category, topic
            ORDER BY category, topic
        ";
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map([], |row| {
            let titles_raw: Option<String> = row.get(4)?;
            let titles = titles_raw
                .map(|s| {
                    s.split("||")
                        .filter(|t| !t.is_empty())
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default();
            Ok(TopicInfo {
                category: row.get(0)?,
                topic: row.get(1)?,
                file_count: row.get(2)?,
                last_updated: row.get(3)?,
                titles,
            })
        })?;
        rows.into_iter()
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Total number of indexed documents.
    pub fn document_count(&self) -> Result<u32> {
        let count: u32 = self
            .conn
            .query_row("SELECT COUNT(*) FROM documents", [], |row| row.get(0))?;
        Ok(count)
    }

    /// Total number of chunks across all documents.
    pub fn chunk_count(&self) -> Result<u32> {
        let count: u32 = self
            .conn
            .query_row("SELECT COUNT(*) FROM chunks", [], |row| row.get(0))?;
        Ok(count)
    }

    /// Read `(model, dim)` from `index_meta`. Returns `None` if either key is
    /// missing or malformed (treated as "no meta recorded yet").
    pub fn read_embedding_meta(&self) -> Result<Option<(String, u32)>> {
        use rusqlite::OptionalExtension;
        let model: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM index_meta WHERE key = 'embedding_model'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let dim_raw: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM index_meta WHERE key = 'embedding_dim'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        match (model, dim_raw) {
            (Some(m), Some(d)) => match d.parse::<u32>() {
                Ok(dim) => Ok(Some((m, dim))),
                Err(_) => Ok(None),
            },
            _ => Ok(None),
        }
    }

    /// Insert or replace the `(embedding_model, embedding_dim)` entries in
    /// `index_meta`.
    pub fn write_embedding_meta(&self, model: &str, dim: u32) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO index_meta (key, value) VALUES ('embedding_model', ?1)",
            params![model],
        )?;
        self.conn.execute(
            "INSERT OR REPLACE INTO index_meta (key, value) VALUES ('embedding_dim', ?1)",
            params![dim.to_string()],
        )?;
        Ok(())
    }

    /// Verify the runtime `(model, dim)` matches the values recorded in
    /// `index_meta`.
    ///
    /// * Empty meta + empty DB → record current values (fresh DB).
    /// * Empty meta + non-empty DB → migrate a pre-feature-8 DB by recording
    ///   the current values, with a one-time log message.
    /// * Matching meta → no-op.
    /// * Mismatching meta → return an actionable error.
    pub fn verify_embedding_meta(&self, model: &str, dim: u32) -> Result<()> {
        match self.read_embedding_meta()? {
            None => {
                if self.chunk_count()? > 0 {
                    eprintln!(
                        "Migrating pre-meta index: recording ({model}, {dim}) into index_meta"
                    );
                }
                self.write_embedding_meta(model, dim)?;
                self.ensure_vec_chunks_table(dim)
            }
            Some((db_model, db_dim)) if db_model == model && db_dim == dim => {
                // init 時に meta が無くて vec_chunks を作れなかったケースをここで補う。
                self.ensure_vec_chunks_table(dim)
            }
            Some((db_model, db_dim)) => anyhow::bail!(
                "embedding model mismatch.\n  \
                 DB was indexed with: {db_model} ({db_dim} dim)\n  \
                 Current runtime:     {model} ({dim} dim)\n\n\
                 Run `kb-mcp index --kb-path <path> --force --model {model}` to rebuild the index, \
                 or switch back to the previous model."
            ),
        }
    }

    /// FTS に未登録の `chunks` を拾って `fts_chunks` に埋め直す。
    /// 主に pre-feature-9 DB のマイグレーション経路で呼ばれる。
    /// 埋め込み再計算は行わないので高速 (既存 content を INSERT するだけ)。
    pub fn backfill_fts(&self) -> Result<u32> {
        let sql = "
            SELECT id, heading, content
            FROM chunks
            WHERE id NOT IN (SELECT rowid FROM fts_chunks)
        ";
        let mut stmt = self.conn.prepare(sql)?;
        let rows: Vec<(i64, Option<String>, String)> = stmt
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let mut count = 0u32;
        for (id, heading, content) in rows {
            self.conn.execute(
                "INSERT INTO fts_chunks (rowid, heading, content) VALUES (?1, ?2, ?3)",
                params![id, heading, content],
            )?;
            count += 1;
        }
        Ok(count)
    }

    /// `vec_chunks` を DROP して指定 `dim` で再生成する。
    /// 呼び出し側で `chunks` / `documents` の整合を別途管理すること
    /// (通常は [`Database::reset_for_model`] 経由で呼ぶ)。
    fn recreate_vec_chunks(&self, dim: u32) -> Result<()> {
        self.conn.execute_batch("DROP TABLE IF EXISTS vec_chunks;")?;
        let sql = format!(
            "CREATE VIRTUAL TABLE vec_chunks USING vec0(
                 chunk_id INTEGER PRIMARY KEY,
                 embedding float[{dim}]
             )"
        );
        self.conn.execute_batch(&sql)?;
        Ok(())
    }

    /// `--force` 時の破壊的再初期化: `documents` / `chunks` / `vec_chunks`
    /// を全消ししてから新しい `(model, dim)` を記録する。`indexer::rebuild_index`
    /// が直後にすべての文書を再インデックスすることを前提とする。
    pub fn reset_for_model(&self, model: &str, dim: u32) -> Result<()> {
        self.conn.execute_batch(
            "DELETE FROM fts_chunks; \
             DELETE FROM chunks; \
             DELETE FROM documents;",
        )?;
        self.recreate_vec_chunks(dim)?;
        self.write_embedding_meta(model, dim)?;
        Ok(())
    }

    /// Return every indexed document path.
    pub fn all_document_paths(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT path FROM documents ORDER BY path")?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        rows.into_iter()
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create a dummy 384-dim embedding filled with `val`.
    fn dummy_embedding(val: f32) -> Vec<f32> {
        vec![val; 384]
    }

    /// Helper: create an in-memory DB and initialize its vec_chunks table
    /// with the legacy 384-dim schema. Most tests below operate on this
    /// setup to mirror a normal runtime where `verify_embedding_meta` has
    /// already run.
    fn db_with_384() -> Database {
        let db = Database::open_in_memory().unwrap();
        db.verify_embedding_meta("bge-small-en-v1.5", 384).unwrap();
        db
    }

    #[test]
    fn test_schema_creation() {
        let db = Database::open_in_memory().expect("open_in_memory");
        assert_eq!(db.document_count().unwrap(), 0);
        assert_eq!(db.chunk_count().unwrap(), 0);
        println!("test_schema_creation: OK — 0 docs, 0 chunks after fresh init");
    }

    #[test]
    fn test_upsert_and_query_document() {
        let db = db_with_384();

        // First insert
        let id1 = db
            .upsert_document(
                "deep-dive/mcp/overview.md",
                Some("MCP Overview"),
                Some("mcp"),
                Some("deep-dive"),
                Some("1"),
                &["mcp".into(), "protocol".into()],
                Some("2026-04-16"),
                "hash_aaa",
            )
            .unwrap();
        println!("insert returned id={id1}");
        assert_eq!(db.document_count().unwrap(), 1);

        // Insert a chunk so we can verify cascade-on-upsert
        db.insert_chunk(id1, 0, Some("Intro"), "Hello MCP", &dummy_embedding(0.1))
            .unwrap();
        assert_eq!(db.chunk_count().unwrap(), 1);

        // Upsert same path with new hash — should still be 1 doc, 0 chunks
        let id2 = db
            .upsert_document(
                "deep-dive/mcp/overview.md",
                Some("MCP Overview v2"),
                Some("mcp"),
                Some("deep-dive"),
                Some("1"),
                &["mcp".into()],
                Some("2026-04-16"),
                "hash_bbb",
            )
            .unwrap();
        println!("upsert returned id={id2} (should equal {id1})");
        assert_eq!(id1, id2, "upsert must reuse the same row id");
        assert_eq!(db.document_count().unwrap(), 1, "still 1 document");
        assert_eq!(db.chunk_count().unwrap(), 0, "old chunks deleted on upsert");

        println!("test_upsert_and_query_document: OK");
    }

    #[test]
    fn test_content_hash_check() {
        let db = Database::open_in_memory().unwrap();

        // Non-existent path
        assert!(
            db.get_document_hash("does/not/exist.md").unwrap().is_none(),
            "non-existent path should return None"
        );

        // After insert
        db.upsert_document(
            "ai-news/2026-04-16.md",
            Some("AI News"),
            None,
            Some("ai-news"),
            None,
            &[],
            Some("2026-04-16"),
            "hash_xyz",
        )
        .unwrap();

        let hash = db
            .get_document_hash("ai-news/2026-04-16.md")
            .unwrap()
            .expect("should be Some");
        assert_eq!(hash, "hash_xyz");

        println!("test_content_hash_check: OK");
    }

    #[test]
    fn test_delete_document() {
        let db = db_with_384();

        let doc_id = db
            .upsert_document(
                "tech-watch/anthropic/2026-04-16.md",
                Some("Anthropic Watch"),
                Some("anthropic"),
                Some("tech-watch"),
                None,
                &["anthropic".into()],
                Some("2026-04-16"),
                "hash_del",
            )
            .unwrap();
        db.insert_chunk(doc_id, 0, None, "some content", &dummy_embedding(0.5))
            .unwrap();
        assert_eq!(db.document_count().unwrap(), 1);
        assert_eq!(db.chunk_count().unwrap(), 1);

        db.delete_document("tech-watch/anthropic/2026-04-16.md")
            .unwrap();
        assert_eq!(db.document_count().unwrap(), 0, "document deleted");
        assert_eq!(db.chunk_count().unwrap(), 0, "chunks deleted");

        println!("test_delete_document: OK");
    }

    #[test]
    fn test_search_similar_executes_knn_query() {
        // Regression: sqlite-vec requires `k = ?` (or literal LIMIT) on knn
        // queries. A bound `LIMIT ?` used to fail with "A LIMIT or 'k = ?'
        // constraint is required on vec0 knn queries".
        let db = db_with_384();

        let doc_id = db
            .upsert_document(
                "deep-dive/mcp/overview.md",
                Some("MCP Overview"),
                Some("mcp"),
                Some("deep-dive"),
                Some("1"),
                &[],
                Some("2026-04-16"),
                "h1",
            )
            .unwrap();
        db.insert_chunk(doc_id, 0, Some("Intro"), "hello", &dummy_embedding(0.1))
            .unwrap();
        db.insert_chunk(doc_id, 1, Some("Body"), "world", &dummy_embedding(0.2))
            .unwrap();

        // No filter path
        let hits = db.search_similar(&dummy_embedding(0.1), 5, None, None).unwrap();
        assert_eq!(hits.len(), 2, "both chunks should be returned");

        // Filter path (category match)
        let hits = db
            .search_similar(&dummy_embedding(0.1), 5, Some("deep-dive"), None)
            .unwrap();
        assert_eq!(hits.len(), 2);

        // Filter path (non-matching topic → empty)
        let hits = db
            .search_similar(&dummy_embedding(0.1), 5, None, Some("no-such-topic"))
            .unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn test_fts_table_created_on_init() {
        let db = Database::open_in_memory().unwrap();
        let name: String = db
            .conn
            .query_row(
                "SELECT name FROM sqlite_master WHERE type='table' AND name='fts_chunks'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(name, "fts_chunks");
    }

    #[test]
    fn test_sanitize_fts_query() {
        assert_eq!(sanitize_fts_query("E0382"), Some("\"E0382\"".to_string()));
        assert_eq!(
            sanitize_fts_query("foo \"bar\" AND"),
            Some("\"foo \"\"bar\"\" AND\"".to_string())
        );
        assert_eq!(sanitize_fts_query(""), None);
        assert_eq!(sanitize_fts_query("   "), None);
        assert_eq!(sanitize_fts_query("ab"), None, "trigram 3 文字未満は None");
        assert_eq!(
            sanitize_fts_query("エラー"),
            Some("\"エラー\"".to_string()),
            "日本語 3 文字は通る"
        );
    }

    #[test]
    fn test_parse_dim_from_create_sql() {
        let sql = "CREATE VIRTUAL TABLE vec_chunks USING vec0(\
                   chunk_id INTEGER PRIMARY KEY, embedding float[1024])";
        assert_eq!(parse_dim_from_create_sql(sql), Some(1024));

        let sql2 = "CREATE VIRTUAL TABLE vec_chunks USING vec0(chunk_id, embedding float[384] )";
        assert_eq!(parse_dim_from_create_sql(sql2), Some(384));

        assert_eq!(parse_dim_from_create_sql("no float here"), None);
    }

    #[test]
    fn test_init_does_not_create_vec_chunks_without_meta() {
        let db = Database::open_in_memory().unwrap();
        assert_eq!(db.current_vec_dim().unwrap(), None);
    }

    #[test]
    fn test_verify_creates_vec_chunks_with_declared_dim() {
        let db = Database::open_in_memory().unwrap();
        db.verify_embedding_meta("bge-m3", 1024).unwrap();
        assert_eq!(db.current_vec_dim().unwrap(), Some(1024));

        // 1024-dim embedding を insert できることを確認
        let doc_id = db
            .upsert_document(
                "x.md", Some("x"), None, None, None, &[], None, "h",
            )
            .unwrap();
        let emb: Vec<f32> = vec![0.1; 1024];
        db.insert_chunk(doc_id, 0, None, "hi", &emb).unwrap();
        assert_eq!(db.chunk_count().unwrap(), 1);
    }

    #[test]
    fn test_ensure_vec_chunks_rejects_mismatched_dim() {
        let db = Database::open_in_memory().unwrap();
        db.ensure_vec_chunks_table(384).unwrap();
        let err = db.ensure_vec_chunks_table(1024).expect_err("must reject");
        assert!(err.to_string().contains("float[384]"));
    }

    /// Helper: FTS row count (contentless でも COUNT は通る)
    fn fts_count(db: &Database) -> u32 {
        db.conn
            .query_row("SELECT COUNT(*) FROM fts_chunks", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn test_insert_chunk_populates_fts() {
        let db = db_with_384();
        let doc_id = db
            .upsert_document("a.md", Some("a"), None, None, None, &[], None, "h")
            .unwrap();
        let chunk_id = db
            .insert_chunk(doc_id, 0, Some("Intro"), "hello world", &dummy_embedding(0.1))
            .unwrap();
        assert_eq!(fts_count(&db), 1);

        // rowid が chunks.id と一致
        let fts_rowid: i64 = db
            .conn
            .query_row("SELECT rowid FROM fts_chunks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fts_rowid, chunk_id);
    }

    #[test]
    fn test_delete_document_cascades_to_fts() {
        let db = db_with_384();
        let doc_id = db
            .upsert_document("a.md", Some("a"), None, None, None, &[], None, "h")
            .unwrap();
        db.insert_chunk(doc_id, 0, None, "hi", &dummy_embedding(0.1))
            .unwrap();
        assert_eq!(fts_count(&db), 1);

        db.delete_document("a.md").unwrap();
        assert_eq!(fts_count(&db), 0);
    }

    #[test]
    fn test_upsert_document_purges_old_fts() {
        let db = db_with_384();
        let doc_id = db
            .upsert_document("a.md", Some("a"), None, None, None, &[], None, "h1")
            .unwrap();
        db.insert_chunk(doc_id, 0, None, "old content", &dummy_embedding(0.1))
            .unwrap();
        assert_eq!(fts_count(&db), 1);

        // 同一 path を異なる content_hash で再 upsert → 旧 chunk/FTS は消える
        db.upsert_document("a.md", Some("a"), None, None, None, &[], None, "h2")
            .unwrap();
        assert_eq!(fts_count(&db), 0);
    }

    #[test]
    fn test_search_hybrid_fts_exact_match_ranks_higher() {
        let db = db_with_384();
        let doc_id = db
            .upsert_document("doc.md", Some("doc"), None, None, None, &[], None, "h")
            .unwrap();
        // chunk A: 完全一致語 E0382 を含む。埋め込みはクエリから等距離
        let a_id = db
            .insert_chunk(doc_id, 0, Some("Errors"), "E0382 is a move error", &dummy_embedding(0.5))
            .unwrap();
        // chunk B: 完全一致語を含まない。埋め込みはクエリから等距離
        let b_id = db
            .insert_chunk(doc_id, 1, Some("Other"), "unrelated content here", &dummy_embedding(0.5))
            .unwrap();

        let hits = db
            .search_hybrid("E0382", &dummy_embedding(0.5), 5, None, None)
            .unwrap();
        assert_eq!(hits.len(), 2);
        // FTS でヒットするのは A だけ → A が上位
        assert!(hits[0].content.contains("E0382"), "got: {:?}", hits[0].content);
        let _ = (a_id, b_id);
    }

    #[test]
    fn test_search_hybrid_falls_back_when_fts_query_empty() {
        let db = db_with_384();
        let doc_id = db
            .upsert_document("a.md", Some("a"), None, None, None, &[], None, "h")
            .unwrap();
        db.insert_chunk(doc_id, 0, None, "content", &dummy_embedding(0.1))
            .unwrap();

        // 2 文字クエリ → sanitize が None → vec-only
        let hits = db
            .search_hybrid("ab", &dummy_embedding(0.1), 5, None, None)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].score > 0.0, "RRF スコアは正の有限値");
    }

    #[test]
    fn test_search_hybrid_overfetches_when_filter_is_selective() {
        // filter で多数の候補が落ちるケース: BGE-small-en-v1.5 の 384 dim で
        // 20 ドキュメント挿入するが、category 一致は 1 件のみ。
        // limit=5 のとき、filter がなければ 5 件返るが、選択的な filter で
        // 1 件 しか残らない。over-fetch で target 側を 10 倍広げているため、
        // その 1 件を取りこぼさない。
        let db = db_with_384();
        for i in 0..20 {
            let path = format!("noise/doc_{i}.md");
            let cat = if i == 0 { "target" } else { "noise" };
            let doc_id = db
                .upsert_document(&path, Some("x"), None, Some(cat), None, &[], None, "h")
                .unwrap();
            db.insert_chunk(doc_id, 0, None, "content", &dummy_embedding(0.5))
                .unwrap();
        }

        let hits = db
            .search_hybrid("noexistent_query", &dummy_embedding(0.5), 5, Some("target"), None)
            .unwrap();
        assert_eq!(hits.len(), 1, "target カテゴリの 1 件を取りこぼさない");
    }

    #[test]
    fn test_search_hybrid_japanese_trigram() {
        let db = db_with_384();
        let doc_id = db
            .upsert_document("ja.md", Some("ja"), None, None, None, &[], None, "h")
            .unwrap();
        db.insert_chunk(
            doc_id,
            0,
            Some("見出し"),
            "E0382 は value moved エラーです",
            &dummy_embedding(0.7),
        )
        .unwrap();
        db.insert_chunk(doc_id, 1, None, "unrelated", &dummy_embedding(0.9))
            .unwrap();

        // 日本語 3 文字 "エラー" が trigram でヒットする
        let hits = db
            .search_hybrid("エラー", &dummy_embedding(0.7), 5, None, None)
            .unwrap();
        assert!(!hits.is_empty());
        assert!(
            hits.iter().any(|h| h.content.contains("エラー")),
            "Japanese trigram should hit"
        );
    }

    #[test]
    fn test_backfill_fts_hydrates_preexisting_db() {
        let db = db_with_384();
        let doc_id = db
            .upsert_document("a.md", Some("a"), None, None, None, &[], None, "h")
            .unwrap();
        db.insert_chunk(doc_id, 0, Some("H1"), "hello world", &dummy_embedding(0.1))
            .unwrap();
        db.insert_chunk(doc_id, 1, Some("H2"), "second chunk", &dummy_embedding(0.2))
            .unwrap();
        assert_eq!(fts_count(&db), 2);

        // pre-feature-9 DB を模擬: FTS だけ空にする
        db.conn.execute("DELETE FROM fts_chunks", []).unwrap();
        assert_eq!(fts_count(&db), 0);

        let n = db.backfill_fts().unwrap();
        assert_eq!(n, 2);
        assert_eq!(fts_count(&db), 2);

        // 冪等: 2 回目は 0 件
        let n2 = db.backfill_fts().unwrap();
        assert_eq!(n2, 0);
    }

    #[test]
    fn test_reset_for_model_switches_dim_and_wipes_data() {
        let db = db_with_384();
        let doc_id = db
            .upsert_document("a.md", Some("a"), None, None, None, &[], None, "h")
            .unwrap();
        db.insert_chunk(doc_id, 0, None, "hi", &dummy_embedding(0.1))
            .unwrap();
        assert_eq!(db.chunk_count().unwrap(), 1);
        assert_eq!(db.document_count().unwrap(), 1);

        db.reset_for_model("bge-m3", 1024).unwrap();

        assert_eq!(db.chunk_count().unwrap(), 0);
        assert_eq!(db.document_count().unwrap(), 0);
        assert_eq!(db.current_vec_dim().unwrap(), Some(1024));
        assert_eq!(
            db.read_embedding_meta().unwrap(),
            Some(("bge-m3".to_string(), 1024))
        );

        // 1024-dim insert が通る
        let doc_id2 = db
            .upsert_document("b.md", Some("b"), None, None, None, &[], None, "h")
            .unwrap();
        let emb: Vec<f32> = vec![0.2; 1024];
        db.insert_chunk(doc_id2, 0, None, "hi2", &emb).unwrap();
        assert_eq!(db.chunk_count().unwrap(), 1);
    }

    #[test]
    fn test_verify_embedding_meta_fresh_db() {
        let db = Database::open_in_memory().unwrap();
        assert!(db.read_embedding_meta().unwrap().is_none());

        db.verify_embedding_meta("bge-small-en-v1.5", 384).unwrap();

        let meta = db.read_embedding_meta().unwrap();
        assert_eq!(meta, Some(("bge-small-en-v1.5".to_string(), 384)));
    }

    #[test]
    fn test_verify_embedding_meta_migrates_preexisting_db() {
        // Simulate a pre-feature-8 DB: chunks exist but meta is empty.
        // In pre-feature-8 code `init()` always created vec_chunks with the
        // 384-dim literal. Reproduce that here by creating it manually.
        let db = Database::open_in_memory().unwrap();
        db.ensure_vec_chunks_table(384).unwrap();
        let doc_id = db
            .upsert_document(
                "deep-dive/mcp/overview.md",
                Some("MCP Overview"),
                Some("mcp"),
                Some("deep-dive"),
                None,
                &[],
                Some("2026-04-16"),
                "h",
            )
            .unwrap();
        db.insert_chunk(doc_id, 0, None, "hi", &dummy_embedding(0.1))
            .unwrap();
        assert!(db.read_embedding_meta().unwrap().is_none());

        db.verify_embedding_meta("bge-small-en-v1.5", 384).unwrap();

        assert_eq!(
            db.read_embedding_meta().unwrap(),
            Some(("bge-small-en-v1.5".to_string(), 384))
        );
    }

    #[test]
    fn test_verify_embedding_meta_idempotent_on_match() {
        let db = Database::open_in_memory().unwrap();
        db.verify_embedding_meta("bge-small-en-v1.5", 384).unwrap();
        // Second call with same args must succeed.
        db.verify_embedding_meta("bge-small-en-v1.5", 384).unwrap();
    }

    #[test]
    fn test_verify_embedding_meta_detects_mismatch() {
        let db = Database::open_in_memory().unwrap();
        db.verify_embedding_meta("bge-small-en-v1.5", 384).unwrap();

        let err = db
            .verify_embedding_meta("bge-m3", 1024)
            .expect_err("mismatch must be rejected");
        let msg = err.to_string();
        assert!(msg.contains("bge-small-en-v1.5"), "msg: {msg}");
        assert!(msg.contains("bge-m3"), "msg: {msg}");
        assert!(msg.contains("--force"), "msg: {msg}");
    }

    #[test]
    fn test_read_embedding_meta_returns_none_when_half_written() {
        let db = Database::open_in_memory().unwrap();
        db.conn
            .execute(
                "INSERT INTO index_meta (key, value) VALUES ('embedding_model', 'x')",
                [],
            )
            .unwrap();
        // dim missing → None (not an error, treated as unrecorded).
        assert!(db.read_embedding_meta().unwrap().is_none());
    }

    #[test]
    fn test_list_topics() {
        let db = Database::open_in_memory().unwrap();

        // 3 docs across 2 topic groups
        db.upsert_document(
            "deep-dive/mcp/overview.md",
            Some("MCP Overview"),
            Some("mcp"),
            Some("deep-dive"),
            Some("1"),
            &[],
            Some("2026-04-15"),
            "h1",
        )
        .unwrap();
        db.upsert_document(
            "deep-dive/mcp/features.md",
            Some("MCP Features"),
            Some("mcp"),
            Some("deep-dive"),
            Some("3"),
            &[],
            Some("2026-04-16"),
            "h2",
        )
        .unwrap();
        db.upsert_document(
            "ai-news/2026-04-16.md",
            Some("AI News Today"),
            None,
            Some("ai-news"),
            None,
            &[],
            Some("2026-04-16"),
            "h3",
        )
        .unwrap();

        let topics = db.list_topics().unwrap();
        println!("topics: {topics:#?}");

        assert_eq!(topics.len(), 2, "2 distinct (category,topic) groups");

        // Find the ai-news group (topic = None)
        let ai = topics
            .iter()
            .find(|t| t.category.as_deref() == Some("ai-news"))
            .expect("should have ai-news group");
        assert_eq!(ai.file_count, 1);
        assert!(ai.titles.contains(&"AI News Today".to_string()));

        // Find the deep-dive/mcp group
        let mcp = topics
            .iter()
            .find(|t| t.topic.as_deref() == Some("mcp"))
            .expect("should have mcp group");
        assert_eq!(mcp.file_count, 2);
        assert!(mcp.titles.contains(&"MCP Overview".to_string()));
        assert!(mcp.titles.contains(&"MCP Features".to_string()));

        println!("test_list_topics: OK");
    }
}
