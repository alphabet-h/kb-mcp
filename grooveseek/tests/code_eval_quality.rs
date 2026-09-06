//! Retrieval quality gate over the committed `kb-code-eval` fixture corpus (AV-15).
//!
//! The twenty prose documents behind `tests/eval_corpus_quality.rs` measure whether a
//! *document* stays findable. Nothing measured whether a *definition* does, so a change
//! to how source files are cut into chunks — the definition walk, the gap filling around
//! it, the chunk budget — could not move a number anywhere in this repository. This file
//! is that number.
//!
//! # How a golden can see a chunking regression at all
//!
//! [`grooveseek::eval::is_hit`] compares the path, and when a golden entry also carries a
//! `heading` it compares that too; an entry with a heading is **never** satisfied by a
//! chunk that has none. A code chunk is headed `"<kind> <name>"`
//! ([`grooveseek::parser::Chunk::heading`]), so an entry scoped to `method push` stops
//! being satisfiable the moment `ring.rs` stops being cut one definition at a time — even
//! though the file itself still ranks first, which is all a path-only entry can ask.
//! Ten of the fifteen queries are scoped that way and no other golden in this tree uses
//! the field at all.
//!
//! # Why a second corpus instead of five more files in `kb-eval`
//!
//! Mixing was tried first, and measured. Five source files inside `kb-eval` moved four of
//! that gate's six floors to one query of headroom or less and its BGE-M3 multi-answer
//! floor to none: prose recall@1 went 0.920 to 0.880 on BGE-small and 1.000 to 0.960 on
//! BGE-M3, and multi-answer recall@5 went 1.000 to 0.900 against a floor of 0.90.
//!
//! The cause is not the fixtures, which is why no amount of rewriting them helped.
//! Reciprocal-rank fusion scores a chunk by its **position in each leg's candidate list**,
//! so a document added anywhere shifts every position below it and re-rolls every near-tie
//! in the corpus. Adding one chunk — a single definition-free file on an unrelated subject
//! — was enough to flip `en-restore-pitr`, whose top six held no code chunk at all and
//! whose rank-1 margin in the prose-only run is 0.00019. Two rounds of fixture rewriting
//! did fix the cases where a code chunk genuinely outranked prose under BGE-M3, and the
//! headroom still did not come back.
//!
//! So the corpora are separate, and `kb-eval`'s recorded baseline goes on measuring what
//! it was measured over.
//!
//! # Layers
//!
//! [`crate::kb_code_eval_corpus_and_golden_stay_in_sync`] and
//! [`crate::kb_code_eval_fixtures_chunk_at_the_headings_the_golden_names`] need no model
//! and run in the ordinary `cargo test` (= the PR gate). The second is the more useful of
//! the two here: `groove eval` can only say a query stopped ranking first, while that one
//! says the definition a golden entry names stopped being a chunk, which is the thing this
//! corpus is about — and it says it on the pull request rather than a day later.
//!
//! The two `#[ignore]` tests do the retrieval and are picked up by `nightly.yml`. The
//! BGE-M3 one is in that workflow's skip list for the windows and macOS legs, alongside
//! the prose gate's; the skip is a substring match and this test's name is not a superset
//! of that one's, so it needed its own entry.
//!
//! # Baseline, measured 2026-09-07 (15 queries, 9 documents, 21 chunks)
//!
//! Every query names exactly one document, so there is one population and one pair of
//! floors. Both models answer every query at rank 1 as shipped.
//!
//! | | recall@1 | recall@5 | MRR |
//! |---|---|---|---|
//! | BGE-small, as shipped | 1.000 | 1.000 | 1.000 |
//! | BGE-small, FTS leg silent | 0.600 | 0.933 | 0.728 |
//! | BGE-small, vector leg silent | 1.000 | 1.000 | 1.000 |
//! | BGE-small, definition structure lost | 0.267 | 0.333 | 0.300 |
//! | BGE-small, gap filling dead | 0.667 | 0.800 | 0.722 |
//! | BGE-small, chunk budget 3500 -> 800 | 0.800 | 0.800 | 0.800 |
//! | BGE-M3, as shipped | 1.000 | 1.000 | 1.000 |
//! | BGE-M3, FTS leg silent | 0.800 | 0.867 | 0.841 |
//! | BGE-M3, vector leg silent | 1.000 | 1.000 | 1.000 |
//! | BGE-M3, definition structure lost | 0.333 | 0.333 | 0.333 |
//! | BGE-M3, gap filling dead | 0.733 | 0.800 | 0.767 |
//! | BGE-M3, chunk budget 3500 -> 800 | 0.800 | 0.800 | 0.800 |
//!
//! The broken rows come from scratch builds, one edit each. Three of the five are in
//! [`grooveseek::parser::code`]: `Bounds::SHIPPED.scope_depth` set to 0, so every source
//! file takes the too-deep fallback and is chunked by lines with no heading anywhere;
//! `fill_gaps` returning immediately; and `DEFAULT_MAX_CHUNK_CHARS` cut from 3500 to 800.
//! The other two silence a retrieval leg, both under [`grooveseek::db`]: the MATCH
//! expression built by `ParsedQuery::match_expr` returning `None`, and
//! `search_split_candidates` returning an empty vector-leg list. Each of the five is
//! private to the module it sits in, so the module is what is linked and the item stays in
//! prose.
//!
//! Five conclusions are baked into the thresholds below:
//!
//! 1. **Losing the definition walk is the largest signal this gate has**, 1.000 down to
//!    0.267 / 0.333. Ten heading-scoped entries stop being satisfiable at once while the
//!    files themselves still rank first, which is exactly the state a path-only golden
//!    reports as healthy. The failure log shows it as the right path winning under no
//!    heading, and [`crate::winning_heading_report`] is what prints that.
//! 2. **Gap filling has its own detector and it is deterministic.** `src/glyphs.rs` holds
//!    no definitions, so every chunk it has comes out of the gap filling named above; with
//!    that dead the file yields no chunks, the indexer drops the document before the
//!    database, and its three queries miss together. The run also reports one skipped
//!    document.
//! 3. **The chunk budget needed three queries, not one.** A container that fits the budget
//!    is emitted whole and its nested definitions produce no chunks of their own, so
//!    `module width` is a heading that exists only while the budget holds it together.
//!    One entry scoped to it would move the mean by 1/15 and never trip a floor set two
//!    queries below a clean sweep; three trip it together, 1.000 down to 0.800.
//! 4. **BGE-M3 is not blind to the keyword leg here, and on `kb-eval` it is.** The prose
//!    gate records BGE-M3 answering all of its queries with the full-text leg dead, because
//!    twenty semantically distinct documents are separable by the vector leg alone. Sibling
//!    definitions of one type are not: `method push` and `method snapshot` share a struct,
//!    a vocabulary and a path. BGE-M3 loses three queries here, so this corpus gives the
//!    model an FTS regression detector it does not otherwise have.
//! 5. **Neither model notices the vector leg dying**, 1.000 in every column. Every query
//!    is written to a sentence with distinctive wording, and the trigram leg finds all of
//!    them alone. The prose gate is what covers that direction; this one does not, and the
//!    row is recorded rather than left out so nobody re-measures to discover it.
//!
//! Thresholds allow **two queries of drift and trip on the third**, the same rule and the
//! same reason as the prose gate: reciprocal-rank fusion runs on `f32` and near-ties can
//! come apart differently on another architecture. Over a clean sweep of fifteen, one
//! query is 1/15 in both metrics, so both floors are 1.000 - 2/15 truncated to 0.86. Every
//! broken state above sits below that except the two vector-leg rows, which sit at the
//! ceiling.

