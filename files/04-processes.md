# 04 · Processes — The Unit of Life

## What it is

A process is the machine's atom of *activity* — one little running thing with its
own private memory, its own mailbox, and its own sense of how much longer it's
allowed to run before it must step aside. A workflow step is a process. An agent is
a process. A supervisor is a process. They are cheap: you can have hundreds of
thousands of them, and spawning one costs microseconds, not the milliseconds an
operating-system thread would.

Crucially, a process is **not** an OS thread. The operating system knows nothing
about them. They're managed entirely inside bearmr, which is exactly why they can be
so cheap and so numerous.

## Why it has to exist — the isolation property

This is the heart of "let it crash." Each process owns its own heap. Nothing is
shared. One process cannot reach into another's memory, cannot corrupt it, cannot
data-race against it — *there is nothing to race over.* So when a process hits
something fatal — bad input, a failed assertion, a divide by zero — it dies, and the
blast radius is *itself*. The rest of the system doesn't even flinch. You get to
write the happy path and let failures be structural rather than defensive. No
try/catch confetti; a process either does its job or dies cleanly and someone above
it decides what to do.

## The intuition

Picture a building full of tiny sealed offices. Each worker has their own desk, their
own notes, their own inbox slot in the door. They never share a desk. If one worker
has a breakdown and flips their desk, it's a mess *in that office* — the worker next
door keeps typing, unaware. A manager (supervisor) notices the office went quiet and
sends in a replacement. That's the whole philosophy in one image.

## What lives inside one

Conceptually: its private heap (its memory), its stack (where it is in its work), its
mailbox (messages waiting), a counter of how many "steps" it has left before it must
yield, its current status (running, waiting for a message, finished), and its
relationships — who it's *linked* to and who's *watching* it.

## What's quietly tricky

Making them genuinely cheap. If a process carries too much baggage, you can't have a
million of them. The discipline is keeping the per-process structure lean — a small
starting heap that grows only if needed, and no feature that isn't earned.

## How it connects

Lives on a **Scheduler**'s run queue. Runs via the **Interpreter**. Cleans its heap
via the **GC**. Talks through its **Mailbox**. Dies into the **Supervision** system.
