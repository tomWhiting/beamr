# 09 · Supervision — Links, Monitors & Letting It Crash

## What it is

This is the layer that makes the system *self-healing*, and it's probably the part
you'll feel proudest of the first time it works. It has three primitives that build
on each other, and one philosophy that ties them together.

The philosophy: **let it crash.** Don't write defensive code that tries to anticipate
every failure inline. Let a process die when something goes wrong, and make recovery
*structural* — handled by something watching from above — rather than tangled into
the work itself.

## The three primitives

**Links** are bidirectional bonds between two processes. If either one dies, the
other is told. By default that "telling" is itself fatal — a linked process dies
when its partner dies — which sounds alarming but is exactly how you make a *group*
of related processes live and die together. Unless, that is, a process declares it
**traps exits**, in which case the death notice arrives politely as a *message* it
can read and act on instead of dying. That one flag is the difference between "I'm a
peer who shares fate" and "I'm a supervisor who survives my children's deaths to do
something about them."

**Monitors** are the one-directional, non-fatal version: "tell me when that process
dies, but don't kill me over it." A watcher gets a notification message and decides
what to do. Used when you care about something's fate but aren't bonded to it.

**Exit signals** are the actual notifications flowing along links and monitors when a
process ends — carrying *why* it ended (finished normally? crashed? killed?).

## Supervisors — and the lovely part

A supervisor is *not* a special kind of VM object. It's just an ordinary process
that traps exits, links to some children, and — when a child dies — restarts it
according to a strategy: restart just the one that died, or restart all the
siblings, or restart that one and everything started after it. And there's a circuit
breaker: if children keep dying too fast (more than N times in M seconds), the
supervisor gives up and dies itself, pushing the problem up to *its* supervisor. That
upward-failing is what stops a doomed component from thrashing forever.

The lovely part: **the VM barely has to know about any of this.** Once bearmr
provides links, monitors, exit signals, and the trap-exit flag *correctly*,
supervision trees are just Gleam library code running like any other program. We
build the four primitives; the whole self-healing edifice is built *on top* in the
language, not baked into the engine.

## The intuition

A workshop with apprentices. Some apprentices are paired and work as a unit (links) —
if one walks out, the pair is broken and both stop. A foreman (supervisor) doesn't
share their fate; he *watches* (traps exits), and when an apprentice storms off, he
quietly brings in a replacement. If the same bench keeps blowing up every five
minutes, the foreman stops replacing and escalates to the manager — because the
problem clearly isn't the apprentice.

## How it connects

Built on **Processes** and the exit signals that ride the **Mailbox** channel. It's
the highest-value-per-line component, and it's what lets Meridian's agents be
supervised exactly the way you've already designed supervision to work.
