// onatopp-dev-norn / workflow.rhai
//
// All-Norn dev cycle: every step runs in-process via Norn (gpt-5.5).
// Progressive enrichment: each step adds a sub-object to the requirements array.
//
// Pipeline:
//   Scout (Norn) -> Dev (Norn) -> Commit -> Review+Harden (Norn) -> Commit -> Done+Notify
//
// Inputs:
//   brief             - brief JSON (required)
//   design_content    - design JSON (design.schema.json format, optional)
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

fn do_norn_step(step_name, profile, prompt, schema_json) {
    let result = run_step_norn(step_name, profile, prompt, schema_json);
    if result.is_error {
        throw step_name + " failed: " + result.error;
    }
    let output = result.output;
    if output == () {
        throw step_name + " returned empty output";
    }
    if result.session_id != () {
        output.session_id = result.session_id;
    }
    output
}

fn add_unique(arr, item) {
    for existing in arr {
        if existing == item { return; }
    }
    arr.push(item);
}

fn crate_from_path(path) {
    let parts = path.split("/");
    if parts.len() >= 2 && parts[0] == "crates" {
        return parts[1];
    }
    return "";
}

fn affected_crates_from_brief(brief) {
    let crates = [];
    for r in brief.requirements {
        if r.files == () { continue; }
        if r.files.create != () {
            for p in r.files.create {
                let c = crate_from_path(p);
                if c != "" { add_unique(crates, c); }
            }
        }
        if r.files.modify != () {
            for p in r.files.modify {
                let c = crate_from_path(p);
                if c != "" { add_unique(crates, c); }
            }
        }
        if r.files.delete != () {
            for p in r.files.delete {
                let c = crate_from_path(p);
                if c != "" { add_unique(crates, c); }
            }
        }
    }
    crates
}

fn verify_command_for_crates(crates) {
    if crates.len() == 0 {
        return "cargo check --workspace && cargo clippy --workspace -- -D warnings";
    }
    let check = "cargo check";
    let clippy = "cargo clippy";
    for c in crates {
        check += " -p " + c;
        clippy += " -p " + c;
    }
    clippy += " -- -D warnings";
    check + " && " + clippy
}

fn build_lookup(items) {
    let lookup = #{};
    for item in items { lookup[item.id] = item; }
    lookup
}

// -- C# and S# resolution: embed the actual text from checklist/stories --

