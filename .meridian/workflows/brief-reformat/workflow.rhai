// brief-reformat / workflow.rhai
//
// Two-step Norn workflow to reformat briefs from the old authoring-skill
// format to the canonical brief.schema.json format.
//
// Step 1 (analyze): Read the source brief and target schema, produce a
//   field-by-field mapping with the content to migrate.
// Step 2 (reformat): Take the mapping and produce the reformatted JSON.
//
// Inputs:
//   brief_path - path to the brief JSON file (required)
//   schema_path - path to brief.schema.json (optional, defaults to
//                 .meridian/design-system/schemas/brief.schema.json)
//   notify     - member name to DM results to (optional)

let workspace_root = env("MERIDIAN_WORKSPACE_ROOT");
if workspace_root == () || workspace_root == "" {
    workspace_root = env("PWD");
}
if workspace_root == () || workspace_root == "" {
    workspace_root = "/Users/tom/Developer/ablative/yggdrasil";
}

let brief_path = if input.brief_path.starts_with("/") {
    input.brief_path
} else {
    workspace_root + "/" + input.brief_path
};
let schema_path = if input.schema_path != () {
    if input.schema_path.starts_with("/") {
        input.schema_path
    } else {
        workspace_root + "/" + input.schema_path
    }
} else {
    workspace_root + "/.meridian/design-system/schemas/brief.schema.json"
};

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
    output
}

// =====================================================================
// READ INPUTS
// =====================================================================

let brief_content = read_file(brief_path);
let schema_content = read_file(schema_path);

let brief_json = parse_json(brief_content);
let brief_id = brief_json.id;

print("brief-reformat: " + brief_id + " from " + brief_path);

// =====================================================================
// SCHEMAS
// =====================================================================

fn build_analyze_schema() {
    let str_type = #{ type: "string" };
    let str_arr = #{ type: "array", items: str_type };

    let file_map_props = #{
        create: str_arr,
        modify: str_arr,
        delete: str_arr
    };
    let file_map = #{ type: "object", properties: file_map_props, required: ["create", "modify"], additionalProperties: false };

    let req_props = #{
        id: str_type,
        title: str_type,
        spec: #{ type: "string", description: "EARS-notation spec derived from the description. WHEN/WHILE/IF + THE SYSTEM SHALL. Strip code examples, type signatures, and implementation detail — those belong in scout context, not the brief." },
        acceptance: #{ type: "array", items: str_type, description: "Observable acceptance criteria. Taken from acceptance_criteria, cleaned up to be assertable." },
        files: file_map,
        checklist: #{ type: "array", items: str_type, description: "C# IDs from the requirement's realises field." },
        stories: #{ type: "array", items: str_type, description: "S# IDs from the requirement's satisfies field." }
    };
    let req_item = #{ type: "object", properties: req_props, required: ["id", "title", "spec", "acceptance", "files", "checklist", "stories"], additionalProperties: false };

    let props = #{
        id: str_type,
        cluster: str_type,
        title: str_type,
        depends_on: str_arr,
        checklist: #{ type: "array", items: str_type, description: "All C# IDs from all requirements' realises fields, deduplicated." },
        stories: #{ type: "array", items: str_type, description: "All S# IDs from all requirements' satisfies fields, deduplicated." },
        purpose: #{ type: "string", description: "Derived from context + why fields. One paragraph, no code." },
        task: #{ type: "string", description: "Plain-language description of the work. Derived from the brief's context and rule fields. What to do, not how." },
        requirements: #{ type: "array", items: req_item },
        boundaries: #{ type: "array", items: str_type, description: "SHALL NOT statements derived from out_of_scope." },
        verification: #{ type: "array", items: str_type, description: "Steps to verify the brief. From the top-level acceptance field." },
        design_anchor: #{ type: "string", description: "Preserved from source if present." }
    };
    to_json(#{ type: "object", properties: props, required: ["id", "cluster", "title", "depends_on", "checklist", "stories", "purpose", "task", "requirements", "boundaries", "verification"], additionalProperties: false })
}

// =====================================================================
// STEP 1: ANALYZE AND REFORMAT
// =====================================================================

let analyze_schema = build_analyze_schema();

let analyze_instruction = "You are reformatting a brief from an old authoring format to the canonical schema. The content is approved — do not change the MEANING of anything. Your job is structural: rename fields, restructure data, and condense verbose descriptions into the target format.\n\n";

analyze_instruction += "## Source Brief\n\n```json\n" + brief_content + "\n```\n\n";

analyze_instruction += "## Target Schema\n\n```json\n" + schema_content + "\n```\n\n";

analyze_instruction += "## Field Mapping Rules\n\n";
analyze_instruction += "Top-level:\n";
analyze_instruction += "- id, cluster, title: copy as-is\n";
analyze_instruction += "- prerequisite_briefs -> depends_on\n";
analyze_instruction += "- context + why -> purpose (one paragraph, no code examples)\n";
analyze_instruction += "- context + rule -> task (plain language, what to do)\n";
analyze_instruction += "- out_of_scope -> boundaries (rewrite as SHALL NOT statements)\n";
analyze_instruction += "- acceptance (top-level) -> verification\n";
analyze_instruction += "- Collect all realises from all requirements -> checklist (deduplicated C# IDs)\n";
analyze_instruction += "- Collect all satisfies from all requirements -> stories (deduplicated S# IDs)\n";
analyze_instruction += "- design_anchor: preserve if present\n\n";

analyze_instruction += "Per requirement:\n";
analyze_instruction += "- id, title: copy as-is\n";
analyze_instruction += "- description -> spec: Convert to EARS notation (WHEN/WHILE/IF + THE SYSTEM SHALL). Strip ALL code examples, type signatures, struct definitions, and implementation-level detail. The spec captures WHAT the system does, not HOW. One to three sentences max.\n";
analyze_instruction += "- acceptance_criteria -> acceptance\n";
analyze_instruction += "- files (flat strings like 'foo.rs (new)') -> files object: parse '(new)' as create, '(modified)' as modify. Strip annotations like '(modified — part of R4)' to just the path.\n";
analyze_instruction += "- realises -> checklist\n";
analyze_instruction += "- satisfies -> stories\n\n";

analyze_instruction += "Drop these fields entirely: wave, group, open_questions, references, files_likely_to_change, rule (fold into task).\n\n";

analyze_instruction += "Produce the reformatted brief as your structured output. Every field must be populated — no nulls, no empty strings for required fields.";

let result = do_norn_step("reformat", "norn-agent", analyze_instruction, analyze_schema);
print("Reformat complete: " + result.id);

// =====================================================================
// WRITE OUTPUT
// =====================================================================

write_json(brief_path, result);
print("Written: " + brief_path);

// =====================================================================
// NOTIFY
// =====================================================================

if input.notify != () {
    let msg = "brief-reformat complete: " + result.id + " — " + result.title + "\nWritten to " + brief_path;
    run_cmd("collective send --as Meridian --to '" + input.notify + "' --subject 'brief-reformat: " + result.id + "' --message '" + msg + "'");
}

print("brief-reformat done: " + result.id);
