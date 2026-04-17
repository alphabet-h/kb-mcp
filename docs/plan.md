# kb-mcp Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** knowledge-base/ を外部からセマンティック検索できる読み取り専用 MCP サーバーを Rust シングルバイナリで構築する

**Architecture:** rmcp (MCP stdio server) + fastembed-rs (ONNX embedding) + sqlite-vec (vector search in SQLite)。Markdown を見出し単位でチャンキングし、384 次元 embedding で sqlite-vec に格納。5 つの MCP ツール（search / list_topics / get_document / get_best_practice / rebuild_index）を提供。

**Tech Stack:** Rust, rmcp ^1.3, fastembed ^5, sqlite-vec ^0.1, rusqlite (bundled), pulldown-cmark, clap ^4, tokio ^1

**Design Spec:** `feature-idea/kb-mcp-design.md`

---

### Task 1: プロジェクトスキャフォールド

**Files:**
- Create: `kb-mcp/Cargo.toml`
- Create: `kb-mcp/src/main.rs`
- Modify: `.gitignore`

- [ ] **Step 1: Cargo プロジェクト作成**

```bash
cd C:/Users/yabushita/workspace/repos/private/ai_organization
cargo init kb-mcp
```

- [ ] **Step 2: Cargo.toml に依存関係を記述**

`kb-mcp/Cargo.toml`:
```toml
[package]
name = "kb-mcp"
version = "0.1.0"
edition = "2021"
description = "MCP server for semantic search over a Markdown knowledge base"

[dependencies]
# MCP
rmcp = { version = "1.3", features = ["server", "transport-io", "macros"] }
tokio = { version = "1", features = ["full"] }

# Embedding
fastembed = "5"

# Database
rusqlite = { version = "0.31", features = ["bundled"] }
sqlite-vec = "0.1"

# Markdown / YAML
pulldown-cmark = "0.12"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
serde_yaml = "0.9"

# CLI
clap = { version = "4", features = ["derive"] }

# Utilities
sha2 = "0.10"
chrono = "0.4"
anyhow = "1"
tracing = "0.1"
tracing-subscriber = "0.3"
walkdir = "2"
```

- [ ] **Step 3: 最小限の main.rs を記述**

`kb-mcp/src/main.rs`:
```rust
use clap::Parser;

#[derive(Parser)]
#[command(name = "kb-mcp", version, about = "MCP server for knowledge base semantic search")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(clap::Subcommand)]
enum Commands {
    /// Run as MCP server (stdio)
    Serve {
        #[arg(long)]
        kb_path: String,
    },
    /// Rebuild the search index
    Index {
        #[arg(long)]
        kb_path: String,
        #[arg(long, default_value_t = false)]
        force: bool,
    },
    /// Show index status
    Status {
        #[arg(long)]
        kb_path: String,
    },
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Commands::Serve { kb_path } => {
            println!("serve: {kb_path}");
        }
        Commands::Index { kb_path, force } => {
            println!("index: {kb_path} (force={force})");
        }
        Commands::Status { kb_path } => {
            println!("status: {kb_path}");
        }
    }
}
```

- [ ] **Step 4: ビルド確認**

```bash
cd kb-mcp && cargo build 2>&1
```
Expected: `Compiling kb-mcp v0.1.0` ... `Finished`（初回は依存 DL で数分かかる）

- [ ] **Step 5: CLI ヘルプ確認**

```bash
cargo run -- --help
cargo run -- serve --help
```
Expected: サブコマンド一覧と引数が表示される

- [ ] **Step 6: .gitignore に DB ファイルを追加**

`.gitignore` に追記:
```
.kb-mcp.db
kb-mcp/target/
```

- [ ] **Step 7: コミット**

```bash
cd .. && git add kb-mcp/Cargo.toml kb-mcp/src/main.rs .gitignore
git commit -m "feat(kb-mcp): scaffold Rust project with CLI skeleton"
```

---

### Task 2: Markdown パーサー（frontmatter 抽出 + チャンキング）

**Files:**
- Create: `kb-mcp/src/markdown.rs`
- Create: `kb-mcp/tests/fixtures/sample.md`
- Create: `kb-mcp/tests/fixtures/no_frontmatter.md`
- Modify: `kb-mcp/src/main.rs` (mod 宣言)

- [ ] **Step 1: テスト用 fixture を作成**

`kb-mcp/tests/fixtures/sample.md`:
```markdown
---
title: "Sample Document"
date: 2026-04-17
topic: "test-topic"
depth: "overview"
tags:
  - test
  - sample
---

# Sample Document

Introduction paragraph.

## Section One

Content of section one. This has enough text to be a valid chunk.
More content here to ensure it passes the minimum length filter.

### Subsection 1.1

Detailed content in subsection. Code example:

\```python
print("hello")
\```

## Section Two

Content of section two with sufficient length for chunking.
Additional sentences to make this a meaningful chunk of text.

## 次の深堀り候補

- [ ] This should be excluded from indexing
- [ ] This too
```

`kb-mcp/tests/fixtures/no_frontmatter.md`:
```markdown
# No Frontmatter

Just plain content without YAML frontmatter.

## A Section

Some text here.
```

- [ ] **Step 2: データ構造を定義**

`kb-mcp/src/markdown.rs`:
```rust
use serde::Deserialize;

#[derive(Debug, Clone, Default)]
pub struct Frontmatter {
    pub title: Option<String>,
    pub date: Option<String>,
    pub topic: Option<String>,
    pub depth: Option<String>,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Chunk {
    pub index: usize,
    pub heading: Option<String>,
    pub content: String,
}

#[derive(Debug)]
pub struct ParsedDocument {
    pub frontmatter: Frontmatter,
    pub chunks: Vec<Chunk>,
    pub raw_content: String,
}
```

