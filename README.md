# Maistro

Maistro turns AppFlowy and Shortcut stories into tested, committed code. One
coordinator selects stories and records results; one coding worker per project
implements, tests, fixes failures, commits and pushes. Projects run in parallel,
with one active story per project. Each project's board or workflow holds its
story state.

Choose Codex or Claude Code and configure the coordinator and worker models
globally. The default is Codex with `gpt-6-luna` for coordination and
`gpt-6-astra` for implementation.

See [HOW_IT_WORKS.md](HOW_IT_WORKS.md) for behavior and recovery details.

## Project modes

| Mode | Worker responsibility | Successful story status |
| --- | --- | --- |
| `pr` | Create/reuse a story branch, implement, test, commit, push, and open/update a PR against `base_branch`. | Review, with PR link |
| `master` | Implement and test directly on `base_branch`, then commit and push it. | Done, with commit reference |

`base_branch` can be `main`, `master`, or another branch. You handle CI, PR merging
and moving reviewed stories to Done.

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

### AppFlowy MCP connection

The AppFlowy poller uses `projects.mcp_server` (default `agents.mcp_server`) from
the selected provider's configuration. Codex supports stdio `command`, `args`,
`cwd`, `env` and inherited environment, or streamable HTTP `url`, `http_headers`,
`env_http_headers` and `bearer_token_env_var`. Claude transport keys are described
[below](#global-provider-models-and-effort). Rust polling uses the MCP SDK directly.

Enable AppFlowy MCP's legacy read tools: `get_workspaces`, `list_databases`,
`get_page`, `get_database_fields`, `get_database_row_ids`,
`get_database_row_details`, `get_page_markdown` and `list_comments`. Use stdio or
streamable HTTP with explicit headers/token for polling authentication.
Connection/authentication errors are reported and retried at the polling interval.

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

### Custom agent prompts

Add either or both optional UTF-8 files at the root of a project's configured
repository (`repo`):

- `maistro-coordinator.md`: story selection or requirements gathering instructions
  appended to the coordinator prompt.
- `maistro-worker.md`: testing commands and coding conventions appended to the
  worker prompt. Start with [the worker example](examples/project-prompt.md).

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
stdio is also supported. Shortcut uses read-only API polling when configured;
Claude's OAuth connection remains responsible for individual story reads and writes.

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

Keep the CLI's Shortcut OAuth login configured for the **same
workspace**; the read-only API token does not replace write authentication.

Without the two API settings, existing CLI/MCP-only projects retain periodic
coordinator reads, including while idle. That compatibility mode may need to
enumerate the entire workspace if the MCP lacks workflow-scoped listing.

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
