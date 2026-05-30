# Beamr — Checklist

## Crate Setup

- [ ] **C1** — Workspace Cargo.toml at repo root declares members: beamr, beamr-cli
- [ ] **C2** — beamr crate exists at crates/beamr/ with edition 2024
- [ ] **C3** — beamr-cli crate exists at crates/beamr-cli/ as a binary crate with edition 2024
- [ ] **C4** — beamr/src/lib.rs declares public modules: atom, loader, term, process, interpreter, scheduler, gc, mailbox, supervision, native, hook, timer, module, error
- [ ] **C5** — beamr has no dependencies on any Meridian, Yggdrasil, or norn crates
- [ ] **C6** — beamr-cli depends on beamr only
- [ ] **C7** — LoadError enum defined in beamr::error with variants for loader failures
- [ ] **C8** — ExecError enum defined in beamr::error with variants for runtime failures
- [ ] **C9** — BeamrError enum defined in beamr::error wrapping LoadError and ExecError with From impls
- [ ] **C10** — cargo check --workspace passes clean
- [ ] **C11** — cargo clippy --workspace -- -D warnings passes clean

## Atom Table

- [ ] **C12** — Global atom table implemented as a concurrent map supporting lock-free reads
- [ ] **C13** — Inserting a new atom string returns a unique Atom index
- [ ] **C14** — Inserting an already-interned atom string returns the same index
- [ ] **C15** — Lookup by index returns the original atom string
- [ ] **C16** — Concurrent inserts from multiple threads never produce duplicate entries for the same string
- [ ] **C17** — Common atoms pre-registered at table creation: ok, error, true, false, nil, undefined, normal, kill, EXIT, badarg, badarith, badmatch, function_clause, case_clause, if_clause, undef, badfun, badarity, noproc

## Loader

- [ ] **C18** — Parses the .beam chunked binary container format (FOR1/BEAM header, chunk headers, chunk data)
- [ ] **C19** — Decodes the Atom/AtU8 chunk and registers all atoms in the global atom table
- [ ] **C20** — Decodes the Code chunk into a vector of internal Instruction values
- [ ] **C21** — Decodes the StrT chunk (string table) as a byte buffer
- [ ] **C22** — Decodes the ImpT chunk into import entries (module, function, arity)
- [ ] **C23** — Decodes the ExpT chunk into export entries (function, arity, label)
- [ ] **C24** — Decodes the FunT chunk into lambda entries (function, arity, label, num_free)
- [ ] **C25** — Decodes the LitT chunk by decompressing (zlib) and deserializing the external term format literals
- [ ] **C26** — Decodes the Line chunk into source location information
- [ ] **C27** — Decodes compact term encoding for instruction operands (tags, extended tags, literals, atoms, labels, registers)
- [ ] **C28** — Resolves imports against loaded modules and the BIF registry; resolved imports point to callable targets
- [ ] **C29** — Produces an unresolved-import report listing every import that could not be resolved, grouped by module
- [ ] **C30** — Validates instruction operands: register indices in range, label targets exist, arities match
- [ ] **C31** — Stores a successfully loaded module in the module registry by its atom name
- [ ] **C32** — Loading a .beam file produced by gleam build + erlc succeeds for a pure Gleam module with no external dependencies beyond erlang: built-ins

## Terms

- [ ] **C33** — Term is a 64-bit value with low-bit tagging to distinguish types
- [ ] **C34** — Small integer encoded as immediate: value fits in the non-tag bits, round-trips encode/decode
- [ ] **C35** — Atom encoded as immediate: atom index in non-tag bits, round-trips encode/decode
- [ ] **C36** — Pid encoded as immediate: process id data in non-tag bits, round-trips encode/decode
- [ ] **C37** — Nil represented as a distinguished constant value
- [ ] **C38** — Tuple allocated on heap as boxed: header word with arity followed by arity element words
- [ ] **C39** — List cons cell allocated on heap as boxed: two words (head, tail), proper list terminates in nil
- [ ] **C40** — Float allocated on heap as boxed: header word followed by an f64
- [ ] **C41** — Binary allocated on heap as boxed: header word followed by length and byte data
- [ ] **C42** — Big integer allocated on heap as boxed: header word followed by sign and digit limbs
- [ ] **C43** — Fun/closure allocated on heap as boxed: header with module, function index, arity, and captured environment terms
- [ ] **C44** — Map allocated on heap as boxed: supports flatmap representation (sorted key array + value array)
- [ ] **C45** — Reference allocated on heap as boxed: unique id for monitors and timers
- [ ] **C46** — Term equality comparison (== semantics): same-type immediates compare by value, boxed terms compare structurally
- [ ] **C47** — Term exact equality comparison (=:= semantics): integer 1 and float 1.0 are not equal
- [ ] **C48** — Term ordering follows the BEAM order: number < atom < reference < fun < port < pid < tuple < map < nil < list < binary

