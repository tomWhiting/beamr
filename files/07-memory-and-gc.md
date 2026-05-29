# 07 · Memory & Garbage Collection — Each Process Cleans Its Own Room

## What it is

Every process has a small private heap where its terms live. As it works, it
allocates — builds lists, tuples, binaries. Eventually the heap fills. Garbage
collection is the act of finding what's still in use, keeping that, and reclaiming
the rest so the process can keep going.

## The property that matters

On most runtimes, garbage collection is a system-wide event: everything pauses while
the collector runs ("stop the world"). The BEAM's trick — and ours — is that GC is
**per-process**. Because each process owns its own heap and shares nothing, you can
collect *one* process's garbage without touching any other. Its neighbours keep
running. There is no global pause, ever.

For a workflow-and-agent engine this is gold. One agent doing something
memory-heavy gets its own little collection hiccup; the other agents, the
schedulers, the message handlers — all oblivious. The cost is paid locally by the
process that incurred it, which is exactly fair.

## How it works, conceptually

The standard approach is *generational copying*. Most data dies young (a temporary
list built and discarded in one operation), so split the heap into a "nursery" for
new things and an "old" area for survivors. Collect the nursery often and cheaply,
copying the few survivors to the old area; collect the old area rarely. Because the
heaps are small, each collection is microseconds. "Copying" means you literally walk
the live data, copy it to fresh space, and abandon the old space wholesale — which
also tidily defragments memory as a side effect.

## The intuition

A small desk you tidy constantly. Most things on it are scrap you sweep into the bin
in seconds (the nursery). The few things that prove they matter get filed in the
drawer (the old generation), which you only clear out occasionally. You never have
to shut down the whole office to tidy your desk.

## Why we can't skip it

It's tempting to defer — "processes will just allocate until they hit a limit." Fine
for the very first proof-of-life. But the compiled bytecode *assumes a collector
exists*: some instructions exist specifically to prepare the heap for one. Run real
Gleam for any length of time without GC and it will assume the safety net is there
and fall through where it isn't. So GC is a "must," just not a "first."

## What's quietly tricky

It's the scariest component, because its failures are *silent*. A bug doesn't crash
loudly — it quietly corrupts a term, and you find out a thousand operations later
when something inexplicable happens. The defence is relentless property-testing (do
terms survive a collection unchanged? is everything reachable still reachable?) and,
once enough is built, running real Gleam test suites as a truth oracle.

## How it connects

Operates on a **Process**'s heap, walking its **Terms**, triggered by the
**Interpreter** when space runs low. Invisible to the **Scheduler** and to every
other process — which is the entire point.
