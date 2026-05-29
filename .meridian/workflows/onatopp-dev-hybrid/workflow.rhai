// onatopp-dev-hybrid / workflow.rhai
//
// Hybrid dev cycle: Norn (gpt-5.5) for scout+review, Claude Runner (Opus 4.7) for dev.
// Uses enrichments arrays for structured output (matching onatopp-dev-lean.yaml pattern).
//
// Pipeline:
//   Scout+Plan (Norn) -> Create Tasks -> Dev (Claude Runner) -> File Scope ->
//   Commit+Fix -> Checks -> [Dev retry if checks fail] ->
//   Review+Harden (Norn) -> [Dev-fix if review fails, max 2] -> Done+Notify
//
// Inputs:
//   brief             - brief JSON (required)
//   design_content    - design document text (optional)
//   checklist_content - checklist JSON for C# resolution (optional)
//   stories_content   - stories JSON for S# resolution (optional)
//   notify            - member name to notify on completion (optional)
//   run-name          - label for notification DM (optional)

let brief = input.brief;
let brief_id = brief.id;
let brief_title = brief.title;

// =====================================================================
// HELPERS
// =====================================================================

// -- Norn in-process helper ------------------------------------------
fn do_norn_step(step_name, profile, prompt, schema_json) {
    let result = run_step_norn(step_name, profile, prompt, schema_json);

    if result.is_error {
        throw step_name + " (norn) failed: " + result.error;
    }

    let output = result.output;
    if output == () {
        throw step_name + " (norn) returned empty output — schema parse may have failed";
    }

    output
}

// -- Enrichments lookup builder --------------------------------------
fn build_lookup(enrichments) {
    let lookup = #{};
    for e in enrichments {
        lookup[e.id] = e;
    }
    lookup
}

// -- Dev status formatter (checklist/stories from dev enrichment) ------
fn format_dev_status(d) {
    let doc = "";
    if d.checklist_status.len() > 0 {
        doc += "\n**Dev checklist status:**\n";
        for c in d.checklist_status { doc += "- " + c.id + ": " + c.note + " (done: " + c.done + ")\n"; }
    }
    if d.stories_status.len() > 0 {
        doc += "\n**Dev stories status:**\n";
        for st in d.stories_status { doc += "- " + st.id + ": " + st.note + " (satisfied: " + st.satisfied + ")\n"; }
    }
    doc
}

// -- Review requirement formatter --------------------------------------
fn format_review_requirement(r, scout_by_id, dev_by_id) {
    let doc = "<requirement id=\"" + r.id + "\">\n";
    doc += "### " + r.id + ": " + r.title + "\n\n";
    doc += r.spec + "\n\n";

    let s = scout_by_id[r.id];
    if s != () {
        doc += "**Related files:** " + to_json(s.related_files) + "\n";
        doc += "**Context:** " + to_json(s.context) + "\n";
        doc += "**References:** " + to_json(s.references) + "\n";
    }

    let d = dev_by_id[r.id];
    if d != () {
        doc += "**Dev implementation:** " + d.how_satisfied + "\n";
        doc += "**Files changed:** " + to_json(d.files_changed) + "\n";
        if d.deviation != () && d.deviation != "" { doc += "**Deviation:** " + d.deviation + "\n"; }
    }

    doc += "\nAcceptance:\n";
    for a in r.acceptance { doc += "- " + a + "\n"; }

    if r.checklist != () { doc += format_list("\nChecklist items:", r.checklist); }
    if r.stories != () { doc += format_list("\nUser stories:", r.stories); }

    if d != () { doc += format_dev_status(d); }

    doc += "</requirement>\n\n";
    doc
}

// -- List formatter (avoids nesting depth inside loops) ----------------
fn format_list(label, items) {
    let out = label;
    for item in items { out += " " + item; }
    out += "\n";
    out
}

