//! (feature-56) The Rust grammar, compiled into `groove` itself.
//!
//! Rust is the one language shipped this way. It is the language groove is written in, so a
//! user pointing groove at this repository gets code search without placing a single extra
//! file — and it is the language whose grammar was measured, at just over a megabyte of
//! binary, small enough that everyone can carry it whether they index code or not.
//!
//! Every other language arrives as a separate library the user chooses to put in place. That
//! asymmetry is the decision recorded in ADR-0013, not an accident of what was implemented
//! first.

use std::sync::Arc;

use anyhow::Result;
use groove_grammar_abi::GrammarDescriptor;
use tree_sitter::Language;

use super::LoadedGrammar;

/// The compiled-in Rust grammar, in the same shape a plugin would hand over.
///
/// Held to the same two rules the loader in [`super::plugin`] applies to a plugin's declared
/// name and extension — [`groove_grammar_abi::extension_is_valid`] and
/// [`groove_grammar_abi::name_is_valid`] — but applied at a different time, because that is when
/// each path's values first exist. A plugin's arrive as strings from native code groove has just
/// opened, so they are checked at load. These are literals in this workspace, so they are checked
/// while groove is built, by the `const` items below: a compiled-in grammar that would be refused
/// from a plugin fails to compile. The rules themselves are written once, so tightening either
/// one tightens both paths, and neither side carries a guard nothing could trip.
///
/// That is the one direction these two could have drifted. The rules live in a crate this one
/// depends on, so nothing over there can see this descriptor, and the grammar nobody has to place
/// would have been the one that quietly stopped satisfying them.
///
/// The extension is not decoration either: [`crate::parser::registry`] registers this parser
/// under the extension declared here, which is what makes getting it wrong the compiled-in twin
/// of the mismatch the loader refuses a plugin for.
///
/// **There is deliberately no tree-sitter ABI check on this side.** The loader has one because a
/// plugin is built and shipped apart from groove, so a grammar from a CLI this runtime cannot
/// speak is a state a user can reach. `tree_sitter_rust` is resolved by the same `Cargo.lock` as
/// the runtime, so the only way this pair comes apart is a dependency bump — and a language
/// outside the range this runtime speaks is refused by `Query::new` where [`LoadedGrammar::new`]
/// compiles the tags query, which makes it a grammar this build cannot construct rather than a
/// grammar that reads Rust wrongly.
pub(crate) const DESCRIPTOR: GrammarDescriptor = GrammarDescriptor {
    name: "rust",
    extension: "rs",
    language: tree_sitter_rust::LANGUAGE,
    tags_query: tree_sitter_rust::TAGS_QUERY,
};

// The rules above, applied. A build with `grammar-rust` off does not compile this module at all,
// so it has no compiled-in grammar and nothing here to check.
const _: () = assert!(
    DESCRIPTOR.extension_is_valid(),
    "the compiled-in grammar declares a file extension groove would refuse from a plugin"
);
const _: () = assert!(
    DESCRIPTOR.name_is_valid(),
    "the compiled-in grammar declares a language name groove would refuse from a plugin"
);

/// Build the grammar, validating its tags query the same way a plugin's would be.
pub(crate) fn grammar() -> Result<Arc<LoadedGrammar>> {
    let language = Language::from(DESCRIPTOR.language);
    LoadedGrammar::new(DESCRIPTOR.name, language, DESCRIPTOR.tags_query).map(Arc::new)
}