fn resolve_checklist(brief, checklist_content) {
    if checklist_content == () { return; }
    if checklist_content.sections == () { return; }
    let lookup = #{};
    for s in checklist_content.sections {
        for item in s.items { lookup[item.id] = item.text; }
    }
    for r in brief.requirements {
        if r.checklist == () { continue; }
        let resolved = [];
        for c_id in r.checklist {
            let text = lookup[c_id];
            if text != () {
                resolved.push(#{id: c_id, text: text});
            } else {
                resolved.push(#{id: c_id, text: ""});
            }
        }
        r.checklist = resolved;
    }
}

fn resolve_stories(brief, stories_content) {
    if stories_content == () { return; }
    if stories_content.personas == () { return; }
    let lookup = #{};
    for p in stories_content.personas {
        for story in p.stories { lookup[story.id] = story.text; }
    }
    for r in brief.requirements {
        if r.stories == () { continue; }
        let resolved = [];
        for s_id in r.stories {
            let text = lookup[s_id];
            if text != () {
                resolved.push(#{id: s_id, text: text});
            } else {
                resolved.push(#{id: s_id, text: ""});
            }
        }
        r.stories = resolved;
    }
}

// -- Design context: extract key fields from structured design JSON -----

fn build_design_context(design, brief) {
    if design == () { return ""; }

    let doc = "## Design Context\n\n";

    if design.intention != () {
        doc += "**Intention:** " + design.intention + "\n\n";
    }

    if design.constraints != () && design.constraints.len() > 0 {
        doc += "**Constraints:**\n";
        for c in design.constraints {
            doc += "- " + c.id + ": " + c.description + "\n";
        }
        doc += "\n";
    }

    if design.goals != () && design.goals.len() > 0 {
        doc += "**Goals:**\n";
        for g in design.goals { doc += "- " + g + "\n"; }
        doc += "\n";
    }

    if design.decisions != () && design.decisions.len() > 0 {
        let anchor = if brief.design_anchor != () { brief.design_anchor } else { "" };
        let has_filter = anchor != "";

        doc += "**Key Decisions:**\n";
        for d in design.decisions {
            if d.status != "active" { continue; }
            if has_filter && !anchor.contains(d.id) { continue; }
            doc += "- " + d.id + ": " + d.title;
            if d.choice != () { doc += " — " + d.choice; }
            doc += "\n";
        }
        doc += "\n";
    }

    doc
}

// -- Requirement rendering helpers --------------------------------------

fn format_list(label, items) {
    let out = label;
    for item in items { out += " " + item; }
    out += "\n";
    out
}

fn render_requirement(r) {
    let doc = "### " + r.id + ": " + r.title + "\n\n";
    doc += r.spec + "\n\n";

    if r.files != () {
        if r.files.create != () && r.files.create.len() > 0 {
            doc += format_list("Create:", r.files.create);
        }
        if r.files.modify != () && r.files.modify.len() > 0 {
            doc += format_list("Modify:", r.files.modify);
        }
        if r.files.delete != () && r.files.delete.len() > 0 {
            doc += format_list("Delete:", r.files.delete);
        }
    }

    doc += "\nAcceptance:\n";
    for a in r.acceptance { doc += "- " + a + "\n"; }

    if r.checklist != () && r.checklist.len() > 0 {
        doc += "\nChecklist:\n";
        for c in r.checklist {
            if type_of(c) == "map" { doc += "- " + c.id + ": " + c.text + "\n"; }
            else { doc += "- " + c + "\n"; }
        }
    }
    if r.stories != () && r.stories.len() > 0 {
        doc += "\nStories:\n";
        for s in r.stories {
            if type_of(s) == "map" { doc += "- " + s.id + ": " + s.text + "\n"; }
            else { doc += "- " + s + "\n"; }
        }
    }
    doc += "\n";
    doc
}

fn render_enriched_requirement(r, scout_by_id) {
    let doc = render_requirement(r);

    let s = scout_by_id[r.id];
    if s != () {
        doc += "**Scout context:**\n";
        if s.files != () && s.files.len() > 0 {
            doc += "- Key files: " + to_json(s.files) + "\n";
        }
        if s.context != () && s.context.len() > 0 {
            for c in s.context { doc += "- " + c + "\n"; }
        }
        if s.approach != () { doc += "- Approach: " + s.approach + "\n"; }
        if s.notes != () && s.notes != "" { doc += "- Notes: " + s.notes + "\n"; }
        doc += "\n";
    }
    doc
}

fn render_review_requirement(r, scout_by_id, dev_by_id) {
    let doc = render_requirement(r);

    let s = scout_by_id[r.id];
    if s != () {
        doc += "**Scout:** ";
        if s.approach != () { doc += s.approach; }
        doc += "\n";
    }

    let d = dev_by_id[r.id];
    if d != () {
        doc += "**Dev:** " + d.status + " — " + d.how + "\n";
        if d.files_changed != () && d.files_changed.len() > 0 {
            doc += "Files changed: " + to_json(d.files_changed) + "\n";
        }
        if d.deviation != () && d.deviation != "" {
            doc += "Deviation: " + d.deviation + "\n";
        }
        if d.checklist != () && d.checklist.len() > 0 {
            doc += "Checklist: ";
            for c in d.checklist {
                let mark = if c.done { "done" } else { "not done" };
                doc += c.id + " (" + mark + ") ";
            }
            doc += "\n";
        }
        if d.stories != () && d.stories.len() > 0 {
            doc += "Stories: ";
            for st in d.stories {
                let mark = if st.satisfied { "satisfied" } else { "not satisfied" };
                doc += st.id + " (" + mark + ") ";
            }
            doc += "\n";
        }
    }
    doc += "\n";
    doc
}

// -- Commit helper -------------------------------------------------------

fn do_commit(message) {
    let status = run_cmd("git status --porcelain");
    if status.stdout.len() == 0 { return #{ committed: false }; }

    write_file(".commit-msg.tmp", message);
    let result = run_cmd("bash .meridian/workflows/onatopp-dev-norn/scripts/commit-and-report.sh .commit-msg.tmp");
    run_cmd("rm -f .commit-msg.tmp");

    if result.exit_code != 0 {
        let err = result.stderr;
        if err.len() == 0 { err = "exit " + result.exit_code; }
        throw "commit failed: " + err;
    }
    parse_json(result.stdout)
}

// -- Schema builders -----------------------------------------------------

fn build_scout_schema() {
    let str_type = #{ type: "string" };
    let str_arr = #{ type: "array", items: str_type };

    let enrichment_props = #{
        id: #{ type: "string", description: "R# id from the brief." },
        files: #{ type: "array", items: str_type, description: "Key files relevant to this R# (path:line-range — brief note). 2-5 per R#, not exhaustive." },
        context: #{ type: "array", items: str_type, description: "Key findings: conventions to match, type signatures, gotchas. 2-4 per R#." },
        approach: #{ type: "string", description: "How to implement this R# — one paragraph." },
        notes: #{ type: "string", description: "Anything non-obvious: edge cases, integration gotchas, things the brief might not have considered. Empty if none." }
    };
    let enrichment_item = #{ type: "object", properties: enrichment_props, required: ["id", "files", "context", "approach", "notes"], additionalProperties: false };

    let props = #{
        summary: #{ type: "string", description: "2-3 sentences orienting the implementer." },
        enrichments: #{ type: "array", description: "One entry per R#.", items: enrichment_item },
        verification: #{ type: "array", items: str_type, description: "Concrete checks to run after implementation." }
    };
    to_json(#{ type: "object", properties: props, required: ["summary", "enrichments", "verification"], additionalProperties: false })
}

fn build_dev_schema() {
    let fc_props = #{
        path: #{ type: "string" },
        change: #{ type: "string", enum: ["created", "modified", "deleted"] },
        note: #{ type: "string" }
    };
    let file_change = #{ type: "object", properties: fc_props, required: ["path", "change", "note"], additionalProperties: false };

    let cl_props = #{ id: #{ type: "string" }, done: #{ type: "boolean" }, note: #{ type: "string" } };
    let checklist_item = #{ type: "object", properties: cl_props, required: ["id", "done", "note"], additionalProperties: false };

    let st_props = #{ id: #{ type: "string" }, satisfied: #{ type: "boolean" }, note: #{ type: "string" } };
    let story_item = #{ type: "object", properties: st_props, required: ["id", "satisfied", "note"], additionalProperties: false };

    let enrichment_props = #{
        id: #{ type: "string", description: "R# id." },
        status: #{ type: "string", enum: ["implemented", "blocked"] },
        files_changed: #{ type: "array", items: file_change },
        how: #{ type: "string", description: "How this requirement was met." },
        deviation: #{ type: "string", description: "Empty if followed plan. Otherwise: what changed and why." },
        checklist: #{ type: "array", items: checklist_item },
        stories: #{ type: "array", items: story_item }
    };
    let enrichment_item = #{ type: "object", properties: enrichment_props, required: ["id", "status", "files_changed", "how", "deviation", "checklist", "stories"], additionalProperties: false };

    let att_props = #{
        no_panics: #{ type: "boolean", description: "No unwrap/expect/panic/todo in library code." },
        no_unsafe: #{ type: "boolean", description: "No unsafe blocks added." },
        boundaries_respected: #{ type: "boolean", description: "All SHALL NOT boundaries observed." },
        tests_pass: #{ type: "boolean", description: "cargo check + clippy + test pass on affected crates." }
    };
    let attestation = #{ type: "object", properties: att_props, required: ["no_panics", "no_unsafe", "boundaries_respected", "tests_pass"], additionalProperties: false };

    let props = #{
        summary: #{ type: "string", description: "1-2 sentences on what was done." },
        commit_message: #{ type: "string", description: "Conventional-commits style." },
        enrichments: #{ type: "array", description: "One entry per R#.", items: enrichment_item },
        attestation: attestation
    };
    to_json(#{ type: "object", properties: props, required: ["summary", "commit_message", "enrichments", "attestation"], additionalProperties: false })
}

fn build_review_schema() {
    let str_type = #{ type: "string" };
    let str_arr = #{ type: "array", items: str_type };

    let enrichment_props = #{
        id: #{ type: "string" },
        alignment: #{ type: "string", enum: ["aligned", "drifted", "fixed"] },
        acceptance_met: #{ type: "boolean" },
        checklist: str_arr,
        stories: str_arr,
        issues: str_arr,
        fixes: str_arr
    };
    let enrichment_item = #{ type: "object", properties: enrichment_props, required: ["id", "alignment", "acceptance_met", "checklist", "stories", "issues", "fixes"], additionalProperties: false };

    let v_props = #{ criterion: str_type, passed: #{ type: "boolean" }, note: str_type };
    let verification_item = #{ type: "object", properties: v_props, required: ["criterion", "passed", "note"], additionalProperties: false };

    let props = #{
        summary: str_type,
        commit_message: str_type,
        enrichments: #{ type: "array", description: "One entry per R#.", items: enrichment_item },
        verification: #{ type: "array", items: verification_item }
    };
    to_json(#{ type: "object", properties: props, required: ["summary", "commit_message", "enrichments", "verification"], additionalProperties: false })
}

