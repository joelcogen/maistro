# Maistro

Maistro turns AppFlowy and Shortcut stories into tested, committed code. One
coordinator selects stories and records results; one coding worker per project
implements, tests, fixes failures, commits and pushes. Projects run in parallel,
with one active story per project. Each project's board or workflow holds its
story state.

Choose Codex or Claude Code and configure the coordinator and worker models
globally. The default is Codex with `gpt-6-luna` for coordination and
`gpt-6-astra` for implementation.

## Project modes

| Mode | Worker responsibility | Successful story status |
| --- | --- | --- |
| `pr` | Create/reuse a story branch, implement, test, commit, push, and open/update a PR against `base_branch`. | Review, with PR link |
| `master` | Implement and test directly on `base_branch`, then commit and push it. | Done, with commit reference |

`base_branch` can be `main`, `master`, or another branch. You handle CI, PR merging
and moving reviewed stories to Done.

A story in a `master` project can request PR delivery in its body or human
comments: “Open a PR for this story” or “Deliver this story as a PR on branch
`feature/new-export`.” A request to implement on a named feature/story branch also
implies PR delivery. PR projects can request custom branches too. Custom names
must be valid Git branch names distinct from `base_branch`.

PR mode defaults to the stable branch `maistro/<project>/<story-id>`. Both modes
use `AppFlowy story: <story-id>` or `Shortcut story: <numeric-id>` commit/PR
markers to recover interrupted work. The repository, remote and PR target come
from project configuration.

PR overrides finish in Review with a PR link and require `gh` and token permission
to create PRs. Delivery overrides require an explicit request; ordinary mentions
of PRs or branches in product requirements keep the project default. The
coordinator selects the effective mode before dispatch, and that mode stays fixed
through result delivery.

Each pending result supplies the coordinator with the exact configured destination
status: completed master work goes directly to Done, completed PR work to Review,
and blocked work to Blocked. A PR mention or link does not change that destination.
Human status changes are preserved; result comments and status updates are read
back before delivery is acknowledged.

## Install and configure

Requires Linux, Git, Rust 1.89+, and an authenticated Codex or Claude Code CLI with
the selected source's MCP connection configured. Codex needs `exec --json`,
`--output-schema`, `--ephemeral` and `--ignore-user-config`. Claude requirements
are below. PR projects also need `gh` (GitHub CLI).

```sh
./install.sh
```

The installer builds the binary, installs the user service, and copies example
configuration while preserving an existing `config.toml`. Edit
`~/.config/maistro/config.toml` using [config.example.toml](config.example.toml),
then start the service as described below.

Each project needs a stable ID, a dedicated local Git checkout, exactly one
`appflowy_page`, `appflowy_url`, `shortcut_workflow`, `shortcut_workflow_id`, or
`shortcut_url`, a `mode`, and a `base_branch`.
Each repository and its linked worktrees must belong to a single project. Maistro
locks each repository for the daemon's lifetime. Reserve these checkouts for
Maistro and preserve interrupted work when restarting.

For a new AppFlowy board, copy the [create-board prompt](prompts/create-board.md)
into a chat with an AppFlowy-connected model. It asks for the parent page, creates
a child board with seven workflow statuses and colors, and returns its reference
and status mapping. The prompt uses `In progress`, so set
`in_progress = "In progress"` in that project's `[projects.statuses]` table.

### GitHub token

