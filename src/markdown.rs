use serde::Deserialize;

/// YAML frontmatter extracted from a Markdown document.
#[derive(Debug, Clone, Default)]
pub struct Frontmatter {
    pub title: Option<String>,
    pub date: Option<String>,
    pub topic: Option<String>,
    pub depth: Option<String>,
    pub tags: Vec<String>,
}

/// A single chunk of a Markdown document, split on headings.
#[derive(Debug, Clone)]
pub struct Chunk {
    pub index: usize,
    pub heading: Option<String>,
    pub content: String,
}

/// A fully parsed Markdown document: frontmatter + body chunks.
#[derive(Debug, Clone)]
pub struct ParsedDocument {
    pub frontmatter: Frontmatter,
    pub chunks: Vec<Chunk>,
    pub raw_content: String,
}

// ---------------------------------------------------------------------------
// Internal: serde helper for flexible YAML deserialization
// ---------------------------------------------------------------------------

/// Intermediate representation for serde_yaml deserialization.
/// `date` is captured as `serde_yaml::Value` so it works regardless of whether
/// the YAML encodes it as a string (`"2026-04-10"`) or a native date value.
#[derive(Deserialize)]
struct RawFrontmatter {
    title: Option<String>,
    date: Option<serde_yaml::Value>,
    topic: Option<String>,
    depth: Option<serde_yaml::Value>,
    #[serde(default)]
    tags: Vec<String>,
}

