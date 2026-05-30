# Component tracker

Status of each beamr component through the build pipeline.

**Legend:** -- = not started, Research = in progress, Done = gate passed

| # | Component | Research | Design | Brief | Implement | Review | Merged |
|---|-----------|----------|--------|-------|-----------|--------|--------|
| 1 | Error types | -- | -- | B-001 | -- | -- | -- |
| 2 | Atom table | -- | -- | B-002 | -- | -- | -- |
| 3 | Loader parser | -- | -- | B-003 | -- | -- | -- |
| 4 | Import resolution | -- | -- | B-004 | -- | -- | -- |
| 5 | Term immediates | -- | -- | B-005 | -- | -- | -- |
| 6 | Boxed types | -- | -- | B-006 | -- | -- | -- |
| 7 | Term comparison | -- | -- | B-007 | -- | -- | -- |
| 8 | BIF/NIF registry | -- | -- | B-008 | -- | -- | -- |
| 9 | CLI | -- | -- | B-009 | R1/R4/R5 | Done | PR #1 |
| 10 | Process model | -- | -- | B-010 | -- | -- | -- |
| 11 | Mailbox | -- | -- | B-011 | -- | -- | -- |
| 12 | Scheduler | -- | -- | B-012 | -- | -- | -- |
| 13 | Interpreter: guards | -- | -- | B-013 | -- | -- | -- |
| 14 | Interpreter: send/recv | -- | -- | B-014 | -- | -- | -- |
| 15 | Interpreter: closures | -- | -- | B-015 | -- | -- | -- |
| 16 | Interpreter: binary | -- | -- | B-016 | -- | -- | -- |
| 17 | GC | -- | -- | B-017 | -- | -- | -- |
| 18 | Supervision | -- | -- | B-018 | -- | -- | -- |
| 19 | Priority + dirty sched | -- | -- | B-019 | -- | -- | -- |
| 20 | Hook + timers | -- | -- | B-020 | -- | -- | -- |
| 21 | Interpreter loop | -- | -- | B-021 | -- | -- | -- |

## Architecture research status

| # | Component | Architecture doc | Status |
|---|-----------|-----------------|--------|
| 0 | BEAM alternatives survey | 00-beam-alternatives-survey.md | Done (1062 lines) |
| 1 | Term representation | 01-term-representation.md | Failed (norn timeout) |
| 2 | Atom table | -- | Not started |
| 3 | Loader + bytecode | -- | Not started |
| 4 | Process model + heap | -- | Not started |
| 5 | Interpreter + opcodes | -- | Not started |
| 6 | Scheduler + work stealing | -- | Not started |
| 7 | Mailbox + selective receive | -- | Not started |
| 8 | GC (generational copying) | -- | Not started |
| 9 | Native function interface | -- | Not started |
| 10 | Supervision primitives | -- | Not started |
| 11 | Reduction boundary hook | -- | Not started |

## Notes

- All 21 briefs are authored and dispatch-ready
- CLI (B-009) partially implemented: R1/R4/R5 done, R2/R3 blocked on core types
- Architecture research phase starting: BEAM source analysis + alternatives survey
- Norn executes all analysis and implementation; team agents coordinate and review
