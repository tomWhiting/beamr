# 08 · Messages & Mailboxes — The Only Way Processes Touch

## What it is

Processes share no memory, so they can't communicate by changing a common variable —
there isn't one. They communicate by **sending messages**. Each process has a
**mailbox**: an inbox where messages pile up in arrival order, waiting to be read.
This is the *entire* inter-process contract. No shared state, no locks, no
data-races — just letters through a slot.

## How a send works

When process A sends a term to process B, the term is **copied** into B's heap and
dropped into B's mailbox. The copy is the whole point: after the send, A and B share
nothing — B has its own private duplicate. A can carry on mutating its world; B's
copy is untouched. Isolation is preserved precisely *because* we pay for a copy.
(For large binaries there's a cheaper path that shares the underlying bytes safely,
but the mental model is "send = copy.")

## How a receive works — and the subtle part

A process reading its mailbox doesn't just take the next letter. It can **pattern
match**: "give me the next message that looks like *this*; leave the rest for now."
That's **selective receive** — the process picks the message it's ready to handle and
*defers* the others, which stay in the mailbox in order. It lets a process say "I'm
waiting for a reply tagged 47; everything else can wait its turn."

That deferral is the quietly tricky part. The mailbox isn't a simple take-from-front
queue; it's a queue you *scan*, skipping non-matches and remembering where you got
to so you don't rescan from scratch. Done naively it can get slow if a process lets
unmatched messages pile up — a famous BEAM footgun in its own right. The mechanism
to handle it (a "save pointer" marking how far you've already looked) is well
understood; you just have to build it deliberately rather than reach for the obvious
queue and move on.

## What happens when there's nothing to read

The process goes to sleep — it tells the scheduler "wake me when mail arrives" and
yields its core. When someone sends it a message, the scheduler moves it back to a
run queue to try matching again. (Optionally with a timeout: "wake me on a message
*or* after five seconds," which is what the timer wheel is for.)

## The intuition

Sealed offices again, each with a mail slot. You can't walk into someone's office and
rearrange their desk; you can only post a letter. And when you read your own mail,
you're allowed to fish out "the one I'm waiting for" and leave the junk in the tray.

## How it connects

Mailboxes belong to **Processes**. Sends copy **Terms** between heaps. Empty
receives put a process to sleep until the **Scheduler** wakes it. Exit signals from
the **Supervision** layer arrive partly through this same channel.
