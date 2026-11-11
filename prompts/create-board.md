Create a new AppFlowy board for use with Maistro. Use the connected AppFlowy
tools and their actual schemas; do not guess tool names, arguments, or IDs.

First ask me for the URL or name of the parent AppFlowy page where the board
should be created. Wait for my answer before creating anything. Resolve the
parent page uniquely; if its name matches multiple pages, ask me to choose or
provide its URL. If the page cannot be found or accessed, explain what is needed
instead of creating the board elsewhere.

Create a child database named `Board` under that parent, with a board view and
a primary text field for story titles. Create a single-select field named
`Status` and group the board view by that field. Configure exactly these options
in this order, preserving the labels and AppFlowy color names exactly:

| Status | Color |
| --- | --- |
| Backlog | iron |
| Ready | azure |
| In progress | Mango |
| Review | mauve |
| Done | jade |
| Blocked | camelia |
| Cancelled | iron |

Replace any template status options on this new board with the options above.
Leave the board empty; remove any automatically generated sample stories from
the new board. Do not modify other boards or existing stories. If creation or
setup fails partway through, inspect the parent and any returned database/view
IDs before retrying so you do not create a duplicate. Report any partial board
and resume its setup when possible.

Read back the parent-child relationship, database fields, Status options and
colors, and board view grouping. Confirm that the view groups by the Status
field's ID and that all seven columns appear in the requested order, including
empty columns. Fix discrepancies on the new board and verify again. If a tool
limitation prevents verification or setup, explain the remaining manual steps
and do not claim the board is ready.

When verified, return the board's URL and name (plus database/view IDs if
available). Explain that Maistro's project configuration must target this new
board, not its parent page: use its URL as `appflowy_url`, or its uniquely
resolvable name as `appflowy_page`, but not both. Include this status mapping
under the corresponding project, because Maistro otherwise defaults to
`In Progress` with an uppercase P:

```toml
[projects.statuses]
backlog = "Backlog"
ready = "Ready"
in_progress = "In progress"
review = "Review"
done = "Done"
blocked = "Blocked"
cancelled = "Cancelled"
```

Explain how to start: add stories with their complete requirements and
acceptance criteria, keep drafts in Backlog, and move a story to Ready when it
is authorized for implementation. Maistro moves successful PR-mode stories to
Review and master-mode stories to Done. Do not start Maistro or authorize work
as part of creating this board.