## Processes

- [ ] **C49** — Process struct contains: pid, heap, stack, mailbox, reduction counter, status, links, monitors, trap_exit flag, group_leader
- [ ] **C50** — New process starts with a small heap (233 words default)
- [ ] **C51** — spawn(Module, Function, Args) creates a new process, pushes the initial call frame, assigns a unique pid, and places it on a run queue
- [ ] **C52** — Process status transitions: new → running → (waiting | exiting), waiting → running on message or timeout
- [ ] **C53** — X registers (at least 256) hold function arguments and temporaries per process
- [ ] **C54** — Y registers are stack-allocated: allocate/deallocate instructions create/destroy stack frames with Y slots
- [ ] **C55** — Process exit deallocates its heap, removes it from the scheduler, and propagates exit signals to linked processes
- [ ] **C56** — A process that exits with reason 'normal' does not cause linked processes to exit (unless they monitor)
- [ ] **C57** — Each process has a unique Pid that is never reused during a VM instance

## Interpreter

- [ ] **C58** — Execution loop: fetch instruction at current code pointer, execute it, advance pointer (or jump)
- [ ] **C59** — Reduction counter decremented on each call/apply instruction
- [ ] **C60** — Process yields (returns to scheduler) when reduction counter reaches zero
- [ ] **C61** — label instruction sets a jump target; func_info provides function metadata for error reporting
- [ ] **C62** — move instruction copies a term between registers, from a literal, or from a stack slot
- [ ] **C63** — call and call_only instructions: local function call within the same module, saving or not saving a return address
- [ ] **C64** — call_ext and call_ext_only instructions: call into another module or a BIF, with import resolution
- [ ] **C65** — call_last and call_ext_last instructions: tail call that deallocates the current stack frame before jumping
- [ ] **C66** — return instruction: pop the return address from the stack, jump to it
- [ ] **C67** — allocate and allocate_zero instructions: push a stack frame with N Y-register slots
- [ ] **C68** — deallocate instruction: pop the top stack frame
- [ ] **C69** — test_heap instruction: ensure N words of heap space available, triggering GC if not
- [ ] **C70** — put_list instruction: allocate a cons cell on the heap from head and tail operands
- [ ] **C71** — put_tuple2 instruction: allocate a tuple on the heap from an element list
- [ ] **C72** — get_tuple_element instruction: extract the Nth element from a tuple into a register
- [ ] **C73** — get_hd and get_tl instructions: extract head or tail from a cons cell
- [ ] **C74** — Type test instructions (is_integer, is_float, is_atom, is_list, is_tuple, is_nil, is_binary, is_boolean, is_map, is_function2): branch to fail label if test fails
- [ ] **C75** — is_eq_exact and is_ne_exact instructions: exact equality comparison, branch on result
- [ ] **C76** — is_lt and is_ge instructions: term ordering comparison, branch on result
- [ ] **C77** — select_val instruction: multi-way branch on a term's value against a list of value/label pairs
- [ ] **C78** — select_tuple_arity instruction: multi-way branch on a tuple's arity against a list of arity/label pairs
- [ ] **C79** — jump instruction: unconditional jump to a label
- [ ] **C80** — gc_bif1, gc_bif2, gc_bif3 instructions: call a guard BIF that may trigger GC, branch to fail label on error
- [ ] **C81** — bif0, bif1, bif2 instructions: call a guard BIF that cannot trigger GC
- [ ] **C82** — send instruction: send the term in x(1) to the process identified by x(0)
- [ ] **C83** — loop_rec, loop_rec_end, remove_message instructions: selective receive mailbox scan
- [ ] **C84** — wait instruction: suspend the process until a message arrives
- [ ] **C85** — wait_timeout and timeout instructions: suspend with a deadline, resume on message or expiry
- [ ] **C86** — try, try_end, try_case, try_case_end instructions: exception handling for Gleam's try expressions
- [ ] **C87** — raise instruction: re-raise a caught exception
- [ ] **C88** — badmatch, case_end, if_end instructions: generate appropriate runtime errors for failed pattern matches and case/if exhaustion
- [ ] **C89** — make_fun2 instruction: create a closure term capturing the current environment
- [ ] **C90** — call_fun instruction: invoke a closure with arguments
- [ ] **C91** — apply and apply_last instructions: dynamic call with module and function as terms
- [ ] **C92** — bs_init2 instruction: initialize a binary construction context on the heap
- [ ] **C93** — bs_put_integer and bs_put_binary instructions: append integer/binary segments to a binary under construction
- [ ] **C94** — bs_start_match2 instruction: initialize a binary matching context from a binary term
- [ ] **C95** — bs_get_integer2 and bs_get_binary2 instructions: extract integer/binary segments from a match context
- [ ] **C96** — bs_match_string instruction: match a literal byte sequence in a match context
- [ ] **C97** — bs_test_tail2 instruction: verify remaining bits in a match context
- [ ] **C98** — has_map_fields instruction: test that a map contains specified keys, branch to fail label if not
- [ ] **C99** — get_map_elements instruction: extract values for specified keys from a map into registers
- [ ] **C100** — put_map_assoc and put_map_exact instructions: create a new map by adding/updating key-value pairs

