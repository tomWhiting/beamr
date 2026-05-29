# 06 · The Scheduler — Fairness Across Every Core

## What it is

If the interpreter runs *one* process, the scheduler decides *which* process runs,
*when*, and *on which core*. It's the air-traffic controller. You have a handful of
worker threads — typically one per CPU core — and a great many processes that all
want to run. The scheduler keeps every core busy and every process moving.

## Why it has to exist

A laptop has, what, eight or sixteen cores. You want to run a hundred thousand
processes. Something has to multiplex the many onto the few, *fairly*, and keep all
the cores fed. Without a scheduler you'd either run one process at a time (wasting
your cores) or hand-manage threads (madness at this scale).

## How it works, conceptually

Each worker thread keeps a queue of processes ready to run. It pulls one off, hands
it to the interpreter with a fresh reduction budget, and lets it run until it yields
— either because its budget ran out (it'll go to the back of the queue, to run again
later) or because it's waiting for a message (it goes to sleep until one arrives).
Then the worker grabs the next process. Round and round.

The clever bit is **work stealing**: when a worker's own queue runs dry, instead of
sitting idle it walks over to a busier worker and *steals* half their waiting
processes. Load balances itself, with no central manager deciding who does what. A
core is never idle while another core has a backlog.

## The intuition

A row of supermarket checkouts. Each till has its own line. The reduction budget is
the rule "serve a fixed number of items, then if there's a queue behind, the next
customer steps up" — so no one with a trolley full of shopping blocks the person
behind them indefinitely. Work stealing is the cashier who, seeing their line empty,
waves over half the queue from the slammed till next door.

## What's quietly tricky — and a deliberate choice

This is *not* the place for an async runtime like tokio. The BEAM's green threads
are our own interpreter loop, not async tasks — so the scheduler is built on plain
OS threads plus a well-worn lock-free work-stealing-queue library. Tokio earns its
place later, only at the edges where we talk to files and networks. Keeping it out
of the hot scheduler keeps the fairness story simple and ours. (Lunatic made the
opposite call and rode tokio; it worked, but it's a different shape with different
tradeoffs.)

## How it connects

Drives the **Interpreter**. Holds **Processes** in run queues. Hands sleeping
processes to a wait set until the **Mailbox** wakes them. Spins up a separate "dirty"
pool for the long-running **Native** calls (a `git push`) that *would* otherwise
break the fairness promise.
