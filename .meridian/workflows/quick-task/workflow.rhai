// quick-task / workflow.rhai
//
// Single-step workflow for focused code changes. Dispatches a task to
// vanilla Norn (no profile), gets structured output reporting against
// requirements, and notifies the dispatcher. Changes are left
// uncommitted for the dispatcher to review before committing.
//
// Inputs:
//   task   - JSON object with: title (string), requirements (array of
//            {id, description, acceptance}), context (optional string)
//   notify - member name to DM results to (optional)

let task = input.task;
let task_title = task.title;

// =====================================================================
// STRUCTURED OUTPUT SCHEMA
// =====================================================================

let output_schema = to_json(#{
    type: "object",
    properties: #{
        summary: #{
            type: "string",
            description: "One paragraph describing what was done"
        },
        requirements: #{
            type: "array",
            items: #{
                type: "object",
                properties: #{
                    id: #{ type: "string" },
                    status: #{
                        type: "string",
                        enum: ["pass", "fail", "partial"]
                    },
                    what_changed: #{
                        type: "string",
                        description: "What was done to satisfy this requirement"
                    },
                    files_touched: #{
                        type: "array",
                        items: #{ type: "string" },
                        description: "File paths modified for this requirement"
                    }
                },
                required: ["id", "status", "what_changed", "files_touched"],
                additionalProperties: false
            }
        },
        concerns: #{
            type: "array",
            items: #{ type: "string" },
            description: "Anything the agent could not resolve or wants to flag"
        },
        commit_message: #{
            type: "string",
            description: "Conventional-commits style message for the changes"
        },
        verification: #{
            type: "object",
            properties: #{
                cargo_check: #{ type: "string", enum: ["pass", "fail", "skipped"] },
                cargo_clippy: #{ type: "string", enum: ["pass", "fail", "skipped"] },
                cargo_test: #{ type: "string", enum: ["pass", "fail", "skipped"] }
            },
            required: ["cargo_check", "cargo_clippy", "cargo_test"],
            additionalProperties: false
        }
    },
    required: ["summary", "requirements", "concerns", "commit_message", "verification"],
    additionalProperties: false
});

// =====================================================================
// BUILD INSTRUCTION
// =====================================================================

let instruction = "# Task: " + task_title + "\n\n";

if task.task != () {
    instruction += task.task + "\n\n";
}

instruction += "## Requirements\n\n";
for r in task.requirements {
    instruction += "### " + r.id + ": " + r.description + "\n";
    instruction += "**Acceptance:** " + r.acceptance + "\n\n";
}

if task.context != () {
    instruction += "## Context\n\n" + task.context + "\n\n";
}

instruction += "## Instructions\n\n";
instruction += "Implement every requirement in full. Run `cargo check`, `cargo clippy -- -D warnings`, and `cargo test` on affected crates after making changes. Fix any failures before submitting.\n\n";
instruction += "Do NOT commit your changes. Leave them uncommitted so the dispatcher can review the diff before committing.\n\n";
instruction += "In your structured output, report against each requirement with its id, pass/fail/partial status, what you changed, and which files you touched. Include a conventional-commits style commit message (the dispatcher will use it when they commit).\n";

// =====================================================================
// EXECUTE
// =====================================================================

print("quick-task: " + task_title);

try {

let result = run_step_norn("task", "norn-developer", instruction, output_schema);

if result.is_error {
    throw "task step failed: " + result.error;
}

let output = result.output;
if output == () {
    throw "task step returned empty output";
}

// =====================================================================
// NOTIFY
// =====================================================================

if input.notify != () {
    let report = "## " + task_title + "\n\n";
    report += "**Summary:** " + output.summary + "\n\n";

    report += "### Requirements\n\n";
    for r in output.requirements {
        let icon = if r.status == "pass" { "[PASS]" } else if r.status == "partial" { "[PARTIAL]" } else { "[FAIL]" };
        report += "- " + icon + " **" + r.id + "**: " + r.what_changed + "\n";
        if r.files_touched.len() > 0 {
            report += "  Files: " + r.files_touched.len() + " modified\n";
        }
    }

    report += "\n### Verification\n\n";
    report += "- cargo check: " + output.verification.cargo_check + "\n";
    report += "- cargo clippy: " + output.verification.cargo_clippy + "\n";
    report += "- cargo test: " + output.verification.cargo_test + "\n";

    if output.concerns.len() > 0 {
        report += "\n### Concerns\n\n";
        for c in output.concerns {
            report += "- " + c + "\n";
        }
    }

    report += "\n**Suggested commit message:** " + output.commit_message;
    report += "\n\nChanges are uncommitted — review with `git diff` and commit when satisfied.";

    write_file(".report.tmp", report);
    run_cmd("collective send --as Meridian --to '" + input.notify + "' --subject 'quick-task complete: " + task_title + "' --message \"$(cat .report.tmp)\"");
    run_cmd("rm -f .report.tmp");
}

print("quick-task complete: " + task_title);

} catch (err) {
    print("quick-task failed: " + err);
    if input.notify != () {
        run_cmd("collective send --as Meridian --to '" + input.notify + "' --subject 'quick-task FAILED: " + task_title + "' --message 'quick-task failed for " + task_title + ": " + err + "'");
    }
    throw err;
}
