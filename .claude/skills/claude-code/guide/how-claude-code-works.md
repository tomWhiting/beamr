# How Claude Code Works

## The Agentic Loop

Claude Code works through three phases: **gather context**, **take action**, and **verify results**. These blend together as Claude chains tool uses, course-correcting along the way.

- The loop adapts to task requirements (questions may only need context gathering, bug fixes cycle through all phases)
- You can interrupt at any point to steer, provide context, or suggest a different approach

## Models

| Model | Best For |
|-------|----------|
| Sonnet | Most coding tasks, balanced performance |
| Opus | Complex architectural decisions, stronger reasoning |

Switch models with `/model` during a session or start with `claude --model <name>`.

## Built-in Tools

| Category | Capabilities |
|----------|--------------|
| **File operations** | Read, edit, create, rename, reorganize files |
| **Search** | Find files by pattern, search content with regex, explore codebases |
| **Execution** | Run shell commands, start servers, run tests, git operations |
| **Web** | Search the web, fetch documentation, look up error messages |
| **Code intelligence** | Type errors/warnings, jump to definitions, find references (requires plugins) |

Claude chooses tools based on your prompt and what it learns. Example flow for "fix the failing tests":
1. Run test suite to see failures
2. Read error output
3. Search for relevant source files
4. Read files to understand the code
5. Edit files to fix the issue
6. Run tests again to verify

## What Claude Can Access

When you run `claude` in a directory, Claude gains access to:

- **Your project** - Files in the directory and subdirectories
- **Your terminal** - Any command you could run (build tools, git, package managers, etc.)
- **Your git state** - Current branch, uncommitted changes, recent commit history
- **CLAUDE.md** - Project-specific instructions loaded every session
- **Extensions** - MCP servers, skills, subagents, Chrome integration

## Sessions

Sessions are saved locally. Each message, tool use, and result is stored for rewinding, resuming, and forking.

**Sessions are ephemeral** - No persistent memory between sessions. Use CLAUDE.md for anything Claude should know across sessions.

### Session Management

| Command | Description |
|---------|-------------|
| `claude --continue` | Resume most recent conversation |
| `claude --resume` | Select from recent sessions |
| `claude --continue --fork-session` | Branch off to try different approach |

### Context Window

The context window holds conversation history, file contents, command outputs, CLAUDE.md, loaded skills, and system instructions.

**When context fills up:**
- Claude compacts automatically (clears older tool outputs, summarizes conversation)
- Put persistent rules in CLAUDE.md rather than relying on conversation history
- Run `/context` to see what's using space
- Run `/compact <focus>` for controlled compaction (e.g., `/compact focus on the API changes`)

**Managing context:**
- Skills load on demand (set `disable-model-invocation: true` to keep descriptions out until needed)
- Subagents get fresh context separate from main conversation

## Safety Mechanisms

### Checkpoints

Every file edit is reversible. Before Claude edits any file, it snapshots the current contents.

- Press `Esc` twice to rewind to a previous state
- Only covers file changes (remote actions can't be checkpointed)

### Permission Modes

Press `Shift+Tab` to cycle:

| Mode | Behavior |
|------|----------|
| **Default** | Claude asks before file edits and shell commands |
| **Auto-accept edits** | Edits files without asking, still asks for commands |
| **Plan mode** | Read-only tools only, creates a plan for approval |

Allow specific commands in `.claude/settings.json` to skip repeated prompts.

## Working Effectively

### Ask Questions
- "How do I set up hooks?"
- "What's the best way to structure my CLAUDE.md?"

### Use Built-in Commands
| Command | Purpose |
|---------|---------|
| `/init` | Create CLAUDE.md for your project |
| `/agents` | Configure custom subagents |
| `/doctor` | Diagnose common issues |

### Prompting Tips

**Be specific upfront:**
```
The checkout flow is broken for users with expired cards.
Check src/payments/ for the issue, especially token refresh.
Write a failing test first, then fix it.
```

**Give Claude verification criteria:**
```
Implement validateEmail. Test cases: 'user@example.com' → true,
'invalid' → false, 'user@.com' → false. Run the tests after.
```

**Explore before implementing:**
Use plan mode (`Shift+Tab` twice) to analyze the codebase first, then implement.

**Delegate, don't dictate:**
```
The checkout flow is broken for users with expired cards.
The relevant code is in src/payments/. Can you investigate and fix it?
```