use std::path::PathBuf;

mod common;
use common::eval_gate as gate;
use common::temp::TempKbLayout;

use grooveseek::eval::{GoldenSet, QueryResult, aggregate_metrics};

/// Every file under `tests/fixtures/kb-code-eval/`, relative to that directory and spelled
/// with `/` the way the indexer stores paths.
///
/// Listed by hand so that adding, renaming or deleting a fixture has to be a deliberate
/// edit here as well. Without it, deleting a document silently turns its query into a
/// permanent miss and the corpus quietly shrinks.
const KB_CODE_EVAL_FILES: &[&str] = &[
    "docs/conventions.md",
    "docs/overview.md",
    "src/align.rs",
    "src/checksum.rs",
    "src/clock.rs",
    "src/duration.rs",
    "src/glyphs.rs",
    "src/ring.rs",
    "src/tabular.rs",
];

/// The parser ids this gate turns on beyond Markdown.
///
/// One list, read twice: [`crate::common::eval_gate::pinned_config`] writes it into
/// `[parsers].enabled`, and the sync test checks no fixture has an extension outside it.
/// Two lists would let a fixture be added that the pinned config cannot index — a
/// permanent miss that every count in this file would still report as satisfied.
const CODE_EXTENSIONS: &[&str] = &["rs"];

