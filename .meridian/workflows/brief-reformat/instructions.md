You are reformatting a brief from an old authoring format to the canonical schema. The content is approved — do not change the MEANING of anything. Your job is structural: rename fields, restructure data, and condense verbose descriptions into the target format.

## Field Mapping Rules

Top-level:
- id, cluster, title: copy as-is
- prerequisite_briefs -> depends_on
- context + why -> purpose (one paragraph, no code examples)
- context + rule -> task (plain language, what to do)
- out_of_scope -> boundaries (rewrite as SHALL NOT statements)
- acceptance (top-level) -> verification
- Collect all realises from all requirements -> checklist (deduplicated C# IDs)
- Collect all satisfies from all requirements -> stories (deduplicated S# IDs)

Per requirement:
- id, title: copy as-is
- description -> spec: Convert to EARS notation (WHEN/WHILE/IF + THE SYSTEM SHALL). Strip ALL code examples, type signatures, struct definitions, and implementation-level detail. The spec captures WHAT the system does, not HOW. One to three sentences max.
- acceptance_criteria -> acceptance
- files (flat strings like 'foo.rs (new)') -> files object: parse '(new)' as create, '(modified)' as modify. Strip annotations like '(modified — part of R4)' to just the path.
- realises -> checklist
- satisfies -> stories

Drop these fields entirely: wave, group, open_questions, references, files_likely_to_change, rule (fold into task), design_anchor.

Produce the reformatted brief as your structured output. Every field must be populated — no nulls, no empty strings for required fields.
