# Architecture Decision Records

Numbered decisions for beamr. Once recorded, a decision is the reference
— cite "per ADR-003" and the reviewer can verify in seconds.

| ADR | Title | Status |
|-----|-------|--------|
| [001](001-loader-in-core.md) | Loader as module inside core crate | Accepted |
| [002](002-atom-table-in-core.md) | Atom table lives in core | Accepted |
| [003](003-no-async-scheduler.md) | No async runtime in scheduler | Accepted |
| [004](004-lowbit-term-tagging.md) | Low-bit term tagging, not NaN-boxing | Accepted |
| [005](005-gleam-opcodes-only.md) | Implement only opcodes Gleam emits | Accepted |
| [006](006-demand-driven-bifs.md) | BIFs are demand-driven via import table | Accepted |
| [007](007-supervision-is-library.md) | Supervision is library code, not VM machinery | Accepted |
| [008](008-messages-copy-terms.md) | Messages copy terms between processes | Accepted |
| [009](009-hook-is-registration.md) | Reduction hook is a registration point | Accepted |
| [010](010-dirty-scheduler-pool.md) | Dirty schedulers are a separate thread pool | Accepted |
| [011](011-lockfree-mailbox.md) | Mailbox uses lock-free MPSC | Accepted |
