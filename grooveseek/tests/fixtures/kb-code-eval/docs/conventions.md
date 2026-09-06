---
title: House Conventions
topic: formatting
category: docs
tags: [style, naming, review]
date: 2026-06-09
---

## Saturating arithmetic everywhere

Nothing in this library is allowed to panic on a value a caller could
plausibly hand it, so additions and multiplications that could run past
the end of their type saturate instead. A total that stops climbing is
wrong in a way somebody notices and reports; a process that stops running
is wrong in a way that takes the caller's work down with it.

## Names say what, comments say why

Identifiers are chosen so that a reader who knows the domain can follow
the flow without the prose. The paragraph above a definition is reserved
for the reason a choice was made, especially where the obvious choice was
rejected. A comment that restates the line beneath it is deleted on sight.

## No dependencies beyond the standard library

Pulling in a crate to save twenty lines trades a small amount of typing
for a supply chain, a version to track and a build that can break without
anybody here touching it. The bar for adding one is that the thing it does
would be a mistake to attempt by hand.
