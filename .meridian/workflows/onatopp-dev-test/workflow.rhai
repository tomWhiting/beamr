// onatopp-dev-test / workflow.rhai
//
// TEST-ONLY variant: no commits, no checks, saves structured output
// at each step for inspection. All profiles use haiku.
// Pipeline: Scout → Save → CreateTasks → Dev → Save → Harden → Save → Review → Save → Done → Notify → End

set_workflow_name("onatopp-dev-test");
set_workflow_description(
    "Test variant of onatopp-dev-compact. No commits, no checks, \
     haiku profiles, saves structured output at each step."
);
set_max_visits(1);

let terminal = "end";

// ── Scout and Plan ──────────────────────────────────────────────────
add_step("scout", "action");
set_input("scout", "profile", "codebase-explorer");
set_template("scout", "instruction", "templates/scout-plan-prompt.md.j2");
set_schema("scout", "output", "schemas/scout-plan-output.json");
set_merge("scout", "enrichments", "input.brief.requirements", "id");
set_route("scout", "success", "save_scout");
set_route("scout", "failure", terminal);

// ── Save Scout Output ───────────────────────────────────────────────
add_step("save_scout", "execute");
set_template("save_scout", "command", "templates/save-output.sh.j2");
set_input("save_scout", "parser", "exit-code");
set_route("save_scout", "success", "create_tasks");
set_route("save_scout", "failure", "create_tasks");

// ── Create Tasks ────────────────────────────────────────────────────
add_step("create_tasks", "execute");
set_template("create_tasks", "command", "templates/create-tasks.sh.j2");
set_input("create_tasks", "parser", "lines");
set_route("create_tasks", "success", "dev");
set_route("create_tasks", "failure", terminal);

// ── Dev ─────────────────────────────────────────────────────────────
add_step("dev", "action");
set_input("dev", "profile", "workflow-developer");
set_template("dev", "instruction", "templates/dev-prompt.md.j2");
set_schema("dev", "output", "schemas/dev-output.json");
set_merge("dev", "enrichments", "input.brief.requirements", "id");
set_route("dev", "success", "save_dev");
set_route("dev", "failure", terminal);

// ── Save Dev Output ─────────────────────────────────────────────────
add_step("save_dev", "execute");
set_template("save_dev", "command", "templates/save-output.sh.j2");
set_input("save_dev", "parser", "exit-code");
set_route("save_dev", "success", "harden");
set_route("save_dev", "failure", "harden");

// ── Harden ──────────────────────────────────────────────────────────
add_step("harden", "action");
set_input("harden", "profile", "hardener");
set_template("harden", "instruction", "templates/harden-prompt.md.j2");
set_schema("harden", "output", "schemas/harden-output.json");
set_merge("harden", "enrichments", "input.brief.requirements", "id");
set_route("harden", "success", "save_harden");
set_route("harden", "failure", terminal);

// ── Save Harden Output ──────────────────────────────────────────────
add_step("save_harden", "execute");
set_template("save_harden", "command", "templates/save-output.sh.j2");
set_input("save_harden", "parser", "exit-code");
set_route("save_harden", "success", "review");
set_route("save_harden", "failure", "review");

// ── Review ──────────────────────────────────────────────────────────
add_step("review", "action");
set_input("review", "profile", "reviewer");
set_template("review", "instruction", "templates/review-prompt.md.j2");
set_schema("review", "output", "schemas/review-output.json");
set_merge("review", "enrichments", "input.brief.requirements", "id");
set_route("review", "success", "save_review");
set_route("review", "failure", terminal);

// ── Save Review Output ──────────────────────────────────────────────
add_step("save_review", "execute");
set_template("save_review", "command", "templates/save-output.sh.j2");
set_input("save_review", "parser", "exit-code");
set_route("save_review", "success", "done");
set_route("save_review", "failure", "done");

// ── Done ────────────────────────────────────────────────────────────
add_step("done", "execute");
set_input("done", "command", "echo 'Workflow complete'");
set_input("done", "parser", "exit-code");
set_route("done", "success", "notify");
set_route("done", "failure", "notify");

// ── Notify ──────────────────────────────────────────────────────────
add_step("notify", "execute");
set_template("notify", "command", "templates/notify.sh.j2");
set_input("notify", "parser", "exit-code");
set_route("notify", "success", terminal);
set_route("notify", "failure", terminal);

// ── End ─────────────────────────────────────────────────────────────
add_step(terminal, "end");

// ── Workflow outputs ────────────────────────────────────────────────
set_output("scout_summary", "string", "{scout.summary}");
set_output("dev_summary", "string", "{dev.summary}");
set_output("harden_summary", "string", "{harden.summary}");
set_output("review_summary", "string", "{review.summary}");
set_output("review_pass", "boolean", "{review.pass}");
