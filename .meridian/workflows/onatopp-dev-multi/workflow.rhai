// onatopp-dev-multi / workflow.rhai
//
// Mixed-provider workflow matching onatopp-dev-compact's pipeline.
//
// Provider assignment (--input provider=mixed|claude|codex):
//   mixed (default): scout=Codex, dev=Claude, review=Codex
//   claude: all Claude
//   codex: all Codex
//
// Pipeline: Scout → Dev → Scope-check + Commit(Dev) → Review → Commit(Review) → Output
//
// Inputs:
//   - brief: the brief JSON
//   - design_content: design doc text
//   - provider: "mixed", "claude", or "codex"

let provider = input.provider;

// ── Codex dispatch helper ───────────────────────────────────────────
fn run_codex_step(step_name, prompt, schema_path) {
    let schema_file = ".codex-schema-" + step_name + ".json";
    let prompt_file = ".codex-prompt-" + step_name + ".md";

    let schema = read_json(schema_path);
    schema.additionalProperties = false;
    write_json(schema_file, schema);
    write_file(prompt_file, prompt);

    let result = run_cmd(
        "cat " + prompt_file + " | codex exec --json --output-schema " + schema_file
    );

    if result.exit_code != 0 {
        throw step_name + " (codex) failed (exit " + result.exit_code + "): " + result.stderr;
    }

    let lines = result.stdout.split('\n');
    let output = ();
    for line in lines {
        if line.len() == 0 { continue; }
        let event = parse_json(line);
        if event.type == "item.completed" {
            output = parse_json(event.item.text);
            break;
        }
        if event.type == "turn.failed" {
            throw step_name + " (codex) turn failed: " + line;
        }
    }

    if output == () {
        throw step_name + " (codex): no item.completed event in output";
    }

    output
}

// ── File-scope enforcement ──────────────────────────────────────────
fn enforce_file_scope(brief) {
    let allowed = #{};

    // Always allow Cargo.lock — dependency changes are a legitimate
    // side effect of adding crates.
    allowed["Cargo.lock"] = true;

    if brief.requirements is defined {
        for r in brief.requirements {
            if r.files is defined {
                if r.files.create is defined {
                    for f in r.files.create { allowed[f] = true; }
                }
                if r.files.modify is defined {
                    for f in r.files.modify {
                        allowed[f] = true;
                        // If Cargo.toml is modified, Cargo.lock changes too
                        if f.ends_with("Cargo.toml") {
                            allowed["Cargo.lock"] = true;
                        }
                    }
                }
            }
        }
    }

    if allowed.len() <= 1 {
        // Only Cargo.lock in the set means brief has no files declarations
        write_file(".codex-scope-reverts.log", "SKIPPED: brief has no files declarations, enforcement disabled");
        return;
    }

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

        if marker == "??" {
            run_cmd("rm -f " + file);
            deleted.push(file);
        } else {
            run_cmd("git checkout -- " + file);
            reverted.push(file);
        }
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
        write_file(".codex-scope-reverts.log", log);
    }
}

// ══════════════════════════════════════════════════════════════════════
// SCOUT STEP — Codex (mixed/codex) or Claude (claude)
// ══════════════════════════════════════════════════════════════════════

let scout_output;

if provider == "claude" {
    add_step("scout", "action");
    set_input("scout", "profile", "codebase-explorer");
    set_template("scout", "instruction", "templates/scout-plan-prompt.md.j2");
    set_schema("scout", "output", "schemas/scout-plan-output.json");
    set_merge("scout", "enrichments", "input.brief.requirements", "id");
    scout_output = execute_step("scout");
} else {
    // mixed + codex both use Codex for scout (analysis, no code changes)
    let prompt = render_template("templates/scout-plan-prompt.md.j2", #{});
    scout_output = run_codex_step("scout", prompt, "schemas/scout-plan-output.json");
    set_context("scout", scout_output);
}

// ══════════════════════════════════════════════════════════════════════
// DEV STEP — Claude (mixed/claude) or Codex (codex)
// ══════════════════════════════════════════════════════════════════════

let dev_output;

if provider == "codex" {
    let prompt = render_template("templates/dev-prompt.md.j2", #{scout: scout_output});
    dev_output = run_codex_step("dev", prompt, "schemas/dev-output.json");
    set_context("dev", dev_output);
} else {
    // mixed + claude both use Claude for dev (actual code changes)
    add_step("dev", "action");
    set_input("dev", "profile", "workflow-developer");
    set_template("dev", "instruction", "templates/dev-prompt.md.j2");
    set_schema("dev", "output", "schemas/dev-output.json");
    set_merge("dev", "enrichments", "input.brief.requirements", "id");
    dev_output = execute_step("dev");
}

// ── Enforce file scope + commit after dev ───────────────────────────
enforce_file_scope(input.brief);
commit("feat: implement " + input.brief.id + " — " + input.brief.title);

// ══════════════════════════════════════════════════════════════════════
// REVIEW STEP — Codex (mixed/codex) or Claude (claude)
// ══════════════════════════════════════════════════════════════════════

let review_output;

if provider == "claude" {
    add_step("review", "action");
    set_input("review", "profile", "reviewer");
    set_template("review", "instruction", "templates/review-prompt.md.j2");
    set_schema("review", "output", "schemas/review-output.json");
    set_merge("review", "enrichments", "input.brief.requirements", "id");
    review_output = execute_step("review");
} else {
    // mixed + codex both use Codex for review (analysis, no code changes)
    let prompt = render_template("templates/review-prompt.md.j2", #{scout: scout_output, dev: dev_output});
    review_output = run_codex_step("review", prompt, "schemas/review-output.json");
    set_context("review", review_output);
}

// ── Commit after review (if review made changes) ────────────────────
commit("review: " + input.brief.id + " post-review fixes");

// ══════════════════════════════════════════════════════════════════════
// SAVE RESULTS
// ══════════════════════════════════════════════════════════════════════

write_json("workflow-output.json", #{
    provider: provider,
    scout: scout_output,
    dev: dev_output,
    review: review_output,
});
