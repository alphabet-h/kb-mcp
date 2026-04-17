use anyhow::Result;
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use std::path::PathBuf;

/// Embedding モデル選択肢。CLI の `--model` と共有される。
///
/// 追加時の手順: variant を足し、`model_id` / `dimension` /
/// `fastembed_model` / `approx_download_mb` の 4 メソッドに分岐を追加する。
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum ModelChoice {
    /// BAAI/bge-small-en-v1.5 (384 dim, 英語特化, ~130 MB)
    #[value(name = "bge-small-en-v1.5")]
    BgeSmallEnV15,
    /// BAAI/bge-m3 (1024 dim, 多言語, ~2.3 GB)
    #[value(name = "bge-m3")]
    BgeM3,
}

impl ModelChoice {
    pub fn model_id(self) -> &'static str {
        match self {
            Self::BgeSmallEnV15 => "bge-small-en-v1.5",
            Self::BgeM3 => "bge-m3",
        }
    }

    pub fn dimension(self) -> usize {
        match self {
            Self::BgeSmallEnV15 => 384,
            Self::BgeM3 => 1024,
        }
    }

    fn fastembed_model(self) -> EmbeddingModel {
        match self {
            Self::BgeSmallEnV15 => EmbeddingModel::BGESmallENV15,
            Self::BgeM3 => EmbeddingModel::BGEM3,
        }
    }

    /// 初回 DL サイズの目安 (ユーザ告知用)
    fn approx_download_mb(self) -> u32 {
        match self {
            Self::BgeSmallEnV15 => 130,
            Self::BgeM3 => 2300,
        }
    }
}

impl Default for ModelChoice {
    // 既存 DB 互換のため据え置き。BGE-M3 へ切り替えたい場合は明示オプトイン。
    fn default() -> Self {
        Self::BgeSmallEnV15
    }
}

/// Thin wrapper around fastembed for generating text embeddings.
///
/// モデルは [`ModelChoice`] で切替可能。ONNX モデルは初回実行時に
/// [`resolve_cache_dir`] のキャッシュディレクトリへダウンロードされる。
pub struct Embedder {
    model: TextEmbedding,
    choice: ModelChoice,
}

impl Embedder {
    /// デフォルトモデル ([`ModelChoice::default`]) で初期化する。
    ///
    /// Cache directory resolution (in order):
    /// 1. `FASTEMBED_CACHE_DIR` environment variable if set
    /// 2. OS-standard cache directory joined with `fastembed`
    ///    (Linux: `~/.cache/fastembed`, macOS: `~/Library/Caches/fastembed`,
    ///     Windows: `%LOCALAPPDATA%\fastembed`)
    /// 3. `.fastembed_cache` relative to the working directory (fastembed's own default)
    pub fn new() -> Result<Self> {
        Self::with_model(ModelChoice::default())
    }

    /// 明示的にモデルを指定して初期化する。
    pub fn with_model(choice: ModelChoice) -> Result<Self> {
        eprintln!(
            "Loading embedding model: {} ({} dim, ~{} MB on first run)...",
            choice.model_id(),
            choice.dimension(),
            choice.approx_download_mb()
        );
        let model = TextEmbedding::try_new(
            InitOptions::new(choice.fastembed_model())
                .with_cache_dir(resolve_cache_dir())
                .with_show_download_progress(true),
        )?;
        Ok(Self { model, choice })
    }

    /// Embed multiple texts in a batch.
    pub fn embed_texts(&mut self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let embeddings = self.model.embed(texts, None)?;
        Ok(embeddings)
    }

    /// Embed a single text.
    pub fn embed_single(&mut self, text: &str) -> Result<Vec<f32>> {
        let mut results = self.embed_texts(&[text])?;
        results
            .pop()
            .ok_or_else(|| anyhow::anyhow!("embedding returned empty result"))
    }

    /// 選択中のモデルの埋め込み次元数。
    pub fn dimension(&self) -> usize {
        self.choice.dimension()
    }

    /// 選択中のモデルの識別子 (index_meta に記録される)。
    pub fn model_id(&self) -> &'static str {
        self.choice.model_id()
    }
}

fn resolve_cache_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("FASTEMBED_CACHE_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(base) = dirs::cache_dir() {
        return base.join("fastembed");
    }
    PathBuf::from(".fastembed_cache")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore] // requires model download (~23 MB)
    fn test_embed_produces_384_dim() {
        let mut embedder = Embedder::new().expect("failed to initialize embedder");
        let embedding = embedder
            .embed_single("hello world")
            .expect("failed to embed");
        assert_eq!(embedding.len(), 384);
    }

    #[test]
    #[ignore] // requires model download (~23 MB)
    fn test_embed_batch() {
        let mut embedder = Embedder::new().expect("failed to initialize embedder");
        let embeddings = embedder
            .embed_texts(&["hello", "world"])
            .expect("failed to embed batch");
        assert_eq!(embeddings.len(), 2);
        assert_eq!(embeddings[0].len(), 384);
        assert_eq!(embeddings[1].len(), 384);
    }

    #[test]
    fn test_model_choice_values() {
        assert_eq!(ModelChoice::BgeSmallEnV15.model_id(), "bge-small-en-v1.5");
        assert_eq!(ModelChoice::BgeSmallEnV15.dimension(), 384);
        assert_eq!(ModelChoice::BgeM3.model_id(), "bge-m3");
        assert_eq!(ModelChoice::BgeM3.dimension(), 1024);
        assert_eq!(ModelChoice::default(), ModelChoice::BgeSmallEnV15);
    }

    #[test]
    #[ignore] // requires BGE-M3 download (~2.3 GB)
    fn test_bge_m3_produces_1024_dim() {
        let mut embedder =
            Embedder::with_model(ModelChoice::BgeM3).expect("failed to load BGE-M3");
        let emb = embedder
            .embed_single("こんにちは、世界")
            .expect("failed to embed");
        assert_eq!(emb.len(), 1024);
    }
}
