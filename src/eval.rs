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
}
