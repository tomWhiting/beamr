// test-rhai-declarative.rhai
//
// First Rhai workflow test. Proves the evaluator produces a valid
// OrchestrationDefinition that the engine executes end-to-end.
// Uses haiku test profiles for speed and cost.
//
// Shape: Scout → Dev → Check → Done
// Scout enriches, Dev implements, Check verifies, Done terminates.

set_workflow_name("test-rhai-declarative");
set_workflow_description("Declarative Rhai test — Scout/Dev/Check cycle with haiku profiles.");
set_max_visits(3);

// ── Scout ────────────────────────────────────────────────────────────────
// Read-only exploration. Uses test-scout (haiku, Glob/Grep/Read only).

add_step("Scout", "action");
set_input("Scout", "instruction",
    "List the top-level crates in this workspace (ls crates/). " +
    "For each crate, read the first line of its Cargo.toml description field. " +
    "Report the crate name and description as a bulleted list. Keep it brief."
);
set_input("Scout", "profile", "test-scout");
set_route("Scout", "success", "Dev");
set_route("Scout", "failure", "Done");

// ── Dev ──────────────────────────────────────────────────────────────────
// Light implementation step. Uses test-developer (haiku, no git/build).

add_step("Dev", "action");
set_input("Dev", "instruction",
    "Create a file at .meridian/stage-output/rhai-test.txt containing " +
    "the text 'Rhai declarative mode: OK'. " +
    "Do not modify any other files in the repository."
);
set_input("Dev", "profile", "test-developer");
set_route("Dev", "success", "Check");
set_route("Dev", "failure", "Done");

// ── Check ────────────────────────────────────────────────────────────────
// Verify the dev step produced output.

add_step("Check", "execute");
set_input("Check", "command",
    "if [ -f .meridian/stage-output/rhai-test.txt ]; then " +
        "echo PASS; cat .meridian/stage-output/rhai-test.txt; " +
    "else " +
        "echo FAIL: file not found; " +
    "fi"
);
set_route("Check", "success", "Done");
set_route("Check", "failure", "Done");

// ── Done ─────────────────────────────────────────────────────────────────

add_step("Done", "end");

// Workflow output
set_output("check_result", "string", "{Check.text}");
