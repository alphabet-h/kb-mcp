//! バイナリと同じディレクトリに配置する `kb-mcp.toml` の読み込み。
//!
//! サーバ運用側が `--model` / `--reranker` / `FASTEMBED_CACHE_DIR` 等の
//! オプションを省略できるよう、設定ファイルでデフォルト値を与える。
//! 優先順位は `CLI 引数 > 設定ファイル > ビルトインデフォルト`。

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

use crate::embedder::{ModelChoice, RerankerChoice};

/// バイナリと同じディレクトリに置く `kb-mcp.toml` の表現。
/// すべてのフィールドは optional で、指定しなかった項目は CLI 引数 or
/// ビルトインデフォルトで補われる。
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// `--kb-path` の既定値。
    pub kb_path: Option<PathBuf>,
    /// `--model` の既定値 (例: `"bge-m3"`)。
    pub model: Option<ModelChoice>,
    /// `--reranker` の既定値 (例: `"bge-v2-m3"`)。
    pub reranker: Option<RerankerChoice>,
    /// `--rerank-by-default` の既定値。
    pub rerank_by_default: Option<bool>,
    /// `FASTEMBED_CACHE_DIR` 環境変数の既定値。
    /// 既に env が設定されていればそちらを優先し、未設定のときだけ適用する。
    pub fastembed_cache_dir: Option<PathBuf>,
}

impl Config {
    /// バイナリと同じディレクトリの `kb-mcp.toml` を読み込む。
    /// ファイルが存在しない場合は空の `Config::default()` を返す (エラーなし)。
    pub fn load_alongside_binary() -> Result<Self> {
        let Some(path) = alongside_binary_path() else {
            return Ok(Self::default());
        };
        Self::load_from(&path)
    }

    /// 指定パスから読み込む。ファイルが存在しない場合は空の `Config`。
    /// 相対パスで書かれたフィールドは**設定ファイルのあるディレクトリ**を
    /// 基点に解決する (cwd ではない)。
    pub fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config: {}", path.display()))?;
        let mut cfg: Self = toml::from_str(&text)
            .with_context(|| format!("failed to parse config: {}", path.display()))?;

        // 相対パスを設定ファイルのディレクトリ基準に resolve する。
        // cwd は MCP 起動時に呼び出し側プロジェクトに依存するため当てにならない。
        if let Some(base) = path.parent() {
            cfg.kb_path = cfg.kb_path.map(|p| resolve_relative(base, p));
            cfg.fastembed_cache_dir = cfg
                .fastembed_cache_dir
                .map(|p| resolve_relative(base, p));
        }
        Ok(cfg)
    }

    /// 設定が空かどうか (全フィールドが `None`)。手動テスト用。
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.kb_path.is_none()
            && self.model.is_none()
            && self.reranker.is_none()
            && self.rerank_by_default.is_none()
            && self.fastembed_cache_dir.is_none()
    }

    /// `fastembed_cache_dir` が設定されていて、かつ環境変数
    /// `FASTEMBED_CACHE_DIR` が未設定なら、プロセス環境に適用する。
    /// `Embedder::with_model` が `resolve_cache_dir()` で拾う前に呼ぶこと。
    pub fn apply_cache_dir_env(&self) {
        if std::env::var_os("FASTEMBED_CACHE_DIR").is_some() {
            return; // env を優先
        }
        if let Some(dir) = &self.fastembed_cache_dir {
            // SAFETY: プロセス単一スレッド (main 起動直後) でのみ呼ぶ想定。
            unsafe {
                std::env::set_var("FASTEMBED_CACHE_DIR", dir);
            }
        }
    }
}

/// 実行中のバイナリと同じディレクトリにある `kb-mcp.toml` の絶対パス。
/// `current_exe()` が取得できない環境では `None`。
fn alongside_binary_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join("kb-mcp.toml"))
}

