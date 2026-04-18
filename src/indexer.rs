use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::Path;
use std::time::Instant;
use walkdir::WalkDir;

use crate::db::Database;
use crate::embedder::Embedder;
use crate::markdown;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Summary returned by [`rebuild_index`].
pub struct IndexResult {
    pub total_documents: u32,
    pub updated: u32,
    pub deleted: u32,
    pub total_chunks: u32,
    pub duration_ms: u64,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Walk `kb_path` recursively, parse Markdown files, embed chunks, and store
/// everything in the database.
///
/// If `force` is `false`, files whose SHA-256 content hash has not changed
/// since the last index run are skipped.
///
/// `exclude_headings`:
/// - `None` → use [`markdown::DEFAULT_EXCLUDED_HEADINGS`]
/// - `Some(list)` → completely overrides the default list (pass `&[]` to
///   disable heading-based exclusion entirely).
pub fn rebuild_index(
    db: &Database,
    embedder: &mut Embedder,
    kb_path: &Path,
    force: bool,
    exclude_headings: Option<&[String]>,
) -> Result<IndexResult> {
    let start = Instant::now();

    let kb_path = kb_path
        .canonicalize()
        .with_context(|| format!("failed to canonicalize kb_path: {}", kb_path.display()))?;

    // pre-feature-9 DB を引き継いだケースで FTS が空のままにならないよう、
    // まず既存 chunks のうち FTS 未登録のものを backfill する。
    let backfilled = db.backfill_fts()?;
    if backfilled > 0 {
        eprintln!("Backfilled {backfilled} chunks into FTS index");
    }

    // 1. Collect all .md files, skipping .obsidian/
    let md_files = collect_md_files(&kb_path)?;
    eprintln!("Found {} markdown files", md_files.len());

    // Track paths we visit so we can detect deletions later.
    let mut visited_paths: HashSet<String> = HashSet::new();
    let mut updated: u32 = 0;

    // 2. Process each file
    for file_path in &md_files {
        let content = std::fs::read_to_string(file_path)
            .with_context(|| format!("failed to read {}", file_path.display()))?;

        // Relative path with forward slashes (portable storage key).
        let rel_path = file_path
            .strip_prefix(&kb_path)
            .unwrap_or(file_path)
            .to_string_lossy()
            .replace('\\', "/");

        visited_paths.insert(rel_path.clone());

        // 2a. SHA-256 content hash
        let hash = sha256_hex(&content);

        // 2b. Skip unchanged files unless forced
        if !force
            && let Some(existing_hash) = db.get_document_hash(&rel_path)?
                && existing_hash == hash {
                    // Count existing chunks for the total
                    // (we don't re-embed, but they still exist in the DB)
                    continue;
                }

        // 2c. Parse markdown (with optional heading exclude override)
        let parsed = match exclude_headings {
            Some(list) => markdown::parse_with_excludes(&content, list),
            None => markdown::parse(&content),
        };

        // Skip files with no embeddable chunks
        if parsed.chunks.is_empty() {
            continue;
        }

        // 2d. Extract category and topic from relative path
        let (category, topic) = extract_category_topic(&rel_path);

        // 2e. Upsert document (deletes old chunks internally)
        let doc_id = db.upsert_document(
            &rel_path,
            parsed.frontmatter.title.as_deref(),
            // Prefer frontmatter topic, fall back to path-derived topic
            parsed
                .frontmatter
                .topic
                .as_deref()
                .or(topic.as_deref()),
            category.as_deref(),
            parsed.frontmatter.depth.as_deref(),
            &parsed.frontmatter.tags,
            parsed.frontmatter.date.as_deref(),
            &hash,
        )?;

        // 2f. Batch-embed all chunks
        let texts: Vec<&str> = parsed.chunks.iter().map(|c| c.content.as_str()).collect();
        let embeddings = embedder
            .embed_texts(&texts)
            .with_context(|| format!("failed to embed chunks for {rel_path}"))?;

        // 2g. Insert each chunk with its embedding
        for (chunk, embedding) in parsed.chunks.iter().zip(embeddings.iter()) {
            db.insert_chunk(
                doc_id,
                chunk.index as i32,
                chunk.heading.as_deref(),
                &chunk.content,
                embedding,
            )?;
        }

        updated += 1;
        eprintln!("  indexed: {} ({} chunks)", rel_path, parsed.chunks.len());
    }

    // 3. Delete documents in DB that no longer exist on disk
    let all_db_paths = db.all_document_paths()?;
    let mut deleted: u32 = 0;
    for db_path in &all_db_paths {
        if !visited_paths.contains(db_path) {
            db.delete_document(db_path)?;
            deleted += 1;
            eprintln!("  deleted: {}", db_path);
        }
    }

    // Count total documents remaining (includes unchanged ones)
    let total_documents = db.document_count()?;
    // Count total chunks in DB (includes unchanged ones)
    let total_chunks_in_db = db.chunk_count()?;

    let duration_ms = start.elapsed().as_millis() as u64;

    Ok(IndexResult {
        total_documents,
        updated,
        deleted,
        total_chunks: total_chunks_in_db,
        duration_ms,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Collect all `.md` files under `kb_path`, skipping `.obsidian/` directories.
fn collect_md_files(kb_path: &Path) -> Result<Vec<std::path::PathBuf>> {
    let mut files = Vec::new();

    for entry in WalkDir::new(kb_path)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            // Skip .obsidian/ directories
            let name = e.file_name().to_string_lossy();
            !(e.file_type().is_dir() && name == ".obsidian")
        })
    {
        let entry = entry.context("walkdir error")?;
        if entry.file_type().is_file()
            && let Some(ext) = entry.path().extension()
                && ext.eq_ignore_ascii_case("md") {
                    files.push(entry.into_path());
                }
    }

    // Sort for deterministic ordering
    files.sort();
    Ok(files)
}

/// Extract `(category, topic)` from a relative path.
///
/// ```text
/// "deep-dive/chromadb/overview.md" → (Some("deep-dive"), Some("chromadb"))
/// "ai-news/2026-04-16.md"         → (Some("ai-news"), None)
/// "index.md"                       → (None, None)
/// ```
fn extract_category_topic(rel_path: &str) -> (Option<String>, Option<String>) {
    let parts: Vec<&str> = rel_path.split('/').collect();
    match parts.len() {
        // "index.md" — no category, no topic
        0 | 1 => (None, None),
        // "ai-news/2026-04-16.md" — category only
        2 => (Some(parts[0].to_string()), None),
        // "deep-dive/chromadb/overview.md" or deeper — category + topic
        _ => (Some(parts[0].to_string()), Some(parts[1].to_string())),
    }
}

/// Compute the hex-encoded SHA-256 digest of a string.
fn sha256_hex(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_category_topic_deep_path() {
        let (cat, topic) = extract_category_topic("deep-dive/chromadb/overview.md");
        assert_eq!(cat.as_deref(), Some("deep-dive"));
        assert_eq!(topic.as_deref(), Some("chromadb"));
    }

    #[test]
    fn test_extract_category_topic_shallow_path() {
        let (cat, topic) = extract_category_topic("ai-news/2026-04-16.md");
        assert_eq!(cat.as_deref(), Some("ai-news"));
        assert_eq!(topic, None);
    }

    #[test]
    fn test_extract_category_topic_root_file() {
        let (cat, topic) = extract_category_topic("index.md");
        assert_eq!(cat, None);
        assert_eq!(topic, None);
    }

    #[test]
    fn test_extract_category_topic_very_deep_path() {
        let (cat, topic) =
            extract_category_topic("tech-watch/anthropic/subdir/2026-04-16.md");
        assert_eq!(cat.as_deref(), Some("tech-watch"));
        assert_eq!(topic.as_deref(), Some("anthropic"));
    }

    #[test]
    fn test_sha256_hex_deterministic() {
        let hash1 = sha256_hex("hello world");
        let hash2 = sha256_hex("hello world");
        assert_eq!(hash1, hash2);
        // Known SHA-256 of "hello world"
        assert_eq!(
            hash1,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn test_sha256_hex_different_content() {
        let hash1 = sha256_hex("hello");
        let hash2 = sha256_hex("world");
        assert_ne!(hash1, hash2);
    }
}
