//! バイナリと同じディレクトリに配置する `kb-mcp.toml` の読み込み。
//!
//! サーバ運用側が `--model` / `--reranker` / `FASTEMBED_CACHE_DIR` 等の
//! オプションを省略できるよう、設定ファイルでデフォルト値を与える。
//! 優先順位は `CLI 引数 > 設定ファイル > ビルトインデフォルト`。

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

use crate::embedder::{ModelChoice, RerankerChoice};
use crate::quality::QualityFilterConfig;

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
    /// Markdown チャンク化時に除外する見出し文字列の一覧 (substring match)。
    /// 省略時 (`None`) は [`crate::markdown::DEFAULT_EXCLUDED_HEADINGS`]。
    /// 明示的に `[]` を与えると「除外しない」という意味になる。
    pub exclude_headings: Option<Vec<String>>,
    /// [feature 13] 検索時に適用するチャンク品質フィルタの設定。
    /// 省略時は [`QualityFilterConfig::default()`] (enabled=true, threshold=0.3)。
    pub quality_filter: Option<QualityFilterConfig>,
    /// [feature 16] `get_best_practice` MCP ツールで使うパス候補テンプレート。
    /// 省略時は `["best-practices/{target}/PERFECT.md"]` (後方互換)。
    pub best_practice: Option<BestPracticeConfig>,
}

/// `get_best_practice` の汎用化設定 (feature 16)。
///
/// `path_templates` に列挙した順に `{target}` を置換してファイルを探し、
/// 最初に存在したものを返す。テンプレート変数:
///   - `{target}` : ツールに渡された target パラメータ
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BestPracticeConfig {
    #[serde(default = "default_best_practice_templates")]
    pub path_templates: Vec<String>,
}

fn default_best_practice_templates() -> Vec<String> {
    vec!["best-practices/{target}/PERFECT.md".to_string()]
}

