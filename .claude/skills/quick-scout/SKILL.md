---
name: quick-scout
description: Dispatch a research or investigation task to Norn via the quick-scout workflow. Use when you need information gathered — from the codebase, the web, or both — without making code changes. Triggered by terms like quick scout, research this, investigate, gather info, find out, look into, what does X do, how does Y work.
---

# Quick Scout Dispatch

Dispatch a research or investigation task to Norn via the `quick-scout` workflow. This is the read-only complement to `quick-task` — same single-step structure, but the agent investigates and reports rather than making code changes. Uses `gpt-5.3-codex-spark` for fast, focused research.

## When to Use

- Investigating how something works in the codebase
- Researching a library, API, or tool via web content
- Gathering information before writing a brief or making a decision
- Cross-referencing codebase state against documentation
- Any question that needs tool access (file reads, web fetches) to answer properly

## When NOT to Use

- Making code changes (use `quick-task`)
- Large multi-step implementations (use `onatopp-dev-norn`)
- Questions you can answer from memory or conversation context

## Command

```sh
meridian workflow run quick-scout \
  --workspace 2d5fdd51-1f25-45a4-8f86-4d4c978d1355 \
  --as c9255b2a-5731-4d17-8124-e3bfa2224186 \
  --input "task=$(cat task.json)" \
  --input "notify=${CLAUDE_SESSION_ID}"
```

## Task JSON Format

Save the task file at `.meridian/tasks/<name>.json` relative to the repository root. Name it after the investigation — lowercase, hyphenated (e.g. `how-provider-auth-works.json`, `axum-middleware-patterns.json`).

```json
{
  "title": "Short description of the research",
  "questions": [
    {
      "id": "Q1",
      "question": "What you want to know"
    },
    {
      "id": "Q2",
      "question": "Second question if needed"
    }
  ],
  "scope": "both",
  "context": "Relevant context — files to start from, URLs to check, what you already know"
}
```

### Fields

- **title** (required): Short name for the research, used in notifications
- **questions** (required): Array of questions, each with:
  - **id**: Identifier (Q1, Q2, etc.)
  - **question**: What you want to find out
- **scope** (optional): Where to look — `"codebase"` (local files only), `"web"` (web only), or `"both"` (default)
- **context** (optional): Relevant background — file paths to start from, URLs to check, what you already know or suspect

## What Happens

1. Norn receives the questions with full tool access
2. Investigates using Read, Glob, Grep, WebFetch, WebSearch as appropriate for the scope
3. Produces structured output with findings per question, confidence levels, and sources
4. DMs the results back to you (the dispatcher) via collective
5. No code changes, no commits — purely read-only

## Structured Output

The workflow returns per-question reporting:

- **summary**: Key findings in one paragraph
- **findings**: Array with id, answer, confidence (high/medium/low), sources
- **gaps**: Questions that could not be fully answered and why
- **follow_up**: Suggested follow-up questions or investigations

## Example

Save as `.meridian/tasks/how-provider-auth-works.json`:

```json
{
  "title": "How does the Norn provider authenticate with OpenAI?",
  "questions": [
    {
      "id": "Q1",
      "question": "What authentication method does the OpenAI provider use — API key, OAuth, or something else?"
    },
    {
      "id": "Q2",
      "question": "Where are credentials stored and how are they refreshed?"
    },
    {
      "id": "Q3",
      "question": "What happens when authentication fails mid-session?"
    }
  ],
  "scope": "codebase",
  "context": "Start from crates/norn/src/provider/openai/ and crates/meridian-services/src/workflow/provider.rs. The codex-login crate handles OAuth."
}
```
