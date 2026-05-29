---
name: project-management
description: Domain expertise for the Project Management system — project items, fields, views, Kanban/List/Gantt, and the unified task system. Add when working on project tracking, task management, or work item features.
tools: Read, Glob, Grep, Bash
---

## Project Management Domain

You are working in the Project Management domain. This provides the unified task/project system with hierarchical items, configurable fields, and multiple view types.

### Key Files

**API (no service layer — handlers go directly to storage):**
- `crates/server/src/api/project_items.rs` — Universal ProjectItem CRUD, fields, views, relations (2469 lines)

**Storage:**
- `crates/storage/src/projects.rs` — Project item storage operations

**Frontend:**
- `apps/web/src/features/projects/` — Project views (Kanban, List, Gantt, Calendar), item detail, fields
- `apps/web/src/features/milestones/` — Milestone tracking

### Data Model

- **Universal `ProjectItem`**: Projects, tasks, and subtasks are the same struct. Hierarchy via `parent_id`.
- **Field system**: Configurable per-project fields — Select, Text, Number, Date, Checkbox, Person types
- **`maps_to` on SelectOption**: Auto-syncs `item.status` when a Select field value changes (enables Kanban drag-drop to update status)
- **Views**: Kanban, List, Gantt, Calendar — JSON config blobs scoped to project
- **Relations**: blocks/blocked_by between items

### Patterns

- No service layer for project management — API handlers call storage traits directly
- Provisioning: Standard/Minimal/Blank templates via `POST /api/projects/provision`
- ProjectItem has: name, description, status, priority, parent_id, assignee_id, workspace_id, due_date, and arbitrary field values
- Kanban view: drag-drop calls the field update endpoint, which triggers `maps_to` status sync
- List view: sortable/filterable with column configuration in view JSON

### Known Issues

- The 2469-line `project_items.rs` handler file should be split but isn't — high coupling
- No service layer means business logic is embedded in HTTP handlers
