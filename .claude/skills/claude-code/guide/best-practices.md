# Best Practices for Claude Code

**Core constraint:** Claude's context window fills up fast, and performance degrades as it fills. Most best practices flow from managing this resource.

## Give Claude Verification Criteria

**Highest-leverage tip.** Claude performs dramatically better when it can verify its own work.

| Strategy | Example |
|----------|---------|
| **Provide test cases** | "Write validateEmail. Test cases: user@example.com → true, invalid → false. Run the tests after." |
| **Visual verification** | "[paste screenshot] Implement this design. Take a screenshot and compare to the original." |
| **Root cause focus** | "The build fails with this error: [paste]. Fix it and verify the build succeeds. Address the root cause." |

## Explore → Plan → Implement → Commit

1. **Explore** (Plan Mode): Read files, understand the code
   ```
   Read src/auth/ and understand how we handle sessions.
   ```

2. **Plan**: Create detailed implementation plan
   ```
   I want to add Google OAuth. What files need to change? Create a plan.
   ```
   Press `Ctrl+G` to edit the plan directly.

3. **Implement**: Switch to Normal Mode, execute the plan
   ```
   Implement the OAuth flow from your plan. Write tests, run them, fix failures.
   ```

4. **Commit**: Create commit and PR
   ```
   Commit with a descriptive message and open a PR
   ```

**Skip planning for:** Typos, small fixes, single-line changes, clear scope tasks.

## Provide Specific Context

| Strategy | Before | After |
|----------|--------|-------|
| **Scope the task** | "add tests for foo.py" | "Write a test for foo.py covering the logged-out edge case. Avoid mocks." |
| **Point to sources** | "why does ExecutionFactory have weird api?" | "Look through ExecutionFactory's git history and summarize how its api came to be" |
| **Reference patterns** | "add a calendar widget" | "Look at HotDogWidget.php. Follow that pattern for a new calendar widget." |
| **Describe symptoms** | "fix the login bug" | "Login fails after session timeout. Check src/auth/, especially token refresh. Write a failing test, then fix it." |

### Rich Content Methods

- **Reference files with `@`** - Claude reads the file before responding
- **Paste images directly** - Copy/paste or drag and drop
- **Give URLs** - Use `/permissions` to allowlist domains
- **Pipe data** - `cat error.log | claude`

## Configure Your Environment

### CLAUDE.md

Run `/init` to generate, then refine. Include:
- Bash commands Claude can't guess
- Code style rules that differ from defaults
- Testing instructions
- Repository etiquette (branch naming, PR conventions)
- Architectural decisions specific to your project
- Developer environment quirks

**Exclude:**
- Anything Claude can figure out from reading code
- Standard language conventions
- Long explanations or tutorials

**Keep it concise.** If Claude ignores rules, the file is probably too long.

### CLAUDE.md Locations

| Location | Applies to |
|----------|------------|
| `~/.claude/CLAUDE.md` | All sessions |
| `./CLAUDE.md` | Current project (commit to git) |
| `./CLAUDE.local.md` | Current project (gitignored) |
| Child directories | Loaded on demand |

Import files: `@docs/git-instructions.md`

### Permissions

Use `/permissions` to allowlist safe commands or `/sandbox` for OS-level isolation.

### CLI Tools

Tell Claude to use CLI tools like `gh`, `aws`, `gcloud`, `sentry-cli`. Most context-efficient way to interact with external services.

### MCP Servers

`claude mcp add` to connect Notion, Figma, databases, etc.

### Hooks

Use for actions that must happen every time without exception. Claude can write hooks for you:
- "Write a hook that runs eslint after every file edit"
- "Write a hook that blocks writes to the migrations folder"

### Skills

Create `SKILL.md` files in `.claude/skills/` for domain knowledge and reusable workflows.

### Subagents

Define in `.claude/agents/` for tasks that read many files or need specialized focus.

## Communicate Effectively

### Ask Codebase Questions
- How does logging work?
- How do I make a new API endpoint?
- What does `async move { ... }` do on line 134 of foo.rs?
- Why does this code call foo() instead of bar()?

### Let Claude Interview You
For larger features, have Claude interview you first:
```
I want to build [brief description]. Interview me using AskUserQuestion.
Ask about technical implementation, UI/UX, edge cases, concerns, and tradeoffs.
Keep interviewing until we've covered everything, then write a spec to SPEC.md.
```

## Manage Your Session

### Course-Correct Early

| Key | Action |
|-----|--------|
| `Esc` | Stop Claude mid-action |
| `Esc + Esc` | Open rewind menu |
| `/rewind` | Restore previous state |
| `/clear` | Reset context between unrelated tasks |

**If you've corrected Claude more than twice on the same issue:** Run `/clear` and start fresh with a better prompt.

### Manage Context Aggressively

- `/clear` frequently between tasks
- `/compact <instructions>` for controlled compaction
- Add compaction instructions to CLAUDE.md: "When compacting, always preserve the list of modified files"

### Use Subagents for Investigation

```
Use subagents to investigate how our authentication system handles token refresh,
and whether we have any existing OAuth utilities I should reuse.
```

### Resume Conversations

```bash
claude --continue    # Resume most recent
claude --resume      # Select from recent
```

Use `/rename` for descriptive names: "oauth-migration", "debugging-memory-leak"

## Automate and Scale

### Headless Mode

```bash
claude -p "Explain what this project does"
claude -p "List all API endpoints" --output-format json
claude -p "Analyze this log file" --output-format stream-json
```

### Multiple Sessions

Use Claude Desktop for parallel local sessions or Claude Code on the web for cloud execution.

**Writer/Reviewer pattern:**
- Session A: Implement rate limiter
- Session B: Review the implementation for edge cases, race conditions
- Session A: Address review feedback

### Fan Out Across Files

```bash
for file in $(cat files.txt); do
  claude -p "Migrate $file from React to Vue. Return OK or FAIL." \
    --allowedTools "Edit,Bash(git commit *)"
done
```

### Safe Autonomous Mode

Use `--dangerously-skip-permissions` only in containers without internet access.

## Common Failure Patterns

| Pattern | Fix |
|---------|-----|
| **Kitchen sink session** - Unrelated tasks mixed | `/clear` between unrelated tasks |
| **Correcting over and over** - Failed approaches pollute context | After two corrections, `/clear` and write better initial prompt |
| **Over-specified CLAUDE.md** - Too long, rules get ignored | Ruthlessly prune, convert to hooks |
| **Trust-then-verify gap** - Looks right but doesn't work | Always provide verification (tests, scripts, screenshots) |
| **Infinite exploration** - Unscoped "investigate" fills context | Scope investigations or use subagents |
