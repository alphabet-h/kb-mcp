use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::sync::Once;

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

        self.conn.execute_batch(
            "
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

            CREATE VIRTUAL TABLE IF NOT EXISTS vec_chunks USING vec0(
                chunk_id INTEGER PRIMARY KEY,
                embedding float[384]
            );

            CREATE TABLE IF NOT EXISTS index_meta (
                key   TEXT PRIMARY KEY,
                value TEXT
            );
            ",
        )?;
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
            // Delete old vector entries for chunks that belong to this document
            self.conn.execute(
                "DELETE FROM vec_chunks WHERE chunk_id IN \
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

    /// Insert a chunk row **and** its corresponding vec_chunks embedding.
    ///
    /// `embedding` must be a 384-element f32 slice (matching the vec0 schema).
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

    /// Delete a document and all associated chunks / vectors.
    pub fn delete_document(&self, path: &str) -> Result<()> {
        // Delete vector entries first (no FK from virtual table)
        self.conn.execute(
            "DELETE FROM vec_chunks WHERE chunk_id IN \
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

    /// Vector-similarity search. Returns up to `limit` results ordered by
    /// ascending distance (lower = more similar).
    ///
    /// Optional `category` / `topic` filters restrict the search to documents
    /// matching those metadata values.
    pub fn search_similar(
        &self,
        query_embedding: &[f32],
        limit: u32,
        category: Option<&str>,
        topic: Option<&str>,
    ) -> Result<Vec<SearchResult>> {
        let embedding_json = serde_json::to_string(query_embedding)?;

        // When no filters are supplied we can query vec_chunks directly and
        // join afterwards. With filters we need a sub-select to restrict the
        // candidate set.
        let has_filters = category.is_some() || topic.is_some();

        if has_filters {
            // Build a filtered query: first find matching chunk_ids, then
            // search within those using vec_chunks.
            // sqlite-vec does not support arbitrary WHERE on the virtual table
            // together with MATCH, so we do a two-step approach: fetch
            // candidates from vec_chunks (generous limit), then filter in Rust.
            let generous_limit = limit.saturating_mul(10).min(10_000); // over-fetch, capped at 10k
            let sql = "
                SELECT v.chunk_id, v.distance,
                       c.content, c.heading,
                       d.path, d.title, d.topic, d.date, d.category
                FROM vec_chunks v
                JOIN chunks c ON c.id = v.chunk_id
                JOIN documents d ON d.id = c.document_id
                WHERE v.embedding MATCH ?1
                ORDER BY v.distance
                LIMIT ?2
            ";
            let mut stmt = self.conn.prepare(sql)?;
            let rows = stmt.query_map(params![embedding_json, generous_limit], |row| {
                Ok((
                    row.get::<_, f32>(1)?,         // distance
                    row.get::<_, String>(2)?,       // content
                    row.get::<_, Option<String>>(3)?, // heading
                    row.get::<_, String>(4)?,       // path
                    row.get::<_, Option<String>>(5)?, // title
                    row.get::<_, Option<String>>(6)?, // topic
                    row.get::<_, Option<String>>(7)?, // date
                    row.get::<_, Option<String>>(8)?, // category
                ))
            })?;

            let mut results = Vec::new();
            for row in rows {
                let (distance, content, heading, path, title, r_topic, date, r_category) =
                    row?;

                // Apply metadata filters
                if let Some(cat) = category
                    && r_category.as_deref() != Some(cat) {
                        continue;
                    }
                if let Some(top) = topic
                    && r_topic.as_deref() != Some(top) {
                        continue;
                    }
                results.push(SearchResult {
                    score: distance,
                    content,
                    heading,
                    path,
                    title,
                    topic: r_topic,
                    date,
                });
                if results.len() >= limit as usize {
                    break;
                }
            }
            Ok(results)
        } else {
            let sql = "
                SELECT v.chunk_id, v.distance,
                       c.content, c.heading,
                       d.path, d.title, d.topic, d.date
                FROM vec_chunks v
                JOIN chunks c ON c.id = v.chunk_id
                JOIN documents d ON d.id = c.document_id
                WHERE v.embedding MATCH ?1
                ORDER BY v.distance
                LIMIT ?2
            ";
            let mut stmt = self.conn.prepare(sql)?;
            let rows = stmt.query_map(params![embedding_json, limit], |row| {
                Ok(SearchResult {
                    score: row.get(1)?,
                    content: row.get(2)?,
                    heading: row.get(3)?,
                    path: row.get(4)?,
                    title: row.get(5)?,
                    topic: row.get(6)?,
                    date: row.get(7)?,
                })
            })?;
            rows.into_iter()
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(Into::into)
        }
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

    #[test]
    fn test_schema_creation() {
        let db = Database::open_in_memory().expect("open_in_memory");
        assert_eq!(db.document_count().unwrap(), 0);
        assert_eq!(db.chunk_count().unwrap(), 0);
        println!("test_schema_creation: OK — 0 docs, 0 chunks after fresh init");
    }

    #[test]
    fn test_upsert_and_query_document() {
        let db = Database::open_in_memory().unwrap();

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
        let db = Database::open_in_memory().unwrap();

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
