# 01 · Atoms — The Shared Vocabulary

## What it is

An atom is a name. `ok`, `error`, `gen_server`, the name of a module, the name of a
function — in the BEAM world these aren't strings, they're *atoms*: a single short
word that stands for itself. The atom table is the one global place where every
name in the whole running system lives, each stored exactly once.

## Why it has to exist

Two reasons, and the first is brutally practical: **you cannot decode a compiled
Gleam file without it.** The bytecode doesn't say "call the function named
`handle_call`" — it says "call function number 47," where 47 is an index into the
file's atom table. Names are referred to by number everywhere. Resolve those
numbers against one shared table and the whole program wires together; fail to, and
the bytecode is gibberish.

The second reason is speed. Because each atom exists *once*, comparing two atoms is
comparing two integers — not walking two strings character by character. Pattern
matching in Gleam leans on this constantly. The atom table is what makes `case x {
ok -> ... }` cheap.

## The intuition

Think of it as the system's dictionary. The first time anyone uses a word, it gets
an entry and a number. Forever after, everyone refers to the word by its number.
The dictionary only grows — atoms are never forgotten while the system runs (which,
on the real BEAM, is famously how you can crash a node by inventing infinite atoms;
we don't need to worry about that for our scoped use).

## What's quietly tricky

It's shared across every process on every core, and it's read constantly. So it has
to be safe for many threads to read at once and occasionally write, without becoming
a bottleneck. That's a "concurrent map" problem, well-trodden in Rust — not a
research project, but the one place early on where a naive lock would hurt.

## How it connects

The **Loader** populates it while reading a file. The **Interpreter** consults it on
every name-based operation. **Terms** can *be* atoms. It's the most foundational
brick — nothing else works until this does.
