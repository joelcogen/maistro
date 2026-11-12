# Maistro

Maistro runs one Luna coordinator for AppFlowy and one Astra coding worker per
project. Projects run in parallel; each project handles one story at a time.
AppFlowy is the source of truth. There is no SQLite database, local board mirror,
independent reviewer, CI watcher, or merge pipeline.

Luna selects a story, gathers its complete requirements and linked context, and
confirms it is In Progress. Astra implements it, runs tests, fixes failures,
commits and pushes. Luna then records the result in AppFlowy.

## Project modes

| Mode | Worker responsibility | Successful story status |
| --- | --- | --- |
| `pr` | Create/reuse a story branch, implement, test, commit, push, and open/update a PR against `base_branch`. | Review, with PR link |
| `master` | Implement and test directly on `base_branch`, then commit and push it. | Done, with commit reference |

`base_branch` can be `main`, `master`, or another branch. CI and PR merging belong
to the user. Maistro does not watch PRs after submission or move Review to Done.
Astra uses stable `maistro/<project>/<story-id>` branch names in PR mode and
`AppFlowy story: <story-id>` commit/PR markers to find interrupted work.

## Install and configure

Requires Linux, Git, Rust 1.89+, a logged-in Codex CLI supporting `exec --json`,
`--output-schema`, `--ephemeral` and `--ignore-user-config`, and an AppFlowy MCP
entry in your existing Codex configuration. PR projects also need `gh` (GitHub CLI).
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
configuration/prompt files. It preserves an existing `config.toml` and does not
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
`appflowy_page` or `appflowy_url`, a `mode`, and a `base_branch`. Different projects
cannot share a repository, including linked worktrees. Maistro locks each Git
repository for the daemon's lifetime to prevent another Maistro instance writing
it concurrently. Use a checkout reserved for this worker; preserve any existing
interrupted work when restarting.

Project `prompt_file` is optional. It adds project-specific instructions to the
complete [worker prompt](prompts/worker.md): testing commands, coding conventions,
and completion expectations. See [the example](examples/project-prompt.md).
Astra also reads the repository's AGENTS.md. Prompt files are read for each
assignment; restart the service to apply TOML changes. Relative paths resolve
from the TOML file's directory; `~/` expands from HOME. Models are fixed to
`gpt-6-luna` and `gpt-6-astra`; additional agents are disabled.

The coordinator uses only the configured AppFlowy MCP, with story read tools,
`update_row_cells` and `append_comment`. The worker uses the normal Codex
configuration, tools, environment, network, and the configured GitHub token with full
filesystem access and no interactive approval prompts. The configured AppFlowy
MCP is disabled for the worker; Luna owns story updates. Maistro disables agent
spawning and hooks in both roles. Scope and Git behavior are directed by the
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
`validate` checks local configuration, repository identities/remotes, Codex login,
the private token file, and the AppFlowy MCP configuration. It makes no model
calls and does not test token validity, remote board access or Git push permission.

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

Private Codex event/stderr logs live in `~/.local/state/maistro/logs` by default.
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
failure output stays in private daemon logs/AppFlowy, not the web response.
There is no application login; local users and permitted tailnet users can read
project IDs, story titles/IDs, safe AppFlowy links, and concise diagnostics.

Current-process activity shows parallel workers, start times, elapsed durations,
cancellation, and completed/blocked results still awaiting AppFlowy recording.
Idle and disabled projects remain visible. Coordinator activity and daemon uptime
are separate from AppFlowy observations. Observations are in memory only and reset
on restart. The page exposes each project's last Rust poll, last observed change,
dirty status, and a concise poll error. Luna returns counts from its existing
coordination reads; they can lag Rust polling until the next pass. Viewing the page makes
no agent or AppFlowy calls. Counts never select or authorize work.

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

## AppFlowy behavior and recovery

Use Backlog, Ready, In Progress, Review, Done, Blocked, and Cancelled, or map their
exact labels in `[projects.statuses]`. Existing boards with `In progress` should
set `in_progress = "In progress"`.

- Ready authorizes new work. Luna honors dependencies and priority and gathers
  the full story body, human comments, and linked reference documents.
- In Progress stories without a live worker are recovered before new work.
  Astra inspects existing changes, commits, remote branches and PRs before
  continuing, including work finished just before a crash. It must preserve
  unrelated local changes and avoid duplicate commits/PRs.
- Backlog and Cancelled never authorize work. Moving an active story to either,
  or otherwise out of In Progress, stops its worker when Luna observes the change.
  Existing files, commits and PRs remain. Maistro preserves the user-selected
  status. Requirement edits alone do not restart active work.