/// Number of queries in `tests/fixtures/kb-code-eval-golden.yml`. Pinned because both
/// metrics are averages over it: dropping the hard half of the golden would raise them and
/// read as an improvement.
const GOLDEN_QUERY_COUNT: usize = 15;

/// How many of those carry a `heading`.
///
/// This is the pin that matters most in this file. Every other count stays satisfied if a
/// future edit deletes the `heading:` lines and leaves fifteen path-only queries — and
/// that golden would pass while measuring nothing this corpus exists for, because a file
/// reduced to headingless line chunks still answers a path-only query.
const MIN_HEADING_SCOPED_QUERIES: usize = 10;

/// Minimums rather than exact counts, so the corpus can grow without editing them, but
/// neither half can drain away. The prose half is not decoration: it is the cross-modal
/// near miss, the thing that wins when a definition stops being retrievable, and the
/// failure log naming a `.md` file is how that reads at a glance.
const MIN_CODE_DOCS: usize = 7;
const MIN_PROSE_DOCS: usize = 2;

/// Floors. Baseline is a clean sweep on both models (module docs), so one query of drift
/// is 1/15 in both metrics and two of them put the floors here. Every deliberate breakage
/// measured sits below them except the vector-leg rows, which no threshold could separate
/// from a healthy run.
const MIN_RECALL_AT_1: f64 = 0.86;
const MIN_MRR: f64 = 0.86;

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

fn corpus_root() -> PathBuf {
    gate::fixtures_root().join("kb-code-eval")
}

/// The golden lives *beside* the corpus, not inside it, so that editing the query set can
/// never change the document set being measured.
fn golden_file() -> PathBuf {
    gate::fixtures_root().join("kb-code-eval-golden.yml")
}

fn corpus_files() -> Vec<String> {
    gate::assert_corpus_matches(
        &corpus_root(),
        KB_CODE_EVAL_FILES,
        "kb-code-eval",
        "tests/code_eval_quality.rs",
    )
}

fn setup_corpus(layout: &TempKbLayout) {
    let files = corpus_files();
    gate::copy_corpus(&corpus_root(), &files, layout.kb());
}

/// The pinned configuration, which for this corpus has to name the Rust parser:
/// `[parsers].enabled` defaults to `["md"]`, so without this line the seven source files
/// would be copied into the knowledge base and never indexed — every code query a
/// permanent miss, and every count in this file still satisfied.
fn pinned_config(layout: &TempKbLayout) -> PathBuf {
    let mut parsers = vec!["md"];
    parsers.extend_from_slice(CODE_EXTENSIONS);
    gate::pinned_config(layout.root(), &parsers)
}

fn extension_of(path: &str) -> &str {
    path.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("")
}

fn is_code_path(path: &str) -> bool {
    CODE_EXTENSIONS.contains(&extension_of(path))
}

// ---------------------------------------------------------------------------
// Reports
// ---------------------------------------------------------------------------

