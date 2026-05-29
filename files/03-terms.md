# 03 · Terms — What Everything Is Made Of

## What it is

A *term* is any piece of data the machine can hold: a number, an atom, a list, a
tuple, a chunk of binary, a process identifier, a function value. Every value that
flows through the interpreter is a term. If atoms are the vocabulary, terms are the
matter.

## The one idea that makes it fast

A term is a single machine word — 64 bits. Some values are small enough to live
*entirely inside that word*: a small integer, an atom (which is just its dictionary
number), `nil`, a process id. These are called **immediates** — no allocation, no
pointer-chasing, the value just *is* the word.

Bigger things — a tuple, a list cell, a binary, a huge number, a function closure —
can't fit in a word, so the word instead holds a *pointer* to where they live on the
heap. These are **boxed**.

How does the machine know which is which? A few **tag bits** in the word. Read the
tag, and you instantly know "this is a small integer" vs "this is a pointer to a
tuple." It's a tagged union, but done at the level of individual bits for speed.

## Why this design and not another

You might have heard of NaN-boxing (hiding values inside the unused bit-patterns of
floating-point numbers). It's clever, and it's the right call when your program is
drowning in floats. Ours isn't — our hot path is integers, atoms, and pointers. So
we use the classic low-bit tagging the BEAM itself uses. (Keep NaN-boxing in your
back pocket for the project where floats dominate; this isn't it.)

## Why we can't get cute about it

Because we run *real* compiled bytecode, the term layout isn't a free choice. The
instructions were generated assuming a particular shape of value, a particular way
heaps are allocated and walked. We inherit that shape. This single fact cascades:
it's *why* we need a garbage collector (the bytecode allocates assuming one exists),
and it's why the binary-handling part is genuinely hard rather than incidental.

## What's quietly tricky

Binaries. A binary can be a standalone blob, or a *slice that shares* another
binary's memory without copying — which is wonderful for performance and a menace
for correctness. The good news: Rust's ecosystem hands you refcounted,
cheaply-sliceable byte buffers, so one of the BEAM's fiddliest subsystems is largely
a solved problem you can adopt rather than build.

## How it connects

Terms live on a **Process**'s heap. The **GC** walks them. The **Interpreter**
manipulates them. Messages are terms copied between processes. This is the
substrate; almost every other doc assumes it.
