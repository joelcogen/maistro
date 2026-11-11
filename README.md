# Maistro

Maistro turns AppFlowy stories into tested pull requests. Move a story to Ready,
and it uses Luna to read the requirements, Astra to write the code, and a separate
Astra session to review it. Rust runs the tests, checks GitHub CI and decides
whether the change can merge. It runs on your server without an open chat.

Each project connects one AppFlowy board to one local Git repository. Work happens
in separate Git worktrees, and SQLite keeps track of it across restarts.
Automatic merging is off by default.

## Install

You need Linux, Git, Bubblewrap, Rust 1.89 or newer, a C compiler/linker (`cc`),
and a logged-in Codex CLI with AppFlowy MCP configured. On Debian or Ubuntu,
install `build-essential` to provide the compiler and linker needed by the Rust
build and bundled SQLite. The host must allow Bubblewrap's user, process and network
namespaces. Development used Codex 0.167.0; CI uses Rust 1.93.1. Keep Codex's helper
executables beside its main binary.

From this checkout, run the interactive installer:

```sh
./install.sh
```

It builds and installs Maistro, explains how to create a GitHub token and prompts
for it without echoing it, then saves it in the private environment file. It opens
the installed `config.toml` with `$EDITOR` (or `vi`) and runs `validate` with the
token. Set an editor that waits for editing to finish, for example
`EDITOR="code --wait" ./install.sh`. Keep `github.token_env` set to
`MAISTRO_GITHUB_TOKEN`.

