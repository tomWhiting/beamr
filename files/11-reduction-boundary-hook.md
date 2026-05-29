# 11 · The Reduction-Boundary Hook — The Bit That's Ours

*This is the one component you won't find in any BEAM book, because it isn't a BEAM
idea. It's the reason bearmr exists inside Meridian rather than as an academic
exercise. Read the others to understand the machine; read this to understand the
**point**.*

## The thread that runs through everything

Step back and notice a pattern that's shown up three times across this whole system:

- **bearmr** stops a process at the **reduction boundary** — the moment its budget
  runs out and it must yield.
- **norn** stops an agent at the **tool boundary** — the moment it reaches for a
  tool and the runtime can inject messages, run diagnostics, and block.
- **norn-memory** makes agents *sing* and lanterns *resonate* at that same boundary
  between actions.

It's one primitive at three altitudes: **the runtime does its important work in the
gap between an actor's actions.** The reduction boundary is simply the *lowest and
sharpest* version of that gap — and bearmr is the layer where it finally gets sharp
enough to do something nobody else can.

## What the hook is

Every time a process yields — budget exhausted, or blocking on a receive — there's a
clean, well-defined moment where the machine holds the process still and decides
what happens next. That moment is a **seam**. The reduction-boundary hook is: *at
that seam, before the process resumes, let Meridian's conventions-and-diagnostics
pipeline look at what just happened and have a say.*

"This process just ran a few thousand reductions. It touched these files, called
these operations. Does that violate anything we care about? If so — advise it, or
stop it here, before it goes further."

## Why this is impossible anywhere else

On the real BEAM, your diagnostics can only run *after* an agent has already acted —
after the file's written, the patch applied, the damage done. You're cleaning up,
not preventing. The tool boundary (norn today) is better — you catch things when the
agent reaches for a tool — but an agent that generates a long stretch of work without
reaching for a tool sails right past, un-tapped, like a process that never yields.

The reduction boundary closes that gap. Because *we own the loop and the counter*, we
get a guaranteed, regular checkpoint **no matter what the process is doing** — exactly
the property that made reductions special in the first place. An agent can't outrun
the tap on the shoulder by simply not pausing, because the machine pauses it anyway.
Diagnostics stop being a post-mortem and become a *live conscience.*

## The intuition

The difference between a referee who reviews the tape after the match and a referee
on the pitch who can blow the whistle the instant a foul happens. Same rules — but one
prevents and one only records. The reduction boundary puts the whistle on the pitch.

## What's quietly tricky

Restraint. A check that runs at *every* yield, on every process, can drown the system
in overhead and drown the agent in interruptions. The art is in *when* to actually
fire — sampling, thresholds, only-on-relevant-activity — the same "how thick is the
pith" calibration problem the memory system has. The hook is cheap to wire; spending
it wisely is the design work.

## How it connects

Hangs off the **Interpreter/Scheduler** yield seam. Feeds **norn**'s existing
conventions engine. Blocking here means signalling the **Process** (via the
**Supervision** machinery) rather than letting it resume. It is the thing that turns
"a fast Rust BEAM" into "the floor of Meridian."