// -- Report builders for notification DM ---------------------------------

fn build_report(brief, dev, dev_by_id, review, review_by_id) {
    let report = "onatopp-dev-norn: " + brief.id + " — " + brief.title + "\n\n";
    report += "Dev: " + dev.summary + "\n";
    report += "Review: " + review.summary + "\n\n";

    for r in brief.requirements {
        let d = dev_by_id[r.id];
        let rv = review_by_id[r.id];
        report += r.id + ": " + r.title + "\n";
        if d != () && rv != () {
            let acc = if rv.acceptance_met { "met" } else { "not met" };
            report += "  " + d.status + " | " + rv.alignment + " | acceptance " + acc + "\n";
            if rv.issues.len() > 0 {
                for issue in rv.issues { report += "  Issue: " + issue + "\n"; }
            }
            if rv.fixes.len() > 0 {
                for fix in rv.fixes { report += "  Fixed: " + fix + "\n"; }
            }
        } else if d != () {
            report += "  " + d.status + " — " + d.how + "\n";
        }
        report += "\n";
    }
    report
}

// =====================================================================
// RESOLVE REFERENCES
// =====================================================================

resolve_checklist(brief, input.checklist_content);
resolve_stories(brief, input.stories_content);

// =====================================================================
// BUILD SHARED CONTEXT
// =====================================================================

