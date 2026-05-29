// orchestrated-dev-v3 / workflow.rhai
//
// Design-driven development cycle. Brief JSON in, enriched implementation out.
// Each stage enriches the requirements array — at completion, every R# carries
// a full record of context, implementation, hardening, and review.
//
// Pipeline: Scout → Plan → CreateTasks → Dev → CommitDev → ChecksDev
//         → Harden → CommitHarden → ChecksHarden → Review → CommitReview
//         → ChecksReview → Done → Notify → End

set_workflow_name("orchestrated-dev-v3");
set_workflow_description(
    "Design-driven development cycle. Brief JSON in, enriched implementation out. \
     Each stage enriches the requirements array."
);

let terminal = "end";

// ── Scout ───────────────────────────────────────────────────────────
add_step("scout", "action");
set_input("scout", "profile", "codebase-explorer");
set_template("scout", "instruction", "templates/scout-prompt.md.j2");
set_schema("scout", "output", "schemas/scout-output.json");
set_merge("scout", "enrichments", "input.brief.requirements", "id");
set_route("scout", "success", "plan");
set_route("scout", "failure", "done");

// ── Plan ────────────────────────────────────────────────────────────
add_step("plan", "action");
set_input("plan", "profile", "planner");
set_template("plan", "instruction", "templates/plan-prompt.md.j2");
set_schema("plan", "output", "schemas/plan-output.json");
set_route("plan", "success", "create_tasks");
set_route("plan", "failure", "done");

// ── Create Tasks ────────────────────────────────────────────────────
add_step("create_tasks", "execute");
set_template("create_tasks", "command", "templates/create-tasks.sh.j2");
set_input("create_tasks", "parser", "lines");
set_route("create_tasks", "success", "dev");
set_route("create_tasks", "failure", "done");

// ── Dev ─────────────────────────────────────────────────────────────
add_step("dev", "action");
set_input("dev", "profile", "workflow-developer");
set_input("dev", "task_list_id", "{input.brief.id}");
set_template("dev", "instruction", "templates/dev-prompt.md.j2");
set_schema("dev", "output", "schemas/dev-output.json");
set_merge("dev", "enrichments", "input.brief.requirements", "id");
set_route("dev", "success", "commit_dev");
set_route("dev", "failure", "done");

// ── Commit and Fix (Dev) ────────────────────────────────────────────
add_step("commit_dev", "execute");
set_template("commit_dev", "command", "templates/commit-dev.sh.j2");
set_input("commit_dev", "parser", "json");
set_route("commit_dev", "success", "checks_dev");
set_route("commit_dev", "failure", "checks_dev");

// ── Run Checks (Dev) ───────────────────────────────────────────────
add_step("checks_dev", "dispatch");
set_input("checks_dev", "workflow", "run-checks-and-tests");
set_input("checks_dev", "inputs.scope", "dev");
set_input("checks_dev", "inputs.crates_affected", "{commit_dev.data.crates_affected}");
set_input("checks_dev", "inputs.changed_files", "{commit_dev.data.changed_files}");
set_route("checks_dev", "success", "harden");
set_route("checks_dev", "failure", "dev");

// ── Harden ──────────────────────────────────────────────────────────
add_step("harden", "action");
set_input("harden", "profile", "hardener");
set_template("harden", "instruction", "templates/harden-prompt.md.j2");
set_schema("harden", "output", "schemas/harden-output.json");
set_merge("harden", "enrichments", "input.brief.requirements", "id");
set_route("harden", "success", "commit_harden");
set_route("harden", "failure", "done");

// ── Commit and Fix (Harden) ─────────────────────────────────────────
add_step("commit_harden", "execute");
set_template("commit_harden", "command", "templates/commit-harden.sh.j2");
set_input("commit_harden", "parser", "json");
set_route("commit_harden", "success", "checks_harden");
set_route("commit_harden", "failure", "checks_harden");

// ── Run Checks (Harden) ────────────────────────────────────────────
add_step("checks_harden", "dispatch");
set_input("checks_harden", "workflow", "run-checks-and-tests");
set_input("checks_harden", "inputs.scope", "harden");
set_input("checks_harden", "inputs.crates_affected", "{commit_harden.data.crates_affected}");
set_input("checks_harden", "inputs.changed_files", "{commit_harden.data.changed_files}");
set_route("checks_harden", "success", "review");
set_route("checks_harden", "failure", "harden");

// ── Review ──────────────────────────────────────────────────────────
add_step("review", "action");
set_input("review", "profile", "reviewer");
set_template("review", "instruction", "templates/review-prompt.md.j2");
set_schema("review", "output", "schemas/review-output.json");
set_merge("review", "enrichments", "input.brief.requirements", "id");
set_route("review", "success", "commit_review");
set_route("review", "failure", "done");

// ── Commit and Fix (Review) ─────────────────────────────────────────
add_step("commit_review", "execute");
set_template("commit_review", "command", "templates/commit-review.sh.j2");
set_input("commit_review", "parser", "json");
set_route("commit_review", "success", "checks_review");
set_route("commit_review", "failure", "checks_review");

// ── Run Checks (Review) ────────────────────────────────────────────
add_step("checks_review", "dispatch");
set_input("checks_review", "workflow", "run-checks-and-tests");
set_input("checks_review", "inputs.scope", "review");
set_input("checks_review", "inputs.crates_affected", "{commit_review.data.crates_affected}");
set_input("checks_review", "inputs.changed_files", "{commit_review.data.changed_files}");
set_route("checks_review", "success", "done");
set_route("checks_review", "failure", "review");

// ── Done ────────────────────────────────────────────────────────────
add_step("done", "execute");
set_input("done", "command", "echo \"Workflow complete: {input.brief.id} — {input.brief.title}\"");
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
set_output("plan_summary", "string", "{plan.tasks}");
set_output("dev_summary", "string", "{dev.summary}");
set_output("harden_summary", "string", "{harden.summary}");
set_output("review_summary", "string", "{review.summary}");
set_output("review_pass", "boolean", "{review.pass}");
