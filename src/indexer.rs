use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::Path;
use std::time::Instant;
use walkdir::WalkDir;

use crate::db::Database;
use crate::embedder::Embedder;
use crate::{markdown, quality};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Summary returned by [`rebuild_index`].
pub struct IndexResult {
    pub total_documents: u32,
    pub updated: u32,
    /// [feature 11] File-rename を検出した件数。embedding は再計算されず
    /// `documents.path` だけが UPDATE された数。
    pub renamed: u32,
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

    // pre-feature-13 DB (quality_score = 1.0 のまま) を一度だけ再評価する。
    // 既にスコアが入っているチャンクは触らないため冪等。
    let quality_updated = db.backfill_quality()?;
    if quality_updated > 0 {
        eprintln!("Backfilled {quality_updated} chunks with quality scores");
    }

    // 1. Collect all .md files, skipping .obsidian/
    let md_files = collect_md_files(&kb_path)?;
    eprintln!("Found {} markdown files", md_files.len());

    // [feature 11] ファイル移動検出のため、先に
    //  (a) disk 側の各ファイルの hash
    //  (b) DB 側の全 (path, hash) ペア
    // を取って「DB には古い path、disk には同 hash の新 path」ペアを探す。
    //
    // force=true (--force) 指定時は embedding を全件再計算する意図なので、
    // rename 検出自体をスキップする (重複 path UPDATE を避ける)。
    let disk_entries: Vec<(String /* rel */, String /* hash */, String /* content */, std::path::PathBuf)> = md_files
        .iter()
        .map(|p| -> Result<_> {
            let content = std::fs::read_to_string(p)
                .with_context(|| format!("failed to read {}", p.display()))?;
            let rel = p
                .strip_prefix(&kb_path)
                .unwrap_or(p)
                .to_string_lossy()
                .replace('\\', "/");
            let hash = sha256_hex(&content);
            Ok((rel, hash, content, p.clone()))
        })
        .collect::<Result<Vec<_>>>()?;

    let mut renamed: u32 = 0;
    let mut renamed_paths: HashSet<String> = HashSet::new(); // new path (rename 済)
    if !force {
        let db_path_hashes = db.all_path_hashes()?;
        let disk_paths: HashSet<&str> =
            disk_entries.iter().map(|(r, ..)| r.as_str()).collect();

        // DB にあるが disk から消えた = 移動 or 削除の候補
        // 複数同 hash があった場合は「先頭に見つけた disk new-path」を採用 (安定順)
        let orphan_in_db: Vec<(String, String)> = db_path_hashes
            .iter()
            .filter(|(p, _)| !disk_paths.contains(p.as_str()))
            .map(|(p, h)| (p.clone(), h.clone()))
            .collect();

        for (old_path, old_hash) in &orphan_in_db {
            // disk 側で (hash 一致 & DB にまだ同 path が無い & 他の rename に
            // 使っていない) 新 path を探す
            let mut chosen: Option<String> = None;
            for (new_rel, new_hash, _, _) in &disk_entries {
                if new_hash != old_hash {
                    continue;
                }
                if db_path_hashes.contains_key(new_rel) {
                    continue; // その path は既に DB にある = 別文書
                }
                if renamed_paths.contains(new_rel) {
                    continue;
                }
                chosen = Some(new_rel.clone());
                break;
            }
            if let Some(new_path) = chosen {
                db.rename_document(old_path, &new_path)?;
                renamed_paths.insert(new_path.clone());
                renamed += 1;
                eprintln!("  renamed: {old_path} -> {new_path}");
            }
        }
    }

    // Track paths we visit so we can detect deletions later.
    let mut visited_paths: HashSet<String> = HashSet::new();
    let mut updated: u32 = 0;

    // 2. Process each file
    for (rel_path, hash, content, _file_path) in &disk_entries {
        visited_paths.insert(rel_path.clone());

        // 2b. Skip unchanged files unless forced.
        // rename で path UPDATE 済のものは「DB 側 hash == disk hash」なので
        // この経路で自然に skip される (embedding 再計算なし)。
        if !force
            && let Some(existing_hash) = db.get_document_hash(rel_path)?
            && &existing_hash == hash
        {
            continue;
        }

        // 2c. Parse markdown (with optional heading exclude override)
        let parsed = match exclude_headings {
            Some(list) => markdown::parse_with_excludes(content, list),
            None => markdown::parse(content),
        };

        // Skip files with no embeddable chunks
        if parsed.chunks.is_empty() {
            continue;
        }

        // 2d. Extract category and topic from relative path
        let (category, topic) = extract_category_topic(rel_path);

        // 2e. Upsert document (deletes old chunks internally)
        let doc_id = db.upsert_document(
            rel_path,
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
            hash,
        )?;

        // 2f. Batch-embed all chunks
        let texts: Vec<&str> = parsed.chunks.iter().map(|c| c.content.as_str()).collect();
        let embeddings = embedder
            .embed_texts(&texts)
            .with_context(|| format!("failed to embed chunks for {rel_path}"))?;

        // 2g. Insert each chunk with its embedding + quality score
        for (chunk, embedding) in parsed.chunks.iter().zip(embeddings.iter()) {
            let score = quality::chunk_quality_score(
                chunk.heading.as_deref(),
                &chunk.content,
            );
            db.insert_chunk(
                doc_id,
                chunk.index as i32,
                chunk.heading.as_deref(),
                &chunk.content,
                embedding,
                score,
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
        renamed,
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
