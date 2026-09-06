---
title: What This Library Is For
topic: formatting
category: docs
tags: [overview, listings, text]
date: 2026-06-02
---

## Scope

Everything here exists to turn values a program already holds into a
block of plain text a person can read in a terminal. There is no colour,
no cursor control and no width detection: the caller decides how wide the
output may be and this library fills it.

## The pieces

Reading a written interval and printing one back are the two halves of the
same job, and they live together. Holding a bounded number of recent items
is separate from printing them, so that a caller can keep the holder and
print only sometimes. Laying values out in columns is the largest piece and
the one with the most decisions in it.

## Why so little configuration

Each choice this library makes is written down beside the value that
encodes it rather than being exposed as a setting. A setting has to be
documented, defended and kept working; a constant with a paragraph above
it only has to be read. Callers who disagree with a choice are better
served copying the twenty lines they need than by a knob nobody else turns.

## Not in scope

Sorting rows, deciding which rows to show, and fetching the values in the
first place all belong to the caller. This library never reads the clock
except when the caller hands it a reading, and never reads the terminal.