// -- Single requirement formatter --------------------------------------
fn format_requirement(r) {
    let doc = "<requirement id=\"" + r.id + "\">\n";
    doc += "### " + r.id + ": " + r.title + "\n\n";
    doc += r.spec + "\n\n";

    if r.files != () {
        if r.files.create != () { doc += format_list("Files to create:", r.files.create); }
        if r.files.modify != () { doc += format_list("Files to modify:", r.files.modify); }
        if r.files.delete != () { doc += format_list("Files to delete:", r.files.delete); }
    }

    doc += "\nAcceptance:\n";
    for a in r.acceptance { doc += "- " + a + "\n"; }

    if r.checklist != () { doc += format_list("\nChecklist items:", r.checklist); }
    if r.stories != () { doc += format_list("\nUser stories:", r.stories); }
    doc += "</requirement>\n\n";
    doc
}

// -- Brief document builder ------------------------------------------
fn build_brief_doc(brief) {
    let doc = "# " + brief.id + ": " + brief.title + "\n\n";
    doc += "## Purpose\n\n" + brief.purpose + "\n\n";
    doc += "## Task\n\n" + brief.task + "\n\n";
    doc += "## Requirements\n\n";

    for r in brief.requirements {
        doc += format_requirement(r);
    }

    if brief.boundaries != () {
        doc += "## Boundaries\n\n";
        for b in brief.boundaries { doc += "- " + b + "\n"; }
        doc += "\n";
    }

    if brief.verification != () {
        doc += "## Verification\n\n";
        for v in brief.verification { doc += "- " + v + "\n"; }
    }

    doc
}

// -- Collect allowed files from a single requirement -------------------
fn collect_allowed_files(r, allowed) {
    if r.files == () { return; }
    if r.files.create != () {
        for f in r.files.create { allowed[f] = true; }
    }
    if r.files.modify != () {
        for f in r.files.modify {
            allowed[f] = true;
            if f.ends_with("Cargo.toml") { allowed["Cargo.lock"] = true; }
        }
    }
}

// -- Revert/delete a single out-of-scope file -------------------------
fn enforce_single_file(marker, file, reverted, deleted) {
    if marker == "??" {
        run_cmd("rm -f " + file);
        deleted.push(file);
    } else {
        run_cmd("git checkout -- " + file);
        reverted.push(file);
    }
}

// -- File scope enforcement ------------------------------------------
fn enforce_file_scope(brief) {
    let allowed = #{};
    allowed["Cargo.lock"] = true;

    if brief.requirements != () {
        for r in brief.requirements {
            collect_allowed_files(r, allowed);
        }
    }

    if allowed.len() <= 1 { return; }

    let status = run_cmd("git status --porcelain");
    if status.stdout.len() == 0 { return; }

    let lines = status.stdout.split('\n');
    let reverted = [];
    let deleted = [];

    for line in lines {
        if line.len() < 4 { continue; }
        let marker = line.sub_string(0, 2);
        let file = line.sub_string(3);
        if allowed.contains(file) { continue; }
        enforce_single_file(marker, file, reverted, deleted);
    }

    if reverted.len() > 0 || deleted.len() > 0 {
        let log = "Scope enforcement for " + brief.id + ":\n";
        if reverted.len() > 0 {
            log += "Reverted " + reverted.len() + " modified files:\n";
            for f in reverted { log += "  - " + f + "\n"; }
        }
        if deleted.len() > 0 {
            log += "Deleted " + deleted.len() + " untracked files:\n";
            for f in deleted { log += "  - " + f + "\n"; }
        }
        print(log);
    }
}

// -- Commit helper ---------------------------------------------------
fn do_commit(message) {
    write_file(".commit-msg.tmp", message);
    let result = run_cmd("bash .meridian/workflows/onatopp-dev-hybrid/scripts/commit-and-report.sh .commit-msg.tmp");
    run_cmd("rm -f .commit-msg.tmp");

    if result.exit_code != 0 {
        let err = result.stderr;
        if err.len() == 0 { err = "exit " + result.exit_code + ", stdout: " + result.stdout; }
        throw "commit failed: " + err;
    }

    parse_json(result.stdout)
}

