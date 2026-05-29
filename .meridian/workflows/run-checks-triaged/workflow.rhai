// run-checks-triaged / workflow.rhai
//
// Check cluster with clippy lint triage. Blocking lints fail,
// deferred lints pass to hardener.

set_workflow_name("run-checks-triaged");
set_workflow_description(
    "Check cluster with clippy lint triage. Cargo check and test \
     failures always block. Clippy lints split into BLOCKING and \
     DEFERRED — only blocking lints fail the verdict."
);
set_max_visits(1);

let terminal = "end";

// ── Cargo Check ─────────────────────────────────────────────────────
add_step("cargo_check", "execute");
set_template("cargo_check", "command", "templates/cargo-check.sh.j2");
set_input("cargo_check", "parser", "rust-diagnostics");
set_route("cargo_check", "success", "cargo_clippy");
set_route("cargo_check", "failure", "ts_check");

// ── Cargo Clippy ────────────────────────────────────────────────────
add_step("cargo_clippy", "execute");
set_template("cargo_clippy", "command", "templates/cargo-clippy.sh.j2");
set_input("cargo_clippy", "parser", "json");
set_route("cargo_clippy", "success", "cargo_test");
set_route("cargo_clippy", "failure", "cargo_test");

// ── Cargo Test ──────────────────────────────────────────────────────
add_step("cargo_test", "execute");
set_template("cargo_test", "command", "templates/cargo-test.sh.j2");
set_input("cargo_test", "parser", "rust-test-results");
set_route("cargo_test", "success", "ts_check");
set_route("cargo_test", "failure", "ts_check");

// ── TS Check ────────────────────────────────────────────────────────
add_step("ts_check", "execute");
set_input("ts_check", "command", "cd apps/web && npx tsc --noEmit --pretty false 2>&1; echo \"exit:$?\"");
set_input("ts_check", "parser", "exit-code");
set_route("ts_check", "success", "biome_check");
set_route("ts_check", "failure", "biome_check");

// ── Biome Check ─────────────────────────────────────────────────────
add_step("biome_check", "execute");
set_input("biome_check", "command", "cd apps/web && npx biome check . 2>&1; echo \"exit:$?\"");
set_input("biome_check", "parser", "exit-code");
set_route("biome_check", "success", "file_size_check");
set_route("biome_check", "failure", "file_size_check");

// ── File Size Check ─────────────────────────────────────────────────
add_step("file_size_check", "execute");
set_template("file_size_check", "command", "templates/file-size-check.sh.j2");
set_input("file_size_check", "parser", "json");
set_route("file_size_check", "success", "render_report");
set_route("file_size_check", "failure", "render_report");

// ── Render Report ───────────────────────────────────────────────────
add_step("render_report", "execute");
set_template("render_report", "command", "templates/render-report.sh.j2");
set_input("render_report", "parser", "json");
set_route("render_report", "success", "verdict");
set_route("render_report", "failure", "verdict");

// ── Overall Verdict ─────────────────────────────────────────────────
add_step("verdict", "evaluate");
set_input("verdict", "mode", "check");
set_input("verdict", "criteria",
    "cargo_check.success == true\ncargo_clippy.success == true\ncargo_test.success == true\nts_check.success == true\nbiome_check.success == true\nfile_size_check.success == true"
);
set_route("verdict", "success", terminal);
set_route("verdict", "failure", terminal);

// ── End ─────────────────────────────────────────────────────────────
add_step(terminal, "end");

// ── Outputs ─────────────────────────────────────────────────────────
set_output("pass", "boolean", "{verdict.passed}");
set_output("summary", "string", "{render_report.data.summary}");
set_output("failure_report", "string", "{render_report.data.failure_report}");
set_output("deferred_clippy_report", "string", "{render_report.data.deferred_report}");
