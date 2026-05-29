// test-merge-simple / workflow.rhai
//
// Minimal merge test: enrich items with a "color" field, merge back,
// verify the merged state. Uses Sonnet for reliable structured output.

// ── Enrich step: add a "color" to each item ──
add_step("enrich", "action");
set_input("enrich", "profile", "sonnet-minimal");
set_template("enrich", "instruction", "templates/enrich-prompt.md.j2");
set_schema("enrich", "output", "schemas/enrich-output.json");
set_merge("enrich", "enrichments", "input.items", "id");

let enrich = execute_step("enrich");

if enrich.is_error {
    throw "enrich step failed: " + enrich.text;
}

// ── Verify step: dump the merged state ──
add_step("verify", "action");
set_input("verify", "profile", "sonnet-minimal");
set_template("verify", "instruction", "templates/verify-prompt.md.j2");
set_schema("verify", "output", "schemas/verify-output.json");

let verify = execute_step("verify");

if verify.is_error {
    throw "verify step failed: " + verify.text;
}