During installation, create a [fine-grained GitHub personal access token](https://github.com/settings/personal-access-tokens/new)
for the appropriate resource owner and **only the repositories Maistro manages**.
Grant **Contents: Read and write** for pushes, **Workflows: Read and write** for
workflow edits, and **Pull requests: Read and write** for PR mode. Set an expiration
and obtain organization approval if required. Repository rules and the token
owner's access apply: master mode requires direct push permission to `base_branch`;
use PR mode when rules require PRs. See [GitHub's token guide](https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/managing-your-personal-access-tokens).

Paste the token at the installer's hidden prompt. For unattended installation,
supply `MAISTRO_GITHUB_TOKEN` through your secret manager. The installer stores it
in `~/.config/maistro/github-token` with mode `0600` in a `0700` directory.
Reinstalling preserves it; explicitly supplying `MAISTRO_GITHUB_TOKEN` replaces it
atomically. Renew it before expiration by supplying a replacement, or remove the
old file and rerun the installer. Keep tokens private and outside prompts and Git.
For a custom location, set `agents.github_token_file` and provision that private
file separately.

Each worker reads the file on assignment and receives `GH_TOKEN` and `GITHUB_TOKEN`,
so rotation applies to the next worker in foreground and service runs. Token
injection is scoped to workers. Process-scoped Git settings authenticate
`https://github.com/` and translate standard `git@github.com:` and
`ssh://git@github.com/` remotes to HTTPS. Saved remotes and global Git settings stay
intact. `gh` authenticates through `GH_TOKEN`. Other Git hosts use their existing
authentication.

### Custom agent prompts

Add either or both optional UTF-8 files at the root of a project's configured
repository (`repo`):

- `maistro-coordinator.md`: story selection or requirements gathering instructions
  appended to the coordinator prompt.
- `maistro-worker.md`: testing commands and coding conventions appended to the
  worker prompt. Start with [the worker example](examples/project-prompt.md).

Maistro discovers these files automatically for both providers. Each applies to
its agent; coordinator additions are labeled by project and included for enabled
projects selected in that pass. Workers also read `AGENTS.md`.

Files are read for each coordinator pass or worker assignment, so changes take
effect on the next invocation. Only nonempty files add instructions; read errors
other than missing files fail the invocation with the project and path. Keep
secrets out of prompts.

To migrate, remove the obsolete `prompt_file` TOML key and move its contents to
`maistro-worker.md` in the repository root. Prompt examples live in this repository.
Other relative configuration paths resolve from the TOML file's directory;
`~/` expands from HOME. Restart the service to apply TOML changes.

### Global provider, models and effort

Set the provider, models and optional effort once in `[agents]` for every project:

```toml
[agents]
provider = "codex" # Default; or "claude" for Claude Code
small_model = "gpt-6-luna" # Coordinator
big_model = "gpt-6-astra" # Implementation workers
# Optional; omit to preserve inherited configuration/provider defaults.
# small_model_effort = "low"
# big_model_effort = "high"
```

Model IDs and aliases pass unchanged to the selected CLI, which checks availability
on invocation. Omitted models use provider defaults: `gpt-6-luna` / `gpt-6-astra`
for Codex, `haiku` / `opus` for Claude. When switching providers, remove explicit
models or replace them with supported ones. Model names must contain text;
per-project provider, model and effort settings are rejected.

Optional `small_model_effort` (coordinator) and `big_model_effort` (all workers)
accept these exact lowercase values:

| Provider | Effort values | CLI override |
| --- | --- | --- |
| Codex | `none`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max`, `ultra` | `-c 'model_reasoning_effort="VALUE"'` |
| Claude | `low`, `medium`, `high`, `xhigh`, `max` | `--effort VALUE` |

Maistro rejects other values, including empty strings, during validation. Claude
must advertise `--effort` when either setting is configured. Model-specific
availability still depends on the selected CLI and model; these lists do not
guarantee every model supports every level. See the
[Codex configuration reference](https://learn.chatgpt.com/docs/config-file/config-reference)
and [Claude effort documentation](https://code.claude.com/docs/en/model-config#adjust-effort-level).
Claude's `ultracode` mode is not an effort level supported by Maistro.

Omitting a setting sends no effort override. Workers keep their normal inherited
configuration; coordinators retain their existing isolated settings behavior and
provider/model defaults. For explicit Claude effort, Maistro removes the inherited
`CLAUDE_CODE_EFFORT_LEVEL` environment override for that invocation so the CLI flag
can take effect. When effort is omitted, that environment variable is preserved.
Agent-start logs and the dashboard show **configured effort**, or
`inherited/default` when omitted, without claiming the effective runtime effort.

For Claude Code, replace `[agents]` with:

```toml
[agents]
provider = "claude"
# small_model = "haiku"
# big_model = "opus"
# claude = "/absolute/path/to/claude" # Defaults to claude on PATH
# claude_mcp_config = "~/.claude.json"
```

Use an authenticated Claude Code CLI supporting print mode, streaming JSON, JSON
Schema output, session-persistence control, strict MCP configuration, tool and
setting-source controls, and noninteractive permissions. `validate` checks required
flags and `claude auth status` locally. Authenticate with `claude auth login`
before starting the service. Claude mode uses Claude's credentials and
authentication environment.

Configure story servers with **user scope**, for example:

```sh
claude mcp add --scope user --transport http shortcut https://mcp.shortcut.com/mcp
```

Complete OAuth authorization in Claude before unattended use. Maistro reads only
the top-level `mcpServers` object in `~/.claude.json`.
`agents.claude_mcp_config` selects an alternative JSON file with the same format;
Claude's login home stays separate. Project `mcp_server` selects a named entry.
Keep credentials in their private configuration files.

AppFlowy Rust polling accepts Claude stdio (`command`, `args`, `env`) and HTTP
(`"type": "http"`, `url`, `headers`) connections. It expands `${VAR}` and
`${VAR:-default}` substitutions. HTTP AppFlowy polling requires explicit headers;
stdio is also supported. Shortcut uses periodic coordinator reads with Claude's
OAuth.

Claude runs use `--print --output-format stream-json --json-schema` with session
persistence disabled. The coordinator loads only selected source connections and
permits their listed read/status/comment tools. Workers inherit normal Claude
settings and development tools with noninteractive full access. Their strict MCP
configuration includes only non-source connections from `claude_mcp_config`.
Repository/plugin MCP entries are excluded. Both roles disable hooks and Agent/Task
spawning. Both providers share GitHub authentication, timeouts, process-group
cancellation, structured-result validation, Git delivery and recovery rules.

See the [Claude CLI reference](https://code.claude.com/docs/en/cli-reference),
[structured output guide](https://code.claude.com/docs/en/headless), and
[MCP configuration guide](https://code.claude.com/docs/en/mcp).

The coordinator uses the MCP servers named by projects in its pass. It writes
AppFlowy results with `update_row_cells` and `append_comment`, and Shortcut results
with `update_story` and `create_story_comment`. With Codex, workers use normal
Codex configuration, tools, environment and network, plus the GitHub token, full
filesystem access and noninteractive permissions. Story-source MCP access belongs
to the coordinator. Maistro disables hooks and agent spawning in both roles;
prompts direct work scope and Git behavior.

## Start, stop and logs

```sh
maistro --config ~/.config/maistro/config.toml validate
systemctl --user daemon-reload
systemctl --user start maistro
systemctl --user stop maistro
systemctl --user restart maistro
systemctl --user status maistro
journalctl --user -u maistro -f
```

Use `systemctl --user enable maistro` to start with the user service manager.
Configure user lingering for operation after logout. `validate` checks local
configuration, repository identities/remotes, the selected CLI login, the private
token file and enabled projects' MCP configuration. Remote authentication, board
access and push permissions are checked during operation.

The service runs as your user with `~/.local/bin` and `~/.cargo/bin` in PATH. It
optionally reads `~/.config/maistro/environment` in systemd environment-file syntax.
Configure a user service environment/drop-in for tools installed elsewhere or SSH
agent access; interactive shell startup files apply only to your shell. Maistro
reads the GitHub token file directly.

For foreground diagnostics:

```sh
maistro --config ~/.config/maistro/config.toml run
```

SIGINT/SIGTERM stops the coordinator and worker process groups. Use systemd for
unattended operation: `KillMode=control-group` also cleans up descendants after a
daemon crash. After SIGKILL of a foreground daemon, check and stop surviving
child processes before restarting.

Private event/stderr logs live in `~/.local/state/maistro/logs` by default and may
contain story, code and tool content. Use them for diagnostics and clean up old
logs as needed. `runtime_dir` holds logs, OS locks and transient invocation files;
it must be outside managed repositories. Normal completion deletes result/schema
files. Crash leftovers can be removed while the service is stopped. Recovery uses
the story source and Git repository.

## Read-only status page

Enable the server in `~/.config/maistro/config.toml`, then restart Maistro:

```toml
[status]
enabled = true
port = 8044
```

Open `http://127.0.0.1:8044/`. The server binds only to **127.0.0.1**. It starts
disabled and defaults to port 8044. A bind failure stops startup with the address
and OS error. Port binding happens at startup; `validate` checks configuration.

The page refreshes `GET /api/status` every three seconds and marks retained data
stale on failure. It supports GET/HEAD and gives local users unauthenticated access
to project IDs, story titles/IDs, safe board/workflow and story links, and concise
diagnostics. Titles render as text; links allow only HTTP(S). Detailed failure
output stays in private logs and the story source.

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
the same six count tiles, arranged in three columns on phones. Treat these counts
as observations; the coordinator authorizes work from fresh source reads.

## Shortcut projects

Select one workflow with `shortcut_workflow` (exact, case-sensitive name),
`shortcut_workflow_id` (positive integer), or an existing `shortcut_url` workflow
link. Set exactly one selector; combining selectors is rejected even if they refer
to the same workflow. Names must be unique in the configured MCP connection's
workspace; use an ID when names are ambiguous. Numeric-looking names remain names.

The coordinator resolves the name, ID or link through that project's configured
MCP connection on each pass and verifies membership before claims and updates.
Missing, inaccessible or ambiguous workflows produce a diagnostic and retry without
dispatch or source writes. Maistro never falls back to a default workflow or the
whole workspace. Workflow resolution and status mapping must succeed before work
can proceed. Scope includes all non-archived stories in that workflow. Use a
dedicated workflow to limit available stories; space, team and label selectors are
not supported. Name/ID selectors do not require or generate a dashboard workflow link.

The coordinator prefers `list_workflow_stories` with the verified workflow ID.
The public [v4 API](https://developer.shortcut.com/api/rest/v4#List-Workflow-Stories)
and [OpenAPI specification](https://developer.shortcut.com/api/rest/v4/shortcut.openapi.json)
document that listing (checked 2026-11-13). MCP availability is checked against the
connected server's tools. If unavailable, it reads all pages of `list_stories`, or
uses the nonempty `is:story` search, then filters by numeric workflow/state IDs.
The [search operators](https://www.shortcut.com/help/fields-and-features/search-operators/)
do not document `workflow:`; state names alone do not isolate workflows.

For continuation calls the coordinator extracts and URL-decodes `cursor` from
`next_page_url`, keeping the same fields and required path IDs and omitting other
query parameters. Every page must succeed before selecting work or publishing
complete counts. Failed, malformed or looping pagination leaves counts unknown
(previous counts remain stale) and schedules a retry. Rust also rejects assignments
for projects marked for retry. Shortcut reads remain in the coordinator's CLI/MCP
connection; tests cover its prompt/tool contract and result handling without live
workspace calls.

```toml
[[projects]]
id = "shortcut-service"
repo = "/absolute/path/to/service"
shortcut_workflow = "Agents"
# Or replace the name selector with one of these:
# shortcut_workflow_id = 500005096
# shortcut_url = "https://app.shortcut.com/your-workspace/settings/workflows/123"
mcp_server = "shortcut" # Optional; name of this workspace's entry in the selected CLI.
mode = "pr"
base_branch = "main"

[projects.statuses]
backlog = "Backlog"
ready = "Ready"
in_progress = "In Progress"
review = "Review"
done = "Done"
blocked = "Blocked"
cancelled = "Cancelled"
```

Create or map each enabled workflow state using its exact name. These are
workflow states, separate from Shortcut's state categories and dependency
`blocked` flag. Configure workflow definitions in Shortcut. Claim, cancellation,
recovery, dependency, PR override and result-delivery rules apply to both sources.
Numeric public story IDs remain stable across restarts; completion comments use
idempotency markers and read-back checks.

Configure and authorize the hosted Shortcut MCP in your selected CLI using
[Shortcut's MCP setup](https://www.shortcut.com/help/integrations/mcp-server/).
The endpoint is `https://mcp.shortcut.com/mcp` and uses OAuth. For Codex, use
Maistro's `agents.codex_home`; for Claude, use a user-scoped entry. Maistro uses
the server's configured name and the CLI's login. Complete authorization before
starting the service; remote access is checked during operation.

`projects.mcp_server` selects a connection for either source. Defaults are
`shortcut` for Shortcut and `agents.mcp_server` (normally `appflowy`) for AppFlowy.
Configure entries in `agents.codex_home/config.toml` for Codex, or the top-level
`mcpServers` in `agents.claude_mcp_config` for Claude. Projects can mix sources in
one process. Configure servers for the sources you use, with a separate server
name for each source type and a single project per board/workflow.

**Shortcut uses periodic coordinator reads, including while idle.** Rust polling
requires explicit transport credentials, so Shortcut uses the CLI's OAuth through
the coordinator. `poll_interval_seconds` sets the delay after a completed or failed
pass; worker completion can trigger earlier coordination. One global pass runs at
a time. Failures retain pending results while other projects' workers continue.
Increase the interval to reduce idle model calls at the cost of dispatch and
cancellation latency. The coordinator handles fresh paginated reads and verified
claims/status writes. Successful reads are required to authorize work; cancellation
checks pause during a Shortcut outage.

Optional states can be explicitly disabled with TOML booleans:

```toml
[projects.statuses]
review = false
cancelled = false
```

Omitted entries retain their default labels. Only Review and Cancelled can be
disabled; enabled labels must remain distinct and nonempty. Review can only be
disabled in master mode. A story requesting PR delivery while Review is disabled
is marked Blocked with an explanation, including during recovery. Shortcut
archiving still cancels active work regardless of the Cancelled setting. These
optional settings also apply to AppFlowy.

## Story behavior and recovery

Use Backlog, Ready, In Progress, Review, Done, Blocked, and Cancelled, or map their
exact labels in `[projects.statuses]`. For boards using `In progress`, set
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

The AppFlowy poller uses `projects.mcp_server` (default `agents.mcp_server`) from
the selected provider's configuration. Codex supports stdio `command`, `args`,
`cwd`, `env` and inherited environment, or streamable HTTP `url`, `http_headers`,
`env_http_headers` and `bearer_token_env_var`. Claude transport keys are described
above. Rust polling uses the MCP SDK directly.

Enable AppFlowy MCP's legacy read tools: `get_workspaces`, `list_databases`,
`get_page`, `get_database_fields`, `get_database_row_ids`,
`get_database_row_details`, `get_page_markdown` and `list_comments`. Use stdio or
streamable HTTP with explicit headers/token for polling authentication.
Connection/authentication errors are reported and retried at the polling interval.

Boards resolve by unique view name/ID (`appflowy_page`) or view ID in an AppFlowy
URL. Use a URL or ID for ambiguous names. Each project keeps a SHA-256 fingerprint,
dirty generations and retry timing in memory. Poll responses are transient. After
restart, projects begin unknown/dirty, obtain a complete observation, and run the
coordinator to recover In Progress work or start Ready work.

The fingerprint covers board name/icon/layout/extra metadata, fields and options,
row membership, cells, full Markdown bodies for rows with `has_doc=true`, row
comments/replies including resolved threads, and inline body comments. Rows,
fields and comments are keyed by ID, JSON objects by key; semantic array order
and Markdown are preserved. MCP request metadata and pagination offsets are
excluded. **External linked reference edits alone are outside detection scope.**
The coordinator reads relevant references when processing a story.

The read contract was checked against AppFlowy MCP's legacy handlers
(`appflowy_mcp/server.py`, revision `008128fabb8459025c2c4fa124ea8713988af0e0`)
and AppFlowy Cloud's `AFDatabaseRowDetail`/`list_database_row_details` in
`libs/shared-entity/src/dto/workspace_dto.rs` and `src/biz/collab/ops.rs`.
Row-detail reads can omit unavailable rows; their optional `doc` is plain text
and can remain null on errors. The poller therefore requires exact requested and
returned row IDs and reads Markdown separately. AppFlowy's `has_doc` flag
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

## Migrating the previous version

Stop the old service, then replace the TOML using the new example. Remove obsolete
`state_db`, worktree settings, reviewer settings, worker limits, retry budgets,
Rust `test_commands`, GitHub/CI settings and path/sandbox policies. Put testing
requirements in each repository's `maistro-worker.md`. Set `mode` and `base_branch`
explicitly; unknown legacy settings produce a migration error.

Reconcile old AppFlowy statuses and preserve unfinished worktrees/branches before
starting. Point the project at the checkout containing unfinished work, or bring
that work into its dedicated checkout. Existing PRs in Review remain yours to
handle. Manage stories and boards in AppFlowy, configuration in a text editor,
and the service through systemd. Rerun the installer to save a fine-grained GitHub
token in the private file used by workers.

## Development

```sh
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
python3 -m unittest discover -s tests -p 'test_install.py'
node --test tests/status_ui.test.cjs
cargo build --release --locked
```

UI tests use Node.js 20+ and its built-in test runner. Tests use simulated Codex
and Claude Code agents, stdio/HTTP MCP fixtures and local Git remotes.
