//! Parent document retriever (display-time content expansion).
//!
//! After relevance is finalized (RRF / reranker / MMR), this stage rewrites
//! `SearchHit.content` to include surrounding context. Two strategies:
//!
//! - small chunk (`token_count < whole_doc_threshold_tokens`) → whole-document fallback
//! - otherwise → adjacent merge `[N-1, N, N+1]` bounded by document edges
//!
//! Invariants:
//! - `SearchHit.score` is NOT modified (relevance reflects original chunk)
//! - `quality_filter` is NOT applied (low-score neighbors are kept as context)
//! - NULL `chunks.level` (legacy DBs) → adjacent merge fallback (Task 3.4)

use crate::db::{Database, ExpandedRange, SearchHit};

/// Parent retriever 設定。kb-mcp.toml `[search.parent_retriever]` と
/// 1:1 対応する。Task 3.5 で `ParentRetrieverConfig` を加えた後はこの構造体を
/// 該当 config から構築する。
#[derive(Debug, Clone, Copy)]
pub struct ParentRetrieverParams {
    pub whole_doc_threshold_tokens: u32,
    pub max_expanded_tokens: u32,
}

/// 1 hit の `content` を P4 戦略で拡張する。`chunk_id` を起点に DB から
/// 前後 chunk (or 同 doc 全 chunk) を引き、連結後の `SearchHit` を返す。
///
/// 入力 `hit` は MMR / reranker 後の relevance 順位を持つもの。`score` /
/// `match_spans` には触らない (match_spans は呼び出し側で再計算する)。
pub fn expand_parent(
    hit: SearchHit,
    chunk_id: i64,
    db: &Database,
    params: ParentRetrieverParams,
) -> anyhow::Result<SearchHit> {
    // Task 3.2 では adjacent merge のみ。Task 3.3 で whole-doc fallback を分岐
    // 追加。
    let (doc_id, chunk_idx, _token_count) = db.get_chunk_meta(chunk_id)?;
    expand_adjacent(hit, doc_id, chunk_idx, db, params)
}

