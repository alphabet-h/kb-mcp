# 18. One range is one definition

- Status: accepted
- Date: 2026-09-06
- Deciders: project owner
- Applies to: v1.6.0

## Context and problem

[ADR-0014](0014-bound-the-chunker-by-the-shape-of-its-input.md) bounds how deeply a
definition may sit before the chunker stops chunking at definitions: the walk that collects a
definition's scope counts its steps, and the first definition past 64 syntax-tree ancestors
ends definition-based chunking for that file. That bound is also what keeps the chunker's one
recursive function from running away. `emit_def` descends the containment tree, and a
containing definition is a syntax-tree ancestor too, so a bound on ancestors was taken to be
a bound on recursion.

It is the "too" that fails. Containment is computed from byte ranges rather than from the
tree: one definition contains another when its range does not end before the other begins.
A range never ends before itself, so two definitions covering *the same* bytes are recorded
as one containing the other. The chain that produces is as long as the number of repeats, and
not one of its links adds a syntax-tree ancestor, so the scope bound never sees it.

The compiled-in Rust grammar produces no such shape, which is why this went unnoticed for
three releases. The PHP grammar this project publishes does. Its tags query hangs
`@definition.field` on a `property_declaration` and `@name` on each `property_element`
beneath it, and tree-sitter-tags keeps one tag per name rather than one per node, so
`public $a, $b, $c;` arrives as three definitions covering one range. A class declaring three
thousand properties in a single statement is a 22,923 byte file — two per cent of the 1 MiB
raw-byte cap that is meant to be the defence here — and indexing it ended the run with
`STATUS_STACK_OVERFLOW`
<!-- via: target/release/groove.exe --config <cfg> index --force -->.
A stack overflow is not a panic. The guard that lets one unreadable file be skipped instead
of ending the whole run never sees it, nothing is written, and over the MCP transport the
server goes down with the process.

The question this answers: **what is a definition, when two of them cover the same bytes?**

## Decision drivers

- A knowledge base is not a trust boundary.
  [ADR-0016](0016-keep-the-plugin-directory-outside-the-knowledge-base.md) already treats what
  a knowledge base can reach as untrusted, and a file inside one must not be able to end the
  process.
- `emit_def` is the only recursion in the chunker, so whatever bounds it has to be something
  the input cannot inflate.
- A bound no input can reach is worse than no bound: it reads as a defence while defending
  nothing, and the next reader trusts it.
- Chunks are the index, so any answer has to leave the same file chunking the same way on any
  machine.

## Options considered

1. **Collapse definitions that cover the same bytes into one.**
2. **Add a third bound**, on containment depth, beside the scope and chunk bounds, and degrade
   to line chunking past it.
3. **Both**: collapse the repeats, and keep the bound behind them.

## Decision

**Option 1.** A definition whose covered range repeats one already recorded is dropped before
the containment forest is built. The leftmost name stands for the declaration.

**Why not option 2.** A bound on containment depth would stop the crash without naming what
is wrong. Definitions covering the same bytes are not deeply nested; they are one definition
seen under several names, and recording the repeat as nesting is the defect itself. The bound
would also cost a file all of its definition metadata because one declaration in it happened
to be wide, and a comma-separated property list is ordinary PHP rather than a pathological
input. Degrading is the right answer to a file whose shape defeats definition chunking, and
the wrong answer to a file the chunker simply mis-read.

**Why not option 3.** Once the repeats are gone, containment nests no deeper than the syntax
tree does, and the scope bound already refused anything past its own limit. No input reaches
a third bound, so shipping one would ship the fourth driver's failure on purpose. The
invariant is written as a `debug_assert!` instead, which says the same thing without claiming
to enforce it where it cannot fire.

The collapse is derived from the file alone, so the same file still chunks the same way
anywhere. Where the whole declaration fits the chunk budget, the output does not change at
all: the outermost definition of the chain already answered for it, and the outermost
definition was already the first name.

### Consequences

- **A declaration naming several things is one chunk headed by the first of them.** Where the
  declaration is over the budget and has to be split, the pieces used to be headed by the
  *last* name — the deepest link of the chain was the only one that emitted — and are now
  headed by the first. The names after the first become neither a heading nor a
  `symbol_kind`; they are still in the chunk's text, so a search for one still reaches the
  declaration.
- **What bounds `emit_def` is now established where it is relied on**, rather than inferred
  from a bound on a different quantity. A file of 20,000 properties in one declaration
  (168,923 bytes) indexes in 629 ms
  <!-- via: target/release/groove.exe --config <cfg> index --force -->.
- No chunking-policy generation is recorded for this. An index built before this release
  holds nothing the parser should have refused: every byte was in a chunk either way, and
  what differs is which name heads the pieces of an over-budget declaration. That is a
  cosmetic difference `groove index --force` clears, not damage `groove doctor` has to warn
  about. The files that would have differed most are the ones that ended the run, and those
  were never indexed at all.
- Two definitions that merely *begin* together are untouched. The containment sort already
  expects that case and breaks the tie by putting the wider one first, so the repeat test
  compares both ends.
- This says nothing about a tags query that reports the same name twice on one node.
  tree-sitter-tags already collapses those, and a query that binds several names to one
  definition is the shape this addresses.

## References

- The chunker is `grooveseek/src/parser/code/`; the collapse and the containment forest live
  in `mod.rs`.
- ADR-0014 for the bound this repairs the reasoning of, ADR-0012 for the promise that every
  byte survives, ADR-0013 for why a grammar arrives as a plugin at all.