- [ ] **Step 3: frontmatter パーサーを実装**

`kb-mcp/src/markdown.rs` に追加:
```rust
#[derive(Deserialize)]
struct RawFrontmatter {
    title: Option<String>,
    date: Option<serde_yaml::Value>,
    topic: Option<String>,
    depth: Option<String>,
    tags: Option<Vec<String>>,
}

fn parse_frontmatter(content: &str) -> (Frontmatter, &str) {
    if !content.starts_with("---\n") {
        return (Frontmatter::default(), content);
    }
    let rest = &content[4..];
    let Some(end) = rest.find("\n---") else {
        return (Frontmatter::default(), content);
    };
    let yaml_str = &rest[..end];
    let body = &rest[end + 4..];
    let body = body.strip_prefix('\n').unwrap_or(body);

    let fm = match serde_yaml::from_str::<RawFrontmatter>(yaml_str) {
        Ok(raw) => Frontmatter {
            title: raw.title,
            date: raw.date.map(|v| match v {
                serde_yaml::Value::String(s) => s,
                other => format!("{other}"),
            }),
            topic: raw.topic,
            depth: raw.depth,
            tags: raw.tags.unwrap_or_default(),
        },
        Err(_) => Frontmatter::default(),
    };
    (fm, body)
}
```

- [ ] **Step 4: チャンキングを実装**

`kb-mcp/src/markdown.rs` に追加:
```rust
const MIN_CHUNK_CHARS: usize = 50;
const EXCLUDED_HEADINGS: &[&str] = &["次の深堀り候補"];

pub fn parse_document(content: &str) -> ParsedDocument {
    let (frontmatter, body) = parse_frontmatter(content);
    let chunks = chunk_by_headings(body);
    ParsedDocument {
        frontmatter,
        chunks,
        raw_content: content.to_string(),
    }
}

fn chunk_by_headings(body: &str) -> Vec<Chunk> {
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut current_heading: Option<String> = None;
    let mut current_lines: Vec<&str> = Vec::new();
    let mut excluded = false;

    for line in body.lines() {
        if line.starts_with("## ") || line.starts_with("### ") {
            // Flush previous chunk
            if !excluded {
                flush_chunk(&mut chunks, &current_heading, &current_lines);
            }
            current_lines.clear();

            let heading_text = line.trim_start_matches('#').trim();
            excluded = EXCLUDED_HEADINGS.iter().any(|h| heading_text == *h);
            current_heading = Some(heading_text.to_string());
        } else {
            current_lines.push(line);
        }
    }
    // Flush last chunk
    if !excluded {
        flush_chunk(&mut chunks, &current_heading, &current_lines);
    }

    // Merge short chunks into previous
    merge_short_chunks(&mut chunks);

    // Re-index
    for (i, chunk) in chunks.iter_mut().enumerate() {
        chunk.index = i;
    }
    chunks
}

fn flush_chunk(chunks: &mut Vec<Chunk>, heading: &Option<String>, lines: &[&str]) {
    let content = lines.join("\n").trim().to_string();
    if content.is_empty() {
        return;
    }
    chunks.push(Chunk {
        index: chunks.len(),
        heading: heading.clone(),
        content,
    });
}

fn merge_short_chunks(chunks: &mut Vec<Chunk>) {
    let mut i = 1;
    while i < chunks.len() {
        if chunks[i].content.len() < MIN_CHUNK_CHARS {
            let short = chunks.remove(i);
            let prev = &mut chunks[i - 1];
            prev.content.push_str("\n\n");
            prev.content.push_str(&short.content);
        } else {
            i += 1;
        }
    }
}
```

- [ ] **Step 5: テストを書いて実行**

`kb-mcp/src/markdown.rs` の末尾に追加:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_frontmatter() {
        let content = "---\ntitle: \"Test\"\ndate: 2026-04-17\ntags:\n  - a\n  - b\n---\n\n# Body\n";
        let (fm, body) = parse_frontmatter(content);
        assert_eq!(fm.title.as_deref(), Some("Test"));
        assert_eq!(fm.tags, vec!["a", "b"]);
        assert!(body.contains("# Body"));
    }

    #[test]
    fn test_no_frontmatter() {
        let content = "# Just a heading\n\nSome text.\n";
        let (fm, body) = parse_frontmatter(content);
        assert!(fm.title.is_none());
        assert_eq!(body, content);
    }

    #[test]
    fn test_chunking_excludes_next_candidates() {
        let content = std::fs::read_to_string("tests/fixtures/sample.md").unwrap();
        let doc = parse_document(&content);
        let headings: Vec<_> = doc.chunks.iter().filter_map(|c| c.heading.as_deref()).collect();
        assert!(!headings.contains(&"次の深堀り候補"));
    }

    #[test]
    fn test_chunking_produces_multiple_chunks() {
        let content = std::fs::read_to_string("tests/fixtures/sample.md").unwrap();
        let doc = parse_document(&content);
        assert!(doc.chunks.len() >= 2, "Expected at least 2 chunks, got {}", doc.chunks.len());
    }

    #[test]
    fn test_frontmatter_extraction() {
        let content = std::fs::read_to_string("tests/fixtures/sample.md").unwrap();
        let doc = parse_document(&content);
        assert_eq!(doc.frontmatter.title.as_deref(), Some("Sample Document"));
        assert_eq!(doc.frontmatter.topic.as_deref(), Some("test-topic"));
    }
}
```

```bash
cd kb-mcp && cargo test markdown -- --nocapture
```
Expected: 5 tests passed

- [ ] **Step 6: main.rs に mod 宣言を追加**

`kb-mcp/src/main.rs` の先頭に追加:
```rust
mod markdown;
```

- [ ] **Step 7: コミット**

```bash
git add kb-mcp/src/markdown.rs kb-mcp/src/main.rs kb-mcp/tests/
git commit -m "feat(kb-mcp): markdown parser with frontmatter extraction and heading-based chunking"
```

---

### Task 3: DB レイヤー（SQLite + sqlite-vec スキーマ）

**Files:**
- Create: `kb-mcp/src/db.rs`
- Modify: `kb-mcp/src/main.rs` (mod 宣言)

- [ ] **Step 1: DB モジュールを実装**

`kb-mcp/src/db.rs`:
```rust
use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use sqlite_vec::sqlite3_vec_init;