fn expand_adjacent(
    mut hit: SearchHit,
    doc_id: i64,
    chunk_idx: i64,
    db: &Database,
    _params: ParentRetrieverParams,
) -> anyhow::Result<SearchHit> {
    let neighbors = db.fetch_chunks_by_index_range(doc_id, chunk_idx - 1, chunk_idx + 1)?;
    if neighbors.is_empty() {
        return Ok(hit);
    }
    let mut sorted = neighbors;
    sorted.sort_by_key(|c| c.chunk_index);
    let from_idx = sorted
        .iter()
        .map(|c| c.chunk_index)
        .min()
        .unwrap_or(chunk_idx) as usize;
    let to_idx = sorted
        .iter()
        .map(|c| c.chunk_index)
        .max()
        .unwrap_or(chunk_idx) as usize;
    let merged: String = sorted
        .iter()
        .map(|c| c.content.clone())
        .collect::<Vec<_>>()
        .join("\n\n");
    hit.content = merged;
    // content が拡張されたので元 chunk 基準の byte offset で計算済み
    // match_spans は invalidate する。呼び出し側 (run_search_pipeline) が
    // 拡張後 content に対して compute_match_spans を再計算する責務だが、
    // ここで defensive に None クリアしておけば「再計算忘れ」で stale offset
    // が leak することを防げる。
    hit.match_spans = None;
    hit.expanded_from = Some(ExpandedRange::Adjacent {
        from_index: from_idx,
        to_index: to_idx,
    });
    Ok(hit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    // tempdir helper: db.rs の test mod の TempPath パターンを踏襲。
    // (db.rs と parent.rs は同 crate 内、再利用するか自前定義するか
    // 状況に応じて、ここでは小型 helper 自前定義)
    struct TempPath(std::path::PathBuf);
    impl Drop for TempPath {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn tempdir_for_test() -> TempPath {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("kb-mcp-parent-test-{pid}-{nanos}"));
        std::fs::create_dir_all(&p).unwrap();
        TempPath(p)
    }

    fn dummy_emb_384() -> Vec<f32> {
        vec![0.1_f32; 384]
    }

    fn make_hit(path: &str, content: &str) -> SearchHit {
        SearchHit {
            score: 1.0,
            path: path.into(),
            title: None,
            heading: None,
            topic: None,
            date: None,
            tags: vec![],
            content: content.into(),
            match_spans: None,
            expanded_from: None,
        }
    }

    fn params() -> ParentRetrieverParams {
        ParentRetrieverParams {
            whole_doc_threshold_tokens: 100,
            max_expanded_tokens: 2000,
        }
    }

    /// 3 chunks ([0, 1, 2]) を同 doc に insert、中間の chunk_id を hit にして
    /// adjacent merge が前後を含めて 3 chunks を連結することを確認。
    #[test]
    fn test_parent_adjacent_merge_3_chunks() {
        let tmp = tempdir_for_test();
        let path = tmp.0.join("test.db");
        let db = Database::open(path.to_str().unwrap()).expect("open");
        db.verify_embedding_meta("bge-small-en-v1.5", 384)
            .expect("vec_chunks");
        let doc_id = db
            .upsert_document("/doc.md", Some("d"), Some("t"), None, None, &[], None, "h")
            .expect("upsert");
        let c0 = db
            .insert_chunk(
                doc_id,
                0,
                Some("h0"),
                None,
                "alpha body content",
                &dummy_emb_384(),
                1.0,
            )
            .expect("c0");
        let c1 = db
            .insert_chunk(
                doc_id,
                1,
                Some("h1"),
                None,
                "beta body content",
                &dummy_emb_384(),
                1.0,
            )
            .expect("c1");
        let _c2 = db
            .insert_chunk(
                doc_id,
                2,
                Some("h2"),
                None,
                "gamma body content",
                &dummy_emb_384(),
                1.0,
            )
            .expect("c2");

        let hit = make_hit("/doc.md", "beta body content");
        let expanded = expand_parent(hit, c1, &db, params()).expect("expand");
        assert!(expanded.content.contains("alpha"));
        assert!(expanded.content.contains("beta"));
        assert!(expanded.content.contains("gamma"));
        match expanded.expanded_from {
            Some(ExpandedRange::Adjacent {
                from_index: 0,
                to_index: 2,
            }) => {}
            other => panic!("expected Adjacent {{0,2}}, got {other:?}"),
        }
        // unused, drops c0 ref clean
        let _ = c0;
    }

    /// chunk_index = 0 (doc の左端) で hit、左拡張なしで [0, 1] のみ返ることを確認。
    #[test]
    fn test_parent_adjacent_at_doc_boundary_left() {
        let tmp = tempdir_for_test();
        let path = tmp.0.join("test.db");
        let db = Database::open(path.to_str().unwrap()).expect("open");
        db.verify_embedding_meta("bge-small-en-v1.5", 384)
            .expect("vec_chunks");
        let doc_id = db
            .upsert_document("/doc.md", Some("d"), Some("t"), None, None, &[], None, "h")
            .expect("upsert");
        let c0 = db
            .insert_chunk(
                doc_id,
                0,
                Some("h0"),
                None,
                "alpha body content",
                &dummy_emb_384(),
                1.0,
            )
            .expect("c0");
        let _c1 = db
            .insert_chunk(
                doc_id,
                1,
                Some("h1"),
                None,
                "beta body content",
                &dummy_emb_384(),
                1.0,
            )
            .expect("c1");

        let hit = make_hit("/doc.md", "alpha body content");
        let expanded = expand_parent(hit, c0, &db, params()).expect("expand");
        match expanded.expanded_from {
            Some(ExpandedRange::Adjacent {
                from_index: 0,
                to_index: 1,
            }) => {}
            other => panic!("expected Adjacent {{0,1}}, got {other:?}"),
        }
    }
}