// -- Check report builder --------------------------------------------
fn format_checklist_section(s) {
    let doc = "**" + s.name + ":**\n";
    for item in s.items { doc += "- " + item.id + ": " + item.text + "\n"; }
    doc
}

fn build_checklist_doc(checklist_content) {
    if checklist_content == () { return ""; }
    if checklist_content.sections == () { return ""; }
    let doc = "<document>\n<source>checklist_reference</source>\n<document_content>\n";
    for s in checklist_content.sections { doc += format_checklist_section(s); }
    doc += "</document_content>\n</document>\n";
    doc
}

fn format_stories_persona(p) {
    let doc = "**" + p.name + " - " + p.role + ":**\n";
    for story in p.stories { doc += "- " + story.id + ": " + story.text + "\n"; }
    doc
}

fn build_stories_doc(stories_content) {
    if stories_content == () { return ""; }
    if stories_content.personas == () { return ""; }
    let doc = "<document>\n<source>stories_reference</source>\n<document_content>\n";
    for p in stories_content.personas { doc += format_stories_persona(p); }
    doc += "</document_content>\n</document>\n";
    doc
}

// -- Schema builders -------------------------------------------------

fn build_scout_schema() {
    let str_type = #{"type": "string"};
    let str_arr = #{"type": "array", "items": str_type};

    let e_id = #{"type": "string", "description": "R# id from the brief (e.g. R1, R2)."};
    let e_rf = #{"type": "array", "items": str_type, "description": "Existing files relevant to this R# (path + brief note on why)."};
    let e_ctx = #{"type": "array", "items": str_type, "description": "Key findings: sibling conventions, type signatures, module layout."};
    let e_ref = #{"type": "array", "items": str_type, "description": "Files to reference during implementation (path:line-range)."};
    let e_app = #{"type": "string", "description": "How to implement this requirement."};
    let e_rsn = #{"type": "string", "description": "Why this approach — trade-offs considered, alternatives rejected."};
    let enrichment_props = #{"id": e_id, "related_files": e_rf, "context": e_ctx, "references": e_ref, "approach": e_app, "reasoning": e_rsn};
    let enrichment_item = #{"type": "object", "properties": enrichment_props, "required": ["id", "related_files", "context", "references", "approach", "reasoning"], "additionalProperties": false};

    let t_id = #{"type": "string", "description": "Sequential number only: 1, 2, 3 — no prefixes."};
    let t_subj = #{"type": "string", "description": "Short task title (imperative, under 80 chars)."};
    let t_desc = #{"type": "string", "description": "Specific enough to execute without questions."};
    let t_blocks = #{"type": "array", "items": str_type, "description": "Task IDs this task blocks."};
    let t_blocked = #{"type": "array", "items": str_type, "description": "Task IDs that block this task."};
    let task_props = #{"id": t_id, "subject": t_subj, "description": t_desc, "blocks": t_blocks, "blockedBy": t_blocked};
    let task_item = #{"type": "object", "properties": task_props, "required": ["id", "subject", "description", "blocks", "blockedBy"], "additionalProperties": false};

    let p_summary = #{"type": "string", "description": "2-3 sentences orienting the implementer."};
    let p_enrich = #{"type": "array", "description": "One entry per R# — codebase context for the implementer.", "items": enrichment_item};
    let p_tasks = #{"type": "array", "description": "Ordered implementation tasks.", "items": task_item};
    let p_verify = #{"type": "array", "items": str_type, "description": "Concrete checks to run after implementation."};
    let props = #{"summary": p_summary, "enrichments": p_enrich, "tasks": p_tasks, "verification_items": p_verify};
    to_json(#{"type": "object", "properties": props, "required": ["summary", "enrichments", "tasks", "verification_items"], "additionalProperties": false})
}

fn build_dev_schema() {
    let fc_path = #{"type": "string"};
    let fc_change = #{"type": "string", "enum": ["created", "modified", "deleted"]};
    let fc_note = #{"type": "string"};
    let fc_props = #{"path": fc_path, "change": fc_change, "note": fc_note};
    let file_change_item = #{"type": "object", "properties": fc_props, "required": ["path", "change", "note"], "additionalProperties": false};

    let cl_props = #{"id": #{"type": "string"}, "done": #{"type": "boolean"}, "note": #{"type": "string"}};
    let checklist_item = #{"type": "object", "properties": cl_props, "required": ["id", "done", "note"], "additionalProperties": false};

    let st_props = #{"id": #{"type": "string"}, "satisfied": #{"type": "boolean"}, "note": #{"type": "string"}};
    let story_item = #{"type": "object", "properties": st_props, "required": ["id", "satisfied", "note"], "additionalProperties": false};

    let e_id = #{"type": "string", "description": "R# id from the brief."};
    let e_status = #{"type": "string", "enum": ["implemented", "blocked"]};
    let e_files = #{"type": "array", "items": file_change_item};
    let e_how = #{"type": "string", "description": "How this requirement was met."};
    let e_dev = #{"type": "string", "description": "Empty if followed plan. Otherwise: what changed and why."};
    let e_cl = #{"type": "array", "items": checklist_item};
    let e_st = #{"type": "array", "items": story_item};
    let enrichment_props = #{"id": e_id, "status": e_status, "files_changed": e_files, "how_satisfied": e_how, "deviation": e_dev, "checklist_status": e_cl, "stories_status": e_st};
    let enrichment_item = #{"type": "object", "properties": enrichment_props, "required": ["id", "status", "files_changed", "how_satisfied", "deviation", "checklist_status", "stories_status"], "additionalProperties": false};

    let att_np = #{"type": "boolean", "description": "No unwrap/expect/panic/todo in library code."};
    let att_nu = #{"type": "boolean", "description": "No unsafe blocks added."};
    let att_br = #{"type": "boolean", "description": "All SHALL NOT boundaries observed."};
    let att_tc = #{"type": "boolean", "description": "Tests added or existing tests still pass."};
    let att_props = #{"no_panics": att_np, "no_unsafe": att_nu, "boundaries_respected": att_br, "tests_considered": att_tc};
    let attestation = #{"type": "object", "properties": att_props, "required": ["no_panics", "no_unsafe", "boundaries_respected", "tests_considered"], "additionalProperties": false};

    let p_summary = #{"type": "string", "description": "1-2 sentences on what was done."};
    let p_commit = #{"type": "string", "description": "Conventional-commits style."};
    let p_enrich = #{"type": "array", "description": "One entry per R# — how it was implemented.", "items": enrichment_item};
    let props = #{"summary": p_summary, "commit_message": p_commit, "enrichments": p_enrich, "attestation": attestation};
    to_json(#{"type": "object", "properties": props, "required": ["summary", "commit_message", "enrichments", "attestation"], "additionalProperties": false})
}

fn build_review_schema() {
    let str_type = #{"type": "string"};
    let str_arr = #{"type": "array", "items": str_type};

    let re_id = #{"type": "string"};
    let re_align = #{"type": "string", "enum": ["aligned", "drifted", "fixed"], "description": "Does the implementation match the design intent?"};
    let re_acc = #{"type": "boolean"};
    let enrichment_props = #{"id": re_id, "alignment": re_align, "acceptance_met": re_acc, "checklist_ticked": str_arr, "stories_satisfied": str_arr, "issues": str_arr, "fixes_applied": str_arr};
    let enrichment_item = #{"type": "object", "properties": enrichment_props, "required": ["id", "alignment", "acceptance_met", "checklist_ticked", "stories_satisfied", "issues", "fixes_applied"], "additionalProperties": false};

    let v_props = #{"criterion": str_type, "passed": #{"type": "boolean"}, "note": str_type};
    let verification_item = #{"type": "object", "properties": v_props, "required": ["criterion", "passed", "note"], "additionalProperties": false};

    let p_summary = #{"type": "string"};
    let p_commit = #{"type": "string"};
    let p_pass = #{"type": "boolean", "description": "True only if all criteria met and no blocking issues remain."};
    let p_enrich = #{"type": "array", "description": "One entry per R# — combined review and hardening verdict.", "items": enrichment_item};
    let p_verify = #{"type": "array", "items": verification_item};
    let props = #{"summary": p_summary, "commit_message": p_commit, "pass": p_pass, "enrichments": p_enrich, "verification_results": p_verify};
    to_json(#{"type": "object", "properties": props, "required": ["summary", "commit_message", "pass", "enrichments", "verification_results"], "additionalProperties": false})
}

// -- Build scout context section for dev instruction -------------------
fn build_scout_context(brief, scout_by_id) {
    let doc = "## Scout Context (per R#)\n\n";
    for r in brief.requirements {
        let s = scout_by_id[r.id];
        if s != () {
            doc += "### " + r.id + "\n";
            doc += "**Related files:** " + to_json(s.related_files) + "\n";
            doc += "**Context:** " + to_json(s.context) + "\n";
            doc += "**References:** " + to_json(s.references) + "\n";
            doc += "**Approach:** " + s.approach + "\n";
            doc += "**Reasoning:** " + s.reasoning + "\n\n";
        } else {
            doc += "### " + r.id + " (no scout context)\n\n";
        }
    }
    doc
}

// -- Collect blocking issues from review enrichments -------------------
fn collect_blocking_issues(brief, review_by_id) {
    let report = "";
    for r in brief.requirements {
        let rv = review_by_id[r.id];
        if rv != () {
            for issue in rv.issues { report += "- [" + r.id + "] " + issue + "\n"; }
        }
    }
    if report != "" { report = "## Review Findings (blocking)\n\n" + report; }
    report
}

// -- Report: format files changed for a requirement --------------------
fn format_report_files(d) {
    let out = "";
    if d.files_changed.len() > 0 {
        out += "  Files:";
        for f in d.files_changed { out += " " + f.path + " (" + f.change + ")"; }
        out += "\n";
    }
    out
}

// -- Report: format review details for a requirement -------------------
fn format_report_review(rv) {
    let out = "";
    if rv.issues.len() > 0 { out += format_list("  Issues:", rv.issues); }
    if rv.fixes_applied.len() > 0 { out += format_list("  Fixes:", rv.fixes_applied); }
    if rv.checklist_ticked.len() > 0 { out += format_list("  Checklist:", rv.checklist_ticked); }
    if rv.stories_satisfied.len() > 0 { out += format_list("  Stories:", rv.stories_satisfied); }
    out
}

// -- Report: single requirement ----------------------------------------
fn format_report_requirement(r, d, rv) {
    let out = r.id + ": " + r.title + "\n";
    if d != () && rv != () {
        let acc = if rv.acceptance_met { "met" } else { "not met" };
        out += "  Status: " + d.status + " | Alignment: " + rv.alignment + " | Acceptance: " + acc + "\n";
        out += "  How: " + d.how_satisfied + "\n";
        if d.deviation != () && d.deviation != "" { out += "  Deviation: " + d.deviation + "\n"; }
        out += format_report_files(d);
        out += format_report_review(rv);
    } else if d != () {
        out += "  Dev: " + d.status + " — " + d.how_satisfied + "\n";
    } else {
        out += "  (no data available)\n";
    }
    out += "\n";
    out
}

// -- Report: verification section --------------------------------------
fn format_report_verification(review) {
    let out = "";
    if review.verification_results.len() > 0 {
        out += "--- Verification ---\n\n";
        for v in review.verification_results {
            let mark = if v.passed { "PASS" } else { "FAIL" };
            out += "  " + v.criterion + ": " + mark;
            if v.note != () && v.note != "" { out += " — " + v.note; }
            out += "\n";
        }
    }
    out
}

// -- Notification report builder -------------------------------------
fn build_report(brief, dev, dev_by_id, review, review_by_id) {
    let status = if review.pass { "PASSED" } else { "FAILED" };
    let report = "onatopp-dev-hybrid: " + brief.id + " — " + brief.title + "\n";
    report += "Status: " + status + "\n\n";
    report += "Dev: " + dev.summary + "\n";
    report += "Review: " + review.summary + "\n\n";
    report += "--- Requirements ---\n\n";

    for r in brief.requirements {
        report += format_report_requirement(r, dev_by_id[r.id], review_by_id[r.id]);
    }

    report += format_report_verification(review);
    report
}

// =====================================================================
// BUILD CONTEXT
// =====================================================================

let design_block = "";
if input.design_content != () {
    design_block = "<document>\n<source>design_document</source>\n<document_content>\n"
        + input.design_content
        + "\n</document_content>\n</document>\n";
}

let checklist_doc = build_checklist_doc(input.checklist_content);
let stories_doc = build_stories_doc(input.stories_content);
let brief_doc = build_brief_doc(brief);

// =====================================================================
// TRY-CATCH — notification on failure
// =====================================================================

try {

// =====================================================================
// PRE-WARM BUILD CACHE
// =====================================================================

// Kick off a workspace build in the background so the target directory
// is warm before the dev step's first edit triggers diagnostics. The
// scout is read-only so this runs safely in parallel.
run_cmd("cargo check --workspace --all-targets >/dev/null 2>&1 &");

// =====================================================================
// SCOUT + PLAN (Norn)
// =====================================================================

let scout_schema = build_scout_schema();

let scout_instruction = "Explore the codebase and plan the implementation for this brief. You are read-only - do not modify files.\n\n"
    + "PHASE 1 - SCOUT: For each R#, find related files, conventions, type signatures, and specific file:line-range references the implementer should look at.\n\n"
    + "PHASE 2 - PLAN: Using the context you just gathered, produce ordered implementation tasks. Be specific - another agent executes these without asking questions. Also produce verification_items: concrete checks to run after implementation.\n\n"
    + "<documents>\n"
    + design_block
    + "<document>\n<source>brief</source>\n<document_content>\n"
    + brief_doc
    + "\n</document_content>\n</document>\n"
    + "</documents>";

let scout = do_norn_step("scout", "norn-codebase-explorer", scout_instruction, scout_schema);
print("Scout complete: " + scout.summary);

let scout_by_id = build_lookup(scout.enrichments);

// =====================================================================
// CREATE TASKS
// =====================================================================

let home_result = run_cmd("printf '%s' \"$HOME\"");
let home = home_result.stdout;
let task_dir = home + "/.claude/tasks/" + brief_id;
run_cmd("rm -rf " + task_dir + " && mkdir -p " + task_dir);

for task in scout.tasks {
    task.status = "pending";
    task.activeForm = task.subject;
    write_json(task_dir + "/" + task.id + ".json", task);
}
print("Created " + scout.tasks.len() + " tasks");

// =====================================================================
// DEV (Claude Runner)
// =====================================================================

let dev_schema = build_dev_schema();
let check_report = "";
let deferred_clippy_report = "";
let is_resume = false;
let dev;
let dev_by_id = #{};

loop {
    let dev_instruction = "";

    if check_report != "" {
        dev_instruction += "CHECKS FAILED - fix only the specific failures below. Do not extend scope.\n\n"
            + check_report + "\n\n";
    }

    if !is_resume {
        dev_instruction += "Implement the brief. Your task list has been populated — use TaskList to see tasks and TaskUpdate to mark them complete as you go.\n\n";

        dev_instruction += build_scout_context(brief, scout_by_id);

        dev_instruction += "<documents>\n" + design_block;
        dev_instruction += "<document>\n<source>brief</source>\n<document_content>\n"
            + brief_doc + "\n</document_content>\n</document>\n";
        dev_instruction += checklist_doc + stories_doc;
        dev_instruction += "<document>\n<source>verification_items</source>\n<document_content>\n";
        for v in scout.verification_items { dev_instruction += "- " + v + "\n"; }
        dev_instruction += "</document_content>\n</document>\n";
        dev_instruction += "<document>\n<source>scout_summary</source>\n<document_content>\n"
            + scout.summary + "\n</document_content>\n</document>\n";
        dev_instruction += "</documents>\n\n";

        dev_instruction += "---\n\n";
        dev_instruction += "For each R#, report: status, files changed (with notes), how satisfied, any deviation from plan. ";
        dev_instruction += "For each checklist item (C#) and user story (S#) assigned to the R#, report whether it was delivered.\n\n";
        dev_instruction += "Before submitting, attest: no panics/unwraps in library code, no unsafe, boundaries respected, tests considered.";
    }

    let step_name = if is_resume { "dev-retry" } else { "dev" };
    let dev_result = run_step(step_name, "workflow-developer", dev_instruction, dev_schema, brief_id);

    if dev_result.is_error {
        throw "Dev failed: " + dev_result.error;
    }

    dev = dev_result;
    if dev.summary == () {
        throw "Dev step returned no structured output — schema extraction failed";
    }
    print("Dev complete: " + dev.summary);

    dev_by_id = build_lookup(dev.enrichments);

    // =====================================================================
    // FILE SCOPE ENFORCEMENT
    // =====================================================================

    enforce_file_scope(brief);

    // =====================================================================
    // COMMIT + FIX
    // =====================================================================

    let commit_info = do_commit(dev.commit_message);
    if commit_info.committed {
        print("Committed: " + dev.commit_message);
    }

    // =====================================================================
    // RUN CHECKS (inline — matches run-checks-triaged suite)
    // =====================================================================

    let checks_result = run_cmd(
        "bash .meridian/workflows/onatopp-dev-hybrid/scripts/run-checks.sh "
        + "'" + commit_info.crates_affected + "' "
        + "'" + commit_info.changed_files + "'"
    );

    let checks;
    if checks_result.exit_code == 0 {
        checks = parse_json(checks_result.stdout);
    } else {
        checks = #{
            "pass": false,
            "summary": "check script failed",
            "failure_report": checks_result.stdout + "\n" + checks_result.stderr,
            "deferred_clippy_report": ""
        };
    }

    deferred_clippy_report = checks.deferred_clippy_report;

    if checks.pass {
        print("All checks passed: " + checks.summary);
        break;
    }

    if is_resume {
        throw "Checks failed on retry - aborting: " + checks.summary;
    }

    print("Checks failed: " + checks.summary + " - sending back to dev");
    check_report = checks.failure_report;
    is_resume = true;
}

// =====================================================================
// REVIEW + HARDEN (Norn) — max 2 attempts with dev-fix loop
// =====================================================================

let review_schema = build_review_schema();
let max_review_attempts = 2;
let review_passed = false;
let review;
let review_by_id = #{};

let review_attempt = 0;
loop {
    review_attempt += 1;

    let review_instruction = "Review and harden the implementation in a single pass. You have two jobs:\n\n";
    review_instruction += "JOB 1 - HARDEN: Verify that what was built matches the design philosophy, naming conventions, error handling patterns, and outcomes the design demands. Fix anything that drifts — naming, error handling, convention violations, missing edge cases. Use Edit and Write to make corrections directly. Only flag issues you cannot fix yourself (architectural changes, missing dependencies, scope decisions).\n\n";

    if deferred_clippy_report != "" {
        review_instruction += "DEFERRED CLIPPY LINTS - style and performance issues that were not blocking but need addressing:\n\n"
            + deferred_clippy_report + "\n\n";
    }

    review_instruction += "JOB 2 - REVIEW: Verify acceptance criteria for each R#. Tick off checklist items. Confirm user stories addressed. Check the ACTUAL CODE (use git diff HEAD~3), not the dev summary.\n\n";

    review_instruction += "<documents>\n" + design_block;
    review_instruction += "<document>\n<source>brief (fully enriched)</source>\n<document_content>\n";
    review_instruction += "# " + brief_id + ": " + brief_title + "\n\n";
    review_instruction += "## Requirements (enriched with scout context + dev implementation)\n\n";

    for r in brief.requirements {
        review_instruction += format_review_requirement(r, scout_by_id, dev_by_id);
    }

    review_instruction += "## Boundaries\n\n";
    if brief.boundaries != () {
        for b in brief.boundaries { review_instruction += "- " + b + "\n"; }
    }
    review_instruction += "</document_content>\n</document>\n";

    review_instruction += checklist_doc + stories_doc;

    review_instruction += "<document>\n<source>dev_attestation</source>\n<document_content>\n"
        + "panics=" + dev.attestation.no_panics
        + ", unsafe=" + dev.attestation.no_unsafe
        + ", boundaries=" + dev.attestation.boundaries_respected
        + ", tests=" + dev.attestation.tests_considered
        + "\n</document_content>\n</document>\n";

    review_instruction += "<document>\n<source>verification_criteria</source>\n<document_content>\n";
    if brief.verification != () {
        for v in brief.verification { review_instruction += "- " + v + "\n"; }
    }
    for v in scout.verification_items { review_instruction += "- " + v + "\n"; }
    review_instruction += "</document_content>\n</document>\n";
    review_instruction += "</documents>\n\n";

    review_instruction += "---\n\n";
    review_instruction += "For each R#: assess alignment (aligned/drifted/fixed), verify acceptance criteria, tick checklist items, confirm stories. Fix issues directly where possible — report what you fixed and what remains unfixable.\n\n";
    review_instruction += "Set pass=true only if all acceptance criteria are met and no blocking issues remain. If you fixed issues during this review, that counts — pass=true means the code is correct NOW, after your fixes.";

    review = do_norn_step("review-" + review_attempt, "norn-reviewer", review_instruction, review_schema);
    print("Review " + review_attempt + " verdict: " + review.pass);
    print("Review summary: " + review.summary);

    review_by_id = build_lookup(review.enrichments);

    if review.pass {
        review_passed = true;
        break;
    }

    if review_attempt >= max_review_attempts {
        print("Review failed after " + max_review_attempts + " attempts");
        break;
    }

    // The reviewer fixes issues directly during review (review+harden+fix).
    // No dev retry — the reviewer has full context and tools. Commit fixes
    // and loop back for another review pass to verify the fixes landed.
    let blocking = collect_blocking_issues(brief, review_by_id);
    if blocking == "" { break; }

    enforce_file_scope(brief);
    do_commit("fix: address review findings for " + brief_id);
}

// =====================================================================
// DONE + NOTIFY
// =====================================================================

let status = if review_passed { "PASSED" } else { "FAILED" };
print("Workflow complete for " + brief_id + ": " + brief_title + " - " + status);

if input.notify != () {
    let run_name = if input["run-name"] != () { input["run-name"] } else { brief_id };
    let report = build_report(brief, dev, dev_by_id, review, review_by_id);
    write_file(".report.tmp", report);
    run_cmd("collective send --as Meridian --to '" + input.notify + "' --subject 'workflow complete: " + run_name + "' --message \"$(cat .report.tmp)\"");
    run_cmd("rm -f .report.tmp");
}

} catch (err) {
    print("Workflow failed: " + err);
    if input.notify != () {
        let run_name = if input["run-name"] != () { input["run-name"] } else { brief_id };
        run_cmd("collective send --as Meridian --to '" + input.notify + "' --subject 'workflow FAILED: " + run_name + "' --message 'onatopp-dev-hybrid failed for " + brief_id + ": " + err + "'");
    }
    throw err;
}
