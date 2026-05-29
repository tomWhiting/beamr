// test-enrichment-dev / workflow.rhai
//
// Packaged Rhai workflow proving WS-008..WS-011 end-to-end:
//   * Workflow package layout discovered by scan_package_dir.
//   * MiniJinja templates with inheritance via set_template.
//   * Progressive enrichment via set_merge.
//   * Output schemas via set_schema.
//   * Shared Rhai variables driving multiple step configs.
//   * Static, script-time conditional toggling the optional Check step.
//
// Pipeline shape: Scout -> Dev -> [Check] -> Done.
// Profiles fall through to global haiku profiles (test-scout, test-developer).

set_workflow_name("test-enrichment-dev");
set_workflow_description(
    "Packaged enrichment workflow: Scout enriches each R# with related files \
     and context; Dev implements; Check verifies; Done terminates."
);
set_max_visits(3);

// ── Shared configuration ────────────────────────────────────────────────────
// Rhai script-time variables drive multiple step configs so profile names
// and shared route targets are declared once. Workflow inputs are NOT
// available at eval time, so the Check toggle is a script-level constant.

let scout_profile = "test-scout";
let dev_profile = "test-developer";
let terminal = "Done";
let include_check = true;

// ── Scout ────────────────────────────────────────────────────────────────
// Read-only enrichment. Renders templates/scout-prompt.md.j2 (which extends
// base-prompt.md.j2) and emits a structured JSON payload that the engine's
// apply_merge_directive folds into input.brief.requirements keyed by id.

add_step("Scout", "action");
set_input("Scout", "profile", scout_profile);
set_template("Scout", "instruction", "templates/scout-prompt.md.j2");
set_schema("Scout", "output", "schemas/scout-output.json");
set_merge("Scout", "enrichments", "input.brief.requirements", "id");
set_route("Scout", "success", "Dev");
set_route("Scout", "failure", terminal);

// ── Dev ──────────────────────────────────────────────────────────────────
// Implementation step. Templates already see the Scout-merged enrichments
// because base-prompt.md.j2 iterates input.brief.requirements after the
// merge has run. dev-prompt.md.j2 also includes a {% if visit_number > 1 %}
// retry banner — exercising the runtime-scalar context.

add_step("Dev", "action");
set_input("Dev", "profile", dev_profile);
set_template("Dev", "instruction", "templates/dev-prompt.md.j2");
set_schema("Dev", "output", "schemas/dev-output.json");

// ── Check (optional) ─────────────────────────────────────────────────────
// Demonstrates a script-level conditional. When include_check is false the
// Check step is never declared and Dev routes straight to Done.

if include_check {
    set_route("Dev", "success", "Check");
    set_route("Dev", "failure", terminal);

    add_step("Check", "execute");
    set_input("Check", "command",
        "echo 'Dev step completed; downstream runner verifies via cargo gates.'"
    );
    set_route("Check", "success", terminal);
    set_route("Check", "failure", terminal);
} else {
    set_route("Dev", "success", terminal);
    set_route("Dev", "failure", terminal);
}

// ── Done ─────────────────────────────────────────────────────────────────

add_step(terminal, "end");

// Workflow-level outputs surface the structured payloads to dispatchers
// that consume the OrchestrationDefinition output mapping.

set_output("scout_summary", "string", "{Scout.summary}");
set_output("dev_summary", "string", "{Dev.summary}");
set_output("dev_commit_message", "string", "{Dev.commit_message}");
