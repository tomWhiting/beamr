# Writing a Checklist

The checklist is the full set of verifiable requirements for a cluster,
grouped by section. Each item is a concrete, testable condition that must
be true when the cluster is complete.

## Format

JSON, validated against `schemas/checklist.schema.json`. Rendered to
Markdown by `scripts/render-checklist.py`.

## Structure

```json
{
  "cluster": "messaging",
  "sections": [
    {
      "name": "Crate Setup",
      "items": [
        {"id": "C1", "text": "...", "done": false},
        {"id": "C2", "text": "...", "done": false}
      ]
    }
  ]
}
```

### cluster

The cluster name, matching the directory under `docs/design/`.

### sections

Group related items under meaningful section names. Sections provide
navigability — without them, a 90-item checklist is a wall of text.

Section names should reflect the work phase or component, not the brief
that delivers the item. Sections outlive individual briefs.

### items

Each item has:

- `id` — Sequential C-number across the whole cluster (C1, C2, C3...).
  Numbering is global, not per-section.
- `text` — A verifiable requirement. One sentence. Should read as a true/
  false assertion: either this condition holds or it doesn't.
- `done` — Boolean. Updated when the item is delivered and verified.

## Writing Good Checklist Items

**Verifiable, not aspirational.** Each item should be checkable by reading
code, running a command, or inspecting output.

Good: "StorageError defined in libmessage::storage::error."
Good: "cargo clippy --workspace -- -D warnings passes clean."
Good: "No imports of meridian_services::messaging::StorageError remain."

Bad: "Error handling is robust." (How do you check this?)
Bad: "The module is well-structured." (By whose standard?)

**Precise scope.** An item should describe one thing, not a category. If
an item covers multiple files or multiple conditions, split it.

Bad: "All storage types moved to the correct locations." (Which types?
Which locations?)

Good: "StorageError defined in libmessage::storage::error."
Good: "MessagingStorage trait defined in libmessage::storage::traits."
Good: "All model types moved to libmessage::storage::models."

Three items instead of one, each independently checkable.

**Stable wording.** Once a brief references a checklist item by C-number,
the text should not change in meaning. Clarifying edits are fine; changing
what the item requires breaks the brief's claim of coverage.

## Assignment

Checklist items are assigned to briefs via the brief's `checklist` array —
not in this document. To find which brief covers a given item, query the
briefs or run `scripts/check-coverage.py`.

When a single checklist item is split across multiple briefs (stub in one,
wiring in another), both briefs should reference the item and note the
split in their task descriptions.

## Numbering

C-numbers are sequential across the entire cluster, not per-section. If
you add items later, append to the end of the relevant section and use the
next available number. Don't renumber existing items — briefs reference
them by C-number.