- A worker failure or unresolved obstacle becomes Blocked with an explanation.
  To retry, resolve the obstacle and move the story back to Ready.
- Completed results stay in process memory until Luna confirms the AppFlowy
  update or observes a user status change. The project remains occupied until
  then. Other projects continue. A coordinator failure retries after the polling
  interval, without releasing those pending results.

Rust polls enabled boards directly over the configured AppFlowy MCP connection.
`orchestrator.poll_interval_seconds` (30 seconds by default) is the delay between
completed poll attempts per project, **not** a schedule for model calls. After a
valid initial observation and coordination, an unchanged idle/empty board makes
no further Luna calls. Changes to any story, including Done or Backlog stories,
and changes written by Maistro itself all wake coordination.

One global Luna pass runs at a time and coalesces dirty projects. Polling and
project workers continue while it runs. A pass acknowledges only the observed
generation at its start; changes observed during that pass remain dirty for a
later pass. Worker completion, failure, cancellation completion and result
acknowledgement also wake Luna, so pending delivery and queued work do not wait
for a board edit. Failed/incomplete coordination and unacknowledged results retry
after the polling interval, independently for each project.

Luna still reads fresh data, checks active-story statuses first, selects and
authorizes stories, writes results, and decides cancellation. Cancellation latency
normally includes up to one polling interval, a complete board read, and Luna's
read/decision time. An existing Luna pass, large boards, inconsistent reads,
failures and the coordinator timeout can extend that latency. A failing project's
poll does not block other projects. There is no cancellation detection during an
AppFlowy outage. A push already underway may finish before cancellation is observed.

### Polling contract and consistency

The poller reuses `agents.mcp_server` from the Codex configuration: stdio
`command`, `args`, `cwd`, `env` and inherited environment, or streamable HTTP
`url`, `http_headers`, `env_http_headers`, and `bearer_token_env_var`. It uses the
MCP Rust SDK directly, without a model. The server must expose the legacy read
tools already used by Luna: `get_workspaces`, `list_databases`, `get_page`,
`get_database_fields`, `get_database_row_ids`, `get_database_row_details`,
`get_page_markdown`, and `list_comments`. Enable that tool profile in AppFlowy MCP
if necessary. Codex-managed HTTP OAuth credentials and legacy SSE transport are
not shared by this client; use the configured stdio server or streamable HTTP
headers/token. Connection/authentication errors are reported and retried without
falling back to idle model calls. `validate` remains a local check, not a live
board probe.

Boards resolve uniquely by view name/ID (`appflowy_page`) or view ID in an
AppFlowy URL; ambiguous names require a URL or ID. Each project keeps only a
SHA-256 fingerprint, dirty generations and retry timing in memory. Poll responses
are transient; no board content, observation baseline or queue is written to disk.
Restart discards comparison state: every enabled project starts unknown/dirty,
obtains a complete observation, then runs Luna to recover In Progress work or
start Ready work.

No board timestamp or updated-row feed is assumed to cover documents/comments.
The fingerprint includes board name/icon/layout/extra metadata, every field and
its options, row membership, every row's cells, full Markdown bodies for rows
with `has_doc=true`, row comments/replies (including resolved threads), and body
inline comments. Rows/fields/comments are keyed by ID and JSON objects by key;
semantic array ordering and Markdown are preserved. MCP request metadata and
pagination offsets are excluded. **Edits to external linked reference documents
alone are outside this detector's scope**; Luna still reads relevant references
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

After restart all in-memory assignments and pending results are gone. Luna reads
AppFlowy afresh; Astra reconstructs interrupted work from the board and repository.
A crash between a remote operation and its AppFlowy update is reconciled by the
models, not an exactly-once transaction. Reads and status writes are not atomic
with human edits. Do not run two independently configured daemons against the
same board with different clones. AppFlowy claims and outcome read-backs are
agent responsibilities; Rust checks response shape and concurrency, not every
remote side effect.

## Migrating the previous version

Stop the old service before installing. Replace the old TOML using the new example;
unknown legacy settings are rejected with a migration error. Remove `state_db`,
worktree settings, reviewer settings, worker limits, retry budgets, Rust
`test_commands`, GitHub/CI settings and path/sandbox policies. Put testing
requirements in each project's prompt file. Set `mode` and `base_branch` explicitly.

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

Tests use simulated Codex agents, stdio/HTTP MCP fixtures, and local Git remotes. They
never call live models, write a real board, or push to an external repository.
