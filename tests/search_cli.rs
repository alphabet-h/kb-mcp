//! `kb-mcp search` CLI integration test。wrapper 形式の出力 + 新フィルタ引数の sanity。

use std::path::{Path, PathBuf};
use std::process::Command;

fn unique_dir(prefix: &str) -> PathBuf {
    let pid = std::process::id();
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("{prefix}-{pid}-{nonce}"))
}

struct Guard(PathBuf);
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn bin() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join(if cfg!(debug_assertions) { "debug" } else { "release" })
        .join(if cfg!(windows) { "kb-mcp.exe" } else { "kb-mcp" })
}

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, content).unwrap();
}

#[test]
#[ignore] // requires built binary + embedding model download
fn cli_search_returns_wrapper_json() {
    let dir = unique_dir("kb-mcp-search-cli");
    std::fs::create_dir_all(&dir).unwrap();
    let _g = Guard(dir.clone());

    write(
        &dir.join("a.md"),
        "---\ntitle: A\ntags: [rust]\n---\n# heading\n\nrust async tokio body\n",
    );

    // Index first
    let st = Command::new(bin())
        .args(["index", "--kb-path", dir.to_str().unwrap()])
        .status()
        .expect("kb-mcp index");
    assert!(st.success());

    // Search with --format json
    let out = Command::new(bin())
        .args([
            "search",
            "rust",
            "--kb-path",
            dir.to_str().unwrap(),
            "--format",
            "json",
        ])
        .output()
        .expect("kb-mcp search");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // wrapper 形式の特徴を検証
    assert!(stdout.contains("\"results\""), "must wrap in 'results'");
    assert!(
        stdout.contains("\"low_confidence\""),
        "must include 'low_confidence'"
    );
    assert!(
        stdout.contains("\"filter_applied\""),
        "must include 'filter_applied'"
    );
}

#[test]
#[ignore]
fn cli_search_with_path_glob_filter_excludes() {
    let dir = unique_dir("kb-mcp-search-cli-pg");
    std::fs::create_dir_all(&dir).unwrap();
    let _g = Guard(dir.clone());

    write(&dir.join("docs/a.md"), "rust body");
    write(&dir.join("notes/b.md"), "rust body");

    let st = Command::new(bin())
        .args(["index", "--kb-path", dir.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(st.success());

    let out = Command::new(bin())
        .args([
            "search",
            "rust",
            "--kb-path",
            dir.to_str().unwrap(),
            "--path-glob",
            "docs/**",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("docs/a.md"));
    assert!(!stdout.contains("notes/b.md"));
}
