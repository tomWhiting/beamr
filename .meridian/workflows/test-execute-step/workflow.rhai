// test-execute-step / workflow.rhai
//
// Tests the WS-019 unified execution model with merge verification:
//   1. Define scout declaratively with set_merge, execute via execute_step
//   2. Verify: dump input.brief.requirements AFTER merge — the verify agent
//      reports what fields each requirement has. If merge worked, requirements
//      will have scout-added fields (files, enrichments) beyond the original
//      brief fields.
//   3. Conditional: if scout summary is short, run a deeper scout
//   4. Summarize: template references {{ scout.summary }} from shared context

// ── Scout step: defined declaratively, executed imperatively ──
add_step("scout", "action");
set_input("scout", "profile", "test-scout");
set_template("scout", "instruction", "templates/scout-prompt.md.j2");
set_schema("scout", "output", "schemas/scout-output.json");
set_merge("scout", "enrichments", "input.brief.requirements", "id");

let scout = execute_step("scout");

// ── Verify merge: dump the actual merged state ──
// This step's template renders {{ input.brief.requirements | tojson }}
// and asks the agent to report exactly what fields are present.
// If the merge worked, each requirement will have "files" from the scout enrichments.
add_step("verify", "action");
set_input("verify", "profile", "test-scout");
set_template("verify", "instruction", "templates/verify-merge-prompt.md.j2");
set_schema("verify", "output", "schemas/verify-output.json");

let verify = execute_step("verify");

// ── Conditional: if scout summary is short, do a deeper pass ──
if scout.summary.len() < 50 {
    let deep_prompt = render_template("templates/scout-prompt.md.j2");
    let deep = run_step("deep_scout", "test-scout", deep_prompt, "schemas/scout-output.json");
}

// ── Summarize step: uses scout output via shared context ──
add_step("summarize", "action");
set_input("summarize", "profile", "test-scout");
set_template("summarize", "instruction", "templates/summary-prompt.md.j2");
set_schema("summarize", "output", "schemas/summary-output.json");

let summary = execute_step("summarize");
