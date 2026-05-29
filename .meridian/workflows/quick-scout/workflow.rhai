// quick-scout / workflow.rhai
//
// Single-step research workflow. Dispatches a research question to a
// lightweight Norn agent (gpt-5.3-codex-spark) with full tool access
// for investigating codebases, fetching web content, and reading files.
// Returns findings as free-form markdown — no code changes, no commits.
//
// Inputs:
//   task   - JSON object with: title (string), questions (array of
//            {id, question}), context (optional string), scope (optional
//            string — "codebase", "web", "both", defaults to "both")
//   notify - member name to DM results to (optional)

let task = input.task;
let task_title = task.title;
let scope = if task.scope != () { task.scope } else { "both" };

// =====================================================================
// BUILD INSTRUCTION
// =====================================================================

let instruction = "# Research: " + task_title + "\n\n";

instruction += "You are a research agent. Your job is to investigate the questions below and return substantive, well-sourced findings. You have access to tools for reading files, searching codebases, and fetching web content.\n\n";

if scope == "codebase" {
    instruction += "**Scope: codebase only.** Focus your investigation on the local codebase. Use read, search, bash, and lsp to find the answers. Do not use web tools.\n\n";
} else if scope == "web" {
    instruction += "**Scope: web only.** Focus your investigation on web sources. Use web_fetch and web_search to find the answers. Do not read local files.\n\n";
} else {
    instruction += "**Scope: codebase and web.** Use all available tools — read local files, search the codebase, fetch web pages, run web searches. Cross-reference where possible.\n\n";
}

instruction += "## Questions\n\n";
for q in task.questions {
    instruction += "### " + q.id + ": " + q.question + "\n\n";
}

if task.context != () {
    instruction += "## Context\n\n" + task.context + "\n\n";
}

instruction += "## Instructions\n\n";
instruction += "Use forks and parallel tool calls to investigate multiple questions or sources simultaneously. Your context window is limited — fan out rather than trying to hold everything in one sequential pass. Spawn sub-agents for independent questions or different source directories.\n\n";
instruction += "Investigate each question thoroughly. For each finding:\n";
instruction += "- Provide a substantive answer with enough context that someone unfamiliar with the topic can understand it\n";
instruction += "- Rate your confidence (high/medium/low) based on the quality of sources found\n";
instruction += "- List every source you consulted (file paths for codebase, URLs for web)\n";
instruction += "- If you cannot fully answer a question, explain what you found and what is missing\n\n";
instruction += "Format your response as markdown with a heading per question (## Q1, ## Q2, etc), a confidence tag, the substantive answer, and a Sources list. End with a ## Gaps section for anything you could not answer and a ## Follow-up section for suggested next investigations.\n\n";
instruction += "Do NOT make code changes. Do NOT commit anything. This is a read-only investigation.\n";

// =====================================================================
// EXECUTE
// =====================================================================

print("quick-scout: " + task_title);

try {

let result = run_step_norn("scout", "norn-scout", instruction, "");

if result.is_error {
    throw "scout step failed: " + result.error;
}

// =====================================================================
// NOTIFY
// =====================================================================

if input.notify != () {
    let report = "## " + task_title + "\n\n";

    let text = result.ai_response_text;
    if text != () && text != "" {
        report += text;
    } else {
        report += "(Scout returned no text output)";
    }

    write_file(".report.tmp", report);
    run_cmd("collective send --as Meridian --to '" + input.notify + "' --subject 'quick-scout complete: " + task_title + "' --message \"$(cat .report.tmp)\"");
    run_cmd("rm -f .report.tmp");
}

print("quick-scout complete: " + task_title);

} catch (err) {
    print("quick-scout failed: " + err);
    if input.notify != () {
        run_cmd("collective send --as Meridian --to '" + input.notify + "' --subject 'quick-scout FAILED: " + task_title + "' --message 'quick-scout failed for " + task_title + ": " + err + "'");
    }
    throw err;
}
