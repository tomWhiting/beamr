# 10 · The Native Boundary — How Gleam Reaches Into Your Rust

## What it is

At some point your Gleam workflow needs to *do* something real: run a git command,
merge two syntax trees, walk a dependency graph, write a file. That work is Rust —
your Yggdrasil operations. The native boundary is how a function call in Gleam
reaches across and runs your Rust, then hands the result back as a term.

You register a Rust function under a name; when the running bytecode calls that name,
the machine invokes your function, hands it the arguments (as terms), takes back what
it returns (as a term), and carries on as if nothing exotic happened.

## Why this is *the* point of the whole project

On the real BEAM, this boundary is a wound. Your Rust would be a "NIF" reaching in
across a C interface, and it comes with rules written in blood: return almost
instantly or you stall a scheduler thread; crash, and you don't crash *your* code —
you crash the *entire node*, because native code shares the VM's address space with
no safety net. Anything slow needs ceremony to avoid wrecking fairness.

In bearmr, **your Rust *is* the machine.** There's no foreign boundary to cross —
it's a plain function call within one program. No serialization, no copying across an
interface, no separate process to babysit. And the thing that was a liability becomes
an asset: because *you* own the reduction counter, *you* decide how a native call
accounts for itself against fairness. You're not fighting the scheduler's rules about
native code — you're writing them.

This is the sentence from the design that belongs at the top of everything: *no IPC,
no serialisation overhead, no separate process to manage.* That's the prize.

## The one honest problem, and its answer

Some native work genuinely takes a while — a `git push`, a `cargo build`. If that
ran on a normal scheduler thread, it'd hold the thread hostage and break the fairness
promise for every process queued behind it. The answer is a **dirty scheduler**: a
separate pool of threads set aside for slow native work. The long operation goes
there, the normal scheduler threads stay free and fair, and when the slow thing
finishes its result rejoins the process that asked for it. Same idea the BEAM uses,
for the same reason.

## The intuition

A kitchen. The line cooks (normal schedulers) must keep plates moving fast — no one
order can jam the pass. But some tasks genuinely take twenty minutes (a slow braise).
You don't let that block the line; you put it on a back burner (the dirty pool) and
the line keeps flowing. When the braise is done, it's plated and sent out.

## How it connects

Invoked by the **Interpreter** when bytecode calls a registered name. The set of
names worth registering is told to you by the **Loader**'s unresolved-import report.
Slow ones lean on the **Scheduler**'s dirty pool. This is the door between bearmr and
the rest of Meridian.