## Scheduler

- [ ] **C101** — Scheduler starts N worker threads where N defaults to the number of CPU cores
- [ ] **C102** — Each worker thread maintains its own run queue of ready processes
- [ ] **C103** — Worker loop: dequeue process, set reduction budget, call interpreter, handle result (yield/wait/exit)
- [ ] **C104** — A yielded process is placed at the back of the run queue for re-scheduling
- [ ] **C105** — A waiting process is moved to the wait set and does not consume scheduler cycles
- [ ] **C106** — Message arrival to a waiting process moves it back to a run queue
- [ ] **C107** — Work stealing: an idle worker steals half the processes from the busiest worker's queue
- [ ] **C108** — High-priority queue: processes at high priority are dequeued before normal-priority processes
- [ ] **C109** — Max-priority queue: processes at max priority are dequeued before high and normal
- [ ] **C110** — Default reduction budget per schedule is 4000 reductions
- [ ] **C111** — No process runs more than one full budget without yielding, even in a tight computational loop
- [ ] **C112** — Dirty scheduler: a separate thread pool for long-running native function calls
- [ ] **C113** — A native call marked as dirty is dispatched to the dirty pool; the normal worker thread is freed immediately
- [ ] **C114** — Scheduler shutdown: all worker threads and dirty threads terminate cleanly when the VM shuts down

## Garbage Collection

- [ ] **C115** — GC is per-process: collecting one process's heap does not pause or affect any other process
- [ ] **C116** — Generational: heap split into young generation (nursery) and old generation
- [ ] **C117** — Minor GC: copy live objects from young generation to old generation, reclaim young space
- [ ] **C118** — Major GC: copy all live objects to fresh space, reclaim both young and old
- [ ] **C119** — GC triggered when the process's heap cannot satisfy an allocation request
- [ ] **C120** — Root set includes: X registers, Y registers (stack), and mailbox terms
- [ ] **C121** — All term references (on stack, in registers, in mailbox) updated to point to new locations after copy
- [ ] **C122** — After GC, all previously reachable terms are still reachable with identical values
- [ ] **C123** — Heap grows on demand following a Fibonacci-like size sequence
- [ ] **C124** — Heap shrinks after a major GC if utilization is below a threshold

## Mailbox

- [ ] **C125** — Each process has a mailbox backed by a lock-free MPSC queue
- [ ] **C126** — Sending a message copies the term into the receiver's heap and appends it to the receiver's mailbox
- [ ] **C127** — Selective receive scans the mailbox from the save pointer forward, testing each message against patterns
- [ ] **C128** — A matched message is removed from the mailbox and the save pointer resets to the beginning
- [ ] **C129** — Unmatched messages remain in the mailbox in their original order
- [ ] **C130** — If no message matches, the process suspends (status → waiting) and the save pointer is preserved
- [ ] **C131** — A new message arriving at a suspended process wakes it to re-scan from the save pointer
- [ ] **C132** — Multiple senders can enqueue messages to the same mailbox concurrently without data loss or corruption

## Supervision

