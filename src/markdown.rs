//! Backwards-compatibility shim for pre-feature-20 code paths.
//!
//! The real Markdown parser now lives in [`crate::parser::markdown`] as an
//! implementation of [`crate::parser::Parser`]. This module re-exports the
//! data types and provides `parse()` / `parse_with_excludes()` free-functions
//! so tests and any lingering callers keep working unchanged.
//!
//! Indexing / server code paths have migrated to the `Registry`-based API.

pub use crate::parser::{
    Chunk, Frontmatter, ParsedDocument, DEFAULT_EXCLUDED_HEADINGS,
};

use crate::parser::{MarkdownParser, Parser};

/// Parse Markdown using the default exclude list.
///
/// Retained for test fixtures written against the pre-feature-20 API. New
/// callers should go through `Registry::by_extension("md").parse(...)`.
pub fn parse(raw: &str) -> ParsedDocument {
    parse_with_excludes(raw, DEFAULT_EXCLUDED_HEADINGS)
}

/// Same as [`parse`] but with a custom exclude list (substring match on
/// heading text).
pub fn parse_with_excludes(
    raw: &str,
    excludes: &[impl AsRef<str>],
) -> ParsedDocument {
    let excludes: Vec<&str> = excludes.iter().map(AsRef::as_ref).collect();
    // path_hint is unused by the Markdown parser (only `.txt` and similar
    // use it for filename-derived titles), so pass an empty string.
    MarkdownParser.parse(raw, "", &excludes)
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
