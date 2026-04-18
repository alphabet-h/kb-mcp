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

/// Per-file metadata collected before the main embed loop. `content` を持たず
/// 追加 I/O を増やす代わりに、大規模 KB でもピークメモリを一定に抑える。
#[derive(Debug, Clone)]
struct DiskEntry {
    /// kb_path 相対 (forward-slash) の保存キー。
    rel: String,
    /// SHA-256 hex。DB 側 `content_hash` と比較する。
    hash: String,
    /// 実ファイルの絶対パス。embed/upsert 段階で再 read_to_string する。
    full: std::path::PathBuf,
}

/// [feature 11] disk と DB の (path, hash) から「移動ペア」を決定する純粋関数。
///
/// - 「DB にあるが disk にない」path は「消えた」候補
/// - 「disk にあるが DB にない」path は「新規出現」候補
/// - 両者で hash が一致すればペア確定
///
/// 重複 hash がある場合も結果が deterministic になるよう、双方を path で
/// ソートしてから first-match マッチングを行う (evaluator 指摘 Med #4)。
fn detect_renames(
    disk_entries: &[DiskEntry],
    db_path_hashes: &std::collections::HashMap<String, String>,
) -> Vec<(String, String)> {
    let disk_paths: HashSet<&str> =
        disk_entries.iter().map(|e| e.rel.as_str()).collect();

    // DB ∖ disk, path で sort
    let mut orphan_in_db: Vec<(&String, &String)> = db_path_hashes
        .iter()
        .filter(|(p, _)| !disk_paths.contains(p.as_str()))
        .collect();
    orphan_in_db.sort_by_key(|(p, _)| *p);

    // disk ∖ DB, path で sort (DiskEntry は元々 walkdir の sort 順だが
    // 念のため明示的に安定化)
    let mut new_on_disk: Vec<&DiskEntry> = disk_entries
        .iter()
        .filter(|e| !db_path_hashes.contains_key(&e.rel))
        .collect();
    new_on_disk.sort_by(|a, b| a.rel.cmp(&b.rel));

    let mut consumed: HashSet<&str> = HashSet::new();
    let mut pairs: Vec<(String, String)> = Vec::new();
    for (old_path, old_hash) in &orphan_in_db {
        let mut chosen: Option<&str> = None;
        for e in &new_on_disk {
            if consumed.contains(e.rel.as_str()) {
                continue;
            }
            if &e.hash == *old_hash {
                chosen = Some(e.rel.as_str());
                break;
            }
        }
        if let Some(new_rel) = chosen {
            consumed.insert(new_rel);
            pairs.push(((*old_path).clone(), new_rel.to_string()));
        }
    }
    pairs
}

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

    // [feature 11] ファイル移動検出の前段階として、disk 側の全ファイルの
    // **hash だけ** を先に計算する。content は持ち回らない (evaluator 指摘
    // High #1: 大規模 KB の memory regression 回避)。embed/upsert 段階で
    // もう一度 read_to_string する — ファイル OS キャッシュで 2 度目の
    // read は十分安く、代わりにピークメモリを `filecount * avg_size` から
    // `filecount * avg_path_len + 1 file worth of content` に圧縮できる。
    let disk_entries: Vec<DiskEntry> = md_files
        .iter()
        .map(|p| -> Result<DiskEntry> {
            let content = std::fs::read_to_string(p)
                .with_context(|| format!("failed to read {}", p.display()))?;
            let rel = p
                .strip_prefix(&kb_path)
                .unwrap_or(p)
                .to_string_lossy()
                .replace('\\', "/");
            let hash = sha256_hex(&content);
            Ok(DiskEntry {
                rel,
                hash,
                full: p.clone(),
            })
        })
        .collect::<Result<Vec<_>>>()?;

    // [feature 11] rename 検出 + atomically な rename 適用。
    // force=true のときは skip (embedding 全件再計算の意図)。
    let renamed: u32 = if force {
        0
    } else {
        let db_path_hashes = db.all_path_hashes()?;
        let pairs = detect_renames(&disk_entries, &db_path_hashes);
        // evaluator 指摘 High #2: rename フェーズ全体を単一 transaction に
        // 包んで部分 rename 残留を防ぐ。pairs が空なら no-op。
        db.rename_documents_atomic(&pairs)?;
        for (old_path, new_path) in &pairs {
            eprintln!("  renamed: {old_path} -> {new_path}");
        }
        pairs.len() as u32
    };

    // Track paths we visit so we can detect deletions later.
    let mut visited_paths: HashSet<String> = HashSet::new();
    let mut updated: u32 = 0;

    // 2. Process each file
    for entry in &disk_entries {
        visited_paths.insert(entry.rel.clone());

        // 2a. Skip unchanged files unless forced.
        // rename で path UPDATE 済のものは「DB 側 hash == disk hash」なので
        // この経路で自然に skip される (embedding 再計算なし)。
        if !force
            && let Some(existing_hash) = db.get_document_hash(&entry.rel)?
            && existing_hash == entry.hash
        {
            continue;
        }

        // 2b. Read + parse markdown only for files we actually need to embed.
        let content = std::fs::read_to_string(&entry.full)
            .with_context(|| format!("failed to read {}", entry.full.display()))?;
        let parsed = match exclude_headings {
            Some(list) => markdown::parse_with_excludes(&content, list),
            None => markdown::parse(&content),
        };

        // Skip files with no embeddable chunks
        if parsed.chunks.is_empty() {
            continue;
        }

        // 2c. Extract category and topic from relative path
        let (category, topic) = extract_category_topic(&entry.rel);

        // 2d. Upsert document (deletes old chunks internally)
        let doc_id = db.upsert_document(
            &entry.rel,
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
            &entry.hash,
        )?;

        // 2e. Batch-embed all chunks
        let texts: Vec<&str> = parsed.chunks.iter().map(|c| c.content.as_str()).collect();
        let embeddings = embedder
            .embed_texts(&texts)
            .with_context(|| format!("failed to embed chunks for {}", entry.rel))?;

        // 2f. Insert each chunk with its embedding + quality score
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
        eprintln!("  indexed: {} ({} chunks)", entry.rel, parsed.chunks.len());
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

    use std::collections::HashMap;

    fn mk_entry(rel: &str, hash: &str) -> DiskEntry {
        DiskEntry {
            rel: rel.to_string(),
            hash: hash.to_string(),
            full: std::path::PathBuf::from(rel),
        }
    }

    #[test]
    fn test_detect_renames_single_move() {
        let disk = vec![mk_entry("new/x.md", "h1"), mk_entry("keep.md", "h2")];
        let mut db = HashMap::new();
        db.insert("old/x.md".to_string(), "h1".to_string());
        db.insert("keep.md".to_string(), "h2".to_string());
        let pairs = detect_renames(&disk, &db);
        assert_eq!(pairs, vec![("old/x.md".to_string(), "new/x.md".to_string())]);
    }

    #[test]
    fn test_detect_renames_no_rename_when_new_path_exists() {
        // new path が既に DB にある = 別文書なので rename ペアにしない
        let disk = vec![mk_entry("b.md", "h1")];
        let mut db = HashMap::new();
        db.insert("a.md".to_string(), "h1".to_string());
        db.insert("b.md".to_string(), "h1".to_string());
        let pairs = detect_renames(&disk, &db);
        // disk には a.md が無いので a.md は DB orphan、b.md は既に DB にある
        // → 新規 disk path が無いのでペア無し
        assert!(pairs.is_empty());
    }

    #[test]
    fn test_detect_renames_no_change_same_path_same_hash() {
        let disk = vec![mk_entry("a.md", "h1")];
        let mut db = HashMap::new();
        db.insert("a.md".to_string(), "h1".to_string());
        let pairs = detect_renames(&disk, &db);
        assert!(pairs.is_empty());
    }

    #[test]
    fn test_detect_renames_deterministic_with_duplicate_hashes() {
        // A, B とも空ファイル (同 hash) で DB、disk 側も C, D の新 path
        // どちらに振っても意味論的には同じだが結果は deterministic であるべき
        let disk = vec![mk_entry("C.md", "hempty"), mk_entry("D.md", "hempty")];
        let mut db = HashMap::new();
        db.insert("A.md".to_string(), "hempty".to_string());
        db.insert("B.md".to_string(), "hempty".to_string());
        let pairs1 = detect_renames(&disk, &db);
        // 2 回目も同じ結果になること (HashMap iteration 順に依存しない)
        let pairs2 = detect_renames(&disk, &db);
        assert_eq!(pairs1, pairs2);
        // path 順の sort により A→C, B→D になるはず
        assert_eq!(
            pairs1,
            vec![
                ("A.md".to_string(), "C.md".to_string()),
                ("B.md".to_string(), "D.md".to_string()),
            ]
        );
    }

    #[test]
    fn test_detect_renames_unmatched_hashes_are_dropped() {
        let disk = vec![mk_entry("new.md", "h_new")];
        let mut db = HashMap::new();
        db.insert("old.md".to_string(), "h_old".to_string()); // 別 hash
        let pairs = detect_renames(&disk, &db);
        // hash 不一致なのでペアにしない (old.md は削除対象、new.md は新規追加)
        assert!(pairs.is_empty());
    }

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