/// For every query that missed rank 1, the heading the winning chunk carried.
///
/// [`crate::common::eval_gate::ranking_report`] names the winning *path*, which is the
/// whole answer for a corpus of prose documents and only half of it here. The state this
/// gate exists to catch leaves the right file at rank 1 and takes its heading away, so a
/// report without this line reads as "the right document won" on the exact run that
/// should be alarming.
fn winning_heading_report(all: &[QueryResult]) -> String {
    let mut report = String::new();
    for q in all {
        if q.metrics.reciprocal_rank >= 1.0 {
            continue;
        }
        // A query whose retrieved window came back empty is the loudest state this report
        // can be asked about, and skipping it would be the quietest thing to print. If
        // every miss looked like that -- indexing dropped the whole corpus, say -- the
        // line below would never run and the fallback text would tell the reader that
        // every query ranked its expected chunk first, on the same failure that just said
        // otherwise in numbers.
        let Some(top) = q.top_k.first() else {
            report.push_str(&format!("  {}: nothing was returned at all\n", q.id));
            continue;
        };
        let heading = match &top.heading {
            Some(h) => format!("under {h:?}"),
            None => {
                "under no heading at all (the file is not being cut into definitions)".to_string()
            }
        };
        report.push_str(&format!("  {}: {} won {heading}\n", q.id, top.path));
    }
    if report.is_empty() {
        report.push_str("  (every query ranked its expected chunk first)\n");
    }
    report
}

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