- [ ] **C133** — link(PidA, PidB) creates a bidirectional link: both processes' link sets contain the other
- [ ] **C134** — When a linked process exits, an exit signal with the exit reason is sent to all linked processes
- [ ] **C135** — A process receiving an exit signal with a non-normal reason exits with that reason (unless it traps exits)
- [ ] **C136** — A process receiving an exit signal with reason 'normal' is not terminated
- [ ] **C137** — A process with trap_exit=true receives exit signals as {'EXIT', Pid, Reason} messages in its mailbox instead of dying
- [ ] **C138** — An exit signal with reason 'kill' terminates the target process unconditionally, even if it traps exits
- [ ] **C139** — A process killed by 'kill' propagates exit reason 'killed' (not 'kill') to its links
- [ ] **C140** — monitor(Pid) returns a reference and registers a monitor; the monitored process's death sends {'DOWN', Ref, process, Pid, Reason} to the monitor owner
- [ ] **C141** — demonitor(Ref) removes the monitor; no DOWN message is sent after demonitor
- [ ] **C142** — unlink(PidA, PidB) removes the bidirectional link; no exit signal is sent after unlink
- [ ] **C143** — Monitoring a process that has already exited immediately sends a DOWN message
- [ ] **C144** — Linking to a process that has already exited immediately sends an exit signal

## Module Registry

- [ ] **C145** — Module registry stores loaded modules indexed by atom name
- [ ] **C146** — Module lookup by name returns the current version of the module or an error if not loaded
- [ ] **C147** — Loading a module with the same name as an already-loaded module replaces the current version
- [ ] **C148** — Function lookup by MFA (module, function, arity) resolves to a code pointer in the target module
- [ ] **C149** — Function lookup for an unloaded module or missing export returns an undef error

## Native Boundary

- [ ] **C150** — BIF registry maps (Module, Function, Arity) to a Rust function pointer
- [ ] **C151** — NIF registry maps (Module, Function, Arity) to a Rust function pointer
- [ ] **C152** — Native functions receive arguments as a slice of Term values and a mutable reference to the calling process
- [ ] **C153** — Native functions return Result<Term, Term> where Err becomes the process exit reason
- [ ] **C154** — Native function registration is available before any module is loaded, so BIFs are present for import resolution
- [ ] **C155** — A native call marked as dirty causes the process to be migrated to the dirty scheduler pool for the duration of the call
- [ ] **C156** — The set of required native functions is derived from the loader's unresolved-import report, not hardcoded
- [ ] **C157** — Minimum BIFs for Gate 1: erlang:+/2, erlang:-/2, erlang:*/2, erlang:div/2, erlang:rem/2, erlang:</2, erlang:>=/2, erlang:=:=/2, erlang:=/=/2, erlang:error/1, erlang:display/1

## Reduction Boundary Hook

- [ ] **C158** — A configurable hook callback fires at process yield points (reduction budget exhausted or blocking on receive)
- [ ] **C159** — The hook receives the process pid, its current module/function/arity, and the reduction count consumed
- [ ] **C160** — The hook can return 'continue' (process resumes normally) or 'suspend' (process is held until explicitly resumed)
- [ ] **C161** — The hook callback is set per VM instance and can be changed at runtime
- [ ] **C162** — When no hook is registered, the hook registration slot is None and the yield path skips hook invocation
- [ ] **C163** — The hook cannot modify the process's registers, heap, or mailbox — it is read-only inspection plus a continue/suspend decision

## Timer

- [ ] **C164** — Timer wheel supports scheduling, cancellation, and expiry of timeouts with O(1) insertion and cancellation
- [ ] **C165** — wait_timeout instruction registers a timer; if it expires before a matching message arrives, the process resumes at the timeout label
- [ ] **C166** — Timer is cancelled automatically when the process receives a matching message before expiry
- [ ] **C167** — erlang:send_after/3 BIF sends a message to a process after a delay
- [ ] **C168** — erlang:start_timer/3 BIF sends a {timeout, Ref, Msg} tuple after a delay
- [ ] **C169** — erlang:cancel_timer/1 BIF cancels a pending timer and returns remaining time
- [ ] **C170** — Timer resolution is millisecond-granularity or better

## CLI

- [ ] **C171** — beamr-cli binary accepts a path to a .beam file and a module:function/arity to execute
- [ ] **C172** — Loads the specified .beam file via beamr::loader and spawns a process calling the specified function
- [ ] **C173** — Registers basic I/O natives: erlang:display/1 prints a term to stdout
- [ ] **C174** — Prints the exit reason when the initial process terminates
- [ ] **C175** — Returns exit code 0 on normal termination, non-zero on crash
- [ ] **C176** — Prints the unresolved-import report if loading fails due to missing imports
