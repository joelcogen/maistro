# Maistro

Maistro runs one coordinator for AppFlowy and Shortcut and one coding worker per
project, using either Codex or Claude Code. The provider and the small coordinator
and big worker models are configured globally. Codex with `gpt-6-luna`/`gpt-6-astra`
remains the default. Projects run in parallel; each project handles one story at a
time. Each project's configured board/workflow is the source of truth. There is no
SQLite database, local board mirror,
independent reviewer, CI watcher, or merge pipeline.

The coordinator selects a story, gathers its complete requirements and linked context, and
confirms it is In Progress. The worker implements it, runs tests, fixes failures,
commits and pushes. The coordinator then records the result on the story's source.

## Project modes

| Mode | Worker responsibility | Successful story status |
| --- | --- | --- |
| `pr` | Create/reuse a story branch, implement, test, commit, push, and open/update a PR against `base_branch`. | Review, with PR link |
| `master` | Implement and test directly on `base_branch`, then commit and push it. | Done, with commit reference |

`base_branch` can be `main`, `master`, or another branch. CI and PR merging belong
to the user. Maistro does not watch PRs after submission or move Review to Done.
A story can override a `master` project's default by explicitly requesting PR
delivery in its body or human comments, for example: “Open a PR for this story”
or “Deliver this story as a PR on branch `feature/new-export`.” Asking to implement
the story on a named feature/story branch also implies PR delivery. A PR project
can likewise specify a custom branch. Without a requested name, the worker uses stable
`maistro/<project>/<story-id>` branch names in PR mode. In both modes it uses
`AppFlowy story: <story-id>` or `Shortcut story: <numeric-id>` commit/PR markers
to find interrupted work.
Custom names must be valid Git branch names distinct from `base_branch`. The
repository, remote and PR target remain configured by the project. A story PR
override finishes in Review with its PR link, even in a master project; it needs
`gh` and token permission to create PRs. Stories without an override retain the
project default. Merely mentioning PRs or branches in product requirements does
not change delivery mode. The coordinator extracts the delivery request, and the
worker receives only the instructions for the effective mode. That mode is kept
through result delivery; edits do not change an already running worker's endpoint.

## Install and configure

Requires Linux, Git, Rust 1.89+, and a logged-in Codex or Claude Code CLI with the
selected source's MCP connection configured. Codex needs `exec --json`,
`--output-schema`, `--ephemeral` and `--ignore-user-config`; Claude requirements
and configuration are described below. PR projects also need `gh` (GitHub CLI).
The installer sets up a GitHub token for worker Git and PR operations.

To get started with a new AppFlowy board, copy the
[create-board prompt](prompts/create-board.md) into a chat with an AppFlowy-connected
model. It asks for the URL or name of the parent page, creates a child board with
the seven workflow statuses and colors, and returns the board reference and
status mapping to use in your project configuration. The prompt uses `In progress`,
so set `in_progress = "In progress"` in that project's `[projects.statuses]` table.

```sh
./install.sh
```

The installer builds the binary, installs the user service, and copies example
configuration. It preserves an existing `config.toml` and does not
start the service. Edit `~/.config/maistro/config.toml`; see
[config.example.toml](config.example.toml).

