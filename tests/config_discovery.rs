//! `kb-mcp --config <path>` global flag の e2e。tests/validate_cli.rs の TempKb
//! パターンを踏襲。embedding DL 不要なので通常の `cargo test` に載せる。

use std::path::{Path, PathBuf};
use std::process::Command;

fn kb_mcp_bin() -> Option<PathBuf> {
    let target = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
    let profile = if cfg!(debug_assertions) { "debug" } else { "release" };
    #[cfg(windows)]
    let bin = target.join(profile).join("kb-mcp.exe");
    #[cfg(not(windows))]
    let bin = target.join(profile).join("kb-mcp");
    if bin.exists() { Some(bin) } else { None }
}

struct TempDir {
    path: PathBuf,
}
impl TempDir {
    fn new(prefix: &str) -> Self {
        let pid = std::process::id();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!("{prefix}-{pid}-{nonce}"));
        std::fs::create_dir_all(&path).unwrap();
        Self { path }
    }
    #[allow(dead_code)]
    fn path(&self) -> &Path { &self.path }
    #[allow(dead_code)]
    fn write(&self, rel: &str, content: &str) {
        let full = self.path.join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(full, content).unwrap();
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[test]
fn test_explicit_config_missing_fails_fast() {
    let Some(bin) = kb_mcp_bin() else { return };
    let dir = TempDir::new("kb-mcp-disc-explicit-miss");
    let nope = dir.path().join("nope.toml");
    let out = Command::new(&bin)
        .args(["--config"])
        .arg(&nope)
        .args(["status", "--kb-path", "/tmp/whatever"])
        .output()
        .expect("spawn kb-mcp");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "expected non-zero exit, stderr={stderr}");
    assert!(
        stderr.contains("--config") && stderr.contains("not found"),
        "stderr must mention `--config ... not found`: {stderr}"
    );
}
