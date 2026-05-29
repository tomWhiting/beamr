// codex-review / workflow.rhai
//
// Example: fire Codex CLI to review code, capture structured output.
// Demonstrates run_cmd + read_file + parse_json + write_file + write_json + templates.
//
// Inputs:
//   - cluster_path: path to the cluster directory
//   - brief_name: which brief to review (e.g. "WS-019")

let brief_path = input.cluster_path + "/briefs/" + input.brief_name + ".json";
let design_path = input.cluster_path + "/DESIGN.md";

let brief = read_json(brief_path);
let design = read_file(design_path);

let prompt = render_template("templates/review-prompt.md.j2", #{
    brief: brief,
    design: design,
});

write_file(".codex-review-prompt.md", prompt);
write_json(".codex-review-schema.json", read_json("schemas/review-output.json"));

let result = run_cmd(
    "codex exec --model gpt-5.3-codex --prompt-file .codex-review-prompt.md --output-schema .codex-review-schema.json"
);

if result.exit_code != 0 {
    throw "codex review failed (exit " + result.exit_code + "): " + result.stderr;
}

let review = parse_json(result.stdout);
write_json("review-output.json", review);
