use anyhow::Result;
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use std::path::PathBuf;

/// Thin wrapper around fastembed for generating text embeddings.
///
/// Uses BGE-small-en-v1.5 (384 dimensions). The ONNX model is downloaded
/// on first use to the resolved cache directory (see [`resolve_cache_dir`]).
pub struct Embedder {
    model: TextEmbedding,
}

impl Embedder {
    /// Initialize with BGE-small-en-v1.5 (384 dimensions).
    ///
    /// Cache directory resolution (in order):
    /// 1. `FASTEMBED_CACHE_DIR` environment variable if set
    /// 2. OS-standard cache directory joined with `fastembed`
    ///    (Linux: `~/.cache/fastembed`, macOS: `~/Library/Caches/fastembed`,
    ///     Windows: `%LOCALAPPDATA%\fastembed`)
    /// 3. `.fastembed_cache` relative to the working directory (fastembed's own default)
    pub fn new() -> Result<Self> {
        let model = TextEmbedding::try_new(
            InitOptions::new(EmbeddingModel::BGESmallENV15)
                .with_cache_dir(resolve_cache_dir())
                .with_show_download_progress(true),
        )?;
        Ok(Self { model })
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

    /// Returns the embedding dimension (384 for BGE-small-en-v1.5).
    pub fn dimension(&self) -> usize {
        384
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
}
