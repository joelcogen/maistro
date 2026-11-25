# Maistro

Maistro turns AppFlowy and Shortcut stories into tested, committed code. A
coordinator picks stories and records results on the board or workflow. A Codex
or Claude Code worker implements, tests, and delivers each change.
Projects run in parallel, with one active story per project.

See [HOW_IT_WORKS.md](HOW_IT_WORKS.md) for behavior and recovery details.

## Project modes

| Mode | Worker responsibility | Successful story status |
| --- | --- | --- |
| `pr` | Create/reuse a story branch, implement, test, commit, push, and open/update a PR against `base_branch`. | Review, with PR link |
| `master` | Implement, test, and commit directly on `base_branch`. Push when a remote is configured. | Done, with commit SHA or link |

`base_branch` can be `main`, `master`, or another branch. You handle CI, merge PRs,
and move reviewed stories to Done. To request PR delivery for an individual story,
see [delivery overrides](HOW_IT_WORKS.md#project-modes-and-delivery).

Projects use `origin` by default. Set `remote` to a string to select another
named remote. For local-only story and dream delivery, set the following in
the project and make sure its configured `base_branch` already exists locally.

```toml
mode = "master"
remote = false
```

Maistro tests and commits changes on the local base branch without fetching or
pushing. Completed stories move to Done with the commit SHA. A PR request on a
local-only project moves the story to Blocked.

## Dreams

Put UTF-8 Markdown instructions in `dreams/` at the root of a project's checkout.
Maistro runs the `.md` files in alphabetical filename order, one worker invocation
per file. For symlinks to regular files, Maistro uses the link name for ordering
and reads the target for instructions. Use prefixes such as `01-` and `02-` to set
the order.

Dreams can start at `03:00` in the daemon's local timezone by default. Set
`orchestrator.dream_time` to change the time. See
[daily dream phases](HOW_IT_WORKS.md#daily-dream-phases) for when Maistro runs
dreams and how it delivers their changes.

```toml
[orchestrator]
dream_time = "03:00" # HH:MM in the daemon's local timezone
```

To enable the [test pruning](examples/dreams/prune_tests.md),
[documentation review](examples/dreams/documentation_style.md), and
[deployment](examples/dreams/deploy.md) examples in a Maistro checkout, create
these links. Git ignores the local `dreams/` directory.

```sh
mkdir -p dreams
ln -s ../examples/dreams/prune_tests.md dreams/01-prune-tests.md
ln -s ../examples/dreams/documentation_style.md dreams/02-documentation-style.md
ln -s ../examples/dreams/deploy.md dreams/03-deploy.md
```

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
into a chat with an AppFlowy-connected model. Give it the parent page when asked.
Use the new board's URL or unique name in your project configuration. The prompt
creates the seven default statuses, including `In Progress`.

Use `projects.mcp_server` to select a named source connection. The default is
`shortcut` for Shortcut and `agents.mcp_server` (normally `appflowy`) for AppFlowy.
Configure the entry in `agents.codex_home/config.toml` for Codex or in the
top-level `mcpServers` object for Claude Code. You can mix sources in one daemon.
Use a separate server name for each source type and one project per board or
workflow.

### AppFlowy MCP connection

Use AppFlowy MCP's default compact profile (`APPFLOWY_TOOL_PROFILE=compact`).
Polling needs `appflowy_fetch`, `appflowy_database`, and `appflowy_rows`.
The coordinator also needs `appflowy_update` to post results.

Use stdio or streamable HTTP. For HTTP polling, configure authentication in the
MCP entry because the poller connects directly.

| Provider | Stdio keys | HTTP keys |
| --- | --- | --- |
| Codex | `command`, `args`, `cwd`, `env` | `url`, `http_headers`, `env_http_headers`, `bearer_token_env_var` |
| Claude Code | `command`, `args`, `env` | `"type": "http"`, `url`, `headers` |

Codex stdio inherits the process environment. Maistro expands `${VAR}` and
`${VAR:-default}` in Claude MCP settings.

### GitHub token

Only enabled projects with a named remote require a GitHub token. For a
token-free installation, create `~/.config/maistro/config.toml` before running
`./install.sh` and set `remote = false` and `mode = "master"` on every enabled
project.

For remote-backed projects, create a [fine-grained GitHub personal access token](https://github.com/settings/personal-access-tokens/new)
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

### Custom agent prompts

Copy either or both examples to the root of a project's configured repository
(`repo`), keeping the filenames. Edit the UTF-8 Markdown to fit the project.
Maistro appends each file to its corresponding agent prompt.

- [maistro-coordinator.md](examples/maistro-coordinator.md) shows how to gather
  project-specific requirements for the worker's assignment.
- [maistro-worker.md](examples/maistro-worker.md) shows how to specify local
  checks and repository conventions.

To migrate, remove the obsolete `prompt_file` TOML key and move its contents to
`maistro-worker.md` in the repository root.
Other relative configuration paths resolve from the TOML file's directory;
`~/` expands from HOME. Restart the service to apply TOML changes.

### Global provider, models and effort

Set the provider, models and optional effort once in `[agents]` for every project:

```toml
[agents]
provider = "codex" # Default; or "claude" for Claude Code
coordinator = "gpt-6-luna" # Coordinator
worker = "gpt-6-astra" # Implementation workers
# Optional; omit to preserve inherited configuration/provider defaults.
# coordinator_effort = "low"
# worker_effort = "high"
```

Codex defaults to `gpt-6-luna` for coordination and `gpt-6-astra` for workers.
Claude Code defaults to `haiku` and `opus`, respectively. Maistro rejects empty
model names and known IDs from the other provider. The selected CLI checks model
and alias availability. When switching providers, remove explicit models or
replace them with supported ones.

Optional `coordinator_effort` (coordinator) and `worker_effort` (all workers)
accept these exact lowercase values:

| Provider | Effort values | CLI override |
| --- | --- | --- |
| Codex | `none`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max`, `ultra` | `-c 'model_reasoning_effort="VALUE"'` |
| Claude | `low`, `medium`, `high`, `xhigh`, `max` | `--effort VALUE` |

Maistro rejects other values, including empty strings, during validation. Claude
must advertise `--effort` when any global or project effort setting is configured.
Model-specific availability still depends on the selected CLI and model; these lists do not
guarantee every model supports every level. See the
[Codex configuration reference](https://learn.chatgpt.com/docs/config-file/config-reference)
and [Claude effort documentation](https://code.claude.com/docs/en/model-config#adjust-effort-level).
Claude's `ultracode` mode is not an effort level supported by Maistro.

Set `worker` or `worker_effort` in `[[projects]]` to override that worker setting
for one project. Omit either field to inherit its value from `[agents]`. If
you omit it in both places, Maistro uses the provider's default worker model or
leaves effort to the CLI. Project effort accepts the same values as global effort.

```toml
[[projects]]
id = "backend"
repo = "/work/backend"
appflowy_page = "Backend Stories"
mode = "pr"
base_branch = "main"
worker = "gpt-6-astra"
worker_effort = "high"

[[projects]]
id = "frontend"
repo = "/work/frontend"
appflowy_page = "Frontend Stories"
mode = "pr"
base_branch = "main"
worker = "gpt-6-luna"
worker_effort = "medium"
# Remove worker to inherit only the global model, or remove
# worker_effort to inherit only the global effort.
```

For Claude Code, replace `[agents]` with:

```toml
[agents]
provider = "claude"
# coordinator = "haiku"
# worker = "opus"
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
Claude's login home stays separate. Keep credentials in their private
configuration files.

See the [Claude CLI reference](https://code.claude.com/docs/en/cli-reference),
[structured output guide](https://code.claude.com/docs/en/headless), and
[MCP configuration guide](https://code.claude.com/docs/en/mcp).

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
configuration, repository identities, configured remotes or local base branches,
the selected CLI login and enabled projects' MCP configuration. It checks the
private GitHub token file when an enabled project uses a named remote. For
AppFlowy it also checks compact tool availability without reading boards.
Remote authentication, board access and push permissions are checked during
operation.

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
logs as needed. `runtime_dir` holds logs, OS locks, dream checkpoints and transient invocation files;
it must be outside managed repositories. Normal completion deletes result/schema
files. Crash leftovers can be removed while the service is stopped. Recovery uses
the story source and Git repository.

## Dashboard

Enable the server in `~/.config/maistro/config.toml`, then restart Maistro:

```toml
[status]
enabled = true
port = 8044
```

Open `http://127.0.0.1:8044/`. The server binds only to **127.0.0.1**. It starts
disabled and defaults to port 8044. A bind failure stops startup with the address
and OS error. Port binding happens at startup; `validate` checks configuration.

Workers show **Dreaming** while they run a dream phase. Use **Dream now** on an
enabled project's card to request a phase without waiting for the schedule. Maistro
checks the board and reports whether the phase started or why it was rejected.
**Dream now** also works when the dashboard is served through a private reverse proxy.
The usual [dream conditions](HOW_IT_WORKS.md#daily-dream-phases) still apply.

The dashboard uses a dark blue palette. Cargo embeds the precompiled CSS and
MIT license from `src/vendor/tabler-1.7.5` in the Maistro executable.
[Tabler 1.7.5 package](https://www.npmjs.com/package/@tabler/core/v/1.7.5)

## Shortcut projects

Select one workflow with `shortcut_workflow` (exact, case-sensitive name),
`shortcut_workflow_id` (positive integer), or an existing `shortcut_url` workflow
link. Set exactly one selector; combining selectors is rejected even if they refer
to the same workflow. Names must be unique in the configured MCP connection's
workspace; use an ID when names are ambiguous. Numeric-looking names remain names.

### Read-only API polling (recommended)

Create a **v4 API token** in Shortcut's
[Settings → API tokens](https://app.shortcut.com/settings/account/api-tokens).
Select the intended workspace and grant only **read** access. The token starts
with `sct_r_` (older read-only v4 tokens use `sct_ro_`). A v3 token or a
write-enabled token is not accepted for polling. The
[v4 API](https://developer.shortcut.com/api/rest/v4) is currently beta.

Save the token as the only line of a private file, for example
`~/.config/maistro/shortcut-deecide-token`. Create it with owner-only permissions
before pasting the token; do not put the token in TOML or repository files:

```sh
install -d -m 700 ~/.config/maistro
(umask 077; touch ~/.config/maistro/shortcut-deecide-token)
chmod 600 ~/.config/maistro/shortcut-deecide-token
# Open the file in your editor and paste the read-only token on one line.
```

Add **both** `shortcut_workspace` (the workspace slug from its app URL) and
`shortcut_token_file` to the project's configuration, alongside its workflow
selector, as shown below. Paths support `~`; relative paths resolve from the
configuration file's directory. Each workspace needs its own token; projects in
one workspace may share a token file. Token files are reopened each read cycle,
so replacing the token does not require a daemon restart. `maistro validate`
checks the local file and token format; actual authorization is checked on reads.

Keep the CLI's Shortcut OAuth login configured for the same workspace. Maistro
uses OAuth for story details and writes. To see how polling works with or without
an API token, see [Shortcut scheduling](HOW_IT_WORKS.md#shortcut-scheduling).

```toml
[[projects]]
id = "shortcut-service"
repo = "/absolute/path/to/service"
shortcut_workflow = "Agents"
shortcut_workspace = "deecide" # Your workspace slug
shortcut_token_file = "~/.config/maistro/shortcut-deecide-token"
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

Use the exact names from the Shortcut workflow's state list in
`[projects.statuses]`.

Configure and authorize the hosted Shortcut MCP in your selected CLI using
[Shortcut's MCP setup](https://www.shortcut.com/help/integrations/mcp-server/).
The endpoint is `https://mcp.shortcut.com/mcp` and uses OAuth. For Codex, use
Maistro's `agents.codex_home`; for Claude, use a user-scoped entry. Maistro uses
the server's configured name and the CLI's login. Complete authorization before
starting the service; remote access is checked during operation.

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
