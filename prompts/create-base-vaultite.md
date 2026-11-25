Create an empty Vaultite story Base for Maistro over the local Vaultite MCP
connection. Use the available tools and their actual schemas.

Ask for the Base name and its parent folder. Check the exact `.base` path and
same-named story folder before writing. Use a new path if either already contains
work the user wants to keep.

For a Base at `Projects/Service/Stories.base`, use the following YAML. Replace
the two folder filters to match the chosen Base path. Use the project's exact
status labels in `groupOrder`, including its empty lanes.

```yaml
views:
  - type: kanban
    name: Board
    filters:
      and:
        - 'file.inFolder("Projects/Service/Stories")'
        - 'type == "note"'
    groupBy: {property: note.status, direction: ASC}
    groupOrder: [Backlog, Ready, In Progress, Review, Done, Blocked, Cancelled]
    order: [file.name]
    sort: [{property: note.story_order, direction: ASC}]
  - type: table
    name: All stories
    filters:
      and:
        - 'file.inFolder("Projects/Service/Stories")'
        - 'type == "note"'
    order: [file.name, note.status]
    sort: [{property: note.story_order, direction: ASC}]
  - type: list
    name: Story template
    filters: 'file.path == "Templates/Maistro story.md"'
    order: [file.name]
```

Check the Vaultite Templates folder setting. Reuse the shared story template if
present and update the template view's path if the folder differs. Otherwise
create `Templates/Maistro story.md` in that folder with this content:

```markdown
---
type: note
kind: note
status: Backlog
---

## Description

## Acceptance criteria

## Comments
```

Verify the Base and template paths and use `render` to check the views. Return
the Base path as `vaultite_base`. Tell the user to create a note from the Base's
Backlog lane and use **Insert template** in the note editor. Vaultite assigns
the note an ID. Move a note to Ready only when work is authorized. Keep the Base
empty and leave service configuration and execution to the requested setup.