fn assert_retrieval_quality(run: &serde_json::Value, model: &str) {
    let query_count = gate::metric(run, "/aggregate/query_count") as usize;
    assert_eq!(
        query_count, GOLDEN_QUERY_COUNT,
        "{model}: eval measured {query_count} queries but the golden holds \
         {GOLDEN_QUERY_COUNT}; the averages below are not comparable to the \
         recorded baseline"
    );

    // Through production's averaging rather than a second spelling of it, so that a later
    // change to which queries are eligible or how a missing metric is treated moves this
    // gate and `groove eval` alike instead of silently parting them. Unlike the prose
    // gate this file can use one call: every query here names exactly one document, so
    // there is a single population and `aggregate` would have been the same number.
    let all = gate::per_query(run);
    let ks = gate::k_values(run);
    let metrics = aggregate_metrics(&all, &ks);

    let recall_at_1 = gate::at_k(&metrics, 1, "code");
    let recall_at_5 = gate::at_k(&metrics, 5, "code");
    let mrr = metrics.mrr;

    let context = format!(
        "{model} over the kb-code-eval corpus ({query_count} queries)\n  \
         recall@1={recall_at_1:.3} recall@5={recall_at_5:.3} MRR={mrr:.3}\n\
         queries that missed rank 1:\n{}\
         what won instead:\n{}",
        gate::ranking_report(&all),
        winning_heading_report(&all)
    );

    assert!(
        recall_at_1 >= MIN_RECALL_AT_1,
        "code retrieval quality regressed: recall@1 {recall_at_1:.3} < {MIN_RECALL_AT_1:.3}\n{context}"
    );
    assert!(
        mrr >= MIN_MRR,
        "code retrieval quality regressed: MRR {mrr:.3} < {MIN_MRR:.3}\n{context}"
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Structural check only — no embedding model, so this one gates pull requests along with
/// the rest of the light suite.
#[test]
fn kb_code_eval_corpus_and_golden_stay_in_sync() {
    let files = corpus_files();

    for file in &files {
        let ext = extension_of(file);
        assert!(
            ext == "md" || CODE_EXTENSIONS.contains(&ext),
            "the kb-code-eval corpus holds {file}, whose extension the pinned config does \
             not enable, so it would be copied into the knowledge base and never indexed"
        );
    }

    let code_docs = files.iter().filter(|f| is_code_path(f)).count();
    assert!(
        code_docs >= MIN_CODE_DOCS,
        "{code_docs} source files in the kb-code-eval corpus, expected at least \
         {MIN_CODE_DOCS}"
    );
    let prose_docs = files.len() - code_docs;
    assert!(
        prose_docs >= MIN_PROSE_DOCS,
        "{prose_docs} prose documents in the kb-code-eval corpus, expected at least \
         {MIN_PROSE_DOCS}; they are the cross-modal near miss, and without them a lost \
         definition is replaced in the failure log by another definition rather than by \
         something that names the loss"
    );

    let golden = GoldenSet::load(&golden_file()).expect("load the kb-code-eval golden");
    assert_eq!(
        golden.queries.len(),
        GOLDEN_QUERY_COUNT,
        "GOLDEN_QUERY_COUNT is stale; re-measure the baseline in this file's module docs \
         after changing the query set"
    );

    let mut ids: Vec<&str> = Vec::with_capacity(golden.queries.len());
    let mut covered: Vec<&str> = Vec::new();
    let mut heading_scoped = 0usize;
    for q in &golden.queries {
        let id = q.id.as_deref().unwrap_or_else(|| {
            panic!("every kb-code-eval golden query needs an id (offending query: {q:?})")
        });
        ids.push(id);
        assert_eq!(
            q.expected.len(),
            1,
            "golden query {id} names {} documents; this corpus has one population and its \
             floors are stated per query, which assumes one",
            q.expected.len()
        );
        for hit in &q.expected {
            assert!(
                files.iter().any(|f| f == &hit.path),
                "golden query {id} expects {}, which is not in the kb-code-eval corpus. \
                 Every expected path is compared verbatim against the indexed path, so a \
                 typo here is a permanent miss rather than an error.",
                hit.path
            );
            if hit.heading.is_some() {
                assert!(
                    is_code_path(&hit.path),
                    "golden query {id} scopes a heading to {}, which is not a source file. \
                     Only a code chunk is headed by the definition it holds; a Markdown \
                     heading is the document's own text and pinning it here would be \
                     measuring the fixture's prose rather than the chunker.",
                    hit.path
                );
                heading_scoped += 1;
            }
            covered.push(&hit.path);
        }
    }

    let mut sorted_ids = ids.clone();
    sorted_ids.sort_unstable();
    sorted_ids.dedup();
    assert_eq!(
        sorted_ids.len(),
        ids.len(),
        "golden query ids must be unique: {ids:?}"
    );

    let uncovered: Vec<&String> = files
        .iter()
        .filter(|f| !covered.iter().any(|c| c == &f.as_str()))
        .collect();
    assert!(
        uncovered.is_empty(),
        "these kb-code-eval documents are not the expected answer of any golden query, so \
         nothing would notice if they stopped being retrievable: {uncovered:?}"
    );

    assert!(
        heading_scoped >= MIN_HEADING_SCOPED_QUERIES,
        "{heading_scoped} golden queries name a heading, expected at least \
         {MIN_HEADING_SCOPED_QUERIES}. A path-only golden cannot see a source file lose \
         its definition structure: the file goes on ranking first out of whatever chunks \
         it was reduced to, and this whole corpus stops measuring anything"
    );
}

/// The headings the golden names, taken from the parser rather than from a run.
///
/// `groove eval` can only report that a query stopped ranking first, and it needs a model
/// and a nightly to say even that. This says the definition a golden entry names stopped
/// being a chunk, it says it on the pull request, and it also catches a fixture that has
/// quietly stopped being valid Rust — which nothing else would, because files under
/// `tests/fixtures/` belong to no cargo target and neither `cargo fmt` nor `cargo clippy`
/// reaches them.
///
/// Gated on the feature so that a `--no-default-features` build still compiles this crate:
/// without the compiled-in grammar there is no `rs` parser to ask.
#[cfg(feature = "grammar-rust")]
#[test]
fn kb_code_eval_fixtures_chunk_at_the_headings_the_golden_names() {
    use grooveseek::parser::{ParserExt, Registry};

    let owned: Vec<String> = CODE_EXTENSIONS.iter().map(|s| (*s).to_string()).collect();
    let registry = Registry::from_enabled(&owned).expect("the code parsers build");

    let parse = |rel: &str| {
        let ext = extension_of(rel);
        let parser = registry
            .by_extension(ext)
            .unwrap_or_else(|| panic!("no parser registered for {ext}"));
        let bytes = std::fs::read(corpus_root().join(rel))
            .unwrap_or_else(|e| panic!("read fixture {rel}: {e}"));
        parser
            .parse_bytes(&bytes, rel, &[])
            .unwrap_or_else(|e| panic!("parse fixture {rel}: {e}"))
    };

    let golden = GoldenSet::load(&golden_file()).expect("load the kb-code-eval golden");
    for q in &golden.queries {
        let id = q.id.as_deref().unwrap_or("<no id>");
        for hit in &q.expected {
            let Some(wanted) = hit.heading.as_deref() else {
                continue;
            };
            let doc = parse(&hit.path);
            let headings: Vec<&str> = doc
                .chunks
                .iter()
                .filter_map(|c| c.heading.as_deref())
                .collect();
            assert!(
                headings.contains(&wanted),
                "golden query {id} expects {} under heading {wanted:?}, but parsing that \
                 fixture produces {headings:?}. Either the fixture changed shape or the \
                 chunker stopped cutting it one definition at a time; the retrieval gate \
                 would have reported this a day later as a recall drop.",
                hit.path
            );
        }
    }

    // The gap-fill detector, checked from the other side. `src/glyphs.rs` earns its place
    // in the golden by having no definitions at all, so that killing gap filling empties
    // it and the indexer drops the document. A grammar bump that started tagging `const`
    // would quietly turn it into a definition detector, and the three queries pointed at
    // it would go on passing while measuring something else.
    let glyphs = parse("src/glyphs.rs");
    assert!(
        !glyphs.chunks.is_empty(),
        "src/glyphs.rs produced no chunks, so the three golden queries pointed at it can \
         never be satisfied"
    );
    let headed: Vec<&str> = glyphs
        .chunks
        .iter()
        .filter_map(|c| c.heading.as_deref())
        .collect();
    assert!(
        headed.is_empty(),
        "src/glyphs.rs is the gap-fill detector and must hold no definitions, but parsing \
         it produced chunks headed {headed:?}. Every chunk it has is supposed to come out \
         of fill_gaps, which is what makes killing gap filling empty the file"
    );
}

/// The keyword-sensitive leg. BGE-small runs on every nightly OS.
#[test]
#[ignore = "indexes the kb-code-eval corpus with BGE-small (~130 MB model download on first run)"]
fn kb_code_eval_retrieval_quality_bge_small() {
    let layout = TempKbLayout::new("groove-code-eval-quality-small");
    setup_corpus(&layout);
    let config = pinned_config(&layout);

    gate::index_corpus(layout.kb(), &config, "bge-small-en-v1.5");
    let run = gate::run_eval(layout.kb(), &config, &golden_file(), "bge-small-en-v1.5");

    assert_retrieval_quality(&run, "bge-small-en-v1.5");
}

/// The model a real knowledge base runs. Skipped on the Windows and macOS nightly legs for
/// the same disk and cache reasons as the other BGE-M3 tests — see the `SKIP` block in
/// `.github/workflows/nightly.yml`, which needed a line of its own because the skip is a
/// substring match and this name does not contain the prose gate's.
#[test]
#[ignore = "indexes the kb-code-eval corpus with BGE-M3 (~2.3 GB model download on first run)"]
fn kb_code_eval_retrieval_quality_bge_m3() {
    let layout = TempKbLayout::new("groove-code-eval-quality-m3");
    setup_corpus(&layout);
    let config = pinned_config(&layout);

    gate::index_corpus(layout.kb(), &config, "bge-m3");
    let run = gate::run_eval(layout.kb(), &config, &golden_file(), "bge-m3");

    assert_retrieval_quality(&run, "bge-m3");
}