impl From<RawFrontmatter> for Frontmatter {
    fn from(raw: RawFrontmatter) -> Self {
        let date = raw.date.map(|v| match v {
            serde_yaml::Value::String(s) => s,
            other => {
                // serde_yaml may parse a bare date like `2026-04-10` as a
                // string already, but just in case it comes through as
                // something else we convert via Display / Debug.
                let s = format!("{other:?}");
                // Try to extract a clean string representation
                s.trim_matches('"').to_string()
            }
        });

        let depth = raw.depth.map(|v| match v {
            serde_yaml::Value::String(s) => s,
            serde_yaml::Value::Number(n) => n.to_string(),
            other => format!("{other:?}"),
        });

        Frontmatter {
            title: raw.title,
            date,
            topic: raw.topic,
            depth,
            tags: raw.tags,
        }
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Parse a Markdown document into frontmatter + heading-based chunks.
///
/// The parser:
/// 1. Extracts YAML frontmatter delimited by `---` at the start of the file.
/// 2. Splits the body on `## ` and `### ` headings.
/// 3. Excludes [`DEFAULT_EXCLUDED_HEADINGS`] sections (substring match).
/// 4. Merges short chunks (< 50 chars of content) into the previous chunk.
/// 5. Re-indexes chunks sequentially starting from 0.
///
/// Use [`parse_with_excludes`] if you need a custom exclude list (loaded from
/// `kb-mcp.toml` via `exclude_headings`).
pub fn parse(raw: &str) -> ParsedDocument {
    parse_with_excludes(raw, DEFAULT_EXCLUDED_HEADINGS)
}

/// Same as [`parse`] but lets the caller supply the list of heading patterns
/// to exclude from chunk output. A heading is excluded when it *contains*
/// any entry in `excludes` as a substring (so short patterns like
/// `"次の深堀り候補"` match `"## 次の深堀り候補"`, `"### 次の深堀り候補 (案)"`
/// etc.). Empty list means "nothing is excluded".
pub fn parse_with_excludes(raw: &str, excludes: &[impl AsRef<str>]) -> ParsedDocument {
    let (frontmatter, body) = extract_frontmatter(raw);
    let excludes: Vec<&str> = excludes.iter().map(AsRef::as_ref).collect();
    let chunks = chunk_body(&body, &excludes);

    ParsedDocument {
        frontmatter,
        chunks,
        raw_content: raw.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Frontmatter extraction
// ---------------------------------------------------------------------------

fn extract_frontmatter(raw: &str) -> (Frontmatter, String) {
    let trimmed = raw.trim_start_matches('\u{feff}'); // strip BOM if present

    if !trimmed.starts_with("---") {
        return (Frontmatter::default(), trimmed.to_string());
    }

    // Find the closing `---` (must be after the first line).
    let after_first = &trimmed[3..];
    let after_first = after_first.trim_start_matches('\r'); // handle \r\n
    let after_first = after_first.strip_prefix('\n').unwrap_or(after_first);

    if let Some(end) = after_first.find("\n---") {
        let yaml_raw = &after_first[..end];
        let body_start = end + 4; // skip the `\n---`
        let rest = &after_first[body_start..];
        // Trim the leading newline(s) after the closing ---
        let body = rest.trim_start_matches(['\r', '\n']).to_string();

        // Windows 生成の `.md` で `\r\n` 改行のとき、yaml_raw 各行末に `\r` が
        // 残ってしまい serde_yaml のパース結果の文字列 value にリークする
        // ことがある (例: `title: "Foo"` が `"Foo\r"` になる)。パース前に
        // CRLF → LF へ正規化しておく。
        let yaml_normalized;
        let yaml_str: &str = if yaml_raw.contains('\r') {
            yaml_normalized = yaml_raw.replace("\r\n", "\n").replace('\r', "\n");
            &yaml_normalized
        } else {
            yaml_raw
        };

        let fm = match serde_yaml::from_str::<RawFrontmatter>(yaml_str) {
            Ok(raw_fm) => Frontmatter::from(raw_fm),
            Err(e) => {
                eprintln!("warning: failed to parse YAML frontmatter: {e}");
                Frontmatter::default()
            }
        };

        (fm, body)
    } else {
        // No closing --- found; treat the whole file as body.
        (Frontmatter::default(), trimmed.to_string())
    }
}

// ---------------------------------------------------------------------------
// Heading-based chunking
// ---------------------------------------------------------------------------

/// Section headings excluded by default when the caller does not override.
/// Matching is substring-based inside [`chunk_body`].
pub const DEFAULT_EXCLUDED_HEADINGS: &[&str] = &["次の深堀り候補"];

fn chunk_body(body: &str, excludes: &[&str]) -> Vec<Chunk> {
    let mut raw_chunks: Vec<(Option<String>, String)> = Vec::new();
    let mut current_heading: Option<String> = None;
    let mut current_lines: Vec<&str> = Vec::new();
    let mut excluded = false;

    for line in body.lines() {
        if let Some(heading_text) = strip_heading(line) {
            // Check if this heading is excluded.
            if excludes.iter().any(|ex| heading_text.contains(ex)) {
                // Flush accumulated lines for the *previous* section first.
                if !excluded {
                    let content = current_lines.join("\n").trim().to_string();
                    if !content.is_empty() || current_heading.is_some() {
                        raw_chunks.push((current_heading.clone(), content));
                    }
                }
                excluded = true;
                current_lines.clear();
                // excluded モードでは以降の行を flush しないので heading 名
                // の保持は不要 (evaluator 指摘: 無意味な代入を除去)。
                current_heading = None;
                continue;
            }

            // Not excluded — flush previous section.
            if !excluded {
                let content = current_lines.join("\n").trim().to_string();
                if !content.is_empty() || current_heading.is_some() {
                    raw_chunks.push((current_heading.clone(), content));
                }
            }

            excluded = false;
            current_heading = Some(heading_text);
            current_lines.clear();
        } else if !excluded {
            current_lines.push(line);
        }
    }

    // Flush the last section.
    if !excluded {
        let content = current_lines.join("\n").trim().to_string();
        if !content.is_empty() || current_heading.is_some() {
            raw_chunks.push((current_heading, content));
        }
    }

    // Merge short chunks (< 50 chars) into the previous chunk.
    let mut merged: Vec<(Option<String>, String)> = Vec::new();
    for (heading, content) in raw_chunks {
        if content.len() < 50 && !merged.is_empty() {
            // Append to the previous chunk.
            let prev = merged.last_mut().unwrap();
            if !prev.1.is_empty() {
                prev.1.push_str("\n\n");
            }
            if let Some(ref h) = heading {
                prev.1.push_str(&format!("## {h}\n\n"));
            }
            prev.1.push_str(&content);
        } else {
            merged.push((heading, content));
        }
    }

    // Re-index sequentially.
    merged
        .into_iter()
        .enumerate()
        .map(|(i, (heading, content))| Chunk {
            index: i,
            heading,
            content,
        })
        .collect()
}

/// If the line is a `## ` or `### ` heading, return the heading text.
fn strip_heading(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if let Some(rest) = trimmed.strip_prefix("### ") {
        Some(rest.trim().to_string())
    } else { trimmed.strip_prefix("## ").map(|rest| rest.trim().to_string()) }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn fixture(name: &str) -> String {
        let base = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name);
        std::fs::read_to_string(&base)
            .unwrap_or_else(|e| panic!("Failed to read fixture {}: {e}", base.display()))
    }

    #[test]
    fn test_parse_frontmatter() {
        let doc = parse(&fixture("sample.md"));
        assert_eq!(doc.frontmatter.title.as_deref(), Some("MCP プロトコル概要"));
        assert_eq!(doc.frontmatter.tags, vec!["mcp", "protocol", "overview"]);
        assert_eq!(doc.frontmatter.date.as_deref(), Some("2026-04-10"));
    }

    #[test]
    fn test_crlf_frontmatter_values_have_no_trailing_cr() {
        // Windows 生成の `.md` を想定した CRLF 改行付きの入力。
        // 正規化を入れないと serde_yaml の string value に `\r` がリークする。
        let crlf = "---\r\n\
                    title: \"CRLF Title\"\r\n\
                    topic: mcp\r\n\
                    tags:\r\n\
                      - a\r\n\
                      - b\r\n\
                    ---\r\n\
                    \r\n\
                    # Body\r\n\
                    Some content.\r\n";
        let doc = parse(crlf);
        let title = doc.frontmatter.title.as_deref().unwrap_or("");
        assert!(!title.contains('\r'), "title must not retain CR: {title:?}");
        assert_eq!(title, "CRLF Title");
        assert_eq!(doc.frontmatter.topic.as_deref(), Some("mcp"));
        assert!(!doc.frontmatter.tags.iter().any(|t| t.contains('\r')));
        assert_eq!(doc.frontmatter.tags, vec!["a", "b"]);
    }

    #[test]
    fn test_no_frontmatter() {
        let doc = parse(&fixture("no_frontmatter.md"));
        assert!(doc.frontmatter.title.is_none());
        assert!(doc.frontmatter.date.is_none());
        assert!(doc.frontmatter.tags.is_empty());
        // Should still produce chunks from the body.
        assert!(!doc.chunks.is_empty());
    }

    #[test]
    fn test_chunking_excludes_next_candidates() {
        let doc = parse(&fixture("sample.md"));
        for chunk in &doc.chunks {
            if let Some(ref heading) = chunk.heading {
                assert!(
                    !heading.contains("次の深堀り候補"),
                    "Excluded heading '次の深堀り候補' should not appear in chunks"
                );
            }
            assert!(
                !chunk.content.contains("OAuth 2.1"),
                "Content under '次の深堀り候補' should not appear in any chunk"
            );
        }
    }

    #[test]
    fn test_parse_with_empty_excludes_keeps_everything() {
        // 空配列を渡すと「次の深堀り候補」もチャンクとして残ることを確認。
        // kb-mcp.toml で `exclude_headings = []` を指定した場合の挙動。
        let empty: &[&str] = &[];
        let doc = parse_with_excludes(&fixture("sample.md"), empty);
        let has_next_heading = doc
            .chunks
            .iter()
            .any(|c| c.heading.as_deref() == Some("次の深堀り候補"));
        assert!(
            has_next_heading,
            "With empty excludes, '次の深堀り候補' section should be present"
        );
    }

    #[test]
    fn test_parse_with_custom_excludes() {
        // デフォルトの除外リストを上書きして「参考リンク」見出しを除外する。
        // 同時に、既定で除外されていた「次の深堀り候補」は上書き後のリストに
        // 含まれないのでチャンクとして残ることも確認する。
        let md = "\
# タイトル

## 概要

本文 1 を ある程度 十分な長さで 書く必要がある ので埋める埋める埋める。

## 参考リンク

リンク集 本文 十分な長さで 書く必要がある ので埋める埋める埋める。

## 次の深堀り候補

候補 本文 十分な長さで 書く必要がある ので埋める埋める埋める。
";
        let doc = parse_with_excludes(md, &["参考リンク"]);
        let headings: Vec<Option<&str>> = doc
            .chunks
            .iter()
            .map(|c| c.heading.as_deref())
            .collect();
        assert!(
            !headings.contains(&Some("参考リンク")),
            "custom excluded heading should not appear: {headings:?}"
        );
        assert!(
            headings.contains(&Some("次の深堀り候補")),
            "previously-default excluded heading should now appear: {headings:?}"
        );
    }

    #[test]
    fn test_chunking_produces_multiple_chunks() {
        let doc = parse(&fixture("sample.md"));
        assert!(
            doc.chunks.len() >= 2,
            "Expected at least 2 chunks, got {}",
            doc.chunks.len()
        );
        // Verify sequential indexing.
        for (i, chunk) in doc.chunks.iter().enumerate() {
            assert_eq!(chunk.index, i, "Chunk index mismatch at position {i}");
        }
    }

    #[test]
    fn test_frontmatter_extraction() {
        let doc = parse(&fixture("sample.md"));
        assert_eq!(doc.frontmatter.title.as_deref(), Some("MCP プロトコル概要"));
        assert_eq!(doc.frontmatter.topic.as_deref(), Some("mcp"));
        assert_eq!(doc.frontmatter.depth.as_deref(), Some("1"));
    }
}