During install, create a [fine-grained GitHub personal access token](https://github.com/settings/personal-access-tokens/new)
with the appropriate resource owner and **only the repositories Maistro manages**.
Grant repository permissions **Contents: Read and write** for pushes,
**Workflows: Read and write** for workflow edits, and **Pull requests: Read and
write** for PR mode. Set an expiration and obtain organization approval if required.
The token owner's access and repository rules still apply: master mode requires
permission to push directly to `base_branch`; use PR mode when rules require PRs.
See [GitHub's token guide](https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/managing-your-personal-access-tokens).

Paste the token at the installer's hidden prompt. For unattended installation,
supply `MAISTRO_GITHUB_TOKEN` through the environment using your secret manager.
It is stored in `~/.config/maistro/github-token` with mode `0600` in a `0700`
directory, separate from TOML. Reinstalling preserves the token; supplying
`MAISTRO_GITHUB_TOKEN` explicitly replaces it atomically. Renew it before expiration
by supplying a replacement, or remove the old file and rerun the installer.
Never put a token in a project prompt or commit it. For a custom location, set
`agents.github_token_file`; provision that private file separately.

Each worker reads the file afresh and receives `GH_TOKEN` and `GITHUB_TOKEN`, so
rotation applies to the next assignment in both foreground and service runs.
The coordinator is not given these variables. Worker-scoped Git settings use
the token for `https://github.com/` and translate standard `git@github.com:` and
`ssh://git@github.com/` remotes to HTTPS without editing saved remotes or global
Git configuration. `gh` uses `GH_TOKEN`; no `gh auth login` is needed. This token
setup targets github.com; other Git hosts retain their existing authentication.

Each project needs a stable ID, a dedicated local Git checkout, exactly one
`appflowy_page`, `appflowy_url`, or `shortcut_url`, a `mode`, and a `base_branch`.
Different projects cannot share a repository, including linked worktrees. Maistro locks each Git
repository for the daemon's lifetime to prevent another Maistro instance writing
it concurrently. Use a checkout reserved for this worker; preserve any existing
interrupted work when restarting.

### Custom agent prompts

Add either or both of these optional UTF-8 files at the root of a project's
configured repository (`repo`):

- `maistro-coordinator.md`: instructions appended to the coordinator prompt,
  such as project-specific story selection or requirements gathering guidance.
- `maistro-worker.md`: instructions appended to that project's worker prompt,
  such as testing commands and coding conventions. See
  [the worker example](examples/project-prompt.md); copy it to this filename.

No `config.toml` setting is needed. Each file applies only to its agent, with
both Codex and Claude Code. Coordinator additions are labeled by project and
included only for enabled projects selected in that pass. The worker still reads
`AGENTS.md`. Additional agents remain disabled.

Files are read afresh for each coordinator pass or worker assignment, so edits,
additions and removals take effect on the next invocation without restarting.
Missing or empty files add no instructions; other read errors fail the invocation
with the project and path. Keep secrets out of these files.

To migrate, remove `prompt_file` from each project's TOML and move its contents
to `maistro-worker.md` in that project's repository root. The old configuration
key is no longer accepted. The installer preserves existing configuration and
no longer installs prompt examples under `~/.config/maistro/prompts`.
Other relative configuration paths resolve from the TOML file's directory;
`~/` expands from HOME. Restart the service to apply TOML changes.

### Global provider and models

Set these keys once in `[agents]`; they apply to every project. Per-project model
or provider settings are rejected.

```toml
[agents]
provider = "codex" # Default; or "claude" for Claude Code
small_model = "gpt-6-luna" # Coordinator
big_model = "gpt-6-astra" # Implementation workers
```

Model IDs and aliases are passed unchanged to the selected CLI. Omitting either
model uses that provider's default for the role: `gpt-6-luna` / `gpt-6-astra` for
Codex, `haiku` / `opus` for Claude. When switching providers, remove the explicit
models or replace them with models supported by the new provider. Empty names
are rejected; model availability is checked by the CLI on invocation.

For Claude Code, a minimal replacement `[agents]` section is:

```toml
[agents]
provider = "claude"
# small_model = "haiku"
# big_model = "opus"
# claude = "/absolute/path/to/claude" # Defaults to claude on PATH
# claude_mcp_config = "~/.claude.json"
```

Use an installed, authenticated Claude Code CLI supporting print mode, streaming
JSON, JSON Schema output, session-persistence control, strict MCP configuration,
tool and setting-source controls, and noninteractive permissions. `validate`
checks the required flags and `claude auth status` without invoking a model.
Authenticate with `claude auth login` before starting the service; normal Claude
credentials and authentication environment remain in use. Claude mode does not
require Codex, its login, or its configuration.

Configure story servers with **user scope** in Claude (for example,
`claude mcp add --scope user --transport http shortcut https://mcp.shortcut.com/mcp`),
and complete OAuth authorization in Claude before unattended use. Maistro reads
the top-level `mcpServers` object in `~/.claude.json`. `agents.claude_mcp_config`
can instead point at a JSON file in the same format, including when using a custom
Claude configuration directory; it selects connections, not the CLI's login home.
Source entries in repository `.mcp.json` or per-repository entries in
`~/.claude.json` are not used. Project `mcp_server` selects the named entry exactly
as with Codex. Do not put credentials in the Maistro TOML or prompts.

AppFlowy Rust polling accepts Claude stdio (`command`, `args`, `env`) and HTTP
(`"type": "http"`, `url`, `headers`) connections. Claude's `${VAR}` and
`${VAR:-default}` substitutions are expanded for polling. HTTP AppFlowy needs
explicit headers or a stdio connection; Rust cannot reuse CLI-managed OAuth.
Shortcut continues to use periodic coordinator reads with Claude's own OAuth.

Claude runs use `--print --output-format stream-json --json-schema` with session
persistence disabled. The coordinator has no built-in tools, loads only the
selected source connections, and automatically permits only the listed source
read/status/comment tools. Workers inherit normal Claude settings and development
tools with noninteractive full access; strict MCP configuration includes only
non-source connections from `claude_mcp_config`. Repository/plugin MCP entries
are excluded. Hooks and the Agent/Task spawning tools are disabled for both roles.
The same GitHub token, timeout, process-group cancellation, structured-result
validation, Git delivery and recovery rules apply to both providers.

See the official [Claude CLI reference](https://code.claude.com/docs/en/cli-reference),
[structured output guide](https://code.claude.com/docs/en/headless), and
[MCP configuration guide](https://code.claude.com/docs/en/mcp).

The coordinator uses only the MCP servers named by the projects in that pass,
with source-specific read tools and status/comment write tools. AppFlowy uses
`update_row_cells` and `append_comment`; Shortcut uses `update_story` and
`create_story_comment`. With Codex selected, the worker uses the normal Codex
configuration, tools, environment, network, and the configured GitHub token with full
filesystem access and no interactive approval prompts. All configured story-source
MCP servers are disabled for the worker; the coordinator owns story updates. Maistro disables
agent spawning and hooks in both roles. Scope and Git behavior are directed by the
prompts; there is no custom filesystem sandbox or Rust verification pipeline.

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
For operation without an interactive login, configure user lingering as usual.
`validate` checks local configuration, repository identities/remotes, the selected CLI login,
the private token file, and each enabled project's MCP configuration. It makes no
model calls and does not test token validity, remote board access or Git push permission.

The service runs as your user. It includes `~/.local/bin` and `~/.cargo/bin` in
PATH and optionally reads `~/.config/maistro/environment` (systemd environment-file
syntax). For tools installed elsewhere or SSH agent access, configure the user
service environment/drop-in. A systemd service does not source your interactive
shell startup files. The GitHub token file is read directly by Maistro, not sourced
as a shell script or systemd environment file.

For foreground diagnostics:

```sh
maistro --config ~/.config/maistro/config.toml run
```

SIGINT/SIGTERM stops the coordinator and all worker process groups. Use systemd
for unattended operation: `KillMode=control-group` also cleans up descendants
when the daemon crashes. Do not SIGKILL a foreground daemon and assume all of its
grandchildren have stopped before restarting it.

Private agent event/stderr logs live in `~/.local/state/maistro/logs` by default.
They may contain story/code/tool content. They are diagnostic only and are never
read to reconstruct workflow state. Clean old logs as needed. `runtime_dir` also
contains OS lock files and transient invocation files; it must be outside managed
repositories. No history, retry queue, project configuration, or story state is
persisted there. Transient result/schema files are deleted on normal completion;
crash leftovers are ignored and can be removed while the service is stopped.

## Read-only status page

Enable the optional server in `~/.config/maistro/config.toml`, then restart Maistro:

```toml
[status]
enabled = true
port = 8044
```

Open `http://127.0.0.1:8044/`. The server always binds to **127.0.0.1**, never a
LAN or tailnet interface. It is disabled by default; port defaults to 8044.
A bind failure stops startup with the address and OS error. `validate` checks
configuration but does not bind the port.

The static page refreshes `GET /api/status` every three seconds and marks retained
data disconnected/stale on failure. GET/HEAD are the only supported methods;
there are no workflow controls, file/log endpoints, or credentials in the status
snapshot. Titles are rendered as text and links allow only HTTP(S). Detailed
failure output stays in private daemon logs/the story source, not the web response.
There is no application login; local users and permitted tailnet users can read
project IDs, story titles/IDs, safe board/workflow and story links, and concise
diagnostics.

Current-process activity shows parallel workers, start times, elapsed durations,
cancellation, and completed/blocked results still awaiting source recording.
Idle and disabled projects remain visible. Coordinator activity and daemon uptime
are separate from board observations. Observations are in memory only and reset
on restart. For AppFlowy the page exposes the last Rust poll, last observed change,
dirty status, and a concise poll error. Shortcut shows periodic coordinator reads and
their observation windows, without claiming a Rust poll or change fingerprint.
The coordinator returns counts from its existing coordination reads; they can lag Rust polling until the next pass. Viewing the page makes
no agent or source calls. Counts never select or authorize work.

Ready means queued work; In Progress includes both active and interrupted work.
Backlog, Cancelled, Review, Blocked and Done are not automatically processed.
Counts use each project's configured status labels. Before a complete read,
counts are unknown. Failed, partial, missing or invalid observations retain the
last complete counts and mark them stale, even when other projects succeed.
Successful reads with failed writes still supply counts. Each successful read is
labelled with its coordinator pass start and receipt time: it happened within
that window and may precede status writes in the same pass. These are observations,
not a live or atomic board view. Overall counts sum known observations across all
configured projects, including stale ones, with coverage shown; missing projects
(including disabled projects) make it an explicitly partial subtotal.

### Private access over Tailscale

Inspect existing Serve mappings first. If the default HTTPS root is unused:

```sh
tailscale serve status
tailscale serve --bg http://127.0.0.1:8044
tailscale serve status
```

Open the private `https://<machine>.<tailnet>.ts.net` URL printed by Serve.
Follow Tailscale's setup link if prompted to enable HTTPS certificates; enable
MagicDNS in the tailnet DNS settings if needed. Access follows tailnet access
rules. Use **Serve**, not public **Funnel**.

Preserve existing mappings. If HTTPS 443 already serves another application,
choose an unused HTTPS port, for example:

```sh
tailscale serve --bg --https=8443 http://127.0.0.1:8044
```

Use the printed URL including `:8443`. To remove only the mapping you added,
repeat its flags and target with `off`:

```sh
# Default mapping, only if you added it:
tailscale serve --bg http://127.0.0.1:8044 off
# Or the separate 8443 mapping, only if you added that one:
tailscale serve --bg --https=8443 http://127.0.0.1:8044 off
tailscale serve status
```

Do not use `tailscale serve reset`, which removes unrelated mappings. If you
choose another Maistro port, replace 8044 in the targets above. Maistro does not
change Tailscale configuration. See the official
[Serve CLI reference](https://tailscale.com/docs/reference/tailscale-cli/serve)
and [Serve setup](https://tailscale.com/docs/features/tailscale-serve).

## Shortcut projects

Set `shortcut_url` instead of either AppFlowy target. Copy a link identifying one
workflow from your Shortcut workspace; a workspace home, individual story, epic,
or ambiguous saved search is not a workflow target. The coordinator resolves the link with
MCP and verifies workflow membership before claims and updates. An unresolved
link or missing/ambiguous status mapping produces a diagnostic and a retry, with
no dispatch. The scope includes all non-archived stories in that workflow, so use
a dedicated workflow when only a subset should be available to Maistro.

```toml
[[projects]]
id = "shortcut-service"
repo = "/absolute/path/to/service"
shortcut_url = "https://app.shortcut.com/your-workspace/settings/workflows/123"
# Replace the illustrative URL above with your workflow's actual link.
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

Create/map seven distinct workflow states with these exact names. Shortcut state
categories such as started/completed and its dependency `blocked` flag are not
substitutes for these states. Maistro reads existing workflows; it does not create
or change their definitions. The normal claim, cancellation, recovery, dependency,
PR override and result-delivery rules below apply to Shortcut as well. Numeric
public story IDs remain stable across restarts. Completion comments include the
same idempotency marker and read-back checks as AppFlowy.

Configure and authorize the hosted Shortcut MCP in the selected CLI, following
[Shortcut's MCP setup](https://www.shortcut.com/help/integrations/mcp-server/).
The hosted endpoint is `https://mcp.shortcut.com/mcp` and uses OAuth. For Codex,
use the same `agents.codex_home` as Maistro; for Claude, use a user-scoped entry
as described above. Maistro preserves the server name and uses the CLI's login;
it does not copy OAuth tokens into project configuration or use a separate
Shortcut API key.
Complete authorization before starting the unattended service. `validate` only
checks local configuration, not remote authentication, workflow access or states.

`projects.mcp_server` overrides the server name for either source, allowing
separate workspace connections per project. Its defaults are `shortcut` for
Shortcut and `agents.mcp_server` (normally `appflowy`) for AppFlowy. Configure each
named entry in `agents.codex_home/config.toml` for Codex, or the top-level
`mcpServers` in `agents.claude_mcp_config` for Claude. Maistro does not load MCP entries
from individual repository config files. Projects can mix both sources in one
process, and a Shortcut-only installation needs no AppFlowy server. One server
name cannot be assigned to both source types. Do not configure overlapping
workflows/boards against separate checkouts.

**Shortcut uses periodic coordinator reads**, including when idle: the Rust MCP transport
cannot reuse CLI-managed OAuth credentials. `poll_interval_seconds` is the delay
from a completed pass to the next Shortcut pass (or a failed pass to its retry).
Worker completion can trigger earlier coordination. One global pass still runs
at a time; failures retain pending results, and other projects' workers continue.
This incurs model calls while idle; increase the interval to reduce frequency at
the cost of dispatch/cancellation latency. AppFlowy's Rust change detection is
unchanged. Shortcut has no Rust fingerprint or atomic snapshot: fresh, paginated
reads and verified claims/status writes are coordinator responsibilities. Read
failures never authorize work, and a Shortcut outage prevents cancellation checks.

## Story behavior and recovery

Use Backlog, Ready, In Progress, Review, Done, Blocked, and Cancelled, or map their
exact labels in `[projects.statuses]`. Existing boards with `In progress` should
set `in_progress = "In progress"`.

- Ready authorizes new work. The coordinator honors dependencies and priority and gathers
  the full story body, human comments, and linked reference documents.
- In Progress stories without a live worker are recovered before new work.
  The worker inspects existing changes, commits, remote branches and PRs before
  continuing, including work finished just before a crash. It must preserve
  unrelated local changes and avoid duplicate commits/PRs.
- Backlog and Cancelled never authorize work. Moving an active story to either,
  or otherwise out of In Progress, stops its worker when the coordinator observes the change.
  Existing files, commits and PRs remain. Maistro preserves the user-selected
  status. Requirement edits alone do not restart active work.
- A worker failure or unresolved obstacle becomes Blocked with an explanation.
  To retry, resolve the obstacle and move the story back to Ready.
- Completed results stay in process memory until the coordinator confirms the source
  update or observes a user status change. The project remains occupied until
  then. Other projects continue. A coordinator failure retries after the polling
  interval, without releasing those pending results.

For AppFlowy, Rust polls enabled boards directly over each project's configured
MCP connection.
`orchestrator.poll_interval_seconds` (30 seconds by default) is the delay between
completed poll attempts per project, **not** a schedule for model calls. After a
valid initial observation and coordination, an unchanged idle/empty board makes
no further coordinator calls. Changes to any story, including Done or Backlog stories,
and changes written by Maistro itself all wake coordination.

One global coordinator pass runs at a time and coalesces dirty projects. Polling and
project workers continue while it runs. A pass acknowledges only the observed
generation at its start; changes observed during that pass remain dirty for a
later pass. Worker completion, failure, cancellation completion and result
acknowledgement also wake the coordinator, so pending delivery and queued work do not wait
for a board edit. Failed/incomplete coordination and unacknowledged results retry
after the polling interval, independently for each project.

The coordinator still reads fresh data, checks active-story statuses first, selects and
authorizes stories, writes results, and decides cancellation. Cancellation latency
normally includes up to one polling interval, a complete board read, and the coordinator's
read/decision time. An existing coordinator pass, large boards, inconsistent reads,
failures and the coordinator timeout can extend that latency. A failing project's
poll does not block other projects. There is no cancellation detection during an
AppFlowy outage. A push already underway may finish before cancellation is observed.

### Polling contract and consistency

The AppFlowy poller reuses `projects.mcp_server` (default `agents.mcp_server`)
from the selected provider configuration. Codex supports stdio `command`, `args`,
`cwd`, `env` and inherited environment, or streamable HTTP `url`, `http_headers`,
`env_http_headers`, and `bearer_token_env_var`. Claude transport keys are described
in the global provider section above. It uses the
MCP Rust SDK directly, without a model. The server must expose the legacy read
tools already used by the coordinator: `get_workspaces`, `list_databases`, `get_page`,
`get_database_fields`, `get_database_row_ids`, `get_database_row_details`,
`get_page_markdown`, and `list_comments`. Enable that tool profile in AppFlowy MCP
if necessary. CLI-managed HTTP OAuth credentials and legacy SSE transport are
not shared by this client; use the configured stdio server or streamable HTTP
headers/token. Connection/authentication errors are reported and retried without
falling back to idle model calls. `validate` remains a local check, not a live
board probe.

Boards resolve uniquely by view name/ID (`appflowy_page`) or view ID in an
AppFlowy URL; ambiguous names require a URL or ID. Each project keeps only a
SHA-256 fingerprint, dirty generations and retry timing in memory. Poll responses
are transient; no board content, observation baseline or queue is written to disk.
Restart discards comparison state: every enabled project starts unknown/dirty,
obtains a complete observation, then runs the coordinator to recover In Progress work or
start Ready work.

No board timestamp or updated-row feed is assumed to cover documents/comments.
The fingerprint includes board name/icon/layout/extra metadata, every field and
its options, row membership, every row's cells, full Markdown bodies for rows
with `has_doc=true`, row comments/replies (including resolved threads), and body
inline comments. Rows/fields/comments are keyed by ID and JSON objects by key;
semantic array ordering and Markdown are preserved. MCP request metadata and
pagination offsets are excluded. **Edits to external linked reference documents
alone are outside this detector's scope**; the coordinator still reads relevant references
when processing a story.

The read contract was checked against AppFlowy MCP's legacy handlers
(`appflowy_mcp/server.py`, revision `008128fabb8459025c2c4fa124ea8713988af0e0`)
and AppFlowy Cloud's `AFDatabaseRowDetail`/`list_database_row_details` implementations
in `libs/shared-entity/src/dto/workspace_dto.rs` and `src/biz/collab/ops.rs`.
Row-detail reads can silently omit unavailable rows; their optional `doc` is plain
text and can remain null on document errors. Therefore the poller checks exact
requested/returned row IDs and reads Markdown separately instead of accepting
those omissions as empty stories. Body presence uses AppFlowy's explicit
`has_doc` flag; a failed body read is never an empty body. Row comments live in
row collabs; inline body comments use the derived document UUID, so both are read.

Comments are fully paginated; malformed/nonadvancing pages, truncated or partial
responses, missing rows, and failed reads reject the entire observation. Field
and row membership reads bookend each scan, and two complete scans must agree
before a fingerprint is accepted. These checks reduce cross-resource/pagination
races, but AppFlowy does not provide an atomic board snapshot. Consistently stale
server reads and edits made and reverted between observations cannot be detected;
ongoing edits may defer acceptance until a stable read. On rejection the last
valid fingerprint and dirty state remain intact. Retries wait the configured
interval; each poll is bounded by `coordinator_timeout_seconds` (individual MCP
reads also have a 60-second limit). Failed projects do not make other pollers wait.

After restart all in-memory assignments and pending results are gone. The coordinator reads
the configured source afresh; the worker reconstructs interrupted work from the board
and repository.
A crash between a remote operation and its source update is reconciled by the
models, not an exactly-once transaction. Reads and status writes are not atomic
with human edits. Do not run two independently configured daemons against the
same board with different clones. Source claims and outcome read-backs are
agent responsibilities; Rust checks response shape and concurrency, not every
remote side effect.

## Migrating the previous version

Stop the old service before installing. Replace the old TOML using the new example;
unknown legacy settings are rejected with a migration error. Remove `state_db`,
worktree settings, reviewer settings, worker limits, retry budgets, Rust
`test_commands`, GitHub/CI settings and path/sandbox policies. Put testing
requirements in each repository's `maistro-worker.md`. Set `mode` and
`base_branch` explicitly.

The old SQLite file is not read, migrated, or deleted. Reconcile old AppFlowy
statuses and preserve any unfinished worktrees/branches before starting the new
version. Point the project at the checkout containing unfinished work, or bring
that work into its dedicated checkout. Existing PRs in Review remain user-owned.
The old status/pause/retry/cancel/reload/config/project/create-board commands are
removed; use AppFlowy, a text editor, and systemd. Create/configure boards in
AppFlowy itself. Rerun the installer to save a fine-grained GitHub token in the
new private file. Workers use that file instead of old token environment settings.

## Development

```sh
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
python3 -m unittest discover -s tests -p 'test_install.py'
node --test tests/status_ui.test.cjs
cargo build --release --locked
```

The UI tests use Node.js 20+ and its built-in test runner; there are no frontend
dependencies or build steps.

Tests use simulated Codex and Claude Code agents, stdio/HTTP MCP fixtures, and local Git remotes. They
never call live models, write a real board, or push to an external repository.
