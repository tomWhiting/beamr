# 05 · The Interpreter & Reductions — The Heartbeat

## What it is

The interpreter is the loop at the centre of everything: fetch the next instruction,
figure out what it means, do it, move on. It's the thing that actually *runs* the
compiled Gleam. Every other component exists to feed it or to clean up after it.

But the interpreter carries a second job that's far more important than "execute
instructions," and it's the reason this whole project is worth doing.

## Reductions — the idea everything pivots on

Give every process a budget. Call each unit of work a **reduction** — roughly, one
function call. Every time the process does a reduction, knock one off its budget.
When the budget hits zero, *stop the process mid-stride*, save exactly where it was,
and hand the core to someone else.

That's it. That's the magic. And here's why it's magic and not just bookkeeping:

A normal Rust async task, or a Go goroutine, only yields the core at *cooperative*
points — when it chooses to `await`, or hits a channel. Write a tight loop with no
such point and it pins a core forever; the runtime is powerless. **A bearmr process
cannot do that.** Even a pure-computation loop with no I/O, no yielding, nothing —
*will* be interrupted, because the interpreter is counting reductions on every step
and will yank it at zero regardless of what it's doing. No process can starve the
others. Fairness is *guaranteed*, not hoped for.

This is the property the BEAM has that almost nothing else does, and we get it for
**free** for one reason: **we own the loop.** Because we're the ones fetching each
instruction, we're the ones who can count and interrupt. (Lunatic got the same
property by borrowing Wasmtime's "fuel," which is this exact idea wearing a
different hat.)

## The intuition

A fair teacher with an egg timer. Every student gets the same number of minutes to
speak, then the timer dings and it's the next student's turn — no matter how
fascinating or rambling the current one is. Nobody hogs the room. The timer is the
reduction counter.

## What's quietly tricky

You don't need all ~170 of the BEAM's instructions. You need the couple-dozen Gleam
actually emits — move a value, call a function, return, prepare some heap, send,
receive, branch on a pattern. Start there. The honest hard part isn't the count of
instructions; it's the binary-matching family, which deserves its own focused effort.

## How it connects

It *is* the engine the **Scheduler** drives. It decrements the **Process**'s
reduction counter — and that yield moment is the exact seam where our diagnostics
hook lives (doc 11). It reads and writes **Terms** on the heap, triggers the **GC**
when heap runs low, and calls out to **Native** functions.