Existing installed configuration is preserved; on first installation, the script
copies the checkout's `config.toml` if present, otherwise `config.example.toml`.
Rerunning reuses the existing environment file without prompting for a token.
Saved environment settings are loaded for validation; edit the environment file
directly if you need to replace the token.
Validation accesses GitHub and AppFlowy and writes local diagnostics. The script
stops on errors and does not start the daemon. After successful validation, follow
the service commands under [Validate and start](#validate-and-start).

Alternatively, install manually:

```sh
cd ~/dev/maistro
cargo build --release --locked
install -Dm755 target/release/maistro "$HOME/.local/bin/maistro"
mkdir -p "$HOME/.config/maistro"
chmod 700 "$HOME/.config/maistro"
cp --update=none config.example.toml "$HOME/.config/maistro/config.toml"
chmod 600 "$HOME/.config/maistro/config.toml"
```

## Configure a project

Edit `~/.config/maistro/config.toml`. For each project, set:

- A stable `id` and an absolute `repo` path. The repository needs a clean working
  tree, an initial commit, an existing base branch and a GitHub `origin`.
- `github_repo` as `owner/repository`, matching that remote.
- One `appflowy_url` or an exact `appflowy_page` name. Use the URL if names overlap.
- `test_commands`, `required_checks` and `enabled = true` when setup is complete.

On startup, `config.toml` takes precedence over saved project settings in SQLite.
Changed settings replace the saved configuration and invalidate cached scans;
run history, pending updates and pause controls are preserved. Pending retries wait
for a successful fresh board scan before their authorization is checked. A board, repository
or base branch can change under the same project ID only at startup and only if
that project has no saved runs or pending updates. Otherwise use a new project ID
so existing work cannot be recovered against a different target.

Projects must use different repositories. Runtime data defaults to
`~/.local/state/maistro/`: `state.db`, `worktrees/` and `logs/`. This location belongs
to the operating-system user running Maistro and is shared by that daemon's
configured projects. Omit `state_db`, `worktree_root` and `log_root` to use these
defaults, or override them individually. Runtime paths beginning with `~/` expand
using `HOME`; other relative paths resolve from the configuration file's directory.
Keep runtime data outside managed repositories. Changing paths does not migrate
an existing installation. Stop the daemon before moving data, preserve the
database, and reconcile any recorded worktree paths before restarting.

Test commands are argument arrays, such as `["cargo", "test", "--locked",
"--offline"]`. Tests run without network access, so prepare dependencies first.
They must exit successfully without changing source files; ignore build outputs
in Git.

When a check fails, Maistro sends the command and its stdout/stderr to the coding
agent, which diagnoses the failure, repairs the code and reruns the checks.
Repairs remain bounded by `max_attempts`. Rust projects also get a compiler and
linker check inside the sandbox before stories are dispatched.

Use `toolchain_paths` to expose specific toolchains and caches read-only. For
Cargo, mount a registry directory such as `/home/you/.cargo/registry`. Maistro
also exposes it as the worker's Cargo registry. A mounted directory with a `bin`
child is added to `PATH`. Keep credentials out of these directories.

Maistro reads the existing `appflowy` MCP entry from `$CODEX_HOME/config.toml`, or
`~/.codex/config.toml` by default. Guarded AppFlowy writes currently support the
stdio transport. Workers need Codex's `auth.json`; keyring-only authentication
won't work. Set `agents.codex` to an absolute path if the service can't find Codex.
Validation checks the installed CLI and configured models.

## Create the GitHub token

Use a **personal access token (classic)** for this version. Maistro reads the
Checks API, which GitHub doesn't currently support with fine-grained personal
access tokens.

The account needs Write access to the managed repositories. You can use your
repository-owner account while `github.auto_merge = false`. Automatic merging
requires a non-administrator account with no protection-bypass role. This check
uses the account's repository role, not the token's `admin:*` scope checkboxes.

1. Sign in as that account and open [Create a classic personal access token](https://github.com/settings/tokens/new).
2. Set **Note** to `Maistro` and choose an expiration, for example 30 days.
3. Under **Select scopes**, check **repo**. Leave its included child scopes selected.
4. Leave every other top-level scope unchecked, including `workflow`, `admin:org`,
   `delete_repo`, and package scopes.
5. Click **Generate token** and save it in the environment file below. If your
   organization uses SAML SSO, authorize the token through **Configure SSO** on
   [Tokens (classic)](https://github.com/settings/tokens).

The `repo` scope covers Git pushes, pull requests, merges and private-repository
CI reads. A classic token can access all repositories available to its account;
it has no repository picker. Limit the dedicated account's access accordingly.
If your organization blocks classic tokens, this setup is blocked by that policy.

Create the local credential file:

```sh
umask 077
mkdir -p "$HOME/.config/maistro"
chmod 700 "$HOME/.config/maistro"
touch "$HOME/.config/maistro/environment"
chmod 600 "$HOME/.config/maistro/environment"
${EDITOR:-vi} "$HOME/.config/maistro/environment"
```

Add one line, replacing the placeholder with the generated token:

```dotenv
MAISTRO_GITHUB_TOKEN=PASTE_TOKEN_HERE
```

Keep the token in this file, outside Git and TOML. Maistro reads it locally; the
GitHub Actions workflow doesn't need it as a secret. When you replace an expired
token, update this file and restart the service so it picks up the new value.

## Prepare the board and CI

To create a new board inside an AppFlowy page, run:

```sh
maistro -c ~/.config/maistro/config.toml create-board 'https://appflowy.com/app/WORKSPACE_ID/PAGE_ID'
```

This explicitly authorizes creation of a child database named `Board`, grouped by
its `Status` select field. It uses the configured coordinator and AppFlowy MCP
server, through a setup guard that permits one creation under the supplied page and
subsequent writes only to the newly returned database/view IDs. Daemon scans and
projection permissions are unchanged. Setup requires the stdio MCP transport. The installed MCP must expose `create_database` and field editing
and reading tools, plus `get_database_view_settings` or `get_database_view`. The command uses only agent settings, so it can run before
adding a project; a missing configuration file uses the default agent settings.
It prints the new board URL, or workspace, database and view IDs with `--json`.
Success requires a creation receipt and final reads confirming that Status is a
single-select field, the created view groups by its field ID, and it has these options:

| Status | Color |
| --- | --- |
| Backlog | iron |
| Ready | azure |
| In progress | Mango |
| Review | mauve |
| Done | jade |
| Blocked | camelia |
| Cancelled | iron |

Each invocation creates a new board. If setup fails, it exits unsuccessfully and
retains private diagnostic logs at the path in the error. Inspect the parent page
before retrying: creation may have succeeded before a later step failed. The
command does not delete partial boards or register a project automatically.
When configuring a project for this board, set `in_progress = "In progress"` in
its `[projects.statuses]` table to match the created label exactly.


Use these board statuses: Backlog, Ready, In Progress, Review, Done, Blocked and
Cancelled. The `Status` select field must match `projects.statuses`. You can map
Ready to an existing label such as To Do.

Only Ready authorizes work. Describe the outcome you want; observable requirements
in prose are enough. The agent chooses ordinary implementation and design details
using repository conventions and reasonable defaults. Examples, mockups and exact
output formats are optional. Luna blocks a story only when a concrete obstacle
prevents meaningful progress, and explains why the agent cannot resolve it.

Dependencies refer to row IDs on the same board. Maistro waits for them to become
Done and explains the wait in a comment.

Stories and human comments may reference other AppFlowy pages. Luna reads linked
documents and resolves named guides through the workspace folder. Rust includes
the fetched text in the worker's context and revision checks. Only stories on the
configured board authorize work; reference documents do not create extra tasks.

Optional fields are `Execution ID`, `PR URL`, `Last error` and `Retry count`.
Maistro fills these in; underscore spellings also work.

The included [GitHub Actions workflow](.github/workflows/ci.yml) runs formatting,
Clippy, all automated tests and a release build. Its check is named `test`, matching
`required_checks = ["test"]` in the example configuration. Verify the check names
in your repository before configuring them.

Keep `github.auto_merge = false` for the first trial. Maistro can open and review
PRs while you handle merging. Each project defaults to
`continue_while_prs_pending = true`: pending PRs release the work slot so Maistro
can prepare more stories for you to review in a batch. It alternates new work
with pending PR checks and runs one coding, review or repair task at a time per
repository. Dependencies still wait for their prerequisite stories to be Done.
Set `continue_while_prs_pending = false` in a `[[projects]]` section to wait for
each PR to merge before starting another story.

When the base branch advances, it updates the PR
branch and asks the coding agent to resolve text conflicts within the repair
budget. Clean base updates do not consume repair attempts or invoke the coding
agent. It reruns local checks and independent review before pushing to the same
PR, then checks CI for the new commit. This also works with automatic merging
disabled and never uses a force push. Conflicts in protected files or files the
agent cannot safely edit require human handling.

To enable automatic squash merges, it also needs
read access to branch protection and rulesets, strict up-to-date required checks,
protection enforced for administrators, and no force pushes, deletions or bypass
actors. Token scopes don't grant access the account itself lacks. If protection
reads fail, automatic merging stays blocked.

Maistro currently requires classic branch protection, even when rulesets exist.
It doesn't change GitHub protection settings. Test the full flow on a disposable
project before enabling automatic merge.

## Validate and start

Load the token and check the configuration:

```sh
set -a
. "$HOME/.config/maistro/environment"
set +a
"$HOME/.local/bin/maistro" -c "$HOME/.config/maistro/config.toml" validate
```

Validation reads GitHub and AppFlowy, checks the repository, models and sandbox,
and writes local diagnostics. Fix any reported errors before starting.

Install and start the user service:

```sh
cd ~/dev/maistro
install -Dm644 systemd/maistro.service "$HOME/.config/systemd/user/maistro.service"
systemctl --user daemon-reload
systemctl --user enable --now maistro.service
```

Check the service, jobs and logs:

```sh
systemctl --user status maistro.service --no-pager
"$HOME/.local/bin/maistro" -c "$HOME/.config/maistro/config.toml" status
journalctl --user -u maistro.service -n 100 -f
```

Stop it with `systemctl --user stop maistro.service`. To keep it running after you
log out, an administrator can run `sudo loginctl enable-linger "$USER"`.

For a foreground run, use `maistro -c PATH run`. Polling defaults to 60 seconds.
`run --once` performs one cycle and can write code, push branches and open PRs.
Use `validate` for quick connectivity checks. It checks the local Codex CLI/login, initializes the configured AppFlowy MCP server and reads the workspace list, and requests each enabled GitHub repository. Each check has a 15-second timeout. It does not invoke models, scan stories, test the compiler/sandbox, or open the state database. Full operational preflight still runs when the daemon starts.

## Commands

All commands accept `-c PATH`. Without it, Maistro reads `config.toml` from the
current directory if present, otherwise the installed `~/.config/maistro/config.toml`.
This lets commands such as `maistro status` and `maistro reload` work from your
home directory after installation. An explicit `-c PATH` always takes precedence.
The systemd unit explicitly uses `~/.config/maistro/config.toml`.

| Command | What it does |
| --- | --- |
| `run [--once]` | Poll boards and process authorized work |
| `validate` | Check Codex login, AppFlowy connectivity, and GitHub repository access |
| `projects` | Show configured projects |
| `status` | Show runs, errors and pending updates |
| `config` | Open the selected config file in `$EDITOR` |
| `create-board PAGE_URL` | Create a child Board with the seven workflow states and colors |
| `project add` | Add a project interactively and validate it |
| `project pause ID` / `project resume ID` | Pause or resume one project |
| `pause` / `resume` | Pause or resume all projects |
| `retry RUN_OR_STORY_ID` | Retry a terminal run with a fresh attempt budget |
| `cancel RUN_OR_STORY_ID` | Stop work and keep the branch and PR for inspection |
| `reload` | Validate configuration changes after active work drains |

Output is human-readable by default. For example, `maistro projects` shows:

```text
Projects (1)

demo — Demo (enabled)
  Repository: /home/alice/demo
  Board: Demo board
  GitHub: alice/demo
  Base branch: main
```

`status` shows dispatch state, event and pending-update counts, project pause and
last-scan information, and runs with full IDs, story titles, state, attempts,
branches, worktrees, PR links and errors. An empty run list says `No runs recorded.`
Last-scan timestamps are labeled Unix seconds. `validate` prints a success line
for each connectivity check; the Codex login check is local and does not verify model availability.
Control commands confirm recorded changes or requests; a request confirmation does
not mean the daemon has finished processing it.

For scripts, use `maistro --json projects`, `maistro status --json`, or
`maistro --json validate`. This preserves the previous projects array, status
object; validation emits one JSON object per connectivity check with `component` and `connected` fields. Control commands remain silent
in JSON mode. Logs go to stderr (JSON with `--json`, plain text otherwise).
`project add` remains interactive, with prompts on stderr and a JSON result on
stdout when `--json` is set. Internal bridge and MCP stdout protocols are unchanged.

Run `EDITOR=nano maistro config` or `EDITOR='code --wait' maistro -c PATH config`.
The command waits for the editor and reports a failure if it exits unsuccessfully.
`EDITOR` is required and interpreted as a shell command, supporting arguments and
quoted executable paths; the config path is passed as a separate literal argument.
It opens the same path selected by `-c` or the default lookup above without parsing
or validating it, allowing broken files to be repaired and missing files to be
created by the editor. Parent directories must already exist. Editing does not
reload a running daemon; use `reload` separately. `--json` does not change editor I/O.

Pause lets active local work and review finish, but holds new jobs and merges.
Cancellation stops local work; it can't undo a merge GitHub has already accepted.
The board's Cancelled status belongs to the user. Maistro never writes it:
stopping an internal run, including through the CLI, leaves the board status alone.
Changed requirements or revoked authorization prevent stale work from merging.

Retry requires the same requirements revision and Ready status. Use the run ID
when a story has several revisions. Reload keeps runtime paths and project
identity fixed; use a new project ID for a different board, repository or base
branch. Invalid reloads keep the previous configuration. Keep a project enabled
while an uncertain `merging` run is being reconciled.

## Test and recover

Check the interactive installer with local fake tools (no credentials or network
access required):

```sh
bash -n install.sh
python3 -m unittest discover -s tests -p 'test_install.py'
```

Run the full checks on a Linux host that permits namespaces and local listeners:

```sh
cargo build --locked --bins
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked -- --include-ignored
```

The ignored tests use disposable repositories and fake external services. They
need host isolation features, which is why the command includes them explicitly.
`cargo run --example worker_probe` is a separate live Astra call that writes and
checks a file in a temporary sandbox.

Workers can access their assigned source and selected tools. Reviewers get
read-only source. Tests have no network or model credentials. Rust owns GitHub
credentials, checks test and review evidence, and enforces merge rules. An agent's
success report can't approve a merge. Changes to protected files, including CI,
policy, authentication, deployment and migrations, require human handling.

Restart with the same configuration and SQLite database to recover interrupted
work. Maistro checks existing branches, PRs and merge results before repeating
remote writes. AppFlowy updates retry separately, so an outage after a merge
doesn't rerun implementation. Worktrees are retained by default. Setting
`retain_worktrees = false` removes only clean, completed worktrees; branches, PRs
and logs remain.

See [implementation progress and limitations](IMPLEMENTATION_PROGRESS.md) for
isolation, recovery, test coverage and integration gaps.

A blocked, failed, or cancelled run can be retried from the board: let Maistro
observe the card in a non-Ready state, then move it to Ready. With unchanged
requirements, a successful fresh scan resets retry budgets and reuses the same
run, branch, worktree, and PR. Readiness and dependency checks still apply.
Completed runs are not reopened, and In Progress/Review alone do not restart
terminal runs. A card that remains Ready across failure or restart is not a new
retry request. This avoids retry loops when board updates are delayed. The
explicit `maistro retry <run-id>` command remains available for such cards.
