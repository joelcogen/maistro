# How Maistro works

See [README.md](README.md) for setup and run instructions.

## Project modes and delivery

A story in a `master` project can request PR delivery in its body or a human
comment. For example, write “Open a PR for this story” or “Implement this on
branch `feature/new-export`.” PR projects can request custom branches too.
Custom names must be valid Git branch names distinct from `base_branch`.

PR mode defaults to the stable branch `maistro/<project>/<story-id>`. Both modes
use `AppFlowy story: <story-id>` or `Shortcut story: <numeric-id>` commit/PR
markers to recover interrupted work. The repository, remote and PR target come
from project configuration.

PR overrides need a named remote, `gh` and a token that can create PRs. A PR
override on a local-only project moves the story to Blocked with an explanation.
For local delivery, the worker checks the configured base branch for the story's
commit marker during recovery. Maistro verifies the reported SHA is reachable
from that local branch before recording success.

PR or branch mentions in requirements keep the project default unless they
explicitly request a different delivery mode or branch. The coordinator fixes
the mode before assigning the story and uses its configured success status.
[Project modes](README.md#project-modes)
A blocked result goes to Blocked. The coordinator preserves human status changes,
uses completion markers to avoid duplicate comments, and reads back its comments
and status updates before acknowledging delivery.

## GitHub authentication

Workers for projects with a named remote read the token file on assignment and
receive `GH_TOKEN` and `GITHUB_TOKEN`, so rotation applies to the next worker in
foreground and service runs. Process-scoped Git settings authenticate
`https://github.com/` and translate standard `git@github.com:` and
`ssh://git@github.com/` remotes to HTTPS. Saved remotes and global Git settings stay
intact. `gh` authenticates through `GH_TOKEN`. Other Git hosts use their existing
authentication. Workers for local-only projects run without GitHub credentials.

## Custom agent prompts

Maistro reads [custom agent prompts](README.md#custom-agent-prompts) for each
coordinator pass or worker assignment, so edits take effect on the next
invocation. The coordinator sees instructions labeled by project for the enabled
projects in its pass. Workers also read `AGENTS.md`. Missing or empty prompt
files add no instructions. Other read errors stop the invocation and report the
project and path. Keep secrets out of prompts.

## Configured effort

Agent-start logs and the dashboard show the effort selected from
[Maistro's configuration](README.md#global-provider-models-and-effort), or
`inherited/default` when both levels leave it to the CLI. When Maistro passes
`--effort` to Claude Code, it removes `CLAUDE_CODE_EFFORT_LEVEL` from that
invocation's environment so the flag takes effect. Otherwise it leaves the
environment variable in place.

## Agent execution

Both providers share GitHub authentication, timeouts, process-group
cancellation, structured-result validation, and Git delivery and recovery rules.
Maistro disables hooks and agent spawning in both roles.

Claude runs use `--print --output-format stream-json --json-schema` with session
persistence disabled. Claude workers inherit normal Claude settings and
development tools with noninteractive full access. Their strict MCP
configuration loads non-source connections from `claude_mcp_config` and adds the
selected project source for dream workers. Repository and plugin MCP entries are
excluded. Codex workers use their normal tools, configuration, environment, and
network with noninteractive permissions and full filesystem access.

## Coordinator and worker access

The coordinator loads the source connections named by projects in its pass and
allows their listed read, status, and comment tools. It writes AppFlowy results
with `appflowy_rows(command=update_cells)` and
`appflowy_update(command=append_comment)`, and Shortcut results with
`update_story` and `create_story_comment`. Prompts specify which commands are
allowed within those tools and set the scope of the work.

## Status page and counts

The page refreshes `GET /api/status` every three seconds and marks retained data
stale on failure. The status endpoint supports GET/HEAD and gives local users
unauthenticated access to project IDs, story titles/IDs, safe board/workflow and
story links, and concise diagnostics. Titles render as text and links allow
only HTTP(S). Detailed failure output stays in private logs and the story source.

The compact Agents box lists Coordinator and each project with its status and
configured effort, followed by uptime and time since the latest coordination pass
began. Disabled projects remain visible. Project cards show worker status, duration
and latest start/finish time, the current story, board and story links, and concise errors. Pending results
remain visible until source recording completes. Observations live in memory and
reset on restart.

Counts come from coordinator reads and may lag Rust polling. Viewing the page
reads in-memory data. Ready counts queued work; In Progress includes active and
interrupted work. Counts use configured status labels and stay unknown until a
complete read. Failed or partial observations retain the last complete counts and
mark them stale. Successful reads still supply counts when writes fail.

Project counts are labeled with the coordinator pass start time. When a verified
Ready claim is accepted, its Ready/In Progress counts are adjusted immediately
if the complete read counted that story as Ready. Recovery and reads already
including the claim do not add it again. Unknown or stale totals are retained;
uncertain claim adjustments mark known totals stale. Other status writes may
lag until the next complete read. Overview counts sum known observations,
including stale ones. Missing projects, including disabled projects, make the
total partial; compact warnings identify partial or stale totals. Both views use
the same six count tiles, arranged in three columns on phones. The coordinator
authorizes stories from fresh source reads.

## Daily dream phases

An enabled project can start one dream phase per local calendar day, from the
[configured time](README.md#dreams) onward. The project needs at least one dream
file and either story worker activity since the last phase or a new or edited
dream file. Blocked story work counts. Maistro compares filenames and contents
with the last phase, so the first set of files counts as new.

The coordinator needs a complete, fresh read that shows no Ready or In Progress
stories. Active work, pending results, and new assignments take precedence. A
project with work queued can become eligible later that day. Incomplete reads
retry on the usual coordination schedule.

The dashboard's **Dream now** button requests a phase for one project over a
local POST. The request skips the configured start time and uses a new
coordination pass to check the project's board. The daily limit and input
requirements apply. A rejected request leaves no pending manual phase.

The phase holds the project's worker slot until all of its dreams finish. Polling
continues. Maistro handles newly queued stories in a fresh coordinator pass after
it releases that slot, and other projects can keep working throughout the phase.
Each dream uses the same provider, model, effort, timeout, and Git credentials
as a story worker. A dream may use its own configured source to read the
project's board or workflow and create Backlog stories if its instructions call
for it. Results and blockers go to private logs.

Before starting a phase, Maistro saves the local date and input manifest
atomically in `runtime_dir/dreams/`. A restart that day skips the phase even if it
was interrupted. Changes to dream files during a phase can make the project
eligible the following day.

Each dream tests and commits any repository changes. Local-only projects commit
on their local base branch. Master projects with a named remote push to their
configured base branch. PR projects use a dedicated `dreams` branch and rebase
it onto the updated base as needed. Maistro verifies each reported commit on the
configured delivery branch. Dreams that require no changes can complete without
a commit.

After all dreams succeed in PR mode and at least one pushes a commit, Maistro
uses the configured worker token to look for an open PR from `dreams` to the
configured base on the GitHub push remote. It reuses that PR or opens one if
`dreams` is ahead of the base. The worker slot stays occupied until publication
finishes. Maistro logs the PR URL or any publication error privately.

## Shortcut API polling

Rust resolves the workflow and reads only that workflow's non-archived stories
by listing all of its states and reading
`GET /api/v4/{workspace}/workflow-states/{state-id}/stories` for each state,
including states not mapped in Maistro. Reads use compact fields, exclude archived
stories, and follow every page. This avoids the workflow-wide story endpoint,
which timed out in live checks. Results must match the expected workflow and
state; duplicate story IDs or a changed state list invalidate the whole read.
The poller still compares two complete scans to detect changes during observation.
Request errors identify the endpoint and distinguish timeouts from connection
failures without logging credentials or cursor values. It validates configured state labels and numeric membership.
Polling compares complete fingerprints; unchanged boards do not invoke the
coordinator. Story metadata and membership changes wake it. Ready, In Progress,
and Blocked stories also have comments, checklist items, and story links checked explicitly, along
with linked story statuses, so these changes do not depend solely on parent
`updated_at` timestamps. Edits only to separately linked documents are not
independently polled; those documents are read when a story is coordinated.

Before each coordination pass Rust obtains a fresh compact workflow snapshot and
supplies it directly to the agent. MCP workflow/state/workspace enumeration tools
are disabled when a connection serves only API-configured projects in that pass.
When sharing a connection with a legacy project, the coordinator is instructed
to use the supplied API snapshot for the API-configured project. The agent still uses MCP for individual
story requirements, comments, linked documents, current active/pending checks, and
verified writes. Incomplete API reads isolate the affected project and retry without assigning,
cancelling, acknowledging, or writing its stories; other projects can proceed.
Credentials are never included in prompts or dashboard data.

## Shortcut scheduling

With API polling, `poll_interval_seconds` controls the delay between Rust polls.
Worker completion and retries can also wake coordination. Failed polls retain
the last complete fingerprint; unchanged successful polls do not erase a pending
retry or worker-completion event. Polling remains active while workers run so
archiving or moving a story can trigger cancellation checks. As with AppFlowy,
the coordinator verifies the current story before acting; cancellation checks
pause while the source is unavailable.

Without API polling, the coordinator reads Shortcut through the configured MCP
connection at each polling interval, including while idle. If the connection
has no workflow-scoped listing, the coordinator may read the workspace and
filter the results to the configured workflow. Both setups use the story's
numeric public ID to identify it across restarts.

## Story behavior and recovery

Map any custom status labels in `[projects.statuses]` using their exact spelling
and case. For example, if your board uses `In progress`, set
`in_progress = "In progress"`.

- Ready authorizes new work. The coordinator honors dependencies and priority and
  gathers the full story body, human comments and linked references.
- Interrupted In Progress stories take priority over new work. Workers inspect
  existing changes, commits, remote branches and PRs, preserve unrelated changes,
  and reuse completed work.
- Moving an active story out of In Progress stops its worker when the coordinator
  observes the change. Files, commits, PRs and the user-selected status are
  preserved. Requirement edits take effect on the next assignment.
- Worker failures or unresolved obstacles become Blocked with an explanation.
  Resolve the obstacle and move the story to Ready to retry.
- Completed results remain in memory and occupy the project until the coordinator
  confirms the source update or observes a user status change. Other projects
  continue. Failed coordination retries after the polling interval.

For AppFlowy, Rust polls enabled boards over each project's MCP connection.
`orchestrator.poll_interval_seconds` (default 30) sets the delay between completed
poll attempts per project. After initial observation and coordination, an unchanged
idle board waits for a change before calling the coordinator again. Changes to any
story, including Done or Backlog stories and Maistro's own writes, wake coordination.

One global coordinator pass combines pending projects while polling and workers
continue. It acknowledges the observed generation at its start; changes during
the pass stay pending for the next pass. Worker completion, failure, cancellation
completion and result acknowledgement also wake coordination. Failed or incomplete
coordination and unacknowledged results retry after the interval per project.

The coordinator reads fresh data, checks active-story statuses, authorizes work,
writes results and decides cancellation. Cancellation usually takes up to one
polling interval plus a complete board read and coordinator decision. Busy passes,
large boards, inconsistent reads, failures and timeouts can extend that delay.
Pollers run independently across projects. Cancellation detection pauses during an
AppFlowy outage. A push underway may finish before cancellation is observed.

### Polling contract and consistency

Boards resolve by unique view name/ID (`appflowy_page`) or view ID in an AppFlowy
URL. Use a URL or ID for ambiguous names. Each project keeps a SHA-256 fingerprint,
dirty generations and retry timing in memory. Poll responses are transient. After
restart, projects begin unknown/dirty, obtain a complete observation, and run the
coordinator to recover In Progress work or start Ready work.

The fingerprint covers board name/icon/layout/extra metadata, fields and options,
row membership and cells for every row. After scanning cells, full Markdown
bodies, row comments/replies (including resolved threads), and inline comments
are read only for Ready, In Progress, Blocked, and unknown-status stories, plus
same-board rows referenced by ID in their cells, bodies, or comments (transitively).
This preserves requirement edits on waiting/active stories and dependency status
changes across the board. Unresolvable status schemas and free-form dependency
cells without known row IDs conservatively retain fuller reads. Unreferenced
Backlog/Done/Review/Cancelled body-only edits do not wake coordination; moving
those stories into an actionable state does. Rows,
fields and comments are keyed by ID, JSON objects by key; semantic array order
and Markdown are preserved. MCP request metadata and pagination offsets are
excluded. **External linked reference edits alone are outside detection scope.**
The coordinator reads relevant references when processing a story.

Before each AppFlowy coordination pass, Rust resolves the configured board using
its MCP connection and supplies `appflowy_board` with workspace, database and view
IDs. Resolution failures exclude that project from the pass and schedule a retry.
The coordinator must include the supplied workspace ID on every scoped call.
Codex approves the compact database tool so the coordinator can run its read
commands. The coordinator's prompt restricts schema changes because approval
applies to the whole tool.

The poller uses AppFlowy MCP's compact API with typed `request` arguments.
Board metadata uses a folder fetch scoped to the view ID, avoiding the page
endpoint's oversized embedded board payload. Workspace, database, field and row
lists follow returned continuation offsets.
Rows marked unavailable or omitted invalidate the observation. Markdown is read
separately in chunks with a stable content hash, checked against the assembled body.
AppFlowy's `has_doc` flag
identifies bodies; failed body reads reject the observation. Row comments use row
collabs, while inline comments use the derived document UUID.

Comments are fully paginated. Malformed or nonadvancing pages, truncated or
partial responses, missing rows and failed reads reject the observation. Field
and membership reads bookend each scan, and two complete scans must agree before
a fingerprint is accepted. These checks reduce races; consistently stale reads
and edits reverted between observations can escape detection. Ongoing edits may
defer acceptance until a stable read. Rejections preserve the last valid
fingerprint and dirty state. Retries wait the configured interval. Polls are
bounded by `coordinator_timeout_seconds`, with a 60-second limit per MCP read.

Restart clears assignments and pending results from memory. Recovery reconstructs
work from the source and repository, including crashes between remote operations
and source updates. Human edits can race with reads and status writes. Use a
single daemon per board, even across separate clones. Agents verify claims and
remote outcomes; Rust validates response shape and concurrency.