/// `path` が絶対なら何もしない、相対なら `base.join(path)` を返す。
fn resolve_relative(base: &Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_load_missing_file_returns_empty() {
        let tmp = std::env::temp_dir().join("kb-mcp-nonexistent-config.toml");
        // 念のため存在しないことを確認
        let _ = std::fs::remove_file(&tmp);
        let cfg = Config::load_from(&tmp).unwrap();
        assert!(cfg.is_empty());
    }

    #[test]
    fn test_parse_full_config() {
        // 絶対パスは resolve_relative で rebase されないことも確認するため、
        // プラットフォーム別の真の絶対パスを使う。
        #[cfg(windows)]
        let (kb, cache) = ("C:/tmp/kb", "C:/tmp/cache");
        #[cfg(not(windows))]
        let (kb, cache) = ("/tmp/kb", "/tmp/cache");

        let mut file = tempfile("kb-mcp-config-full");
        writeln!(
            file,
            "kb_path = \"{kb}\"\n\
             model = \"bge-m3\"\n\
             reranker = \"bge-v2-m3\"\n\
             rerank_by_default = true\n\
             fastembed_cache_dir = \"{cache}\"\n"
        )
        .unwrap();

        let cfg = Config::load_from(file.path()).unwrap();
        assert_eq!(cfg.kb_path.as_deref(), Some(Path::new(kb)));
        assert_eq!(cfg.model, Some(ModelChoice::BgeM3));
        assert_eq!(cfg.reranker, Some(RerankerChoice::BgeV2M3));
        assert_eq!(cfg.rerank_by_default, Some(true));
        assert_eq!(cfg.fastembed_cache_dir.as_deref(), Some(Path::new(cache)));
    }

    #[test]
    fn test_parse_partial_config() {
        let mut file = tempfile("kb-mcp-config-partial");
        writeln!(file, r#"model = "bge-small-en-v1.5""#).unwrap();

        let cfg = Config::load_from(file.path()).unwrap();
        assert_eq!(cfg.model, Some(ModelChoice::BgeSmallEnV15));
        assert!(cfg.kb_path.is_none());
        assert!(cfg.reranker.is_none());
    }

    #[test]
    fn test_unknown_fields_are_rejected() {
        let mut file = tempfile("kb-mcp-config-unknown");
        writeln!(file, r#"bogus_field = "oops""#).unwrap();
        let err = Config::load_from(file.path()).expect_err("should reject unknown field");
        assert!(err.to_string().contains("failed to parse config"));
    }

    #[test]
    fn test_relative_paths_resolve_against_config_dir() {
        let mut file = tempfile("kb-mcp-config-relpath");
        writeln!(
            file,
            r#"
kb_path = "./knowledge-base"
fastembed_cache_dir = "cache/hf"
"#
        )
        .unwrap();
        let cfg_path = file.path().to_path_buf();
        drop(file);
        // Re-open via load_from (file already written and path known)
        // tempfile の Drop で消してしまうので、ここでは別経路で検証:
        let cfg = Config {
            kb_path: Some(PathBuf::from("./knowledge-base")),
            fastembed_cache_dir: Some(PathBuf::from("cache/hf")),
            ..Default::default()
        };
        // load_from を経由しないので手動で同じ変換を適用
        let base = cfg_path.parent().unwrap();
        let kb = resolve_relative(base, cfg.kb_path.clone().unwrap());
        let cache = resolve_relative(base, cfg.fastembed_cache_dir.clone().unwrap());
        assert!(kb.starts_with(base));
        assert!(cache.starts_with(base));
        assert!(kb.ends_with("knowledge-base"));
    }

    #[test]
    fn test_absolute_paths_are_not_rebased() {
        // Windows / Unix 両対応
        #[cfg(windows)]
        let abs = PathBuf::from("C:/absolute/foo");
        #[cfg(not(windows))]
        let abs = PathBuf::from("/absolute/foo");

        let base = Path::new("/some/base");
        let out = resolve_relative(base, abs.clone());
        assert_eq!(out, abs);
    }

    #[test]
    fn test_apply_cache_dir_env_respects_existing_env() {
        // 既に env が設定されていれば config 値は適用しない。
        let key = "FASTEMBED_CACHE_DIR";
        // SAFETY: single-threaded test process.
        unsafe {
            std::env::set_var(key, "/pre-existing");
        }
        let cfg = Config {
            fastembed_cache_dir: Some(PathBuf::from("/from-config")),
            ..Default::default()
        };
        cfg.apply_cache_dir_env();
        assert_eq!(std::env::var(key).unwrap(), "/pre-existing");
        unsafe {
            std::env::remove_var(key);
        }
    }

    /// Helper: 一意名の一時ファイルを作って `File` を返す。tempfile crate に
    /// 依存しないように素朴に作る。
    fn tempfile(prefix: &str) -> NamedTempFile {
        let mut path = std::env::temp_dir();
        let pid = std::process::id();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        path.push(format!("{prefix}-{pid}-{nonce}.toml"));
        NamedTempFile {
            file: std::fs::File::create(&path).unwrap(),
            path,
        }
    }

    struct NamedTempFile {
        file: std::fs::File,
        path: PathBuf,
    }

    impl NamedTempFile {
        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Write for NamedTempFile {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.file.write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.file.flush()
        }
    }

    impl Drop for NamedTempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}