const SCHEMA_VERSION: &str = "1";
const EMBEDDING_DIM: usize = 384;

pub struct Database {
    pub conn: Connection,
}

impl Database {
    pub fn open(path: &str) -> Result<Self> {
        unsafe {
            rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute(
                sqlite3_vec_init as *const (),
            )));
        }
        let conn = Connection::open(path)
            .with_context(|| format!("Failed to open database: {path}"))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        let db = Database { conn };
        db.ensure_schema()?;
        Ok(db)
    }

    pub fn open_in_memory() -> Result<Self> {
        unsafe {
            rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute(
                sqlite3_vec_init as *const (),
            )));
        }
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        let db = Database { conn };
        db.ensure_schema()?;
        Ok(db)
    }

    fn ensure_schema(&self) -> Result<()> {
        self.conn.execute_batch(&format!(
            r#"
            CREATE TABLE IF NOT EXISTS documents (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                path        TEXT UNIQUE NOT NULL,
                title       TEXT,
                topic       TEXT,
                category    TEXT,
                depth       TEXT,
                tags        TEXT,
                date        TEXT,
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
                embedding float[{EMBEDDING_DIM}]
            );

            CREATE TABLE IF NOT EXISTS index_meta (
                key   TEXT PRIMARY KEY,
                value TEXT
            );

            INSERT OR IGNORE INTO index_meta(key, value) VALUES
                ('schema_version', '{SCHEMA_VERSION}'),
                ('embedding_dim', '{EMBEDDING_DIM}');
            "#
        ))?;
        Ok(())
    }

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
        let tags_json = serde_json::to_string(tags)?;
        let now = chrono::Utc::now().to_rfc3339();

        // Delete existing chunks + vec entries for this document
        if let Ok(doc_id) = self.conn.query_row(
            "SELECT id FROM documents WHERE path = ?1",
            params![path],
            |row| row.get::<_, i64>(0),
        ) {
            self.delete_chunks_for_document(doc_id)?;
        }

        self.conn.execute(
            r#"INSERT INTO documents (path, title, topic, category, depth, tags, date, content_hash, last_indexed)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
               ON CONFLICT(path) DO UPDATE SET
                 title=?2, topic=?3, category=?4, depth=?5, tags=?6, date=?7,
                 content_hash=?8, last_indexed=?9"#,
            params![path, title, topic, category, depth, tags_json, date, content_hash, now],
        )?;

        let doc_id = self.conn.query_row(
            "SELECT id FROM documents WHERE path = ?1",
            params![path],
            |row| row.get(0),
        )?;
        Ok(doc_id)
    }

    pub fn insert_chunk(
        &self,
        document_id: i64,
        chunk_index: usize,
        heading: Option<&str>,
        content: &str,
        embedding: &[f32],
    ) -> Result<i64> {
        let token_count = content.len() / 4; // rough estimate
        self.conn.execute(
            "INSERT INTO chunks (document_id, chunk_index, heading, content, token_count) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![document_id, chunk_index as i64, heading, content, token_count as i64],
        )?;
        let chunk_id = self.conn.last_insert_rowid();

        let embedding_json = serde_json::to_string(embedding)?;
        self.conn.execute(
            "INSERT INTO vec_chunks (chunk_id, embedding) VALUES (?1, ?2)",
            params![chunk_id, embedding_json],
        )?;
        Ok(chunk_id)
    }

    fn delete_chunks_for_document(&self, doc_id: i64) -> Result<()> {
        let chunk_ids: Vec<i64> = self.conn
            .prepare("SELECT id FROM chunks WHERE document_id = ?1")?
            .query_map(params![doc_id], |row| row.get(0))?
            .filter_map(|r| r.ok())
            .collect();
        for cid in &chunk_ids {
            self.conn.execute("DELETE FROM vec_chunks WHERE chunk_id = ?1", params![cid])?;
        }
        self.conn.execute("DELETE FROM chunks WHERE document_id = ?1", params![doc_id])?;
        Ok(())
    }

    pub fn get_document_hash(&self, path: &str) -> Result<Option<String>> {
        match self.conn.query_row(
            "SELECT content_hash FROM documents WHERE path = ?1",
            params![path],
            |row| row.get(0),
        ) {
            Ok(hash) => Ok(Some(hash)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn delete_document(&self, path: &str) -> Result<()> {
        if let Ok(doc_id) = self.conn.query_row(
            "SELECT id FROM documents WHERE path = ?1",
            params![path],
            |row| row.get::<_, i64>(0),
        ) {
            self.delete_chunks_for_document(doc_id)?;
            self.conn.execute("DELETE FROM documents WHERE id = ?1", params![doc_id])?;
        }
        Ok(())
    }

    pub fn search_similar(
        &self,
        query_embedding: &[f32],
        limit: u32,
        category: Option<&str>,
        topic: Option<&str>,
    ) -> Result<Vec<SearchResult>> {
        let embedding_json = serde_json::to_string(query_embedding)?;
        let mut sql = String::from(
            r#"SELECT
                 v.chunk_id,
                 v.distance,
                 c.content,
                 c.heading,
                 d.path,
                 d.title,
                 d.topic,
                 d.date
               FROM vec_chunks v
               JOIN chunks c ON c.id = v.chunk_id
               JOIN documents d ON d.id = c.document_id
               WHERE v.embedding MATCH ?1
            "#
        );
        if category.is_some() {
            sql.push_str(" AND d.category = ?3");
        }
        if topic.is_some() {
            sql.push_str(if category.is_some() { " AND d.topic = ?4" } else { " AND d.topic = ?3" });
        }
        sql.push_str(&format!(" ORDER BY v.distance LIMIT {limit}"));

        let mut stmt = self.conn.prepare(&sql)?;

        // Build params dynamically
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = vec![
            Box::new(embedding_json),
            Box::new(limit as i64),
        ];
        if let Some(cat) = category {
            param_values.push(Box::new(cat.to_string()));
        }
        if let Some(top) = topic {
            param_values.push(Box::new(top.to_string()));
        }
        let param_refs: Vec<&dyn rusqlite::types::ToSql> = param_values.iter().map(|p| p.as_ref()).collect();

        let results = stmt.query_map(param_refs.as_slice(), |row| {
            Ok(SearchResult {
                score: row.get(1)?,
                content: row.get(2)?,
                heading: row.get(3)?,
                path: row.get(4)?,
                title: row.get(5)?,
                topic: row.get(6)?,
                date: row.get(7)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

        Ok(results)
    }

    pub fn list_topics(&self) -> Result<Vec<TopicInfo>> {
        let mut stmt = self.conn.prepare(
            r#"SELECT category, topic, COUNT(*) as cnt, MAX(date) as last_date,
                      GROUP_CONCAT(title, '|||') as titles
               FROM documents
               GROUP BY category, topic
               ORDER BY category, topic"#
        )?;
        let results = stmt.query_map([], |row| {
            let titles_str: String = row.get(4)?;
            Ok(TopicInfo {
                category: row.get(0)?,
                topic: row.get(1)?,
                file_count: row.get(2)?,
                last_updated: row.get(3)?,
                titles: titles_str.split("|||").map(String::from).collect(),
            })
        })?
        .filter_map(|r| r.ok())
        .collect();
        Ok(results)
    }

    pub fn document_count(&self) -> Result<u32> {
        self.conn.query_row("SELECT COUNT(*) FROM documents", [], |row| row.get(0))
            .map_err(Into::into)
    }

    pub fn chunk_count(&self) -> Result<u32> {
        self.conn.query_row("SELECT COUNT(*) FROM chunks", [], |row| row.get(0))
            .map_err(Into::into)
    }

    pub fn all_document_paths(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT path FROM documents")?;
        let paths = stmt.query_map([], |row| row.get(0))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(paths)
    }
}

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

#[derive(Debug, Clone)]
pub struct TopicInfo {
    pub category: Option<String>,
    pub topic: Option<String>,
    pub file_count: u32,
    pub last_updated: Option<String>,
    pub titles: Vec<String>,
}
```

- [ ] **Step 2: テストを追加**

`kb-mcp/src/db.rs` の末尾:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema_creation() {
        let db = Database::open_in_memory().unwrap();
        assert_eq!(db.document_count().unwrap(), 0);
        assert_eq!(db.chunk_count().unwrap(), 0);
    }

    #[test]
    fn test_upsert_and_query_document() {
        let db = Database::open_in_memory().unwrap();
        let doc_id = db.upsert_document(
            "test/doc.md", Some("Test"), Some("test"), Some("deep-dive"),
            Some("overview"), &["tag1".into()], Some("2026-04-17"), "abc123",
        ).unwrap();
        assert!(doc_id > 0);
        assert_eq!(db.document_count().unwrap(), 1);

        // Upsert same path updates
        let doc_id2 = db.upsert_document(
            "test/doc.md", Some("Updated"), Some("test"), Some("deep-dive"),
            Some("overview"), &[], Some("2026-04-17"), "def456",
        ).unwrap();
        assert_eq!(db.document_count().unwrap(), 1);
        assert_eq!(doc_id, doc_id2);
    }

    #[test]
    fn test_content_hash_check() {
        let db = Database::open_in_memory().unwrap();
        assert!(db.get_document_hash("nonexistent.md").unwrap().is_none());

        db.upsert_document(
            "doc.md", None, None, None, None, &[], None, "hash1",
        ).unwrap();
        assert_eq!(db.get_document_hash("doc.md").unwrap().as_deref(), Some("hash1"));
    }

    #[test]
    fn test_delete_document() {
        let db = Database::open_in_memory().unwrap();
        db.upsert_document("doc.md", None, None, None, None, &[], None, "h").unwrap();
        assert_eq!(db.document_count().unwrap(), 1);
        db.delete_document("doc.md").unwrap();
        assert_eq!(db.document_count().unwrap(), 0);
    }

    #[test]
    fn test_list_topics() {
        let db = Database::open_in_memory().unwrap();
        db.upsert_document("a/1.md", Some("A1"), Some("topicA"), Some("deep-dive"), None, &[], Some("2026-04-17"), "h1").unwrap();
        db.upsert_document("a/2.md", Some("A2"), Some("topicA"), Some("deep-dive"), None, &[], Some("2026-04-16"), "h2").unwrap();
        db.upsert_document("b/1.md", Some("B1"), Some("topicB"), Some("ai-news"), None, &[], Some("2026-04-15"), "h3").unwrap();

        let topics = db.list_topics().unwrap();
        assert_eq!(topics.len(), 2);
        assert_eq!(topics[0].file_count, 1); // ai-news/topicB
        assert_eq!(topics[1].file_count, 2); // deep-dive/topicA
    }
}
```

- [ ] **Step 3: main.rs に mod 追加 + テスト実行**

`kb-mcp/src/main.rs` に追加:
```rust
mod db;
```

```bash
cd kb-mcp && cargo test db -- --nocapture
```
Expected: 5 tests passed

- [ ] **Step 4: コミット**

```bash
git add kb-mcp/src/db.rs kb-mcp/src/main.rs
git commit -m "feat(kb-mcp): database layer with sqlite-vec schema and CRUD operations"
```

---

### Task 4: Embedder（fastembed ラッパー）

**Files:**
- Create: `kb-mcp/src/embedder.rs`
- Modify: `kb-mcp/src/main.rs` (mod 宣言)

- [ ] **Step 1: Embedder を実装**

`kb-mcp/src/embedder.rs`:
```rust
use anyhow::Result;
use fastembed::{InitOptions, TextEmbedding, EmbeddingModel};

pub struct Embedder {
    model: TextEmbedding,
}

impl Embedder {
    pub fn new() -> Result<Self> {
        let options = InitOptions::new(EmbeddingModel::BGESmallENV15)
            .with_show_download_progress(true);
        let model = TextEmbedding::try_new(options)?;
        Ok(Embedder { model })
    }

    pub fn embed_texts(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let docs: Vec<String> = texts.iter().map(|t| t.to_string()).collect();
        let embeddings = self.model.embed(docs, None)?;
        Ok(embeddings)
    }

    pub fn embed_single(&self, text: &str) -> Result<Vec<f32>> {
        let results = self.embed_texts(&[text])?;
        results.into_iter().next().ok_or_else(|| anyhow::anyhow!("No embedding returned"))
    }

    pub fn dimension(&self) -> usize {
        384
    }
}
```

- [ ] **Step 2: main.rs に mod 追加**

```rust
mod embedder;
```

- [ ] **Step 3: ビルド確認**

```bash
cd kb-mcp && cargo build
```
Expected: コンパイル成功（fastembed の ONNX リンクが通る）

注意: 初回ビルドで ONNX Runtime のダウンロードが発生する場合がある。

- [ ] **Step 4: 簡易テスト（embed が動くか）**

`kb-mcp/src/embedder.rs` の末尾:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore] // 初回はモデル DL が必要なため CI では skip
    fn test_embed_produces_384_dim() {
        let embedder = Embedder::new().unwrap();
        let result = embedder.embed_single("hello world").unwrap();
        assert_eq!(result.len(), 384);
    }

    #[test]
    #[ignore]
    fn test_embed_batch() {
        let embedder = Embedder::new().unwrap();
        let results = embedder.embed_texts(&["hello", "world"]).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].len(), 384);
    }
}
```

```bash
cd kb-mcp && cargo test embedder -- --ignored --nocapture
```
Expected: 2 tests passed（初回はモデル DL で時間がかかる）

- [ ] **Step 5: コミット**

```bash
git add kb-mcp/src/embedder.rs kb-mcp/src/main.rs
git commit -m "feat(kb-mcp): fastembed wrapper for BGE-small-en-v1.5 embeddings"
```

---

### Task 5: Indexer（ファイル走査 + チャンキング + embedding → DB）

**Files:**
- Create: `kb-mcp/src/indexer.rs`
- Modify: `kb-mcp/src/main.rs` (mod 宣言 + index サブコマンド実装)

- [ ] **Step 1: Indexer を実装**

`kb-mcp/src/indexer.rs`:
```rust
use std::path::{Path, PathBuf};
use anyhow::Result;
use sha2::{Sha256, Digest};
use walkdir::WalkDir;

use crate::db::Database;
use crate::embedder::Embedder;
use crate::markdown;

#[derive(Debug)]
pub struct IndexResult {
    pub total_documents: u32,
    pub updated: u32,
    pub deleted: u32,
    pub total_chunks: u32,
    pub duration_ms: u64,
}

pub fn rebuild_index(db: &Database, embedder: &Embedder, kb_path: &Path, force: bool) -> Result<IndexResult> {
    let start = std::time::Instant::now();
    let mut updated = 0u32;
    let mut total_chunks = 0u32;

    let md_files = collect_md_files(kb_path)?;
    let existing_paths = db.all_document_paths()?;

    // Index new / changed files
    for file_path in &md_files {
        let rel_path = file_path.strip_prefix(kb_path)
            .unwrap_or(file_path)
            .to_string_lossy()
            .replace('\\', "/");

        let content = std::fs::read_to_string(file_path)?;
        let hash = sha256_hex(&content);

        if !force {
            if let Some(existing_hash) = db.get_document_hash(&rel_path)? {
                if existing_hash == hash {
                    // Count existing chunks
                    continue;
                }
            }
        }

        let doc = markdown::parse_document(&content);
        let (category, topic) = extract_category_topic(&rel_path);

        let doc_id = db.upsert_document(
            &rel_path,
            doc.frontmatter.title.as_deref(),
            doc.frontmatter.topic.as_deref().or(topic.as_deref()),
            category.as_deref(),
            doc.frontmatter.depth.as_deref(),
            &doc.frontmatter.tags,
            doc.frontmatter.date.as_deref(),
            &hash,
        )?;

        // Generate embeddings in batch
        let texts: Vec<&str> = doc.chunks.iter().map(|c| c.content.as_str()).collect();
        if texts.is_empty() {
            continue;
        }
        let embeddings = embedder.embed_texts(&texts)?;

        for (chunk, embedding) in doc.chunks.iter().zip(embeddings.iter()) {
            db.insert_chunk(doc_id, chunk.index, chunk.heading.as_deref(), &chunk.content, embedding)?;
            total_chunks += 1;
        }
        updated += 1;
    }

    // Delete documents no longer on disk
    let current_rel_paths: std::collections::HashSet<String> = md_files.iter()
        .map(|p| p.strip_prefix(kb_path).unwrap_or(p).to_string_lossy().replace('\\', "/"))
        .collect();
    let mut deleted = 0u32;
    for existing in &existing_paths {
        if !current_rel_paths.contains(existing) {
            db.delete_document(existing)?;
            deleted += 1;
        }
    }

    let total_documents = db.document_count()?;
    let total_chunks_final = db.chunk_count()?;

    Ok(IndexResult {
        total_documents,
        updated,
        deleted,
        total_chunks: total_chunks_final,
        duration_ms: start.elapsed().as_millis() as u64,
    })
}

fn collect_md_files(kb_path: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in WalkDir::new(kb_path).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.extension().map_or(false, |ext| ext == "md") {
            // Skip .obsidian directory
            let rel = path.strip_prefix(kb_path).unwrap_or(path);
            if !rel.to_string_lossy().starts_with(".obsidian") {
                files.push(path.to_path_buf());
            }
        }
    }
    Ok(files)
}

fn sha256_hex(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn extract_category_topic(rel_path: &str) -> (Option<String>, Option<String>) {
    let parts: Vec<&str> = rel_path.split('/').collect();
    match parts.len() {
        0 | 1 => (None, None),
        2 => (Some(parts[0].to_string()), None),
        _ => (Some(parts[0].to_string()), Some(parts[1].to_string())),
    }
}
```

- [ ] **Step 2: main.rs の Index サブコマンドを実装**

`kb-mcp/src/main.rs` に `mod indexer;` を追加し、`Commands::Index` のハンドラを書き換え:

```rust
mod markdown;
mod db;
mod embedder;
mod indexer;

// ... (Cli / Commands は既存のまま)

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    match cli.command {
        Commands::Serve { kb_path } => {
            println!("serve: {kb_path} (not yet implemented)");
        }
        Commands::Index { kb_path, force } => {
            let db_path = resolve_db_path(&kb_path);
            let db = db::Database::open(&db_path)?;
            eprintln!("Loading embedding model...");
            let embedder = embedder::Embedder::new()?;
            eprintln!("Indexing {kb_path}...");
            let result = indexer::rebuild_index(&db, &embedder, std::path::Path::new(&kb_path), force)?;
            eprintln!(
                "Done in {}ms: {} docs ({} updated, {} deleted), {} chunks",
                result.duration_ms, result.total_documents, result.updated, result.deleted, result.total_chunks
            );
        }
        Commands::Status { kb_path } => {
            let db_path = resolve_db_path(&kb_path);
            if !std::path::Path::new(&db_path).exists() {
                eprintln!("No index found. Run `kb-mcp index --kb-path {kb_path}` first.");
                return Ok(());
            }
            let db = db::Database::open(&db_path)?;
            eprintln!("Documents: {}", db.document_count()?);
            eprintln!("Chunks: {}", db.chunk_count()?);
        }
    }
    Ok(())
}

fn resolve_db_path(kb_path: &str) -> String {
    let parent = std::path::Path::new(kb_path).parent().unwrap_or(std::path::Path::new("."));
    parent.join(".kb-mcp.db").to_string_lossy().to_string()
}
```

- [ ] **Step 3: ビルド確認**

```bash
cd kb-mcp && cargo build
```

- [ ] **Step 4: 実データでインデックス構築テスト**

```bash
cargo run -- index --kb-path ../knowledge-base
```
Expected: `Loading embedding model...` → `Indexing...` → `Done in Xms: N docs (N updated, 0 deleted), M chunks`

- [ ] **Step 5: status コマンドで確認**

```bash
cargo run -- status --kb-path ../knowledge-base
```
Expected: `Documents: 100+` / `Chunks: 数百`

- [ ] **Step 6: コミット**

```bash
git add kb-mcp/src/indexer.rs kb-mcp/src/main.rs
git commit -m "feat(kb-mcp): indexer — scan, chunk, embed, and store knowledge base files"
```

---

### Task 6: MCP ツール実装 + サーバー起動

**Files:**
- Create: `kb-mcp/src/server.rs`
- Modify: `kb-mcp/src/main.rs` (serve サブコマンド)

- [ ] **Step 1: MCP サーバーとツールを実装**

`kb-mcp/src/server.rs`:
```rust
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use anyhow::Result;
use rmcp::prelude::*;
use serde_json::json;

use crate::db::Database;
use crate::embedder::Embedder;
use crate::indexer;

pub struct KbServer {
    db: Mutex<Database>,
    embedder: Embedder,
    kb_path: PathBuf,
}

impl KbServer {
    pub fn new(kb_path: &str) -> Result<Self> {
        let db_path = crate::resolve_db_path(kb_path);
        let db = Database::open(&db_path)?;
        let embedder = Embedder::new()?;
        Ok(KbServer {
            db: Mutex::new(db),
            embedder,
            kb_path: PathBuf::from(kb_path),
        })
    }
}

#[tool_router]
impl KbServer {
    #[tool(description = "Semantic search over the knowledge base. Returns relevant chunks ranked by cosine similarity.")]
    fn search(
        &self,
        #[param(description = "Natural language search query")] query: String,
        #[param(description = "Max results to return (default 5)")] limit: Option<u32>,
        #[param(description = "Filter by category: deep-dive, ai-news, tech-watch, best-practices")] category: Option<String>,
        #[param(description = "Filter by topic name, e.g. chromadb, claude-code")] topic: Option<String>,
    ) -> String {
        let limit = limit.unwrap_or(5);
        let embedding = match self.embedder.embed_single(&query) {
            Ok(e) => e,
            Err(e) => return json!({"error": format!("Embedding failed: {e}")}).to_string(),
        };
        let db = self.db.lock().unwrap();
        match db.search_similar(&embedding, limit, category.as_deref(), topic.as_deref()) {
            Ok(results) => {
                let items: Vec<_> = results.iter().map(|r| json!({
                    "score": r.score,
                    "path": r.path,
                    "title": r.title,
                    "heading": r.heading,
                    "content": r.content,
                    "topic": r.topic,
                    "date": r.date,
                })).collect();
                json!({"results": items}).to_string()
            }
            Err(e) => json!({"error": format!("Search failed: {e}")}).to_string(),
        }
    }

    #[tool(description = "List all topics in the knowledge base with file counts and dates.")]
    fn list_topics(&self) -> String {
        let db = self.db.lock().unwrap();
        match db.list_topics() {
            Ok(topics) => {
                let items: Vec<_> = topics.iter().map(|t| json!({
                    "category": t.category,
                    "topic": t.topic,
                    "file_count": t.file_count,
                    "last_updated": t.last_updated,
                    "titles": t.titles,
                })).collect();
                json!({"topics": items}).to_string()
            }
            Err(e) => json!({"error": format!("{e}")}).to_string(),
        }
    }

    #[tool(description = "Get the full content of a specific document by its path relative to knowledge-base/.")]
    fn get_document(
        &self,
        #[param(description = "Relative path, e.g. deep-dive/chromadb/overview.md")] path: String,
    ) -> String {
        let full_path = self.kb_path.join(&path);
        match std::fs::read_to_string(&full_path) {
            Ok(content) => {
                let doc = crate::markdown::parse_document(&content);
                json!({
                    "content": content,
                    "title": doc.frontmatter.title,
                    "date": doc.frontmatter.date,
                    "tags": doc.frontmatter.tags,
                    "topic": doc.frontmatter.topic,
                }).to_string()
            }
            Err(e) => json!({"error": format!("File not found: {path} ({e})")}).to_string(),
        }
    }

    #[tool(description = "Get a specific section of the best practices document (PERFECT.md). Omit category to get table of contents.")]
    fn get_best_practice(
        &self,
        #[param(description = "Target name, e.g. claude-code")] target: String,
        #[param(description = "Category like hooks, skills, plugins. Omit for TOC.")] category: Option<String>,
    ) -> String {
        let perfect_path = self.kb_path.join(format!("best-practices/{target}/PERFECT.md"));
        let content = match std::fs::read_to_string(&perfect_path) {
            Ok(c) => c,
            Err(_) => return json!({"error": format!("No PERFECT.md for target: {target}")}).to_string(),
        };

        let doc = crate::markdown::parse_document(&content);

        // Extract available categories (h2 headings)
        let categories: Vec<String> = content.lines()
            .filter(|l| l.starts_with("## ") && !l.starts_with("## 目次") && !l.starts_with("## 付録"))
            .map(|l| l.trim_start_matches("## ").to_string())
            .collect();

        let version = content.lines()
            .find(|l| l.contains("対象バージョン"))
            .map(|l| l.to_string())
            .unwrap_or_default();

        let last_updated = doc.frontmatter.date.clone().unwrap_or_default();

        if let Some(cat) = &category {
            // Find matching section
            let cat_lower = cat.to_lowercase();
            let section = extract_section(&content, &cat_lower);
            json!({
                "content": section.unwrap_or_else(|| format!("Category '{cat}' not found")),
                "version": version,
                "last_updated": last_updated,
                "categories": categories,
            }).to_string()
        } else {
            // Return TOC + overview
            let toc = content.lines()
                .skip_while(|l| !l.starts_with("## 目次"))
                .take_while(|l| !l.starts_with("## 前提"))
                .collect::<Vec<_>>()
                .join("\n");
            json!({
                "content": if toc.is_empty() { "No TOC found".to_string() } else { toc },
                "version": version,
                "last_updated": last_updated,
                "categories": categories,
            }).to_string()
        }
    }

    #[tool(description = "Rebuild the search index. Run after adding new documents to knowledge-base/.")]
    fn rebuild_index(
        &self,
        #[param(description = "Force full rebuild ignoring content hashes")] force: Option<bool>,
    ) -> String {
        let force = force.unwrap_or(false);
        let db = self.db.lock().unwrap();
        match indexer::rebuild_index(&db, &self.embedder, &self.kb_path, force) {
            Ok(result) => json!({
                "total_documents": result.total_documents,
                "updated": result.updated,
                "deleted": result.deleted,
                "total_chunks": result.total_chunks,
                "duration_ms": result.duration_ms,
            }).to_string(),
            Err(e) => json!({"error": format!("{e}")}).to_string(),
        }
    }
}

fn extract_section(content: &str, category_lower: &str) -> Option<String> {
    let lines: Vec<&str> = content.lines().collect();
    let mut start = None;
    let mut end = None;

    for (i, line) in lines.iter().enumerate() {
        if line.starts_with("## ") {
            let heading = line.trim_start_matches("## ").to_lowercase();
            if heading.contains(category_lower) {
                start = Some(i);
            } else if start.is_some() && end.is_none() {
                end = Some(i);
            }
        }
    }

    start.map(|s| {
        let e = end.unwrap_or(lines.len());
        lines[s..e].join("\n")
    })
}

pub async fn run_server(kb_path: &str) -> Result<()> {
    eprintln!("kb-mcp: Loading embedding model...");
    let server = KbServer::new(kb_path)?;
    eprintln!("kb-mcp: MCP server ready (stdio)");
    server.run_stdio_server().await?;
    Ok(())
}
```

- [ ] **Step 2: main.rs の serve を接続**

`kb-mcp/src/main.rs` の `Commands::Serve` ハンドラを:
```rust
Commands::Serve { kb_path } => {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(server::run_server(&kb_path))?;
}
```

`main.rs` 先頭に `mod server;` を追加。

- [ ] **Step 3: ビルド確認**

```bash
cd kb-mcp && cargo build
```
Expected: コンパイル成功

注意: `rmcp` の `#[tool_router]` / `#[tool]` マクロの正確なシグネチャが API バージョンで変わる可能性がある。コンパイルエラーが出たら `docs.rs/rmcp` を参照して修正する。

- [ ] **Step 4: serve を試行（手動テスト）**

```bash
cargo run -- serve --kb-path ../knowledge-base
```
Expected: `kb-mcp: Loading embedding model...` → `kb-mcp: MCP server ready (stdio)` と表示され、stdin を待機する

`Ctrl+C` で終了。

- [ ] **Step 5: コミット**

```bash
git add kb-mcp/src/server.rs kb-mcp/src/main.rs
git commit -m "feat(kb-mcp): MCP server with 5 tools — search, list_topics, get_document, get_best_practice, rebuild_index"
```

---

### Task 7: 統合テスト + 実データ検証

**Files:**
- Create: `kb-mcp/tests/integration_test.rs`

- [ ] **Step 1: fixture ベースの統合テスト**

`kb-mcp/tests/integration_test.rs`:
```rust
use std::path::Path;

// These tests require the embedding model to be downloaded.
// Run with: cargo test --test integration_test -- --ignored

#[test]
#[ignore]
fn test_full_index_and_search_pipeline() {
    let fixtures_path = Path::new("tests/fixtures");
    let db_path = "/tmp/kb-mcp-test.db";

    // Clean up
    let _ = std::fs::remove_file(db_path);

    // This test validates the full pipeline:
    // 1. Parse markdown fixtures
    // 2. Generate embeddings
    // 3. Store in sqlite-vec
    // 4. Search and get results
    // Actual implementation depends on the public API surface.
    // For now, this is a placeholder that will be filled once
    // Task 6 compiles and the API is stable.

    let _ = std::fs::remove_file(db_path);
}
```

- [ ] **Step 2: 実データでエンドツーエンドテスト**

```bash
# 1. インデックス構築
cd kb-mcp && cargo run -- index --kb-path ../knowledge-base

# 2. ステータス確認
cargo run -- status --kb-path ../knowledge-base

# 3. serve 起動して MCP リクエストを手動送信
# (外部プロジェクトから .mcp.json で接続して確認)
```

- [ ] **Step 3: .mcp.json のサンプルを README に記載**

`kb-mcp/README.md`:
```markdown
# kb-mcp

knowledge-base/ を外部からセマンティック検索できる MCP サーバー。

## ビルド

\```bash
cd kb-mcp && cargo build --release
\```

## 使い方

\```bash
# インデックス構築（初回 + 更新時）
kb-mcp index --kb-path /path/to/knowledge-base

# MCP サーバー起動
kb-mcp serve --kb-path /path/to/knowledge-base

# ステータス確認
kb-mcp status --kb-path /path/to/knowledge-base
\```

## 外部プロジェクトからの接続

`.mcp.json`:
\```json
{
  "mcpServers": {
    "ai-knowledge": {
      "command": "/path/to/kb-mcp",
      "args": ["serve", "--kb-path", "/path/to/knowledge-base"],
      "type": "stdio"
    }
  }
}
\```

## 提供ツール

| ツール | 説明 |
|---|---|
| `search` | セマンティック検索 |
| `list_topics` | トピック一覧 |
| `get_document` | ドキュメント全文取得 |
| `get_best_practice` | PERFECT.md セクション取得 |
| `rebuild_index` | インデックス再構築 |
```

- [ ] **Step 4: コミット**

```bash
git add kb-mcp/tests/integration_test.rs kb-mcp/README.md
git commit -m "docs(kb-mcp): README with usage instructions and MCP connection example"
```

---

### Task 8: リリースビルド + 動作確認

**Files:** なし（ビルド + 手動テスト）

- [ ] **Step 1: リリースビルド**

```bash
cd kb-mcp && cargo build --release
ls -la target/release/kb-mcp*
```
Expected: `kb-mcp.exe`（Windows）が生成される。サイズを確認。

- [ ] **Step 2: リリースバイナリでインデックス + serve テスト**

```bash
./target/release/kb-mcp index --kb-path ../knowledge-base
./target/release/kb-mcp status --kb-path ../knowledge-base
```
Expected: debug ビルドより高速にインデックス構築

- [ ] **Step 3: 外部プロジェクトから MCP 接続テスト**

別プロジェクトの `.mcp.json` に追加して Claude Code セッションで `search` ツールを呼んでみる。

- [ ] **Step 4: 最終コミット**

```bash
git add -A && git commit -m "feat(kb-mcp): complete MCP server for knowledge base semantic search"
```