let design_context = build_design_context(input.design_content, brief);
let cluster = if brief.cluster != () { brief.cluster } else { "" };
let design_file_ref = if cluster != "" {
    "Full design document: docs/design/" + cluster + "/DESIGN.md\n"
} else { "" };

// =====================================================================
// TRY-CATCH — notification on failure
// =====================================================================

try {

// Pre-warm build cache while scout runs (read-only, safe in parallel)
run_cmd("cargo check --workspace --all-targets >/dev/null 2>&1 &");

// =====================================================================
// SCOUT (Norn)
// =====================================================================

let scout_schema = build_scout_schema();

let scout_instruction = "Explore the codebase and gather implementation context for each R# in this brief. You are read-only — do not modify files.\n\n";
scout_instruction += "For each R#, find:\n";
scout_instruction += "- 2-5 key files the implementer should look at (with line ranges)\n";
scout_instruction += "- Conventions to match (sibling patterns, naming, error handling)\n";
scout_instruction += "- A concrete implementation approach\n";
scout_instruction += "- Any gotchas or edge cases the brief might not have considered\n\n";
scout_instruction += "The implementing agent has the same tools you do — focus on saving them time, not cataloguing every file. Be concise.\n\n";

scout_instruction += design_context;

scout_instruction += "## Brief: " + brief_id + " — " + brief_title + "\n\n";
scout_instruction += brief.purpose + "\n\n";
scout_instruction += "## Requirements\n\n";
for r in brief.requirements {
    scout_instruction += render_requirement(r);
}

if brief.boundaries != () && brief.boundaries.len() > 0 {
    scout_instruction += "## Boundaries\n\n";
    for b in brief.boundaries { scout_instruction += "- " + b + "\n"; }
    scout_instruction += "\n";
}

scout_instruction += design_file_ref;

let scout = do_norn_step("scout", "norn-codebase-explorer", scout_instruction, scout_schema);
print("Scout complete: " + scout.summary);

let scout_by_id = build_lookup(scout.enrichments);

// =====================================================================
// DEV (Norn)
// =====================================================================

let dev_schema = build_dev_schema();
let dev;
let dev_by_id = #{};

let dev_instruction = "Implement every R# in this brief. Do not run verification commands yourself; the workflow will run cargo check and clippy after your implementation and resume your session with any failures.\n\n";

dev_instruction += design_context;

dev_instruction += "## Brief: " + brief_id + " — " + brief_title + "\n\n";
dev_instruction += brief.task + "\n\n";
dev_instruction += "## Requirements\n\n";
for r in brief.requirements {
    dev_instruction += render_enriched_requirement(r, scout_by_id);
}

if brief.boundaries != () && brief.boundaries.len() > 0 {
    dev_instruction += "## Boundaries\n\n";
    for b in brief.boundaries { dev_instruction += "- " + b + "\n"; }
    dev_instruction += "\n";
}

if scout.verification != () && scout.verification.len() > 0 {
    dev_instruction += "## Verification\n\n";
    for v in scout.verification { dev_instruction += "- " + v + "\n"; }
    dev_instruction += "\n";
}

dev_instruction += design_file_ref;
dev_instruction += "\nFor each R#, report: status, files changed, how satisfied, any deviation. For each C# and S# assigned to the R#, report whether delivered. Attest: no panics/unwraps in library code, no unsafe, boundaries respected, tests pass.";

dev = do_norn_step("dev", "norn-developer", dev_instruction, dev_schema);
if dev.summary == () {
    throw "Dev returned no structured output";
}
print("Dev complete: " + dev.summary);
dev_by_id = build_lookup(dev.enrichments);

// =====================================================================
// COMMIT (after dev) + VERIFY/RESUME LOOP
// =====================================================================

let commit_info = do_commit(dev.commit_message);
if commit_info.committed {
    print("Committed: " + dev.commit_message);
}

let affected_crates = affected_crates_from_brief(brief);
let verify_cmd = verify_command_for_crates(affected_crates);
print("Verification command: " + verify_cmd);

let verify_attempt = 1;
let verify_clean = false;
while verify_attempt <= 3 {
    let verify_result = run_cmd(verify_cmd);
    if verify_result.exit_code == 0 {
        verify_clean = true;
        print("Verification clean on attempt " + verify_attempt);
        break;
    }

    if verify_attempt == 3 {
        throw "Verification failed after 3 attempts. stdout:\n" + verify_result.stdout + "\nstderr:\n" + verify_result.stderr;
    }

    let error_msg = "The workflow verification command failed on attempt " + verify_attempt + ".\n\n";
    error_msg += "Command:\n" + verify_cmd + "\n\n";
    error_msg += "stdout:\n" + verify_result.stdout + "\n\n";
    error_msg += "stderr:\n" + verify_result.stderr + "\n\n";
    error_msg += "Fix the implementation in this same session. Return the same dev schema, updating files_changed, summary, commit_message, and attestation.";

    dev = resume_step_norn(dev.session_id, error_msg, dev_schema);
    if dev.summary == () {
        throw "Dev resume returned no structured output";
    }
    print("Dev fixes complete: " + dev.summary);
    dev_by_id = build_lookup(dev.enrichments);

    let fix_commit = do_commit(dev.commit_message);
    if fix_commit.committed {
        print("Committed verification fixes: " + dev.commit_message);
    }

    verify_attempt += 1;
}

if !verify_clean {
    throw "Verification loop exited without clean result";
}

// =====================================================================
// REVIEW + HARDEN (Norn) — single pass
// =====================================================================

let review_schema = build_review_schema();

let review_instruction = "Review and harden the implementation. You have two jobs:\n\n";
review_instruction += "1. HARDEN: Fix naming drift, missing error handling, convention violations, edge cases. Use Edit and Write directly.\n";
review_instruction += "2. REVIEW: Verify acceptance criteria for each R#. Check the ACTUAL CODE (use git diff HEAD~1), not the dev summary. Tick checklist items. Confirm stories.\n\n";

review_instruction += design_context;

review_instruction += "## Brief: " + brief_id + " — " + brief_title + "\n\n";
review_instruction += "## Requirements\n\n";
for r in brief.requirements {
    review_instruction += render_review_requirement(r, scout_by_id, dev_by_id);
}

if brief.boundaries != () && brief.boundaries.len() > 0 {
    review_instruction += "## Boundaries\n\n";
    for b in brief.boundaries { review_instruction += "- " + b + "\n"; }
    review_instruction += "\n";
}

review_instruction += "## Verification Criteria\n\n";
if brief.verification != () {
    for v in brief.verification { review_instruction += "- " + v + "\n"; }
}
if scout.verification != () {
    for v in scout.verification { review_instruction += "- " + v + "\n"; }
}
review_instruction += "\n";

review_instruction += "Dev attestation: panics=" + dev.attestation.no_panics + ", unsafe=" + dev.attestation.no_unsafe + ", boundaries=" + dev.attestation.boundaries_respected + ", tests=" + dev.attestation.tests_pass + "\n\n";

review_instruction += design_file_ref;

let review = do_norn_step("review", "norn-reviewer", review_instruction, review_schema);
print("Review: " + review.summary);
let review_by_id = build_lookup(review.enrichments);

// =====================================================================
// COMMIT (after review — if reviewer made changes)
// =====================================================================

let review_commit = do_commit(review.commit_message);
if review_commit.committed {
    print("Review commit: " + review.commit_message);
}
print("Complete: " + brief_id);

if input.notify != () {
    let run_name = if input["run-name"] != () { input["run-name"] } else { brief_id };
    let report = build_report(brief, dev, dev_by_id, review, review_by_id);
    write_file(".report.tmp", report);
    run_cmd("collective send --as Meridian --to '" + input.notify + "' --subject 'workflow complete: " + run_name + "' --message \"$(cat .report.tmp)\"");
    run_cmd("rm -f .report.tmp");
}

} catch (err) {
    print("Failed: " + err);
    if input.notify != () {
        let run_name = if input["run-name"] != () { input["run-name"] } else { brief_id };
        run_cmd("collective send --as Meridian --to '" + input.notify + "' --subject 'workflow FAILED: " + run_name + "' --message 'onatopp-dev-norn failed for " + brief_id + ": " + err + "'");
    }
    throw err;
}
