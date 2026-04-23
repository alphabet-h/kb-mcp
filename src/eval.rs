//! `kb-mcp eval` — retrieval quality evaluation subcommand.
//!
//! Opt-in パワーユーザ向け機能。Golden query YAML を読み、`db::search_hybrid`
//! で検索し、recall@k / MRR / nDCG@k を計算する。直前実行との diff を表示する。

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

// ---------- Golden ----------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoldenSet {
    #[serde(default)]
    pub defaults: Option<GoldenDefaults>,
    pub queries: Vec<GoldenQuery>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoldenDefaults {
    pub limit: Option<u32>,
    pub rerank: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoldenQuery {
    pub id: Option<String>,
    pub query: String,
    pub expected: Vec<ExpectedHit>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ExpectedHit {
    pub path: String,
    #[serde(default)]
    pub heading: Option<String>,
}

impl GoldenSet {
    /// Golden YAML を読み込む。欠損時は hint 付きエラー。
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            anyhow::bail!(
                "no golden file at {} (hint: pass --golden or create <kb>/.kb-mcp-eval.yml)",
                path.display()
            );
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read golden file: {}", path.display()))?;
        let gs: Self = serde_yaml::from_str(&text)
            .with_context(|| format!("failed to parse golden file: {}", path.display()))?;
        Ok(gs)
    }

    /// Golden ファイルの生バイト列を sha256 ハッシュ化 (fingerprint 用)。
    pub fn hash_bytes(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(bytes);
        format!("{:x}", h.finalize())
    }
}

// ---------- Metrics ----------

/// Heading 比較用の正規化: 前後空白 trim + 小文字化。
fn normalize_heading(s: &str) -> String {
    s.trim().to_lowercase()
}

/// ヒット判定: path は完全一致、heading は指定があれば正規化後一致。
pub fn is_hit(expected: &ExpectedHit, hit: &HitRecord) -> bool {
    if expected.path != hit.path {
        return false;
    }
    match (&expected.heading, &hit.heading) {
        (Some(e), Some(h)) => normalize_heading(e) == normalize_heading(h),
        (Some(_), None) => false,
        (None, _) => true,
    }
}

/// recall@k = |expected ∩ top[..k]| / |expected|。
/// expected 0 件または top 0 件では 0.0。
pub fn recall_at_k(expected: &[ExpectedHit], top: &[HitRecord], k: usize) -> f64 {
    if expected.is_empty() || top.is_empty() {
        return 0.0;
    }
    let window = top.iter().take(k);
    let mut matched = 0usize;
    for e in expected {
        if window.clone().any(|h| is_hit(e, h)) {
            matched += 1;
        }
    }
    matched as f64 / expected.len() as f64
}

/// MRR 用: 最初にヒットした expected の rank の逆数。無ければ 0.0。
pub fn reciprocal_rank(expected: &[ExpectedHit], top: &[HitRecord]) -> f64 {
    if expected.is_empty() || top.is_empty() {
        return 0.0;
    }
    for h in top {
        if expected.iter().any(|e| is_hit(e, h)) {
            return 1.0 / h.rank as f64;
        }
    }
    0.0
}

/// nDCG@k (binary relevance)。
/// DCG = Σ_{rank ≤ k, hit} 1 / log2(rank + 1)
/// IDCG = Σ_{i=1..=min(|expected|, k)} 1 / log2(i + 1)
pub fn ndcg_at_k(expected: &[ExpectedHit], top: &[HitRecord], k: usize) -> f64 {
    if expected.is_empty() || top.is_empty() || k == 0 {
        return 0.0;
    }
    let window = top.iter().take(k);
    let dcg: f64 = window
        .clone()
        .filter(|h| expected.iter().any(|e| is_hit(e, h)))
        .map(|h| 1.0 / ((h.rank as f64 + 1.0).log2()))
        .sum();
    let ideal_count = expected.len().min(k);
    let idcg: f64 = (1..=ideal_count)
        .map(|i| 1.0 / ((i as f64 + 1.0).log2()))
        .sum();
    if idcg == 0.0 {
        0.0
    } else {
        dcg / idcg
    }
}

/// クエリ単位で recall@k / RR / nDCG@k をまとめて計算する。
pub fn compute_query_metrics(
    expected: &[ExpectedHit],
    top: &[HitRecord],
    k_values: &[usize],
) -> QueryMetrics {
    let mut recall_at_k_map = std::collections::BTreeMap::new();
    let mut ndcg_at_k_map = std::collections::BTreeMap::new();
    for &k in k_values {
        recall_at_k_map.insert(k, recall_at_k(expected, top, k));
        ndcg_at_k_map.insert(k, ndcg_at_k(expected, top, k));
    }
    QueryMetrics {
        recall_at_k: recall_at_k_map,
        reciprocal_rank: reciprocal_rank(expected, top),
        ndcg_at_k: ndcg_at_k_map,
    }
}

/// 全クエリにわたる平均を取る。expected 0 件のクエリはスキップする。
pub fn aggregate_metrics(per_query: &[QueryResult], k_values: &[usize]) -> AggregateMetrics {
    let valid: Vec<&QueryResult> = per_query
        .iter()
        .filter(|q| !q.expected.is_empty())
        .collect();
    let n = valid.len();
    if n == 0 {
        return AggregateMetrics::default();
    }
    let mut recall_at_k_map = std::collections::BTreeMap::new();
    let mut ndcg_at_k_map = std::collections::BTreeMap::new();
    for &k in k_values {
        let sum_r: f64 = valid
            .iter()
            .map(|q| q.metrics.recall_at_k.get(&k).copied().unwrap_or(0.0))
            .sum();
        let sum_n: f64 = valid
            .iter()
            .map(|q| q.metrics.ndcg_at_k.get(&k).copied().unwrap_or(0.0))
            .sum();
        recall_at_k_map.insert(k, sum_r / n as f64);
        ndcg_at_k_map.insert(k, sum_n / n as f64);
    }
    let mrr: f64 =
        valid.iter().map(|q| q.metrics.reciprocal_rank).sum::<f64>() / n as f64;
    AggregateMetrics {
        recall_at_k: recall_at_k_map,
        mrr,
        ndcg_at_k: ndcg_at_k_map,
        query_count: n,
    }
}

// ---------- Result ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalRun {
    pub timestamp: DateTime<Utc>,
    pub fingerprint: ConfigFingerprint,
    pub per_query: Vec<QueryResult>,
    pub aggregate: AggregateMetrics,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConfigFingerprint {
    pub model: String,
    pub reranker: Option<String>,
    pub limit: u32,
    pub k_values: Vec<usize>,
    pub golden_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryResult {
    pub id: String,
    pub query: String,
    pub expected: Vec<ExpectedHit>,
    pub top_k: Vec<HitRecord>,
    pub metrics: QueryMetrics,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HitRecord {
    pub rank: usize,
    pub path: String,
    pub heading: Option<String>,
    pub score: f32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QueryMetrics {
    /// k -> recall
    pub recall_at_k: std::collections::BTreeMap<usize, f64>,
    pub reciprocal_rank: f64,
    /// k -> nDCG
    pub ndcg_at_k: std::collections::BTreeMap<usize, f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AggregateMetrics {
    pub recall_at_k: std::collections::BTreeMap<usize, f64>,
    pub mrr: f64,
    pub ndcg_at_k: std::collections::BTreeMap<usize, f64>,
    pub query_count: usize,
}

// ---------- History ----------

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct History {
    pub runs: VecDeque<EvalRun>,
}

impl History {
    /// JSON ファイルから履歴を読む。不在・破損時は warn を出して空 History を返す。
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("failed to read eval history {}: {}", path.display(), e);
                return Ok(Self::default());
            }
        };
        match serde_json::from_slice::<Self>(&bytes) {
            Ok(h) => Ok(h),
            Err(e) => {
                tracing::warn!("eval history corrupted ({}), starting fresh", e);
                Ok(Self::default())
            }
        }
    }

    /// 最新の run を front に積み、`size` 件を超えたら末尾を切り落とす。
    pub fn push_front(&mut self, run: EvalRun, size: usize) {
        self.runs.push_front(run);
        while self.runs.len() > size {
            self.runs.pop_back();
        }
    }

    /// 直前の run (= front) を取得する。
    #[allow(dead_code)]
    pub fn previous(&self) -> Option<&EvalRun> {
        self.runs.front()
    }

    /// atomic rename で書き出す。
    pub fn save(&self, path: &Path) -> Result<()> {
        let bytes =
            serde_json::to_vec_pretty(self).context("failed to serialize eval history")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &bytes)
            .with_context(|| format!("failed to write temp history: {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| {
            format!(
                "failed to rename temp history into place: {}",
                path.display()
            )
        })?;
        Ok(())
    }
}

// ---------- Options ----------

pub struct RunOpts {
    pub kb_path: PathBuf,
    pub golden_path: PathBuf,
    pub model_choice: crate::embedder::ModelChoice,
    pub reranker_choice: crate::embedder::RerankerChoice,
    pub k_values: Vec<usize>,
    pub limit: u32,
    pub write_history: bool,
    pub history_size: usize,
    pub regression_threshold: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_types_compile() {
        // 型が互いに整合していることの最小確認。後続 Task でテストを足していく。
        let _ = ExpectedHit {
            path: "x".into(),
            heading: None,
        };
    }

    fn write_yaml(name: &str, content: &str) -> PathBuf {
        let pid = std::process::id();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!("{name}-{pid}-{nonce}.yml"));
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn test_golden_minimal_parse() {
        let path = write_yaml(
            "eval-golden-min",
            "queries:\n- query: \"hello\"\n  expected:\n  - path: \"a.md\"\n",
        );
        let gs = GoldenSet::load(&path).unwrap();
        assert_eq!(gs.queries.len(), 1);
        assert_eq!(gs.queries[0].query, "hello");
        assert_eq!(gs.queries[0].expected[0].path, "a.md");
        assert!(gs.queries[0].expected[0].heading.is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_golden_with_heading_and_id_and_tags() {
        let path = write_yaml(
            "eval-golden-full",
            "defaults:\n  limit: 5\n  rerank: true\nqueries:\n- id: \"q1\"\n  query: \"RRF の k\"\n  expected:\n  - path: \"docs/arch.md\"\n    heading: \"Data flow\"\n  - path: \"src/db.rs\"\n  tags: [\"retrieval\"]\n",
        );
        let gs = GoldenSet::load(&path).unwrap();
        let d = gs.defaults.as_ref().unwrap();
        assert_eq!(d.limit, Some(5));
        assert_eq!(d.rerank, Some(true));
        let q = &gs.queries[0];
        assert_eq!(q.id.as_deref(), Some("q1"));
        assert_eq!(q.expected[0].heading.as_deref(), Some("Data flow"));
        assert!(q.expected[1].heading.is_none());
        assert_eq!(q.tags.as_deref(), Some(&["retrieval".to_string()][..]));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_golden_rejects_unknown_field() {
        let path = write_yaml(
            "eval-golden-bad",
            "queries:\n- query: \"x\"\n  expected: []\n  bogus: 1\n",
        );
        let err = GoldenSet::load(&path).expect_err("unknown field must reject");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("bogus") || msg.contains("unknown"),
            "error chain should mention bogus/unknown, got: {msg}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_golden_missing_file_is_error() {
        let path = std::env::temp_dir().join("nonexistent-eval-golden.yml");
        let _ = std::fs::remove_file(&path);
        let err = GoldenSet::load(&path).expect_err("missing file must error");
        assert!(err.to_string().contains("no golden file"));
    }

    fn hit(rank: usize, path: &str, heading: Option<&str>) -> HitRecord {
        HitRecord {
            rank,
            path: path.into(),
            heading: heading.map(|s| s.into()),
            score: 1.0,
        }
    }
    fn exp(path: &str, heading: Option<&str>) -> ExpectedHit {
        ExpectedHit {
            path: path.into(),
            heading: heading.map(|s| s.into()),
        }
    }

    #[test]
    fn test_is_hit_path_only() {
        assert!(is_hit(&exp("a.md", None), &hit(1, "a.md", Some("H1"))));
        assert!(!is_hit(&exp("a.md", None), &hit(1, "b.md", Some("H1"))));
    }

    #[test]
    fn test_is_hit_heading_match_case_and_whitespace() {
        assert!(is_hit(
            &exp("a.md", Some("Data Flow")),
            &hit(1, "a.md", Some("  data flow "))
        ));
    }

    #[test]
    fn test_is_hit_heading_mismatch() {
        assert!(!is_hit(
            &exp("a.md", Some("X")),
            &hit(1, "a.md", Some("Y"))
        ));
    }

    #[test]
    fn test_recall_at_k_all_hit() {
        let expected = vec![exp("a.md", None), exp("b.md", None)];
        let top = vec![
            hit(1, "a.md", None),
            hit(2, "b.md", None),
            hit(3, "c.md", None),
        ];
        assert!((recall_at_k(&expected, &top, 5) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn test_recall_at_k_partial_within_k() {
        let expected = vec![exp("a.md", None), exp("b.md", None)];
        let top = vec![
            hit(1, "a.md", None),
            hit(2, "x.md", None),
            hit(3, "b.md", None),
        ];
        assert!((recall_at_k(&expected, &top, 2) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn test_recall_at_k_no_expected_is_nan_sentinel() {
        let top = vec![hit(1, "a.md", None)];
        assert_eq!(recall_at_k(&[], &top, 5), 0.0);
    }

    #[test]
    fn test_recall_at_k_empty_top() {
        let expected = vec![exp("a.md", None)];
        assert_eq!(recall_at_k(&expected, &[], 5), 0.0);
    }

    #[test]
    fn test_reciprocal_rank_first_hit() {
        let expected = vec![exp("a.md", None)];
        let top = vec![
            hit(1, "x.md", None),
            hit(2, "a.md", None),
            hit(3, "b.md", None),
        ];
        assert!((reciprocal_rank(&expected, &top) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn test_reciprocal_rank_no_hit() {
        let expected = vec![exp("a.md", None)];
        let top = vec![hit(1, "x.md", None)];
        assert_eq!(reciprocal_rank(&expected, &top), 0.0);
    }

    #[test]
    fn test_reciprocal_rank_empty() {
        assert_eq!(reciprocal_rank(&[], &[]), 0.0);
    }

    #[test]
    fn test_ndcg_ideal_order() {
        let expected = vec![exp("a.md", None), exp("b.md", None)];
        let top = vec![
            hit(1, "a.md", None),
            hit(2, "b.md", None),
            hit(3, "x.md", None),
        ];
        assert!((ndcg_at_k(&expected, &top, 5) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn test_ndcg_reversed() {
        let expected = vec![exp("a.md", None), exp("b.md", None)];
        let top = vec![
            hit(1, "x.md", None),
            hit(2, "a.md", None),
            hit(3, "b.md", None),
        ];
        let score = ndcg_at_k(&expected, &top, 5);
        assert!(
            score > 0.0 && score < 1.0,
            "expected 0<score<1, got {score}"
        );
    }

    #[test]
    fn test_ndcg_no_hit() {
        let expected = vec![exp("a.md", None)];
        let top = vec![hit(1, "x.md", None), hit(2, "y.md", None)];
        assert_eq!(ndcg_at_k(&expected, &top, 5), 0.0);
    }

    #[test]
    fn test_ndcg_empty_expected() {
        let top = vec![hit(1, "a.md", None)];
        assert_eq!(ndcg_at_k(&[], &top, 5), 0.0);
    }

    #[test]
    fn test_compute_query_metrics() {
        let expected = vec![exp("a.md", None), exp("b.md", None)];
        let top = vec![
            hit(1, "a.md", None),
            hit(2, "x.md", None),
            hit(3, "b.md", None),
        ];
        let m = compute_query_metrics(&expected, &top, &[1, 3, 5]);
        assert!((m.recall_at_k[&1] - 0.5).abs() < 1e-9);
        assert!((m.recall_at_k[&3] - 1.0).abs() < 1e-9);
        assert!((m.reciprocal_rank - 1.0).abs() < 1e-9);
        let ndcg3 = m.ndcg_at_k[&3];
        assert!(ndcg3 > 0.7 && ndcg3 < 1.0, "ndcg@3 = {ndcg3}");
    }

    #[test]
    fn test_aggregate_metrics_mean() {
        let q1 = QueryResult {
            id: "1".into(),
            query: "q1".into(),
            expected: vec![exp("a.md", None)],
            top_k: vec![hit(1, "a.md", None)],
            metrics: compute_query_metrics(
                &[exp("a.md", None)],
                &[hit(1, "a.md", None)],
                &[1, 5],
            ),
        };
        let q2 = QueryResult {
            id: "2".into(),
            query: "q2".into(),
            expected: vec![exp("b.md", None)],
            top_k: vec![hit(1, "x.md", None)],
            metrics: compute_query_metrics(
                &[exp("b.md", None)],
                &[hit(1, "x.md", None)],
                &[1, 5],
            ),
        };
        let agg = aggregate_metrics(&[q1, q2], &[1, 5]);
        assert!((agg.recall_at_k[&1] - 0.5).abs() < 1e-9);
        assert!((agg.mrr - 0.5).abs() < 1e-9);
        assert_eq!(agg.query_count, 2);
    }

    fn sample_run(ts_secs: i64, recall10: f64) -> EvalRun {
        use chrono::TimeZone;
        let mut agg = AggregateMetrics::default();
        agg.recall_at_k.insert(10, recall10);
        agg.query_count = 1;
        EvalRun {
            timestamp: Utc.timestamp_opt(ts_secs, 0).unwrap(),
            fingerprint: ConfigFingerprint {
                model: "bge-m3".into(),
                reranker: None,
                limit: 10,
                k_values: vec![1, 5, 10],
                golden_hash: "deadbeef".into(),
            },
            per_query: vec![],
            aggregate: agg,
        }
    }

    #[test]
    fn test_history_load_missing_returns_empty() {
        let path = std::env::temp_dir().join("kb-mcp-hist-missing.json");
        let _ = std::fs::remove_file(&path);
        let h = History::load(&path).unwrap();
        assert!(h.runs.is_empty());
    }

    #[test]
    fn test_history_load_corrupt_returns_empty_with_warn() {
        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("kb-mcp-hist-corrupt-{pid}.json"));
        std::fs::write(&path, "{not json").unwrap();
        let h = History::load(&path).unwrap();
        assert!(h.runs.is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_history_save_and_reload_round_trip() {
        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("kb-mcp-hist-rt-{pid}.json"));
        let _ = std::fs::remove_file(&path);
        let mut h = History::default();
        h.push_front(sample_run(100, 0.5), 10);
        h.save(&path).unwrap();
        let reloaded = History::load(&path).unwrap();
        assert_eq!(reloaded.runs.len(), 1);
        assert!((reloaded.runs[0].aggregate.recall_at_k[&10] - 0.5).abs() < 1e-9);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_history_push_front_truncates_to_size() {
        let mut h = History::default();
        for i in 0..15 {
            h.push_front(sample_run(i as i64, 0.0), 10);
        }
        assert_eq!(h.runs.len(), 10);
        assert_eq!(h.runs.front().unwrap().timestamp.timestamp(), 14);
    }

    #[test]
    fn test_aggregate_metrics_skips_empty_expected() {
        let q_empty = QueryResult {
            id: "e".into(),
            query: "q".into(),
            expected: vec![],
            top_k: vec![hit(1, "a.md", None)],
            metrics: compute_query_metrics(&[], &[hit(1, "a.md", None)], &[1]),
        };
        let q_ok = QueryResult {
            id: "o".into(),
            query: "q".into(),
            expected: vec![exp("a.md", None)],
            top_k: vec![hit(1, "a.md", None)],
            metrics: compute_query_metrics(&[exp("a.md", None)], &[hit(1, "a.md", None)], &[1]),
        };
        let agg = aggregate_metrics(&[q_empty, q_ok], &[1]);
        assert_eq!(agg.query_count, 1);
        assert!((agg.recall_at_k[&1] - 1.0).abs() < 1e-9);
    }
}
