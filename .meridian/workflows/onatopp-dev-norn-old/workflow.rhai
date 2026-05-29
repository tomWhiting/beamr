// onatopp-dev-norn — Lean dev cycle via Norn in-process agent.
// Three action steps: Scout+Plan, Dev, Review+Harden.
// Same inputs as onatopp-dev-lean. All tools available, vanilla Norn.

let brief = input.brief;
let brief_id = brief.id;
let brief_title = brief.title;

// Build the brief document block reused across steps.
let brief_doc = `# ${brief_id}: ${brief_title}

## Purpose

${brief.purpose}

## Task

${brief.task}

## Requirements

`;

for r in brief.requirements {
    brief_doc += `### ${r.id}: ${r.title}

${r.spec}

Acceptance:
`;
    for a in r.acceptance {
        brief_doc += `- ${a}
`;
    }
    brief_doc += "\n";
}

brief_doc += "## Boundaries\n\n";
for b in brief.boundaries {
    brief_doc += `- ${b}
`;
}

let design_block = "";
if input.design_content != () {
    design_block = `<document>
<source>design_document</source>
<document_content>
${input.design_content}
</document_content>
</document>
`;
}

// ═══════════════════════════════════════════════════════════════════════════
// SCOUT AND PLAN
// ═══════════════════════════════════════════════════════════════════════════

let scout_schema = to_json(#{
    "type": "object",
    "properties": #{
        "summary": #{"type": "string"},
        "tasks": #{
            "type": "array",
            "items": #{
                "type": "object",
                "properties": #{
                    "id": #{"type": "string"},
                    "subject": #{"type": "string"},
                    "description": #{"type": "string"}
                },
                "required": ["id", "subject", "description"]
            }
        },
        "verification_items": #{
            "type": "array",
            "items": #{"type": "string"}
        }
    },
    "required": ["summary", "tasks", "verification_items"]
});

let scout_instruction = `Explore the codebase and plan the implementation for this brief. You are read-only — do not modify files.

PHASE 1 — SCOUT: For each requirement, find related files, conventions, type signatures, and specific file:line references the implementer should look at.

PHASE 2 — PLAN: Using the context you just gathered, produce ordered implementation tasks. Be specific — another agent executes these without asking questions. Also produce verification_items: concrete checks to run after implementation.

${design_block}
<document>
<source>brief</source>
<document_content>
${brief_doc}
</document_content>
</document>`;

let scout_result = run_step_norn("scout", "developer", scout_instruction, scout_schema);

if scout_result.is_error {
    print(`Scout failed: ${scout_result.error}`);
    // Cannot proceed without a plan
    return;
}

let plan = scout_result.output;
print(`Scout complete: ${plan.summary}`);
print(`Tasks planned: ${plan.tasks.len()}`);

// ═══════════════════════════════════════════════════════════════════════════
// DEV
// ═══════════════════════════════════════════════════════════════════════════

let dev_schema = to_json(#{
    "type": "object",
    "properties": #{
        "summary": #{"type": "string"},
        "commit_message": #{"type": "string"}
    },
    "required": ["summary", "commit_message"]
});

let task_list = "";
for t in plan.tasks {
    task_list += `- [${t.id}] ${t.subject}: ${t.description}
`;
}

let dev_instruction = `Implement the brief. Here is your task list:

${task_list}

${design_block}
<document>
<source>brief</source>
<document_content>
${brief_doc}
</document_content>
</document>

Implement all requirements. Stage and commit your changes with a conventional-commits message.`;

let dev_result = run_step_norn("dev", "developer", dev_instruction, dev_schema);

if dev_result.is_error {
    print(`Dev failed: ${dev_result.error}`);
    return;
}

print(`Dev complete: ${dev_result.output.summary}`);

// Commit if dev did not already commit
let commit_result = commit(dev_result.output.commit_message);
if commit_result.committed {
    print(`Committed: ${dev_result.output.commit_message}`);
}

// ═══════════════════════════════════════════════════════════════════════════
// RUN CHECKS (inline — cargo check + clippy + test)
// ═══════════════════════════════════════════════════════════════════════════

let check_output = run_cmd("cargo check --workspace 2>&1 | tail -5");
let clippy_output = run_cmd("cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -20");
let test_output = run_cmd("cargo test --workspace 2>&1 | tail -10");

let checks_passed = check_output.exit_code == 0
    && clippy_output.exit_code == 0
    && test_output.exit_code == 0;

let check_report = "";
if !checks_passed {
    if check_output.exit_code != 0 {
        check_report += `### cargo check FAILED\n${check_output.stdout}\n\n`;
    }
    if clippy_output.exit_code != 0 {
        check_report += `### clippy FAILED\n${clippy_output.stdout}\n\n`;
    }
    if test_output.exit_code != 0 {
        check_report += `### tests FAILED\n${test_output.stdout}\n\n`;
    }
}

if !checks_passed {
    print("Checks failed — sending back to dev for fixes");

    let fix_instruction = `The following checks failed after your implementation. Fix ONLY the failures below — do not extend scope.

${check_report}

Stage and commit your fixes.`;

    let fix_result = run_step_norn("dev-fix", "developer", fix_instruction, dev_schema);
    if !fix_result.is_error {
        commit(fix_result.output.commit_message);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// REVIEW AND HARDEN
// ═══════════════════════════════════════════════════════════════════════════

let review_schema = to_json(#{
    "type": "object",
    "properties": #{
        "verdict": #{"type": "string", "enum": ["pass", "fail"]},
        "findings": #{
            "type": "array",
            "items": #{
                "type": "object",
                "properties": #{
                    "severity": #{"type": "string", "enum": ["blocking", "suggestion"]},
                    "file": #{"type": "string"},
                    "description": #{"type": "string"},
                    "fix_applied": #{"type": "boolean"}
                },
                "required": ["severity", "file", "description", "fix_applied"]
            }
        },
        "summary": #{"type": "string"}
    },
    "required": ["verdict", "findings", "summary"]
});

let verification_list = "";
for v in plan.verification_items {
    verification_list += `- ${v}
`;
}

let review_instruction = `Review and harden the implementation. You have full write access — fix anything you find.

1. Read every file changed in the most recent commits (use git diff HEAD~2).
2. Verify against the brief requirements.
3. Check: no panics/unwrap/expect in lib code, no unsafe, clippy clean, boundaries respected.
4. Fix any issues directly — do not just report them.
5. Run these verification checks:
${verification_list}

${design_block}
<document>
<source>brief</source>
<document_content>
${brief_doc}
</document_content>
</document>

If you make fixes, stage and commit them.`;

let review_result = run_step_norn("review", "developer", review_instruction, review_schema);

if review_result.is_error {
    print(`Review failed: ${review_result.error}`);
} else {
    let review = review_result.output;
    print(`Review verdict: ${review.verdict}`);
    print(`Findings: ${review.findings.len()}`);
    print(`Summary: ${review.summary}`);

    if review.findings.len() > 0 {
        commit("fix: review findings");
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// DONE
// ═══════════════════════════════════════════════════════════════════════════

print(`Workflow complete for ${brief_id}: ${brief_title}`);

if input.notify != () {
    let notify_msg = `Workflow onatopp-dev-norn complete for ${brief_id}.`;
    run_cmd(`collective send --to "${input.notify}" --subject "Workflow complete" --message "${notify_msg}"`);
}