impl Default for BestPracticeConfig {
    fn default() -> Self {
        Self {
            path_templates: default_best_practice_templates(),
        }
    }
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
            && self.exclude_headings.is_none()
            && self.quality_filter.is_none()
            && self.best_practice.is_none()
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
             fastembed_cache_dir = \"{cache}\"\n\
             exclude_headings = [\"次の深堀り候補\", \"参考リンク\"]\n"
        )
        .unwrap();

        let cfg = Config::load_from(file.path()).unwrap();
        assert_eq!(cfg.kb_path.as_deref(), Some(Path::new(kb)));
        assert_eq!(cfg.model, Some(ModelChoice::BgeM3));
        assert_eq!(cfg.reranker, Some(RerankerChoice::BgeV2M3));
        assert_eq!(cfg.rerank_by_default, Some(true));
        assert_eq!(cfg.fastembed_cache_dir.as_deref(), Some(Path::new(cache)));
        assert_eq!(
            cfg.exclude_headings.as_deref(),
            Some(&["次の深堀り候補".to_string(), "参考リンク".to_string()][..])
        );
    }

    #[test]
    fn test_best_practice_default_templates() {
        // 省略時はレガシーの PERFECT.md パス 1 件
        let cfg = BestPracticeConfig::default();
        assert_eq!(
            cfg.path_templates,
            vec!["best-practices/{target}/PERFECT.md".to_string()]
        );
    }

    #[test]
    fn test_best_practice_config_parses_from_toml() {
        let mut file = tempfile("kb-mcp-config-bp");
        writeln!(
            file,
            "[best_practice]\n\
             path_templates = [\"docs/{{target}}.md\", \"guides/{{target}}/README.md\"]\n"
        )
        .unwrap();
        let cfg = Config::load_from(file.path()).unwrap();
        let bp = cfg.best_practice.expect("best_practice must be Some");
        assert_eq!(bp.path_templates.len(), 2);
        assert_eq!(bp.path_templates[0], "docs/{target}.md");
        assert_eq!(bp.path_templates[1], "guides/{target}/README.md");
    }

    #[test]
    fn test_best_practice_empty_path_templates_uses_default() {
        // path_templates 省略時は default_best_practice_templates() が入る
        let mut file = tempfile("kb-mcp-config-bp2");
        writeln!(file, "[best_practice]").unwrap();
        let cfg = Config::load_from(file.path()).unwrap();
        let bp = cfg.best_practice.expect("best_practice must be Some");
        assert_eq!(
            bp.path_templates,
            vec!["best-practices/{target}/PERFECT.md".to_string()]
        );
    }

    #[test]
    fn test_parse_empty_exclude_headings_overrides_default() {
        // `exclude_headings = []` を明示すると「除外しない」という意図になるため、
        // Option::None と区別して保持されていることを確認する。
        let mut file = tempfile("kb-mcp-config-empty-excludes");
        writeln!(file, "exclude_headings = []").unwrap();
        let cfg = Config::load_from(file.path()).unwrap();
        let list = cfg.exclude_headings.expect("Some(vec![]) must be preserved");
        assert!(list.is_empty());
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
        // load_from 内部の「parent → resolve_relative」経路を実際に通す e2e。
        // tempfile helper は Drop でファイルを消してしまうので、ここではテスト
        // 終了時に削除する `DirGuard` でファイル書込から load_from まで 1 本化する。
        let pid = std::process::id();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("kb-mcp-test-relpath-{pid}-{nonce}"));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg_path = dir.join("kb-mcp.toml");
        std::fs::write(
            &cfg_path,
            "kb_path = \"./knowledge-base\"\n\
             fastembed_cache_dir = \"cache/hf\"\n",
        )
        .unwrap();

        struct DirGuard(PathBuf);
        impl Drop for DirGuard {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _guard = DirGuard(dir.clone());

        let cfg = Config::load_from(&cfg_path).unwrap();

        let kb = cfg.kb_path.expect("kb_path must be Some");
        let cache = cfg.fastembed_cache_dir.expect("fastembed_cache_dir must be Some");
        assert!(kb.is_absolute() || kb.starts_with(&dir), "kb_path not rebased: {kb:?}");
        assert!(kb.ends_with("knowledge-base"));
        assert!(cache.starts_with(&dir));
        assert!(cache.ends_with(Path::new("cache/hf")) || cache.ends_with(Path::new("cache\\hf")));
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

    #[cfg(windows)]
    #[test]
    fn test_windows_unc_and_verbatim_paths_not_rebased() {
        // UNC パスと \\?\ verbatim プレフィックスは std::path::Path::is_absolute
        // で true を返すので、resolve_relative は touch しない。
        let base = Path::new("C:/some/base");

        let unc = PathBuf::from(r"\\server\share\foo");
        assert!(unc.is_absolute(), "UNC should be absolute");
        assert_eq!(resolve_relative(base, unc.clone()), unc);

        let verbatim = PathBuf::from(r"\\?\C:\verbatim\bar");
        assert!(verbatim.is_absolute(), "verbatim prefix should be absolute");
        assert_eq!(resolve_relative(base, verbatim.clone()), verbatim);
    }

    #[test]
    fn test_toml_example_parses_with_all_keys_uncommented() {
        // kb-mcp.toml.example のすべてのキーが Config で受け入れられるかを検証。
        // Config にフィールドが追加されたのに example を更新し忘れたり、逆に
        // example に古いキーが残って deny_unknown_fields に引っかかるのを
        // 回帰テストで検知する。
        //
        // example はコメント (`#`) で各フィールド例を書いているので、
        // 行頭 `#` を剥がして「全行有効」な設定としてパースする。
        let example_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("kb-mcp.toml.example");
        let raw = std::fs::read_to_string(&example_path)
            .expect("kb-mcp.toml.example must exist at repository root");

        // 同じキーを 2 回以上コメント化して例示することがある
        // (例: exclude_headings の `[...]` と `[]` の両方を示す)。
        // uncomment 後に重複キーになると toml::from_str がエラーになるので、
        // 「同じキーは最初の 1 行だけ uncomment、以降はコメントのまま残す」
        // 方針で剥がす。
        let mut seen_keys: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let uncommented: String = raw
            .lines()
            .map(|line| {
                let trimmed = line.trim_start();
                // 見出しコメントや空行はそのまま (除外しても同じ挙動)
                if trimmed.is_empty() {
                    return String::new();
                }
                // `# key = value` 行を剥がす。ただし純粋な説明コメント
                // (例: `# Copy this file...`) はそのまま残す (toml には
                // 影響しないので除外しても同じ)。判定は `# <ident> =` の形。
                if let Some(rest) = trimmed.strip_prefix('#') {
                    let rest = rest.trim_start();
                    if let Some(eq_idx) = rest.find('=')
                        && rest
                            .chars()
                            .next()
                            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                    {
                        let key = rest[..eq_idx].trim().to_string();
                        if seen_keys.insert(key) {
                            return rest.to_string();
                        }
                        // 2 回目以降はコメントのまま残して重複を避ける
                    }
                }
                line.to_string()
            })
            .collect::<Vec<_>>()
            .join("\n");

        let cfg: Config = toml::from_str(&uncommented).unwrap_or_else(|e| {
            panic!(
                "kb-mcp.toml.example failed to parse with all keys enabled: {e}\n\
                 --- generated TOML ---\n{uncommented}\n---"
            )
        });

        // 全フィールドが埋まっていれば is_empty は false。example に少なくとも
        // 1 つのキーが書かれていることの最低限チェック。
        assert!(
            !cfg.is_empty(),
            "kb-mcp.toml.example contains no parseable keys"
        );
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
